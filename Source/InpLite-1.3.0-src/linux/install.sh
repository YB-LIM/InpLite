#!/bin/sh
# InpLite — install for the current user (no root / sudo needed).
#   ./install.sh                 install into ~/.local (menu entry, desktop icon, PATH)
#   ./install.sh --prefix DIR    install into DIR/bin (e.g. a shared group folder)
#   ./install.sh --no-desktop    no desktop icon
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
PREFIX="$HOME/.local"
DESKTOP_ICON=1
while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) PREFIX="$2"; shift 2 ;;
    --no-desktop) DESKTOP_ICON=0; shift ;;
    -h|--help) sed -n '2,6p' "$0"; exit 0 ;;
    *) echo "unknown option: $1"; exit 1 ;;
  esac
done
BIN="$PREFIX/bin"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
mkdir -p "$BIN" "$DATA/applications" "$DATA/icons/hicolor/256x256/apps" "$DATA/mime/packages"

cp "$HERE/inplite" "$BIN/inplite"
chmod 755 "$BIN/inplite"
cp "$HERE/inplite.png" "$DATA/icons/hicolor/256x256/apps/inplite.png"

# file types for .inp and .sta, so file managers offer "Open with InpLite" (the default program is not changed)
cat > "$DATA/mime/packages/inplite.xml" <<'XML'
<?xml version="1.0" encoding="UTF-8"?>
<mime-info xmlns="http://www.freedesktop.org/standards/shared-mime-info">
  <mime-type type="text/x-abaqus-input">
    <sub-class-of type="text/plain"/>
    <comment>Abaqus input file</comment>
    <glob pattern="*.inp"/>
  </mime-type>
  <mime-type type="text/x-abaqus-status">
    <sub-class-of type="text/plain"/>
    <comment>Abaqus status file</comment>
    <glob pattern="*.sta"/>
  </mime-type>
</mime-info>
XML
command -v update-mime-database >/dev/null 2>&1 && update-mime-database "$DATA/mime" >/dev/null 2>&1 || true

cat > "$DATA/applications/inplite.desktop" <<DESK
[Desktop Entry]
Type=Application
Name=InpLite
GenericName=Abaqus input file editor
Comment=Lightweight editor for Abaqus input (.inp) files, viewer for .dat/.msg/.sta
Exec="$BIN/inplite" %F
Icon=$DATA/icons/hicolor/256x256/apps/inplite.png
Terminal=false
Categories=Development;TextEditor;
MimeType=text/x-abaqus-input;text/x-abaqus-status;text/plain;
DESK
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$DATA/applications" >/dev/null 2>&1 || true

# desktop icon (remote desktops such as VNC / FastX / X2Go)
if [ "$DESKTOP_ICON" = 1 ]; then
  DESK_DIR=$(xdg-user-dir DESKTOP 2>/dev/null || echo "$HOME/Desktop")
  if [ -d "$DESK_DIR" ]; then
    cp "$DATA/applications/inplite.desktop" "$DESK_DIR/inplite.desktop"
    chmod 755 "$DESK_DIR/inplite.desktop"
    command -v gio >/dev/null 2>&1 && gio set "$DESK_DIR/inplite.desktop" metadata::trusted true 2>/dev/null || true
  fi
fi

# make 'inplite' available in new terminals
case ":$PATH:" in
  *":$BIN:"*) ;;
  *)
    case "$(basename "${SHELL:-sh}")" in
      csh|tcsh)
        RC="$HOME/.cshrc"; LINE="set path = ( $BIN \$path )  # InpLite" ;;
      zsh)
        RC="$HOME/.zshrc"; LINE="export PATH=\"$BIN:\$PATH\"  # InpLite" ;;
      *)
        RC="$HOME/.bashrc"; LINE="export PATH=\"$BIN:\$PATH\"  # InpLite" ;;
    esac
    grep -q "# InpLite" "$RC" 2>/dev/null || echo "$LINE" >> "$RC"
    echo "Added $BIN to PATH in $RC (open a new terminal, or run: $LINE)"
    ;;
esac

echo "InpLite installed: $BIN/inplite"
echo "Run:  inplite model.inp     (or from the application menu)"
