#!/bin/bash
# Build and test this checkout on Linux, in a disposable Docker container
# (on a Mac: start Colima first, `colima start`). It sends the tracked files
# of the working tree, so uncommitted changes are tested too, and keeps the
# Cargo registry and the build in named volumes between runs. The whole
# check is what CI's Linux job runs: the build, cargo test, the Skein suites
# (at both tiers here), skein check, and the lang-tests.
#
#   tools/check-linux.sh                  the whole check
#   tools/check-linux.sh tests/skein/X.skn …   just those suites, at both tiers
set -u
cd "$(dirname "$0")/.."
git ls-files | COPYFILE_DISABLE=1 tar --no-xattrs --no-mac-metadata -cf - -T - 2>/dev/null |
  docker run -i --rm \
    -v skein-linux-target:/w/target -v skein-linux-cargo:/usr/local/cargo/registry \
    -e CARGO_TERM_COLOR=never -e SKEIN_UI_HEADLESS=1 -e FILES="$*" rust:1 bash -c '
set -u
mkdir -p /w && cd /w && tar -x 2>/dev/null
apt-get update -qq >/dev/null && apt-get install -y -qq libsqlite3-0 zsh git >/dev/null 2>&1
echo "== $(rustc --version) on $(uname -srm)"
echo "== cargo build --locked --release"
cargo build --locked --release 2>&1 | grep -E "^(warning|error)" -A6 | head -40
status=0
run_suites() {
  for t in 0 1; do
    SKEIN_TIER=$t target/release/skein test "$@" > /tmp/t.log 2>&1 || status=1
    echo "-- tier $t: $(grep -E "^[0-9]+ tests?, [0-9]+ checks?" /tmp/t.log | tail -1)"
    grep -E "FAIL in|ERROR in" -A5 /tmp/t.log | grep -v plain_test | grep -v "^--" | head -30
  done
}
if [ -n "$FILES" ]; then
  run_suites $FILES
else
  echo "== cargo test --locked"
  cargo test --locked --no-fail-fast > /tmp/c.log 2>&1 || status=1
  grep -E "^test result|FAILED|panicked" /tmp/c.log | sort | uniq -c
  echo "== skein suites"
  run_suites tests/skein
  echo "== skein check"
  target/release/skein check > /tmp/k.log 2>&1 || status=1
  tail -3 /tmp/k.log
  echo "== lang-tests"
  target/release/skein lang-tests/run.skn > /tmp/l.log 2>&1 || status=1
  grep -E "FAIL|skipped|all passed|failed:" /tmp/l.log | head -30
fi
exit $status
'
