# Loom

A general-purpose Lisp in the family of Common Lisp, Clojure and Racket:
CLOS-style generic functions, the condition system and images; immutable
data and identity/state separation; hygienic macros and modules — on a Rust
runtime built for many cores.

- Language design: [docs/design.md](docs/design.md)
- Runtime architecture and milestones: [docs/runtime.md](docs/runtime.md)

**Status: M1** — a bytecode interpreter running millions of fibers on a
work-stealing scheduler (one carrier per core), with channels, `alts!`,
atoms, structured concurrency scopes and a parallel MMTk garbage collector.
See [runtime.md §12](docs/runtime.md#12-revised-milestones) for what exists
and what's next.

## Build and run

Requires Rust (stable, edition 2024).

```sh
cargo build --release
./target/release/loom                       # REPL
./target/release/loom -e '(reduce + (range 1000000))'
./target/release/loom tests/loom/core_test.loom
cargo test                                  # Rust + Loom test suites
cargo run -p jit-spike --release            # tier-1 platform checks
./target/release/loom -e '(do (load-file "bench/pingpong.loom") (ping-pong 500000 10))'
                                            # 1M fibers ping-pong
LOOM_GC_STRESS=1 ./target/release/loom tests/loom/conc_test.loom
                                            # force a GC at every slow-path poll
```

## Layout

```
crates/loom-rt   values, heap, objects, collections, printer, namespaces
crates/loom-vm   reader, seed compiler, interpreter, scheduler, channels, primitives
crates/loom      the `loom` binary
lib/core         loom.core, written in Loom
tests/loom       Loom test suites
bench/           benchmarks
spikes/          throwaway platform experiments
docs/            design documents
```
