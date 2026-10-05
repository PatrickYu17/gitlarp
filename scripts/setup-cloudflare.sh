#!/usr/bin/env bash
set -euo pipefail

# One-shot Cloudflare setup for the Rust schedule worker (apps/worker):
#   1. create the D1 database `gitlarp-schedules` (or reuse an existing one)
#   2. patch the real database_id into apps/worker/wrangler.jsonc
#   3. store GITLARP_SCHEDULE_SECRET as a worker secret
#
# Afterwards: cd apps/worker && wrangler deploy

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WRANGLER_JSONC="$ROOT/apps/worker/wrangler.jsonc"
PLACEHOLDER="00000000-0000-0000-0000-000000000000"
UUID_RE='[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}'

command -v wrangler >/dev/null 2>&1 ||
  { echo "FAIL: wrangler not found. Install it (e.g. 'bun install -g wrangler') or run via 'bunx wrangler'"; exit 1; }

[ -f "$WRANGLER_JSONC" ] ||
  { echo "FAIL: $WRANGLER_JSONC not found"; exit 1; }

echo "==> Step 1/3: creating D1 database 'gitlarp-schedules'"
DB_ID=""
if out=$(wrangler d1 create gitlarp-schedules 2>&1); then
  echo "$out"
  DB_ID=$(printf '%s' "$out" | grep -oE "$UUID_RE" | head -1 || true)
else
  echo "WARN: wrangler d1 create failed; database probably already exists:"
  echo "$out"
  echo "     looking up the existing database id instead..."
  DB_ID=$(wrangler d1 list 2>/dev/null | grep -F "gitlarp-schedules" \
    | grep -oE "$UUID_RE" | head -1 || true)
fi

if [ -z "$DB_ID" ]; then
  echo "FAIL: could not determine the D1 database id."
  echo "      Create the database manually and put its database_id in"
  echo "      apps/worker/wrangler.jsonc, then re-run this script."
  exit 1
fi
echo "    database_id: $DB_ID"

echo "==> Step 2/3: patching database_id into apps/worker/wrangler.jsonc"
if grep -q "$PLACEHOLDER" "$WRANGLER_JSONC"; then
  sed -i.bak "s/$PLACEHOLDER/$DB_ID/" "$WRANGLER_JSONC" && rm -f "$WRANGLER_JSONC.bak"
  echo "    patched: $(grep -F "$DB_ID" "$WRANGLER_JSONC" | head -1 | xargs)"
else
  echo "    no placeholder found; leaving existing database_id as-is"
fi

echo "==> Step 3/3: setting the worker secret (interactive; paste a secret)"
echo "    e.g.: $(openssl rand -base64 32 2>/dev/null || echo '<pick something random>')"
(
  cd "$ROOT/apps/worker"
  wrangler secret put GITLARP_SCHEDULE_SECRET
)

echo
echo "Done. Deploy the worker with:"
echo "  cd apps/worker && wrangler deploy"
