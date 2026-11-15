# Norn TODO

## Standard library

- [ ] **Command-line parsing (`norn.cli`).** Build on `norn.sys/args` for typed flags, positional arguments, defaults, subcommands, and generated usage. Keep parsing in Norn.
- [ ] **Stream and filesystem helpers (`norn.io`, `norn.fs`).** Add exact byte reads, bounded stream copying, incremental text reads using the existing decoder, scoped temporary files, and atomic file replacement. Stream helpers can use existing handles. Add shared OS primitives where exclusive creation or file synchronization requires them.
- [ ] **Named time zones and calendar arithmetic (`norn.time`).** Add IANA zones, date arithmetic, and explicit policies for skipped or repeated local times. Choose a source for zone data. Reuse `LocalDate`, `LocalTime`, `LocalDateTime`, `Instant`, and `Duration`.
- [ ] **Host terminal support (`norn.terminal`).** Add terminal detection, size, scoped raw input, key decoding, and ANSI styling. Use OS primitives for host terminal controls. Build key decoding and styling in Norn. Child PTYs are already available in `norn.process`.
- [ ] **HTTP client streaming and multipart forms (`norn.http`).** Expose a streamed response body with explicit close and connection reuse semantics. Add multipart form encoding and parsing. The server already streams responses.
- [ ] **Property-based testing (`norn.test`).** Add generators, seeds that reproduce failures, and shrinking. Start with round-trip properties for the data codecs.
- [ ] **TAR archives (`norn.tar`).** Read and write archive entries over Norn streams. Reuse `norn.compress` for gzip support and use a streaming compression API for large `.tar.gz` archives when available.

## Language and runtime gaps for review

Review stack safety first. Settle the core API changes before sealing `norn.core`.

- [x] **Make structural operations safe on deep values.** Printing, equality, hashing and `compare` walk values with explicit stacks instead of native recursion, so a vector, list or map nested a million deep (built in code, past the reader's limit) prints, compares and hashes. Hash values are unchanged. Regressions in `core_test.norn`. [Printer](../crates/norn-rt/src/print.rs), [equality and hashing](../crates/norn-rt/src/equal.rs)
- [ ] **Let library-defined collections implement core operations.** Define how classes participate in `seq`, `count`, and `nth` while preserving the fast paths for built-in collections. Use `norn.buffer/Buffer` as the first implementation. Today its elements are available through `norn.buffer/nth` and `norn.buffer/count`. [Sequence implementation](../crates/norn-rt/src/seq.rs)
- [ ] **Add owned mutable packed buffers.** Provide a way to update pixels and numeric samples in place, with an immutable `Bytes` result for passing to other fibers or I/O. Use Norn's existing ownership model to define mutation and handoff. [Ownership model](reference/objects.md#construction-and-access)
- [ ] **Decide how library-defined numbers participate in core arithmetic.** Use `Decimal` to settle arithmetic dispatch, mixed-type coercion, and division and rounding. Check numeric equality and ordering against the existing structural equality and hashing rules before changing the operators. [Numeric dispatch](../crates/norn-vm/src/numeric.rs)

### Additional sealing blockers

- [x] **Keep contract checking consistent across cold and warm module loads.** The contract mode is part of the module cache key, so code expanded under `:none` never loads under `:boundaries`. Tested against one cache under both modes (`cache_test.norn`). [Module cache](../lib/compiler/expander.norn)
- [x] **Preserve hash-key lookup across live class redefinition.** An instance hashes by its slots that don't hold their class's default, in any order, so adding a defaulted slot or reordering slots keeps existing keys findable. Equality is unchanged: an old instance reads a new slot's default, so it equals a new instance with that value. Changing a slot's default changes the hash of instances holding the old or new default. Tested in `clos_test.norn`. [Equality and hashing](../crates/norn-rt/src/equal.rs)
- [x] **Detect circular module loads across fibers.** Each load records the fiber running it, and a fiber about to wait for another's load follows the chain of waits; reaching itself is a circular require, reported as an error instead of waiting forever. Tested with both loads started concurrently (`module_test.norn`). [Module loader](../lib/core/core.norn)
- [x] **Make mixed numeric ordering consistent.** A float compares with an integer or ratio by its exact value, so `compare`, `sort`, `<` and `==` are transitive across categories, and sorted collections can't find `2^53 + 1` through the float `2^53`. `(== 1/3 0.3333333333333333)` is now false. Tested in `core_test.norn`. [Numeric comparisons](../crates/norn-vm/src/numeric.rs)

### Other review items

- [x] **Propagate early termination through `cat`.** `cat` keeps the `reduced` signal through its inner reduction, so `(comp cat (take 1))` stops after the first group, and `mapcat` stops on an infinite input. Tested in `core_test.norn`. [Transducer implementation](../lib/core/core.norn)
- [x] **Define export removal on module reload.** `require :reload` removes the module's own names its source no longer defines. Code already compiled against a removed var keeps calling it; new code can't refer to it. The exports a module's current source defines (what `ns-publics` shows after its last load) are its baseline for sealing and API compatibility checks. Tested in `module_test.norn`. [Module loader](../lib/core/core.norn)
- [x] **Drive a UI event loop from Norn code.** `:main-thread` foreign functions run on the process's first thread under stock `norn` (see the main-thread lane in [runtime.md](runtime.md)). The program owns its loop and pumps events with main-thread calls: `norn-sdl3` opens windows, polls events, and shuts down on the main thread with no native helper, and `norn-raylib` works the same way. A toolkit whose own loop never returns (winit's `run_app`, `NSApplication run`) would still need a pump-style entry point. [Main-thread lane](../crates/norn-vm/src/main_lane.rs)

Module sealing and API compatibility checks are tracked in the [M8](plan-m8.md) and [M9](plan-m9.md) plans.

## App packaging

- [ ] **Add `norn package` on top of `norn build`.** Build a runnable `.nimg` from the project and bundle the exact Norn runtime that built it. Use `norn.edn` for the existing `:main` and `:version` and add package metadata for the app ID, display name, icon, and resources. Build from resolved dependencies and include any native helpers. [Build tooling](tooling.md#images)
- [ ] **Add an app launcher.** Resolve the image, assets, and helpers relative to the installed app. Preserve the caller's working directory and pass every argument to `:main`, including a leading `-e`, which `norn --image` currently handles as a runtime flag. For Norn UI, point `NORN_UI_HOST` at the bundled renderer. [Launcher implementation](../crates/norn/src/lib.rs)
- [ ] **Produce release archives.** Ship a CLI archive per OS and architecture with an app-named launcher. Start desktop packaging with a macOS Apple Silicon `.app` in a ZIP, including the launcher, runtime, image, app resources, and `norn-ui-host`. [macOS bundle layout](https://developer.apple.com/documentation/bundleresources/placing-content-in-a-bundle)
- [ ] **Package Mandelbrot first.** Move the `.app` away from its checkout and launch it from Finder without a separate Norn installation. Confirm it renders, quits cleanly, and shuts down its renderer. Keep headless startup and CLI argument forwarding as automated checks.
- [ ] **Prepare public macOS releases.** Sign the app and bundled executables with Developer ID, notarize the finished distribution, and verify that the downloaded copy opens. [Apple notarization guide](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution)

## Linux

- [ ] **Unblock Colima startup.** The 2026-11-15 attempt stopped before Docker could start. Colima’s default profile reported `Broken`, and `colima start` could not write `~/.colima/default/colima.yaml` (`operation not permitted`). An isolated profile using writable temporary directories could not resolve `github.com` to download the Ubuntu 24.04 ARM64 VM image. The test session also received `operation not permitted` when binding loopback TCP and Unix sockets.
- [ ] **Run the Linux ARM64 Docker checks.** Use the working tree in a disposable container and run `cargo build --locked --release`, `cargo test --locked`, and `target/release/norn check`. The blocked run produced no Linux build or test results.
