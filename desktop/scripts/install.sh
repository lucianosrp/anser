#!/usr/bin/env bash
# Install anser for this user so launchers (fuzzel, rofi, the dock)
# find it, with an icon in the current accent color.
#
#   desktop/scripts/install.sh              build (release) + install
#   desktop/scripts/install.sh --uninstall  remove what install put in place
#
# Installs (all per-user, nothing system-wide, no sudo):
#   ~/.local/bin/anser                           symlink to the release build,
#                                                          so a rebuild updates the app
#   ~/.local/share/applications/anser.desktop    launcher entry
#   ~/.local/share/icons/hicolor/scalable/apps/anser.svg
#                                                          icon; the app keeps it in its
#                                                          accent color from then on
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
bin="$HOME/.local/bin/anser"
desktop="${XDG_DATA_HOME:-$HOME/.local/share}/applications/anser.desktop"
icon="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/scalable/apps/anser.svg"

refresh() {
  command -v update-desktop-database >/dev/null && update-desktop-database -q "$(dirname "$desktop")" || true
  command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -f -t "$(dirname "$icon")/../../.." 2>/dev/null || true
}

if [[ "${1:-}" == "--uninstall" ]]; then
  rm -f "$bin" "$desktop" "$icon"
  refresh
  echo "removed anser (your cache and settings in ~/.local/share/finance-desktop and ~/.config/finance-desktop are kept)"
  exit 0
fi

echo "==> building (release)"
cargo build --release -p finance-desktop --manifest-path "$here/Cargo.toml"

echo "==> linking $bin"
mkdir -p "$(dirname "$bin")"
ln -sfn "$here/target/release/anser" "$bin"

echo "==> icon in the current accent"
"$bin" --icon "$icon"

echo "==> launcher entry"
mkdir -p "$(dirname "$desktop")"
cat > "$desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Anser
GenericName=Personal finance
Comment=Net worth, spending and investments -- local-first
Exec=$bin
Icon=anser
Terminal=false
Categories=Office;Finance;
Keywords=money;budget;net worth;spending;investments;
StartupWMClass=anser
DESKTOP

refresh
case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) echo "note: ~/.local/bin is not on your PATH (the launcher entry works anyway)";; esac
echo "done -- launch 'Anser' from your launcher, or run anser"
