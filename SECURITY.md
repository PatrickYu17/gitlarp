# Security Policy

## Supported versions

Only the latest release on `master` is supported.

## Reporting a vulnerability

Use GitHub's **private security advisory** for this repository
(Report a vulnerability → Security tab). Please do not open a public
issue for security reports.

Include what you found, how it could be abused, and — if you have it —
a minimal reproduction. You should get a first response within a few
days.

## What counts as a security issue

gitlarp handles GitHub PATs and an operator secret, so the interesting
bugs are:

- any path that leaks a PAT: responses, logs, error bodies, URLs,
  argv, or at-rest storage
- auth bypass on the headless API or the run endpoint
  (`POST /api/schedules/run`)
- weaknesses in the schedule-store encryption
  (`GITLARP_SCHEDULE_SECRET`) or its rotation
- injection into the GitHub API calls, D1 queries, or the filesystem
  schedule store
- XSS in the web UI or the embedded widget

Hosted deployments are configured by their operators; report issues
that affect any correctly configured deployment of `apps/server` or
`apps/worker`.

## Out of scope

- GitHub's opinion of backdated commits (see the README's Caveats)
- misconfigured self-hosts: an operator who sets a weak
  `GITLARP_SCHEDULE_SECRET`, disables TLS, or fronts the server with a
  proxy that breaks the rate limiter's peer-address assumption
- the widget being pointed at an attacker-chosen API base — documented
  behavior: the widget sends the typed PAT to the API base it shows
  next to the input; embedding third-party widgets is a trust decision
- brute-forcing an operator's weak secret (documented: use
  `openssl rand -base64 32`, the AES key is a plain SHA-256 of the
  secret)
