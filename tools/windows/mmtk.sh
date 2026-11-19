#!/bin/bash
# Skein on Windows needs MMTk with a Windows layer, which MMTk 0.33.0
# doesn't have (docs/plans/plan-windows.md). This makes one:
# target/mmtk-windows is MMTk 0.33.0 as crates.io has it, with
# tools/windows/mmtk-0.33.0.patch applied. Then build Skein against it:
#
#   tools/windows/mmtk.sh
#   cargo --config "patch.crates-io.mmtk.path='target/mmtk-windows'" build --release
#
# (Not --locked: Cargo.lock names the MMTk on crates.io.) Run it in Git
# Bash on Windows; it works on macOS and Linux too, where the patch
# changes nothing that's compiled.
set -eu
cd "$(dirname "$0")/../.."
version=0.33.0
cargo fetch --locked
src=$(ls -d "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/mmtk-$version | head -1)
[ -d "$src" ] || { echo "mmtk.sh: MMTk $version isn't in Cargo's registry" >&2; exit 1; }
rm -rf target/mmtk-windows
mkdir -p target
cp -R "$src" target/mmtk-windows
cd target/mmtk-windows
# (MMTk pins an older Rust; Skein builds it with its own)
rm -f rust-toolchain
git apply --whitespace=nowarn ../../tools/windows/mmtk-$version.patch
echo "target/mmtk-windows: MMTk $version with the Windows layer"
