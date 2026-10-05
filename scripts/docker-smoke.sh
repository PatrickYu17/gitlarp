#!/usr/bin/env bash
# One-shot compose smoke test: builds both images, boots the stack,
# probes the server API and the web UI, then tears everything down.
#
# The web runtime image (oven/bun) runs `next start`, a Node-coupled
# entry point — the exact combination that breaks most quietly. Run
# this before publishing a release (needs a Docker daemon):
#
#   bash scripts/docker-smoke.sh
#
# The smoke stack is isolated from any live deployment on this host:
# it runs as its own compose project (gitlarp-smoke) on remapped host
# ports 39080/39000, so it neither fights a live stack for the
# published ports 8080/3000 nor tears it down with its exit trap
# (`down -v` on the shared project would delete its gitlarp-data
# volume — the stored encrypted-PAT schedules).
set -u
cd "$(dirname "$0")/.."

command -v docker >/dev/null 2>&1 || { echo "FAIL: docker is required"; exit 1; }
docker info >/dev/null 2>&1 || { echo "FAIL: docker daemon is not running"; exit 1; }

PROJECT=gitlarp-smoke
SRV_PORT=39080
WEB_PORT=39000

# docker-compose.yml hardcodes the published ports and the web build
# arg, so remap them with a generated override. `!override` replaces
# each port list outright (a plain override only appends, leaving
# 8080/3000 published too — a collision with a live stack), and the
# build arg must follow the remap: it is inlined into the browser
# bundle, so it has to point at the smoke stack's published server
# port, not the default 8080.
OVERRIDE_DIR=$(mktemp -d) || { echo "FAIL: mktemp failed"; exit 1; }
OVERRIDE="$OVERRIDE_DIR/smoke-override.yml"
cat >"$OVERRIDE" <<EOF
services:
  server:
    ports: !override
      - "$SRV_PORT:8080"
  web:
    ports: !override
      - "$WEB_PORT:3000"
    build:
      args:
        NEXT_PUBLIC_GITLARP_API_URL: http://localhost:$SRV_PORT
EOF

# every compose call carries the isolated project + the override
compose() { docker compose -f docker-compose.yml -f "$OVERRIDE" -p "$PROJECT" "$@"; }

cleanup() {
  compose down -v --remove-orphans >/dev/null 2>&1 || true
  rm -rf "$OVERRIDE_DIR"
}
trap cleanup EXIT

compose version >/dev/null 2>&1 || { echo "FAIL: docker compose (v2) is required"; exit 1; }
# resolves both files up front: fails fast here (rather than minutes
# into the build) on a compose too old for the `!override` tag
compose config >/dev/null 2>&1 || { echo "FAIL: compose stack did not resolve (needs docker compose >= 2.24 for !override)"; exit 1; }

printf '%-16s %-6s %s\n' probe status note
printf '%-16s %-6s %s\n' ----- ----- ----
FAILS=0
record() {
  status=PASS
  [ "$2" = 0 ] || { status=FAIL; FAILS=$((FAILS + 1)); }
  printf '%-16s %-6s %s\n' "$1" "$status" "$3"
}

# the server refuses to boot with a short secret; ship a real one
export GITLARP_SCHEDULE_SECRET=${GITLARP_SCHEDULE_SECRET:-$(openssl rand -base64 32)}

echo "building images (this can take a few minutes)..."
if compose build --quiet; then
  record build 0 "server + web images built"
else
  record build 1 "docker compose build failed"
  echo; echo "FAIL: build red"; exit 1
fi

if compose up -d --wait; then
  record up 0 "stack is up"
else
  record up 1 "docker compose up --wait failed"
  compose logs --tail=20
  echo; echo "FAIL: stack did not come up"; exit 1
fi

body=$(curl -fsS --max-time 10 http://localhost:$SRV_PORT/healthz || true)
[ "$body" = '{"ok":true}' ]
record server-healthz $? "${body:-no response}"

code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 http://localhost:$WEB_PORT/ || echo 000)
[ "$code" = 200 ]
record web-ui $? "GET / -> HTTP $code (bun image serves the Next app)"

# a junk PAT must come back as GitHub's own 401: proves the API path
# (routing, error mapping, upstream passthrough) is alive end to end
http=$(curl -s -o /dev/null -w '%{http_code}' --max-time 15 \
  -H 'Authorization: Bearer junk' http://localhost:$SRV_PORT/api/graph || echo 000)
[ "$http" = 401 ]
record api-graph-401 $? "junk PAT -> HTTP $http"

http=$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 \
  http://localhost:$SRV_PORT/api/schedules || echo 000)
[ "$http" = 400 ]
record api-guard $? "no PAT -> HTTP $http (input validation live)"

echo
if [ "$FAILS" -eq 0 ]; then
  echo "PASS: docker compose stack is healthy"
  exit 0
else
  echo "FAIL: $FAILS probe(s) red"
  exit 1
fi
