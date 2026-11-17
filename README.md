# Norn

A general-purpose Lisp in the family of Common Lisp, Clojure and Racket:
CLOS-style generic functions, the condition system and images; immutable
data and identity/state separation; hygienic macros and modules — on a Rust
runtime built for many cores.

- [User documentation](docs/README.md) — getting started, runnable examples,
  language reference, and symbol index.
- Language design: [docs/design.md](docs/design.md)
- Runtime architecture and milestones: [docs/runtime.md](docs/runtime.md)

**Status: M7** — a bytecode interpreter running millions of fibers on a
work-stealing scheduler (one carrier per core), with channels, `alts!`,
atoms, structured concurrency scopes and a parallel MMTk garbage collector;
plus CLOS-style classes and generic functions (multiple dispatch, method
combination, live redefinition across all cores), and a Common Lisp-style
condition system with a debugger: an unhandled error parks the fiber with its
stack intact, you fix the code, and resume it from the REPL or an editor
(`norn --server`). The compiler is written in Norn and compiles itself, and hot functions are
compiled to native code with Cranelift in the background (2–10× faster on
the benchmarks; `NORN_TIER=0` runs the interpreter alone).
Images save the running heap, parked fibers included (even one waiting in
the debugger), and start in about 35 ms. I/O never blocks a carrier: files
on a blocking pool, sockets (TCP, UDP, TLS) and pipes on a reactor, and C
functions through a basic FFI. One process can hold 10,000 connections on
two carriers. Numbers promote to bignums and ratios; collections include
sorted maps and sets, queues, transients and ropes; sequence functions keep
their collection's type, with explicit lazy streams and transducers. The
standard library covers I/O and the system (`norn.io`, `norn.fs`,
`norn.sys`, `norn.process`, `norn.time`, `norn.signal`, `norn.net`,
`norn.ffi`, `norn.bytes`), text and data (`norn.string`, `norn.regex`,
`norn.json`, `norn.ndjson`, `norn.xml`, `norn.yaml`, `norn.cbor`, `norn.edn`,
`norn.decimal`, `norn.buffer`, `norn.format`, `norn.pprint`, `norn.rope`) and
the rest (`norn.math`, `norn.set`, `norn.walk`, `norn.random`,
`norn.async`, `norn.sync`, `norn.cache`, `norn.log`). Tools: `norn test`,
`norn fmt`, `norn lint`, `norn doc`, `norn check` and `norn image`, plus
`doc`, `source`, `apropos`, `inspect`, `trace` and `norn.bench` at the
REPL.
See [runtime.md §12](docs/runtime.md#12-revised-milestones) for what exists
and what's next.

## Build and run

Requires Rust (stable, edition 2024).

```sh
cargo build --release
./target/release/norn                       # REPL
./target/release/norn -e '(reduce + (range 1000000))'
./target/release/norn test                  # every tests/**/*_test.norn
./target/release/norn check                 # fmt --check, lint, test, doc examples
./target/release/norn doc norn.string       # what a library offers
cargo test                                  # Rust + Norn test suites
cargo run -p jit-spike --release            # tier-1 platform checks
./target/release/norn -e '(do (load-file "bench/pingpong.norn") (ping-pong 500000 10))'
                                            # 1M fibers ping-pong
./target/release/norn -e '(do (load-file "bench/dispatch.norn") (live-redefinition 8 300))'
                                            # live defmethod under 8 cores
NORN_GC_STRESS=2 ./target/release/norn test
                                            # force a GC at every slow-path poll
./target/release/norn -e '(save-image "dev.nimg")'
./target/release/norn --image dev.nimg       # carry on from a saved image
./target/release/norn build app.nimg app.norn --entry app/main
./target/release/norn --image app.nimg ARGS… # run its entry function
./target/release/norn -e "(do (require '[norn.process :as p]) (p/sh \"uname\" \"-a\"))"
```

## Layout

```
crates/norn-rt   values, heap, objects, collections, printer, namespaces
crates/norn-vm   reader, seed compiler, interpreter, scheduler, channels, primitives,
                 reactor, blocking pool, sockets, FFI
crates/norn      the `norn` binary
lib/core         norn.core, written in Norn
lib/compiler     the Norn compiler, written in Norn
lib/norn         the standard libraries (norn.io, norn.net, …)
tests/norn       Norn test suites
bench/           benchmarks
spikes/          throwaway platform experiments
docs/            user documentation, language reference, and design documents
```

## License

MIT — see [LICENSE](LICENSE).
