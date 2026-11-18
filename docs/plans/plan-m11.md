# Plan: M11 — a collector that gathers what lives

Drafted 2026-11-18, and **its proposals were accepted the same day**
(see Decisions, at the end): objects may move; StickyImmix with the
nursery copied, reached through Immix with defragmentation; the
runtime's own objects stay put; the milestone is the collector alone. runtime.md §12's row:
generational collection (write barriers), opportunistic defragmentation
(the M1 fragmentation finding), concurrent collection; deadlock detection
for fibers nothing can wake; complex numbers as a library type. Done when
there are "bounded pause times and heap overhead under the M1 stress
workloads".

This plan reorders that row around one finding: **the heap's overhead is
the first problem, and it can't be fixed without moving objects.** The
row lists defragmentation second and calls it opportunistic; the
measurements below make it the centre.

## How things are now

- **The collector** is MMTk 0.33's Immix built with `immix_non_moving`
  (`skein-rt/src/heap.rs`): parallel, stop-the-world, and no object ever
  moves. Every collection traces the whole heap.
- **Allocation never collects** (runtime.md §4.3): a full heap asks for a
  collection, which happens at the next safepoint, where every live value
  is in a heap frame. Primitives are leaves and hold raw values freely;
  runs from Rust into Skein are GC-inhibited.
- **The heap grows by its live bytes** (since 2026-11-18; §4.1), not by
  the pages in use.
- **Instruments** (2026-11-18): `(gc-stats)` (pauses, `:heap-bytes`,
  `:live-bytes`), `SKEIN_GC_LOG=1` (a line a collection), `bench/stash.skn`
  with its baseline, `SKEIN_GC_PLAN=marksweep` to compare.

**Measured**, `bench/stash.skn` on a 10-core M-series laptop, 50 clients
over TCP, client and server in one process:

| | 1 carrier | 10 carriers |
| --- | --- | --- |
| Live objects (440,000 keys) | 194 MB | 195 MB |
| Pages in use after 20 s | 1,460 MB | 2,275 MB |
| Requests a second | 22,300 | 73,600 |
| A request: median, 99.9% | 2.3 ms, 56 ms | 0.6 ms, 2.3 ms |
| Collections in 20 s, longest pause | 11, 54 ms | 23, 148 ms |
| Time paused | 2.8% | 7.7% |

Left running, the pages in use keep rising, more slowly each cycle: 4.5 GB
at 150 s, heading for about 7 GB. With 44,000 keys the heap is level
(from 45 s to 180 s) at **715 MB for 25 MB live: 28 times**.

## What the measurements say

1. **Each value written ends up nearly alone on its block.** Between one
   write and the next the server makes 15 KB or so of garbage (requests
   read, replies written). The value written, a few hundred bytes that
   live until the key is written again, sits among it. Nothing moves, so
   its 32 KB block stays in use for its sake. Pages in use ≈ live values ×
   half a block.
2. **No allocator that leaves objects where they were made fixes it.**
   MMTk's mark-sweep plan, which reuses freed cells by size, holds 520 MB
   against Immix's 715, at 20% fewer requests a second. A loop overwriting
   a map's values with no garbage between holds 2× its live data under
   mark-sweep and 4× under Immix.
3. **Pauses are whole-heap traces.** The longest pause is the worst
   request, within 2 ms, at every number of carriers. A pause is about
   60 ms for 200 MB live and grows with the pages in use (95 ms at 4.5 GB).
4. **Ruled out:** a string's size (a 100-character string is 128 bytes,
   under Immix's 256-byte line); the benchmark client's allocations; a
   leak (reads alone, and writes without the network's garbage, hold
   steady); the growth policy (it fed this, and is fixed).

One thing collects both problems: **a young generation whose survivors
are copied.** What survives a collection is moved out from among the
garbage and packed together, and most collections trace only what's new.

## The choice

| | What it is | Fragmentation | Pauses | What it asks of the runtime |
| --- | --- | --- | --- | --- |
| **A. StickyImmix, nursery copied** (proposed) | MMTk's generational Immix: young objects are those allocated since the last collection, in the same space; a young collection copies the survivors it can and leaves pinned ones | Survivors gathered at their first collection; full collections can defragment the old | Young collections trace the young | Write barriers; copying; pinning for what can't move |
| B. GenImmix | A separate copying nursery, Immix behind it | As A, more thoroughly | As A | As A, but **nothing in the nursery can be pinned**: every root must be a slot the collector can rewrite |
| C. Immix with defragmentation, not generational | Today's plan without `immix_non_moving` | Sparse blocks evacuated at full collections | Unchanged: every collection is whole-heap | Copying and pinning, no barriers |
| D. `sticky_immix_non_moving_nursery` | What runtime.md §4.1 names as next | **Unchanged**: survivors stay where they were made | Young collections | Barriers only |

**Proposed: A, reached through C.** C is A without the barriers, so it is
the first half of the work and can be measured by itself. D, the present
plan of record, fixes pauses and leaves the overhead, which the
measurements say is the larger problem. B is the cleanest design, but
Skein's roots are values held in Rust (run queues, timers, the reactor,
linked closures), handed to MMTk as pinning roots (`heap.rs`,
`scan_vm_specific_roots`); B would need each of those to become a slot.

*Read from MMTk's source:* that StickyImmix copies nursery survivors
unless `sticky_immix_non_moving_nursery` is set
(`prefer_copy_on_nursery_gc`), that Immix leaves in place what a pinning
root names, and that every plan has a non-moving space reached with
`AllocationSemantics::NonMoving`. **Checked in parts 1 and 2 (2026-11-18):**
the non-moving space is in use, and with Immix moving everything it can
at every collection the suites pass, the roots' objects where they were.
StickyImmix itself is still only read: its constraints name the object
barrier and a log bit (the binding has the bit's spec), and a young
object reached through a pinning root is traced without moving.

**A thing part 3 has to settle:** MMTk's barrier is a call on the
*mutator* (`object_reference_write_post`), and `set_field` has none in
hand. Its quick check (is the object written to one the collector
remembers?) needs no mutator, only the bit; the slow path, taken once an
object until the next collection, does. Carriers have a mutator each.
Threads that store into heap objects without being carriers (the timer
thread delivering to a waiter, the reactor, the blocking pool, the main
lane) need one too, or a shared list of their own that the binding hands
to a young collection.

## What assumes an object stays put

The design expected objects to move ("the GC moves objects and an address
can't serve as a hash", runtime.md §2.2), and then M1 chose a non-moving
plan so that "nothing in the runtime has to update raw pointers"
(`heap.rs`). So some of it is ready and some was built on that promise.

| What | Where | Ready? | What to do |
| --- | --- | --- | --- |
| References between heap objects | `heap.rs` `scan_object`: slots | Yes: the collector rewrites slots | — |
| Identity hash | `object.rs` `identity_hash`: the header's meta word | Yes | — |
| `%identity-hash` | `prims.rs`: the address | **No** (the compiler's constant index uses it) | Use the header's hash; a cons has no header, so say what it gets |
| Roots held in Rust | `sched.rs` `for_each_root` and the others: values, not slots | Yes under A and C (pinning roots: what they name stays, what that reaches may move); no under B | — |
| The interpreter's frame and code pointers | `vm.rs` `St`: raw pointers into the running segment and the proto's code, kept across safepoint polls | **No** | Allocate stack segments and protos (code, constants) non-moving |
| Addresses in compiled code | `jit.rs`: protos, generic functions, classes, keywords and linked closures as constants in the code | **No** (linked closures are roots already) | Allocate those kinds non-moving |
| Buffers handed to C | `ffi.rs`: `:bytes` is a pointer into the value | Yes while the call is on: its arguments are roots (`root_many`). **No** for a pointer C keeps after the call | Pin on request; say so in the FFI's contract |
| The text position cache | `string.rs` `positions`: keyed by address | Yes: dropped at every collection | — |
| Weak references, cleanups | `weak.rs`: referents by address | **No** | Follow a moved referent to where it went |
| Dispatch caches | `clos.rs`: keyed by the class's header hash, compared by value in a heap array | Yes | — |
| Copying itself | `heap.rs`: `copy`, `copy_to`, `get_reference_when_copied_to` are `unreachable!` | **No** | Write them; choose where the forwarding pointer goes (a cons is two words) |
| Images | `image.rs` | To check: saving walks the heap as it is | A test that saves after objects have moved |
| Values in Rust locals | Primitives are leaves; nested runs are GC-inhibited | Yes | Keep it so: a primitive that could reach a safepoint with a value in hand is a bug the stress mode must catch |

**The rule (decided):** *data moves, the runtime's own objects don't.*
Strings, bytes, numbers, conses, vectors, maps, sets, instances and their
nodes are allocated as now and may move. Stack segments, protos, closures
the JIT has linked, classes, layouts, generic functions, vars, symbols,
keywords, fibers, channels and atoms are allocated non-moving: they are
few, long-lived, and what the interpreter and compiled code point at.

## Write barriers (for A)

A young collection must know every old object that has come to point at
a young one. runtime.md §4.4 already decided the kind (MMTk's
object-remembering barrier) and where: only mutable stores, since
immutable objects are finished before anyone else can see them.

- **Stores that need one:** atoms (`reset!`, `swap!`, CAS), vars (`def`,
  `alter-var-root`), `(:mutable)` instances' slots, transients, arrays
  written in place, channels' buffers and waiters, fibers' and scopes'
  fields, promises, delays and lazy streams as they're realized, dispatch
  and other caches, namespaces' tables. About 240 `set_field` and
  `set_word` calls across `skein-rt` and `skein-vm` to sort into "into a
  new object" and "into one that may be old".
- **Stack frames need none:** a fiber's stack is scanned with the fiber.
  Scanning every parked fiber at every young collection is a cost of its
  own (the M1 workload parks a million); a fiber that hasn't run since
  the last collection needn't be scanned again. Part 5.
- **Tier 1** writes registers into heap frames (no barrier) and calls out
  for everything else, so the barrier is in Rust, not in generated code,
  to begin with.

**Where the stores are (looked through 2026-11-18).** Fewer places than
the 240 calls suggest, because nearly every store goes through one
function:

- **`Value::set_field`** (`object.rs`), which `arr_set` and the rest
  call. A barrier inside it covers every store there is, the first ones
  into a new object too. The check for those is one bit read (is the
  object one the collector remembers?), so the safe way is the cheap one
  to begin with; a constructor on a hot path can be given a store without
  the check afterwards, each one a measured choice.
- **Twelve atomic stores of a value**, which don't go through it: a var's
  root (`func.rs:62`), an atom's `reset!` and compare-and-set
  (`conc.rs:266`, `271`), a class's and a generic function's fields and
  state (`clos.rs:131`, `393`, `512`, `513`, `637`), `%set-slot-typed`
  (`prims.rs:2192`), a fiber's field set from another thread
  (`sched.rs:56`), and an image's stand-ins (`image.rs:142`, `143`).
- **Raw writes** (`copy_nonoverlapping`, `ptr::write`) are all in
  constructors, into an object nobody else has yet: none needs one.
- **Stack frames** are written by the interpreter and by compiled code
  with no barrier, and a segment is an old object like any other once it
  has survived a collection. So a fiber's stack has to be given to a
  young collection whenever the fiber has run since the last one: one
  call as a fiber is taken up to run (`sched.rs` `run_one`), which
  remembers its segments. A fiber parked since before the last
  collection is then left alone, which is also what the million parked
  fibers of the M1 workload need.

## Parts

| # | Part | Done when |
| --- | --- | --- |
| 0 | **Instruments and a baseline.** `gc-stats`, the GC log, `bench/stash.skn`. Add the overwrite loop as a small benchmark, and a stress mode that moves everything it can at every collection | Done but for the last two |
| 1 | **Say what doesn't move.** The kinds in the rule above allocated non-moving, under today's plan; `%identity-hash` from the header; weak references ready to follow a move | Every suite passes; nothing measurable changes |
| 2 | **Copying.** The binding's copy functions and forwarding; build without `immix_non_moving`; the three things read from MMTk's source checked. Immix defragmenting at full collections (C) | Every suite, the lang-tests and the image tests pass with a move forced at every collection, under `SKEIN_GC_STRESS` and `gc-check`; stash's pages in use measured |
| 3 | **Write barriers.** Each mutable store sorted and barriered; a checking mode that compares a young collection's result with a full one's | The checking mode finds nothing across every suite at both tiers |
| 4 | **Generational** (A): StickyImmix with the nursery copied | The done-check below |
| 5 | **Tuning:** the nursery's size and the growth policy again; fibers not rescanned while parked; register liveness (the imprecision of §4.5) | Measured, each kept only if it pays |

Concurrent collection, deadlock detection and complex numbers aren't
parts: see Decisions.

**Done-check (decided)**, `bench/stash.skn` against its baseline, 10
carriers:

- pages in use level, at no more than 3× the live bytes, after 150 s
  (now about 23× and rising);
- the longest pause under 20 ms, and time paused under 2% (now 148 ms and
  7.7%);
- requests a second no lower than the baseline's;
- the overwrite loop under 2× its live data (now 4×);
- every suite at both tiers, the lang-tests and the image tests passing
  in the moving stress mode;
- `bench/compare.skn`: nothing slower.

## Status

- **Part 1: built (2026-11-18), on today's plan.**
  - `Kind::stays_put` (`skein-rt/src/object.rs`) is the rule in one
    place: symbols, keywords, vars, code, protos, closure templates,
    primitives, stack segments, fibers, channels, waiters, claims,
    scopes, atoms, readers, classes, generic functions, weak references,
    handles and regexes are allocated with MMTk's `NonMoving` semantics
    (`Mutator::alloc_words_in`); everything else as before. The collector
    log shows the second space (`SKEIN_GC_LOG=2`: `nonmoving`).
  - A proto's constants are an `Array`, which is data, but the
    interpreter reads them through a pointer it keeps (`St.consts`):
    `proto_new` takes its own copy where the collector leaves it
    (`array_staying`).
  - The image loader allocates each object where its kind belongs, and
    looks through the protos first for their constants.
  - `%identity-hash` is the header's hash, not the address. A cons has no
    header and still gives its address: the compiler's constant index and
    the expander's tables look up with `identical?`, so after a move they
    miss at worst (a constant not shared).
  - A weak reference follows its referent to where a collection moved it
    (`weak.rs`: the collector doesn't trace a referent, so it doesn't
    rewrite it).
  - **Not yet non-moving, to settle in part 2:** closures (tier 1 writes
    a linked closure's address into code, but those are roots already,
    and so pinned); the arrays a class or a generic function's dispatch
    cache holds (reached through slots, not through kept pointers).
  - **A cost to watch:** waiters and claims are made at every park, and
    now share a space with the long-lived. If that space fragments as the
    whole heap did, they move back to the data side.

- **Part 2: objects move, behind a build switch (2026-11-18).**
  - **The switch.** The crates' default feature `non-moving` is MMTk's
    `immix_non_moving`: today's collector, unchanged. `cargo build
    --no-default-features` builds one that moves data
    (`heap::moves_objects`). Two builds, because MMTk fixes it when it's
    compiled.
  - **Copying** (`heap.rs`, `ObjectModel::copy`): an object's words as
    they are, header and identity hash with them; MMTk leaves the new
    address over the old copy's first word.
  - **A stress mode**, `SKEIN_GC_MOVE=stress`: every collection
    defragments and every block is one to move objects out of. With
    `SKEIN_GC_STRESS` a pointer kept where the collector can't rewrite it
    fails within the suite it's in. `(gc-stats)` has `:moved`.
  - **What moving found**, each fixed:
    - the five functions unwinding knows by address (`FRAME_KINDS`, a
      static) weren't roots: once one moved, an error called whatever was
      at its old address. They're roots now, and so stay put;
    - the carrier's own copy of the running fiber's dynamic environment
      (`Vm.dyn_env`) was stale after a collection: `save` puts it in the
      fiber before a stop, and `reload` now reads it back after one, at a
      poll and on the way back from native code (a C call, and a callback
      from C). The second was found only with every suite running at
      once, from macOS's crash reports: both crashes were a dynamic
      variable's lookup;
    - a collector thread that moves objects maps memory for them, deeper
      than Rust's 2 MB stack: 16 MB.
  - **Passing with objects moved at every collection** (and one forced
    every 50th poll): every suite at tier 0, three runs, and once more in
    a build with the heap's checks on (`gc-check`: every pointer the
    collector follows must be a live object); and `cargo test`'s whole
    workspace, the image tests among them, on the moving build. **Part
    2's done-check is met** but for the one timing test below.
  - **Measured**, `bench/stash.skn`, 10 carriers, 40 s, 195 MB live:
    moving everything at every collection the heap ends at 211 MB
    (1.1× its live data, where it was 14×) and peaks at 345: the
    fragmentation is gone. And requests fall from 77,000 a second to
    27,000, with 27 s of the 40 paused: every collection copies all that
    lives. That is what a young generation is for (part 4).
  - **MMTk's own choice of when to defragment does nothing here**, and
    why (read from `policy/immix/defrag.rs`): a collection defragments
    only in an emergency, on a second attempt, when asked for by the
    program, or when the holes in used blocks weren't all refilled before
    it. Skein over-commits (so no emergencies), and small objects refill
    the holes first, so none of those holds. `immix_always_defrag`
    (`SKEIN_GC_MOVE=always`) makes every collection one; it then moves
    out of the blocks with the most holes first, as far as its to-space
    goes, which is the headroom (`SKEIN_GC_HEADROOM`, 2% of the heap)
    plus the holes of blocks it isn't emptying. A block with fewer than
    two holes is never emptied (`MIN_SPILL_THRESHOLD`), and a block with
    one survivor has one or two. **Measured: nowhere.** With every
    collection defragmenting, at 2% headroom or 10%, stash's heap climbs
    as it does with nothing moved (2.8 GB at 40 s). Immix's
    defragmentation is for blocks riddled with holes; a store's blocks
    are nearly empty ones with a survivor each. So C, moving without
    generations, has no setting between "moves nothing that matters" and
    "copies everything": it was the way to A, as planned, and isn't a
    place to stop.
  - **Also passing** on the moving build under the stress mode: every
    suite with everything compiled from its first call, the lang-tests,
    and 300,000 fibers' ping-pong. At tier 1 as it starts, one check
    fails in both runs, prof_test's count of samples: the profiler's
    timing under a collection every 50th poll, the test plan-m10.md
    already lists as one that depends on the machine.

## Risks

- **A value in a Rust local across a safepoint.** Today it's harmless if
  the object is otherwise reachable; once objects move it's a stale
  pointer. The rule (primitives are leaves, nested runs GC-inhibited)
  says there are none; part 2's stress mode is what proves it. This is
  the likeliest source of bugs, and they'll be rare ones.
- **A missed barrier** loses an object only at a young collection, and
  only sometimes. Part 3's checking mode exists for this.
- **Nested runs** are GC-inhibited, so a long one (a slow `print-object`
  method, a macro) holds every collection off. Unchanged by this plan,
  but a small nursery fills sooner.
- **MMTk's StickyImmix** is younger than its Immix, and Skein allocates
  with over-commit and never blocks. Part 2 tries C, which uses only
  Immix, before A depends on it.
- **Throughput.** Barriers cost a little on every mutable store, and
  copying costs what it copies. The done-check holds requests a second at
  the baseline; if A can't, C alone is the fallback.

## Decisions (the user, 2026-11-18)

1. **Objects may move.** This reverses M1's "objects never move", which
   the interpreter, tier 1 and the FFI were written against. The
   measurements say the heap can't be kept near its live data otherwise.
2. **A (StickyImmix, nursery copied), through C**, over B, C alone or D.
3. **Data moves; the runtime's own objects don't**, atoms and channels
   among those that don't: they are few and long-lived, and it leaves
   fewer places for a stale pointer.
4. **Concurrent collection is out of M11**, to be decided after part 4's
   numbers: with young collections short it may not be needed. It stays
   an open question in runtime.md §13.
5. **The done-check's numbers**: pages in use level at 3× the live bytes
   or less, the longest pause under 20 ms, under 2% of the time paused,
   requests a second held.
6. **Deadlock detection and complex numbers move out of M11** (to
   todo.md): the milestone is the collector alone.
