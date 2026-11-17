# Performance survey before sealing (2026-11-15)

Sealing (M8) freezes each standard-library module's public interface.
After that, a change that breaks an interface needs a major version and a
deprecation period, but an implementation change is still free. The
optimising compiler (M10) and the generational collector (M11) come after
sealing, and M10 needs sealed modules for cross-module inlining and direct
linking.

This survey looks for slowness that only an interface change could fix,
so those changes can land before sealing. It sorts every finding into two
groups:

- **A: settle before sealing.** The fix changes something callers can
  see.
- **B: fixable later.** The fix is in the implementation, so it can come
  in a minor release, in M10, or in M11.

## Method

The two benchmark files are new:

- `bench/micro.norn` times core conventions. Each case runs 1,000
  operations in a loop, and the loop alone is the first row.
- `bench/workloads.norn` times whole jobs. Each row is the best run in
  about 2 seconds. The suite is the baseline for M10's done-check
  ("measured wins on `bench/`").

Both ran at tier 0 (the interpreter, the default) and tier 1 (Cranelift,
`NORN_TIER=1`) on an Apple M4 with 10 cores. Run them yourself with:

```sh
norn bench/micro.norn
NORN_TIER=1 norn bench/workloads.norn
```

## Results

### Conventions (ns per operation)

| Case | Tier 0 | Tier 1 | Notes |
| --- | ---: | ---: | --- |
| Loop alone | 35 | 5 | |
| Call, one argument | 54 | 18 | |
| Call, rest arguments (two given) | 73 | 35 | |
| Call, keyword options (none given) | 133 | 73 | builds a map |
| Call, keyword options (two given) | 356 | 279 | |
| Call, an options map | 191 | 123 | |
| Call into another module, plain | 74 | 26 | |
| Same, with a checked `^Integer` signature | **4,084** | **3,476** | B1 |
| Generic function, one method | 111 | 69 | |
| Generic function, `:before` and `:after` | 161 | 114 | |
| Slot read `(:x p)` | 52 | 14 | |
| Making an instance | **2,219** | **1,773** | B4; a map is 67–109 |
| Owned mutable `update-slot!` | 247 | 159 | |
| Dynamic var read | 36 | 4 | |
| `binding` a dynamic var | 563 | 394 | B3 |
| `count` of a library collection (Buffer) | 760 | 664 | B5; a vector is 13–52 |
| `nth` of a library collection (Buffer) | **2,957** | **2,069** | B5 |
| `nth` of a non-ASCII string | 60 | 21 | |
| `mapv inc`, per item | 152 | 91 | B7 |
| `into` with a transducer, per item | 235 | 163 | B7 |
| `swap!` / `vswap!` | 187 / 241 | 114 / 158 | `vswap!` is slower than `swap!` |
| `try` with no error | **1,580** | **1,240** | B3 |
| Error signalled and caught | 5,216 | 4,341 | |
| Fixnum `+` and `<` | 54 | 5 | |
| Float `*` | 74 | 33 | boxed |

### Workloads (ms per run)

| Workload | Tier 0 | Tier 1 | Notes |
| --- | ---: | ---: | --- |
| JSON: write / read 1 MB | 1.6 / 4.1 | 2.0 / 5.0 | fine |
| EDN: write / read 1 MB | 1.8 / 6.0 | 2.4 / 7.7 | fine |
| `norn.bin`: encode / decode | 1.3 / 3.8 | 1.6 / 4.3 | fine |
| Read `core.norn`'s forms (158 KB) | 2.1 | 2.5 | fine |
| Format `http.norn` (34 KB) | 13 | 14 | |
| Lint `http.norn` (34 KB) | **243** | **307** | B8 |
| Compile and load `http.norn` | 40 | 56 | |
| 1,000 inserts into a 1.2 MB rope | 100 | 108 | B2 |
| 20,000 line lookups in a rope | **2,422** | **2,197** | B2 |
| Regex, every match in 1.2 MB | 19 | 16 | fine |
| Build 100,000 pieces with `str` | 36 | 33 | |
| Read 20,000 lines from a file | **1,629** | **1,708** | A1 and B2 |
| Sort 100,000 integers | 52 | 50 | |
| `group-by` and `frequencies`, 100,000 | 395 | 357 | B6 (GC) |
| `into` a map, 100,000 entries | **535** | **533** | B6 (GC) |
| SQLite: insert 10,000 rows | 123 | 104 | |
| HTTP: 200 local requests | 40 | 29 | |
| Spawn and await 10,000 fibers | 21 | 4.7 | fine |
| 10,000 messages through a channel | 1.6 | 0.6 | fine |
| Decode 70 KB of terminal keys | 116 | 82 | B9 |

Tier 1 speeds up calls and arithmetic by 3–10 times, but it barely
changes the outliers. They're spent in Rust primitives, in algorithms, or
in the collector, not in interpreted code.

## A: settle before sealing

1. **The line and text readers expose their insides.**
   `norn.io/line-reader` and `text-reader` return plain maps
   (`{:handle h :buf (atom …)}`). The line reader's buffering has to
   change (see B2), and after sealing its keys would be part of the
   interface. Make both readers opaque objects now. They could also be
   streams themselves, through `stream-read`. Nothing reaches into them
   today: not the standard library, Norn Studio or norn-ui.
2. **Whether signatures are checked in release builds.** A checked
   signature costs about 3.5 µs per call from another module (B1). The
   cost is an implementation problem, but whether release builds check
   at all changes behaviour (a `ContractError` raised, or not). The
   recommendation is to keep checking on by default, as Racket does,
   because the blame is the point. Make the checks cheap (B1), and keep
   the contract mode `:none` as an opt-out for a build. Write the decision
   down before sealing.

Nothing else needs an interface change:

- **Keyword options** cost about 300 ns because the callee builds a map.
  The compiler can parse them into locals instead. The convention can
  stay.
- **The collection protocol** is slow (B5), but its shape is fine. A
  faster hook, such as a `collection-reduce` generic with a default
  through `collection-seq`, can be added later as a minor change.
- **The `declare` and `^Type` syntax** that M10 will read is enough. M10
  can unbox floats by inference, without new hints.
- **Conveying bindings and handlers to spawned fibers** costs nothing in
  itself. The slowness is in how `binding` is built (B3).

## B: fixable later, by impact

1. **Checking signatures (about 3.5 µs a call).** On every call,
   `%wrap-for` builds the contract's var name as a string and interns a
   symbol to look it up. `%check-call` formats an "its 1st argument"
   message for each argument, failure or not, maps the arguments into a
   vector, and calls through `apply`. Cache the parts on the var, build
   messages only on failure, and check classes with `instance?` directly.
   Calls should then take tens of nanoseconds. Worth doing soon, since the
   standard library's public functions are meant to carry signatures.
2. **Quadratic buffering, and the rope.**
   - `read-line` copies the rest of its 64 KB buffer for every line, about
     81 µs a line. Keep an offset into the buffer instead. The HTTP
     connection reader and the streamed response body do the same thing.
   - The rope's `slice`, and so `line`, splits the rope twice and rebuilds
     about half the tree (77 µs). It should walk the chunks in range.
     `insert` splits and rejoins too (100 µs). The Studio editor relies on
     both.
   - These are bugs more than tuning, and quick to fix.
3. **`try` and `binding` (1.2 µs and 400 ns).** `try` expands to a
   handler-case. That makes a catch tag, a closure for each handler and a
   vector, then binds `*handlers*` (a bind-map and a dynamic frame), sets
   up a catch frame and builds an `[:ok value]` result. Cheaper dynamic
   frames, and handler-case without the closures and result vector, would
   cut most of it. Many library paths use `try`.
4. **Making an instance (1.8 µs against 67 ns for a map).** Every instance
   goes through the `initialize-instance` generic function with keyword
   arguments. A class with no methods of its own on it could take a
   cached fast path.
5. **The collection protocol (660 ns to 2 µs).** Each operation on a
   library collection hands off to `%collection-op` and its `case`, then
   to a generic dispatch. Dispatching the generic function straight from
   the primitive would remove the hand-off.
6. **The collector.** A collection costs about 7–8 ms, because Immix with
   no generations marks the whole live heap, the standard library
   included. `into {}` with 100,000 entries triggers 45 collections.
   Allocation-heavy work is dominated by this. Generational collection
   (M11) is the fix.
7. **Sequence functions (90–160 ns per item at tier 1).** `mapv`,
   `into` with transducers and the like. This is M10's territory:
   inlining the step functions and fusing loops.
8. **Lint (243 ms for 34 KB).** Twenty times slower than the formatter.
   Worth a profile, since the editor lints as you type.
9. **Smaller items:** key decoding at 1.6 µs a byte; boxed floats (33
   ns a multiply at tier 1); `vswap!` slower than `swap!`; `str` at 80–120
   ns for two strings.

## Done after the survey (2026-11-15)

A1, A2, B1 and B2 are done:

| | Before | After |
| --- | ---: | ---: |
| Checked call from another module, tier 0 | 4.1 µs | 0.79 µs |
| Same, tier 1 | 3.5 µs | 0.58 µs |
| Read 20,000 lines from a file | 1,629 ms | 22 ms |
| 20,000 line lookups in a rope | 2,422 ms | 309 ms |
| 1,000 inserts into a rope | 100 ms | 26 ms |

- **A1:** the line and text readers are opaque objects.
- **A2:** release builds check signatures, as recorded in
  [design §8.4](design.md#84-types-decided).

The workload run also turned up a lost interrupt, a scheduler race. Two
carriers took a root task and its cancel at the same time, and the cancel
looked for the fiber before it was registered. A cancel that arrives
early is now kept and applied when the fiber is registered.

## Recommendations

- Before sealing: do A1, and record A2's decision in the design doc.
- Soon, because they're cheap and affect everyday use: B1 (signatures)
  and B2 (buffering and the rope).
- With M10: B3–B5 and B7. The workload suite is the baseline to measure
  against.
- With M11: B6.
