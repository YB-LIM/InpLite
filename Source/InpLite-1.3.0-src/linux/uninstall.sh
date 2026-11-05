#!/bin/sh
# Remove InpLite installed with install.sh (same --prefix if one was used).
PREFIX="$HOME/.local"
[ "$1" = "--prefix" ] && PREFIX="$2"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
rm -f "$PREFIX/bin/inplite" "$DATA/applications/inplite.desktop" "$DATA/icons/hicolor/256x256/apps/inplite.png" "$DATA/mime/packages/inplite.xml"
DESK_DIR=$(xdg-user-dir DESKTOP 2>/dev/null || echo "$HOME/Desktop")
rm -f "$DESK_DIR/inplite.desktop"
command -v update-mime-database >/dev/null 2>&1 && update-mime-database "$DATA/mime" >/dev/null 2>&1
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$DATA/applications" >/dev/null 2>&1
for RC in "$HOME/.bashrc" "$HOME/.zshrc" "$HOME/.cshrc"; do
  [ -f "$RC" ] && sed -i '/# InpLite$/d' "$RC"
done
echo "InpLite removed (settings are kept in ~/.config/InpLite)."
