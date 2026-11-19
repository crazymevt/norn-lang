#!/usr/bin/env bash
# tools/ci/watchdog.sh SECONDS COMMAND [ARGS…]: run COMMAND (a skein); if it
# is still running after SECONDS, have it print every fiber's stack
# (SIGQUIT: os::dump_on_sigquit), then the native threads' stacks where a
# debugger can show them, and end it. Its exit status otherwise.
#
# For CI: a suite that waits forever only ends at the job's time limit,
# with nothing in the log to say which test, or what its fibers wait on.
set -u
limit=$1
shift
"$@" &
pid=$!
waited=0
while kill -0 "$pid" 2>/dev/null; do
  if [ "$waited" -ge "$limit" ]; then
    echo "watchdog: still running after ${limit} s: $*" >&2
    echo "watchdog: its fibers (SIGQUIT):" >&2
    kill -QUIT "$pid" 2>/dev/null
    sleep 15
    # (a carrier stuck in Rust shows only here)
    echo "watchdog: its native threads:" >&2
    if [ "$(uname)" = Darwin ]; then
      sample "$pid" 3 -mayDie 2>&1 | sed '/^Binary Images:/q' | sed -n '1,500p' >&2 || true
    elif command -v gdb >/dev/null; then
      sudo -n gdb -p "$pid" -batch -ex 'thread apply all bt 30' 2>&1 | sed -n '1,600p' >&2 || true
    else
      echo "watchdog: (no debugger here)" >&2
    fi
    kill -KILL "$pid" 2>/dev/null
    wait "$pid" 2>/dev/null
    exit 124
  fi
  sleep 1
  waited=$((waited + 1))
done
wait "$pid"
