#!/bin/sh
# Install anser and/or anser-server from GitHub Releases.
#
#   curl -fsSL https://raw.githubusercontent.com/lucianosrp/anser/main/install.sh | sh
#   curl -fsSL … | sh -s -- --server
#   FINANCE_VERSION=v0.1.0 sh install.sh --server
#
# One binary per machine. The desktop app (x86_64) includes the server
# (`anser server ...`); --server installs the small headless anser-server
# for a box without a screen (x86_64 or aarch64, the default on aarch64).
# INSTALL_DIR (default ~/.local/bin) is created; no sudo.
#
# Latest assets come from GitHub's /releases/latest/download/ CDN redirect
# (no api.github.com, no tag lookup). --version / FINANCE_VERSION pins a tag.
#
# FINANCE_RELEASE_BASE (testing): download SHA256SUMS and the tarballs from
# this URL or directory instead of GitHub (http://host:port, file:///path, or
# a local dir). Assets there are the version-less names (no tag in the file).
set -eu

REPO=lucianosrp/anser
GITHUB=https://github.com/$REPO
INSTALL_DIR=${INSTALL_DIR:-${HOME:?HOME is not set}/.local/bin}

die() { printf '%s\n' "$*" >&2; exit 1; }

usage() {
  cat <<'EOF'
Install Anser from GitHub Releases.

  curl -fsSL https://raw.githubusercontent.com/lucianosrp/anser/main/install.sh | sh
  curl -fsSL … | sh -s -- --server
  sh install.sh [--desktop|--server] [--version vX.Y.Z]

  --desktop          anser, the app with the server built in
                     (x86_64 linux; default on x86_64)
  --server           anser-server, the headless server alone, for a box
                     without a screen (x86_64 or aarch64; default on aarch64)
  --both             both files (rarely needed: anser already runs the server)
  --version vX.Y.Z   that release (else the latest stable)
  -h, --help         this text

  INSTALL_DIR            install here (default ~/.local/bin)
  FINANCE_VERSION        same as --version
  FINANCE_RELEASE_BASE   testing: download from this URL or directory
EOF
}

want_desktop=0
want_server=0
explicit=0
version=${FINANCE_VERSION:-}

while [ $# -gt 0 ]; do
  case "$1" in
    --desktop) want_desktop=1; explicit=1; shift ;;
    --server)  want_server=1;  explicit=1; shift ;;
    --both)    want_desktop=1; want_server=1; explicit=1; shift ;;
    --version)
      [ $# -ge 2 ] || die "--version needs a tag (vX.Y.Z)"
      version=$2
      shift 2
      ;;
    --version=*) version=${1#--version=}; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown option: $1" ;;
  esac
done

case "$version" in
  ''|v*) ;;
  *) version="v$version" ;;
esac

os=$(uname -s)
arch=$(uname -m)
case "$os" in
  Linux) ;;
  *) die "linux only (this machine is $os)" ;;
esac
case "$arch" in
  x86_64|amd64) arch=x86_64 ;;
  aarch64|arm64) arch=aarch64 ;;
  *) die "unsupported arch: $arch (need x86_64 or aarch64)" ;;
esac

# the desktop app where it is built (x86_64), else the headless server
if [ "$explicit" -eq 0 ]; then
  if [ "$arch" = x86_64 ]; then want_desktop=1; else want_server=1; fi
fi

if [ "$want_desktop" -eq 1 ] && [ "$arch" != x86_64 ]; then
  die "anser releases are x86_64 linux only (this machine is $arch). Use --server."
fi

command -v curl >/dev/null 2>&1 || die "need curl"
command -v tar >/dev/null 2>&1 || die "need tar"
command -v xz >/dev/null 2>&1 || die "need xz (install xz-utils)"
if command -v sha256sum >/dev/null 2>&1; then
  sha256_c() { sha256sum -c -; }
elif command -v shasum >/dev/null 2>&1; then
  sha256_c() { shasum -a 256 -c -; }
else
  die "need sha256sum or shasum"
fi

# --parallel is curl >= 7.66. `curl --version` is tiny; `curl --help all` is not.
HAVE_PARALLEL=0
IFS= read -r curl_ver <<EOF
$(curl --version)
EOF
case "$curl_ver" in
  curl\ 7.6[6-9]*|curl\ 7.[7-9]*|curl\ 7.[1-9][0-9]*|curl\ [8-9]*|curl\ [1-9][0-9]*)
    HAVE_PARALLEL=1 ;;
esac

if [ -n "${FINANCE_RELEASE_BASE:-}" ]; then
  dl_base=${FINANCE_RELEASE_BASE%/}
  case "$dl_base" in
    http://*|https://*|file://*) ;;
    *)
      [ -d "$dl_base" ] || die "FINANCE_RELEASE_BASE is not a directory: $dl_base"
      dl_base=file://$(CDPATH= cd "$dl_base" && pwd)
      ;;
  esac
elif [ -n "$version" ]; then
  dl_base="$GITHUB/releases/download/$version"
else
  dl_base="$GITHUB/releases/latest/download"
fi

mkdir -p "$INSTALL_DIR"
INSTALL_DIR=$(CDPATH= cd "$INSTALL_DIR" && pwd)

tmp=$(mktemp -d "$INSTALL_DIR/.finance-dl.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT HUP TERM

desktop_asset=anser-x86_64-linux.tar.xz
server_asset=anser-server-${arch}-linux.tar.xz

# "$@" is -o file url -o file url ...
fetch_all() {
  if [ "$HAVE_PARALLEL" -eq 1 ]; then
    curl -fsSL --parallel --fail-early --retry 2 -H "User-Agent: $REPO-install" "$@" \
      || die "download failed
If this is a new tag, wait for the Release workflow to attach the files."
    return
  fi
  while [ $# -ge 3 ]; do
    [ "$1" = -o ] || die "internal: expected -o"
    curl -fsSL --retry 2 -H "User-Agent: $REPO-install" -o "$2" "$3" \
      || die "download failed: $3
If this is a new tag, wait for the Release workflow to attach the files."
    shift 3
  done
}

verify_sha256() {
  f=$1
  line=$(awk -v f="$f" '$2 == f' SHA256SUMS)
  [ -n "$line" ] || die "SHA256SUMS has no entry for $f"
  printf '%s\n' "$line" | sha256_c >/dev/null \
    || die "sha256 mismatch for $f"
}

install_from_tar() {
  asset=$1
  name=$2
  tar -xJf "$asset" -C "$tmp" "$name" || die "tarball $asset has no $name"
  chmod 755 "$tmp/$name"
  mv -f "$tmp/$name" "$INSTALL_DIR/$name"
}

install_launcher() {
  bin=$1
  data=${XDG_DATA_HOME:-$HOME/.local/share}
  desktop_file=$data/applications/anser.desktop
  icon=$data/icons/hicolor/scalable/apps/anser.svg
  mkdir -p "$(dirname "$desktop_file")" "$(dirname "$icon")"
  # Exec= is quoted, with " ` $ \ escaped and % doubled (Desktop Entry spec)
  exec_path=$(printf '%s' "$bin" | sed 's/[\\"`$]/\\&/g; s/%/%%/g')
  cat > "$desktop_file" <<EOF
[Desktop Entry]
Type=Application
Name=Anser
GenericName=Personal finance
Comment=Net worth, spending and investments -- local-first
Exec="$exec_path"
Icon=anser
Terminal=false
Categories=Office;Finance;
Keywords=money;budget;net worth;spending;investments;
StartupWMClass=anser
EOF
  if [ ! -f "$icon" ]; then
    if "$bin" --icon "$icon" >/dev/null 2>&1; then
      command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -f -t "$data/icons/hicolor" 2>/dev/null || true
    else
      printf 'note: could not write the icon (%s --icon); the launcher still works\n' "$bin"
    fi
  fi
  command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$(dirname "$desktop_file")" || true
}

print_version() {
  bin=$1
  if out=$("$bin" --version 2>/dev/null) && [ -n "$out" ]; then
    printf '%s\n' "$out"
  fi
}

if [ -t 1 ]; then
  if [ "$want_desktop" -eq 1 ] && [ "$want_server" -eq 1 ]; then
    printf 'downloading anser and anser-server…\n'
  elif [ "$want_desktop" -eq 1 ]; then
    printf 'downloading anser…\n'
  else
    printf 'downloading anser-server…\n'
  fi
fi

set -- -o "$tmp/SHA256SUMS" "$dl_base/SHA256SUMS"
if [ "$want_desktop" -eq 1 ]; then
  set -- "$@" -o "$tmp/$desktop_asset" "$dl_base/$desktop_asset"
fi
if [ "$want_server" -eq 1 ]; then
  set -- "$@" -o "$tmp/$server_asset" "$dl_base/$server_asset"
fi
fetch_all "$@"

cd "$tmp"

if [ "$want_desktop" -eq 1 ]; then
  verify_sha256 "$desktop_asset"
  install_from_tar "$desktop_asset" anser
  install_launcher "$INSTALL_DIR/anser"
  print_version "$INSTALL_DIR/anser"
  cat <<EOF

Launch Anser from your launcher, or run: $INSTALL_DIR/anser
The server is built in: the app runs it for you. By hand:
  $INSTALL_DIR/anser server serve    http://127.0.0.1:8787
EOF
fi

if [ "$want_server" -eq 1 ]; then
  verify_sha256 "$server_asset"
  install_from_tar "$server_asset" anser-server
  print_version "$INSTALL_DIR/anser-server"
  cat <<EOF

Next steps for the server (this script does not start it). See DEPLOY.md.
Data lives in ~/.local/share/finance unless FINANCE_HOME says otherwise.
  $INSTALL_DIR/anser-server serve   http://127.0.0.1:8787
  from a checkout: systemd user unit   scripts/deploy.sh --install-units
  anser-server pair                 one-time code for a device
  tailscale serve                 publish 127.0.0.1:8787 on the tailnet
EOF
fi

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) printf 'note: %s is not on your PATH\n' "$INSTALL_DIR" ;;
esac
printf 'done\n'
