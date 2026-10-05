# gitlarp

LARP your GitHub contribution graph. Two thin tools, a Rust CLI and a headless web API, write backdated commits to a private repo, and they actually count.

## Install (CLI)

```sh
cargo install --path apps/cli
```

Requires the `gh` CLI on PATH, logged in (`gh auth login`). Every CLI command
takes its token from `gh auth token` and sends all GitHub traffic through
`gh api`: that's the whole auth story; nothing in the code verifies email.

## Quickstart

```sh
gitlarp init
gitlarp day 2026-08-20 3
gitlarp fill --from 2026-08-21 --to 2026-08-25 --min 1 --max 5
gitlarp schedule --min 1 --max 4 --no-weekends --from 2026-08-26
gitlarp cron --install
gitlarp wipe
```

- `day <YYYY-MM-DD> <n>`: n commits on one day
- `fill --from <YYYY-MM-DD> --to <YYYY-MM-DD> [--min n] [--max n]`: 1-5 random commits per day in the range
- `schedule` / `cron`: auto-pilot (see below)
- `wipe`: reset the repo to `gitlarp-base`, undoing every larp commit
- `--dry-run`: print the plan without committing or pushing
- `--force`: allow out-of-window dates and >100 commits/day (cap becomes 1000)

Dates are clamped to the rolling 12-month window; commits are stamped 12:00 UTC. The repo is private and stays private.

## Scheduling (auto-pilot)

`gitlarp schedule` defines a daily plan; `gitlarp cron` executes whatever is due. Together they automate future commits, gapless.sh-style, from your own machine.

```sh
gitlarp schedule --min 1 --max 4 --no-weekends --from 2026-08-26
gitlarp cron --install    # daily launchd entry (macOS) or crontab line (Linux)
gitlarp cron              # run due days now (also --dry-run)
gitlarp schedule          # status: next due, last run, cron installed
gitlarp schedule --off    # disable
```

- `--min n --max n`: random commits per day in that range (max 100)
- `--from` / `--to`: first/last scheduled day (default today / ongoing)
- `--no-weekends`: skip Saturdays and Sundays
- `--catch-up n`: after the machine was off, backfill up to n missed days (default 14); older days stay empty
- each day's commits are created on that day, stamped 12:00 UTC like `day`, always inside the 12-month window
- the cron trigger fires daily at a random 12:xx minute local time (and at login on macOS); logs go to `~/.gitlarp/cron.log`
- runs unattended: `cron` re-uses `gh`'s stored credentials non-interactively

## Web app

```sh
bun install
bun run dev            # http://localhost:3000
```

Pure UI: no API routes, no scheduler, no store. It needs a headless API to
talk to, resolved in this order: the on-page "API URL" input (persisted in
sessionStorage) → `NEXT_PUBLIC_GITLARP_API_URL` (build time) →
`http://localhost:8080`. The PAT travels in the `Authorization` header for
graph fetches and in the JSON body for commits; the UI shows the exact API
base it will be sent to next to the PAT input.

One core, four shells. `apps/core` (Rust) holds every piece of domain
logic: plan validation, schedules, the GitHub commit engine, crypto,
the schedule runner. The other apps are thin adapters over it:

- `apps/cli`: the local client; links core natively, talks to GitHub
  through the `gh` CLI's transport, so the binary stays tiny
- `apps/web`: the Next.js UI; pure client, talks to the headless API over HTTP
- `apps/server`: a generic actix-web API server (Docker / any host)
- `apps/worker`: the Cloudflare Workers adaptor (Rust → WASM, D1 store,
  daily cron trigger)

The headless API (`/api/commits`, `/api/graph`, `/api/schedules`) is
served by either `apps/server` or `apps/worker`; `apps/web` provides the
UI. Scheduling is provider-agnostic: one encrypted schedule store with
two adapters (filesystem for the server, D1 for Cloudflare), and one
executor reachable from an in-process loop, a cron trigger, or a plain
HTTP call.

### Docker (1-click)

Two services: `server` (the headless API, port 8080, filesystem store on
the mounted `/data` volume, optional hourly in-process scheduler with
`SCHEDULE_LOOP=1`) and `web` (the UI, built with
`NEXT_PUBLIC_GITLARP_API_URL` pointing at the server's published port).

```sh
export GITLARP_SCHEDULE_SECRET=$(openssl rand -base64 32)
docker compose up -d --build     # web on http://localhost:3000, API on http://localhost:8080
```

Without the secret, scheduling is disabled; everything else keeps working.

### Cloudflare Workers (1-click)

```sh
wrangler login
bash scripts/setup-cloudflare.sh  # creates the D1 database, patches the id into
                                  # apps/worker/wrangler.jsonc, sets the secret
cd apps/worker && wrangler deploy # daily cron trigger at 12:30 UTC + D1 store
```

### Other providers

`POST /api/schedules/run` runs whatever is due; point any cron service,
k8s CronJob, or uptime monitor at it (with `Authorization: Bearer <secret>`).
On the server, schedules live in `SCHEDULE_DIR` (default `./data/schedules`).

## Embeddable widget

```html
<iframe src="https://your-web.app/widget?theme=dark" width="800" height="400"></iframe>
```

Served by the web deploy (point it at your headless API via the API-URL
input or `NEXT_PUBLIC_GITLARP_API_URL`). Works cross-origin: CORS is open
on all responses, preflights included. `theme=light` also supported.

The framed widget deliberately ignores an `?api=` parameter: any site can
embed the iframe, and a forwarded base would let the embedding site choose
where a visitor's typed PAT is sent. The UI always shows the resolved API
base next to the PAT input — only paste a PAT into a widget whose API base
you trust.

## Headless API (the product)

`POST /api/commits`

```sh
curl -X POST https://your-server.app/api/commits \
  -H 'Content-Type: application/json' \
  -d '{"pat":"github_pat_...","days":[{"date":"2026-08-20","count":3}]}'
```

- dates are `YYYY-MM-DD`; out-of-window dates are clamped to the rolling 12 months and counted in `clamped`
- caps: 500 commits per request, 500 per day on `apps/server`; the Cloudflare worker enforces its own caps (40 per request, 40 per day) because Workers allows ~50 upstream fetches per invocation on the free plan — larger requests fail fast with a 400. Work is chunked into ≤50-commit sub-batches with ~1s delays; 403/429 retried once
- the private repo `gitlarp-history` is auto-created on first use
- full success: `{"created":3,"total":3}`
- partial failure: `{"created":2,"total":3,"partial":true,"error":"..."}`
- clamped dates add `"clamped":1`

`GET /api/graph` returns `{"counts":{"YYYY-MM-DD":n}}` for the last 12 months.

Auth: `Authorization: Bearer <PAT>` on `GET /api/graph` and
`GET|DELETE /api/schedules` — the PAT lives in the header only, never in
URLs (a `?pat=` query parameter existed once and was removed; it put live
tokens in proxies' logs and browser history). `POST /api/commits` and
`POST /api/schedules` carry the PAT in the JSON body.
`GET /healthz` → `200 {"ok":true}`, no auth.

### Scheduling API

```sh
# create a schedule (min..max commits per matching day)
curl -X POST https://your-server.app/api/schedules \
  -H 'Content-Type: application/json' \
  -d '{"pat":"github_pat_...","min":1,"max":4,"weekends":false,"catchup":14,"from":"2026-08-26","to":"2026-12-31"}'
# -> {"id":"..."}

curl https://your-server.app/api/schedules \
  -H 'Authorization: Bearer github_pat_...'      # list (spec + lastRun, never the PAT)
curl -X DELETE 'https://your-server.app/api/schedules?id=...' \
  -H 'Authorization: Bearer github_pat_...'      # remove; missing id param -> 400,
                                                 # unknown id -> 404 {"error":"no such schedule"}
curl -X POST https://your-server.app/api/schedules/run \
  -H 'Authorization: Bearer <GITLARP_SCHEDULE_SECRET>'   # run due schedules now
```

- PATs are stored server-side, AES-256-GCM encrypted with `GITLARP_SCHEDULE_SECRET` (scheduling is disabled without it); the secret must be at least 16 bytes — the server refuses to boot and the worker fail-closes with shorter secrets. Use a **random** secret (`openssl rand -base64 32`), not a passphrase: the AES key is a plain SHA-256 of it, so a guessable secret is brute-forceable offline if the store leaks
- the run endpoint fails closed: secret unset → `503` (it never executes); wrong bearer → `401` (constant-time compare)
- rotation: set `GITLARP_SCHEDULE_SECRET_OLD` (server) and records encrypted with the old secret decrypt and are lazily re-encrypted — on read, and on run (the runner heals them; a healed record's payload is stored under the current secret). The worker has no old-secret support: rotated records stay skipped until a GET heals them
- each day's commits are created on that day (12:00 UTC), inside the rolling window; no future-dated commits
- missed days are caught up on the next run, bounded by `catchup` (default 14, max 365); days beyond that stay empty. On the worker each schedule run is capped at 40 commits (subrequest limits): excess days are deferred to the next tick, not dropped, and `max` above 40 is rejected at create time there. The worker's cron tick is capped invocation-wide at 40 commits too (~50 upstream fetches per invocation): one tick comfortably serves one active schedule, and further records are deferred — `lastRun` untouched, counted in `deferred` — to the next tick
- `weekends` (default `true`), `from`/`to` (default today/ongoing), `min <= max <= 100` on `apps/server` (worker: `max <= 40`)
- failure policy: a run that fails leaves `lastRun` untouched and retries the window on the next tick; partial successes are marked done

Rate limiting: fixed window of 30 req/min per IP on every endpoint that
talks to upstream GitHub (`POST /api/commits`, `POST /api/schedules`,
`POST /api/schedules/run`, `DELETE /api/schedules`, `GET /api/schedules`,
and `GET /api/graph`, which costs two upstream calls per request) —
enforced on both `apps/server` and `apps/worker`; over-limit → `429` +
`Retry-After: 60`. `GET /healthz` stays exempt. The limiter keys on the
direct TCP peer address (it deliberately does not trust `X-Forwarded-For`),
so behind a reverse proxy every client shares one bucket — expose the
server directly or account for that.

Errors: core 4xx (validation: bad dates, missing pat, caps, invalid
PAT, unknown schedule) pass through verbatim; anything else maps to
`502` (GitHub failure). Schedule create/list/run endpoints (and
`/api/schedules/run`) return `503` when `GITLARP_SCHEDULE_SECRET` is
unset; `DELETE /api/schedules` works without it (deletion never
decrypts). CORS `*`.

## Env vars

| Var | Where | Meaning |
|---|---|---|
| `PORT` | server | listen port (default 3000) |
| `SCHEDULE_DIR` | server | schedule store dir (default `./data/schedules`) |
| `GITLARP_SCHEDULE_SECRET` | server, worker | schedule encryption + run-endpoint bearer; unset ⇒ run 503s, scheduling disabled. Must be ≥16 **bytes** and random (`openssl rand -base64 32`), not a passphrase: the AES key is a plain SHA-256 of it, and the server refuses to boot / the worker fail-closes with shorter secrets |
| `GITLARP_SCHEDULE_SECRET_OLD` | server | lazy-rotation fallback decrypt (runner heals on run and on read; worker: none — GET heals) |
| `SCHEDULE_LOOP` | server | `1` = hourly in-process scheduler |
| `GH_TOKEN` / gh auth | CLI | transport auth (never argv) |
| `GITLARP_HOME`, `GITLARP_REPO` | CLI | config dir / repo name |
| `GITLARP_LANG` | CLI | `es` or anything-else→en |
| `NEXT_PUBLIC_GITLARP_API_URL` | web (build time) | API base for the UI |

## Caveats

- Private contributions must be on or nothing shows: https://github.com/settings/profile
- API-made commits are unsigned; GitHub marks them Unverified (GPG)
- The graph only shows the rolling 12 months; updates lag a few minutes
- This flirts with GitHub's ToS; keep the repo private; your account, your risk

## License

[MIT](./LICENSE)

## Gates + CI

- `bash scripts/gates.sh`: prints the resource-budget table (CLI binary < 512000 B, CLI startup < 5 ms, web first-load JS < 100 KB gzipped, worker bundle < 1 MB) and exits 1 on any red row
- `bash scripts/docker-smoke.sh`: builds the compose stack (server + web) and probes `/healthz`, the web UI, and the API end to end; runs as its own compose project on remapped ports (39080/39000), so it is safe next to a live deployment; run before a release
- `.github/workflows/ci.yml`: web build + tests, `cargo test --workspace` (core, CLI, server), a wasm32 check plus host tests of `apps/worker`, and gates on every push and PR
- `.github/workflows/smoke.yml`: manual or monthly live smoke against a real account; needs secrets `SMOKE_PAT`, `SMOKE_LOGIN`, optional `SMOKE_WORKER_URL`
