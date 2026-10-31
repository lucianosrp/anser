#!/usr/bin/env bash
# Build the web app: the desktop app's pages compiled to wasm (desktop/web),
# plus its JS glue and index.html, into one flat directory the server serves
# at /app/ (and at / once installed).
#
#   desktop/scripts/build-web.sh [OUT]                 build into OUT (default desktop/target/webapp)
#   desktop/scripts/build-web.sh [OUT] --install URL   build, then install it (anser-server install-webapp; *.ts.net via MagicDNS)
#
# Needs (user toolchain, no system packages):
#   rustup target add wasm32-unknown-unknown
#   cargo install wasm-bindgen-cli --version 0.2.129   (must match Cargo.lock's wasm-bindgen)
#
# gpui's web backend pulls in wasm_thread, which uses one unstable feature
# even when the page runs single-threaded; RUSTC_BOOTSTRAP=wasm_thread lets
# stable rustc build that one crate and nothing else.
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
out="$here/target/webapp"
install_url=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --install) install_url="${2:?--install needs the server URL}"; shift 2 ;;
    -h|--help) sed -n '2,16p' "$0"; exit 0 ;;
    *) out="$1"; shift ;;
  esac
done

want=$(grep -A1 '^name = "wasm-bindgen"$' "$here/Cargo.lock" | sed -n 's/^version = "\(.*\)"/\1/p' | head -1)
have=$(wasm-bindgen --version 2>/dev/null | awk '{print $2}' || true)
if [[ -z "$have" ]]; then
  echo "wasm-bindgen not found: cargo install wasm-bindgen-cli --version $want" >&2; exit 1
fi
if [[ "$have" != "$want" ]]; then
  echo "wasm-bindgen $have, but Cargo.lock has $want: cargo install wasm-bindgen-cli --version $want" >&2; exit 1
fi
rustup target list --installed 2>/dev/null | grep -qx wasm32-unknown-unknown \
  || { echo "missing target: rustup target add wasm32-unknown-unknown" >&2; exit 1; }

# FINANCE_WEB_OPT: the wasm's opt-level (default s; 3 trades size for speed)
( cd "$here" && RUSTC_BOOTSTRAP=wasm_thread CARGO_PROFILE_RELEASE_OPT_LEVEL="${FINANCE_WEB_OPT:-s}" \
    cargo build -p finance-web --target wasm32-unknown-unknown --release --locked )

rm -rf "$out"
mkdir -p "$out"
wasm-bindgen --target web --no-typescript --out-dir "$out" \
  "$here/target/wasm32-unknown-unknown/release/finance_web.wasm"
# the installable-app files: manifest, icons (rendered once from icon.svg
# with rsvg-convert and committed), and the service worker, whose cache is
# named after this build so a phone picks up a new one (web/sw.js)
cp "$here/web/index.html" "$here/web/manifest.webmanifest" "$here/web/icon.svg" "$here"/web/icons/*.png "$out/"
build=$(cat "$out/finance_web_bg.wasm" "$out/finance_web.js" "$out/index.html" "$out/manifest.webmanifest" "$out/icon.svg" | sha256sum | cut -c1-12)
sed "s/__BUILD__/$build/" "$here/web/sw.js" > "$out/sw.js"
# the server sends the .gz to clients that accept gzip (all browsers)
gzip -9 -k "$out/finance_web_bg.wasm"
gzip -9 -k "$out/finance_web.js"

wasm=$(stat -c %s "$out/finance_web_bg.wasm")
wgz=$(stat -c %s "$out/finance_web_bg.wasm.gz")
total=$(du -sb "$out" | cut -f1)
mb() { awk -v b="$1" 'BEGIN { printf "%.1f", b / 1048576 }'; }
echo "web app in $out (build $build)"
echo "  wasm $(mb "$wasm") MB ($(mb "$wgz") MB gzipped, what a browser downloads)"
echo "  bundle $(mb "$total") MB on disk"

if [[ -n "$install_url" ]]; then
  # anser-server, not curl: it resolves *.ts.net through MagicDNS when a VPN
  # (NordVPN) answers the system DNS with tailscale's public addresses
  ( cd "$here" && cargo run -q --release -p finance-server --bin anser-server -- \
      install-webapp --dir "$out" --server "${install_url%/}" )
fi
