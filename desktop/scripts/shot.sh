#!/usr/bin/env bash
# Screenshot the running app's own window (Hyprland + grim + jq), not the whole screen.
#
#   scripts/shot.sh OUT.png [VAR=value ...]     e.g. FINANCE_DESKTOP_PAGE=banks
#
# Runs target/release/anser with the given env, waits for its window,
# captures exactly that window (found by PID via hyprctl, so parallel runs never
# capture each other), then stops it by PID. Build first:
#   cargo build -p finance-desktop --release
set -euo pipefail
out=$1; shift
here=$(cd "$(dirname "$0")/.." && pwd)
env "$@" "$here/target/release/anser" >"${out%.png}.log" 2>&1 &
pid=$!
trap 'kill $pid 2>/dev/null || true; wait $pid 2>/dev/null || true' EXIT
geom=""
for _ in $(seq 1 40); do
  sleep 0.25
  geom=$(hyprctl clients -j | jq -r --argjson pid "$pid" \
    '.[] | select(.pid == $pid and .mapped) | "\(.at[0]),\(.at[1]) \(.size[0])x\(.size[1])"' | head -1 || true)
  [[ -n "$geom" ]] && break
done
[[ -z "$geom" ]] && { echo "window never appeared; log:" >&2; tail -20 "${out%.png}.log" >&2; exit 1; }
sleep "${SHOT_DELAY:-2.5}"   # let data load and charts lay out
grim -g "$geom" "$out"
echo "$out ($geom)"
