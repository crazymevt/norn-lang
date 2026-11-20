#!/bin/sh
# Rebuild skein.pe's test programs (on any OS with rustup's Windows targets:
# rustup target add aarch64-pc-windows-msvc x86_64-pc-windows-msvc).
# rust-lld links them; no Windows SDK or runtime library is needed.
set -e
cd "$(dirname "$0")"
build() {
  t="$1"; shift
  rustc --edition 2024 --target "$t" -C panic=abort -C opt-level=s -C debuginfo=0 \
    -C linker=rust-lld -C linker-flavor=lld-link -C link-arg=/NODEFAULTLIB \
    -C link-arg=/ENTRY:mainCRTStartup -C link-arg=/SUBSYSTEM:CONSOLE -C link-arg=/DEBUG:NONE \
    -C link-arg=/DYNAMICBASE -C link-arg=/BREPRO "$@" hello.rs
}
build aarch64-pc-windows-msvc -o hello-arm64.exe
build x86_64-pc-windows-msvc -o hello-x64.exe
# with a resource already: manifest.res, an RT_MANIFEST (UTF-8 code page)
build aarch64-pc-windows-msvc -C link-arg=manifest.res -o hello-manifest-arm64.exe
