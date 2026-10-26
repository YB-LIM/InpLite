InpLite for Linux (x86_64)
==========================

Runs on Linux distributions with glibc 2.17 or newer: RHEL / CentOS / Rocky / Alma 7, 8, 9,
SUSE 12+, Ubuntu 14.04+, ... — typical HPC clusters included. No root access is needed.

Install
  tar xzf InpLite-<version>-linux-x86_64.tar.gz
  cd InpLite-<version>-linux-x86_64
  ./install.sh                 # into ~/.local/bin, adds a menu entry, a desktop icon and PATH
  ./install.sh --no-desktop    # without the desktop icon
  ./install.sh --prefix /path/to/shared/apps/inplite   # e.g. a folder your group can read

  Without installing at all:  ./inplite model.inp

Run
  inplite model.inp            # from a terminal (new terminal after install, or source ~/.bashrc)
  inplite job.sta job.msg      # output files: summary, errors/warnings; a running job's .sta is
                               # followed live (eye button = monitor)
  or: application menu -> Development -> InpLite, or the desktop icon

Display
  InpLite is a graphical program (OpenGL 2.0, software rendering is fine). Use it in a remote
  desktop session (VNC, FastX, X2Go, ETX, ThinLinc, ...) or on a Linux workstation. Plain
  "ssh -X" forwarding often does not support OpenGL; if InpLite does not start over ssh -X,
  use a remote desktop session instead.

  Needed system libraries (present on any machine with a desktop): libX11, libXcursor, libXrandr,
  libXi, libxkbcommon(-x11), libGL / libEGL (Mesa). Wayland desktops are supported as well.

Uninstall
  ./uninstall.sh               # settings stay in ~/.config/InpLite

File dialogs are built into InpLite (no GTK or zenity needed).
