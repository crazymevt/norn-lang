# Skein

A general-purpose Lisp in the family of Common Lisp, Clojure and Racket:
CLOS-style generic functions, the condition system and images; immutable
data and identity/state separation; hygienic macros and modules — on a Rust
runtime built for many cores.

Skein was called Norn until 2026-11-17, when another language took that
name (see [the rename](docs/plans/plan-rename-skein.md)).

- [User documentation](docs/README.md) — getting started, runnable examples,
  language reference, and symbol index.
- Language design: [docs/internals/design.md](docs/internals/design.md)
- Runtime architecture and milestones: [docs/internals/runtime.md](docs/internals/runtime.md)

**Status: M7** — a bytecode interpreter running millions of fibers on a
work-stealing scheduler (one carrier per core), with channels, `alts!`,
atoms, structured concurrency scopes and a parallel MMTk garbage collector;
plus CLOS-style classes and generic functions (multiple dispatch, method
combination, live redefinition across all cores), and a Common Lisp-style
condition system with a debugger: an unhandled error parks the fiber with its
stack intact, you fix the code, and resume it from the REPL or an editor
(`skein --server`). The compiler is written in Skein and compiles itself, and hot functions are
compiled to native code with Cranelift in the background (2–10× faster on
the benchmarks; `SKEIN_TIER=0` runs the interpreter alone).
Images save the running heap, parked fibers included (even one waiting in
the debugger), and start in about 35 ms. I/O never blocks a carrier: files
on a blocking pool, sockets (TCP, UDP, TLS) and pipes on a reactor, and C
functions through a basic FFI. One process can hold 10,000 connections on
two carriers. Numbers promote to bignums and ratios; collections include
sorted maps and sets, queues, transients and ropes; sequence functions keep
their collection's type, with explicit lazy streams and transducers. The
standard library covers I/O and the system (`skein.io`, `skein.fs`,
`skein.sys`, `skein.process`, `skein.time`, `skein.signal`, `skein.net`,
`skein.ffi`, `skein.bytes`), text and data (`skein.string`, `skein.regex`,
`skein.json`, `skein.ndjson`, `skein.xml`, `skein.yaml`, `skein.cbor`, `skein.edn`,
`skein.decimal`, `skein.buffer`, `skein.format`, `skein.pprint`, `skein.rope`) and
the rest (`skein.math`, `skein.set`, `skein.walk`, `skein.random`,
`skein.async`, `skein.sync`, `skein.cache`, `skein.log`). Tools: `skein test`,
`skein fmt`, `skein lint`, `skein doc`, `skein check` and `skein image`, plus
`doc`, `source`, `apropos`, `inspect`, `trace` and `skein.bench` at the
REPL.
See [runtime.md §12](docs/internals/runtime.md#12-revised-milestones) for what exists
and what's next.

## Build and run

Requires Rust (stable, edition 2024).

```sh
cargo build --release
./target/release/skein                       # REPL
./target/release/skein -e '(reduce + (range 1000000))'
./target/release/skein test                  # every tests/**/*_test.skn
./target/release/skein check                 # fmt --check, lint, test, doc examples
./target/release/skein doc skein.string       # what a library offers
cargo test                                  # Rust + Skein test suites
./target/release/skein lang-tests/run.skn   # five real programs at every tier (lang-tests/README.md)
./target/release/skein bench/compare.skn    # the benchmarks against bench/baseline.edn
cargo run -p jit-spike --release            # tier-1 platform checks
./target/release/skein -e '(do (load-file "bench/pingpong.skn") (ping-pong 500000 10))'
                                            # 1M fibers ping-pong
./target/release/skein -e '(do (load-file "bench/dispatch.skn") (live-redefinition 8 300))'
                                            # live defmethod under 8 cores
SKEIN_GC_STRESS=2 ./target/release/skein test
                                            # force a GC at every slow-path poll
./target/release/skein -e '(save-image "dev.skimg")'
./target/release/skein --image dev.skimg       # carry on from a saved image
./target/release/skein build app.skimg app.skn --entry app/main
./target/release/skein --image app.skimg ARGS… # run its entry function
./target/release/skein -e "(do (require '[skein.process :as p]) (p/sh \"uname\" \"-a\"))"
```

## Layout

```
crates/skein-rt   values, heap, objects, collections, printer, namespaces
crates/skein-vm   reader, seed compiler, interpreter, scheduler, channels, primitives,
                 reactor, blocking pool, sockets, FFI
crates/skein      the `skein` binary
lib/core         skein.core, written in Skein
lib/compiler     the Skein compiler, written in Skein
lib/skein         the standard libraries (skein.io, skein.net, …)
tests/skein       Skein test suites
lang-tests/      five programs written as users would, checking each change to Skein
bench/           benchmarks
spikes/          throwaway platform experiments
docs/            user documentation, language reference, and design documents
```

## License

MIT — see [LICENSE](LICENSE).
