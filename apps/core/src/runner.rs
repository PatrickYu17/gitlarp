//! Runs every due schedule once: decrypts each record, computes the
//! due days (with catch-up), writes the commits, advances `lastRun`.
//!
//! Failure policy: `write_commits` guarantees that when it errors, no
//! reachable commit was created, so `lastRun` stays put and the next
//! tick retries the same window. On success (including partial
//! results, where the ref was updated) the window is marked done.
//! With `RunLimits`, a window too big for the shell's per-record
//! budget runs only its leading days; `lastRun` then advances just
//! past the last attempted day, so the remainder comes due again on
//! the next tick. A record whose due work would not fit the
//! invocation-wide budget defers untouched: `lastRun` stays put, the
//! record is not failed, and the next tick retries it. Records the
//! current secret cannot decrypt fall back to `old_secret` (secret
//! rotation); the store-back heals them under the current secret.

use serde::{Deserialize, Serialize};
use time::Date;

use crate::crypto::{decrypt_json, encrypt_json};
use crate::engine::write_commits;
use crate::gh::DEFAULT_REPO;
use crate::http::Runtime;
use crate::schedule::{due_days, ScheduleSpec};
use crate::store::Store;
use crate::Error;

/// Plaintext shape of an encrypted schedule record. Field names match
/// the old TS wire format exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredSchedule {
    pub pat: String,
    pub spec: ScheduleSpec,
    #[serde(with = "crate::schedule::date_str")]
    pub last_run: Date,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunnerResult {
    pub schedules: u32,
    pub ran: u32,
    pub commits: u32,
    pub failed: u32,
    /// Records deferred by the invocation-wide budget: not failures,
    /// they are retried untouched on the next tick.
    pub deferred: u32,
}

/// The secrets the runner decrypts with: the current one first,
/// then the rotated-out predecessor as a fallback (records still
/// encrypted under it heal on the store-back).
#[derive(Debug, Clone, Copy)]
pub struct Secrets<'a> {
    pub current: &'a str,
    pub old: Option<&'a str>,
}

/// Budgets for one runner invocation.
///
/// Shells with a hard per-invocation request ceiling (the Cloudflare
/// worker: one fetch subrequest per commit, ~50/invocation on the
/// free plan) use these to stay under it. When a record's due window
/// exceeds `max_commits_per_run`, only the leading days run and
/// `lastRun` advances to the last attempted day, so the deferred days
/// come due again on the next tick instead of being silently dropped.
/// The first day always runs (even when its count alone exceeds the
/// budget), so a record with big days still makes progress.
/// `max_commits_total` spans every record in the invocation — the
/// platform ceiling is per invocation, not per record — and a record
/// whose due work would not fit the remainder defers entirely:
/// `lastRun` untouched, not failed, retried on the next tick.
#[derive(Debug, Clone, Copy)]
pub struct RunLimits {
    /// Max commits per schedule record per invocation; 0 = uncapped.
    pub max_commits_per_run: u32,
    /// Max commits across all records in one invocation; None = uncapped.
    pub max_commits_total: Option<u32>,
}

impl RunLimits {
    pub const fn none() -> Self {
        RunLimits { max_commits_per_run: 0, max_commits_total: None }
    }
}

/// Uncapped run (native server semantics): the whole due window runs
/// in one pass.
pub async fn run_due_schedules(
    rt: &dyn Runtime,
    store: &dyn Store,
    secrets: &Secrets<'_>,
    today: Date,
    rng: &mut dyn FnMut(u32, u32) -> u32,
    log: &dyn Fn(&str),
) -> Result<RunnerResult, Error> {
    run_due_schedules_limited(rt, store, secrets, today, rng, log, RunLimits::none()).await
}

pub async fn run_due_schedules_limited(
    rt: &dyn Runtime,
    store: &dyn Store,
    secrets: &Secrets<'_>,
    today: Date,
    rng: &mut dyn FnMut(u32, u32) -> u32,
    log: &dyn Fn(&str),
    limits: RunLimits,
) -> Result<RunnerResult, Error> {
    let mut out = RunnerResult::default();
    // invocation-wide budget: the platform ceiling is per cron
    // invocation, not per record, so every record draws from one pot
    let mut budget = limits.max_commits_total;
    'invocation: for user in store.list_users().await? {
        for record in store.list(&user).await? {
            out.schedules += 1;
            let key = format!("{user}/{}", record.id);
            // current secret first; on failure try the old one
            // (rotation) — the store-back below then heals the
            // record under the current secret
            let mut data = match decrypt_json::<StoredSchedule>(secrets.current, &record.payload) {
                Ok(d) => d,
                Err(_) => match secrets
                    .old
                    .map(|old| decrypt_json::<StoredSchedule>(old, &record.payload))
                {
                    Some(Ok(d)) => d,
                    _ => {
                        log(&format!("schedule {key}: cannot decrypt (secret rotated?), skipped"));
                        out.failed += 1;
                        continue;
                    }
                },
            };
            let mut days = due_days(&data.spec, Some(data.last_run), today, rng);
            // Trim the window at a day boundary to fit the per-run
            // budget; advance lastRun only to the last attempted day
            // so the rest come due again next tick.
            let mut last_done = today;
            if limits.max_commits_per_run > 0 {
                let mut total = 0u32;
                let mut cut = days.len();
                for (i, &(_, n)) in days.iter().enumerate() {
                    // i > 0: the first due day always runs, whatever its count
                    if i > 0 && total.saturating_add(n) > limits.max_commits_per_run {
                        cut = i;
                        break;
                    }
                    total += n;
                }
                if cut < days.len() {
                    last_done = days[cut - 1].0;
                    log(&format!(
                        "schedule {key}: deferring {} day(s) to the next run (per-run cap {})",
                        days.len() - cut,
                        limits.max_commits_per_run
                    ));
                    days.truncate(cut);
                }
            }
            // Invocation-wide budget: a record that would not fit the
            // remaining pot defers untouched (not failed) and stops
            // the run — later records cannot fit either. Counted by
            // the attempted window: a failed run still burns the
            // platform's subrequests.
            let due: u32 = days.iter().map(|&(_, n)| n).sum();
            if let Some(remaining) = budget {
                if due > remaining {
                    log(&format!(
                        "schedule {key}: deferred to the next run ({due} commit(s) due, \
                         {remaining} left in the invocation budget)"
                    ));
                    out.deferred += 1;
                    break 'invocation;
                }
                budget = Some(remaining.saturating_sub(due));
            }
            if !days.is_empty() {
                match write_commits(rt, &data.pat, DEFAULT_REPO, &days, None).await {
                    Ok(r) => {
                        out.ran += 1;
                        out.commits += r.created;
                        match &r.partial {
                            Some(err) => log(&format!(
                                "schedule {key}: partial ({}/{}): {err}",
                                r.created, r.total
                            )),
                            None => log(&format!(
                                "schedule {key}: {} commit(s) on {} day(s)",
                                r.created,
                                days.len()
                            )),
                        }
                    }
                    Err(e) => {
                        log(&format!("schedule {key}: failed, will retry ({e})"));
                        out.failed += 1;
                        continue;
                    }
                }
            }
            data.last_run = last_done;
            let blob = match encrypt_json(secrets.current, &data) {
                Ok(b) => b,
                Err(_) => {
                    out.failed += 1;
                    continue;
                }
            };
            if store.put(&user, &record.id, &blob).await.is_err() {
                out.failed += 1;
            }
            rt.sleep(500).await;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::CommitResult;
    use crate::http::{HttpRequest, HttpResponse};
    use crate::schedule::parse_spec;
    use crate::store::ScheduleRecord;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use time::macros::date;

    struct Mock {
        responses: Mutex<VecDeque<Result<HttpResponse, Error>>>,
    }

    impl Runtime for Mock {
        fn fetch(
            &self,
            _req: HttpRequest,
        ) -> crate::http::BoxFut<Result<HttpResponse, Error>> {
            let r = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(Error::new(500, "mock: unexpected request")));
            Box::pin(std::future::ready(r))
        }
        fn sleep(&self, _ms: u64) -> crate::http::BoxFut<()> {
            Box::pin(std::future::ready(()))
        }
        fn random(&self, buf: &mut [u8]) {
            for (i, b) in buf.iter_mut().enumerate() {
                *b = i as u8;
            }
        }
    }

    fn ok(body: serde_json::Value) -> Result<HttpResponse, Error> {
        Ok(HttpResponse { status: 200, body: body.to_string() })
    }

    struct MemStore {
        recs: Mutex<Vec<(String, String, String)>>, // (user, id, payload)
    }

    impl Store for MemStore {
        fn list_users(&self) -> crate::http::BoxFut<Result<Vec<String>, Error>> {
            let mut users: Vec<String> =
                self.recs.lock().unwrap().iter().map(|(u, _, _)| u.clone()).collect();
            // one entry per user, like both real stores (FileStore
            // lists user dirs, D1 does SELECT DISTINCT)
            users.sort();
            users.dedup();
            Box::pin(std::future::ready(Ok(users)))
        }
        fn list(&self, user: &str) -> crate::http::BoxFut<Result<Vec<ScheduleRecord>, Error>> {
            let recs = self
                .recs
                .lock()
                .unwrap()
                .iter()
                .filter(|(u, _, _)| u == user)
                .map(|(_, id, p)| ScheduleRecord { id: id.clone(), payload: p.clone() })
                .collect();
            Box::pin(std::future::ready(Ok(recs)))
        }
        fn get(&self, user: &str, id: &str) -> crate::http::BoxFut<Result<Option<String>, Error>> {
            let hit = self
                .recs
                .lock()
                .unwrap()
                .iter()
                .find(|(u, i, _)| u == user && i == id)
                .map(|(_, _, p)| p.clone());
            Box::pin(std::future::ready(Ok(hit)))
        }
        fn put(
            &self,
            user: &str,
            id: &str,
            payload: &str,
        ) -> crate::http::BoxFut<Result<(), Error>> {
            let mut recs = self.recs.lock().unwrap();
            recs.retain(|(u, i, _)| !(u == user && i == id));
            recs.push((user.into(), id.into(), payload.into()));
            Box::pin(std::future::ready(Ok(())))
        }
        fn delete(&self, user: &str, id: &str) -> crate::http::BoxFut<Result<(), Error>> {
            let mut recs = self.recs.lock().unwrap();
            recs.retain(|(u, i, _)| !(u == user && i == id));
            Box::pin(std::future::ready(Ok(())))
        }
    }

    fn gh_responses(commits: u32) -> VecDeque<Result<HttpResponse, Error>> {
        let mut q = VecDeque::new();
        q.push_back(ok(serde_json::json!({ "id": 1, "login": "octocat" })));
        q.push_back(ok(serde_json::json!({ "default_branch": "main" })));
        q.push_back(ok(serde_json::json!({
            "object": { "sha": "existing" }
        })));
        q.push_back(ok(serde_json::json!({ "tree": { "sha": "tree" } })));
        q.push_back(ok(serde_json::json!({ "object": { "sha": "base" } })));
        for i in 0..commits {
            q.push_back(ok(serde_json::json!({ "sha": format!("c{i}") })));
        }
        q.push_back(ok(serde_json::json!({})));
        q
    }

    #[test]
    fn runner_advances_on_success_and_retries_on_failure() {
        let secret = "topsecret";
        let spec = parse_spec(&serde_json::json!({ "min": 1, "max": 1 })).unwrap();
        let stored = StoredSchedule {
            pat: "pat".into(),
            spec,
            last_run: date!(2026-08-28),
            created_at: "2026-08-01".into(),
        };
        let blob = crate::crypto::encrypt_json_with_iv(secret, &stored, &[0u8; 12]).unwrap();
        let store = MemStore { recs: Mutex::new(vec![("u".into(), "s1".into(), blob)]) };
        let logs = Mutex::new(Vec::<String>::new());
        let log = |line: &str| logs.lock().unwrap().push(line.into());

        // success path: one due day -> commits -> lastRun advanced
        let mock = Mock { responses: Mutex::new(gh_responses(1)) };
        let mut rng = |lo: u32, _hi: u32| lo;
        let r = crate::block_on(run_due_schedules(
            &mock,
            &store,
            &Secrets { current: secret, old: None },
            date!(2026-08-29),
            &mut rng,
            &log,
        ))
        .unwrap();
        assert_eq!((r.schedules, r.ran, r.commits, r.failed, r.deferred), (1, 1, 1, 0, 0));
        let payload = crate::block_on(store.get("u", "s1")).unwrap().unwrap();
        let now: StoredSchedule = crate::crypto::decrypt_json(secret, &payload).unwrap();
        assert_eq!(now.last_run, date!(2026-08-29));

        // failure path: due day but GitHub is down -> lastRun stays, failed++
        let mock = Mock {
            responses: Mutex::new({
                let mut q = VecDeque::new();
                q.push_back(Err(Error::new(500, "boom")));
                q
            }),
        };
        let stored2 = StoredSchedule { last_run: date!(2026-08-28), ..now.clone() };
        let blob2 = crate::crypto::encrypt_json_with_iv(secret, &stored2, &[0u8; 12]).unwrap();
        let store2 = MemStore { recs: Mutex::new(vec![("u".into(), "s1".into(), blob2)]) };
        let r2 = crate::block_on(run_due_schedules(
            &mock,
            &store2,
            &Secrets { current: secret, old: None },
            date!(2026-08-29),
            &mut rng,
            &log,
        ))
        .unwrap();
        assert_eq!((r2.schedules, r2.ran, r2.commits, r2.failed, r2.deferred), (1, 0, 0, 1, 0));
        let payload2 = crate::block_on(store2.get("u", "s1")).unwrap().unwrap();
        let after: StoredSchedule = crate::crypto::decrypt_json(secret, &payload2).unwrap();
        assert_eq!(after.last_run, date!(2026-08-28), "failure must not advance the window");
    }

    /// A budgeted run only writes the leading due days and advances
    /// `lastRun` to the last attempted day, so the next run picks up
    /// the remainder instead of skipping it.
    #[test]
    fn limited_run_defers_overflow_days_and_catches_up() {
        let secret = "topsecret";
        let spec = parse_spec(&serde_json::json!({ "min": 1, "max": 1 })).unwrap();
        let stored = StoredSchedule {
            pat: "pat".into(),
            spec,
            last_run: date!(2026-08-27),
            created_at: "2026-08-01".into(),
        };
        let blob = crate::crypto::encrypt_json_with_iv(secret, &stored, &[0u8; 12]).unwrap();
        let store = MemStore { recs: Mutex::new(vec![("u".into(), "s1".into(), blob)]) };
        let log = |_: &str| {};

        // 3 due days (28, 29, 30), budget 2 -> only 28 and 29 run
        let mock = Mock { responses: Mutex::new(gh_responses(2)) };
        let mut rng = |lo: u32, _hi: u32| lo;
        let limits = RunLimits { max_commits_per_run: 2, max_commits_total: None };
        let r = crate::block_on(run_due_schedules_limited(
            &mock,
            &store,
            &Secrets { current: secret, old: None },
            date!(2026-08-30),
            &mut rng,
            &log,
            limits,
        ))
        .unwrap();
        assert_eq!((r.schedules, r.ran, r.commits, r.failed, r.deferred), (1, 1, 2, 0, 0));
        let payload = crate::block_on(store.get("u", "s1")).unwrap().unwrap();
        let mid: StoredSchedule = crate::crypto::decrypt_json(secret, &payload).unwrap();
        assert_eq!(
            mid.last_run,
            date!(2026-08-29),
            "deferred day must stay due: lastRun stops at the last attempted day"
        );

        // next tick (same day): the remaining day 30 runs, and lastRun
        // advances to today with nothing left due after it
        let mock = Mock { responses: Mutex::new(gh_responses(1)) };
        let r2 = crate::block_on(run_due_schedules_limited(
            &mock,
            &store,
            &Secrets { current: secret, old: None },
            date!(2026-08-30),
            &mut rng,
            &log,
            limits,
        ))
        .unwrap();
        assert_eq!((r2.schedules, r2.ran, r2.commits, r2.failed, r2.deferred), (1, 1, 1, 0, 0));
        let payload = crate::block_on(store.get("u", "s1")).unwrap().unwrap();
        let done: StoredSchedule = crate::crypto::decrypt_json(secret, &payload).unwrap();
        assert_eq!(done.last_run, date!(2026-08-30));
    }

    /// The first due day always runs even when its count alone exceeds
    /// the budget: a record with big days still makes progress.
    #[test]
    fn limited_run_attempts_oversized_first_day() {
        let secret = "topsecret";
        let spec = parse_spec(&serde_json::json!({ "min": 3, "max": 3 })).unwrap();
        let stored = StoredSchedule {
            pat: "pat".into(),
            spec,
            last_run: date!(2026-08-27),
            created_at: "2026-08-01".into(),
        };
        let blob = crate::crypto::encrypt_json_with_iv(secret, &stored, &[0u8; 12]).unwrap();
        let store = MemStore { recs: Mutex::new(vec![("u".into(), "s1".into(), blob)]) };
        let log = |_: &str| {};

        // day 28 alone wants 3 commits against a budget of 2
        let mock = Mock { responses: Mutex::new(gh_responses(3)) };
        let mut rng = |lo: u32, _hi: u32| lo;
        let r = crate::block_on(run_due_schedules_limited(
            &mock,
            &store,
            &Secrets { current: secret, old: None },
            date!(2026-08-28),
            &mut rng,
            &log,
            RunLimits { max_commits_per_run: 2, max_commits_total: None },
        ))
        .unwrap();
        assert_eq!((r.schedules, r.ran, r.commits, r.failed, r.deferred), (1, 1, 3, 0, 0));
        let payload = crate::block_on(store.get("u", "s1")).unwrap().unwrap();
        let after: StoredSchedule = crate::crypto::decrypt_json(secret, &payload).unwrap();
        assert_eq!(after.last_run, date!(2026-08-28));
    }

    /// RunLimits::none() keeps the old uncapped semantics.
    #[test]
    fn unlimited_limits_match_legacy_behavior() {
        let secret = "topsecret";
        let spec = parse_spec(&serde_json::json!({ "min": 1, "max": 1 })).unwrap();
        let stored = StoredSchedule {
            pat: "pat".into(),
            spec,
            last_run: date!(2026-08-27),
            created_at: "2026-08-01".into(),
        };
        let blob = crate::crypto::encrypt_json_with_iv(secret, &stored, &[0u8; 12]).unwrap();
        let store = MemStore { recs: Mutex::new(vec![("u".into(), "s1".into(), blob)]) };
        let log = |_: &str| {};
        // 3 due days, all written in one pass despite a tiny budget
        let mock = Mock { responses: Mutex::new(gh_responses(3)) };
        let mut rng = |lo: u32, _hi: u32| lo;
        assert_eq!(RunLimits::none().max_commits_per_run, 0);
        assert!(RunLimits::none().max_commits_total.is_none());
        let r = crate::block_on(run_due_schedules_limited(
            &mock,
            &store,
            &Secrets { current: secret, old: None },
            date!(2026-08-30),
            &mut rng,
            &log,
            RunLimits::none(),
        ))
        .unwrap();
        assert_eq!(r.commits, 3);
        let payload = crate::block_on(store.get("u", "s1")).unwrap().unwrap();
        let after: StoredSchedule = crate::crypto::decrypt_json(secret, &payload).unwrap();
        assert_eq!(after.last_run, date!(2026-08-30));
    }

    /// The invocation budget spans records: when the first record
    /// spends it all, the second defers with `lastRun` untouched
    /// (and is not failed), and the next run picks it up.
    #[test]
    fn invocation_budget_defers_later_records_to_the_next_run() {
        let secret = "topsecret";
        let mk = || StoredSchedule {
            pat: "pat".into(),
            spec: parse_spec(&serde_json::json!({ "min": 1, "max": 1 })).unwrap(),
            last_run: date!(2026-08-29),
            created_at: "2026-08-01".into(),
        };
        let store = MemStore {
            recs: Mutex::new(vec![
                ("u".into(), "s1".into(), crate::crypto::encrypt_json_with_iv(
                    secret,
                    &mk(),
                    &[0u8; 12],
                )
                .unwrap()),
                ("u".into(), "s2".into(), crate::crypto::encrypt_json_with_iv(
                    secret,
                    &mk(),
                    &[0u8; 12],
                )
                .unwrap()),
            ]),
        };
        let logs = Mutex::new(Vec::<String>::new());
        let log = |line: &str| logs.lock().unwrap().push(line.into());

        // one shared commit for the whole invocation: s1 runs, s2 defers
        let mock = Mock { responses: Mutex::new(gh_responses(1)) };
        let mut rng = |lo: u32, _hi: u32| lo;
        let limits = RunLimits { max_commits_per_run: 0, max_commits_total: Some(1) };
        let r = crate::block_on(run_due_schedules_limited(
            &mock,
            &store,
            &Secrets { current: secret, old: None },
            date!(2026-08-30),
            &mut rng,
            &log,
            limits,
        ))
        .unwrap();
        assert_eq!((r.schedules, r.ran, r.commits, r.failed, r.deferred), (2, 1, 1, 0, 1));
        let payload = crate::block_on(store.get("u", "s2")).unwrap().unwrap();
        let s2: StoredSchedule = crate::crypto::decrypt_json(secret, &payload).unwrap();
        assert_eq!(s2.last_run, date!(2026-08-29), "a deferred record keeps its window");
        assert!(
            logs.lock().unwrap().iter().any(|l| l.contains("u/s2") && l.contains("deferred")),
            "the deferral must name the record: {:?}",
            logs.lock().unwrap()
        );

        // next tick: a fresh budget; s1 has nothing due, s2 catches up
        let mock = Mock { responses: Mutex::new(gh_responses(1)) };
        let r2 = crate::block_on(run_due_schedules_limited(
            &mock,
            &store,
            &Secrets { current: secret, old: None },
            date!(2026-08-30),
            &mut rng,
            &log,
            limits,
        ))
        .unwrap();
        assert_eq!((r2.schedules, r2.ran, r2.commits, r2.failed, r2.deferred), (2, 1, 1, 0, 0));
        let payload = crate::block_on(store.get("u", "s2")).unwrap().unwrap();
        let done: StoredSchedule = crate::crypto::decrypt_json(secret, &payload).unwrap();
        assert_eq!(done.last_run, date!(2026-08-30));
    }

    /// A record still encrypted under the old secret runs via the
    /// rotation fallback, and the store-back heals it: the stored
    /// payload decrypts under the current secret afterwards.
    #[test]
    fn old_secret_record_runs_and_heals_under_current_secret() {
        let (old, cur) = ("old-secret", "cur-secret");
        let spec = parse_spec(&serde_json::json!({ "min": 1, "max": 1 })).unwrap();
        let stored = StoredSchedule {
            pat: "pat".into(),
            spec,
            last_run: date!(2026-08-28),
            created_at: "2026-08-01".into(),
        };
        let blob = crate::crypto::encrypt_json_with_iv(old, &stored, &[0u8; 12]).unwrap();
        let store = MemStore { recs: Mutex::new(vec![("u".into(), "s1".into(), blob)]) };
        let log = |_: &str| {};
        let mock = Mock { responses: Mutex::new(gh_responses(1)) };
        let mut rng = |lo: u32, _hi: u32| lo;
        let r = crate::block_on(run_due_schedules_limited(
            &mock,
            &store,
            &Secrets { current: cur, old: Some(old) },
            date!(2026-08-29),
            &mut rng,
            &log,
            RunLimits::none(),
        ))
        .unwrap();
        assert_eq!((r.schedules, r.ran, r.commits, r.failed, r.deferred), (1, 1, 1, 0, 0));
        let payload = crate::block_on(store.get("u", "s1")).unwrap().unwrap();
        let healed: StoredSchedule = crate::crypto::decrypt_json(cur, &payload)
            .expect("the store-back must re-encrypt under the current secret");
        assert_eq!(healed.last_run, date!(2026-08-29));
    }

    /// Without the fallback configured, a rotated record stays
    /// skipped-failed: the runner never writes or re-stores it.
    #[test]
    fn old_secret_record_fails_closed_without_fallback() {
        let (old, cur) = ("old-secret", "cur-secret");
        let spec = parse_spec(&serde_json::json!({ "min": 1, "max": 1 })).unwrap();
        let stored = StoredSchedule {
            pat: "pat".into(),
            spec,
            last_run: date!(2026-08-28),
            created_at: "2026-08-01".into(),
        };
        let blob = crate::crypto::encrypt_json_with_iv(old, &stored, &[0u8; 12]).unwrap();
        let store = MemStore { recs: Mutex::new(vec![("u".into(), "s1".into(), blob)]) };
        let log = |_: &str| {};
        // empty mock: a skipped record must not touch GitHub at all
        let mock = Mock { responses: Mutex::new(VecDeque::new()) };
        let mut rng = |lo: u32, _hi: u32| lo;
        let r = crate::block_on(run_due_schedules_limited(
            &mock,
            &store,
            &Secrets { current: cur, old: None },
            date!(2026-08-29),
            &mut rng,
            &log,
            RunLimits::none(),
        ))
        .unwrap();
        assert_eq!((r.schedules, r.ran, r.commits, r.failed, r.deferred), (1, 0, 0, 1, 0));
        let payload = crate::block_on(store.get("u", "s1")).unwrap().unwrap();
        assert!(crate::crypto::decrypt_json::<StoredSchedule>(cur, &payload).is_err());
        let unchanged: StoredSchedule = crate::crypto::decrypt_json(old, &payload).unwrap();
        assert_eq!(unchanged.last_run, date!(2026-08-28), "no re-store without a run");
    }

    #[test]
    fn engine_commit_result_shape() {
        let r = CommitResult { created: 3, total: 4, partial: Some("x".into()) };
        assert_eq!(r.created, 3);
    }
}
