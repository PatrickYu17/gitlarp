//! gitlarp-server: the hardened actix-web API over gitlarp-core.
//!
//! Endpoints:
//!
//! - `POST /api/commits`: write backdated commits (pat in body)
//! - `GET /api/graph`: contribution calendar for a PAT
//! - `GET /api/schedules`: list a user's schedules (decrypted)
//! - `POST /api/schedules`: create a schedule (pat in body)
//! - `DELETE /api/schedules`: delete one by id (?id=)
//! - `POST /api/schedules/run`: run every due schedule (bearer = secret)
//! - `GET /healthz`: liveness, no auth
//!
//! Hardening: PAT moves in the `Authorization: Bearer` header (legacy
//! `?pat=` still honored), 1 MiB bodies, 30s client timeout, 30/min
//! fixed-window rate limit per IP on every endpoint that talks to
//! upstream GitHub (all mutations + `/api/graph`, whose two upstream
//! calls per request would otherwise let anonymous clients burn the
//! server's egress IP against GitHub's unauthenticated quota), a
//! process-wide lock serializing commit writes (the engine is not
//! atomic against concurrent runs: see engine.rs), fail-closed
//! runner gating with a constant-time bearer compare, and lazy
//! secret rotation heal (`GITLARP_SCHEDULE_SECRET_OLD`). The schedule
//! secret must be at least MIN_SECRET_LEN chars: it is the only thing
//! protecting stored PATs and the AES key is a plain SHA-256 of it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use actix_web::dev::{Service as _, ServiceFactory, ServiceRequest, ServiceResponse};
use actix_web::http::header::AUTHORIZATION;
use actix_web::http::{Method, StatusCode};
use actix_web::middleware::{DefaultHeaders, Logger};
use actix_web::{error::InternalError, web, App, HttpRequest, HttpResponse, HttpServer};
use actix_web::error::ResponseError as _;
use gitlarp_core::http::{
    BoxFut, HttpRequest as CoreRequest, HttpResponse as CoreResponse,
};
use gitlarp_core::runner::{run_due_schedules, StoredSchedule};
use gitlarp_core::store::file::FileStore;
use gitlarp_core::store::Store;
use gitlarp_core::{crypto, date, engine, gh, plan, schedule, Error, Runtime};
use serde_json::{json, Value};

/// Max request body (applies to every extractor).
const BODY_LIMIT: usize = 1024 * 1024;
/// Fixed-window rate limit: 30 requests per 60s per IP.
const RATE_MAX_PER_WINDOW: u32 = 30;
const RATE_WINDOW_SECS: u64 = 60;
/// The schedule secret derives the AES key for stored PATs via plain
/// SHA-256 (no KDF stretching), so anything shorter is brute-forceable.
const MIN_SECRET_LEN: usize = 16;

// ---------------------------------------------------------------------------
// runtime seam
// ---------------------------------------------------------------------------

type FetchOverride = Arc<dyn Fn(CoreRequest) -> Result<CoreResponse, Error> + Send + Sync>;

/// The production `Runtime`: reqwest for fetch, tokio for sleep, OS
/// entropy for random. `fetch_override` is the test seam: when set,
/// every upstream call short-circuits into it (production leaves it
/// `None`).
struct ServerRuntime {
    http: reqwest::Client,
    fetch_override: Option<FetchOverride>,
}

impl ServerRuntime {
    fn new(fetch_override: Option<FetchOverride>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("gitlarp")
            .build()
            .expect("reqwest client builds");
        Self { http, fetch_override }
    }
}

impl Runtime for ServerRuntime {
    fn fetch(&self, req: CoreRequest) -> BoxFut<Result<CoreResponse, Error>> {
        if let Some(f) = &self.fetch_override {
            return Box::pin(std::future::ready(f(req)));
        }
        let client = self.http.clone();
        let CoreRequest { method, url, headers, body } = req;
        Box::pin(async move {
            let mut b = match method.as_str() {
                "GET" => client.get(&url),
                "POST" => client.post(&url),
                "PATCH" => client.patch(&url),
                "PUT" => client.put(&url),
                "DELETE" => client.delete(&url),
                _ => return Err(Error::new(400, format!("unsupported method: {method}"))),
            };
            for (k, v) in &headers {
                b = b.header(k.as_str(), v.as_str());
            }
            if let Some(body) = &body {
                b = b.body(body.clone());
            }
            let res = b
                .send()
                .await
                .map_err(|e| Error::new(502, format!("upstream unreachable: {e}")))?;
            let status = res.status().as_u16();
            let body = res
                .text()
                .await
                .map_err(|e| Error::new(502, format!("upstream read failed: {e}")))?;
            Ok(CoreResponse { status, body })
        })
    }

    fn sleep(&self, ms: u64) -> BoxFut<()> {
        Box::pin(tokio::time::sleep(Duration::from_millis(ms)))
    }

    fn random(&self, buf: &mut [u8]) {
        getrandom::getrandom(buf).expect("OS entropy unavailable");
    }
}

// ---------------------------------------------------------------------------
// app state
// ---------------------------------------------------------------------------

/// In-memory fixed-window limiter: `(ip, window) -> count`.
struct RateLimiter {
    hits: Mutex<HashMap<(String, u64), u32>>,
}

impl RateLimiter {
    fn new() -> Self {
        Self { hits: Mutex::new(HashMap::new()) }
    }

    /// Records one hit and returns whether it is under the limit.
    fn allow(&self, ip: &str) -> bool {
        let Ok(mut hits) = self.hits.lock() else { return false };
        let window = epoch_secs() / RATE_WINDOW_SECS;
        // opportunistically drop closed windows so the map stays small
        hits.retain(|(_, w), _| *w == window);
        let count = hits.entry((ip.to_string(), window)).or_insert(0);
        *count += 1;
        *count <= RATE_MAX_PER_WINDOW
    }
}

fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

struct AppState {
    rt: Arc<ServerRuntime>,
    store: Arc<dyn Store>,
    /// Current schedule-secret; unset means schedule features fail closed.
    secret: Option<String>,
    /// Previous secret, for lazy rotation heal.
    old_secret: Option<String>,
    limiter: RateLimiter,
    /// Serializes every commit-writing path (POST /api/commits, the
    /// runner, the hourly loop): engine runs are not atomic against
    /// each other, and interleaved runs lose commits to force-patched
    /// refs. Single-process by design — the in-memory limiter already
    /// assumes one instance; replicas need external coordination.
    write_lock: Arc<tokio::sync::Mutex<()>>,
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Core errors carry an HTTP status: 400–499 pass through verbatim,
/// anything else is an upstream failure → 502.
fn core_error(e: Error) -> HttpResponse {
    let status = if (400..500).contains(&e.status) { e.status } else { 502 };
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    HttpResponse::build(code).json(json!({ "error": e.message }))
}

fn bad_request(msg: &str) -> HttpResponse {
    HttpResponse::BadRequest().json(json!({ "error": msg }))
}

fn not_configured() -> HttpResponse {
    HttpResponse::ServiceUnavailable()
        .json(json!({ "error": "schedule secret not configured" }))
}

/// Hand-rolled constant-time compare: XOR-accumulates the content
/// deltas *and* the length delta, so timing does not leak a match
/// prefix. No `subtle` dependency needed for one comparison.
fn const_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= (*x ^ *y) as usize;
    }
    diff == 0
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn os_seed_u64() -> u64 {
    let mut b = [0u8; 8];
    getrandom::getrandom(&mut b).expect("OS entropy unavailable");
    u64::from_ne_bytes(b)
}

/// xorshift64 PRNG for fill counts (not a nonce source; nonces come
/// from `crypto::random_iv` / OS entropy only).
fn xorshift_rng(seed: u64) -> impl FnMut(u32, u32) -> u32 {
    let mut s = seed.max(1);
    move |lo: u32, hi: u32| {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        if hi <= lo {
            return lo;
        }
        lo + (s % ((hi - lo) as u64 + 1)) as u32
    }
}

/// PAT transport: `Authorization: Bearer <PAT>` wins; the legacy
/// `?pat=` query param is still honored (deprecated) as a fallback.
fn pat_from(req: &HttpRequest, query: &HashMap<String, String>) -> String {
    let header = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string);
    header
        .or_else(|| query.get("pat").cloned())
        .unwrap_or_default()
}

fn rate_guard(state: &web::Data<AppState>, req: &HttpRequest) -> Option<HttpResponse> {
    let ip = req
        .peer_addr()
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    if state.limiter.allow(&ip) {
        None
    } else {
        Some(
            HttpResponse::TooManyRequests()
                .insert_header(("Retry-After", "60"))
                .json(json!({ "error": "rate limit exceeded (30 requests/min)" })),
        )
    }
}

async fn resolve_uid(state: &web::Data<AppState>, pat: &str) -> Result<String, Error> {
    let gh = gh::GhClient::new(state.rt.as_ref(), pat, gh::DEFAULT_REPO);
    let (id, _) = gh.user().await?;
    Ok(id.to_string())
}

/// `{id, spec, lastRun, createdAt}`, the stored record minus the PAT.
fn render_schedule(id: &str, s: &StoredSchedule) -> Value {
    let mut v = serde_json::to_value(s).unwrap_or_else(|_| json!({}));
    if let Some(o) = v.as_object_mut() {
        o.remove("pat");
        o.insert("id".to_string(), json!(id));
    }
    v
}

// ---------------------------------------------------------------------------
// handlers
// ---------------------------------------------------------------------------

async fn healthz() -> HttpResponse {
    HttpResponse::Ok().json(json!({ "ok": true }))
}

async fn commits_post(
    state: web::Data<AppState>,
    req: HttpRequest,
    body: web::Json<Value>,
) -> HttpResponse {
    if let Some(resp) = rate_guard(&state, &req) {
        return resp;
    }
    let Some(pat) = body.get("pat").and_then(Value::as_str).filter(|p| !p.is_empty()) else {
        return bad_request("missing pat");
    };
    let days = body.get("days").unwrap_or(&Value::Null);
    let plan = match plan::parse_api_plan(days, date::today_utc()) {
        Ok(p) => p,
        Err(e) => return core_error(e),
    };
    // serialize against the runner and the hourly loop: interleaved
    // engine runs lose commits to force-patched refs
    let _write = state.write_lock.lock().await;
    match engine::write_commits(state.rt.as_ref(), pat, gh::DEFAULT_REPO, &plan.days, None)
        .await
    {
        Ok(res) => {
            let mut out = json!({ "created": res.created, "total": res.total });
            if let Some(err) = &res.partial {
                out["partial"] = json!(true);
                out["error"] = json!(err);
            }
            if plan.clamped > 0 {
                out["clamped"] = json!(plan.clamped);
            }
            HttpResponse::Ok().json(out)
        }
        Err(e) => core_error(e),
    }
}

async fn graph_get(
    state: web::Data<AppState>,
    req: HttpRequest,
    query: web::Query<HashMap<String, String>>,
) -> HttpResponse {
    // rate-limited like the mutations: each call makes two upstream
    // GitHub requests, so anonymous hammering would burn the server's
    // egress IP against GitHub's unauthenticated quota
    if let Some(resp) = rate_guard(&state, &req) {
        return resp;
    }
    let pat = pat_from(&req, &query);
    if pat.is_empty() {
        return bad_request("missing pat");
    }
    let gh = gh::GhClient::new(state.rt.as_ref(), &pat, gh::DEFAULT_REPO);
    let res = match gh.user().await {
        Ok((_, login)) => gh.contributions_graph(&login).await,
        Err(e) => Err(e),
    };
    match res {
        Ok(counts) => HttpResponse::Ok().json(json!({ "counts": counts })),
        Err(e) => core_error(e),
    }
}

async fn schedules_get(
    state: web::Data<AppState>,
    req: HttpRequest,
    query: web::Query<HashMap<String, String>>,
) -> HttpResponse {
    let pat = pat_from(&req, &query);
    if pat.is_empty() {
        return bad_request("missing pat");
    }
    let Some(secret) = state.secret.clone() else {
        return not_configured();
    };
    let uid = match resolve_uid(&state, &pat).await {
        Ok(u) => u,
        Err(e) => return core_error(e),
    };
    let records = match state.store.list(&uid).await {
        Ok(r) => r,
        Err(e) => return core_error(e),
    };
    let mut out = Vec::new();
    for rec in records {
        // current secret first; on failure try the old one (rotation)
        let healed = match crypto::decrypt_json::<StoredSchedule>(&secret, &rec.payload) {
            Ok(data) => (data, false),
            Err(_) => match state
                .old_secret
                .as_deref()
                .map(|old| crypto::decrypt_json::<StoredSchedule>(old, &rec.payload))
            {
                Some(Ok(data)) => (data, true),
                _ => {
                    out.push(json!({ "id": rec.id, "broken": true }));
                    continue;
                }
            },
        };
        let (data, needs_heal) = healed;
        // lazy heal: re-encrypt under the current secret and put it back
        if needs_heal {
            let iv = crypto::random_iv(state.rt.as_ref());
            if let Ok(blob) = crypto::encrypt_json(&secret, &data, &iv) {
                if state.store.put(&uid, &rec.id, &blob).await.is_ok() {
                    log::info!("schedule {uid}/{}: re-encrypted under current secret", rec.id);
                }
            }
        }
        out.push(render_schedule(&rec.id, &data));
    }
    HttpResponse::Ok().json(json!({ "schedules": out }))
}

async fn schedules_post(
    state: web::Data<AppState>,
    req: HttpRequest,
    body: web::Json<Value>,
) -> HttpResponse {
    if let Some(resp) = rate_guard(&state, &req) {
        return resp;
    }
    let Some(pat) = body.get("pat").and_then(Value::as_str).filter(|p| !p.is_empty()) else {
        return bad_request("missing pat");
    };
    // parse_spec reads only the spec fields; pat rides along in the same body
    let spec = match schedule::parse_spec(&body) {
        Ok(s) => s,
        Err(e) => return core_error(e),
    };
    let Some(secret) = state.secret.clone() else {
        return not_configured();
    };
    let uid = match resolve_uid(&state, pat).await {
        Ok(u) => u,
        Err(e) => return core_error(e),
    };
    let today = date::today_utc();
    let stored = StoredSchedule {
        pat: pat.to_string(),
        spec,
        last_run: today,
        created_at: date::fmt(today),
    };
    let iv = crypto::random_iv(state.rt.as_ref());
    let blob = match crypto::encrypt_json(&secret, &stored, &iv) {
        Ok(b) => b,
        Err(e) => return core_error(e),
    };
    // id: 16 bytes of OS entropy, hex-encoded; never time-seeded
    let mut idb = [0u8; 16];
    state.rt.random(&mut idb);
    let id = hex(&idb);
    match state.store.put(&uid, &id, &blob).await {
        Ok(()) => HttpResponse::Ok().json(json!({ "id": id })),
        Err(e) => core_error(e),
    }
}

async fn schedules_delete(
    state: web::Data<AppState>,
    req: HttpRequest,
    query: web::Query<HashMap<String, String>>,
) -> HttpResponse {
    if let Some(resp) = rate_guard(&state, &req) {
        return resp;
    }
    let pat = pat_from(&req, &query);
    if pat.is_empty() {
        return bad_request("missing pat");
    }
    let Some(id) = query.get("id").map(String::as_str).filter(|i| !i.is_empty()) else {
        return bad_request("missing id");
    };
    let uid = match resolve_uid(&state, &pat).await {
        Ok(u) => u,
        Err(e) => return core_error(e),
    };
    match state.store.get(&uid, id).await {
        Ok(Some(_)) => match state.store.delete(&uid, id).await {
            Ok(()) => HttpResponse::Ok().json(json!({ "deleted": true })),
            Err(e) => core_error(e),
        },
        Ok(None) => HttpResponse::NotFound().json(json!({ "error": "no such schedule" })),
        Err(e) => core_error(e),
    }
}

async fn schedules_run(state: web::Data<AppState>, req: HttpRequest) -> HttpResponse {
    if let Some(resp) = rate_guard(&state, &req) {
        return resp;
    }
    // fail closed: no secret configured -> the runner cannot run
    let Some(secret) = state.secret.clone() else {
        return not_configured();
    };
    let bearer = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if !const_eq(bearer.as_bytes(), secret.as_bytes()) {
        return HttpResponse::Unauthorized().json(json!({ "error": "invalid bearer token" }));
    }
    let today = date::today_utc();
    let mut rng = xorshift_rng(os_seed_u64());
    let logger = |line: &str| log::info!("runner: {line}");
    // serialize against POST /api/commits and the hourly loop
    let _write = state.write_lock.lock().await;
    match run_due_schedules(
        state.rt.as_ref(),
        state.store.as_ref(),
        &secret,
        today,
        &mut rng,
        &logger,
    )
    .await
    {
        Ok(res) => HttpResponse::Ok().json(res),
        Err(e) => core_error(e),
    }
}

async fn cors_preflight() -> HttpResponse {
    HttpResponse::NoContent().finish()
}

// ---------------------------------------------------------------------------
// wiring
// ---------------------------------------------------------------------------

/// Shared by `main` and the test harness (`init_service`); the return
/// type mirrors `App::wrap`'s own output so both `HttpServer::new` and
/// `test::init_service` accept it.
fn build_app(
    state: web::Data<AppState>,
) -> App<
    impl ServiceFactory<
        ServiceRequest,
        Config = (),
        Response = ServiceResponse,
        Error = actix_web::Error,
        InitError = (),
    >,
> {
    App::new()
        .app_data(state)
        .app_data(web::PayloadConfig::default().limit(BODY_LIMIT))
        .app_data(web::JsonConfig::default().limit(BODY_LIMIT).error_handler(json_error))
        // CORS is intentionally wide open: the widget embeds this API.
        .wrap(
            DefaultHeaders::new()
                .add(("Access-Control-Allow-Origin", "*"))
                .add(("Access-Control-Allow-Methods", "GET, POST, DELETE, OPTIONS"))
                .add(("Access-Control-Allow-Headers", "Authorization, Content-Type")),
        )
        // Log method + URL path but NOT the query string: the legacy
        // ?pat= fallback would otherwise write PATs into the logs.
        .wrap(Logger::new("%a \"%m %U %H\" %s %b %Dms"))
        // Logger wraps bodies in a private StreamLog type; re-boxing them
        // here keeps the App's type expressible in this fn's signature.
        .wrap_fn(|req: ServiceRequest, srv| {
            let call = srv.call(req);
            async move {
                let res = call.await?;
                Ok::<ServiceResponse, actix_web::Error>(res.map_into_boxed_body())
            }
        })
        .service(web::resource("/healthz").route(web::get().to(healthz)))
        .service(
            web::resource("/api/commits")
                .route(web::post().to(commits_post))
                .route(web::method(Method::OPTIONS).to(cors_preflight)),
        )
        .service(
            web::resource("/api/graph")
                .route(web::get().to(graph_get))
                .route(web::method(Method::OPTIONS).to(cors_preflight)),
        )
        .service(
            web::resource("/api/schedules")
                .route(web::get().to(schedules_get))
                .route(web::post().to(schedules_post))
                .route(web::delete().to(schedules_delete))
                .route(web::method(Method::OPTIONS).to(cors_preflight)),
        )
        .service(
            web::resource("/api/schedules/run")
                .route(web::post().to(schedules_run))
                .route(web::method(Method::OPTIONS).to(cors_preflight)),
        )
}

/// Body/JSON errors stay in the `{"error": ...}` shape (400 for
/// malformed JSON, 413 for oversize payloads).
fn json_error(err: actix_web::error::JsonPayloadError, _req: &HttpRequest) -> actix_web::Error {
    let status = err.status_code();
    let msg = err.to_string();
    InternalError::from_response(err, HttpResponse::build(status).json(json!({ "error": msg })))
        .into()
}

// ---------------------------------------------------------------------------
// optional hourly loop
// ---------------------------------------------------------------------------

/// Dedicated OS thread with its own current-thread tokio runtime; the
/// runner future borrows non-Send state, so it never touches the
/// actix worker pool. Ticks hourly (first tick immediately).
fn spawn_schedule_loop(
    rt: Arc<ServerRuntime>,
    store: Arc<dyn Store>,
    secret: String,
    write_lock: Arc<tokio::sync::Mutex<()>>,
) {
    let builder = std::thread::Builder::new().name("gitlarp-schedule-loop".into());
    let join = builder.spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        let Ok(runtime) = runtime else {
            log::error!("schedule loop: could not build tokio runtime");
            return;
        };
        runtime.block_on(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(3600));
            // serialize against POST /api/commits and the runner endpoint
            let write_lock = write_lock.clone();
            loop {
                tick.tick().await;
                let today = date::today_utc();
                let mut rng = xorshift_rng(os_seed_u64());
                let logger = |line: &str| log::info!("schedule loop: {line}");
                let _write = write_lock.lock().await;
                match run_due_schedules(
                    rt.as_ref(),
                    store.as_ref(),
                    &secret,
                    today,
                    &mut rng,
                    &logger,
                )
                .await
                {
                    Ok(res) => log::info!(
                        "schedule tick: {} schedule(s), {} run, {} commit(s), {} failed",
                        res.schedules,
                        res.ran,
                        res.commits,
                        res.failed
                    ),
                    Err(e) => log::error!("schedule tick failed: {e}"),
                }
            }
        });
    });
    if join.is_err() {
        log::error!("schedule loop: could not spawn thread");
    }
}

fn nonempty_env(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => Some(v),
        _ => None,
    }
}

/// A configured schedule secret must be at least MIN_SECRET_LEN chars:
/// it is the only thing protecting stored GitHub PATs, and the AES key
/// is a plain SHA-256 of it (no KDF stretching), so short secrets are
/// brute-forceable. Refuse to start rather than run under one.
fn check_secret(s: &str) -> Result<(), String> {
    if s.chars().count() >= MIN_SECRET_LEN {
        Ok(())
    } else {
        Err(format!(
            "GITLARP_SCHEDULE_SECRET must be at least {MIN_SECRET_LEN} characters \
             (e.g. `openssl rand -base64 32`); refusing to run with a brute-forceable secret"
        ))
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init();

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);
    let schedule_dir =
        std::env::var("SCHEDULE_DIR").unwrap_or_else(|_| "./data/schedules".into());
    let secret = nonempty_env("GITLARP_SCHEDULE_SECRET");
    let old_secret = nonempty_env("GITLARP_SCHEDULE_SECRET_OLD");

    if let Some(s) = &secret {
        if let Err(msg) = check_secret(s) {
            log::error!("{msg}");
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, msg));
        }
    }

    let write_lock = Arc::new(tokio::sync::Mutex::new(()));
    let state = web::Data::new(AppState {
        rt: Arc::new(ServerRuntime::new(None)),
        store: Arc::new(FileStore::new(schedule_dir)),
        secret,
        old_secret,
        limiter: RateLimiter::new(),
        write_lock,
    });

    if std::env::var("SCHEDULE_LOOP").ok().as_deref() == Some("1") {
        if let Some(secret) = state.secret.clone() {
            log::info!("schedule loop enabled: due schedules run hourly");
            spawn_schedule_loop(
                state.rt.clone(),
                state.store.clone(),
                secret,
                state.write_lock.clone(),
            );
        } else {
            log::warn!("SCHEDULE_LOOP=1 but GITLARP_SCHEDULE_SECRET is unset; loop disabled");
        }
    }

    log::info!("gitlarp-server listening on 0.0.0.0:{port}");
    HttpServer::new(move || build_app(state.clone()))
        .bind(("0.0.0.0", port))?
        .client_request_timeout(Duration::from_secs(30))
        .run()
        .await
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test::{self, TestRequest};
    use std::path::PathBuf;

    // -- fixtures -----------------------------------------------------------

    fn temp_root() -> PathBuf {
        let mut b = [0u8; 8];
        getrandom::getrandom(&mut b).expect("entropy");
        std::env::temp_dir().join(format!("gitlarp-server-test-{}", hex(&b)))
    }

    fn mock_ok(v: Value) -> CoreResponse {
        CoreResponse { status: 200, body: v.to_string() }
    }

    /// Canned GitHub: `p1` is user id 1 "tester", `p2` is id 2
    /// "tester2"; the calendar count echoes which login the GraphQL
    /// query targeted (1 for tester, 2 for tester2). The engine flow
    /// runs against a fresh repo (branch ref 404 -> seed path).
    fn mock_fetch(req: CoreRequest) -> Result<CoreResponse, Error> {
        let path = req
            .url
            .strip_prefix("https://api.github.com")
            .unwrap_or(req.url.as_str())
            .to_string();
        let pat = req
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            .and_then(|(_, v)| v.strip_prefix("Bearer ").map(str::to_string))
            .unwrap_or_default();
        if path == "/user" {
            return match pat.as_str() {
                "p1" => Ok(mock_ok(json!({ "id": 1, "login": "tester" }))),
                "p2" => Ok(mock_ok(json!({ "id": 2, "login": "tester2" }))),
                _ => Ok(CoreResponse {
                    status: 401,
                    body: json!({ "message": "Bad credentials" }).to_string(),
                }),
            };
        }
        if path == "/graphql" {
            let n = if req.body.as_deref().unwrap_or("").contains("tester2") { 2 } else { 1 };
            return Ok(mock_ok(json!({
                "data": { "user": { "contributionsCollection": { "contributionCalendar": {
                    "weeks": [
                        { "contributionDays": [ { "date": "2026-01-01", "contributionCount": n } ] }
                    ]
                } } } }
            })));
        }
        match (req.method.as_str(), path.as_str()) {
            ("GET", "/repos/tester/gitlarp-history") => {
                Ok(mock_ok(json!({ "default_branch": "main" })))
            }
            ("GET", "/repos/tester/gitlarp-history/git/ref/heads/main") => Ok(CoreResponse {
                status: 404,
                body: json!({ "message": "Not Found" }).to_string(),
            }),
            ("POST", "/repos/tester/gitlarp-history/git/blobs") => {
                Ok(mock_ok(json!({ "sha": "blob" })))
            }
            ("POST", "/repos/tester/gitlarp-history/git/trees") => {
                Ok(mock_ok(json!({ "sha": "tree" })))
            }
            ("POST", "/repos/tester/gitlarp-history/git/commits") => {
                Ok(mock_ok(json!({ "sha": "c1" })))
            }
            ("POST", "/repos/tester/gitlarp-history/git/refs") => Ok(mock_ok(json!({}))),
            ("PATCH", "/repos/tester/gitlarp-history/git/refs/heads/main") => {
                Ok(mock_ok(json!({})))
            }
            _ => Err(Error::new(500, format!("mock: unexpected {} {}", req.method, path))),
        }
    }

    /// State with a mock fetch (or the real one), a temp-dir store,
    /// and explicit secrets. Nothing here touches env vars, so tests
    /// can run fully in parallel.
    fn test_state(
        dir: PathBuf,
        secret: Option<&str>,
        old_secret: Option<&str>,
        mock: bool,
    ) -> web::Data<AppState> {
        web::Data::new(AppState {
            rt: Arc::new(ServerRuntime::new(if mock {
                Some(Arc::new(mock_fetch))
            } else {
                None
            })),
            store: Arc::new(FileStore::new(&dir)),
            secret: secret.map(str::to_string),
            old_secret: old_secret.map(str::to_string),
            limiter: RateLimiter::new(),
            write_lock: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    fn bearer(v: &str) -> (&'static str, String) {
        ("Authorization", format!("Bearer {v}"))
    }

    async fn call_json<S, R, B>(
        app: &S,
        req: R,
    ) -> (StatusCode, Value)
    where
        S: actix_web::dev::Service<
            R,
            Response = actix_web::dev::ServiceResponse<B>,
            Error = actix_web::Error,
        >,
        B: actix_web::body::MessageBody,
    {
        let resp = test::call_service(app, req).await;
        let status = resp.status();
        (status, test::read_body_json(resp).await)
    }

    // -- (a) healthz -------------------------------------------------------

    #[actix_web::test]
    async fn healthz_is_open_and_never_rate_limited() {
        let dir = temp_root();
        let state = test_state(dir.clone(), Some("S"), None, true);
        let app = test::init_service(build_app(state)).await;
        // 40 hits: far over the mutating-endpoint budget, still all green
        for _ in 0..40 {
            let req = TestRequest::get().uri("/healthz").to_request();
            let resp = test::call_service(&app, req).await;
            assert_eq!(resp.status(), StatusCode::OK);
        }
        let req = TestRequest::get().uri("/healthz").to_request();
        let (status, v) = call_json(&app, req).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v, json!({ "ok": true }));
        std::fs::remove_dir_all(dir).ok();
    }

    // -- (b) commits --------------------------------------------------------

    #[actix_web::test]
    async fn commits_missing_pat_is_400() {
        let dir = temp_root();
        let state = test_state(dir.clone(), Some("S"), None, true);
        let app = test::init_service(build_app(state)).await;
        let req = TestRequest::post()
            .uri("/api/commits")
            .set_json(json!({ "days": [{ "date": date::fmt(date::today_utc()), "count": 1 }] }))
            .to_request();
        let (status, v) = call_json(&app, req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(v["error"], "missing pat");
        std::fs::remove_dir_all(dir).ok();
    }

    #[actix_web::test]
    async fn commits_creates_commits_and_reports_clamping() {
        let dir = temp_root();
        let state = test_state(dir.clone(), Some("S"), None, true);
        let app = test::init_service(build_app(state)).await;
        let today = date::fmt(date::today_utc());
        // 2020-01-01 is far outside the rolling 12-month window -> clamped
        let req = TestRequest::post()
            .uri("/api/commits")
            .set_json(json!({ "pat": "p1", "days": [
                { "date": today, "count": 2 },
                { "date": "2020-01-01", "count": 1 }
            ] }))
            .to_request();
        let (status, v) = call_json(&app, req).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["created"], 3);
        assert_eq!(v["total"], 3);
        assert_eq!(v["clamped"], 1);
        assert!(v.get("partial").is_none(), "no partial failure in the happy path");
        std::fs::remove_dir_all(dir).ok();
    }

    #[actix_web::test]
    async fn commits_over_cap_rejects_with_400() {
        let dir = temp_root();
        let state = test_state(dir.clone(), Some("S"), None, true);
        let app = test::init_service(build_app(state)).await;
        let today = date::fmt(date::today_utc());
        let req = TestRequest::post()
            .uri("/api/commits")
            .set_json(json!({ "pat": "p1", "days": [{ "date": today, "count": 501 }] }))
            .to_request();
        let (status, v) = call_json(&app, req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(v["error"].as_str().unwrap().contains("cap"));
        std::fs::remove_dir_all(dir).ok();
    }

    // -- (c) graph ----------------------------------------------------------

    #[actix_web::test]
    async fn graph_via_bearer_header() {
        let dir = temp_root();
        let state = test_state(dir.clone(), Some("S"), None, true);
        let app = test::init_service(build_app(state)).await;
        let req = TestRequest::get()
            .uri("/api/graph")
            .insert_header(bearer("p1"))
            .to_request();
        let (status, v) = call_json(&app, req).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["counts"]["2026-01-01"], 1);
        std::fs::remove_dir_all(dir).ok();
    }

    #[actix_web::test]
    async fn graph_via_deprecated_query_param() {
        let dir = temp_root();
        let state = test_state(dir.clone(), Some("S"), None, true);
        let app = test::init_service(build_app(state)).await;
        let req = TestRequest::get().uri("/api/graph?pat=p2").to_request();
        let (status, v) = call_json(&app, req).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["counts"]["2026-01-01"], 2, "counts come from tester2's calendar");
        std::fs::remove_dir_all(dir).ok();
    }

    #[actix_web::test]
    async fn graph_header_wins_over_query_param() {
        let dir = temp_root();
        let state = test_state(dir.clone(), Some("S"), None, true);
        let app = test::init_service(build_app(state)).await;
        let req = TestRequest::get()
            .uri("/api/graph?pat=p2")
            .insert_header(bearer("p1"))
            .to_request();
        let (status, v) = call_json(&app, req).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["counts"]["2026-01-01"], 1, "the header PAT (p1/tester) must win");
        std::fs::remove_dir_all(dir).ok();
    }

    #[actix_web::test]
    async fn graph_missing_pat_is_400() {
        let dir = temp_root();
        let state = test_state(dir.clone(), Some("S"), None, true);
        let app = test::init_service(build_app(state)).await;
        let req = TestRequest::get().uri("/api/graph").to_request();
        let (status, v) = call_json(&app, req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(v["error"], "missing pat");
        std::fs::remove_dir_all(dir).ok();
    }

    // -- (d) schedules CRUD -------------------------------------------------

    #[actix_web::test]
    async fn schedules_crud_roundtrip() {
        let dir = temp_root();
        let state = test_state(dir.clone(), Some("S"), None, true);
        let app = test::init_service(build_app(state)).await;

        // create
        let req = TestRequest::post()
            .uri("/api/schedules")
            .set_json(json!({ "pat": "p1", "min": 1, "max": 2, "weekends": false }))
            .to_request();
        let (status, v) = call_json(&app, req).await;
        assert_eq!(status, StatusCode::OK);
        let id = v["id"].as_str().expect("create returns id").to_string();
        assert_eq!(id.len(), 32, "16 random bytes, hex-encoded");

        // list: full record minus the PAT
        let req = TestRequest::get()
            .uri("/api/schedules")
            .insert_header(bearer("p1"))
            .to_request();
        let (status, v) = call_json(&app, req).await;
        assert_eq!(status, StatusCode::OK);
        let scheds = v["schedules"].as_array().expect("schedules array");
        assert_eq!(scheds.len(), 1);
        assert_eq!(scheds[0]["id"], id);
        assert_eq!(scheds[0]["spec"]["min"], 1);
        assert_eq!(scheds[0]["spec"]["max"], 2);
        assert_eq!(scheds[0]["spec"]["weekends"], false);
        assert!(scheds[0]["lastRun"].is_string());
        assert!(scheds[0]["createdAt"].is_string());
        assert!(scheds[0].get("pat").is_none(), "the PAT must never render");
        assert!(scheds[0].get("broken").is_none());

        // delete wrong id -> 404
        let req = TestRequest::delete()
            .uri("/api/schedules?id=definitely-not-mine")
            .insert_header(bearer("p1"))
            .to_request();
        let (status, v) = call_json(&app, req).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(v["error"], "no such schedule");

        // delete right id -> ok
        let req = TestRequest::delete()
            .uri(&format!("/api/schedules?id={id}"))
            .insert_header(bearer("p1"))
            .to_request();
        let (status, v) = call_json(&app, req).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v, json!({ "deleted": true }));

        // gone: list is empty, delete again is 404
        let req = TestRequest::get()
            .uri("/api/schedules")
            .insert_header(bearer("p1"))
            .to_request();
        let (_, v) = call_json(&app, req).await;
        assert_eq!(v["schedules"].as_array().unwrap().len(), 0);
        let req = TestRequest::delete()
            .uri(&format!("/api/schedules?id={id}"))
            .insert_header(bearer("p1"))
            .to_request();
        let (status, _) = call_json(&app, req).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        std::fs::remove_dir_all(dir).ok();
    }

    #[actix_web::test]
    async fn schedules_delete_missing_id_is_400() {
        let dir = temp_root();
        let state = test_state(dir.clone(), Some("S"), None, true);
        let app = test::init_service(build_app(state)).await;
        let req = TestRequest::delete()
            .uri("/api/schedules")
            .insert_header(bearer("p1"))
            .to_request();
        let (status, v) = call_json(&app, req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(v["error"], "missing id");
        std::fs::remove_dir_all(dir).ok();
    }

    #[actix_web::test]
    async fn schedules_undecryptable_record_renders_broken() {
        let dir = temp_root();
        // seed a garbage record straight into the store (user "1" = p1)
        let seed = FileStore::new(&dir);
        seed.put("1", "junk1", "v1.not-a-real-payload").await.unwrap();
        let state = test_state(dir.clone(), Some("S"), None, true);
        let app = test::init_service(build_app(state)).await;
        let req = TestRequest::get()
            .uri("/api/schedules")
            .insert_header(bearer("p1"))
            .to_request();
        let (status, v) = call_json(&app, req).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            v["schedules"][0],
            json!({ "id": "junk1", "broken": true }),
            "undecryptable records must render as {{id, broken:true}}"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    // -- (e) run gating -----------------------------------------------------

    #[actix_web::test]
    async fn run_fails_closed_without_secret() {
        let dir = temp_root();
        let state = test_state(dir.clone(), None, None, true);
        let app = test::init_service(build_app(state)).await;
        let req = TestRequest::post()
            .uri("/api/schedules/run")
            .insert_header(bearer("anything"))
            .to_request();
        let (status, v) = call_json(&app, req).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(v["error"].is_string());
        std::fs::remove_dir_all(dir).ok();
    }

    #[actix_web::test]
    async fn run_rejects_wrong_bearer() {
        let dir = temp_root();
        let state = test_state(dir.clone(), Some("S"), None, true);
        let app = test::init_service(build_app(state)).await;
        let req = TestRequest::post()
            .uri("/api/schedules/run")
            .insert_header(bearer("wrong"))
            .to_request();
        let (status, v) = call_json(&app, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(v["error"], "invalid bearer token");
        std::fs::remove_dir_all(dir).ok();
    }

    #[actix_web::test]
    async fn run_accepts_correct_bearer() {
        let dir = temp_root();
        let state = test_state(dir.clone(), Some("S"), None, true);
        let app = test::init_service(build_app(state)).await;
        let req = TestRequest::post()
            .uri("/api/schedules/run")
            .insert_header(bearer("S"))
            .to_request();
        let (status, v) = call_json(&app, req).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            v,
            json!({ "schedules": 0, "ran": 0, "commits": 0, "failed": 0 })
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[actix_web::test]
    async fn run_processes_a_due_schedule_end_to_end() {
        let dir = temp_root();
        let today = date::today_utc();
        let spec = schedule::parse_spec(&json!({ "min": 1, "max": 1 })).unwrap();
        // due: last run yesterday, catch-up 7 -> today is in the window
        let stored = StoredSchedule {
            pat: "p1".into(),
            spec,
            last_run: date::add_days(today, -1),
            created_at: date::fmt(date::add_days(today, -10)),
        };
        let blob = crypto::encrypt_json("S", &stored, &[0u8; 12]).unwrap();
        let seed = FileStore::new(&dir);
        seed.put("1", "sched1", &blob).await.unwrap();

        let state = test_state(dir.clone(), Some("S"), None, true);
        let app = test::init_service(build_app(state)).await;
        let req = TestRequest::post()
            .uri("/api/schedules/run")
            .insert_header(bearer("S"))
            .to_request();
        let (status, v) = call_json(&app, req).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["schedules"], 1);
        assert_eq!(v["ran"], 1);
        assert_eq!(v["commits"], 1);
        assert_eq!(v["failed"], 0);

        // the window advanced and the record re-encrypted in place
        let payload = seed.get("1", "sched1").await.unwrap().unwrap();
        let now: StoredSchedule = crypto::decrypt_json("S", &payload).unwrap();
        assert_eq!(now.last_run, today);
        std::fs::remove_dir_all(dir).ok();
    }

    // -- (f) rate limiting --------------------------------------------------

    #[actix_web::test]
    async fn mutating_endpoints_are_rate_limited_with_retry_after() {
        let dir = temp_root();
        let state = test_state(dir.clone(), Some("S"), None, true);
        let app = test::init_service(build_app(state)).await;
        // 30 requests pass (each one resolves the user, finds no schedule)
        for i in 0..RATE_MAX_PER_WINDOW {
            let req = TestRequest::delete()
                .uri("/api/schedules?id=x")
                .insert_header(bearer("p1"))
                .to_request();
            let resp = test::call_service(&app, req).await;
            assert_eq!(
                resp.status(),
                StatusCode::NOT_FOUND,
                "request {i} must be under the limit"
            );
        }
        // the 31st is over the limit: 429 + Retry-After
        let req = TestRequest::delete()
            .uri("/api/schedules?id=x")
            .insert_header(bearer("p1"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            resp.headers().get("Retry-After").and_then(|v| v.to_str().ok()),
            Some("60")
        );
        std::fs::remove_dir_all(dir).ok();
    }

    /// /api/graph also hits upstream GitHub twice per call, so it is
    /// rate-limited too (an anonymous client must not be able to burn
    /// the server's egress IP against GitHub's unauthenticated quota).
    #[actix_web::test]
    async fn graph_is_rate_limited_after_the_window_budget() {
        let dir = temp_root();
        let state = test_state(dir.clone(), Some("S"), None, true);
        let app = test::init_service(build_app(state)).await;
        for i in 0..RATE_MAX_PER_WINDOW {
            let req = TestRequest::get()
                .uri("/api/graph")
                .insert_header(bearer("p1"))
                .to_request();
            let resp = test::call_service(&app, req).await;
            assert_eq!(resp.status(), StatusCode::OK, "graph call {i} under the limit");
        }
        let req = TestRequest::get()
            .uri("/api/graph")
            .insert_header(bearer("p1"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        std::fs::remove_dir_all(dir).ok();
    }

    /// healthz must stay exempt from the limiter even after graph has
    /// exhausted the per-IP budget.
    #[actix_web::test]
    async fn healthz_exempt_from_graph_budget() {
        let dir = temp_root();
        let state = test_state(dir.clone(), Some("S"), None, true);
        let app = test::init_service(build_app(state)).await;
        for _ in 0..(RATE_MAX_PER_WINDOW + 5) {
            let req = TestRequest::get()
                .uri("/api/graph")
                .insert_header(bearer("p1"))
                .to_request();
            let _ = test::call_service(&app, req).await;
        }
        let req = TestRequest::get().uri("/healthz").to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        std::fs::remove_dir_all(dir).ok();
    }

    // -- (g) secret rotation ------------------------------------------------

    #[actix_web::test]
    async fn secret_rotation_lazy_heals_old_records() {
        let dir = temp_root();
        // step 1: create a schedule while secret A is current
        let state_a = test_state(dir.clone(), Some("A"), None, true);
        let app_a = test::init_service(build_app(state_a)).await;
        let req = TestRequest::post()
            .uri("/api/schedules")
            .set_json(json!({ "pat": "p1", "min": 1, "max": 1 }))
            .to_request();
        let (status, v) = call_json(&app_a, req).await;
        assert_eq!(status, StatusCode::OK);
        let id = v["id"].as_str().unwrap().to_string();
        drop(app_a);

        // step 2: "restart" with secret B and A as the old secret
        let state_b = test_state(dir.clone(), Some("B"), Some("A"), true);
        let app_b = test::init_service(build_app(state_b)).await;
        let req = TestRequest::get()
            .uri("/api/schedules")
            .insert_header(bearer("p1"))
            .to_request();
        let (status, v) = call_json(&app_b, req).await;
        assert_eq!(status, StatusCode::OK);
        let scheds = v["schedules"].as_array().unwrap();
        assert_eq!(scheds.len(), 1);
        assert_eq!(scheds[0]["id"], id);
        assert!(scheds[0].get("broken").is_none(), "old-secret record must heal, not break");

        // the stored payload now decrypts under B (was re-encrypted)
        let seed = FileStore::new(&dir);
        let payload = seed.get("1", &id).await.unwrap().unwrap();
        let healed: StoredSchedule = crypto::decrypt_json("B", &payload)
            .expect("record must have been re-encrypted under the current secret");
        assert_eq!(healed.pat, "p1");
        assert_eq!(healed.spec.min, 1);
        std::fs::remove_dir_all(dir).ok();
    }

    // -- body limits --------------------------------------------------------

    #[actix_web::test]
    async fn oversized_body_is_rejected() {
        let dir = temp_root();
        let state = test_state(dir.clone(), Some("S"), None, true);
        let app = test::init_service(build_app(state)).await;
        let big = vec![b'x'; 2 * BODY_LIMIT];
        let req = TestRequest::post()
            .uri("/api/commits")
            .insert_header(("content-type", "application/json"))
            .set_payload(big)
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
        std::fs::remove_dir_all(dir).ok();
    }

    // -- units --------------------------------------------------------------

    #[test]
    fn error_mapping_passes_4xx_through_and_maps_the_rest_to_502() {
        assert_eq!(core_error(Error::new(400, "bad")).status(), StatusCode::BAD_REQUEST);
        assert_eq!(core_error(Error::new(401, "no")).status(), StatusCode::UNAUTHORIZED);
        assert_eq!(core_error(Error::new(404, "gone")).status(), StatusCode::NOT_FOUND);
        assert_eq!(
            core_error(Error::new(418, "teapot")).status(),
            StatusCode::from_u16(418).unwrap()
        );
        assert_eq!(core_error(Error::new(500, "boom")).status(), StatusCode::BAD_GATEWAY);
        assert_eq!(core_error(Error::new(502, "upstream")).status(), StatusCode::BAD_GATEWAY);
        assert_eq!(core_error(Error::new(0, "weird")).status(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn constant_time_compare_covers_content_and_length() {
        assert!(const_eq(b"", b""));
        assert!(const_eq(b"secret", b"secret"));
        assert!(!const_eq(b"secret", b"secreT"));
        assert!(!const_eq(b"secret", b"secret2"));
        assert!(!const_eq(b"secret2", b"secret"));
        assert!(!const_eq(b"", b"x"));
        assert!(!const_eq(b"x", b""));
    }

    #[test]
    fn xorshift_rng_stays_in_bounds_and_varies() {
        let mut a = xorshift_rng(0x1234_5678_9abc_def0);
        let mut b = xorshift_rng(0x0fed_cba9_8765_4321);
        let mut differing = 0;
        for _ in 0..100 {
            let x = a(1, 100);
            let y = b(1, 100);
            assert!((1..=100).contains(&x));
            assert!((1..=100).contains(&y));
            if x != y {
                differing += 1;
            }
        }
        assert!(differing > 0, "two seeds must produce different streams");
        let mut pinned = xorshift_rng(42);
        assert_eq!(pinned(5, 5), 5);
        let mut zero = xorshift_rng(0);
        assert_eq!(zero(1, 3), 1, "a zero seed must still produce values");
    }

    #[test]
    fn hex_encoding_is_lowercase_pairs() {
        assert_eq!(hex(&[]), "");
        assert_eq!(hex(&[0x00, 0x0f, 0xff]), "000fff");
    }

    #[test]
    fn check_secret_enforces_minimum_length() {
        assert!(check_secret("a-16-char-secret").is_ok());
        assert!(check_secret("openssl-rand-base64-32-bytes-long").is_ok());
        for weak in ["", "hunter2", "shortsecret13"] {
            assert!(check_secret(weak).is_err(), "must reject: {weak:?}");
            assert!(check_secret(weak).unwrap_err().contains("GITLARP_SCHEDULE_SECRET"));
        }
    }
}
