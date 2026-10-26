#!/usr/bin/env bash
# Cross-compile InpLite for Windows x64 on Ubuntu 24.04 and build the installer.
# Uses Ubuntu's rustc-1.91 + rust-src (std is built from source for x86_64-pc-windows-gnu).
#   sudo apt install rustc-1.91 cargo-1.91 rust-1.91-src gcc-mingw-w64-x86-64 nsis
# Ubuntu's rust-src drops the windows-targets dependency and the rsbegin/rsend objects;
# restore them once:
#   L=/usr/lib/rust-1.91/lib/rustlib/src/rust/library
#   (add  [target.'cfg(any(windows, target_os = "cygwin"))'.dependencies.windows-targets] path = "../windows_targets"
#    to $L/std/Cargo.toml and set windows_raw_dylib = ["windows-targets/windows_raw_dylib"])
#   for f in rsbegin rsend; do RUSTC_BOOTSTRAP=1 rustc-1.91 --target x86_64-pc-windows-gnu --emit=obj -C panic=abort \
#       --crate-type=lib -o /usr/lib/rust-1.91/lib/rustlib/x86_64-pc-windows-gnu/lib/$f.o $L/rtstartup/$f.rs; done
# With rustup on Windows simply run:  cargo build --release
set -euo pipefail
cd "$(dirname "$0")"
RUSTC=rustc-1.91 RUSTC_BOOTSTRAP=1 cargo-1.91 build --release \
  -Zbuild-std=std,panic_abort --target x86_64-pc-windows-gnu
mkdir -p dist
cp target/x86_64-pc-windows-gnu/release/inplite.exe dist/inplite.exe
(cd installer && makensis -V2 inplite.nsi)
ls -la dist
