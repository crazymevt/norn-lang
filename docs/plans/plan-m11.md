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
- the longest pause under 20 ms for a young collection and 60 ms for a
  full one (decision 7; it was 20 ms for both; now 148 ms). Time paused
  under 2% (now 7.7%) is M11b's (decision 9);
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

- **Part 3: the write barrier, and the generational plan behind a switch
  (2026-11-18). Its done-check is met:** on the generational plan in a
  build that moves objects, every suite passes with the heap walked
  after every collection (nothing found), and at tier 0, tier 1 and
  with everything compiled, a collection forced every 50th poll; the
  lang-tests and 300,000 fibers' ping-pong too. Part 2's own mode still
  passes, and the default build is as it was (every suite, `skein
  check`, the lang-tests, `cargo test`).
  - **The barrier** is MMTk's own (`heap::barrier`): an old object carries
    a bit, and the first store into it since the last collection clears
    the bit and puts the object in the storing thread's list. It is in
    `Value::set_field`, after each of the twelve atomic stores, and where
    an object old from the start is filled by a raw copy (a proto's
    constants; the image loader). It's off unless the plan has
    generations: one load and a branch.
  - **Which mutator** (the thing plan-m11 said to settle): a thread's
    own, found through a thread-local set when it makes its `Mutator`;
    for a thread that stores into the heap without one, a shared one
    behind a lock. No thread needs to know which it is.
  - **A fiber's stack** has no barrier on its frames. The collector looks
    through frames as part of their *fiber* (a segment scanned by itself
    is only its link to the next), so it's the fiber that's remembered:
    as `run_fiber` takes it up, and again after each stop for a
    collection (`reload`), since a collection forgets what it was told.
    (Remembering the segment, the first try, lost a frame's values at
    the next young collection.)
  - **The switch:** `SKEIN_GC_PLAN=sticky` is StickyImmix. In a build
    that moves objects its young survivors are copied, which is plan A.
  - **What MMTk 0.33 doesn't do, found by losing the whole heap at the
    first collection:** with generations, objects in the non-moving
    space count as old from the start (a young collection doesn't look
    through that space), so each must carry the bit from birth or no
    store into it is ever noticed. MMTk sets it for its immortal and
    large-object spaces and not for an Immix space used as the
    non-moving one. `alloc_words_in` sets it.
  - **Asking for a collection:** `(gc)` means all of the heap
    (`force_full_heap_collection`), or a cleanup waiting on an old
    object never ran; the stress mode asks for an ordinary one, which
    is usually young, the kind a missed barrier shows in. (MMTk refuses
    an "internal" request outright unless a plan is concurrent.)
  - **Weak references in a young collection:** the non-moving space
    isn't looked through, so an object there is alive whatever its mark
    says (`weak.rs` `reachable`): a weak reference made since the last
    full collection was otherwise dropped, and a referent there cleared
    while alive.
  - **The check** (part 3's done-check): a build with `gc-check` walks
    everything reachable after every collection and stops at the first
    reference to something that isn't an object, naming the kind that
    holds it, the word, and whether the holder was remembered
    (`verify_heap`). `(gc-stats)` has `:remembered`.
  - **A cost to come back to:** every object made in the non-moving
    space is remembered at its first store, so each new fiber, waiter
    and claim goes through the barrier's slow path once. 17,000 objects
    are remembered by the time skein.core has loaded.

- **Part 4: measured (2026-11-18). Two of the done-check's four numbers
  hold; the two about pauses don't.** Generations are still asked for
  (`SKEIN_GC_PLAN=sticky`, in a build that moves objects), not the
  default.
  - **What was wrong first.** 40 of 46 collections were full ones:
    MMTk's nursery is a quarter of the heap at the least, and a heap
    grown by what's live never leaves a quarter free. The nursery is now
    a fixed size (`SKEIN_GC_NURSERY`, 256 MB; `NurserySize::Bounded`).
  - **Two sizes, not one** (`grow_heap`). After every collection the
    heap's limit is what's in use and a nursery's room over it, so young
    collections come as the nursery fills. What's in use after young
    collections has a budget, set by each full collection as the heap's
    size always was (what's in use and as much again as is live); the
    young collection that spends it is followed at once by a full one.
    At once, not a nursery later: a full collection marks what's young
    where it lies, and a nursery of new objects each alone on its block
    is the fragmentation again.
  - **The numbers** (`bench/stash.skn`, 10 carriers, 50 clients, 197 MB
    live, a 256 MB nursery; 150 s for the heap, 40 s for the rest):

    | | wanted | generations | before (Immix, objects moved) |
    |---|---|---|---|
    | pages in use after a full collection | level, ≤ 3× live | level from 60 s at 430–440 MB, 2.2× | 1.8 GB at the end, peak 2.7 GB |
    | the longest pause | < 20 ms | full collections 33–56 ms; young ones 4–7 ms | 160 ms |
    | time paused | < 2% | 3.3–3.7% | 6% |
    | requests a second | held | 65,900 beside 60,800 in the same sitting | |

    The highest the heap gets between collections is 775–815 MB (4×): a
    nursery on top of what's old. With a 64 MB nursery the peak is
    615 MB and the time paused 5.8%; with 512 MB, 1.1 GB and 2.5%.
    With 1, 2 and 4 carriers: 22,800, 38,000 and 45,600 requests a
    second, the longest pause 29, 35 and 46 ms.
  - **Young collections** take about 2 ms and 1 ms more for each 100 MB
    of nursery (2.3 ms at 64 MB, 4.3 at 256, 7 at 512), whatever
    survives. `SKEIN_GC_LOG=2` now says where a pause went: stopping
    the carriers 0.1 ms, listing 16,000 roots 0.1 ms, weak references
    0.01 ms. The rest is MMTk's own work, not looked into yet (its
    `work_packet_stats`). The slow young collections (20–100 ms) are
    all while the keys are loaded, when everything new survives and is
    copied.
  - **Full collections** grow with what's old: 33 ms at 330 MB in use,
    55 ms at 440 MB. They come every 4 s. Nothing here shortens them:
    this is the question decision 4 put off.
  - **Tried and taken back: waiters, claims and atoms as data** (young,
    and moved). Every suite on the moving build crashed, a blocking
    thread in `try_claim` with a claim that had moved; with them where
    they were, the numbers are the same. Decision 3 stands.
  - **Checked**: the moving build with generations under
    `SKEIN_GC_STRESS=50` at tier 0, tier 1 and everything compiled,
    with a 4 MB nursery, lang-tests, and the heap walked after every
    collection (`gc-check`); `SKEIN_GC_MOVE=stress` without
    generations; the default build's suites at both tiers and
    `cargo test`. On macOS only: the branch has not run on Linux.
    `.github/workflows/nightly.yml` has a `moving` job for these modes
    (not yet run).
  - **The nursery is 256 MB by default** (the user, 2026-11-18): over
    64 MB it's a third less time paused for 180 MB more at the peak.
  - **Open**: full collections' pauses; what
    MMTk does in a young collection's fixed 2 ms; making generations
    the default.

- **Part 5: begun (2026-11-18).**
  - **The longest pauses weren't collections.** A collection waits for
    every thread that reads the heap, and tier 1's compile thread was
    one for the whole of a compile: 40 and 80 ms for a large function,
    at start-up and as the clients start. It now is one only while it
    reads the function; while Cranelift generates the code (most of the
    time, and nothing of the heap's) a collection goes ahead, the proto
    in hand a root (`jit.rs` `COMPILING`). Stash's longest pause: 87–112
    ms before, 44 ms after, which is a full collection; no collection
    waits 1 ms to stop. Checked with compiles in the background
    (`SKEIN_JIT_THRESHOLD=1`) under the stress modes and `gc-check`.
  - **Where a collection's work goes** (a build with `gc-packets`:
    MMTk's own timing, between a benchmark's two `(gc)`s). Stash, 40 s:
    tracing and copying 72%, sweeping 21% (all of the heap, at young
    collections too), the remembered objects 4%, the roots, weak
    references and the rest under 1%. A young collection is what
    survives it and a sweep that grows with the heap, both MMTk's.
  - **Fibers aren't rescanned while parked**, at young collections:
    generations did it. A fiber stays put, so it's old from birth and a
    young collection looks through it only if it was remembered, which
    it is when it next runs (`run_fiber`'s barrier). Full collections
    look through every fiber, as they must.
  - **A larger budget for what's old doesn't reach 2%.** Twice as much
    (`SKEIN_HEAP_GROWTH=200`): 2.7% of the time paused, the peak 980 MB
    where it was 790. With a 512 MB nursery as well: 2.2%, 1.27 GB.
    The defaults stay; under 2% is M11b's, with the pauses (decision 9).
  - **The overwrite loop** (`bench/overwrite.skn`: 200,000 values each
    written 20 times, 30 MB alive), the heap's pages over what's alive
    after a collection at the end: 5.6× in the default build, 3.8× in
    the moving build without generations, **1.3× with them**. It was
    5.9× with them at first: `(gc)` went straight to a full collection,
    which marks what's young where it lies. `(gc)` now collects the
    young first (`heap.rs` `collect`), as the heap's own full
    collections do. The most the heap comes to while writing is 375 MB
    with generations (a nursery) and 240 without.
  - **`bench/compare.skn`**, the generational build beside the default
    one in one sitting (80 cases, both tiers): the times' geometric mean
    is 0.99 of the default's at each tier. One case is slower at both:
    assoc on a 20-key map, by 17% and 23% (85 ns to 104 at tier 1); not
    looked into. Nothing else is outside what two runs differ by.
  - **`(gc)` and the carrier it's called on** (found writing the loop):
    the collection comes at that carrier's next poll, not before `(gc)`
    returns. A fiber that reads `(gc-stats)` at once, or sleeps and
    wakes on another carrier, sees the heap as it was.
  - **assoc on a 20-key map** was the barrier's check, 40 times over:
    each store into the node being copied asked whether that node was
    old, and it was made a moment before. `Value::init_field` stores
    with no barrier into data just made (`map.rs`, `colls.rs`'s sorted
    nodes). At tier 1: 104 ns to 56 with generations, 85 to 75 in the
    default build, which paid for part of the check.
  - **`(gc)` has collected when it returns.** The fiber yields and its
    carrier collects before queueing it again (`sched.rs` `run_one`);
    where a fiber can't yield it's still the next poll. That found a
    bug older than M11: **what a carrier spawns between fibers started
    in the last fiber's dynamic environment** (`vm.dyn_env`, left as it
    was when the fiber stopped): a collected object's cleanup, or a root
    fiber. Wrong bindings always, and after a collection that moved or
    freed that map, a pointer to nothing: the generational suite crashed
    one run in three once `(gc)` collected there, and `gc-check` named
    the fiber's field. The carrier's is now nil between fibers, and a
    cleanup starts with none wherever it's spawned (a test in
    `conc_test.skn`, which fails without).
  - **A full collection when the program is idle** (decision 10): the
    last carrier to go idle collects all of the heap if half the budget
    for what's old is spent (`heap.rs` `idle_collection_due`;
    `SKEIN_GC_IDLE=0` for never). Bursts of 150,000 writes to a
    million-key map, 150 ms apart, 320 MB alive: the longest a burst
    stalls is 22–24 ms where it was 32–34, and 10 bursts of 80 stall
    over 20 ms where 20 did. There are 20 full collections where there
    were 13: coming at half the budget, they come more often. A program
    that's never idle is as it was.
  - **Generations are the default** (decision 11): a build moves
    objects unless it's built with the `non-moving` feature, and its
    plan is StickyImmix unless `SKEIN_GC_PLAN` says otherwise (`immix`:
    no generations). CI runs every suite again with collections forced
    on each push; the nightly's `collector` job runs the stress modes,
    a small nursery, Immix with everything moved, the build with the
    heap checked, and the non-moving build. The FFI was looked at
    first: skein-sdl3's tests and asteroids, and the rogue demo on
    skein-raylib (its check and its smoke test), pass in each mode, and
    each game ran in a window through 250 collections; `ffi_test.skn`
    has a collection during a callback, on the calling carrier and in
    a fiber serving one.
  - **Left**: register liveness (§4.5's imprecision): to do, or to
    move to todo.md and close M11 without (not decided). (Linux: a quick look in a container passed, both
    builds; CI's nightly `moving` job is the whole of it.)

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
7. **The longest pause is two numbers** (2026-11-18, after part 4's):
   a young collection under 20 ms, a full one under 60 ms. Full
   collections take 33–56 ms on stash and grow with what's old; only
   collecting while the program runs would shorten them, and that isn't
   M11's (decision 4). (Under 2% of the time paused stood until
   decision 9.)
8. **Concurrent collection is a milestone of its own, after M11**
   (2026-11-18; runtime.md §12 has it as M11b, M12 being the
   platforms). It begins by finding out what MMTk's concurrent plan
   can do beside a copied nursery.
9. **Under 2% of the time paused moves to M11b** (2026-11-18, after
   part 5's measurements): it's 3.1% by default, and 2.2% is the least
   a larger nursery and budget reach, at 6.5× what's live. M11's
   done-check has no number for it.
10. **M11b is parked** (2026-11-18): MMTk 0.33's concurrent plan isn't
    generational and moves nothing (runtime.md §4.1), and we won't fork
    mmtk-core or write a plan of our own. It's taken up when an MMTk
    release has a generational plan that marks concurrently. Meanwhile,
    in part 5: a full collection taken early when the program is idle,
    and the budget for what's old said to be the setting it is.
11. **Generations are the default now** (2026-11-18), not after some
    nights of the nightly's job: nobody but the user runs Skein yet.
