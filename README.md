# Norn

A general-purpose Lisp in the family of Common Lisp, Clojure and Racket:
CLOS-style generic functions, the condition system and images; immutable
data and identity/state separation; hygienic macros and modules — on a Rust
runtime built for many cores.

- Language design: [docs/design.md](docs/design.md)
- Runtime architecture and milestones: [docs/runtime.md](docs/runtime.md)

**Status: M5** — a bytecode interpreter running millions of fibers on a
work-stealing scheduler (one carrier per core), with channels, `alts!`,
atoms, structured concurrency scopes and a parallel MMTk garbage collector;
plus CLOS-style classes and generic functions (multiple dispatch, method
combination, live redefinition across all cores), and a Common Lisp-style
condition system with a debugger: an unhandled error parks the fiber with its
stack intact, you fix the code, and resume it from the REPL or an editor
(`norn --server`). The compiler is written in Norn and compiles itself; set `NORN_TIER=1`
for Cranelift-generated native code (2–9× faster on the benchmarks so far).
Images save the running heap, parked fibers included (even one waiting in
the debugger), and start in about 35 ms.
See [runtime.md §12](docs/runtime.md#12-revised-milestones) for what exists
and what's next.

## Build and run

Requires Rust (stable, edition 2024).

```sh
cargo build --release
./target/release/norn                       # REPL
./target/release/norn -e '(reduce + (range 1000000))'
./target/release/norn tests/norn/core_test.norn
cargo test                                  # Rust + Norn test suites
cargo run -p jit-spike --release            # tier-1 platform checks
./target/release/norn -e '(do (load-file "bench/pingpong.norn") (ping-pong 500000 10))'
                                            # 1M fibers ping-pong
./target/release/norn -e '(do (load-file "bench/dispatch.norn") (live-redefinition 8 300))'
                                            # live defmethod under 8 cores
NORN_GC_STRESS=1 ./target/release/norn tests/norn/conc_test.norn
                                            # force a GC at every slow-path poll
./target/release/norn -e '(save-image "dev.nimg")'
./target/release/norn --image dev.nimg       # carry on from a saved image
./target/release/norn build app.nimg app.norn --entry app/main
./target/release/norn --image app.nimg ARGS… # run its entry function
```

## Layout

```
crates/norn-rt   values, heap, objects, collections, printer, namespaces
crates/norn-vm   reader, seed compiler, interpreter, scheduler, channels, primitives
crates/norn      the `norn` binary
lib/core         norn.core, written in Norn
tests/norn       Norn test suites
bench/           benchmarks
spikes/          throwaway platform experiments
docs/            design documents
```

## License

MIT — see [LICENSE](LICENSE).
