#!/usr/bin/env bash
set -u
cd "$(dirname "$0")/.."

fail=0

printf '%-14s %-12s %-12s %s\n' gate measured limit status
printf '%-14s %-12s %-12s %s\n' ---- -------- ----- ------

row() {
  gate=$1; measured=$2; limit=$3; status=$4
  [ "$status" = FAIL ] && fail=1
  printf '%-14s %-12s %-12s %s\n' "$gate" "$measured" "$limit" "$status"
}

BIN=target/release/gitlarp
# The same source measures ~17% larger as a Linux ELF than a macOS
# Mach-O, so the size budget is per-OS (calibrated: darwin ~455KB,
# linux ~535KB; keep headroom for toolchain drift).
cli_limit=512000
[ "$(uname -s)" = "Linux" ] && cli_limit=560000
if [ -x "$BIN" ]; then
  size=$(wc -c < "$BIN" | tr -d ' ')
  if awk -v s="$size" -v l="$cli_limit" 'BEGIN { exit !(s < l) }'; then
    row cli-binary "${size}B" "${cli_limit}B" PASS
  else
    row cli-binary "${size}B" "${cli_limit}B" FAIL
  fi
else
  row cli-binary missing "${cli_limit}B" FAIL
  echo "hint: cargo build --release (in apps/cli)"
fi

if [ -x "$BIN" ]; then
  "$BIN" --help >/dev/null 2>&1
  best=""
  for _ in 1 2 3; do
    if [ -x /usr/bin/time ]; then
      t=$(/usr/bin/time -p "$BIN" --help 2>&1 >/dev/null | awk '/^real/ { print $2; exit }')
    else
      TIMEFORMAT='%R'
      t=$({ time "$BIN" --help >/dev/null; } 2>&1)
    fi
    best=$(awk -v b="${best:-999}" -v t="$t" 'BEGIN { print (t+0 < b+0) ? t : b }')
  done
  if awk -v t="$best" 'BEGIN { exit !(t+0 < 0.005) }'; then
    row cli-startup "${best}s" "0.005s" PASS
  else
    row cli-startup "${best}s" "0.005s" FAIL
  fi
else
  row cli-startup missing "0.005s" FAIL
  echo "hint: cargo build --release (in apps/cli)"
fi

JS=apps/web/.next/static
MANIFEST=apps/web/.next/server/app/page_client-reference-manifest.js
if [ -d "$JS" ] && [ -f "$MANIFEST" ]; then
  entries=$(grep -o 'static/chunks/[^" ]*\.js' "$MANIFEST" | sort -u)
  if [ -n "$entries" ]; then
    total=0
    for entry in $entries; do
      size=$(gzip -c "apps/web/.next/$entry" | wc -c | tr -d ' ')
      total=$((total + size))
    done
    if awk -v t="$total" 'BEGIN { exit !(t < 102400) }'; then
      row web-js "${total}B" "102400B" PASS
    else
      row web-js "${total}B" "102400B" FAIL
    fi
  else
    row web-js "no-entry-chunks" "102400B" FAIL
    echo "hint: bun run build (in apps/web)"
  fi
else
  row web-js missing "102400B" FAIL
  echo "hint: bun run build (in apps/web)"
fi

W=apps/web/.open-next/worker.js
if [ -f "$W" ]; then
  wsize=$(wc -c < "$W" | tr -d ' ')
  if awk -v s="$wsize" 'BEGIN { exit !(s < 1048576) }'; then
    row worker-bundle "${wsize}B" "1048576B" PASS
  else
    row worker-bundle "${wsize}B" "1048576B" FAIL
  fi
else
  row worker-bundle missing "1048576B" FAIL
  echo "hint: bunx opennextjs-cloudflare build (in apps/web)"
fi

echo
if [ "$fail" -eq 0 ]; then
  echo "PASS: all gates green"
  exit 0
else
  echo "FAIL: one or more gates red"
  exit 1
fi
