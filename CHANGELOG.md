# Changelog

All notable changes are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); the project
follows Cargo's version numbers.

## [Unreleased]

Hardening pass from the pre-publication security audit.

### Security

- The framed `/widget` no longer honors an `?api=` parameter: any site
  can embed the iframe, and a forwarded base let the embedding site
  pick where a visitor's typed PAT was sent. The UI now also shows the
  resolved API base next to the PAT input and warns when that base
  would send the PAT in cleartext (plain `http:` to a non-loopback
  host).
- Removed the deprecated `?pat=` query parameter from the headless API
  (server and worker): PATs travel in the `Authorization` header (or
  JSON body) only, keeping live tokens out of URLs, proxy logs, and
  browser history.
- `scripts/smoke.sh` no longer prints the smoke account's login into
  public CI logs, and no longer deletes any repo it did not create
  (unique per-run repo name + created-flag cleanup).
- `scripts/docker-smoke.sh` runs as its own compose project
  (`gitlarp-smoke`) on remapped ports, so it can no longer tear down a
  live deployment or its `gitlarp-data` volume.
- `.dockerignore` excludes `.env` files so local secrets cannot be
  baked into images.
- Schedule secret minimum length is 16 **bytes** on both surfaces
  (server previously counted chars); the docs now require a random
  secret, not a passphrase.

### Fixed

- Worker: `GET /api/graph` and `GET /api/schedules` answer CORS
  preflights (the missing `OPTIONS` route on `/api/graph` broke every
  cross-origin browser client against the worker).
- Rate limiting now covers every upstream-talking route on both
  surfaces, including `GET /api/schedules` and the worker's
  `GET /api/graph` (previously unbounded).

### Changed

- The worker's cron tick is capped invocation-wide at 40 commits:
  multiple stored schedules can no longer blow the ~50-subrequest
  ceiling and starve later records; deferred records are counted in
  `deferred` and keep their `lastRun`.
- Secret rotation heals in the runner as well: old-secret records
  decrypt, run, and re-encrypt under the current secret (worker keeps
  no old-secret support and heals via GET).
- `encrypt_json` generates its IV internally; caller-supplied IVs are
  confined to deterministic tests via `encrypt_json_with_iv`.
- Web image runs as a non-root user with a `HEALTHCHECK`; the CLI
  `chunk_plan` asserts `chunk size > 0`.

### Added

- `SECURITY.md`, `CONTRIBUTING.md`, issue/PR templates, Dependabot
  config (cargo workspaces + bun), and least-privilege `permissions`
  blocks on the CI workflows.

## [0.2.0] - 2026-10-04

Initial release: gitlarp CLI, headless API (server + Cloudflare
worker), and web UI.
