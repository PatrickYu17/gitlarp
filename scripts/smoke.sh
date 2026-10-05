#!/usr/bin/env bash
set -u
cd "$(dirname "$0")/.."

: "${SMOKE_PAT:?SMOKE_PAT is required (GitHub PAT with repo scope)}"
: "${SMOKE_LOGIN:?SMOKE_LOGIN is required (GitHub username)}"
WORKER_URL=${SMOKE_WORKER_URL:-}

BIN=target/release/gitlarp
if [ ! -x "$BIN" ]; then
  echo "FAIL: $BIN missing. Run: cargo build --release (in apps/cli)"
  exit 1
fi

export GITHUB_TOKEN=$SMOKE_PAT
command -v gh >/dev/null 2>&1 || { echo "FAIL: gh CLI is required"; exit 1; }
gh auth setup-git >/dev/null 2>&1 || true

REPO=gitlarp-smoke
cleanup() { gh repo delete "$SMOKE_LOGIN/$REPO" --yes >/dev/null 2>&1 || true; }
trap cleanup EXIT

cleanup
gh repo create "$REPO" --private >/dev/null 2>&1 ||
  { echo "FAIL: could not create github.com/$SMOKE_LOGIN/$REPO"; exit 1; }

printf '%-22s %-5s %s\n' probe status note
printf '%-22s %-5s %s\n' ----- ----- ----
FAILS=0
record() {
  status=PASS
  [ "$2" = 0 ] || { status=FAIL; FAILS=$((FAILS + 1)); }
  printf '%-22s %-5s %s\n' "$1" "$status" "$3"
}

export GITLARP_HOME=$(mktemp -d)
export GITLARP_REPO=$REPO

if [ "$(uname -s)" = Darwin ]; then
  d0=$(date -v-3d +%F)
  d1=$(date -v-2d +%F)
  d2=$(date -v-1d +%F)
else
  d0=$(date -d '-3 days' +%F)
  d1=$(date -d '-2 days' +%F)
  d2=$(date -d '-1 day' +%F)
fi

if out=$("$BIN" init 2>&1) && [ -f "$GITLARP_HOME/config.toml" ]; then
  record "cli init" 0 ""
else
  record "cli init" 1 "$out"
fi

if out=$("$BIN" day "$d0" 2 2>&1); then
  record "cli day" 0 ""
else
  record "cli day" 1 "$out"
fi

if out=$("$BIN" fill --from "$d1" --to "$d2" --min 1 --max 2 2>&1); then
  record "cli fill" 0 ""
else
  record "cli fill" 1 "$out"
fi

AUTH="Authorization: Bearer $SMOKE_PAT"
BASE="https://api.github.com/repos/$SMOKE_LOGIN/$REPO"

if curl -sf -H "$AUTH" "$BASE/branches/main" >/dev/null; then
  record "branch main exists" 0 ""
else
  record "branch main exists" 1 "branch main not found on $SMOKE_LOGIN/$REPO"
fi

commits=$(curl -sf -H "$AUTH" "$BASE/commits?per_page=100" || true)
count=$(printf '%s' "$commits" | grep -o '"node_id"' | wc -l | tr -d ' ')
if [ "${count:-0}" -ge 3 ] 2>/dev/null; then
  record "commit count >= 3" 0 "$count commits"
else
  record "commit count >= 3" 1 "$count commits"
fi

if printf '%s' "$commits" | grep -Eq '"date": ?"'$d0; then
  record "commit dated $d0" 0 ""
else
  record "commit dated $d0" 1 "no commit with date starting $d0"
fi

if out=$("$BIN" wipe 2>&1); then
  code=$(curl -s -o /dev/null -w '%{http_code}' -H "$AUTH" "$BASE/branches/main")
  if [ "$code" = 404 ]; then
    record "wipe removes branch" 0 ""
  else
    record "wipe removes branch" 1 "branches/main -> $code (want 404)"
  fi
else
  record "wipe removes branch" 1 "$out"
fi

if [ -n "$WORKER_URL" ]; then
  resp=$(curl -s -w '\n%{http_code}' -X POST -H 'Content-Type: application/json' \
    -d "{\"pat\":\"$SMOKE_PAT\",\"days\":[{\"date\":\"$d2\",\"count\":2}]}" \
    "$WORKER_URL/api/commits")
  code=$(printf '%s' "$resp" | tail -1)
  body=$(printf '%s' "$resp" | head -n -1)
  created=$(printf '%s' "$body" | grep -o '"created":[0-9]*' | cut -d: -f2)
  if [ "$code" = 200 ] && [ "$created" = 2 ]; then
    record "web POST -> 200" 0 "created=$created"
  else
    record "web POST -> 200" 1 "code=$code body=$body"
  fi

  code=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'Content-Type: application/json' \
    -d "{\"pat\":\"not-a-real-pat\",\"days\":[{\"date\":\"$d2\",\"count\":1}]}" \
    "$WORKER_URL/api/commits")
  if [ "$code" = 401 ]; then
    record "web bad PAT -> 401" 0 ""
  else
    record "web bad PAT -> 401" 1 "got $code"
  fi

  code=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'Content-Type: application/json' \
    -d '{"pat":"not-a-real-pat","days":[{"date":"2026/08/20","count":1}]}' \
    "$WORKER_URL/api/commits")
  if [ "$code" = 400 ]; then
    record "web bad date -> 400" 0 ""
  else
    record "web bad date -> 400" 1 "got $code"
  fi
fi

echo
if [ "$FAILS" -eq 0 ]; then
  echo "PASS: all probes green"
else
  echo "FAIL: $FAILS probe(s) red"
fi
echo "graph check: open github.com/$SMOKE_LOGIN?tab=contributions: commits appear within minutes (private contributions must be on)"
exit "$FAILS"