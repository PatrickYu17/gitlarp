mod config;
mod locale;

use std::env;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use gitlarp_core::gh::GhClient;
use gitlarp_core::http::{HttpRequest, HttpResponse, Runtime};
use gitlarp_core::plan::{build_plan, Plan, CLI_CAPS};
use gitlarp_core::schedule::{catchup_skipped, due_days, next_due, ScheduleSpec};
use gitlarp_core::{date, engine};

use config::Config;
use gitlarp_core::date::Date;

const DEFAULT_REPO: &str = "gitlarp-history";

struct Ctx {
    home: PathBuf,
    force: bool,
}

impl Ctx {
    fn from_env(force: bool) -> Ctx {
        let home = match env::var("GITLARP_HOME") {
            Ok(h) => PathBuf::from(h),
            Err(_) => PathBuf::from(env::var("HOME").unwrap_or_else(|_| ".".into())).join(".gitlarp"),
        };
        Ctx { home, force }
    }

    fn config_path(&self) -> PathBuf {
        self.home.join("config.toml")
    }
}

fn repo_name() -> String {
    env::var("GITLARP_REPO").unwrap_or_else(|_| DEFAULT_REPO.into())
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if let Err(e) = run(&args) {
        eprintln!("{}", t!("err.prefix", e = e));
        std::process::exit(1);
    }
}

fn scan_args(args: &[String]) -> (bool, bool, Vec<String>) {
    let mut force = false;
    let mut dry = false;
    let mut rest = Vec::new();
    for a in args {
        match a.as_str() {
            "--force" => force = true,
            "--dry-run" => dry = true,
            _ => rest.push(a.clone()),
        }
    }
    (force, dry, rest)
}

/// Entry point minus process-exit side effects, so tests can drive
/// every command directly. Unknown/empty command yields the usage
/// text as an error.
fn run(args: &[String]) -> Result<(), String> {
    let (force, dry, rest) = scan_args(args);
    let ctx = Ctx::from_env(force);
    match rest.first().map(String::as_str) {
        Some("day") => day(&ctx, &rest[1..], dry),
        Some("fill") => fill(&ctx, &rest[1..], dry),
        Some("schedule") => schedule(&ctx, &rest[1..]),
        Some("cron") => cron(&ctx, &rest[1..], dry),
        Some("init") => init(),
        Some("wipe") => wipe(),
        Some("--help" | "-h") => {
            println!("{}", t!("usage"));
            Ok(())
        }
        _ => Err(t!("usage")),
    }
}

// --- platform: the GitHub API via the gh CLI's transport+auth ---

struct GhTransport;

impl Runtime for GhTransport {
    fn fetch(&self, req: HttpRequest) -> gitlarp_core::BoxFut<Result<HttpResponse, gitlarp_core::Error>> {
        Box::pin(std::future::ready(run_gh(req)))
    }

    fn sleep(&self, ms: u64) -> gitlarp_core::BoxFut<()> {
        std::thread::sleep(Duration::from_millis(ms));
        Box::pin(std::future::ready(()))
    }

    fn random(&self, buf: &mut [u8]) {
        for b in buf {
            *b = fastrand::u8(..);
        }
    }
}

/// A hung `gh` (network black hole) must not hang the CLI forever.
const GH_TIMEOUT_MS: u64 = 60_000;

/// `gh api --include` prints status line + headers + body to stdout
/// (even for HTTP errors), and authenticates via GH_TOKEN.
fn run_gh(req: HttpRequest) -> Result<HttpResponse, gitlarp_core::Error> {
    run_gh_timeout(req, GH_TIMEOUT_MS)
}

/// `run_gh` with an explicit deadline (ms) so tests can exercise the
/// timeout path with a tiny budget. Stdout/stderr are drained on
/// reader threads while waiting, so a `gh` writing more than a pipe
/// buffer of output cannot deadlock against the poll loop.
fn run_gh_timeout(req: HttpRequest, timeout_ms: u64) -> Result<HttpResponse, gitlarp_core::Error> {
    let path = req.url.strip_prefix("https://api.github.com").unwrap_or(&req.url);
    let mut cmd = Command::new("gh");
    cmd.arg("api")
        .arg(path)
        .arg("--method")
        .arg(&req.method)
        .arg("--include")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in &req.headers {
        match k.as_str() {
            "Authorization" => {
                cmd.env("GH_TOKEN", v.strip_prefix("Bearer ").unwrap_or(v));
            }
            "Accept" | "Content-Type" => {
                cmd.arg("-H").arg(format!("{k}: {v}"));
            }
            _ => {}
        }
    }
    let has_body = req.body.is_some();
    if has_body {
        cmd.arg("--input").arg("-");
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| gitlarp_core::Error::new(502, format!("gh CLI not found: {e}")))?;
    if has_body {
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(req.body.as_deref().unwrap_or("").as_bytes())
                .map_err(|e| gitlarp_core::Error::new(502, format!("gh stdin: {e}")))?;
        }
    } else {
        // gh only reads stdin with --input; close it anyway so the
        // child never sees a pipe held open for the whole poll loop
        drop(child.stdin.take());
    }
    // drain the pipes concurrently; reading only after exit would
    // deadlock once gh exceeds the OS pipe buffer
    let read_pipe = |mut pipe: Option<std::process::ChildStdout>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(p) = pipe.as_mut() {
                let _ = std::io::Read::read_to_string(p, &mut s);
            }
            s
        })
    };
    let read_err = |mut pipe: Option<std::process::ChildStderr>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(p) = pipe.as_mut() {
                let _ = std::io::Read::read_to_string(p, &mut s);
            }
            s
        })
    };
    let stdout = read_pipe(child.stdout.take());
    let stderr = read_err(child.stderr.take());

    // poll for exit until the deadline, then kill
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Ok(st),
            Ok(None) => {
                if Instant::now() >= deadline {
                    child.kill().ok();
                    break Err(gitlarp_core::Error::new(
                        502,
                        format!("gh api timed out after {timeout_ms}ms"),
                    ));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => break Err(gitlarp_core::Error::new(502, format!("gh wait: {e}"))),
        }
    };

    let out_text = stdout.join().unwrap_or_default();
    let err_text = stderr.join().unwrap_or_default();
    status?; // exit status, or the timeout/kill error propagated

    let text = out_text;
    let Some(status) = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
    else {
        let stderr = err_text.trim().to_string();
        let tail = if stderr.is_empty() {
            text.lines().take(1).collect::<String>()
        } else {
            stderr
        };
        return Err(gitlarp_core::Error::new(502, format!("gh api: {tail}")));
    };
    let body = text
        .split_once("\n\n")
        .or_else(|| text.split_once("\r\n\r\n"))
        .map(|(_, b)| b.trim_start_matches('\r').to_string())
        .unwrap_or_default();
    Ok(HttpResponse { status, body })
}

fn gh_token() -> Result<String, String> {
    let out = Command::new("gh")
        .args(["auth", "token"])
        .output()
        .map_err(|_| t!("err.no_gh"))?;
    let tok = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || tok.is_empty() {
        return Err(t!("err.gh_token"));
    }
    Ok(tok)
}

// --- commands ---

fn day(ctx: &Ctx, args: &[String], dry: bool) -> Result<(), String> {
    let ds = args.first().ok_or(t!("err.day_usage"))?;
    let date = date::parse(ds).map_err(msg)?;
    let n: u32 = args
        .get(1)
        .ok_or(t!("err.missing_count"))?
        .parse()
        .map_err(|_| t!("err.bad_count"))?;
    let plan = build_plan(&[(date, n)], ctx.force, date::today_utc(), CLI_CAPS).map_err(msg)?;
    run_plan(&plan, dry)
}

fn parse_fill(args: &[String]) -> Result<(Date, Date, u32, u32), String> {
    let mut from: Option<Date> = None;
    let mut to: Option<Date> = None;
    let mut min: u32 = 1;
    let mut max: u32 = 5;
    let mut i = 0;
    while i < args.len() {
        let v = args
            .get(i + 1)
            .ok_or(t!("err.missing_value", flag = args[i]))?;
        match args[i].as_str() {
            "--from" => from = Some(date::parse(v).map_err(|e| e.message)?),
            "--to" => to = Some(date::parse(v).map_err(|e| e.message)?),
            "--min" => min = v.parse().map_err(|_| t!("err.bad_min"))?,
            "--max" => max = v.parse().map_err(|_| t!("err.bad_max"))?,
            f => return Err(t!("err.unknown_flag", flag = f)),
        }
        i += 2;
    }
    let from = from.ok_or(t!("err.missing_from"))?;
    let to = to.ok_or(t!("err.missing_to"))?;
    if from > to {
        return Err(t!("err.from_after_to"));
    }
    if min > max {
        return Err(t!("err.min_over_max"));
    }
    Ok((from, to, min, max))
}

fn fill(ctx: &Ctx, args: &[String], dry: bool) -> Result<(), String> {
    let (from, to, min, max) = parse_fill(args)?;
    let span = (to - from).whole_days() + 1;
    if span > 365 {
        return Err(t!("err.span_over_cap", span = span, limit = 365));
    }
    let mut days = Vec::new();
    let mut d = from;
    while d <= to {
        days.push((d, fastrand::u32(min..=max)));
        d = date::add_days(d, 1);
    }
    let plan = build_plan(&days, ctx.force, date::today_utc(), CLI_CAPS).map_err(msg)?;
    run_plan(&plan, dry)
}

fn msg(e: gitlarp_core::Error) -> String {
    e.message
}

fn run_plan(plan: &Plan, dry: bool) -> Result<(), String> {
    if plan.clamped > 0 {
        eprintln!("{}", t!("warn.clamped", n = plan.clamped));
    }
    if plan.outside > 0 {
        eprintln!("{}", t!("warn.outside", n = plan.outside));
    }
    let total: u32 = plan.days.iter().map(|(_, n)| n).sum();
    if dry {
        for (d, n) in &plan.days {
            println!("{}", t!("info.dry_day", date = date::fmt(*d), n = n));
        }
        println!("{}", t!("info.dry_summary", total = total, days = plan.days.len()));
        return Ok(());
    }
    let tok = gh_token()?;
    let result = gitlarp_core::block_on(engine::write_commits(
        &GhTransport,
        &tok,
        &repo_name(),
        &plan.days,
        None,
    ))
    .map_err(msg)?;
    if let Some(err) = &result.partial {
        eprintln!("{}", t!("warn.partial", e = err));
    }
    println!("{}", t!("info.created", made = result.created));
    print_warnings();
    Ok(())
}

fn print_warnings() {
    eprintln!("{}", t!("warn.private"));
    eprintln!("{}", t!("warn.unsigned"));
}

fn wipe() -> Result<(), String> {
    let tok = gh_token()?;
    match gitlarp_core::block_on(engine::wipe(&GhTransport, &tok, &repo_name(), None)) {
        Ok(engine::WipeOutcome::Reset { sha }) => {
            println!("{}", t!("info.wiped", sha = sha));
            Ok(())
        }
        Ok(engine::WipeOutcome::Deleted { branch }) => {
            println!("{}", t!("info.wiped_empty", branch = branch));
            Ok(())
        }
        Err(e) => Err(e.message),
    }
}

fn init() -> Result<(), String> {
    let tok = gh_token()?;
    let gh = GhClient::new(&GhTransport, &tok, &repo_name());
    let (_, login) = gitlarp_core::block_on(gh.user()).map_err(msg)?;
    gitlarp_core::block_on(gh.ensure_repo(&login)).map_err(msg)?;
    let ctx = Ctx::from_env(false);
    fs::create_dir_all(&ctx.home).map_err(|e| e.to_string())?;
    config::save(&ctx.config_path(), &Config::default())?;
    println!(
        "{}",
        t!(
            "info.init_done",
            repo = format!("https://github.com/{login}/{}", repo_name()),
            path = format!("{:?}", ctx.config_path())
        )
    );
    println!("{}", t!("info.init_next"));
    Ok(())
}

// --- schedules ---

fn parse_schedule_args(args: &[String]) -> Result<ScheduleSpec, String> {
    let mut from: Option<String> = None;
    let mut to: Option<String> = None;
    let mut min: Option<u32> = None;
    let mut max: Option<u32> = None;
    let mut weekends = true;
    let mut catchup: Option<u32> = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--no-weekends" {
            weekends = false;
            i += 1;
            continue;
        }
        let v = args
            .get(i + 1)
            .ok_or(t!("err.missing_value", flag = args[i]))?;
        match args[i].as_str() {
            "--from" => from = Some(v.clone()),
            "--to" => to = Some(v.clone()),
            "--min" => min = Some(v.parse().map_err(|_| t!("err.bad_min"))?),
            "--max" => max = Some(v.parse().map_err(|_| t!("err.bad_max"))?),
            "--catch-up" => catchup = Some(v.parse().map_err(|_| t!("err.bad_catchup"))?),
            f => return Err(t!("err.unknown_flag", flag = f)),
        }
        i += 2;
    }
    // validation lives in core (one source of truth)
    let (Some(min), Some(max)) = (min, max) else {
        return Err(t!("err.sched_needs_minmax"));
    };
    let mut o = gitlarp_core::serde_json::json!({ "min": min, "max": max });
    if !weekends {
        o["weekends"] = false.into();
    }
    if let Some(c) = catchup {
        o["catchup"] = c.into();
    }
    if let Some(f) = from {
        o["from"] = f.as_str().into();
    }
    if let Some(t) = to {
        o["to"] = t.as_str().into();
    }
    gitlarp_core::schedule::parse_spec(&o).map_err(msg)
}

fn sched_from_config(s: &config::Schedule) -> Result<ScheduleSpec, String> {
    let mut o = gitlarp_core::serde_json::json!({
        "min": s.min, "max": s.max, "weekends": s.weekends, "catchup": s.catchup
    });
    if let Some(f) = &s.from {
        o["from"] = f.as_str().into();
    }
    if let Some(t) = &s.to {
        o["to"] = t.as_str().into();
    }
    gitlarp_core::schedule::parse_spec(&o).map_err(msg)
}

fn state_path(ctx: &Ctx) -> PathBuf {
    ctx.home.join("schedule-state")
}

fn load_last(ctx: &Ctx) -> Result<Option<Date>, String> {
    let p = state_path(ctx);
    if !p.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&p).map_err(|e| t!("state.read", path = format!("{:?}", p), e = e))?;
    let line = text.lines().next().unwrap_or_default().trim();
    let Some(v) = line.strip_prefix("last = \"") else {
        return Err(t!("state.bad", path = format!("{:?}", p)));
    };
    date::parse(v.trim_end_matches('"')).map(Some).map_err(|e| e.message)
}

fn save_last(ctx: &Ctx, d: Date) -> Result<(), String> {
    fs::create_dir_all(&ctx.home).map_err(|e| e.to_string())?;
    fs::write(state_path(ctx), format!("last = \"{}\"\n", date::fmt(d))).map_err(|e| e.to_string())
}

fn schedule_status(ctx: &Ctx) -> Result<(), String> {
    let config = load_config(ctx)?;
    let Some(s) = &config.schedule else {
        println!("{}", t!("info.no_schedule_status"));
        return Ok(());
    };
    let sched = sched_from_config(s)?;
    let last = load_last(ctx)?;
    let next = next_due(&sched, last, date::today_utc());
    println!(
        "{}",
        t!(
            "info.sched_line",
            min = s.min,
            max = s.max,
            weekends = if s.weekends { t!("info.weekends_included") } else { t!("info.weekends_excluded") },
            from = s.from.as_deref().unwrap_or("today"),
            to = s.to.as_deref().unwrap_or("ongoing")
        )
    );
    println!("{}", t!("info.catchup", n = s.catchup));
    match last {
        Some(d) => println!("{}", t!("info.last_run", date = date::fmt(d))),
        None => println!("{}", t!("info.last_run_never")),
    }
    match next {
        Some(d) => println!("{}", t!("info.next_due", date = date::fmt(d))),
        None => println!("{}", t!("info.next_due_none")),
    }
    println!(
        "{}",
        if cron_installed() { t!("info.cron_installed") } else { t!("info.cron_not_installed") }
    );
    Ok(())
}

fn schedule(ctx: &Ctx, args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return schedule_status(ctx);
    }
    if args.len() == 1 && args[0] == "--off" {
        let mut config = load_config(ctx)?;
        if config.schedule.take().is_none() {
            println!("{}", t!("info.nothing_to_off"));
            return Ok(());
        }
        config::save(&ctx.config_path(), &config)?;
        println!("{}", t!("info.schedule_off"));
        return Ok(());
    }
    let sched = parse_schedule_args(args)?;
    let mut config = load_config(ctx)?;
    let updated = config.schedule.is_some();
    config.schedule = Some(config::Schedule {
        from: sched.from.map(date::fmt),
        to: sched.to.map(date::fmt),
        min: sched.min,
        max: sched.max,
        weekends: sched.weekends,
        catchup: sched.catchup,
    });
    config::save(&ctx.config_path(), &config)?;
    save_last(ctx, date::today_utc())?;
    println!(
        "{}",
        t!(
            "info.schedule_set",
            state = if updated { t!("info.state_updated") } else { t!("info.state_enabled") }
        )
    );
    schedule_status(ctx)
}

fn load_config(ctx: &Ctx) -> Result<Config, String> {
    config::load(&ctx.config_path())
}

// --- cron ---

fn cron(ctx: &Ctx, args: &[String], dry: bool) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("--install") => cron_install(ctx),
        Some("--uninstall") => cron_uninstall(),
        Some(f) => Err(t!("err.unknown_flag", flag = f)),
        None => cron_run(ctx, dry),
    }
}

fn cron_run(ctx: &Ctx, dry: bool) -> Result<(), String> {
    let config = load_config(ctx)?;
    let Some(s) = &config.schedule else {
        return Err(t!("err.no_schedule"));
    };
    let sched = sched_from_config(s)?;
    let now = date::today_utc();
    let last = load_last(ctx)?;
    let skipped = catchup_skipped(&sched, last, now);
    if skipped > 0 {
        eprintln!("{}", t!("warn.catchup", n = skipped, cap = sched.catchup));
    }
    let mut rng = |lo: u32, hi: u32| fastrand::u32(lo..=hi);
    let days = due_days(&sched, last, now, &mut rng);
    if days.is_empty() {
        match next_due(&sched, Some(now), now) {
            Some(d) => println!("{}", t!("info.nothing_due_today", date = date::fmt(d))),
            None => println!("{}", t!("info.nothing_due")),
        }
        if !dry {
            save_last(ctx, now)?;
        }
        return Ok(());
    }
    let plan = build_plan(&days, false, now, CLI_CAPS).map_err(msg)?;
    run_plan(&plan, dry)?;
    if !dry {
        save_last(ctx, now)?;
    }
    Ok(())
}

fn cron_installed() -> bool {
    #[cfg(target_os = "macos")]
    {
        cron_installed_in(&PathBuf::from(env::var("HOME").unwrap_or_default()))
    }
    #[cfg(target_os = "linux")]
    {
        Command::new("crontab")
            .arg("-l")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("# gitlarp"))
            .unwrap_or(false)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        false
    }
}

#[cfg(target_os = "macos")]
fn cron_installed_in(home: &std::path::Path) -> bool {
    launchd_plist_path(home).exists()
}

#[cfg(target_os = "macos")]
fn launchd_plist_path(home: &std::path::Path) -> PathBuf {
    home.join("Library/LaunchAgents/io.gitlarp.schedule.plist")
}

fn cron_install(ctx: &Ctx) -> Result<(), String> {
    let exe = env::current_exe().map_err(|e| t!("err.exe", e = e))?;
    #[cfg(target_os = "macos")]
    {
        let minute = fastrand::u8(0..60);
        let home = PathBuf::from(env::var("HOME").map_err(|_| t!("err.no_home"))?);
        let log = ctx.home.join("cron.log");
        let path = write_launchd_plist(&home, &exe, minute, &log)?;
        let p = path.to_str().unwrap_or_default();
        let _ = Command::new("launchctl").args(["unload", p]).status();
        let st = Command::new("launchctl")
            .args(["load", "-w", p])
            .status()
            .map_err(|e| t!("err.launchctl_run", e = e))?;
        if !st.success() {
            return Err(t!("err.launchctl_load"));
        }
        println!(
            "{}",
            t!("info.installed_macos", minute = format!("{minute:02}"), log = log.display())
        );
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        let minute = fastrand::u8(0..60);
        let out = Command::new("crontab")
            .arg("-l")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
        let entry = crontab_entry(&exe, minute, &ctx.home.join("cron.log").display().to_string());
        let new = merge_crontab(&out, &entry);
        install_crontab(&new)?;
        println!(
            "{}",
            t!("info.installed_linux", minute = format!("{minute:02}"), log = ctx.home.join("cron.log").display())
        );
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = exe;
        Err(t!("err.unsupported_install"))
    }
}

/// Pure plist writer: `launchctl load` stays in cron_install so
/// tests can exercise this without touching the real scheduler.
#[cfg(target_os = "macos")]
fn write_launchd_plist(
    home: &std::path::Path,
    exe: &std::path::Path,
    minute: u8,
    log: &std::path::Path,
) -> Result<PathBuf, String> {
    let dir = home.join("Library/LaunchAgents");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("io.gitlarp.schedule.plist");
    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \x20<key>Label</key><string>io.gitlarp.schedule</string>\n\
         \x20<key>ProgramArguments</key>\n\
         \x20<array><string>{bin}</string><string>cron</string></array>\n\
         \x20<key>RunAtLoad</key><true/>\n\
         \x20<key>StartCalendarInterval</key>\n\
         \x20<dict><key>Hour</key><integer>12</integer><key>Minute</key><integer>{minute:02}</integer></dict>\n\
         \x20<key>StandardOutPath</key><string>{log}</string>\n\
         \x20<key>StandardErrorPath</key><string>{log}</string>\n\
         </dict>\n\
         </plist>\n",
        bin = exe.display(),
        log = log.display(),
    );
    fs::write(&path, plist).map_err(|e| e.to_string())?;
    Ok(path)
}

#[cfg(target_os = "linux")]
fn crontab_entry(exe: &std::path::Path, minute: u8, log: &str) -> String {
    format!(
        "{minute:02} 12 * * * {} cron >> {} 2>&1 # gitlarp",
        exe.display(),
        log
    )
}

/// Replace any existing # gitlarp line with `entry`, preserving the
/// user's other lines.
#[cfg(target_os = "linux")]
fn merge_crontab(existing: &str, entry: &str) -> String {
    let mut new: String = existing
        .lines()
        .filter(|l| !l.contains("# gitlarp"))
        .collect::<Vec<_>>()
        .join("\n");
    if !new.is_empty() {
        new.push('\n');
    }
    new.push_str(entry);
    new.push('\n');
    new
}

#[cfg(target_os = "linux")]
fn install_crontab(text: &str) -> Result<(), String> {
    let mut child = Command::new("crontab")
        .arg("-")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| t!("err.crontab_run", e = e))?;
    child
        .stdin
        .as_mut()
        .ok_or(t!("err.no_stdin"))?
        .write_all(text.as_bytes())
        .map_err(|e| e.to_string())?;
    let st = child.wait().map_err(|e| e.to_string())?;
    if !st.success() {
        return Err(t!("err.crontab_install"));
    }
    Ok(())
}

fn cron_uninstall() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let home = env::var("HOME").map_err(|_| t!("err.no_home"))?;
        let path = PathBuf::from(home).join("Library/LaunchAgents/io.gitlarp.schedule.plist");
        if !path.exists() {
            println!("{}", t!("info.no_launchd"));
            return Ok(());
        }
        let p = path.to_str().unwrap_or_default();
        let _ = Command::new("launchctl").args(["unload", p]).status();
        fs::remove_file(&path).map_err(|e| e.to_string())?;
        println!("{}", t!("info.uninstalled_launchd", path = p));
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        let out = Command::new("crontab")
            .arg("-l")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
        let mut new: String = out
            .lines()
            .filter(|l| !l.contains("# gitlarp"))
            .collect::<Vec<_>>()
            .join("\n");
        if new.is_empty() {
            println!("{}", t!("info.no_crontab"));
            return Ok(());
        }
        new.push('\n');
        install_crontab(&new)?;
        println!("{}", t!("info.uninstalled_crontab"));
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err(t!("err.unsupported_uninstall"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn locale_resolves_and_interpolates() {
        assert!(locale::t("usage").contains("gitlarp init"));
        assert!(!locale::t("usage").contains('{'));
        assert!(locale::t("err.prefix").contains("{e}"));
    }

    #[test]
    fn fill_parses_flags() {
        let (from, to, min, max) =
            parse_fill(&args(&["--from", "2026-01-01", "--to", "2026-01-03", "--min", "2", "--max", "2"])).unwrap();
        assert_eq!(min, 2);
        assert_eq!(max, 2);
        assert_eq!((to - from).whole_days(), 2);
    }

    #[test]
    fn fill_rejects_backwards_range() {
        assert!(parse_fill(&args(&["--from", "2026-01-03", "--to", "2026-01-01"])).is_err());
    }

    #[test]
    fn fill_rejects_min_over_max() {
        assert!(parse_fill(&args(&["--from", "2026-01-01", "--to", "2026-01-02", "--min", "5", "--max", "1"])).is_err());
    }

    #[test]
    fn schedule_parses_flags() {
        let s = parse_schedule_args(&args(&[
            "--min", "2", "--max", "4", "--no-weekends", "--catch-up", "7",
            "--from", "2026-09-01", "--to", "2026-09-30",
        ]))
        .unwrap();
        assert_eq!(s.min, 2);
        assert_eq!(s.max, 4);
        assert!(!s.weekends);
        assert_eq!(s.catchup, 7);
        assert_eq!(s.from, Some(date::parse("2026-09-01").unwrap()));
        assert_eq!(s.to, Some(date::parse("2026-09-30").unwrap()));
    }

    #[test]
    fn schedule_rejects_bad_flags() {
        assert!(parse_schedule_args(&args(&["--min", "5", "--max", "1"])).is_err());
        assert!(parse_schedule_args(&args(&["--min", "0", "--max", "1"])).is_err());
        assert!(parse_schedule_args(&args(&["--min", "1", "--max", "101"])).is_err());
        assert!(parse_schedule_args(&args(&["--min", "1", "--max", "1", "--catch-up", "0"])).is_err());
        assert!(parse_schedule_args(&args(&["--min", "1"])).is_err());
        assert!(parse_schedule_args(&args(&[])).is_err());
    }

    #[test]
    fn state_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("gitlarp-state-{}", fastrand::u32(..)));
        let ctx = Ctx { home: tmp.clone(), force: false };
        assert_eq!(load_last(&ctx).unwrap(), None);
        save_last(&ctx, date::parse("2026-08-24").unwrap()).unwrap();
        assert_eq!(load_last(&ctx).unwrap(), Some(date::parse("2026-08-24").unwrap()));
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn config_roundtrip_with_schedule() {
        let path = std::env::temp_dir().join(format!("gitlarp-cfg-sched-{}", fastrand::u32(..)));
        let c = Config {
            schedule: Some(config::Schedule {
                from: Some("2026-09-01".into()),
                to: None,
                min: 2,
                max: 4,
                weekends: false,
                catchup: 7,
            }),
        };
        config::save(&path, &c).unwrap();
        let l = config::load(&path).unwrap();
        let s = l.schedule.clone().unwrap();
        assert_eq!(s.min, 2);
        assert_eq!(s.max, 4);
        assert!(!s.weekends);
        assert_eq!(s.catchup, 7);
        assert_eq!(s.from.as_deref(), Some("2026-09-01"));
        assert!(s.to.is_none());
        assert!(
            !std::path::Path::new(&format!("{}.tmp", path.display())).exists(),
            "atomic save leaves no .tmp litter"
        );
        fs::remove_file(&path).ok();
    }

    #[test]
    fn config_schedule_needs_min_and_max() {
        let path = std::env::temp_dir().join(format!("gitlarp-cfg-bad-{}", fastrand::u32(..)));
        fs::write(&path, "schedule_min = 2\n").unwrap();
        assert!(config::load(&path).is_err());
        fs::remove_file(&path).ok();
    }

    #[test]
    fn config_ignores_legacy_keys() {
        let path = std::env::temp_dir().join(format!("gitlarp-cfg-old-{}", fastrand::u32(..)));
        fs::write(
            &path,
            "remote = \"https://github.com/x/y.git\"\nemail = \"a@b.c\"\nbranch = \"main\"\nschedule_min = 1\nschedule_max = 2\n",
        )
        .unwrap();
        let l = config::load(&path).unwrap();
        assert!(l.schedule.is_some());
        fs::remove_file(&path).ok();
    }

    #[test]
    fn gh_status_line_parses() {
        // run_gh needs gh installed; just verify the parser via a fake text
        let text = "HTTP/2.0 404 Not Found\nX: y\n\n{\"message\":\"Not Found\"}";
        let status = text.lines().next().and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse::<u16>().ok()).unwrap();
        assert_eq!(status, 404);
        let body = text.split_once("\n\n").map(|(_, b)| b.to_string()).unwrap();
        assert_eq!(body, "{\"message\":\"Not Found\"}");
    }

    // --- fake-gh integration tests ---

    use std::path::Path;
    use std::sync::Mutex;

    /// Every env-mutating / PATH-touching test holds this lock: the
    /// mutations are process-global and Rust runs tests in parallel.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Snapshot of the env vars we mutate, restored on drop.
    struct EnvGuard {
        path: Option<String>,
        home: Option<String>,
        gitlarp_home: Option<String>,
    }

    impl EnvGuard {
        fn new() -> EnvGuard {
            EnvGuard {
                path: env::var("PATH").ok(),
                home: env::var("HOME").ok(),
                gitlarp_home: env::var("GITLARP_HOME").ok(),
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.path.take() {
                Some(v) => env::set_var("PATH", v),
                None => env::remove_var("PATH"),
            }
            match self.home.take() {
                Some(v) => env::set_var("HOME", v),
                None => env::remove_var("HOME"),
            }
            match self.gitlarp_home.take() {
                Some(v) => env::set_var("GITLARP_HOME", v),
                None => env::remove_var("GITLARP_HOME"),
            }
        }
    }

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("{tag}-{}", fastrand::u32(..)));
        fs::create_dir_all(&p).unwrap();
        p
    }

    const FAKE_GH: &str = r#"#!/bin/sh
case "$1 $2" in
  "auth token") echo "ghp_fake_token" ;;
  api*)
    cat >/dev/null 2>/dev/null || true
    case "$2" in
      /user) echo "HTTP/2.0 200 OK"; echo ""; echo '{"id":123,"login":"octocat"}' ;;
      /repos/*) echo "HTTP/2.0 200 OK"; echo ""; echo '{"default_branch":"main"}' ;;
      *)
        echo "HTTP/2.0 201 Created"
        echo ""
        echo "{\"gh_token\":\"$GH_TOKEN\",\"args\":\"$*\"}"
        ;;
    esac ;;
  *) echo "unexpected gh call: $*" >&2; exit 1 ;;
esac
"#;

    /// Install `script` as `gh` on PATH for the duration of `f`
    /// (serialized + env-restored via ENV_LOCK and EnvGuard).
    fn with_gh_script(script: &str, f: impl FnOnce(&Path)) {
        let _g = ENV_LOCK.lock().unwrap();
        let _e = EnvGuard::new();
        let dir = tmp("gitlarp-fakegh");
        let bin = dir.join("gh");
        fs::write(&bin, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        }
        env::set_var(
            "PATH",
            format!("{}:{}", dir.display(), env::var("PATH").unwrap_or_default()),
        );
        f(&dir);
        fs::remove_dir_all(&dir).ok();
    }

    /// GITLARP_HOME pointed at a fresh temp dir (no PATH change).
    fn with_gitlarp_home(f: impl FnOnce(&Path)) {
        let _g = ENV_LOCK.lock().unwrap();
        let _e = EnvGuard::new();
        let home = tmp("gitlarp-e2e");
        env::set_var("GITLARP_HOME", &home);
        f(&home);
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn scan_args_splits_flags() {
        let (force, dry, rest) = scan_args(&args(&["day", "2026-08-20", "3", "--dry-run", "--force"]));
        assert!(force);
        assert!(dry);
        assert_eq!(rest, vec!["day".to_string(), "2026-08-20".to_string(), "3".to_string()]);
    }

    #[test]
    fn run_unknown_command_is_usage_error() {
        assert!(run(&args(&["nonsense"])).is_err());
        assert!(run(&args(&[])).is_err());
        assert!(run(&args(&["--help"])).is_ok());
    }

    /// The REAL run_gh against a REAL fake `gh` on PATH: status parse,
    /// body extraction, Authorization -> GH_TOKEN, flag wiring,
    /// stdin body.
    #[test]
    fn run_gh_end_to_end_parses_status_body_and_args() {
        with_gh_script(FAKE_GH, |_| {
            let req = HttpRequest {
                method: "POST".into(),
                url: "https://api.github.com/graphql".into(),
                headers: vec![
                    ("Authorization".into(), "Bearer ghp_x".into()),
                    ("Content-Type".into(), "application/json".into()),
                ],
                body: Some("{\"query\":\"q\"}".into()),
            };
            let res = run_gh(req).unwrap();
            assert_eq!(res.status, 201);
            let v: gitlarp_core::serde_json::Value = gitlarp_core::serde_json::from_str(&res.body).unwrap();
            assert_eq!(v["gh_token"], "ghp_x", "Authorization header -> GH_TOKEN env");
            let a = v["args"].as_str().unwrap();
            assert!(a.contains("--method POST"), "method wiring: {a}");
            assert!(a.contains("-H Content-Type: application/json"), "header wiring: {a}");
            assert!(a.contains("--input -"), "stdin body wiring: {a}");
            assert!(a.contains("/graphql"), "path: {a}");
        });
    }

    /// A `gh` whose output format changed (no parseable status line)
    /// must surface an error, not a bogus 0/200.
    #[test]
    fn run_gh_changed_output_format_errors() {
        let script = "#!/bin/sh\ncat >/dev/null 2>/dev/null || true\necho \"HTTP/1.1\"\necho \"\"\necho '{}'\n";
        with_gh_script(script, |_| {
            let req = HttpRequest {
                method: "GET".into(),
                url: "https://api.github.com/user".into(),
                headers: vec![],
                body: None,
            };
            let err = run_gh(req).map(|_| ()).unwrap_err();
            assert!(err.message.contains("HTTP/1.1"), "tail of broken output: {}", err.message);
        });
    }

    #[test]
    fn run_gh_stderr_only_errors() {
        let script = "#!/bin/sh\necho \"gh: boom\" >&2\nexit 1\n";
        with_gh_script(script, |_| {
            let req = HttpRequest {
                method: "GET".into(),
                url: "https://api.github.com/user".into(),
                headers: vec![],
                body: None,
            };
            let err = run_gh(req).map(|_| ()).unwrap_err();
            assert!(err.message.contains("gh: boom"), "{}", err.message);
        });
    }

    /// A hung `gh` is killed at the deadline and surfaces a timeout
    /// error instead of hanging the CLI forever.
    #[test]
    fn run_gh_timeout_kills_hung_subprocess() {
        let script = "#!/bin/sh\ncat >/dev/null 2>/dev/null || true\nsleep 5\n";
        with_gh_script(script, |_| {
            let req = HttpRequest {
                method: "GET".into(),
                url: "https://api.github.com/user".into(),
                headers: vec![],
                body: None,
            };
            let started = std::time::Instant::now();
            let err = run_gh_timeout(req, 100).map(|_| ()).unwrap_err();
            assert!(err.message.contains("timed out"), "{}", err.message);
            // killed long before the script's own 5s sleep ends
            assert!(started.elapsed() < std::time::Duration::from_secs(3));
        });
    }

    /// A gh that finishes within the budget still parses normally.
    #[test]
    fn run_gh_timeout_completes_fast_child() {
        let script = "#!/bin/sh\ncat >/dev/null 2>/dev/null || true\necho \"HTTP/2.0 200 OK\"\necho \"\"\necho '{}'\n";
        with_gh_script(script, |_| {
            let req = HttpRequest {
                method: "GET".into(),
                url: "https://api.github.com/user".into(),
                headers: vec![],
                body: None,
            };
            let res = run_gh_timeout(req, 10_000).unwrap();
            assert_eq!(res.status, 200);
        });
    }

    #[test]
    fn gh_token_reads_gh_auth_token() {
        with_gh_script(FAKE_GH, |_| {
            assert_eq!(gh_token().unwrap(), "ghp_fake_token");
        });
    }

    /// Full `init` through run(): gh auth token -> /user -> ensure_repo
    /// -> config written to GITLARP_HOME.
    #[test]
    fn e2e_init_writes_config() {
        with_gh_script(FAKE_GH, |_| {
            let home = tmp("gitlarp-e2e-init");
            env::set_var("GITLARP_HOME", &home);
            run(&args(&["init"])).unwrap();
            assert!(home.join("config.toml").exists(), "init writes config.toml");
            fs::remove_dir_all(&home).ok();
        });
    }

    #[test]
    fn e2e_day_dry_run_needs_no_gh() {
        // dry-run prints the plan only, proving dispatch + parsing +
        // plan caps without any transport
        run(&args(&["day", "2026-08-20", "3", "--dry-run"])).unwrap();
        assert!(run(&args(&["day", "not-a-date", "1"])).is_err());
        assert!(run(&args(&["day"])).is_err());
        assert!(run(&args(&["day", "2026-08-20"])).is_err());
    }

    #[test]
    fn e2e_fill_dry_run() {
        run(&args(&[
            "fill", "--from", "2026-08-20", "--to", "2026-08-22", "--min", "1", "--max", "2",
            "--dry-run",
        ]))
        .unwrap();
        assert!(run(&args(&["fill", "--min", "1", "--max", "2", "--dry-run"])).is_err());
    }

    #[test]
    fn e2e_cron_without_schedule_errors() {
        with_gitlarp_home(|_| {
            assert!(run(&args(&["cron"])).is_err());
        });
    }

    /// last=today -> nothing due today, cron exits Ok.
    #[test]
    fn e2e_cron_nothing_due_saves_state() {
        with_gitlarp_home(|home| {
            let ctx = Ctx { home: home.to_path_buf(), force: false };
            config::save(
                &ctx.config_path(),
                &Config { schedule: Some(config::Schedule { from: None, to: None, min: 1, max: 4, weekends: true, catchup: 14 }) },
            )
            .unwrap();
            save_last(&ctx, date::today_utc()).unwrap();
            run(&args(&["cron", "--dry-run"])).unwrap();
            let state = fs::read_to_string(state_path(&ctx)).unwrap();
            assert!(state.contains(&date::fmt(date::today_utc())), "state advanced: {state}");
        });
    }

    #[test]
    fn e2e_schedule_set_off_and_status() {
        with_gitlarp_home(|home| {
            run(&args(&["schedule", "--min", "1", "--max", "4", "--no-weekends"])).unwrap();
            assert!(home.join("config.toml").exists());
            let ctx = Ctx { home: home.to_path_buf(), force: false };
            let c = config::load(&ctx.config_path()).unwrap();
            let s = c.schedule.clone().unwrap();
            assert_eq!((s.min, s.max, s.weekends), (1, 4, false));
            let state = fs::read_to_string(state_path(&ctx)).unwrap();
            assert!(state.contains(&date::fmt(date::today_utc())), "schedule stamps last=today");
            run(&args(&["schedule"])).unwrap(); // status
            run(&args(&["schedule", "--off"])).unwrap();
            assert!(config::load(&ctx.config_path()).unwrap().schedule.is_none());
            run(&args(&["schedule", "--off"])).unwrap(); // nothing to off; still Ok
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn launchd_plist_writes_without_launchctl() {
        let home = tmp("gitlarp-launchd");
        let log = home.join("cron.log");
        let exe = env::current_exe().unwrap();
        let path = write_launchd_plist(&home, &exe, 35, &log).unwrap();
        assert_eq!(path, launchd_plist_path(&home));
        assert!(cron_installed_in(&home));
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("<string>io.gitlarp.schedule</string>"));
        assert!(text.contains(exe.display().to_string().as_str()));
        assert!(text.contains("<integer>35</integer>"));
        fs::remove_dir_all(&home).ok();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn crontab_merge_replaces_gitlarp_line() {
        let existing = "0 9 * * * backup\n45 8 * * * old # gitlarp\n";
        let entry = "35 12 * * * /usr/bin/gitlarp cron >> /home/l/cron.log 2>&1 # gitlarp";
        let merged = merge_crontab(existing, entry);
        assert!(merged.contains("0 9 * * * backup"), "user lines preserved: {merged}");
        assert!(!merged.contains("old # gitlarp"));
        assert!(merged.contains(entry));
        assert_eq!(merge_crontab("", entry), format!("{entry}\n"));
    }
}
