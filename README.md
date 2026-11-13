# Loom

A general-purpose Lisp in the family of Common Lisp, Clojure and Racket:
CLOS-style generic functions, the condition system and images; immutable
data and identity/state separation; hygienic macros and modules — on a Rust
runtime built for many cores.

- Language design: [docs/design.md](docs/design.md)
- Runtime architecture and milestones: [docs/runtime.md](docs/runtime.md)

**Status: M0** — a bytecode interpreter on heap-allocated stacks, persistent
collections, macros and a REPL. No garbage collector yet (M1); see
[runtime.md §12.1](docs/runtime.md#121-m0-status-2026-11-13) for what exists
and what's deferred.

## Build and run

Requires Rust (stable, edition 2024).

```sh
cargo build --release
./target/release/loom                       # REPL
./target/release/loom -e '(reduce + (range 1000000))'
./target/release/loom tests/loom/core_test.loom
cargo test                                  # Rust + Loom test suites
cargo run -p jit-spike --release            # tier-1 platform checks
```

## Layout

```
crates/loom-rt   values, heap, objects, collections, printer, namespaces
crates/loom-vm   reader, seed compiler, bytecode interpreter, primitives
crates/loom      the `loom` binary
lib/core         loom.core, written in Loom
tests/loom       Loom test suites
spikes/          throwaway platform experiments
docs/            design documents
```
