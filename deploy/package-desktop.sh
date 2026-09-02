#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
platform="${1:-}"
stage="out/stage/endif"
rm -rf out/stage
mkdir -p "$stage"

case "$platform" in
  windows)
    cargo build --release -p endif-client -p endif-updater
    cp target/release/endif-client.exe "$stage/endif.exe"
    cp target/release/endif-updater.exe "$stage/endif-updater.exe"
    ;;
  linux)
    cargo build --release -p endif-client -p endif-updater
    cp target/release/endif-client "$stage/endif"
    cp target/release/endif-updater "$stage/endif-updater"
    ;;
  *)
    echo "usage: $0 windows|linux" >&2
    exit 2
    ;;
esac

cp -r crates/client/assets "$stage/assets"
cat > "$stage/README.txt" <<'TXT'
endif.tf - desktop build
========================
Windows: SmartScreen may warn about an unknown publisher: "More info" > "Run anyway".
Linux:   x86_64, needs glibc 2.35 or newer (Ubuntu 22.04+).
Updates: the game offers "update now" when the server has moved on; endif-updater
         fetches the current package into this folder and restarts the game.

Source: https://github.com/sarahkittyy/endif-tf
TXT

cd out/stage
case "$platform" in
  windows) rm -f ../endif-windows.zip; 7z a -tzip -r ../endif-windows.zip endif >/dev/null ;;
  linux)   tar czf ../endif-linux.tar.gz endif ;;
esac
cd ../..
rm -rf out/stage
ls -l out/
