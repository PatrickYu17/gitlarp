## What & why

<!-- One-paragraph summary; say *why*, not just *what*. -->

## Where it lives

<!-- core (domain logic) or one of the adapters (cli / server / worker / web) -->

## Checklist

- [ ] Tests added/updated next to the existing suites
- [ ] `cargo test --workspace` + worker host tests + wasm check pass
- [ ] `cargo clippy --workspace --all-targets` warning-free
- [ ] `bun run test` passes; `bash scripts/gates.sh` stays green
- [ ] No PAT can appear in argv, logs, responses, or plaintext at rest
- [ ] README updated if behavior changed
