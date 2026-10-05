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
# Ports 3000/8080 must be free on the host.
set -u
cd "$(dirname "$0")/.."

command -v docker >/dev/null 2>&1 || { echo "FAIL: docker is required"; exit 1; }
docker compose version >/dev/null 2>&1 || { echo "FAIL: docker compose (v2) is required"; exit 1; }
docker info >/dev/null 2>&1 || { echo "FAIL: docker daemon is not running"; exit 1; }

cleanup() { docker compose down -v --remove-orphans >/dev/null 2>&1 || true; }
trap cleanup EXIT

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
if docker compose build --quiet; then
  record build 0 "server + web images built"
else
  record build 1 "docker compose build failed"
  echo; echo "FAIL: build red"; exit 1
fi

if docker compose up -d --wait; then
  record up 0 "stack is up"
else
  record up 1 "docker compose up --wait failed"
  docker compose logs --tail=20
  echo; echo "FAIL: stack did not come up"; exit 1
fi

body=$(curl -fsS --max-time 10 http://localhost:8080/healthz || true)
[ "$body" = '{"ok":true}' ]
record server-healthz $? "${body:-no response}"

code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 http://localhost:3000/ || echo 000)
[ "$code" = 200 ]
record web-ui $? "GET / -> HTTP $code (bun image serves the Next app)"

# a junk PAT must come back as GitHub's own 401: proves the API path
# (routing, error mapping, upstream passthrough) is alive end to end
http=$(curl -s -o /dev/null -w '%{http_code}' --max-time 15 \
  -H 'Authorization: Bearer junk' http://localhost:8080/api/graph || echo 000)
[ "$http" = 401 ]
record api-graph-401 $? "junk PAT -> HTTP $http"

http=$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 \
  http://localhost:8080/api/schedules || echo 000)
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
