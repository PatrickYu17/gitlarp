use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

use gitlarp_core::plan::Caps;
use gitlarp_core::runner::{RunLimits, Secrets};
use gitlarp_core::schedule::ScheduleSpec;
use gitlarp_core::serde_json::{json, Value};
use gitlarp_core::{
    crypto, date, engine, gh, plan, runner, schedule, store, Error as CoreError, HttpRequest,
    HttpResponse, Runtime,
};
use gitlarp_core::store::Store;
use gitlarp_core::http::BoxFut;
use worker::wasm_bindgen::JsValue;
use worker::{
    console_error, console_log, event, Delay, Env, Fetch, Headers, Method, Request, RequestInit,
    Response, RouteContext, Router,
};
use worker::send::{SendFuture, SendWrapper};

// ---- subrequest budget ------------------------------------------------
// Workers allows 50 fetch subrequests per invocation on the free plan
// (1000 on paid), and the engine spends one fetch per commit plus
// ~5 overhead calls (user, repo, ref, tree, ref update) and one retry
// per rate-limited call. 40 commits/request leaves that headroom, and
// larger requests fail fast with a 400 instead of dying mid-run.
pub const WORKER_PER_RUN: u32 = 40;
pub const WORKER_CAPS: Caps = Caps { per_day: 40, hard_per_day: 40, total: 40 };
/// Schedule secrets protect stored GitHub PATs; the core derives the
/// AES key as a plain SHA-256 of the secret (no KDF stretching), so
/// short secrets are brute-forceable and must be refused.
pub const MIN_SECRET_LEN: usize = 16;

// ---- policy: pure decision logic (plain args only, host-testable) ----
// Not cfg-gated: handlers call these on every target, and the fns touch no
// worker-rs/js types, so wasm builds them cleanly; tests stay behind #[cfg(test)].

mod policy {
    use super::{MIN_SECRET_LEN, ScheduleSpec};

    /// CORS headers stamped on every response (the widget embeds
    /// this API cross-origin); every API path also has a same-path
    /// OPTIONS route so browsers' preflights get them instead of a
    /// bare 405.
    pub const CORS_HEADERS: &[(&str, &str)] = &[
        ("Access-Control-Allow-Origin", "*"),
        ("Access-Control-Allow-Methods", "GET, POST, OPTIONS, DELETE"),
        ("Access-Control-Allow-Headers", "Content-Type, Authorization"),
    ];

    /// Core errors with status 400–499 pass through verbatim; anything
    /// else (5xx, 0, unknown) → 502.
    pub fn core_status(status: u16) -> u16 {
        if (400..500).contains(&status) { status } else { 502 }
    }

    /// `Authorization: Bearer <PAT>` only — the legacy `?pat=` query
    /// fallback is gone (PATs leak via browser history, proxy logs,
    /// and Referer headers); no usable header → None.
    pub fn extract_pat(auth_header: Option<&str>) -> Option<String> {
        auth_header
            .and_then(|h| h.strip_prefix("Bearer "))
            .filter(|t| !t.is_empty())
            .map(str::to_string)
    }

    /// Run endpoint fails closed: secret unset → 503 (never execute),
    /// missing/wrong bearer → 401, exact bearer → Ok.
    pub fn run_gate(secret: Option<&str>, auth_header: Option<&str>) -> Result<(), u16> {
        let Some(secret) = secret else { return Err(503) };
        let Some(auth) = auth_header else { return Err(401) };
        if ct_eq(auth, &format!("Bearer {secret}")) { Ok(()) } else { Err(401) }
    }

    /// Constant-time compare: XOR-accumulate every byte plus the length delta,
    /// so runtime doesn't leak where the first mismatch is.
    fn ct_eq(a: &str, b: &str) -> bool {
        let (a, b) = (a.as_bytes(), b.as_bytes());
        let mut diff = a.len() ^ b.len();
        for (x, y) in a.iter().zip(b) {
            diff |= (*x ^ *y) as usize;
        }
        diff == 0
    }

    /// A schedule secret is the only thing protecting stored GitHub
    /// PATs (the AES key is SHA-256(secret), no stretching), so
    /// anything short is treated as unset: schedule features fail
    /// closed rather than run under a brute-forceable key.
    pub fn usable_secret(s: &str) -> bool {
        s.len() >= MIN_SECRET_LEN
    }

    /// A schedule whose `max` exceeds the per-run budget can never run
    /// without blowing the subrequest ceiling, so the worker rejects
    /// it at create time with a clear error instead of at run time.
    pub fn spec_within_run_limit(spec: &ScheduleSpec, per_run: u32) -> bool {
        spec.max <= per_run
    }

    /// Cleanup pass for the rate_limit table (one row per IP per
    /// window, otherwise unbounded): runs whenever the current window
    /// id is a multiple of `every`.
    pub fn should_cleanup(window: u64, every: u64) -> bool {
        every > 0 && window.is_multiple_of(every)
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum RateDecision {
        Allow,
        Deny { retry_after: u64 },
    }

    /// Fixed-window identity: epoch seconds bucketed into window_secs slots.
    pub fn rate_window_id(now: u64, window_secs: u64) -> u64 {
        now / window_secs
    }

    /// Fixed window admits `limit` requests; over → deny with a flat
    /// Retry-After of one whole window (the contract pins the literal
    /// "Retry-After: 60").
    pub fn rate_decision(window_secs: u64, limit: u32, count: u32) -> RateDecision {
        if count > limit {
            RateDecision::Deny { retry_after: window_secs }
        } else {
            RateDecision::Allow
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        // test-only items from the crate root and core
        use crate::{date, json, plan, WORKER_CAPS};

        #[test]
        fn core_status_4xx_passthrough() {
            assert_eq!(core_status(400), 400);
            assert_eq!(core_status(401), 401);
            assert_eq!(core_status(404), 404);
            assert_eq!(core_status(418), 418);
            assert_eq!(core_status(499), 499);
        }

        #[test]
        fn core_status_everything_else_maps_502() {
            assert_eq!(core_status(0), 502);
            assert_eq!(core_status(399), 502);
            assert_eq!(core_status(500), 502);
            assert_eq!(core_status(503), 502);
            assert_eq!(core_status(5999), 502);
        }

        #[test]
        fn pat_comes_from_bearer_header() {
            assert_eq!(extract_pat(Some("Bearer h")), Some("h".into()));
        }

        /// No usable header — including what used to be query-only
        /// requests — yields None, which handlers answer with 400
        /// "missing pat".
        #[test]
        fn pat_missing_when_no_usable_header() {
            assert_eq!(extract_pat(None), None);
            assert_eq!(extract_pat(Some("Bearer ")), None);
            assert_eq!(extract_pat(Some("Basic x")), None);
        }

        #[test]
        fn rate_boundary_30_per_min_deny_31st() {
            assert_eq!(rate_decision(60, 30, 29), RateDecision::Allow);
            assert_eq!(rate_decision(60, 30, 30), RateDecision::Allow);
            assert_eq!(
                rate_decision(60, 30, 31),
                RateDecision::Deny { retry_after: 60 }
            );
        }

        #[test]
        fn rate_window_id_buckets_seconds() {
            assert_eq!(rate_window_id(0, 60), 0);
            assert_eq!(rate_window_id(59, 60), 0);
            assert_eq!(rate_window_id(125, 60), 2);
            assert_eq!(rate_window_id(180, 60), 3);
        }

        #[test]
        fn run_gate_fails_closed() {
            assert_eq!(run_gate(None, Some("Bearer s")), Err(503));
            assert_eq!(run_gate(None, None), Err(503));
            assert_eq!(run_gate(Some("s"), None), Err(401));
            assert_eq!(run_gate(Some("s"), Some("Bearer wrong")), Err(401));
            assert_eq!(run_gate(Some("s"), Some("Bearer")), Err(401));
            assert_eq!(run_gate(Some("s"), Some("Bearer s")), Ok(()));
        }

        fn spec(min: u32, max: u32) -> ScheduleSpec {
            ScheduleSpec {
                from: None,
                to: None,
                min,
                max,
                weekends: true,
                catchup: gitlarp_core::schedule::DEFAULT_CATCHUP,
            }
        }

        #[test]
        fn usable_secret_requires_min_length() {
            assert!(!usable_secret(""));
            assert!(!usable_secret("hunter2"));
            assert!(!usable_secret("15-char-secret"), "15 chars is too short");
            assert!(usable_secret("0123456789abcdef"), "16 chars is the minimum");
            assert!(usable_secret("definitely-not-a-short-secret"));
        }

        #[test]
        fn spec_within_run_limit_boundaries() {
            assert!(spec_within_run_limit(&spec(1, 40), 40));
            assert!(!spec_within_run_limit(&spec(1, 41), 40));
            assert!(!spec_within_run_limit(&spec(45, 100), 40));
        }

        #[test]
        fn should_cleanup_fires_on_every_nth_window() {
            assert!(should_cleanup(0, 10));
            assert!(should_cleanup(10, 10));
            assert!(should_cleanup(120, 10));
            assert!(!should_cleanup(1, 10));
            assert!(!should_cleanup(9, 10));
            assert!(!should_cleanup(10, 0), "every=0 disables cleanup entirely");
        }

        /// The worker caps are wired into the shared core validator: a
        /// request over the subrequest budget fails fast at parse time.
        #[test]
        fn worker_caps_reject_oversized_plans() {
            let today = date::parse("2026-09-11").unwrap();
            let over = json!([{ "date": "2026-09-11", "count": 41 }]);
            assert!(plan::parse_api_plan_capped(&over, today, WORKER_CAPS).is_err());
            let two_days_over_total = json!([
                { "date": "2026-09-10", "count": 25 },
                { "date": "2026-09-11", "count": 25 },
            ]);
            assert!(plan::parse_api_plan_capped(&two_days_over_total, today, WORKER_CAPS).is_err());
            let fits = json!([{ "date": "2026-09-11", "count": 40 }]);
            assert!(plan::parse_api_plan_capped(&fits, today, WORKER_CAPS).is_ok());
        }
    }
}

// ---- Runtime: the platform seam ----

struct WorkerRuntime;

impl Runtime for WorkerRuntime {
    fn fetch(&self, req: HttpRequest) -> BoxFut<Result<HttpResponse, CoreError>> {
        Box::pin(SendFuture::new(async move {
            let mut init = RequestInit::new();
            init.with_method(Method::from(req.method.clone()));
            let headers = Headers::new();
            for (k, v) in &req.headers {
                headers
                    .set(k, v)
                    .map_err(|e| CoreError::new(500, e.to_string()))?;
            }
            init.with_headers(headers);
            init.with_body(req.body.as_deref().map(JsValue::from_str));
            let wreq = Request::new_with_init(&req.url, &init)
                .map_err(|e| CoreError::new(500, e.to_string()))?;
            let mut res = Fetch::Request(wreq)
                .send()
                .await
                .map_err(|e| CoreError::new(502, format!("github unreachable: {e}")))?;
            let body = res
                .text()
                .await
                .map_err(|e| CoreError::new(502, format!("github unreadable: {e}")))?;
            Ok(HttpResponse { status: res.status_code(), body })
        }))
    }

    fn sleep(&self, ms: u64) -> BoxFut<()> {
        Box::pin(SendFuture::new(Delay::from(Duration::from_millis(ms))))
    }

    fn random(&self, buf: &mut [u8]) {
        random_bytes(buf);
    }
}

// infallible: the Workers runtime always exposes crypto.getRandomValues; the
// Runtime::random trait signature is also infallible, so errors can't propagate
fn random_bytes(buf: &mut [u8]) {
    let arr = js_sys::Uint8Array::new_with_length(buf.len() as u32);
    let crypto = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("crypto"))
        .expect("global crypto");
    let f = js_sys::Reflect::get(&crypto, &JsValue::from_str("getRandomValues"))
        .expect("crypto.getRandomValues");
    js_sys::Function::from(f)
        .call1(&crypto, &arr)
        .expect("getRandomValues");
    arr.copy_to(buf);
}

// ---- D1Store: port of the original TS d1Store() from
// apps/web/src/lib/store.ts, since removed from the web app ----

struct D1Store {
    db: SendWrapper<Rc<worker::D1Database>>,
}

thread_local! {
    static TABLE_READY: Cell<bool> = const { Cell::new(false) };
}

fn d1_err(e: worker::Error) -> CoreError {
    CoreError::new(500, format!("d1: {e}"))
}

impl D1Store {
    fn new(db: worker::D1Database) -> Self {
        D1Store { db: SendWrapper::new(Rc::new(db)) }
    }
}

async fn ensure_table(db: &worker::D1Database) -> Result<(), CoreError> {
    if !TABLE_READY.with(Cell::get) {
        db.prepare(
            "CREATE TABLE IF NOT EXISTS schedules (\
             user_id TEXT NOT NULL, id TEXT NOT NULL, payload TEXT NOT NULL, \
             PRIMARY KEY (user_id, id))",
        )
        .run()
        .await
        .map_err(d1_err)?;
        TABLE_READY.with(|c| c.set(true));
    }
    Ok(())
}

impl store::Store for D1Store {
    fn list_users(&self) -> BoxFut<Result<Vec<String>, CoreError>> {
        let db = self.db.clone();
        Box::pin(SendFuture::new(async move {
            ensure_table(&db).await?;
            #[derive(serde::Deserialize)]
            struct Row {
                user_id: String,
            }
            Ok(db
                .prepare("SELECT DISTINCT user_id FROM schedules")
                .all()
                .await
                .map_err(d1_err)?
                .results::<Row>()
                .map_err(d1_err)?
                .into_iter()
                .map(|r| r.user_id)
                .collect())
        }))
    }

    fn list(&self, user: &str) -> BoxFut<Result<Vec<store::ScheduleRecord>, CoreError>> {
        let db = self.db.clone();
        let user = user.to_string();
        Box::pin(SendFuture::new(async move {
            ensure_table(&db).await?;
            #[derive(serde::Deserialize)]
            struct Row {
                id: String,
                payload: String,
            }
            Ok(db
                .prepare("SELECT id, payload FROM schedules WHERE user_id = ?")
                .bind(&[JsValue::from_str(&user)])
                .map_err(d1_err)?
                .all()
                .await
                .map_err(d1_err)?
                .results::<Row>()
                .map_err(d1_err)?
                .into_iter()
                .map(|r| store::ScheduleRecord { id: r.id, payload: r.payload })
                .collect())
        }))
    }

    fn get(&self, user: &str, id: &str) -> BoxFut<Result<Option<String>, CoreError>> {
        let db = self.db.clone();
        let user = user.to_string();
        let id = id.to_string();
        Box::pin(SendFuture::new(async move {
            ensure_table(&db).await?;
            db.prepare("SELECT payload FROM schedules WHERE user_id = ? AND id = ?")
                .bind(&[
                    JsValue::from_str(&user),
                    JsValue::from_str(&id),
                ])
                .map_err(d1_err)?
                .first::<String>(Some("payload"))
                .await
                .map_err(d1_err)
        }))
    }

    fn put(&self, user: &str, id: &str, payload: &str) -> BoxFut<Result<(), CoreError>> {
        let db = self.db.clone();
        let (user, id, payload) = (user.to_string(), id.to_string(), payload.to_string());
        Box::pin(SendFuture::new(async move {
            ensure_table(&db).await?;
            db.prepare("INSERT OR REPLACE INTO schedules (user_id, id, payload) VALUES (?, ?, ?)")
                .bind(&[
                    JsValue::from_str(&user),
                    JsValue::from_str(&id),
                    JsValue::from_str(&payload),
                ])
                .map_err(d1_err)?
                .run()
                .await
                .map_err(d1_err)?;
            Ok(())
        }))
    }

    fn delete(&self, user: &str, id: &str) -> BoxFut<Result<(), CoreError>> {
        let db = self.db.clone();
        let (user, id) = (user.to_string(), id.to_string());
        Box::pin(SendFuture::new(async move {
            ensure_table(&db).await?;
            db.prepare("DELETE FROM schedules WHERE user_id = ? AND id = ?")
                .bind(&[
                    JsValue::from_str(&user),
                    JsValue::from_str(&id),
                ])
                .map_err(d1_err)?
                .run()
                .await
                .map_err(d1_err)?;
            Ok(())
        }))
    }
}

// ---- helpers ----

fn cors(mut res: Response) -> Response {
    let h = res.headers_mut();
    for (k, v) in policy::CORS_HEADERS {
        let _ = h.set(k, v);
    }
    res
}

fn json_res(value: Value, status: u16) -> worker::Result<Response> {
    Ok(cors(Response::from_json(&value)?.with_status(status)))
}

fn err_json(msg: &str, status: u16) -> worker::Result<Response> {
    json_res(json!({ "error": msg }), status)
}

fn core_err(e: CoreError) -> worker::Result<Response> {
    json_res(json!({ "error": e.message }), policy::core_status(e.status))
}

fn to_value<T: serde::Serialize>(v: &T) -> Value {
    // infallible: only core's plain response structs/values are serialized here
    gitlarp_core::serde_json::to_value(v).expect("serialize")
}

fn schedule_secret(env: &Env) -> Option<String> {
    env.secret("GITLARP_SCHEDULE_SECRET")
        .or_else(|_| env.var("GITLARP_SCHEDULE_SECRET"))
        .ok()
        .map(|s| s.to_string())
        .filter(|s| {
            if policy::usable_secret(s) {
                true
            } else {
                console_error!(
                    "[gitlarp] GITLARP_SCHEDULE_SECRET is shorter than {MIN_SECRET_LEN} bytes; \
                     schedule features are disabled (fail closed)"
                );
                false
            }
        })
}

fn q(req: &Request, key: &str) -> Option<String> {
    req.url()
        .ok()?
        .query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

fn date_str(d: &js_sys::Date) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        d.get_full_year(),
        d.get_month() + 1,
        d.get_date()
    )
}

fn today() -> Result<time::Date, CoreError> {
    date::parse(&date_str(&js_sys::Date::new_0()))
}

fn uuid() -> Result<String, CoreError> {
    let err = |e: JsValue| CoreError::new(500, format!("uuid unavailable: {e:?}"));
    let crypto = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("crypto"))
        .map_err(err)?;
    let f = js_sys::Reflect::get(&crypto, &JsValue::from_str("randomUUID"))
        .map_err(err)?;
    let v = js_sys::Function::from(f).call0(&crypto).map_err(err)?;
    v.as_string().ok_or_else(|| CoreError::new(500, "uuid: non-string return"))
}

fn d1(env: &Env) -> worker::Result<D1Store> {
    Ok(D1Store::new(env.d1("GITLARP_DB")?))
}

// fixed-window rate limiter (D1-backed: in-memory state is useless across isolates)
const RATE_WINDOW_SECS: u64 = 60;
const RATE_LIMIT: u32 = 30;
/// Stale-window housekeeping runs whenever the current window id is a
/// multiple of this (i.e. once every 10 minutes).
const RATE_CLEANUP_EVERY: u64 = 10;

thread_local! {
    static RATE_TABLE_READY: Cell<bool> = const { Cell::new(false) };
}

async fn ensure_rate_table(db: &worker::D1Database) -> Result<(), CoreError> {
    if !RATE_TABLE_READY.with(Cell::get) {
        db.prepare(
            "CREATE TABLE IF NOT EXISTS rate_limit (\
             key TEXT NOT NULL, window INTEGER NOT NULL, count INTEGER NOT NULL, \
             PRIMARY KEY (key, window))",
        )
        .run()
        .await
        .map_err(d1_err)?;
        RATE_TABLE_READY.with(|c| c.set(true));
    }
    Ok(())
}

/// Returns `Some(429 response)` when this IP has exceeded the window budget,
/// `None` to proceed. Atomic upsert-and-count via `RETURNING`.
/// Stale windows are deleted once every RATE_CLEANUP_EVERY windows; the
/// cleanup is best-effort and never blocks or fails the request.
async fn rate_gate(env: &Env, req: &Request) -> worker::Result<Option<Response>> {
    let db = env.d1("GITLARP_DB")?;
    ensure_rate_table(&db)
        .await
        .map_err(|e| worker::Error::from(e.message.as_str()))?;
    let ip = req
        .headers()
        .get("CF-Connecting-IP")
        .ok()
        .flatten()
        .unwrap_or_else(|| "unknown".to_string());
    let now = (js_sys::Date::now() / 1000.0) as u64;
    let window = policy::rate_window_id(now, RATE_WINDOW_SECS);
    if policy::should_cleanup(window, RATE_CLEANUP_EVERY) {
        // best-effort: rows for closed windows only
        let _ = async {
            db.prepare("DELETE FROM rate_limit WHERE window < ?")
                .bind(&[JsValue::from_f64(window.saturating_sub(1) as f64)])?
                .run()
                .await?;
            Ok::<(), worker::Error>(())
        }
        .await;
    }
    let count = db
        .prepare(
            "INSERT INTO rate_limit (key, window, count) VALUES (?, ?, 1) \
             ON CONFLICT(key, window) DO UPDATE SET count = count + 1 RETURNING count",
        )
        .bind(&[JsValue::from_str(&ip), JsValue::from_f64(window as f64)])?
        .first::<i64>(Some("count"))
        .await?
        // fail CLOSED: a NULL/absent count is an anomaly; deny rather
        // than treat it as "at the limit but allowed"
        .unwrap_or(i64::MAX);
    let count = u32::try_from(count).unwrap_or(u32::MAX);
    Ok(match policy::rate_decision(RATE_WINDOW_SECS, RATE_LIMIT, count) {
        policy::RateDecision::Allow => None,
        policy::RateDecision::Deny { retry_after } => {
            let mut resp = err_json("rate limited", 429)?;
            let _ = resp.headers_mut().set("Retry-After", &retry_after.to_string());
            Some(resp)
        }
    })
}

// ---- routes ----

/// Runner with worker policy: the run is limited to WORKER_PER_RUN
/// commits per schedule record and — the ~50-fetch subrequest
/// ceiling is per cron INVOCATION, not per record — to the same
/// budget shared across every record, with deferred days and
/// records staying due (see runner::RunLimits). Per-day counts are
/// capped at the same budget so records created elsewhere (or
/// before this cap existed) cannot blow the subrequest ceiling
/// either.
async fn run_schedules(
    rt: &dyn Runtime,
    store: &dyn store::Store,
    secret: &str,
    today: time::Date,
) -> Result<runner::RunnerResult, CoreError> {
    let mut rng = |lo: u32, hi: u32| {
        let mut b = [0u8; 4];
        random_bytes(&mut b);
        (lo + u32::from_le_bytes(b) % (hi.saturating_sub(lo) + 1)).min(WORKER_PER_RUN)
    };
    let log = |line: &str| console_log!("[gitlarp] {line}");
    runner::run_due_schedules_limited(
        rt,
        store,
        // no old-secret support on the worker: rotated records stay
        // skipped until a GET /api/schedules heals them
        &Secrets { current: secret, old: None },
        today,
        &mut rng,
        &log,
        RunLimits {
            max_commits_per_run: WORKER_PER_RUN,
            max_commits_total: Some(WORKER_PER_RUN),
        },
    )
    .await
}

async fn options(_req: Request, _ctx: RouteContext<()>) -> worker::Result<Response> {
    Ok(cors(Response::empty()?.with_status(204)))
}

async fn commits(mut req: Request, _ctx: RouteContext<()>) -> worker::Result<Response> {
    let body = match req.json::<Value>().await {
        Ok(v) => v,
        Err(_) => return err_json("invalid JSON", 400),
    };
    let Some(pat) = body["pat"].as_str().filter(|p| !p.is_empty()) else {
        return err_json("missing pat", 400);
    };
    let pat = pat.to_string();
    let today = match today() {
        Ok(d) => d,
        Err(e) => return core_err(e),
    };
    // subrequest-safe caps: see WORKER_CAPS above
    let p = match plan::parse_api_plan_capped(&body["days"], today, WORKER_CAPS) {
        Ok(p) => p,
        Err(e) => return core_err(e),
    };
    match engine::write_commits(&WorkerRuntime, &pat, gh::DEFAULT_REPO, &p.days, None).await {
        Ok(r) => {
            let mut out = json!({ "created": r.created, "total": r.total });
            if let Some(err) = &r.partial {
                out["partial"] = json!(true);
                out["error"] = json!(err);
            }
            if p.clamped > 0 {
                out["clamped"] = json!(p.clamped);
            }
            json_res(out, 200)
        }
        Err(e) => core_err(e),
    }
}

async fn graph(req: Request, _ctx: RouteContext<()>) -> worker::Result<Response> {
    let auth = req.headers().get("Authorization").ok().flatten();
    let Some(pat) = policy::extract_pat(auth.as_deref()) else {
        return err_json("missing pat", 400);
    };
    let gh = gh::GhClient::new(&WorkerRuntime, &pat, gh::DEFAULT_REPO);
    let res = async {
        let (_, login) = gh.user().await?;
        let counts = gh.contributions_graph(&login).await?;
        Ok::<_, CoreError>(json!({ "counts": to_value(&counts) }))
    }
    .await;
    match res {
        Ok(v) => json_res(v, 200),
        Err(e) => core_err(e),
    }
}

async fn schedules_get(req: Request, ctx: RouteContext<()>) -> worker::Result<Response> {
    let auth = req.headers().get("Authorization").ok().flatten();
    let Some(pat) = policy::extract_pat(auth.as_deref()) else {
        return err_json("missing pat", 400);
    };
    let Some(secret) = schedule_secret(&ctx.env) else {
        return err_json("schedule secret not configured", 503);
    };
    let store = d1(&ctx.env)?;
    let res = async {
        let (uid, _) = gh::GhClient::new(&WorkerRuntime, &pat, gh::DEFAULT_REPO)
            .user()
            .await?;
        let user = uid.to_string();
        let mut out = Vec::new();
        for rec in store.list(&user).await? {
            match crypto::decrypt_json::<runner::StoredSchedule>(&secret, &rec.payload) {
                Ok(data) => out.push(json!({
                    "id": rec.id,
                    "spec": to_value(&data.spec),
                    "lastRun": date::fmt(data.last_run),
                    "createdAt": data.created_at,
                })),
                Err(_) => out.push(json!({ "id": rec.id, "broken": true })),
            }
        }
        Ok::<_, CoreError>(json!({ "schedules": out }))
    }
    .await;
    match res {
        Ok(v) => json_res(v, 200),
        Err(e) => core_err(e),
    }
}

async fn schedules_post(mut req: Request, ctx: RouteContext<()>) -> worker::Result<Response> {
    let body = match req.json::<Value>().await {
        Ok(v) => v,
        Err(_) => return err_json("invalid JSON", 400),
    };
    let Some(pat) = body["pat"].as_str().filter(|p| !p.is_empty()) else {
        return err_json("missing pat", 400);
    };
    let pat = pat.to_string();
    let Some(secret) = schedule_secret(&ctx.env) else {
        return err_json("schedule secret not configured", 503);
    };
    let spec = match schedule::parse_spec(&body) {
        Ok(s) => s,
        Err(e) => return core_err(e),
    };
    if !policy::spec_within_run_limit(&spec, WORKER_PER_RUN) {
        return err_json(
            &format!("max must be <= {WORKER_PER_RUN} on this worker (subrequest limits)"),
            400,
        );
    }
    let store = d1(&ctx.env)?;
    let res = async {
        let (uid, _) = gh::GhClient::new(&WorkerRuntime, &pat, gh::DEFAULT_REPO)
            .user()
            .await?;
        let user = uid.to_string();
        let now = date_str(&js_sys::Date::new_0());
        let data = runner::StoredSchedule {
            pat,
            spec,
            last_run: date::parse(&now)?,
            created_at: now,
        };
        let id = uuid()?;
        let blob = crypto::encrypt_json(&secret, &data)?;
        store.put(&user, &id, &blob).await?;
        Ok::<_, CoreError>(json!({ "id": id }))
    }
    .await;
    match res {
        Ok(v) => json_res(v, 200),
        Err(e) => core_err(e),
    }
}

async fn schedules_delete(req: Request, ctx: RouteContext<()>) -> worker::Result<Response> {
    let auth = req.headers().get("Authorization").ok().flatten();
    let Some(pat) = policy::extract_pat(auth.as_deref()) else {
        return err_json("need pat and id", 400);
    };
    let Some(id) = q(&req, "id").filter(|i| !i.is_empty()) else {
        return err_json("need pat and id", 400);
    };
    // No secret needed: deletion never decrypts the stored payload.
    let store = d1(&ctx.env)?;
    let res = async {
        let (uid, _) = gh::GhClient::new(&WorkerRuntime, &pat, gh::DEFAULT_REPO)
            .user()
            .await?;
        let user = uid.to_string();
        if store.get(&user, &id).await?.is_none() {
            return Err(CoreError::new(404, "no such schedule"));
        }
        store.delete(&user, &id).await?;
        Ok::<_, CoreError>(json!({ "deleted": true }))
    }
    .await;
    match res {
        Ok(v) => json_res(v, 200),
        Err(e) => core_err(e),
    }
}

async fn schedules_run(req: Request, ctx: RouteContext<()>) -> worker::Result<Response> {
    let secret = schedule_secret(&ctx.env);
    let auth = req.headers().get("Authorization").ok().flatten();
    match policy::run_gate(secret.as_deref(), auth.as_deref()) {
        Ok(()) => {}
        Err(503) => return err_json("schedule secret not configured", 503),
        Err(_) => return err_json("unauthorized", 401),
    }
    let store = d1(&ctx.env)?;
    let today = match today() {
        Ok(d) => d,
        Err(e) => return core_err(e),
    };
    match run_schedules(
        &WorkerRuntime,
        &store,
        secret.as_deref().unwrap_or(""),
        today,
    )
    .await
    {
        Ok(r) => json_res(to_value(&r), 200),
        Err(e) => core_err(e),
    }
}

async fn healthz(_req: Request, _ctx: RouteContext<()>) -> worker::Result<Response> {
    json_res(json!({ "ok": true }), 200)
}

// ---- route table + entry points -----------------------------------------
// The route table is data: the handlers are wasm-bound (Request/Env
// only run inside the Workers runtime), so registration, CORS
// preflight coverage, and rate-gate wiring live here where the host
// test suite can pin them.

/// The future every route returns; boxed so all handler fns share
/// one registration shape.
type RouteFut = Pin<Box<dyn Future<Output = worker::Result<Response>>>>;
type RouteFn = Box<dyn Fn(Request, RouteContext<()>) -> RouteFut>;

/// Which handler a route dispatches to; every handler fn has the
/// same worker-rs signature, so the table stores a tag.
#[derive(Clone, Copy)]
enum H {
    Healthz,
    Commits,
    Options,
    Graph,
    SchedulesGet,
    SchedulesPost,
    SchedulesDelete,
    SchedulesRun,
}

struct Route {
    method: Method,
    path: &'static str,
    /// Gated rows consult the D1 rate limiter before their handler:
    /// every route that talks to upstream GitHub burns the 30/min
    /// window. Preflights and healthz stay exempt — they cost
    /// nothing, and browsers send preflights mechanically.
    gated: bool,
    handler: H,
}

const ROUTES: &[Route] = &[
    Route { method: Method::Get, path: "/healthz", gated: false, handler: H::Healthz },
    Route { method: Method::Post, path: "/api/commits", gated: true, handler: H::Commits },
    Route { method: Method::Options, path: "/api/commits", gated: false, handler: H::Options },
    Route { method: Method::Get, path: "/api/graph", gated: true, handler: H::Graph },
    Route { method: Method::Options, path: "/api/graph", gated: false, handler: H::Options },
    Route { method: Method::Get, path: "/api/schedules", gated: true, handler: H::SchedulesGet },
    Route { method: Method::Post, path: "/api/schedules", gated: true, handler: H::SchedulesPost },
    Route { method: Method::Delete, path: "/api/schedules", gated: true, handler: H::SchedulesDelete },
    Route { method: Method::Options, path: "/api/schedules", gated: false, handler: H::Options },
    Route { method: Method::Post, path: "/api/schedules/run", gated: true, handler: H::SchedulesRun },
    Route { method: Method::Options, path: "/api/schedules/run", gated: false, handler: H::Options },
];

/// The closure registered per row: gated rows consult the limiter
/// first (429 + Retry-After once the window budget is spent), then
/// dispatch on the tag.
fn route(r: &'static Route) -> RouteFn {
    Box::new(move |req: Request, ctx: RouteContext<()>| {
        let fut: RouteFut = Box::pin(async move {
            if r.gated {
                if let Some(resp) = rate_gate(&ctx.env, &req).await? {
                    return Ok(resp);
                }
            }
            match r.handler {
                H::Healthz => healthz(req, ctx).await,
                H::Commits => commits(req, ctx).await,
                H::Options => options(req, ctx).await,
                H::Graph => graph(req, ctx).await,
                H::SchedulesGet => schedules_get(req, ctx).await,
                H::SchedulesPost => schedules_post(req, ctx).await,
                H::SchedulesDelete => schedules_delete(req, ctx).await,
                H::SchedulesRun => schedules_run(req, ctx).await,
            }
        });
        fut
    })
}

fn build_router() -> Router<'static, ()> {
    let mut router: Router<'static, ()> = Router::new();
    for r in ROUTES {
        router = match r.method {
            Method::Get => router.get_async(r.path, route(r)),
            Method::Post => router.post_async(r.path, route(r)),
            Method::Delete => router.delete_async(r.path, route(r)),
            Method::Options => router.options_async(r.path, route(r)),
            _ => unreachable!("the route table only registers these methods"),
        };
    }
    router
}

#[event(fetch)]
async fn main(req: Request, env: Env, _ctx: worker::Context) -> worker::Result<Response> {
    build_router().run(req, env).await
}

#[event(scheduled)]
async fn scheduled(event: worker::ScheduledEvent, env: Env, _ctx: worker::ScheduleContext) {
    let rt = WorkerRuntime;
    let Ok(db) = env.d1("GITLARP_DB") else {
        console_error!("[gitlarp] scheduled: GITLARP_DB binding missing");
        return;
    };
    let store = D1Store::new(db);
    let d = js_sys::Date::new_0();
    d.set_time(event.schedule());
    let today = match date::parse(&date_str(&d)) {
        Ok(t) => t,
        Err(e) => {
            console_error!("[gitlarp] scheduled: {e}");
            return;
        }
    };
    let secret = schedule_secret(&env);
    if secret.is_none() {
        console_error!(
            "[gitlarp] scheduled: GITLARP_SCHEDULE_SECRET missing or too short; nothing to run"
        );
        return;
    }
    match run_schedules(&rt, &store, secret.as_deref().unwrap_or(""), today).await {
        Ok(r) => console_log!("[gitlarp] scheduled run: {r:?}"),
        Err(e) => console_error!("[gitlarp] scheduled failed: {e}"),
    }
}

// ---- host tests ---------------------------------------------------------
// Request/Env/Router are wasm-bound, so the host suite pins the route
// table those handlers hang off of: registration, preflight coverage,
// and rate-gate wiring are all data in ROUTES.

#[cfg(test)]
mod tests {
    use super::*;

    fn registered(method: Method, path: &str) -> bool {
        ROUTES.iter().any(|r| r.method == method && r.path == path)
    }

    /// The table registers cleanly: a duplicate or conflicting pattern
    /// would panic inside matchit at registration, so wiring the
    /// router once on host catches table typos before deploy.
    #[test]
    fn route_table_registers_without_conflicts() {
        let _router = build_router();
    }

    /// The exact (method, path) set the worker serves; the widget and
    /// scripts call every one of these, and the graph preflight went
    /// missing once already (browsers got a bare 405).
    #[test]
    fn route_table_matches_the_public_api() {
        let served: Vec<String> =
            ROUTES.iter().map(|r| format!("{:?} {}", r.method, r.path)).collect();
        for route in [
            "Get /healthz",
            "Post /api/commits",
            "Options /api/commits",
            "Get /api/graph",
            "Options /api/graph",
            "Get /api/schedules",
            "Post /api/schedules",
            "Delete /api/schedules",
            "Options /api/schedules",
            "Post /api/schedules/run",
            "Options /api/schedules/run",
        ] {
            assert!(served.contains(&route.to_string()), "missing route {route}");
        }
        assert_eq!(served.len(), 11, "no stray routes: {served:?}");
    }

    /// Cross-origin widget fetches preflight every request that carries
    /// an Authorization header; an unregistered OPTIONS method gets a
    /// bare 405 with no CORS headers and the browser never sends the
    /// real request. Every API path needs a same-path preflight
    /// (healthz is exempt: a safelisted simple GET).
    #[test]
    fn every_api_path_answers_preflight() {
        let api_paths: Vec<&str> = ROUTES
            .iter()
            .filter(|r| r.method != Method::Options && r.path != "/healthz")
            .map(|r| r.path)
            .collect();
        assert!(!api_paths.is_empty());
        for path in api_paths {
            assert!(registered(Method::Options, path), "no preflight for {path}");
        }
        assert!(
            registered(Method::Options, "/api/graph"),
            "the widget reads /api/graph cross-origin"
        );
    }

    /// The preflight handler answers with the headers browsers need to
    /// send the real cross-origin request: any origin, the registered
    /// methods (incl. OPTIONS), and the Authorization header the API
    /// authenticates with.
    #[test]
    fn preflight_headers_allow_origin_methods_and_authorization() {
        let get = |k: &str| {
            policy::CORS_HEADERS
                .iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| *v)
                .unwrap_or_default()
        };
        assert_eq!(get("Access-Control-Allow-Origin"), "*");
        assert!(get("Access-Control-Allow-Methods").contains("OPTIONS"));
        assert!(get("Access-Control-Allow-Headers").contains("Authorization"));
    }

    /// Every route that talks to upstream GitHub burns the 30/min
    /// window — graph and schedules_get included, which is what the
    /// 429 + Retry-After denial hangs off of. Preflights and healthz
    /// never do: browsers send preflights mechanically, and healthz
    /// must stay up while an IP is throttled.
    #[test]
    fn upstream_talking_routes_are_rate_gated() {
        for r in ROUTES {
            let gated = r.method != Method::Options && r.path != "/healthz";
            assert_eq!(
                r.gated, gated,
                "{:?} {} is wrongly {}",
                r.method,
                r.path,
                if gated { "ungated" } else { "gated" }
            );
        }
    }
}
