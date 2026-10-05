//! The commit engine: writes backdated commits to the private
//! gitlarp-history repo over the GitHub REST API.
//!
//! Guarantees (same contract as the old TS engine): if this returns
//! Err, no reachable commit was created, so retrying is safe. If it
//! returns Ok, `created > 0` and the branch ref was updated (a
//! partial result means some commits failed mid-run).
//!
//! Concurrency contract: a run is *not* atomic against other runs.
//! It reads the branch ref once, chains commits, then force-updates
//! the ref, so two overlapping runs (e.g. the hourly scheduler racing
//! a manual API call) interleave and the last ref update wins — the
//! loser's commits become unreachable (they cost API calls but never
//! count). Shells that can overlap writes must serialize them: the
//! native server takes a per-process lock around every commit run;
//! the worker relies on its single cron trigger. Multi-instance
//! deployments need external coordination.

use serde::Serialize;
use time::Date;

use crate::gh::GhClient;
use crate::http::Runtime;
use crate::plan::chunk_plan;
use crate::Error;

const SEED_DATE: &str = "2000-01-01";

#[derive(Debug, Clone)]
pub struct CommitResult {
    pub created: u32,
    pub total: u32,
    pub partial: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum WipeOutcome {
    /// reset the branch to gitlarp-base at this sha
    Reset { sha: String },
    /// no base existed (repo was empty when larping started): branch deleted
    Deleted { branch: String },
}

pub async fn write_commits(
    rt: &dyn Runtime,
    pat: &str,
    repo: &str,
    days: &[(Date, u32)],
    branch: Option<&str>,
) -> Result<CommitResult, Error> {
    let gh = GhClient::new(rt, pat, repo);
    let (user_id, login) = gh.user().await?;
    let email = format!("{user_id}+{login}@users.noreply.github.com");
    let default_branch = gh.ensure_repo(&login).await?;
    let branch = branch.unwrap_or(&default_branch).to_string();
    let branch_ref = format!("refs/heads/{branch}");

    let mut parent: String;
    let tree_sha: String;
    match gh.get_ref(&login, &branch_ref).await? {
        Some(sha) => {
            parent = sha.clone();
            tree_sha = gh.commit_tree(&login, &sha).await?;
            // mark the pre-larp state once, so wipe always has a target
            if gh.get_ref(&login, "refs/heads/gitlarp-base").await?.is_none() {
                gh.create_ref(&login, "refs/heads/gitlarp-base", &sha).await?;
            }
        }
        None => {
            let blob = gh.create_blob(&login).await?;
            tree_sha = gh.create_tree(&login, &blob).await?;
            parent = gh.create_commit(&login, &email, SEED_DATE, &tree_sha, None).await?;
            gh.create_ref(&login, &branch_ref, &parent).await?;
        }
    }

    let total: u32 = days.iter().map(|(_, n)| n).sum();
    let mut created = 0u32;
    let mut failed: Option<String> = None;
    let chunks = chunk_plan(days, 50);
    for (ci, chunk) in chunks.iter().enumerate() {
        for &(date, count) in chunk {
            let ds = crate::date::fmt(date);
            for _ in 0..count {
                match gh.create_commit(&login, &email, &ds, &tree_sha, Some(&parent)).await {
                    Ok(sha) => {
                        parent = sha;
                        created += 1;
                    }
                    Err(e) => {
                        failed = Some(e.message);
                        break;
                    }
                }
            }
            if failed.is_some() {
                break;
            }
        }
        if failed.is_some() {
            break;
        }
        if ci < chunks.len() - 1 {
            rt.sleep(1000).await;
        }
    }

    if created == 0 {
        return Err(Error::new(502, failed.unwrap_or_else(|| "no commits created".into())));
    }

    gh.update_ref(&login, &branch_ref, &parent).await.map_err(|e| {
        Error::new(502, format!("ref update failed ({e}): commits may be unreachable"))
    })?;

    Ok(CommitResult { created, total, partial: failed })
}

/// Reset the branch to gitlarp-base (undo all larp commits), or
/// delete it when the repo was empty when larping started.
pub async fn wipe(
    rt: &dyn Runtime,
    pat: &str,
    repo: &str,
    branch: Option<&str>,
) -> Result<WipeOutcome, Error> {
    let gh = GhClient::new(rt, pat, repo);
    let (_, login) = gh.user().await?;
    let default_branch = gh.ensure_repo(&login).await?;
    let branch = branch.unwrap_or(&default_branch).to_string();
    let branch_ref = format!("refs/heads/{branch}");

    if let Some(sha) = gh.get_ref(&login, "refs/heads/gitlarp-base").await? {
        gh.update_ref(&login, &branch_ref, &sha).await?;
        return Ok(WipeOutcome::Reset { sha });
    }
    if gh.get_ref(&login, &branch_ref).await?.is_some() {
        gh.delete_ref(&login, &branch_ref).await?;
        return Ok(WipeOutcome::Deleted { branch });
    }
    Err(Error::bad("nothing to wipe (no gitlarp-base, no remote branch)"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{HttpRequest, HttpResponse};
    use crate::Error;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use time::macros::date;

    struct Mock {
        calls: Mutex<Vec<HttpRequest>>,
        responses: Mutex<VecDeque<Result<HttpResponse, Error>>>,
        sleeps: Mutex<u32>,
    }

    impl crate::http::Runtime for Mock {
        fn fetch(&self, req: HttpRequest) -> crate::http::BoxFut<Result<HttpResponse, Error>> {
            self.calls.lock().unwrap().push(req);
            let r = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(Error::new(500, "mock: unexpected request")));
            Box::pin(std::future::ready(r))
        }
        fn sleep(&self, _ms: u64) -> crate::http::BoxFut<()> {
            *self.sleeps.lock().unwrap() += 1;
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

    fn nf() -> Result<HttpResponse, Error> {
        Ok(HttpResponse { status: 404, body: "{\"message\":\"Not Found\"}".into() })
    }

    fn json_body(req: &HttpRequest) -> serde_json::Value {
        serde_json::from_str(req.body.as_deref().unwrap_or("null")).unwrap()
    }

    fn mock(queue: VecDeque<Result<HttpResponse, Error>>) -> Mock {
        Mock {
            calls: Mutex::new(Vec::new()),
            responses: Mutex::new(queue),
            sleeps: Mutex::new(0),
        }
    }

    fn user_and_repo() -> Vec<Result<HttpResponse, Error>> {
        vec![
            ok(serde_json::json!({ "id": 123, "login": "octocat" })),
            ok(serde_json::json!({ "default_branch": "main" })),
        ]
    }

    #[test]
    fn fresh_repo_seeds_and_chains() {
        let mut q: VecDeque<_> = user_and_repo().into();
        q.push_back(nf()); // get branch -> 404
        q.push_back(ok(serde_json::json!({ "sha": "blob" })));
        q.push_back(ok(serde_json::json!({ "sha": "tree" })));
        q.push_back(ok(serde_json::json!({ "sha": "seed" })));
        q.push_back(ok(serde_json::json!({}))); // create branch ref
        q.push_back(ok(serde_json::json!({ "sha": "c1" })));
        q.push_back(ok(serde_json::json!({ "sha": "c2" })));
        q.push_back(ok(serde_json::json!({}))); // update ref
        let m = mock(q);
        let days = [(date!(2026-08-20), 1), (date!(2026-08-21), 1)];
        let r = crate::block_on(write_commits(&m, "pat", "gitlarp-history", &days, None)).unwrap();
        assert_eq!(r.created, 2);
        assert!(r.partial.is_none());

        let calls = m.calls.lock().unwrap();
        let paths: Vec<String> = calls.iter().map(|c| c.url.clone()).collect();
        assert_eq!(paths[0], "https://api.github.com/user");
        assert_eq!(paths[2], "https://api.github.com/repos/octocat/gitlarp-history/git/ref/heads/main");
        // seed commit: dated 2000-01-01, no parents, noon UTC
        let seed = json_body(&calls[5]);
        assert_eq!(seed["message"], "2000-01-01");
        assert_eq!(seed["parents"], serde_json::json!([]));
        assert_eq!(seed["author"]["date"], "2000-01-01T12:00:00Z");
        assert_eq!(seed["author"]["email"], "123+octocat@users.noreply.github.com");
        // larp commits chain on the same tree
        let c1 = json_body(&calls[7]);
        assert_eq!(c1["message"], "2026-08-20");
        assert_eq!(c1["tree"], "tree");
        assert_eq!(c1["parents"], serde_json::json!(["seed"]));
        assert_eq!(c1["author"]["date"], "2026-08-20T12:00:00Z");
        // final ref update is a forced PATCH to the last commit
        let upd = calls.last().unwrap();
        assert_eq!(upd.method, "PATCH");
        let b = json_body(upd);
        assert_eq!(b["sha"], "c2");
        assert_eq!(b["force"], true);
    }

    #[test]
    fn existing_branch_marks_base_once() {
        let mut q: VecDeque<_> = user_and_repo().into();
        q.push_back(ok(serde_json::json!({ "object": { "sha": "head" } })));
        q.push_back(ok(serde_json::json!({ "tree": { "sha": "tree" } })));
        q.push_back(nf()); // base 404
        q.push_back(ok(serde_json::json!({}))); // create base ref
        q.push_back(ok(serde_json::json!({ "sha": "c1" })));
        q.push_back(ok(serde_json::json!({}))); // update branch
        let m = mock(q);
        let days = [(date!(2026-08-20), 1)];
        let r = crate::block_on(write_commits(&m, "pat", "gitlarp-history", &days, None)).unwrap();
        assert_eq!(r.created, 1);
        let calls = m.calls.lock().unwrap();
        let base = calls.iter().find(|c| c.url.ends_with("/git/refs")).unwrap();
        let b = json_body(base);
        assert_eq!(b["ref"], "refs/heads/gitlarp-base");
        assert_eq!(b["sha"], "head");
    }

    #[test]
    fn chunk_sleeps_between_batches() {
        let mut q: VecDeque<_> = user_and_repo().into();
        q.push_back(nf());
        q.push_back(ok(serde_json::json!({ "sha": "blob" })));
        q.push_back(ok(serde_json::json!({ "sha": "tree" })));
        q.push_back(ok(serde_json::json!({ "sha": "seed" })));
        q.push_back(ok(serde_json::json!({})));
        for i in 0..3 {
            q.push_back(ok(serde_json::json!({ "sha": format!("c{i}") })));
        }
        q.push_back(ok(serde_json::json!({})));
        let m = mock(q);
        let days = [(date!(2026-08-20), 2), (date!(2026-08-21), 1)];
        crate::block_on(write_commits(&m, "pat", "gitlarp-history", &days, None)).unwrap();
        assert_eq!(*m.sleeps.lock().unwrap(), 0, "3 commits fit one chunk of 50");
    }

    #[test]
    fn total_failure_errors() {
        let mut q: VecDeque<_> = user_and_repo().into();
        q.push_back(nf());
        q.push_back(ok(serde_json::json!({ "sha": "blob" })));
        q.push_back(ok(serde_json::json!({ "sha": "tree" })));
        q.push_back(ok(serde_json::json!({ "sha": "seed" })));
        q.push_back(ok(serde_json::json!({})));
        q.push_back(Ok(HttpResponse { status: 500, body: "{\"message\":\"boom\"}".into() }));
        let m = mock(q);
        let days = [(date!(2026-08-20), 1)];
        let r = crate::block_on(write_commits(&m, "pat", "gitlarp-history", &days, None));
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().message, "boom");
    }

    #[test]
    fn wipe_resets_to_base_or_deletes() {
        // base exists -> forced reset
        let mut q: VecDeque<_> = user_and_repo().into();
        q.push_back(ok(serde_json::json!({ "object": { "sha": "base-sha" } })));
        q.push_back(ok(serde_json::json!({}))); // PATCH branch
        let m = mock(q);
        let out = crate::block_on(wipe(&m, "pat", "gitlarp-history", None)).unwrap();
        assert!(matches!(out, WipeOutcome::Reset { sha } if sha == "base-sha"));

        // no base, branch exists -> delete
        let mut q: VecDeque<_> = user_and_repo().into();
        q.push_back(nf()); // base
        q.push_back(ok(serde_json::json!({ "object": { "sha": "head" } })));
        q.push_back(ok(serde_json::json!({}))); // DELETE
        let m = mock(q);
        let out = crate::block_on(wipe(&m, "pat", "gitlarp-history", None)).unwrap();
        assert!(matches!(out, WipeOutcome::Deleted { branch } if branch == "main"));

        // nothing there -> error
        let mut q: VecDeque<_> = user_and_repo().into();
        q.push_back(nf());
        q.push_back(nf());
        let m = mock(q);
        assert!(crate::block_on(wipe(&m, "pat", "gitlarp-history", None)).is_err());
    }
}
