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

- [ ] **Make structural operations safe on deep values.** M9 records native stack overflow in printing and equality on deeply nested values built in code. Use a bounded or iterative traversal and add regressions for values that bypass the reader's nesting limit. Review hashing for the same traversal issue. [M9 status](plan-m9.md#status)
- [ ] **Let library-defined collections implement core operations.** Define how classes participate in `seq`, `count`, and `nth` while preserving the fast paths for built-in collections. Use `norn.buffer/Buffer` as the first implementation. Today its elements are available through `norn.buffer/nth` and `norn.buffer/count`. [Sequence implementation](../crates/norn-rt/src/seq.rs)
- [ ] **Add owned mutable packed buffers.** Provide a way to update pixels and numeric samples in place, with an immutable `Bytes` result for passing to other fibers or I/O. Use Norn's existing ownership model to define mutation and handoff. [Ownership model](reference/objects.md#construction-and-access)
- [ ] **Decide how library-defined numbers participate in core arithmetic.** Use `Decimal` to settle arithmetic dispatch, mixed-type coercion, and division and rounding. Check numeric equality and ordering against the existing structural equality and hashing rules before changing the operators. [Numeric dispatch](../crates/norn-vm/src/numeric.rs)

### Additional sealing blockers

- [ ] **Keep contract checking consistent across cold and warm module loads.** A module cached with `*check-contracts* :none` can still skip checks when loaded under `:boundaries`. Include the contract mode in the cache key or enforce it independently of expansion. Test loads under both modes against the same cache. [Module cache](../lib/compiler/expander.norn)
- [ ] **Preserve hash-key lookup across live class redefinition.** Adding a defaulted slot can change an existing instance's hash, making it unreachable in a map or set. Define the equality and hashing rules when a class's compared slots change. Test keys inserted before redefinition. [Equality and hashing](../crates/norn-rt/src/equal.rs)
- [ ] **Detect circular module loads across fibers.** Two fibers loading opposite ends of a dependency cycle can wait on each other indefinitely. Make the loader report a circular require and add a regression that starts both loads concurrently. [Module loader](../lib/core/core.norn)
- [ ] **Make mixed numeric ordering consistent.** At `2^53`, two distinct integers each compare equal to the same float while comparing unequal to each other. Define exact-number comparisons against floats and test `compare`, `sort`, and sorted collection lookup at precision boundaries. [Numeric comparisons](../crates/norn-vm/src/numeric.rs)

### Other review items

- [ ] **Propagate early termination through `cat`.** `(comp cat (take 1))` still visits input groups after producing its result. Preserve the `reduced` signal through the inner reduction and test that later groups are untouched. [Transducer implementation](../lib/core/core.norn)
- [ ] **Define export removal on module reload.** `require :reload` leaves deleted definitions in `ns-publics`. Specify what happens to existing references and which exports form the sealing and API compatibility baseline. [Module loader](../lib/core/core.norn)
- [ ] **Drive a UI event loop from Norn code.** `:main-thread` foreign functions now run on the process's first thread under stock `norn` (see the main-thread lane in [runtime.md](runtime.md)). Still to define: who owns a long-running event loop on that thread, and how shutdown works when windows close. Use a minimal SDL3 app that opens a window, pumps events, and shuts down on the main thread as the acceptance test. [Main-thread lane](../crates/norn-vm/src/main_lane.rs)

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
