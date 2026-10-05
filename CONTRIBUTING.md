# Contributing

One core, four shells. `apps/core` (Rust) holds every piece of domain
logic: plan validation, schedules, the GitHub commit engine, crypto,
the schedule runner. The other apps are thin adapters: `apps/cli`
(local client, `gh` transport), `apps/server` (actix-web API),
`apps/worker` (Cloudflare Workers + D1), `apps/web` (Next.js UI, pure
client). Fix domain bugs in core, adapter bugs in the adapter.

## Setup

- Rust stable toolchain
- bun 1.4.x (matches `packageManager` in `package.json`)
- for the worker: `wasm32-unknown-unknown` target
- for end-to-end docker checks: docker with compose

## Before you open a PR

Run the full gate set; CI runs the same:

```sh
bun install
cargo test --workspace                    # core, cli, server
cargo test --manifest-path apps/worker/Cargo.toml   # worker host tests
cargo check --manifest-path apps/worker/Cargo.toml --target wasm32-unknown-unknown
cargo clippy --workspace --all-targets   # must be warning-free
bun run test                              # cli + web
bash scripts/gates.sh                     # resource budget table
bash scripts/docker-smoke.sh              # before releases; isolated compose project
```

Guidelines:

- **Tests move with the change.** New behavior gets a test next to
  the existing suites (Rust tests live in-module; web tests are
  `*.test.ts(x)` run by bun).
- **Security invariants are load-bearing.** PATs must never appear in
  argv, logs, responses, or plaintext at rest. The run endpoint fails
  closed. Nonces come from OS entropy. Keep the tests that prove
  these behaviors intact.
- **Keep it small.** The CLI binary, first-load JS, and worker bundle
  are budgeted (`scripts/gates.sh` exits red over budget).
- **Match the local style**: plain std/actix/worker-rs, no new heavy
  dependencies, terse comments that say *why*.

## Commit / branch hygiene

Small focused commits, imperative subject lines. Don't commit
generated state (`.next/`, `.open-next/`, `.wrangler/`, `target/`,
`data/` are ignored — keep it that way).
