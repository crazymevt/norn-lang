# Plan: M6 and M7

The work plan for the next two milestones (runtime.md §12 has the scope and
done-checks). Each part ends with its tests passing at tier 0 and tier 1 and
under GC stress, and with a commit. Decisions made along the way are recorded
in design.md or runtime.md and listed at the end of this file.

## Before M6: loading libraries

Modules arrive in M8, but M6 already adds a dozen libraries. Until then:

- The libraries live in `lib/norn/` (`io.norn`, `net.norn`, …), one
  namespace each (`norn.io`, …).
- They are embedded in the binary with `include_str!`, so `norn` stays a
  single file.
- `(require 'norn.io)` loads one the first time, from the embedded copy
  (or from `NORN_PATH` for user code), and caches it.
- An image includes whatever was loaded before the save.

## M6: I/O, the system, and a basic FFI — done (2026-11-13)

| Part | Work | Checked by |
|---|---|---|
| 6.1 | **Weak references and finalizers** in the MMTk binding (`ReferenceGlue`, finalizable objects). The resource registry becomes weak. | Weak values disappear after a GC; finalizers run once; GC stress |
| 6.2 | **Byte arrays and buffers** (a new heap kind); UTF-8, base64 and hex encodings | Round trips; printing and equality |
| 6.3 | **The reactor**: a `mio` thread (kqueue/epoll) that wakes parked fibers through waiters and claims, as timers do; a **blocking pool** for files and blocking calls; the **monitor thread**, which starts a replacement carrier when one is stuck in a long native call | Thousands of fibers waiting on sockets without blocking a carrier; a carrier stuck in a blocking call doesn't stall the others |
| 6.4 | **`norn.io`** (files, streams, paths), **`norn.fs`** (walking directories, glob patterns, watching with `notify`), **`norn.sys`**, **`norn.process`** (child processes with pipes), **`norn.time`**, **`norn.signal`** (signals as channel messages, via `signal-hook`), **fiber dumps** | Library suites; signals; a fiber dump of a busy runtime |
| 6.5 | **`norn.net`**: TCP and UDP on the reactor; TLS with `rustls` | Echo client and server; TLS against a local certificate |
| 6.6 | **The basic FFI**: load a library (`libloading`); call functions with integer, float, pointer and byte-buffer arguments; Cranelift-generated call stubs; fast or blocking calls; handles as resources with finalizers | Calls into libc and a test library; a blocking call doesn't stall its carrier |
| 6.7 | **Done-check and docs**: a 10,000-connection TCP echo server; a socket saved in an image comes back as `ResourceLost` and `reconnect` works; runtime.md status §12.7 | `crates/norn/tests/io.rs` |

## M7: numbers, collections, the pure stdlib and tooling

Decisions for M7 are at the end, after M6's.

| Part | Work |
|---|---|
| 7.1 | **The numeric tower**: bignums (a heap kind holding limbs in the Norn heap) and ratios; automatic promotion in the primitives; tier 1's overflow takes the slow path; the equality rules of design §3.4 (reflexive NaN, normalisation, categories) |
| 7.2 | **Equality declarations and mutable objects**: `(:equality :identity)`, `:compare false`; `(:mutable)` classes with fiber ownership, `release!`, `share` and `locking` (design §4.2) |
| 7.3 | **Sequences**: type-preserving `map`, `filter` and the rest (design §3.3); `lazy` streams and the generators; transducers (with `preduce` and channels) |
| 7.4 | **Collections**: sorted maps and sets, a persistent queue, transients, and persistent ropes (for the editor) |
| 7.5 | **Text and data**: `norn.string` (with Unicode graphemes and display widths), `norn.format`, `norn.regex` (the `regex` crate), `norn.json`, `norn.edn`, `norn.pprint` |
| 7.6 | **Everything else in the pure stdlib**: `norn.math`, `norn.set`, `norn.walk`, `norn.random`, `norn.async` extras, `norn.sync`, `norn.cache`, `norn.log` |
| 7.7 | **Testing**: `norn.test` and the `norn test` runner; the existing suites ported to it |
| 7.8 | **Formatting and linting**: the reader mode that keeps comments and whitespace; `norn fmt`; indentation hints on macros; `norn lint` (compiler warnings); `:format`/`:lint` in the editor protocol; `lib/` and `tests/` formatted and lint-clean |
| 7.9 | **Docs and the REPL**: the docstring convention, `norn doc`, deprecation; `doc`, `apropos`, `source`, an inspector, `trace`/`untrace`; `norn.bench`; a terminal line editor (`rustyline`); `norn image info` and image diff; `norn check`; coverage if cheap |
| 7.10 | **Done-check and docs**: every suite on `norn.test`; promotion past 62 bits identical at both tiers; `fmt --check` and `lint` clean; runtime.md status §12.8 |

## Decisions made along the way

- **2026-11-13: Emacs and VS Code plugins dropped from M7.** The Norn
  editor (M7.6) is the target; other editors can use `norn lsp` (M9).
- **Library loading (before modules).**
  - `(require 'a.b)` looks for an embedded library first, then
    `a/b.norn` under `NORN_PATH` and the current directory. Dashes stay
    dashes in file names.
  - `ns` gained `(:require [lib :as x :refer [...]])`. Aliases also work
    inside syntax-quote, where they expand to the full namespace name.
  - When several fibers require the same library at once, one loads it and
    the others wait. A circular require is an error.
  - Images save aliases (root format version 2).
- **Weak references are cleanups, not finalizers** (runtime.md §4.5): no
  resurrection, and cleanups run in their own fibers. Cleanups aren't saved
  in images. The resource registry is now weak.
- **A known GC imprecision.** A finished call's registers stay reachable
  until the caller reuses them, so weak references clear eventually rather
  than at the first possible collection. The fix is in M11 (runtime.md §4.5).
- **`Bytes` is immutable**, a value like a string, with the same layout.
  Reads return fresh `Bytes`; mutable buffers wait for M7's owned mutable
  objects. The literal is `#bytes "68656c6c6f"` (hex), so bytes print
  readably. `=` and `hash` compare contents, and bytes never equal a vector
  or a string. Bytes have their own builtin class, `Bytes`.
- **Hex and base64 are hand-written** (`norn-vm/src/codec.rs`, about 80
  lines, tested against RFC 4648's examples) rather than a crate.
- **Standard-library tests** live in one suite, `tests/norn/lib_test.norn`,
  which runs in process, at both tiers, and from an image.
- **I/O waits use event counts, not readiness flags** (runtime.md §5.5).
  The wrapper reads the count before trying the operation and waits only if
  it hasn't changed, so an event between "would block" and parking is never
  lost.
- **Handles carry the process epoch.** A handle restored from an image
  reports "resource lost" rather than reaching a reused descriptor number.
- **Spare carriers** live in extra scheduler slots (`n..2n`). The monitor
  starts one when carriers are stuck in native code for over 20 ms while
  work waits; it retires when idle.
- **Fibers waiting on I/O or the blocking pool are saved dead in images.**
- **`mio` 1.x** for the reactor; no async runtime (no Tokio) is involved.
- **All file I/O runs on the blocking pool**, including `slurp` and `spit`.
  A slow disk or network file system can't stall a carrier.
- **Files are handles too**, so `read`, `write` and `close` work the same on
  files, pipes and sockets. An unreachable handle is closed by a cleanup
  that holds only its number.
- **OS handles don't go in the resource registry.** Their process epoch
  already tells whether they're live, and registering every socket would
  make opening one cost time proportional to the number open.
- **`ensure-live` returns the live resource.** A handle's `reconnect`
  returns a new handle; a file opened `:write` reopens as `:append`, so what
  was written before the restore stays.
- **`try` recognises `catch` and `finally` in any namespace.** A macro's
  syntax-quote qualifies them (`norn.io/finally`), which used to turn them
  into ordinary calls.
- **File watching uses `notify` 8.** Events go to a channel buffering 1,024
  of them, and are dropped while it's full.
- **`norn.sys` reads environment variables but doesn't set them**; children
  get their environment through `norn.process`.
- **Child processes:**
  - A child's pipes are ordinary reactor streams.
  - Waiting runs on the blocking pool; `kill` signals by pid, so it never
    waits for the waiter.
  - `sh` takes options after the arguments, as Clojure's does:
    `(sh "ls" "-l" :dir "/")`.
- **Time:**
  - `Instant` and `Duration` are classes holding nanoseconds, with
    instants measured from the epoch in UTC. Text is RFC 3339.
  - Zones are UTC, local time (from the OS, via `localtime_r`) or a fixed
    offset in seconds. A time-zone database (named zones) waits for a
    real need.
  - Calendar arithmetic uses Howard Hinnant's civil-date algorithms (no
    crate).
- **Signals use `signal-hook`.** Each `listen` gets a channel buffering 64
  signals. `:kill` and `:stop` can't be caught.
- **Fiber dumps** (`(fiber-dump)`, or SIGQUIT / Ctrl-\ printing to stderr,
  as the JVM does):
  - Every fiber is paused while the dump walks the heap from the GC roots
    (`norn-rt/src/walk.rs`). It lists each unfinished fiber's state, what
    it waits for, its namespace and its frames.
  - The walk holds off the reactor and the pool but not the timer thread.
    Listing the roots takes the timer lock, and a timer firing can only move
    a fiber to a run queue.
- **Sockets:**
  - TCP and UDP are non-blocking `mio` sockets; only DNS resolution runs on
    the blocking pool.
  - TLS is `rustls` with the `ring` provider (no C build), trusting the OS's
    root certificates (`rustls-native-certs`) unless given `:ca-file`.
  - A TLS client finishes its handshake in `connect`, so an untrusted
    server fails there. A server handshakes on its first read, so one slow
    client can't hold up an accept loop.
  - I/O primitives answer `:again` (wait to read) or `:again-write` (wait
    to write). TLS can need either direction for either operation.
- **The open-file limit is raised at startup** to the most the OS allows
  (macOS caps it at `kern.maxfilesperproc`), as Go does.
- **Test certificates** live in `tests/norn/certs` (a CA and a
  `localhost`/`127.0.0.1` server certificate valid for 100 years; the CA's
  key was deleted).
- **Found and fixed: heap corruption under GC stress.** Two causes:
  - The reactor took waiters out of its table before becoming a running
    participant, so a collection could free them.
  - Feeder threads' thread-local mutators were destroyed at thread exit
    outside a safepoint.

  Also a deadlock: closing a handle on a carrier went through `delivering`.
  The rule now in runtime.md §5.5 covers all three.
- **The basic FFI** (runtime.md §5.6):
  - Cranelift call stubs per signature, so no `libffi`.
  - `:bytes`, `:string` and `:out` cover buffers without struct support.
  - Library and function handles reconnect automatically after an image
    restore (no `ResourceLost`), since a library has no session state.
  - Libraries are never unloaded while one of their functions exists.

### M7

- **The numeric tower** (runtime.md §2.5) uses `num-bigint` and
  `num-rational` for the arithmetic. `BigInt` and `Ratio` are heap kinds,
  always normalised.
  - Bignums share the `Integer` class; `Ratio` has a builtin class of its
    own.
  - `numerator`, `denominator`, `ratio?` and `rational?` are added.
  - Bit operations work at any size: `bit-shift-left` promotes instead of
    overflowing.
- **NaN:** `=` treats it as equal to itself (so maps and sets find it).
  `<`, `==` and the like are false with NaN, as IEEE says. `sort` puts it
  last.
- **The FFI's 64-bit results** now come back as bignums when they don't fit
  a fixnum, and `:u64` arguments accept bignums.
- **Class policy.** The unused class field 6 became `POLICY`:
  `[flags excluded-keys settable-keys]`, computed by `defclass` over the
  whole class precedence list, so subclasses inherit mutability and
  identity equality. `=` and `hash` read it in Rust; no user code runs.
- **Mutable instances** carry one extra owner word after their slots,
  marked by bit 62 of the slot-count word, so the GC and images size them
  correctly. The image loader masks raw size fields rather than clamping
  them, keeping that bit.
- **Rust errors only carry a message**, so `Ownership: …` messages become
  `OwnershipError`s in `%runtime-error`. A general mechanism for typed
  errors from Rust is left for later.
- **Shared objects:** reads skip the lock and writes take it (design §4.2).
  The lock is a one-token channel plus a holder atom, so it parks; it's
  reentrant.
- **Streams are realized by the interpreter** (runtime.md §5.4.1), not by
  Norn wrappers around `first` and friends, so streams are real seqs and
  the primitives keep their speed. `cons` keeps a stream tail unrealized.
- **On a map:** the selecting functions keep a map; the ones that compute
  new items give a vector (design §3.3).
- **Transducers** are the sequence functions with one argument fewer;
  `reduced`, `reduced?` and `unreduced` are primitives (a small `Reduced`
  heap kind), so `reduce` checks for early exit cheaply.
- **`norn -e`** realizes a stream result before printing it, as a REPL
  prints a lazy seq in full.
- **Found and fixed:** `comp` of three or more functions skipped the first
  one. A `loop` binding shadowed the list it was reading, and it had gone
  untested until transducers.
