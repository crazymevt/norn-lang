# Bug-fix scan (2026-11-19)

Seven read-only scans of skein-lang (interpreter, JIT, scheduler and I/O,
runtime data/GC/images, prims/reader/FFI, the Skein compiler and core, the
data-format libraries) found about sixty defects, each reproduced against the
release binary unless marked *(reasoning)*. This is their order and the plan
for fixing them.

**Priority.** P0: memory safety, a crash or hang of the whole process,
silent security weakening, wrong dispatch. P1: wrong results or hangs and
leaks in ordinary use. P2: edge cases, spec conformance, low-impact
leniency.

## P0 — fixed first, here

| # | Defect | Where | Status |
|---|---|---|---|
| 1 | Compiled code keeps object pointers across a call that lets a moving collection run (FFI, main lane): SIGSEGV | jit.rs `call_any` | fixed: the GC count is compared around the call; if it changed, the interpreter goes on at the next instruction |
| 2 | Closing a handle a blocking-pool job holds (file read, child wait) blocks the carrier; a collection then deadlocks the runtime | reactor.rs `close_id_running` | fixed: `try_lock`; the job's reference drops the source when it ends |
| 3 | A panic on a carrier hangs the process (it stays registered for safepoints) | sched.rs `start_carrier` | fixed: the process ends with status 101 |
| 4 | `pbkdf2` iterations cast to u32: 2^32+1 means 1 iteration; 2^32 panics | pack.rs | fixed: range-checked |
| 5 | `%throw`, `%restart-frame`, `%abort-fiber` skip their arity check: index panic | vm.rs `invoke` | fixed |
| 6 | `process/kill` after `wait` signals a reaped pid (pid reuse) | os.rs `kill` | fixed: nil for a child already waited for |
| 7 | `(chan 4611686018427387903)` wraps the allocation size: heap corruption (SIGBUS) | heap.rs `alloc`, prims.rs `chan` | |
| 8 | CLOS dispatch cache: an `eql` specializer whose value is a class collides with "an instance of that class" | clos.rs `dispatch_key` | |
| 9 | An unrealized lazy seq hashes as `()`: maps and sets hold two `=` keys; a stream doesn't find a vector key | equal.rs `Frame::sequential`, prims.rs `assoc`/`hash-set`/`get` | |
| 10 | Image loader: unchecked size add (panic) and reference words without a reloc bit copied raw (wild pointer) | skein-rt image.rs `load_layer` | |
| 11 | Verifier doesn't check inline records in debug locals: crafted protos segfault | verify.rs | |

## P1 — wrong results, hangs, leaks

Runtime (here):
- Instants outside ~1823–2116 wrap (nanos through a 63-bit fixnum); bigint nanos can't be formatted — prims.rs `%parse-iso`/`%from-civil`, os.rs.
- Mutable buffer `:i64` reads wrap beyond ±2^62 — prims.rs `mbuf_load`.
- `retry` after a failed parked call in tail position re-runs the caller — vm.rs `handle_error`.
- Restarting a named local fn's frame calls a global of the same name — vm.rs.
- Tier 1 `(= s s)` doesn't force a stream — jit.rs.
- Formatting at an offset with seconds names another instant — os.rs `format_iso`.
- `char` truncates code points; `count` of a huge range wraps — prims.rs.

Concurrency libraries (worktree A):
- A fiber cancelled during a blocking open leaks its descriptor — blocking.rs/sys.rs.
- A failed TLS connect leaks its TCP socket; `net/connect :timeout` leaks a fiber and socket when its caller is cancelled, and drops a handle when the timeout races the connect — net.skn.
- `once` hangs forever if its first caller is cancelled — sync.skn.
- `io/close` on TLS hangs while the peer isn't reading — io.skn.
- Abandoned `alts!` waiters pile up on a channel that rarely fires — conc.rs.
- `process/sh` fails with EPIPE when the child doesn't read all of `:in` — process.skn.
- `take-token!` with n above the burst never returns — sync.skn.

Compiler and core (worktree B):
- `map` with 3+ collections and `mapcat` with 2+ run eagerly over streams (hang on `range`).
- `reductions` ignores `reduced`.
- `recur` inside `try`/`handler-case`/`catch-tag` targets the hidden thunk.
- Hygiene: a `defsyntax` template handing a bare user identifier to a `defmacro` macro (`let`, `when`) captures it — expander.skn `precise-step`.
- `syntax`/`syntax-rules` don't substitute inside map or set literals — syntax.skn `%fill`.
- `delay` hangs forever if the computing fiber is cancelled — core.skn.

Text, reader, FFI (worktree D):
- Panics: `last-index-of` with a start on non-ASCII text; JSON `\` before a non-ASCII char; `format` precision > 65535; unbounded `format` width (abort/hang); CST reader without a depth limit (stack overflow in `skein fmt`); `#(%N)` with huge N hangs the reader.
- FFI: `:i64`/`:long`/`:isize` refuse values ≥ 2^62 (BigInts); float parameters refuse ratios and BigInts.
- `matches?` on a `(?x)` pattern ending in a comment.

Data libraries (worktree C):
- `skein.edn/write-string` writes keywords and symbols that don't read back.

## P2 — edge cases and conformance

- Markdown (C): fence with backticks in its info string; tabs in code expanded; nested emphasis; code spans closing on part of a run; entities not decoded; `javascript:` hrefs kept.
- Time (C and here): `%j` rolls over; `+24:00` accepted; `from-fields` treats non-integers as 0.
- TOML (C): misplaced underscores accepted. JSON writer (C): duplicate keys from `"a"` and `:a`; `1e400` reads as `##Inf`. YAML writer (C): DEL and C1 written raw.
- Core (B): `sort-by` with a comparator calls keyfn per comparison; `::skip`/`::none` sentinels leak; `flatten` of nil; `condp` `:>>`.
- Reader/text (D): `\u+041` accepted; hex/base64 errors name the wrong character; `%bytes-index-of` truncates.
- `gensym` counter one ahead when started from the base image (here).

## Left for a decision

- Calling a Var object ignores dynamic bindings (`((var *g*))` → root), as `var-get` does. Clojure honours the binding. Not changed.

## Result

Everything above is fixed with a regression check, except:
- hex/base64 decode errors naming the wrong character (codec.rs), `1e400`
  reading as `##Inf`, and `%bytes-index-of` truncating: not done (P2).
- A cancel landing between a blocking open's finish and the fiber resuming
  can still leak that descriptor (the common case is closed).
- Restarting a named local fn's frame runs it again when it's inside a named
  function (it's named outer/name now); inside an anonymous top-level one
  it's still `ns/name`, and a global of that name wins.
- `reductions` follows Clojure: a `reduced` step's value comes out once
  more at the end, `(1 3 6 6)`.

Checked: every Skein suite at tier 0 and tier 1, `cargo test --release`,
clippy, `skein fmt --check` and `skein lint`, and lang-tests.

## How

P0 here first, then four worktree agents on disjoint files (A concurrency,
B compiler/core, C data libraries, D text/reader/FFI) while the runtime
P1/P2 items are fixed here. Each fix gets a regression check in the nearest
`tests/skein/*_test.skn`. Worktree changes are applied here as diffs; then
`cargo test` and every Skein suite at tier 0 and tier 1.
