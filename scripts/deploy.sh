#!/usr/bin/env bash
# Ship committed main to your server, install anser-server, restart it.
# anser-server serves 127.0.0.1:8787 behind tailscale serve (DEPLOY.md).
#
#   scripts/deploy.sh                  # bundle -> server, ff-merge, install, backup, restart, health-check
#                                      # install from the GitHub release when HEAD is a v* tag, else cargo build
#   scripts/deploy.sh --release vX.Y.Z # take anser-server and the web app from that release
#   scripts/deploy.sh --build          # force cargo build on the server (even if HEAD is tagged)
#   scripts/deploy.sh --install-units  # same, and (re)install the systemd user units first
#   scripts/deploy.sh --restart        # restart only (e.g. after rotating a token in .env)
#   FINANCE_HOST=my-server scripts/deploy.sh   # one-off target, overrides deploy.local
#
# Where: FINANCE_HOST, ssh destinations separated by spaces (the first that
# answers wins, e.g. a LAN alias then a Tailscale one). Unset, it comes from
# scripts/deploy.local (gitignored; copy scripts/deploy.local.example), which
# may also set REMOTE_DIR, PORT and BRANCH.
#
# Why a bundle: the server has no key for the git remote, so commits travel as a
# git bundle over scp and are fast-forwarded in place (never a merge commit;
# a diverged server checkout aborts instead of being clobbered). Only COMMITTED
# work ships -- uncommitted changes are reported and left behind. Tags merged
# into the branch go in the bundle too, so the server's checkout sees v*.
#
# A v* tag on HEAD (or --release) downloads anser-server and the web app from
# GitHub Releases on the server (sha256-verified), so a Pi does not spend
# 20+ min compiling. --build keeps today's cargo path. KEEP_TARGET=1 skips
# the cargo clean after a build (small disks can't spare target/ otherwise).
#
# FINANCE_RELEASE_BASE (testing): download SHA256SUMS and the version-less
# tarballs from this URL instead of GitHub; the server must be able to fetch it.
set -euo pipefail

BRANCH=main
REMOTE_DIR='~/finance'
PORT=8787
GITHUB_RELEASES='https://github.com/lucianosrp/anser/releases/download'
install_units=0
restart_only=0
force_build=0
release_tag=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --install-units) install_units=1; shift ;;
    --restart) restart_only=1; shift ;;
    --build) force_build=1; shift ;;
    --release)
      release_tag="${2:?--release needs a tag (vX.Y.Z)}"
      shift 2
      ;;
    --release=*) release_tag="${1#--release=}"; shift ;;
    -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done
if [[ $force_build -eq 1 && -n "$release_tag" ]]; then
  echo "use --build or --release, not both" >&2; exit 2
fi
if [[ -n "$release_tag" ]]; then
  case "$release_tag" in
    v[0-9]*) ;;
    *) echo "--release tag must look like v0.1.0 (got $release_tag)" >&2; exit 2 ;;
  esac
fi

cd "$(dirname "$0")/.."

# the environment's FINANCE_HOST wins over deploy.local's
env_host=${FINANCE_HOST:-}
if [[ -f scripts/deploy.local ]]; then source scripts/deploy.local; fi
if [[ -n "$env_host" ]]; then FINANCE_HOST=$env_host; fi

# the first candidate that answers over ssh (a lone one is used as is)
pick_host() {
  local hosts
  read -ra hosts <<< "${FINANCE_HOST:-}"
  if [[ ${#hosts[@]} -eq 0 ]]; then
    cat >&2 <<'EOF'
no deploy target: set FINANCE_HOST to your server's ssh destination(s), e.g.
  FINANCE_HOST=my-server scripts/deploy.sh
or put it in scripts/deploy.local (gitignored):
  cp scripts/deploy.local.example scripts/deploy.local
EOF
    exit 1
  fi
  if [[ ${#hosts[@]} -eq 1 ]]; then echo "${hosts[0]}"; return; fi
  for h in "${hosts[@]}"; do
    if ssh -o ConnectTimeout=5 -o BatchMode=yes "$h" true 2>/dev/null; then echo "$h"; return; fi
  done
  echo "no route to the server (tried ${hosts[*]})" >&2; exit 1
}
HOST=$(pick_host)
echo "==> server reachable as '$HOST'"

release_url() {
  local file=$1
  if [[ -n "${FINANCE_RELEASE_BASE:-}" ]]; then
    printf '%s/%s\n' "${FINANCE_RELEASE_BASE%/}" "$file"
  else
    printf '%s/%s/%s\n' "$GITHUB_RELEASES" "$release_tag" "$file"
  fi
}

norm_arch() {
  case "$1" in
    x86_64|amd64) echo x86_64 ;;
    aarch64|arm64) echo aarch64 ;;
    *) echo "unsupported server arch: $1 (need x86_64 or aarch64)" >&2; exit 1 ;;
  esac
}

# Download anser-server and the web app on the server, verify, install atomically.
install_from_release() {
  local arch rs_asset web_asset rs_url web_url sums_url
  arch=$(norm_arch "$(ssh "$HOST" 'uname -m')")
  rs_asset="anser-server-${arch}-linux.tar.xz"
  web_asset="anser-web.tar.xz"
  rs_url=$(release_url "$rs_asset")
  web_url=$(release_url "$web_asset")
  sums_url=$(release_url SHA256SUMS)
  echo "==> installing $release_tag from the release ($arch linux)"
  # stdin is the remote script; args are the URLs so a local FINANCE_RELEASE_BASE works
  ssh -T "$HOST" bash -s -- "$rs_url" "$web_url" "$sums_url" "$rs_asset" "$web_asset" "$REMOTE_DIR" "$release_tag" <<'EOS'
set -euo pipefail
rs_url=$1
web_url=$2
sums_url=$3
rs_asset=$4
web_asset=$5
remote_dir=$6
tag=$7
remote_dir="${remote_dir/#\~/$HOME}"

missing() {
  echo "release $tag is not available yet ($1)." >&2
  echo "Wait for the Release workflow to finish, or deploy with --build." >&2
  exit 1
}

command -v xz >/dev/null 2>&1 || { echo "need xz (install xz-utils)" >&2; exit 1; }

to_url() {
  case "$1" in
    http://*|https://*|file://*) printf '%s\n' "$1" ;;
    /*) printf 'file://%s\n' "$1" ;;
    *) printf '%s\n' "$1" ;;
  esac
}

fetch() {
  local dest=$1 url=$2
  echo "==> downloading $url"
  case "$url" in
    http://*|https://*|file://*)
      curl -fsSL --retry 2 -o "$dest" "$url" || missing "$url"
      ;;
    *)
      cp "$url" "$dest" || missing "$url"
      ;;
  esac
}

verify() {
  local f=$1 line
  line=$(grep "  ${f}$" SHA256SUMS) || { echo "SHA256SUMS has no entry for $f" >&2; exit 1; }
  printf '%s\n' "$line" | sha256sum -c -
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"

rs_url=$(to_url "$rs_url")
web_url=$(to_url "$web_url")
sums_url=$(to_url "$sums_url")

have_parallel=0
IFS= read -r curl_ver <<EOF
$(curl --version)
EOF
case "$curl_ver" in
  curl\ 7.6[6-9]*|curl\ 7.[7-9]*|curl\ 7.[1-9][0-9]*|curl\ [8-9]*|curl\ [1-9][0-9]*)
    have_parallel=1 ;;
esac
if [[ $have_parallel -eq 1 ]]; then
  echo "==> downloading $rs_asset, $web_asset, SHA256SUMS"
  curl -fsSL --parallel --fail-early --retry 2 \
    -o SHA256SUMS "$sums_url" \
    -o "$rs_asset" "$rs_url" \
    -o "$web_asset" "$web_url" || missing "release assets"
else
  fetch SHA256SUMS "$sums_url"
  fetch "$rs_asset" "$rs_url"
  fetch "$web_asset" "$web_url"
fi
verify "$rs_asset"
verify "$web_asset"

mkdir -p "$HOME/.local/bin"
stage=$(mktemp -d "$HOME/.local/bin/.anser-server.XXXXXX")
tar -xJf "$rs_asset" -C "$stage" anser-server
chmod 755 "$stage/anser-server"
mv -f "$stage/anser-server" "$HOME/.local/bin/anser-server"
rmdir "$stage"

web="$remote_dir/data/webapp"
new="$remote_dir/data/webapp.new"
old="$remote_dir/data/webapp.old"
rm -rf "$new" "$old"
mkdir -p "$new" "$remote_dir/data"
tar -xJf "$web_asset" -C "$new"
[[ -f "$new/index.html" ]] || { echo "web tarball has no index.html" >&2; exit 1; }
if [[ -e "$web" ]]; then mv "$web" "$old"; fi
mv "$new" "$web"
rm -rf "$old"

echo "installed $(~/.local/bin/anser-server --version 2>/dev/null || echo anser-server) and web app in $web"
EOS
}

build_on_server() {
  echo "==> building anser-server on the server (cargo build --release -p finance-server)"
  # a login shell so ~/.cargo/env puts cargo on PATH
  ssh "$HOST" "bash -lc 'cd $REMOTE_DIR/desktop && cargo build --release -p finance-server'"
  # install atomically (the unit may be running the old one), then free the build dir
  ssh "$HOST" "mkdir -p ~/.local/bin && install -m 755 $REMOTE_DIR/desktop/target/release/anser-server ~/.local/bin/anser-server.new && mv -f ~/.local/bin/anser-server.new ~/.local/bin/anser-server"
  if [[ -z "${KEEP_TARGET:-}" ]]; then
    echo "==> cargo clean on the server"
    ssh "$HOST" "bash -lc 'cd $REMOTE_DIR/desktop && cargo clean -q' && df -h / | tail -1"
  fi
}

if [[ $restart_only -eq 0 ]]; then
  [[ "$(git rev-parse --abbrev-ref HEAD)" == "$BRANCH" ]] || { echo "not on $BRANCH" >&2; exit 1; }
  if ! git diff --quiet HEAD -- ; then
    echo "note: uncommitted changes to tracked files are NOT shipped:"; git status --short --untracked-files=no
  fi
  if [[ $force_build -eq 0 && -z "$release_tag" ]]; then
    release_tag=$(git tag --points-at HEAD --list 'v*' --sort=-v:refname | head -1 || true)
  fi
  local_sha=$(git rev-parse HEAD)
  remote_sha=$(ssh "$HOST" "cd $REMOTE_DIR && git rev-parse HEAD")
  if [[ "$local_sha" == "$remote_sha" ]]; then
    echo "==> server already at ${local_sha:0:7}"
  else
    echo "==> shipping ${remote_sha:0:7}..${local_sha:0:7}:"; git log --oneline "$remote_sha..$local_sha" 2>/dev/null || true
    bundle=$(mktemp --suffix=.bundle); trap 'rm -f "$bundle"' EXIT
    extra_tags=()
    while IFS= read -r t; do
      [[ -n "$t" ]] && extra_tags+=("$t")
    done < <(git tag --merged "$BRANCH")
    if [[ ${#extra_tags[@]} -gt 0 ]]; then
      git bundle create -q "$bundle" "$BRANCH" "${extra_tags[@]}"
    else
      git bundle create -q "$bundle" "$BRANCH"
    fi
    rbundle=$(ssh "$HOST" 'mktemp --suffix=.bundle')   # a private path, not a fixed /tmp name
    scp -q "$bundle" "$HOST:$rbundle"
    ssh "$HOST" "trap 'rm -f $rbundle' EXIT; cd $REMOTE_DIR && git fetch -q $rbundle $BRANCH && git merge --ff-only -q FETCH_HEAD && { git fetch -q $rbundle 'refs/tags/*:refs/tags/*' || true; } && git log --oneline -1"
  fi

  if [[ $force_build -eq 1 ]]; then
    build_on_server
  elif [[ -n "$release_tag" ]]; then
    install_from_release
  else
    build_on_server
  fi
fi

# One-time move from the pre-rename units (finance-rs, finance-backup): a
# server that still has them gets the anser-* units, keeps the backup timer
# it had, and loses the old units and binary. The data tree is untouched.
old_units=0
if ssh "$HOST" "test -f ~/.config/systemd/user/finance-rs.service"; then
  old_units=1
  install_units=1
fi

if [[ $install_units -eq 1 ]]; then
  echo "==> installing the anser-server systemd user unit"
  ssh "$HOST" "mkdir -p ~/.config/systemd/user"
  scp -q deploy/anser-server.service deploy/anser-backup.service deploy/anser-backup.timer "$HOST:~/.config/systemd/user/"
  if [[ $old_units -eq 1 ]]; then
    echo "==> retiring the old finance-rs units"
    ssh "$HOST" "timer=0; systemctl --user is-enabled -q finance-backup.timer 2>/dev/null && timer=1; \
      systemctl --user disable --now -q finance-rs.service finance-backup.timer 2>/dev/null || true; \
      rm -f ~/.config/systemd/user/finance-rs.service ~/.config/systemd/user/finance-backup.service \
        ~/.config/systemd/user/finance-backup.timer ~/.local/bin/finance-rs; \
      systemctl --user daemon-reload; \
      if [ \$timer = 1 ]; then systemctl --user enable --now -q anser-backup.timer; fi"
  fi
  # retire a hand-launched `anser-server serve` so the unit can bind :$PORT.
  # Pattern 'bin/[a]nser-server serve': the bracket still matches the
  # server but not this ssh command line itself -- a plain 'anser-server serve'
  # appears verbatim in it, and pkill would kill its own shell (DEPLOY.md).
  ssh "$HOST" "systemctl --user daemon-reload \
    && if ! systemctl --user is-active -q anser-server; then pkill -f 'bin/[a]nser-server serve' || true; fi \
    && systemctl --user enable -q anser-server.service"
fi

echo "==> backing up the db before restart"
ssh "$HOST" "cd $REMOTE_DIR && FINANCE_HOME=\$PWD ~/.local/bin/anser-server backup"

echo "==> restarting anser-server"
ssh "$HOST" "systemctl --user restart anser-server"

echo -n "==> waiting for /healthz on :$PORT "
for _ in $(seq 1 30); do
  if ssh "$HOST" "curl -fs -m 2 http://127.0.0.1:$PORT/healthz" >/dev/null 2>&1; then echo " ok"; exit 0; fi
  echo -n "."; sleep 1
done
echo " FAILED"
ssh "$HOST" "journalctl --user -u anser-server -n 30 --no-pager"
exit 1
