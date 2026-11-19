# Skein Runtime Architecture (draft 0.1)

Companion to [design.md](design.md). This document covers *how* the Rust
runtime delivers the language's promises: fibers that can be parked and
debugged, fibers saved in images, live redefinition on all cores, and
multicore throughput.

Decisions are marked **[decided]**, **[proposed]** or **[open]**, as in the
design doc.

---

## 0. The three load-bearing decisions

Most of this document follows from these three choices:

1. **Skein frames live in heap-allocated, segmented stacks — not on the
   native stack** (§3). A fiber's stack is ordinary heap data. That makes
   fiber switches cheap, makes stacks visible to the GC precisely, and lets
   the debugger inspect them and images save them.
2. **Rust never calls Skein** (§2.3). Runtime primitives are leaves. Anything
   that needs to call user code (sorting with a comparator, `print-object`,
   reader macros) is written in Skein. This is what makes rule 1 possible:
   there are never Rust frames sandwiched between Skein frames.
3. **One frame format for interpreted and compiled code** (§3.3). The
   bytecode interpreter and Cranelift-compiled code push the same frames, so
   stacks can mix both, the debugger handles them identically, and
   deoptimisation on redefinition is just "resume this frame in the
   interpreter".

---

## 1. Layers

```
┌─────────────────────────────────────────────────────────────────────┐
│ Skein code: stdlib, CLOS/MOP, conditions, compiler, REPL server      │  lib/*.skn
├─────────────────────────────────────────────────────────────────────┤
│ Execution:  bytecode interpreter (tier 0) │ Cranelift JIT/AOT (tier 1)│  skein-vm, skein-jit
├─────────────────────────────────────────────────────────────────────┤
│ Runtime core: values · object model · persistent collections        │
│   fibers & stacks · scheduler · reactor · sync primitives           │  skein-rt
│   image save/load · FFI                                             │
├─────────────────────────────────────────────────────────────────────┤
│ MMTk (GC)   Cranelift (codegen)   crossbeam-deque   mio             │  dependencies
└─────────────────────────────────────────────────────────────────────┘
```

Tier-1 targets: `aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`,
`aarch64-unknown-linux-gnu`. Windows comes later.

---

## 2. Values and objects

### 2.1 Value representation **[decided]**

A Skein value is one 64-bit word, `#[repr(transparent)] struct Value(u64)`,
with low-bit tagging (as in SBCL), not NaN-boxing: fast fixnums matter more
for a Lisp than unboxed floats do, and the JIT can unbox floats locally.

```
...xxxxxxx0   fixnum (63-bit). Tag 0, so add/sub/compare need no untagging
...pppp001    pointer to heap object (8-byte aligned)
...pppp011    pointer to cons cell   (fast car/cdr; code is still lists)
...ssss101    immediate: sub-tag in bits 3..7 → nil, true, false, char,
                unbound-marker, eof, small-float (reserved) …
...xxxx111    reserved
```

`class-of` is a three-way branch: fixnum → `Integer`, immediate → table
lookup by sub-tag, pointer → `header.layout.class`.

### 2.2 Heap object layout **[decided]**

```
 word 0   layout*      → Layout (class version: class, slot map, GC map, flags)
 word 1   meta         identity-hash:32 | flags:16 | gc:8 | spare:8
 word 2…  slots        tagged values (or raw bytes for strings/byte-vectors)
```

Conses are the exception: they have no header, just two words (`car`,
`cdr`), and the tag identifies them.

- **Layouts are the "hidden class"** (shape) of an object. Redefining a class
  creates a new layout and marks the old one *obsolete*. Compiled slot access
  uses inline caches keyed on the layout pointer, so an obsolete layout simply
  misses the cache, and the slow path migrates the instance (design §4.4).
  The fast path needs no separate version check.
- **Identity hash** lives in the header and is assigned lazily, because the
  GC moves objects and an address can't serve as a hash.
- **Mutable instances** (`(:mutable)` classes) carry an extra hidden slot,
  `owner`, holding a fiber id. Immutable instances pay nothing for it.

### 2.3 "Rust never calls Skein" **[decided]**

Rust primitives (`vector-conj`, `string-concat`, `atom-cas`, …) are leaf
functions: they may allocate but never invoke Skein code. Consequences:

- **Equality and hashing are structural and runtime-defined**, so the HAMT in
  Rust can implement `assoc`/`get` without calling back into user code. That
  also guarantees map operations can't deadlock or run user side effects in
  parallel folds. Custom key semantics use a key function. **[decided,
  2026-11-13]**: classes may declare `(:equality :identity)` or leave slots
  out with `:compare false`, which the Rust side reads from the class (design
  §3.4).
- **Sorted collections, `sort` with a comparator, `print-object`, reader
  macros and anything else higher-order are written in Skein.**
- **FFI callbacks** are the single exception: they are *continuation
  barriers*. While native code is on the stack, the fiber is pinned to its
  carrier and can't be captured, saved or migrated. Racket's CS backend has
  the same rule.

### 2.4 Collections **[built in M7]**

| In Rust (no user callbacks needed) | In Skein |
|---|---|
| Persistent vector (RRB), hash map/set (CHAMP), string, byte-vector, cons | Lazy streams, transducers, `sort` |
| Sorted map/set (persistent AVL, natural `compare` order), persistent queue, transients (with owner-fiber check) | Ropes (`skein.rope`), everything built on the above |

Rust gives them speed while everything runs on the bytecode interpreter.
Once the native compiler is good, moving more into Skein is an option.

**Status (M7 part 4).**
- `Sorted` (`[count][root][set?]`) and `SNode` (`[key][val][left][right][height]`)
  heap kinds hold sorted maps and sets: a persistent AVL tree ordered by
  `compare`. They are maps and sets for `=`, `hash`, `get`, `assoc`,
  `conj`, `seq` and the rest; a sorted map equals a hash map with the same
  entries. `subseq` and `rsubseq` give ranges. Keys that `compare` can't
  order are an error at insertion.
- `Queue` (`[count][front list][rear vector]`) is a FIFO: `conj` adds at the
  back, `peek` and `pop` work at the front. It prints as `#queue [..]`.
  Its class is `Queue`, a `Sequential`, so it is `sequential?` and `=`
  to a list or vector of the same items.
- `Transient` (`[kind][owner][count][data][done]`) is one fiber's builder
  for a vector, map or set. Use from another fiber is an `OwnershipError`;
  use after `persistent!` is an error. `get`, `nth`, `contains?` and
  `count` read one, as Clojure lets them. Transient vectors fill an array in
  place; transient maps and sets have the full API but still copy per
  change (in place is later work).
- Ropes are a Skein library: a height-balanced tree of string chunks
  (at most 512 characters), each branch caching its length and newline
  count, so edits and line lookups are O(log n).


### 2.5 The numeric tower **[built in M7]**

`skein-rt/src/numbers.rs` (representation) and `skein-vm/src/numeric.rs`
(arithmetic, using `num-bigint` and `num-rational`).

- **Fixnums:** 63-bit, unboxed. Arithmetic that overflows them promotes to
  a bignum instead of failing.
- **`BigInt`:** a heap kind, `[sign | limb count][64-bit limbs]`.
- **`Ratio`:** `[numerator][denominator]`, each a fixnum or a bignum.
- **Floats:** boxed `f64`.
- **Always normalised**, which is what keeps `=` and `hash` structural:
  - a bignum never holds a value that fits a fixnum;
  - a ratio is in lowest terms with a denominator above 1, otherwise it is
    an integer.
- **Mixed operations** promote to the wider type: fixnum, then bignum,
  then ratio, then float. Dividing integers gives a ratio unless it divides
  exactly.
- **Comparison:** `<` and `==` are exact for integers and ratios. With a
  float they compare as floats, and are false when either side is NaN.
  `compare` and `sort` put NaN after every other number.
- **Tier 1** keeps its fixnum fast paths. On overflow it bails out to the
  interpreter, whose primitive promotes.
---

### 2.6 Text **[built in M7]**

- **Strings index by code point.** `count`, `nth`, `subs` and every index
  in `skein.string` and `skein.regex` count characters (Unicode scalar
  values), never bytes; the Rust side converts at the boundary (text.rs),
  with a fast path for ASCII. For other strings, each thread caches the
  character count and the last position found for its last few strings
  (`skein-rt` string.rs), and counts from the nearest known position, so a
  pass through a string costs what its text does. An entry lasts until the
  next collection, since only a collection frees memory for reuse and
  objects never move. What a person sees as one character can be
  several code points, so `skein.string` adds `graphemes`, `grapheme-count`
  and `width` (terminal and editor columns; `unicode-segmentation` and
  `unicode-width`). `reverse`, `truncate` and the padding functions work in
  graphemes and columns.
- **Regexes** use the `regex` crate: linear-time matching, no
  backreferences or lookaround. `#"…"` reads a regex (backslashes as
  written; `\"` is a quote). A regex is a heap kind holding its pattern
  string, so it prints as written, compares and hashes by pattern, and
  saves in images. Compiled regexes live in a per-carrier cache keyed by
  pattern (512 entries, then cleared), so the hot path takes no lock.
- **JSON** is read and written by a small hand-written Rust parser and
  printer (no serde): objects become hash maps with keyword keys (strings
  with `:keys identity`), integers of any size become fixnums or
  bignums. Writing turns ratios into floats and refuses NaN, the
  infinities and values with no JSON form. An unrealized stream is
  realized first (§5.4.1).
- **`format`** (in core) takes printf-style directives, implemented in
  Rust; widths and precisions for `%s` count display columns.


## 3. Stacks, frames and fibers

### 3.1 Segmented heap stacks **[decided]**

This follows Chez Scheme's design. Each fiber's stack is a chain of **stack
segments**, which are ordinary heap objects (class `StackSegment`).

```
 Fiber ──► current segment (1 KiB initial, doubling to 256 KiB)
            ┌───────────────────────────────────┐
            │ header: capacity, prev segment,   │──► previous segment ─► …
            │         prev fp, depth, live top  │
            │ frame: fn, proto, resume, prev    │
            │        registers …                │
            │ frame …                           │ ◄─ top
            └───────────────────────────────────┘
```

- **Overflow:** when a frame doesn't fit, a new segment (double the size, up
  to 32K words) is linked to the previous one through its header. Deep
  recursion is bounded only by a per-fiber limit (32M words), which raises
  `Stack overflow`.
- **Live top:** each segment records its topmost live frame whenever a GC
  could run (at safepoints, at parks, and when a newer segment is pushed). The
  GC scans exactly the live words, never stale frames above the top.
- **Fiber switch** means saving the segment and frame offset into the old
  fiber and loading the next fiber's. There are no registers to save between
  Skein calls (§3.2) and no assembly context switching.
- **Memory (measured, M1):** a parked fiber costs about 1.7 KB in total
  (fiber object, first segment, closure, waiter). 1M parked fibers use
  1.73 GB of heap. A fiber gets its first segment when it starts running.

### 3.2 Calls compile to "push frame + tail call" **[decided]**

The native stack never holds Skein values across a Skein call.

- **Non-tail call:** spill live values into the current Skein frame, record the
  *resume point*, then jump to the callee (Cranelift's `tail` calling
  convention with `return_call`).
- **Return:** the callee jumps to the caller's frame's resume point.
- **Tail call:** reuse the frame and jump. TCO is guaranteed.

Cost: returns are indirect jumps, so the CPU's return-address predictor
doesn't help (estimate 10–30% on call-heavy code). Mitigation: inlining,
plus a later **direct-call fast path** that uses real native calls into known
leaf functions which contain no safepoint.

What this buys: precise GC roots with no Cranelift stack maps, continuation
capture by splitting a segment, and stacks that are data — the debugger and
image saver walk them like any other object.

### 3.3 Frame format **[decided — implemented in M0]**

```
 +0   fn        closure being executed (upvalues, self-reference)
 +8   proto     the arity being executed (→ code, constants, debug info);
                frame size = 4 + proto.nregs
 +16  resume    resume point: bytecode pc (tier 0) or continuation index (tier 1)
 +24  prev      caller's frame as an *offset* within this segment (fixnum),
                -1 = caller is in the previous segment, -2 = return to Rust
 +32  slots…    tagged values; raw slots (unboxed f64) only in tier-1 frames,
                described by the resume point's GC map
```

Frames are linked by offsets, never addresses, so a moving GC (or an image
loader) can relocate a segment without rewriting frames. Every header word is
a tagged value, so the GC scans frames with no side tables. The result
register of a call is not stored: on return, the VM decodes it from the CALL
instruction just before the caller's resume point (as Lua does).

The same layout serves both tiers. Each resume point has a descriptor (live
slots, raw slots, source location, which variable lives where), which drives
GC scanning, the debugger's "locals in frame", and **deoptimisation**: when a
sealed module is unsealed or an inlined function is redefined, affected
tier-1 frames get their `resume` rewritten to the equivalent bytecode pc.

### 3.4 Delimited continuations **[decided]**

The kernel provides **one-shot** delimited control: `%prompt` and
`%capture`. With segmented stacks, capturing splits the segment at the
prompt (O(1)), and resuming relinks it. Fibers, generators, `await`, effect
handlers and the condition system's non-local exits are all built on this
in Skein.

Multi-shot continuations (copy on reinstatement) are **[open]**; nothing in
the design needs them yet.

### 3.5 Kernel language **[proposed]**

All of Skein macro-expands to roughly 14 forms, which both tiers implement:

```
quote  if  do  let*  letrec*  fn*  def  var
%prim                      ; call a Rust primitive (leaf)
%prompt  %capture          ; delimited control
%unwind-protect            ; cleanup frames (run on unwind / abort)
%dynamic-bind              ; push fiber-local binding frame
%safepoint                 ; explicit poll (compiler inserts the rest)
```

Locals are immutable: there is no `set!` on locals, so closures capture
values rather than boxes.

---

## 4. Memory management

### 4.1 GC: MMTk **[decided]**

- **M1: MMTk 0.33, `Immix` plan built with `immix_non_moving`**
  (`skein-rt/src/heap.rs`): parallel (one GC worker per core), stop-the-world,
  and objects never move, so the interpreter's cached code and frame pointers
  stay valid across a GC. The heap is 64 MB to 8 GB by default
  (`SKEIN_HEAP_MIN`, `SKEIN_HEAP_MAX`); after each collection it may grow by
  what's live times `SKEIN_HEAP_GROWTH` percent (100 by default: a
  collection when the program has taken as much new memory as it had live
  data, as Go's `GOGC=100`), a trigger of our own (`ProportionalTrigger`,
  MMTk's delegated trigger). **What's live is the bytes of live objects
  (since 2026-11-18), not the pages in use after the collection**: objects
  don't move, so a page is in use while anything on it lives, and sized by
  pages the heap fed itself (more room spread a store's survivors over more
  pages, which counted as more live data). stash's benchmark, 200 MB of
  live data on 1.3 GB of pages after loading, reached `SKEIN_HEAP_MAX` in
  40 s with pauses of half a second; sized by live bytes it holds 4.5 GB
  after 150 s, its longest pause 133 ms. It still rises (towards about 7
  GB): that is the fragmentation itself, M11's to fix. `SKEIN_GC_LOG=1`
  prints a line for each collection (its pause, the pages in use before
  and after, the live bytes, the next limit; `=2` adds each space), and
  `(gc-stats)` gives a program the pauses, the pages in use and the bytes
  live on them. `SKEIN_GC_PLAN=marksweep`
  runs MMTk's free-list plan instead, to compare. MMTk's dynamic
  size (MemBalancer, still there as `SKEIN_HEAP_GROWTH=balancer`) sized
  the room above live data by how fast collections trace against how fast
  the program allocates: Skein traces fast and allocates a lot, so it left
  a few MB, and a program with 65 MB live collected every few MB it
  allocated (linting `http.skn` beside the workloads' data: about 100
  collections and 560 ms a run; now 4 and 124 ms, peak RSS 137 → 233 MB).
  Small programs stay within the minimum (`skein -e nil`: 79 MB either
  way).
- **M11 ([plan-m11.md](../plans/plan-m11.md), decided 2026-11-18; the
  default since that day): objects may move.** The default build's plan
  is MMTk's `StickyImmix` with the young copied: young collections as a
  256 MB nursery fills (`SKEIN_GC_NURSERY`), a full one when what's old
  has grown by as much as is live (`SKEIN_HEAP_GROWTH`) or, sooner, when
  the program is idle (`SKEIN_GC_IDLE=0` for never). On stash the heap
  is 2.2× what's alive where it was 30×, young collections stop the
  program 4–7 ms and full ones 33–56. `SKEIN_GC_PLAN=immix` is Immix
  with no generations; a build with the `non-moving` feature (`cargo
  build -p skein --features non-moving`) is the collector of M1 to M10,
  and CI's nightly keeps it working. How it was decided: generational `StickyImmix` with its nursery copied,
  so that what survives is gathered out from among the garbage, reached
  through Immix with defragmentation. (It was to be
  `sticky_immix_non_moving_nursery`: that shortens pauses and leaves the
  heap at thirty times its live data.) It needs write barriers on stores
  into possibly-old objects (var roots, atoms, fiber, channel and scope
  fields; §4.4), and the runtime's own objects kept where they are. Stack
  frames don't need barriers, because frames are scanned through their
  fiber at every GC.
- **After M11: concurrent collection** (its own milestone, M11b; the
  user, 2026-11-18). **Parked the same day: not through a fork of
  mmtk-core or a plan of our own (the user). It waits for an MMTk
  release with a plan that is generational and marks concurrently:
  look at `src/plan` at each upgrade.** Until then a full collection's
  pause grows with what's old (about 55 ms for 440 MB in use on an M1
  Max's ten cores, so some hundreds of ms for a few GB), and what
  there is to set is how often they come: `SKEIN_HEAP_GROWTH=200`
  halves it for a quarter more memory at the peak, and a full
  collection is taken early when the program is idle (M11 part 5). M11's full collections stop the program 33–56 ms
  on stash and grow with what's old, and only collecting while the
  program runs shortens them. Through MMTk's concurrent plan or our
  own. **[open: which]** What MMTk 0.33 has, read from its source
  (2026-11-18, `src/plan/concurrent`):
  - **One plan, `ConcurrentImmix`, and it isn't generational.** One
    Immix space; a cycle is a pause to start (the roots), marking while
    the program runs, and a pause to finish. StickyImmix and GenImmix
    are other plans and don't mark concurrently: it's a copied nursery
    or concurrent marking, not both.
  - **A concurrent cycle moves nothing** ("strictly non-moving"). Only
    its stop-the-world full collection defragments, and that's a pause
    like M11's full ones. On stash that is the fragmentation back
    between full collections (30× what's alive when nothing moved).
  - **It needs a second barrier**: what a store overwrites
    (`object_reference_write_pre`, snapshot-at-the-beginning), and one
    on reading a weak reference's referent. M11's barrier is the other
    kind (after the store, the object written to).
  - **Fibers' stacks** are written with no barrier, so either every
    fiber is looked through in the starting pause (a million parked
    fibers is a long one) or each is looked through before it next
    runs.
  - **It's young**: its own comments list what isn't safe yet (a full
    collection can't take over from a cycle in hand; the finishing
    pause comes only at a poll).
  So MMTk's plan alone gives up what M11 gained, and the alternatives
  are a plan of our own in a fork of mmtk-core (StickyImmix's nursery
  with its old objects marked concurrently) or waiting for MMTk to
  have one.
- **One MMTk mutator per carrier thread**, not per fiber. Fibers allocate from
  their carrier's thread-local buffer.
- **Conses carry a one-word header** (`[layout][car][cdr]`, 24 bytes), because
  MMTk reads every object's size from the object itself. Their pointer tag
  still identifies them without a memory load.

### 4.2 Roots and safepoints **[decided]**

Skein values live only in the heap (stack frames included), so the roots that
live *outside* the heap are few (implemented in M1):

- Runtime tables: interned symbols and keywords, namespaces → vars, the
  shared empty collections.
- Scheduler state: every carrier's run queue, the global queue, timers, root
  fibers submitted by the host, and each carrier's *running* fiber.
- Nothing else. A parked fiber is reachable from whatever will wake it
  (a channel, a timer, the fiber it awaits); a parked fiber that nothing can
  wake is garbage and is collected.

A fiber's stack is scanned when the GC traces the fiber object: it walks the
segment chain and visits each segment's live words.

The **safepoint poll** sits at function entry (after the new frame is fully
written) and at loop back-edges. Its fast path is a budget decrement plus one
load of a global *attention* counter, compared with the value the carrier last
saw. Anything that needs a busy carrier's attention bumps that counter:

| Slow path taken because… | Action |
|---|---|
| reduction budget (2000) used up | yield, if other fibers are waiting and the slice has run 0.5 ms |
| stop-the-world requested | save stack top, block until the GC finishes |
| `(gc)` or `SKEIN_GC_STRESS` | save stack top, request a collection, block |
| fiber cancelled / interrupted | raise `Cancelled` / `Interrupted` |
| work submitted from outside (REPL, cancel) | handle it |

The **safepoint protocol** (`skein-rt/src/safepoint.rs`) counts *running*
participants: carriers executing Skein code, and the timer thread while it
fires timers. Stopping the world waits until none is running. Idle carriers
count as safe.

### 4.3 Rooting in Rust **[decided — revised in M0]**

**Allocation never triggers a collection.** If allocation runs out of room it
takes more memory and sets the poll word, and the collection then happens at
the next safepoint, where every live Skein value is in a heap stack frame.

This removes almost all rooting from Rust. Primitives are leaves (§2.3), so
they contain no safepoints and can hold raw `Value`s across allocation
freely. Handles (`Handle<'scope>`, as in V8's HandleScope) are only needed by
the little Rust code that runs Skein code *and* keeps values across it. In M0
that is only the seed compiler holding forms while it calls a macro.

- **Risk:** a primitive that allocates enormously between safepoints
  (`(vec (range 1e9))`) over-commits the heap. That's acceptable: it's bounded
  by the size of the result, which has to be built anyway.
- **Verified in M1:** MMTk allocates with
  `AllocationOptions { allow_overcommit: true, at_safepoint: false }`. A full
  heap makes it *request* a collection, and the collection then starts
  through our `stop_all_mutators`, which waits for every carrier to reach a
  safepoint.
- **Nested runs are GC-inhibited.** Rust code that re-enters Skein (the seed
  compiler expanding macros, boot) holds values the GC can't see. While such a
  run is active, the carrier's polls don't stop for a GC, which simply waits
  for the run to end. The seed compiler is replaced in M4.
- **GC stress mode:** `SKEIN_GC_STRESS=N` forces a collection at every N-th
  slow-path poll of every carrier. With N=1, the core suite completed
  2,032,983 collections with 0 failures.

### 4.4 Barriers and publication **[decided]**

- **Generational write barrier** (MMTk object-remembering) only on mutable
  stores: atoms, vars, mutable slots, transients, GC caches. Immutable objects
  are initialised before they are published, so they need no barrier.
- **Safe publication:** every way of sharing a value between fibers (atom
  CAS, var set, channel put, `spawn` argument passing) is a release/acquire
  edge. Immutable data therefore needs no further synchronisation. The only
  places a data race can occur are `volatile` and owner-checked mutable
  objects with checks compiled out, and both are explicit opt-ins.
- **Pinning** for objects handed to FFI uses Immix's pinning support.

### 4.5 Weak references and cleanups **[built in M6]**

`crates/skein-rt/src/weak.rs`.

- **`(weak-ref x)`** makes a `Weak` object whose referent the GC doesn't
  trace. `(weak-get w)` returns the referent, or nil once it's been
  collected.
- **`(add-cleanup! x f)`** runs `(f)` in a fiber of its own after `x` has
  been collected; `(cancel-cleanup! w)` drops a pending cleanup. This follows
  Java's `Cleaner` and JavaScript's `FinalizationRegistry`, not finalizers:
  the dead object is never handed back, so nothing is resurrected. `f` must
  not capture `x`, or `x` stays reachable through `f`.
- **How it works.**
  - Every `Weak` is in a registry.
  - After each collection's transitive closure, MMTk calls the binding's
    `process_weak_refs`, which clears dead referents and moves their
    cleanups to a ready list.
  - A `Weak` with a pending cleanup is itself a root, so its cleanup
    function stays alive. Due cleanups are roots too, until a carrier spawns
    them (in `process_external`).
- **Images** save weak references, but a referent only if something else
  saved reaches it. Cleanups are never saved: they release per-process
  resources, such as an FFI handle that must never be freed by another
  process.
- **Imprecision.** Frames are scanned exactly, but registers aren't checked
  for liveness. A callee's frame overlaps the top of its caller's register
  window, so a value a finished call left there stays reachable until that
  register is reused. Weak references therefore clear, and cleanups run,
  *eventually* rather than at the first possible collection. Register
  liveness maps (or clearing on return) were M11's and are in todo.md
  since 2026-11-18 (the user): nothing M11 measured comes from this.

---

## 5. Scheduler and I/O

### 5.1 Carriers **[decided]**

```
 carrier 0..N-1 (one per core)               reactor thread         monitor thread
 ┌──────────────────────────┐               ┌─────────────┐        ┌───────────────┐
 │ local deque (Chase-Lev)  │ ◄─ steal ─►   │ mio: kqueue │        │ detects stuck │
 │ run fiber until poll/park│               │ / epoll     │        │ carriers in   │
 │ timer wheel              │ ◄── wake ───  │ (io_uring   │        │ FFI → spawn   │
 └──────────────────────────┘               │  later)     │        │ spare carrier │
          ▲  global injector queue          └─────────────┘        └───────────────┘
```

- **As built in M1** (`skein-vm/src/sched.rs`):
  - one carrier thread per core (`SKEIN_THREADS` overrides), plus a timer thread;
  - each carrier has a mutex-guarded run queue, and there is one global queue;
  - a carrier takes from its own queue first, then the global one, then
    steals from other carriers;
  - every 61st pick checks the global queue first, as Go does, and fibers
    preempted at the end of a time slice go to the global queue, so a busy
    carrier can't starve fibers woken by the timer thread. (This was a real
    starvation bug at one carrier.)
  - A lock-free Chase-Lev deque is a later optimisation. The run queues are
    GC roots, so a replacement must let the GC enumerate them.
- **Fiber states:** `QUEUED → RUNNING → {PARKED, DONE}`, plus `NOTIFIED` for
  a wake-up that arrives between registering a wait and committing the park.
  The owner commits a park with a CAS `RUNNING → PARKED`; if that fails, the
  fiber continues immediately. There are no lost wake-ups and no locks around
  parking.
- **Claims:** every wait is a heap `Waiter` with a `Claim`. Whoever CASes the
  claim first (a channel partner, a timer, a canceller, a finishing fiber)
  delivers the wake-up. An `alts!` shares one claim across its channels.
- **The reactor, the blocking pool and the monitor thread** were built in
  M6 (§5.5).
- **Preemption** via the reduction budget (§4.2): a fiber yields after
  2000 reductions, if other fibers are waiting, once its slice has run at
  least 0.5 ms. Reductions alone favoured slow code: with tier 1's native
  calls (M10), 2000 of them take about 100 µs, so a busy fiber got a
  fraction of the share of interpreted ones on a loaded carrier.

### 5.2 Dynamic bindings and conveyance **[decided]**

- Each fiber holds a pointer to a **persistent map** of var → value.
  `binding` pushes a new map, and `spawn` copies the pointer (O(1)
  conveyance).
- Each var has a `has-ever-been-dynamically-bound` flag, so reading an
  ordinary global is a single acquire-load of its root.
- Condition handlers and restarts are just dynamic bindings, so they are
  conveyed into children too (design §5.3).
- **As built (M3):**
  - The current dynamic environment is `Vm.dyn_env`, saved in the fiber's
    `DYN` field at every park, yield and safepoint, and copied into a child at
    `spawn`.
  - `GETVAR` consults it only for vars flagged `VAR_DYNBOUND` (set the first
    time a var is bound), which costs one relaxed load for every other var.
  - `binding` is the Skein function `%with-bindings*`, whose frame records the
    environment to restore.

### 5.4 Errors, unwinding and the debugger **[implemented in M3]**

**Frames that mark dynamic extent.** Four small Skein functions in core.skn,
recognised by the runtime by closure identity:

| Frame (its parameters are its registers) | Meaning |
|---|---|
| `%catch* [tag thunk dyn]` | a throw target |
| `%unwind-protect* [body cleanup dyn]` | a cleanup to run if unwound |
| `%with-bindings* [new thunk old]` | the dynamic environment to restore |
| `%finish-unwind [cleanups dyn mode x args]` | completes an unwind (below) |

**Unwinding is stack surgery.** `%throw`, `%restart-frame` and
`%abort-fiber` are interpreter intrinsics:
1. Walk the frames from the top to the target (a catch tag, a given frame, or
   the bottom).
2. Collect each `%unwind-protect*` cleanup passed, with its dynamic
   environment, and note the environment in force at the target.
3. Cut the stack back to the target and replace the target frame with
   `%finish-unwind`.
4. `%finish-unwind` runs the cleanups innermost first, restores the
   environment, then:
   - **`:return`** returns the thrown value from the catch;
   - **`:call`** calls the restarted frame's function (its newest definition)
     with the frame's original arguments;
   - **`:die`** ends the fiber.

Frames are data, so none of this needs native stack unwinding.

**Errors from Rust code** carry a kind:

| Kind | Examples | Handling |
|---|---|---|
| `Runtime` | divide by zero, unbound var, arity, compile error, failed `await` | becomes a call to `%runtime-error` *in place of the failed instruction* (CALL, TAILCALL, GETVAR), returning into that instruction's destination register |
| `Abort` | cancellation, stack overflow | unwinds the whole fiber (running cleanups), then ends it |
| `Fatal` | the condition system already ran; internal errors | ends the fiber immediately |

How a `Runtime` conversion works:
- `%runtime-error` signals a `RuntimeError` with the restarts `use-value` and
  `retry`.
- `use-value` returns its value as the failed operation's value.
- `retry` returns a reserved immediate, `RETRY`. When `do_return` sees it
  coming out of a `%runtime-error` frame, it re-executes the failed
  instruction instead of delivering a value.
- The compiler emits an unreachable `RETURN` after every `TAILCALL`, so a
  *failed* tail call is handled like a failed call: its frame stays on the
  stack, debuggable and restartable.

**Race fixed along the way:** a fiber cancelled between starting a blocking
operation and parking used to sleep forever. See the handshake in `cancel`
(sched.rs).

**The debugger.** It is Skein code on top of all this:
- `%debug-park` registers the fiber, posts a `{:event :debug …}` line on the
  event bus (`sched::subscribe`), and parks on a channel.
- `backtrace` reads a parked fiber's frames, with local names recorded by the
  compiler in each proto.
- `eval-in-frame` evaluates with those locals bound.
- `debug-restart`, `debug-restart-frame` and `debug-abort` send the choice
  down the channel.
- The terminal REPL and the editor server (`server.rs`) are both clients of
  the event bus.

### 5.4.1 Realizing streams **[built in M7]**

Streams (design §3.3) are lazy seqs: `Stream: [state][value]`, holding a
thunk until realized, then the seq it produced. Rust never runs the thunk.
- A seq operation that meets an unrealized stream fails with
  `seq::NEEDS_FORCE`.
- `Vm::call_prim` turns any primitive failure whose arguments hold an
  unrealized stream into `ErrKind::Force(stream, all)`.
  - `first`, `rest`, `next`, `seq`, `nth` and `empty?` ask for one cell.
  - Every other primitive asks for all of it.
  - `=`, `hash` and printing don't fail on unrealized streams, so they ask
    up front.
- `handle_error` calls `skein.core/%force-stream` in place of the failed
  instruction, just as `%runtime-error` is called. When it returns the
  retry marker, the instruction runs again.
- `nth` steps along a chain cell by cell. Bulk iteration (`SeqIter`) first
  checks that the whole chain is realized.

### 5.5 I/O: the reactor, the blocking pool, native regions **[built in M6]**

**The reactor** (`skein-vm/src/reactor.rs`):
- One thread runs `mio` (kqueue or epoll).
- Each open pipe or socket is an entry in a handle table. The Skein `Handle`
  object (a heap kind) holds its number and the process epoch, so a handle
  restored from an image reports "resource lost" instead of reaching
  whatever now has that number.
- **Waiting without lost wake-ups.** Sources are registered edge-triggered,
  and each entry counts readiness events per direction.
  - An I/O primitive that would block returns `:again`.
  - The wrapper then calls `(%io-wait h dir seen)` with the count it read
    *before* trying. If an event arrived meanwhile the count differs, and it
    retries at once.
  - Otherwise it parks on a waiter that the reactor wakes, as the timer
    thread does.
- Waiters parked on the reactor are GC roots.
- Closing a handle drops the source (closing the descriptor) and wakes its
  waiters, which then find it closed.

**The blocking pool** (`blocking.rs`): threads for calls that block in the
OS (files, DNS, blocking FFI), so carriers never do.
- A job's blocking half runs without touching the heap. Its `Finish` turns
  the result into a Skein value on the pool thread. The thread allocates with
  its own mutator, as a running participant, and hands the value to the
  parked fiber.
- The pool grows on demand to 64 threads; idle threads above 2 exit after
  30 s.
- A job can't be interrupted: a cancel takes effect when it ends.

**Native regions and spare carriers** (`sched.rs`):
- `Vm::native_region` saves the stack, roots the values it's given (a
  tail call's arguments live only in a Rust vector), and marks the carrier
  safe for the GC while native code runs. The code it runs must not touch
  the heap.
- **It drops the carrier's cached free segment first** (`Vm::spare`, which
  isn't a root), as every way into a GC-safe state must. A collection
  during the native call would otherwise free it, and the next frames would
  be written over reused memory. (This was M9's intermittent crash; see
  plan-m9.md.)
- The **monitor thread** checks every 10 ms. When carriers have been in a
  native region for more than 20 ms while fibers wait, it starts a spare
  carrier in one of the extra slots (`n..2n`). A spare retires when it's
  idle and enough regular carriers are back. All run queues, including the
  spares', can be stolen from.

**The rule for threads outside the carriers.** A thread may take a heap
value out of a GC root (a waiter out of the reactor's table, a timer's
target out of the timer table) only while it is a running participant.
`sched::delivering` makes it one, so it must be entered *before* the value
leaves the root. Otherwise the thread can wait in `delivering` while a
collection frees what it holds. That bug, in the reactor, corrupted the
heap under GC stress in M6; the fix and this rule came together.
- Such threads allocate with one shared feeder mutator, under a lock, and
  never drop it. A thread-local mutator would be destroyed by the thread's
  exit destructor, outside any safepoint, possibly in the middle of a
  collection.
- Carriers are already running participants, so they never use
  `delivering`: it could wait for a collection that is waiting for them.

**Images.**
- An image save holds off the timer thread, the reactor and the pool
  together (`sched::delivering` takes a shared lock; the save takes it
  exclusively).
- Fibers waiting on I/O or the pool are saved dead, since what they wait for
  doesn't exist in the restored process.

### 5.6 The basic FFI **[built in M6]**

`crates/skein-vm/src/ffi.rs`, with `lib/skein/ffi.skn` (`library`,
`function`, `call`, `defcfun`).

- **Libraries** load with `libloading`. Each foreign function holds the
  library, so it stays loaded while any of its functions exists.
- **Call stubs.** For each signature, Cranelift compiles
  `extern "C" fn(target, args, ret)` once (cached). The stub:
  - loads each argument from an array of words as its C type, extending
    narrow integers as the platform ABI requires;
  - calls the target with the default C calling convention;
  - stores the result.

  Stubs live in the JIT's executable memory (`MAP_JIT` on macOS).
- **Arguments:**
  - integers are range-checked against their C type;
  - floats;
  - `:pointer` (an address as an integer; nil is NULL);
  - `:bytes` (a pointer to a `Bytes` value's data; it never moves);
  - `:string` (a NUL-terminated copy);
  - `:out n` (a zeroed buffer that C fills, returned as `Bytes` after the
    result).

  A `:string` return copies the C string (NULL is nil). 64-bit results that
  don't fit a fixnum are an error, until M7's bignums.
- **Fast or blocking.** A fast call runs in a native region on the carrier:
  the GC may run meanwhile, and the monitor starts a spare carrier if it
  takes over 20 ms while work waits. A `:blocking` function runs on the
  blocking pool. Either way, the `Bytes` it points into are rooted until it
  returns.
- **The main-thread lane.** A `:main-thread` function runs on the thread
  serving the lane (`main_lane.rs`). The `skein` command runs its command
  line on a second thread and serves the lane on the process's first, which
  macOS requires for windows and events and which keeps a graphics context
  on one thread. A program embedding Skein serves it by calling
  `skein_vm::main_lane::serve` on a thread; otherwise a `:main-thread` call
  is an error. As on the blocking pool, the caller's fiber parks while the
  lane runs the job, which doesn't touch the heap, and a caller that can't
  park waits in a native region. Jobs run one at a time in order. A
  `:string` result is copied on the lane, before the next job can reuse a
  static buffer. A callback C makes on the lane goes to the callback's
  serving fiber, and the lane keeps running jobs until it answers, so the
  callback can make main-thread calls itself.
- **Images.** Library and function handles are dead after a restore. The
  Skein objects hold their handle in an atom and reload and re-resolve by
  themselves, with no `ResourceLost`, because a loaded library holds no
  session state.
- **M9 part 9.8 added:**
  - **Memory:** addresses are integers. `%mem-alloc` and `%mem-free`
    (calloc and free), `%mem-get` and `%mem-set!` (a C type at an address
    and offset, unaligned), and C strings and byte copies.
  - **Struct layouts** are Skein (`defcstruct` in `skein.ffi`), on those
    primitives: C's rules, nested structs and arrays. A struct is passed
    by pointer (`[:* layout]`, a map copied to temporary memory) or
    filled by C (`[:out layout]`, read back as a map).
  - **Callbacks.** Each one gets a Cranelift trampoline with its own C
    signature, which stores its arguments as words and calls `dispatch`
    with the callback's id.
    - On the carrier making a fast call (the thread-local `CALLING_VM`
      says which), the callback leaves the native region and runs as a
      nested run, then enters the region again.
    - On any other thread (the blocking pool, or C's own), the call is
      posted through a `Port` to the callback's channel, with a reply
      token. A fiber serving the channel answers with
      `%ffi-callback-reply!`, and the C thread waits on a condition
      variable meanwhile.
    - An error can't unwind through C, so C gets 0. In the first case the
      error is kept and signalled when the fast call returns; in the
      second it is reported.
    - A trampoline's code isn't reclaimed: executable memory isn't
      reused.
  - **Function pointers:** `%ffi-fn` with a nil library takes an address.
  - **Unsafe modules:** the var flag `VAR_UNSAFE` (32) marks `skein.ffi`'s
    raw functions and the `%ffi-`/`%mem-` primitives. The expander refuses
    a reference to one, at phase 0, from a namespace that isn't in
    `%unsafe-namespaces`: those whose `ns` says `(:unsafe)`, and `user`.
- **M9 part 9.8b added:**
  - **Structs by value,** for arm64 (AAPCS64, and Apple's, which agree
    here). `skein.ffi` passes a struct as `[:struct size leaves]`: its
    scalar pieces with their offsets. The runtime classifies it (`Agg`):
    - a homogeneous floating-point aggregate (one to four floats or
      doubles of one type) goes in float registers;
    - anything else up to 16 bytes goes in one or two integer registers,
      as words of its memory;
    - anything larger is passed as a pointer to a copy, and a result that
      size comes back through x8 (Cranelift's `StructReturn`).

    `lower` turns a signature into Cranelift parameters by those rules. A
    struct that doesn't fit the registers left goes on the stack whole:
    filler parameters take the rest of the registers. The stub loads a
    struct's pieces from its copy, and stores a struct result where the
    caller says.
  - **Callbacks** take and return structs by value the same way: the
    trampoline stores the pieces in its stack frame and passes their
    address on; for a struct result, `f` gets first the address to write
    it to.
  - **Variadic functions** (`:...`, last). The extra arguments' types are
    each call's own (64-bit integers, doubles and pointers, C's default
    promotions), and so is the stub, cached by them. On Apple arm64 every
    variadic argument goes on the stack: fillers use up all the
    registers before them. On other arm64 systems they go as normal
    arguments.
  - **Unions** (`defcunion`) and **bit fields** (`[:bits type width]`,
    in units of their type, from the lowest bit, as both ABIs lay them
    out) are Skein.
  - A freed callback's trampoline is reused by the next callback of its
    signature.
  - `tests/skein/ffi_fixture.c`, built by `ffi_test.skn` with `cc`,
    checks every case against C.
  - **x86-64 System V** (M12): each eightbyte of a struct up to 16 bytes
    goes in a float register if it holds only floats, else an integer
    one; a struct with too few registers left, or larger, is copied onto
    the stack (Cranelift's `StructArgument`, whose memcpy call the FFI
    links itself), and later arguments may still take registers. A
    variadic call is made through a thunk that sets `%al` (which
    Cranelift can't) and jumps to the function.
  - **Windows** (M12): on arm64, AAPCS64's rules, but a variadic
    function takes every argument, the named ones too, in integer
    registers and then on the stack. On x86-64, Win64's: a struct of 1,
    2, 4 or 8 bytes goes and comes back as an integer of that size (no
    HFAs), any other by reference to a copy, a result through a pointer
    passed first; a float among the variadic arguments goes in the
    integer register, as its bits.

### 5.7 Native extensions and the main thread **[built in M7.5]**

For native code larger than the FFI suits, the first being `skein-ui`'s
engine (design §9.1). Nothing here is UI-specific.

- **Extensions** (`skein_vm::ext`): a Rust crate registers an `Extension`
  (a name, a namespace it owns, primitives) before the runtime starts.
  - Extension primitives are numbered after the built-in ones and follow
    the same rules (§2.3: Rust leaf functions that never call Skein).
  - Their names are part of the build fingerprint, so an image only loads
    in a binary with the same extensions.
- **Embedded libraries:** an extension can carry Skein libraries (name and
  source), which `require` finds like the built-in ones; `skein-ui` brings
  `skein.ui` this way.
- **Native objects** are `Handle`s owning a `Box<dyn Any + Send>`
  (`ext::handle`, `ext::with_native`). Closing a handle drops the object.
  A handle saved in an image comes back lost, as M6's sockets do.
- **Ports:** a thread outside the carriers posts to a `Port`.
  - The Port keeps its channel rooted, and builds each value inside
    `delivering` with the shared feeder mutator, so §5.5's rule is enforced
    by the API rather than by care.
  - `post` puts only if there is room now; the extension decides whether to
    coalesce, retry or drop.
- **The main thread:** `skein::run_with_main_thread(f)` runs the whole
  command line (REPL, scripts, `-e`, `test`…) on a thread of its own. Once
  the runtime has started, it gives the process's first thread to `f`
  (macOS requires its UI event loop there). The process ends when either
  finishes.
- **Tested** by `skein-ext-demo` (`crates/skein/tests/ext.rs`):
  - 20,000 events posted from the main thread arrive in order, under GC
    stress and at tier 1;
  - native handles work, and close;
  - an image keeps its extension fingerprint, and its handles come back
    lost.

### 5.3 Sync primitives **[M1: atoms, channels, scopes built]**

All shared mutable state is in the heap, guarded by one-word spinlocks inside
the objects (`skein-rt/src/conc.rs`). There are no Rust-side allocations,
finalisers or extra root tables.

| Primitive | Implementation |
|---|---|
| `atom` | `AtomicU64` CAS on a tagged value; `swap!` is a CAS loop written in Skein |
| `var` | root slot (release store / acquire load) + flags |
| `chan` | spinlock + optional ring buffer + FIFO queues of waiters (two lists each) for takers and putters; unbuffered channels hand values over directly. `alts!` locks its channels in address order and parks on all of them with one shared claim. `timeout` is a channel the timer thread closes. |
| scope | children list, pending count, first failure, owner waiter. A failing child cancels its siblings; cancelling the owner cancels the tree. |
| fiber-aware mutex | **not yet** |
| STM `ref` | **not yet** (still open: core or library) |
| dynamic bindings / conveyance (§5.2) | **not yet** (arrive with conditions in M3); a fiber inherits its creator's current namespace |

---

## 6. Generic function dispatch

**[implemented in M2]** — `skein-rt/src/clos.rs` holds the shapes and the hot
path; `lib/core/core.skn` holds the policy.

**Objects.**

```
Class:    [name][supers][cpl][slots][layout][builtin][-][meta][subclasses]
Instance: [class][layout][n][slot0 … slotn-1]          immutable
layout:   Array [k0 … kn-1, d0 … dn-1]                 slot keywords, then defaults
Generic:  [name][methods][state][epoch][nreq][rest][dispatch][combination][lock][meta]
state:    Array [mask, eqls, cache, class-epoch]        immutable, swapped atomically
cache:    Array [nkeys, cap, count, (k0 … kn-1, em) × cap]
```

**Hot path** (in the interpreter; Rust never calls Skein):

1. Load the generic function's `state` (one acquire load).
2. If its class epoch isn't the current global class epoch, it's a miss.
3. For each specialised position in `mask`, the key part is the argument
   itself if it's in that position's `eql` set, otherwise its class.
4. Probe the open-addressed cache (no allocation, no locks).
5. On a hit, the interpreter *continues its call loop* with the cached
   effective method, so the generic function adds no frame of its own.
6. On a miss, it calls `%gf-miss` (Skein) in the same fiber, which computes the
   applicable methods, sorts them by specificity, and builds the effective
   method. The effective method is a fixed-arity function compiled with
   `eval`, with the method functions embedded as constants. `%gf-miss` caches
   it and tail-calls it.

**Correctness under redefinition.**
- `defmethod` replaces the methods with a CAS and swaps in a fresh `state` (empty
  cache) under the generic function's spinlock, then bumps its epoch.
- A miss records the epoch and class epoch *before* reading the methods, and
  installs its result only if neither has changed: a CAS on `state` from the
  snapshot it started from. A stale effective method can therefore never be
  cached.
- `defclass` bumps the global class epoch, which invalidates every cache at
  once (class precedence may have changed anywhere below the class).
- Because mask, `eql` sets, cache and epoch live in one immutable object,
  readers always see a consistent snapshot.

**Methods** take the next-method function as a hidden first argument;
`call-next-method` and `next-method?` are bound only if the body uses them.
Custom-dispatch generic functions compute the dispatch value in Skein and key
the cache on it.

**Measured** (release, tier 0, Apple M-series): a generic call costs about
39 ns more than a plain call (107 vs 68 ns). 8 fibers on 8 cores sustain
about 37.8M generic calls per second, and after a live `defmethod` every one
of them saw the new method within 167 µs.

**Later:** tier-1 call-site inline caches keyed on the same epochs; readers
as generic functions; `define-method-combination`; the full MOP.

---

## 7. Images

### 7.1 Save **[decided]**

1. Bring every carrier to a safepoint (as for GC) and stop the reactor.
2. Mark external resources dead in the snapshot (see design §7.2).
3. Trace from the image's roots and **copy objects into image segments in a
   deterministic order** (breadth-first from the sorted root list). Identity
   hashes are already stored in headers, so hash tables survive without
   rehashing, and build-mode images are byte-for-byte reproducible.
4. Write the segments: `heap` (objects), `code` (tier-1 machine code,
   position-independent), `meta` (roots, layer id, relocations, symbol
   table).

### 7.2 Load **[decided]**

- `mmap` the heap segment at its preferred base address. If the address is
  free (the usual case), no relocation is needed. Otherwise apply the
  relocation table.
- Image objects live in MMTk's **VM space** (a non-collected space for boot
  images). Pages are mapped `MAP_PRIVATE`, so mutation is copy-on-write, and
  the write barrier records image objects that point into the dynamic heap as
  roots.
- Code reaches heap constants through a per-code-object **constant table**
  (relocated with the heap), never through absolute pointers embedded in
  instructions, so code pages stay shareable and unmodified.
- **Startup target: under 50 ms** to the REPL prompt for a base image. This
  is mostly page-fault cost.

### 7.3 Layers **[decided]**

A layer's references into a lower layer are encoded as
`(layer-id, offset)` and resolved at load time. A delta layer contains only
objects that are new or copy-on-write-modified since the layer below it was
loaded. The modified-object list comes from the same dirty-page tracking
that `MAP_PRIVATE` already gives us.

### 7.4 Platform findings — macOS arm64 **[verified in M0: `spikes/jit-spike`]**

Results from `cargo run -p jit-spike --release` on aarch64-macos
(Cranelift 0.137):

| Check | Result |
|---|---|
| Cranelift JIT (W^X via `MAP_JIT`) | ✓ works |
| `tail` calling convention, `return_call` | ✓ 100M self tail calls in constant stack |
| `cranelift-jit` with `is_pic = true` | ✗ panics — on aarch64 it supports only non-PIC code |
| `mmap(PROT_EXEC)` of a code file (zero-copy image code) | ✗ `EPERM` — Apple Silicon allows executable pages only if they are signed or `MAP_JIT` |
| Copy code from a file into `MAP_JIT` memory | ✓ works |

Consequences **[decided]**:

- **Tier 1 uses `cranelift-codegen` directly, with our own code loader,**
  not `cranelift-jit`. We compile position-independent code, and the same
  bytes go into JIT memory now and into image code segments later.
- **Image code segments are copied into `MAP_JIT` memory at load** (one
  memcpy plus relocations) instead of being mapped straight from the file.
  The heap segment, which is the bulk of an image, is still mmap'd
  zero-copy. The <50 ms startup target is unaffected for typical image sizes.
- **Signed distributions:** packaging an image's code as an ad-hoc-signed
  dylib would allow zero-copy mapping on macOS. Revisit in M5 if code
  segments get large.
- Linux allows file-backed `PROT_EXEC` mappings, so it keeps the zero-copy
  path.

### 7.5 As built in M5 **[built]**

`crates/skein-rt/src/image.rs` (format, writer, loader) and
`crates/skein-vm/src/image.rs` (roots, the save, the restore).

**Format.** Little-endian 64-bit words: a header, the base image's path (for
a delta), the object area, a relocation bitmap over the area (one bit per
word: is it a reference?), and the roots. A reference is stored as
`(object number << 3) | tag`, and an object's layout word as its kind
number. Nothing in the file is an address.

**Save.**
1. `save-image` parks the calling fiber and hands the work to a helper
   thread.
2. The helper pauses the scheduler. A carrier counts itself `in_flight`
   before it picks a fiber or handles external jobs, then checks `paused`;
   both are sequentially consistent, so either the carrier sees the pause or
   the helper sees the carrier. Running fibers yield at their next poll,
   whether or not other work is waiting.
3. When nothing is in flight, the helper becomes a running safepoint
   participant (so the GC can't start), holds the timer lock, and walks the
   heap breadth-first from the roots. The walk mostly reuses the GC's `scan`.
   Segments store their live frames, zeroed beyond the top. A fiber's
   namespace pointer is stored as the namespace's name. A proto's native
   entry is stored as 0, since tier 1 recompiles lazily. Completion tickets
   are cleared.
4. The roots, in order:
   - counters: identity hashes, gensyms, class epoch and fiber ids;
   - the empty collections and the builtin classes;
   - the frame-kind closures and the entry function;
   - symbols and keywords, sorted by name;
   - namespaces, sorted, each with its sorted mappings;
   - the run queues;
   - the pending timers, each with the time it had left.
5. **Dead fibers:** the saving fiber (and with `:fibers :none`, every
   unfinished fiber) is saved without its stack. Their waiters' claims are
   saved as taken, so no channel hands them a value.
6. The file is written to a temporary name and renamed.

**Load.** `skein --image FILE` loads before any carrier starts:
1. Allocate every object in the MMTk heap.
2. Copy each object in, relocating its references through the object table.
3. Rebuild the runtime tables (`runtime::init_from_image`), and turn each
   fiber's namespace name back into a namespace.

Carrier 0 then finishes the restore in place of boot:
- it requeues the runnable fibers;
- it re-arms the timers;
- it ends the dead fibers through the normal `finish` path, so their joiners
  and scopes see "Fiber was not saved in the image".

Last, `(skein.core/%image-restored)` runs `on-image-restore`.

**Layers.** `(save-image f :layered true)` writes a delta against the image
the process started from:
- the objects created since the load;
- *patches* for loaded objects whose canonical encoding differs from the one
  in their file.

Loading a delta loads its base chain first (the base is named relative to
the delta when they share a directory) and checks the base's checksum.
Image objects are **immortal**: they are GC roots for the life of the
process, as a boot-image space would be. So their addresses are never
reused, and a later delta can recognise them by address.

**Safety.**
- An image records a fingerprint of the runtime build that wrote it, covering:
  - primitive names and arities;
  - object kinds and builtin classes;
  - the image format;
  - the core library and compiler sources.
- A checksum covers the whole file.
- The loader validates the structure:
  - section sizes;
  - object kinds and sizes;
  - every reference's range and tag;
  - each root's kind.
- Damaged files are refused with a message, not a crash.
- Not covered: a deliberately crafted file with a valid checksum could still
  describe a bad stack or bytecode. That needs the bytecode verifier
  (design §11).

**Measured** (release, Apple M-series, base image of 7,900 objects / 434 KB):

| Start-up path | Time |
|---|---|
| From an image (`-e '(+ 1 2)'`) | 35 ms |
| Booting from source | 40 ms |

- The image load itself takes about 2 ms; booting core.skn and the compiler
  from source takes about 7 ms.
- Most of the remainder is `mmtk_init` at about 14 ms. It touches about 40 MB
  before the first allocation.

**Deviations from §7.1–7.3:**
- **Objects are copied in, not mapped.** Loading allocates and fills each
  object. Mapping the heap segment at a preferred address would need MMTk's
  VM space. Copying costs 2 ms at this size and needs no relocation table or
  write barrier.
- **No code segment.** Tier-1 code isn't saved; protos are recompiled when
  first run.
- **Deltas don't use dirty-page tracking.** They compare encodings: the save
  walks the whole reachable heap (a few ms) but writes only what changed.

---

## 8. Execution tiers

### 8.1 Tier 0 — bytecode interpreter **[decided]**

- **Register-based** (Lua 5 style), with all registers tagged and frames
  laid out as in §3.3.
- **Permanent, not just for bootstrap:** it gives instant eval at the REPL,
  eval-in-frame in the debugger, a deoptimisation target, and a fallback for
  platforms that forbid JIT (iOS).
- The dispatch loop is a Rust `loop { match op { … } }`. Threaded dispatch
  can come later if profiling says so.

### 8.2 Tier 1 — Cranelift **[decided; first version built in M4]**

**As built in M4** (`crates/skein-vm/src/jit.rs`):

*Shape.*
- A **native accelerator over the same frames**: each proto's bytecode
  becomes one native function, `(regs, consts, ctx, pc) → pc`, with an entry
  for every instruction (a jump table).
- Registers stay in the heap frame. Native code runs from the current pc
  until an instruction it doesn't handle, and returns that instruction's
  index for the interpreter to execute.
- Every exit leaves the frame exactly as the interpreter expects, so:
  - mixed stacks, the GC, the debugger and frame restarts need nothing new;
  - deoptimisation is simply interpreting from the current pc.

*What runs natively.*
- Moves, constants and upvalues.
- Var reads, with a fast path that falls back to the interpreter for unbound
  or dynamically bound vars.
- Branches, and loop back-edges, with the same safepoint check as `Vm::poll`.
- Calls to `+ - < > <= >= == = inc dec zero?` when the callee *is* that
  primitive (compared at run time, so rebinding stays live) and the
  arguments are fixnums. The overflow check is done on the raw tagged
  values.

*Compilation.*
- At tier 1 (the default since M10; `SKEIN_TIER=0` turns it off), once a
  proto has been entered 100 times (calls and
  loop turns, counted in its native word; `SKEIN_JIT_THRESHOLD` sets it, 1
  to 4000). Until M10 it was on first execution, which compiled all the
  code that runs once: `skein -e` started in 1.07 s at tier 1 against 0.11
  s at tier 0, and 0.54 s at 100. At M10's end compiling is slower than that, the code
  doing more: `skein -e nil` compiles 258 protos (2.9 MB), which took 1.0
  s on the carriers that reached the threshold, waiting for each.
- **On a thread of its own** (background compilation, M10): reaching the
  threshold puts the proto on a queue (its native word QUEUED, below
  MIN_ENTRY, so it keeps running in the interpreter, still counting its
  entries) and a compile thread compiles the hottest queued (the newest
  of equals) and installs its code with a compare-and-swap from the
  queued word: if it has left the queued range meanwhile, the code is
  dropped. (First in, first out, a loop started after start-up waited a
  second behind the 350-odd functions start-up and loading queue; newest
  first, a function warm but no longer running could wait behind newer
  ones indefinitely.) MIN_ENTRY is 2^20, QUEUED 2^19. Carriers pick it
  up at the proto's next call or loop turn, as they do code another
  carrier compiled. The thread reads the heap, so while it compiles it's
  a running safepoint participant (a collection waits out the compile in
  hand, a few ms, and can't free what it reads); waiting for work it's
  safe, and the queue is a GC root. `skein -e nil` starts in 0.10 s at
  tier 1, as at tier 0. `SKEIN_JIT_SYNC=1` compiles on the carrier, as
  before: with `SKEIN_JIT_THRESHOLD=1`, every function runs compiled from
  its first call, which is how the suites check everything compiled
  (without it about 2,000 protos queue at start-up, compiled over the
  next 1.5 s). `jit-stats`' `:pending` counts what's queued or being
  compiled: tests that need code compiled wait for it to be 0.
- A compile that panics (a bug in tier 1, or a platform it doesn't work
  on) leaves its function interpreted. The first is reported on standard
  error and each is counted (`jit-stats`' `:panicked`); tiers.rs's
  `tier1_compiles_here` checks that a hot function compiles and none
  fails, since every suite passes without tier 1 too.
- Code goes into `MAP_JIT` memory (§7.4) and is never freed.
- No relocations: constants and runtime addresses arrive through the
  arguments.

*Measured* (release, Apple M-series):

| Benchmark | Tier 0 | Tier 1 |
|---|---|---|
| counting loop, 10M iterations | 425 ms | 47 ms (9×) |
| `(reduce + (range 1000000))` | 58 ms | 13 ms (4.5×) |
| `(fib 27)` | 32 ms | 15 ms (2.1×) |

The core test session compiled 627 protos (516 KB of code), none rejected.

**Calls, as built in M10 part 2.** Native functions use Cranelift's
`tail` convention, and the interpreter enters them through a trampoline.

- **A call** is a helper (`vm::jit_call`) that works out the callee as
  `Vm::invoke` does (through vars, generic functions' dispatch and
  classes). For a closure it pushes the frame in the heap, exactly as
  `push_frame` does, and native code `return_call`s into the callee's code
  at pc 0, after checking the entry safepoint itself. A primitive or data
  function is called there and native code goes on with its value.
- **A return** is a helper that pops the frame and `return_call`s into the
  caller's code at its resume point. Tail calls replace the frame in place.
  The native stack doesn't grow, however deep the Skein stack.
- **`CLOSURE`** is a helper too: helpers may allocate, since allocation
  never collects (§4.3).
- **What goes back to the interpreter:** the bottom of a run, a callee not
  compiled yet, a safepoint that's due (POLL_FLAG), a primitive that
  failed or parked (PENDING_FLAG: the interpreter handles the error or
  park at that instruction, without calling it again), `apply`, `eval`,
  `throw` and the other primitives that need the interpreter's state, and
  a stack overflow. The interpreter's state (`St`) stays the source of
  truth: the helpers update it as they push and pop, so native code can
  stop anywhere.
- **Measured** against M10's baseline (`bench/compare.skn`, tier 1): a call
  to a closure 24 → 17 ns, to another module's function 38 → 30 ns; the
  programs 4–17% faster (Mandelbrot 293 → 249 ms, the ledger 206 → 176
  ms); compiling and loading `http.skn` 49 → 36 ms with the threshold.
  A plain call costs about what it did through the interpreter (18 ns):
  the two helper calls are most of it. Pushing frames inline, for callees
  known at compile time, belongs with direct linking (M10 part 3).

**Linked calls, as built in M10 part 3.** A call through a var is
compiled against the function the var holds when the caller compiles,
behind a guard, for every var (not only sealed modules': the user's
decision, 2026-11-16, plan-m10.md).

- **What's linked:** a `CALL` or `TAILCALL` whose callee register was last
  loaded by `GETVAR` of a var holding a closure with a fixed arity for the
  call's arguments, or by `SELF` (a function calling itself, when the
  count is its own arity's).
- **The guard:** the register must hold that closure (one compare; for
  `SELF`, the frame's own function). Then native code writes the callee's
  frame itself, exactly as `push_frame` does, when it fits in the segment,
  checks the entry safepoint and jumps into the callee's code, or stops
  for the interpreter if the callee isn't compiled yet. Otherwise the call
  goes the general way.
- **Redefinition:** a var redefined fails its callers' guards at once, so
  the new function is called even by a loop already running. The failed
  guard also resets the caller's entry, so it compiles again (after 100
  more entries) against what the var holds now; a proto relinked 8 times
  compiles unlinked from then on. Sealing isn't needed for any of this; it
  stays for api-diff and for inlining (part 4).
- **Keeping addresses valid:** the heap doesn't move objects, and linked
  closures stay alive (the JIT reports them as roots), so a linked address
  is never reused by another object. Their protos and constants are
  reachable from them.
- **Returns** pop inline when the caller's frame is below in the same
  segment: the value goes to the register the caller's `CALL` names, and
  native code goes on in the caller's code at its resume point.
- **The interpreter's state** lags behind inline pushes and pops; every
  helper first syncs it from the registers native code passes, and
  `run_inner` from where native code stopped. `JitCtx` carries the
  segment's bounds for the fit check.
- `jit-stats` counts linked call sites (`:linked`) and protos compiled again
  after a redefinition (`:relinked`).
- **Measured** (tier 1 against the baseline): a plain call 17.5 → 7.2 ns
  (2.4×), another module's function 38 → 10.6 ns (3.6×), `(fib 27)` 11.5 →
  5.3 ms; the programs 1.09–1.29× (the static site 43 → 34 ms, the ledger
  206 → 172 ms); the rope workloads 1.38×. The code is 15% larger than
  part 2's (4.8 MB for everything at threshold 1).

**Inlining, as built in M10 part 4.** In the compiler (`skein.compiler`),
so both tiers gain.

- **What's inlined:** a call of a var whose value is a small function
  defined with `def` (`defn`): an arity with a fixed number of
  parameters matching the call's arguments, a body of at most 24 IR
  nodes with no function, `def` or `letrec` in it, no captured local and
  no call of itself. The compiler keeps such functions' IR, by var, with
  the template they compiled to; a call compiled later inlines the arity
  if the var still holds a closure of that template. Calls nest three
  deep at most. The libraries Skein compiles at boot (skein.core, with the
  seed compiler) keep no IR, so their functions aren't inlined yet.
- **The guard:** the callee and its arguments go to registers as for a
  call, then `GUARD a bx` (a new instruction: if register a holds
  constant bx, skip the next one) and a jump to the ordinary call after
  the inlined body. The body's parameters are the argument registers. A
  redefinition fails the guard, so the new definition is called at once.
- **Backtraces, the debugger, profiles:** each proto's debug locals end
  with a record per inlined call `[start end fname locals holes slow-pc
  callee-register var nargs]` (register -1, which no frame has). Error
  traces, `%frames` (the debugger's frames, with `:inlined true` and the
  inlined function's locals) and profile stacks show the inlined
  function above its caller. A hole is where the inlined function would
  have tail-called a closure: its frame would be gone, so it isn't shown
  there. Restarting an inlined frame (`debug-restart-frame`) unwinds to
  the frame it's in and runs that frame's ordinary call of the function
  (its newest definition, with the argument registers), the frame going
  on after it. `skein.xref` reports what the source calls: constants used
  only by inlined code, and guards' closures, aren't counted.
- **Contracts at the call site:** a call of another module's function
  with a contract goes through a checking wrapper in the caller's
  module, as before; it's now plain code (`%wrapper-source`) when the
  contract's tests are skein.core predicates or classes (`instance?` of
  the class's var while it holds that class; also nilable or either-of),
  so it's inlined like any small function: the checks are at the call
  site. An arity taking more arguments (keyword arguments, a rest
  parameter without a contract of its own) is written twice: with its
  fixed arguments alone, the clause a call with just those inlines, and
  with the rest passed on through `apply`; counts no arity takes go the
  generic way. It's written for the contract's stamp (a hash of the
  declared types and their module, in a `f%stamp` var): a redefinition
  with other types, or none, falls back to `%wrap-for`'s generic
  wrapper. Its code names only vars, so modules stay cacheable. Other
  contracts (collections, function contracts, rest arguments with a
  contract) use the generic wrapper.
  Since part 5.3 the stamp isn't tested on each call: each wrapper is
  registered with its callee (`%register-wrapper!`), checked against the
  stamp when it's defined (a module loaded from the cache after the
  callee changed gets the generic one), and replaced by the generic one
  when the callee's contract changes (`%restamp`, `%contract-changed!`);
  code that inlined the old one calls the new one, its guard failing.

**Types, as built in M10 part 5.3.** The compiler knows what kinds of
value a node can give as it generates (a set of simple kinds: nil, true,
false, integer, float, ratio, string, character, keyword, symbol; or
unknown): from literals, locals bound to them, an inlined function's
arguments (its parameters), skein.core's type predicates and primitives
with fixed result types (`str`, `count`, `%str-trim`, comparisons…), an
inlined function's body, and the tests an `if` has passed in its
branches. An `if` whose test is pure (constants, locals, predicates of
those) and surely true or false generates only its branch: a contract's
checks at a call site go where the types prove them (a literal argument,
a result the inlined callee's body shows), at both tiers. A loop's locals
aren't typed (they change from turn to turn). skein.core's predicates
and primitives are taken as they are when compiling, while each var
holds what it did when the compiler was loaded (decision 11):
redefining one affects code compiled after it.

**The cheap paths, as built in M10 part 6.** `binding`'s map is made by a
primitive, `%bind-map` (var value …), which marks each var dynamic. A
`handler-case` clause is data in `*handlers*`, `[class nil owner tag i]`,
which `signal` throws to itself; its form's value comes back as it is, a
caught condition as `[%caught i condition]`, `%caught` a private object
no other value can be. A plain `(Class :k v…)` is made natively
(`%make-fast`: a standard class, the pairs all naming slots, every other
slot's default constant, recorded from a literal `:initform` as
`:initvalue`); anything else goes through `%make-from`, which runs
initforms in the fiber. `count`, and `nth` without a default, of a library
collection call `collection-count` and `collection-nth` directly.
Generic functions' dispatch caches compare and hash class keys by identity
(no frame for `equal`/`hash` per call). Tier 1 links a call of a generic
function whose cache holds one entry to its effective method (a guard on
the function, the argument's class and both epochs; part 3's inline frame),
and reads `(:k x)` natively from an instance that isn't mutable or a map.

**Sequence functions, as built in M10 part 7.** `into`, and the eager
functions behind `map`, `filter` and the rest on a collection, build into a
transient when they start from an empty vector (`transient` copies a
vector's items, and a map's or set's transient only wraps it); `mapv` and `filterv`
loop directly over vectors and ranges. Tier 1 runs `quot`, `rem`, `mod`,
`even?` and `odd?` on fixnums natively, and a call whose callee came from
a local or upvalue writes a closure's frame itself (its arity found at run
time). A map pattern without `:as` reads each key from a keyword-argument
seq directly (`%kw-get`), making no map.
- **Tier 1's fast primitives** grew (behind the same check that the
  callee is the primitive): `count` and `nth` of a vector (the trie
  walked natively), `first` of a list, `identical?`, `nil?`, `some?`,
  `not`, and the type predicates the checks use (`string?`, `keyword?`,
  `symbol?`, `float?`, `vector?`, `regex?`, `bytes?`, `char?`,
  `boolean?`, and `integer?` for a fixnum). A call that names one
  through its skein.core var tries only that one. Later in M10, the
  primitives code calls once an item: `reduced?` and `unreduced`
  (`reduce`'s loop); `count` and `nth` of an all-ASCII string (its length
  word's top bit, then a byte); `seq` and `next` of a list (nil, the cell
  itself, its cdr when that's a list or nil); `deref` of an atom (an
  acquire load); and `=` of the same value (true), of two immediates or
  two keywords that differ (false: keywords are interned). Anything else
  goes to the primitive.
- **Measured** (tier 1 against the baseline): a plain call 3.6× (4.8
  ns: the call is gone), a checked call across modules 26.7× (417 →
  15.6 ns; tier 0 3.8×), `count` and `nth` of a vector 2.6×; the static
  site 1.40×, the other programs 1.12–1.19×; the rope workloads 2.3–2.6×.
  **The cost was compile time:** compiling and loading `http.skn` took
  67 ms at tier 0 against 45 before inlining. Making the compiler's state
  mutable (transient vectors as records and buffers, constants found by
  an identity index) brought it to 35 ms (24–28 at tier 1).

**Registers in SSA, as built in M10 part 5.1.** Native code is entered
only at some instructions (`entries` in jit.rs): the first, the one after
each instruction it can stop at (every call included, its frame resuming
there), and a loop's head (after its back-edge's safepoint stopped). At
any other pc it stops at once and the interpreter runs on to an entry.
Between entries each register is a Cranelift variable: reads don't go back
to the frame. Writes go through to it (a store is cheap; reloading what
was just stored was the cost), so the frame stays exact wherever native
code stops. `SKEIN_JIT_SSA=0` enters anywhere, keeps no floats unboxed, as
before part 5.

**Floats, as built in M10 part 5.2.** `+ - * / inc dec zero? < > <= >= ==
=` run natively on floats and a float with a fixnum, as numeric.rs does
(a fixnum compared exactly: converted only within 2^53; `=` of floats
true of two NaNs, never of a float and a fixnum), and `+ - *` with three
arguments when the first or second is a float. A register that surely
holds a float (FloFlow: a float constant, a copy, a call of one of those
with a float argument, assumed to give a float; the greatest fixpoint,
so a loop's float variables are) is kept unboxed, an f64, and boxed only
when something needs the value itself: a generic call, a return, a stop,
an edge into code that doesn't know it's a float and reads it. A flag per
register says whether it has its box yet, and copies of a value share one
box (a must-alias analysis), so `identical?` holds as at tier 0. Where an
assumption fails (a call assumed to give a float doesn't: `*` redefined),
native code stops after the call; an entry that finds a register it
assumed a float isn't refuses, and the function is compiled again without
assumptions after four failures. `SKEIN_JIT_DUMP=name` prints the machine code
of compiled functions whose names have `name` in them;
`SKEIN_JIT_TRACE_FLO` prints each one's analysis.

Smaller things the code does (M10's done-check, for the empty loop:
2.58 → 1.85 ns a turn): a comparison whose value a `JMPF` tests next
(perhaps after a `MOVE` of it, as an `if`'s test is left) branches to
where the `JMPF` would go on its fast paths, its value still stored; a
register no debug local names (its function's or an inlined call's) and
nothing reads after an instruction isn't stored to the frame (the
interpreter reads only live registers, and the older value the slot keeps
is as good to the GC); a var's root and the attention epoch are plain
loads (an acquire held every later load back; what a root points to is
read through it, an address dependency); and a test of two conditions is
two branches, not their `and`. A call whose callee isn't known to be a
primitive's var tries only `+ - < = inc` natively before the general
call (all 35 made such call sites large and compiling slow;
`SKEIN_JIT_CASCADE` tries them all).

*Next.* The full design below (inline caches) keeps the same frame layout,
so it can replace this piece by piece.

**Full design:**

- **Input:** the compiler's ANF IR (design §8.3), lowered with the
  push-frame + `return_call` convention (§3.2).
- **When:** hot tier-0 code gets promoted by a counter of entries (built:
  see above). Compiling every `defn` eagerly was the plan; it would compile
  code that runs once, which the counter avoids.
- **How:** `cranelift-codegen` emits position-independent code into our own
  loader (§7.4). The same bytes serve the JIT and image code segments.
- **Inline caches** for generic function calls, slot access (keyed on
  layout) and var reads in sealed modules.

---

## 9. Bootstrap

```
1. skein-rt + skein-vm (Rust) compile.
2. skein-vm contains a small Rust "seed compiler": reader → kernel forms
   (+ non-hygienic defmacro) → bytecode.                 [throwaway, ~2k LOC]
3. Seed compiler loads lib/core/*.skn (stdlib basics, CLOS, conditions).
4. Seed compiler loads lib/compiler/*.skn — the real compiler, in Skein.
5. Real compiler recompiles itself and the stdlib → save base.skimg.
6. From here on: `skein` = skein-rt + base.skimg. The seed compiler is used only
   to rebuild from scratch (and in CI, to prove we still can).
```

**As built in M4:** steps 1–4.
- The Skein compiler (`lib/compiler/compiler.skn`) is a faithful port of the
  seed. Boot installs it as `eval`; `SKEIN_COMPILER=seed` keeps the seed.
- **Byte-identical output.** On every form of the core library (235 forms,
  11,549 instructions), the compiler and the test suites,
  `compare-compilers` finds no difference from the seed.
- **Self-hosting fixpoint.** The compiler compiled by the seed, by itself
  and by that result produce identical code for every function
  (`tests/skein/compiler_test.skn`).
- Step 5 is available since M5: `skein build base.skimg` saves the booted
  runtime, and `skein --image base.skimg` starts from it.
- **Step 6, built 2026-11-19.** By then booting from source took about 55 ms
  of a 91 ms start (`skein -e nil`, release, Apple M-series), where starting
  from an image took 36 ms. Without `--image`, `skein` starts from its base
  image (`image::base_image`): the runtime as booted from source, saved by
  the first start, before anything else runs, in the module cache's folder.
  A start that finds no image, or one that doesn't load, boots from source
  and saves one; that first start takes about 15 ms longer. The image is
  named for the executable's path and for its build: `build_id`, and the
  file's size and time, since a change to the VM's Rust alone changes
  neither. Saving one deletes that executable's older builds' images.
  None is used with `SKEIN_BASE_IMAGE=off`, `SKEIN_CACHE=off`,
  `SKEIN_COMPILER=seed` or `SKEIN_EXPANDER=off`, nor by `skein build`, which
  compiles from source, or a packaged app, which has its own image. It's
  made where it's used, not shipped, because what a library works out as
  it loads (`skein.io`'s `windows?`, say) is saved with it. Tested in
  `crates/skein/tests/base_image.rs`; the suites' child `skein`s start from
  it.
- **Macros run in the compiling fiber.** Since the compiler is Skein code,
  macros are ordinary calls: they can block, and compilation no longer
  inhibits the GC. The nested-run machinery remains only for boot and the
  Rust `macroexpand` primitives.

---

## 10. Repository layout

```
./
  Cargo.toml                 workspace
  crates/
    skein-rt/                 values, objects, collections, heap (MMTk binding),
                             fibers, scheduler, reactor, sync, images, FFI
    skein-vm/                 bytecode, interpreter, seed compiler, reader (bootstrap)
    skein-jit/                Cranelift backend (M4)
    skein/                    `skein` binary: REPL, run image, build
  lib/
    core/                    kernel macros, seqs, printing, numbers
    clos/                    classes, generic functions, MOP, method combination
    cond/                    condition system, restarts, debugger protocol
    async/                   scopes, channels, processes, supervisors, parallel
    compiler/                expander, ANF, optimiser, codegen drivers
    repl/                    network REPL server
  bench/                     benchmarks (bench/pingpong.skn: the M1 done-check)
  spikes/
    jit-spike/               platform checks for tier 1 and images (§7.4)
  docs/
  tests/skein/                .skn test suites (run by crates/skein-vm/tests/)
```

---

## 11. Testing strategy

**[proposed]**

- **Concurrency:** the scheduler, channels and atoms are model-checked with
  [`loom`](https://github.com/tokio-rs/loom) (Tokio's permutation tester)
  and/or `shuttle`.
- **GC:** stress mode (collect on every allocation, poison freed memory) runs
  in CI on the whole test suite.
- **Differential:** every `.skn` test runs on tier 0 and tier 1, and the
  results must agree. Images are tested by round-tripping (save → load → run
  the suite).
- **Fuzzing:** the reader, the bytecode verifier and image load (a malformed
  image must not crash the runtime).
- **Miri** on the `unsafe` core of `skein-rt`, wherever the code is
  Miri-compatible.

---

## 12. Revised milestones

These replace design.md §10.3; the order changed so that GC and fibers land
together.

**Revised after M5 (2026-11-13).** The standard library is split by what
each part needs from the runtime:

- **M6, I/O, comes first.** I/O needs a runtime piece that isn't built yet:
  the reactor, so that sockets park fibers instead of carriers. Its handles
  are exactly what M5's resource protocol is for.
- **M7 comes before sealing.** It adds the libraries that are mostly Skein,
  and the runtime types they build on. Sealing freezes interfaces, so
  anything other modules depend on has to exist first.
- **The numeric tower is decided:** fixnums promote to bignums automatically
  (as in Clojure and Common Lisp), and division of integers can give ratios.
  This changes what `+` means, so it lands before `skein.core` is sealed.
  Tier 1 keeps its fixnum fast path; overflow takes the slow path instead of
  raising an error.
- **Transducers, `format` and `edn` are in M7** because they shape
  `skein.core` itself: transducers add arities to `map`, `filter` and the
  rest, which a sealed module couldn't take later.
- **A basic FFI is in M6.** It reuses M6's blocking pool and byte buffers,
  and M5's resource protocol. It lets libraries bring native code without
  changing the runtime build, so they don't invalidate saved images, as a
  new Rust primitive would.
  - Buffers passed to C need no pinning, because the GC never moves objects.
  - Callbacks (C calling Skein, with frames in heap segments), struct layouts
    and the safety model stay in M9; the safety model needs M8's modules.
- **`skein fmt` and `skein lint` are in M7**, so the stdlib is sealed already
  formatted and lint-clean, and the core macros ship with indentation hints.
  The linter is the compiler in analysis mode, so the same warnings appear
  when compiling at the REPL. Rules that need modules come in M8.
- **Tools whose conventions end up in sealed source come before sealing:**
  the docstring convention and `skein doc`, deprecation, the test runner and
  `skein check` (M7). Tools that need a baseline taken at sealing, or that
  follow hygiene, come with it: `api-diff` and the macro stepper (M8).
- **Decisions due by M7** (they change `skein.core`, so they must be made
  before sealing). All three are now decided:
  - ~~**Laziness**~~ — decided (2026-11-13): an eager, type-preserving core
    with explicit lazy streams (design §3.3).
  - ~~**Mutable objects**~~ — decided (2026-11-13): opt-in, owned by a fiber,
    checked, with `release!`, `share` and `locking` (design §4.2).
  - ~~**Equality**~~ — decided (2026-11-13): structural, never running user
    code, with declarative per-class options (design §3.4).
- **M7.5 and M7.6 come before sealing**, so the stdlib can still change in
  response to what the UI library and the editor need.
- **Linux is the second platform** (decided 2026-11-13), then Windows.
  Linux CI can start any time: M12 is where the platform work is finished,
  not where it begins.
- **M7.5 is the runtime side of `skein.ui`, and M7.6 the Skein editor**
  (decided 2026-11-13; design §9.1).
  - `skein.ui` itself is a versioned package in its own repository,
    `skein-ui` (semver from 0.1): a shared Rust UI engine (retained tree,
    layout, styling, text, GPU painting), platform backends, and the Skein
    widget library. It isn't part of the stdlib and isn't sealed at M8.
  - This repository gains only two general hooks: main-thread handoff and
    a native extension API (Rust crates registering primitives and posting
    to channels). Until loadable extensions have a stable ABI, `skein-ui`
    builds its own statically linked `skein` binary.
  - The protocol is thick: Skein sends tree patches; Rust sends semantic
    events over channels. Skein isn't on each frame's critical path, so the
    UI doesn't wait for M10's optimiser.
  - The editor runs as its own process with the UI engine linked in; the
    code being edited runs in a separate `skein` process over the editor
    protocol.
  - The editor needs, from earlier milestones:
    - persistent ropes and Unicode graphemes and display widths (M7);
    - the comment-keeping reader, `fmt` and `lint` (M7);
    - file watching and `skein.process` (M6);
    - the editor protocol (M3).
  - Both target macOS first. Linux and Windows follow with M12, and the
    crates chosen work on all three.
- **Ongoing, outside the milestones:** a language reference and a tutorial.
  The project is MIT-licensed. Versioning is semantic, per module, enforced
  by `api-diff`; a breaking change needs a major version and at least one
  minor release of deprecation.
- **M9 splits around sealing** (decided 2026-11-14; plan-m9.md). Writing
  real libraries is how the stdlib's gaps show, so M9a comes before
  sealing: what changes `skein.core` or the runtime's contracts (types,
  contracts, the MOP, pattern matching, processes, STM, the full FFI), and
  the libraries most likely to find gaps (HTTP, SQLite, pseudo-terminals,
  a binary value format). M9b, after sealing, is what needs a sealed module
  (tree shaking) and tooling that doesn't shape core. Sealing waits for
  M9a as well as the M7.6 done-check.

| # | Milestone | Done when |
|---|---|---|
| M0 ✅ | Workspace, `Value`/object model, cons/vector/map/string, reader, printer, seed compiler, tier-0 VM on heap stacks, REPL; NoGC heap | `(reduce + (range 1000000))` at the REPL; macOS JIT spike works |
| M1 ✅ | Fibers, scheduler, safepoints, MMTk GC (non-moving Immix), atoms, channels, scopes | 1M fibers ping-pong across all cores under GC stress |
| M2 ✅ | CLOS in Skein: `defclass`, layouts, generic functions, method combination, dispatch caches | Live `defmethod` while 8 cores call the generic function |
| M3 ✅ | Conditions, restarts, parked-fiber debugger, REPL protocol | Fix a crashed fiber from the editor and resume it |
| M4 ✅ | Real compiler in Skein, Cranelift tier 1, deopt, self-host | Compiler compiles itself; tier 0 and tier 1 agree on all tests |
| M5 ✅ | Images: save/load, layers, parked fibers, `on-image-restore` | Under 50 ms startup; a saved fiber resumes after load |
| M6 ✅ | I/O: the reactor (`mio`) plus a blocking pool for files; byte arrays and buffers; `skein.io` (files, streams, paths), `skein.net` (TCP, UDP, TLS via `rustls`), `skein.process`, `skein.time`, `skein.sys` (environment variables (read-only), command-line arguments, exit, OS type and architecture, core count, memory, pid, directories, hostname, the Skein version and build; all read live, so a restored image sees the machine it runs on), `skein.fs` (walking directories, glob patterns, file watching), `skein.signal` (OS signals as channel messages); byte encodings (UTF-8, base64, hex); a basic FFI (load a dynamic library; call C functions with integer, float, pointer and byte-buffer arguments, either fast on the carrier or blocking on the pool; Cranelift-generated call stubs). Handles, including FFI handles, are image resources; weak references and finalizers in the GC binding (for resource registries, caches and freeing FFI handles); the monitor thread (replaces a carrier stuck in a long native call); fiber dumps (every fiber's stack, on a signal) | A TCP echo server holds 10,000 connections on fibers without blocking a carrier; a socket saved in an image comes back as `ResourceLost` and its `reconnect` restart works |
| M7 ✅ | Numbers, collections and the pure stdlib: bignums and ratios with automatic promotion; sorted maps and sets, a persistent queue, transients; `skein.string`, `skein.math`, `skein.set`, `skein.walk`, `skein.regex`, `skein.json`, `skein.pprint`, `skein.random`, `skein.test`; type-preserving sequence functions and explicit `lazy` streams (design §3.3); transducers (one vocabulary for sequences, channels and `preduce`); `skein.format` (`printf` directives; `cl-format` is deferred, below); `skein.edn` (a safe reader for untrusted data); `skein.async` extras (`mult`, `pub`/`sub`, `pipeline`, `merge`, dropping and sliding buffers), `skein.sync` (semaphores, latches, a rate limiter), `skein.cache` (`memoize` with LRU and TTL variants); REPL tools (`doc`, `apropos`, `source`, an inspector, `trace`/`untrace`), `skein.bench` and `skein.log`; tooling: a reader mode that keeps comments and whitespace, `skein fmt` (one canonical style, no options; macros declare indentation, e.g. `{:indent 1}`), `skein lint` (the compiler in analysis mode, signalling `Warning`s: unresolved symbols, unused locals, wrong arity to known functions, shadowed core names, `def` inside a function, `defmethod` lambda lists that don't match), and `:format`/`:lint` editor ops; `skein doc` with a docstring convention (Markdown; `:since`, `:deprecated`, `:see-also`, runnable `:examples`) and deprecation warnings; `skein test` (discovers tests, runs them in parallel on fibers, filters, TAP and JUnit output); `skein check` (fmt, lint, test and docs in one command); a terminal REPL line editor (history, multi-line editing, completion); `skein image info` and image diff; code coverage from bytecode counters if it's cheap. equality per design §3.4 (`:equality :identity`, `:compare false`, reflexive NaN, numeric-tower rules); mutable objects per design §4.2 (`(:mutable)` classes, `set-slot!`/`update-slot!`, fiber ownership with `release!`, `share` and `locking`) | Every suite runs on `skein.test`; arithmetic promotes past 62 bits identically at tier 0 and tier 1; `skein fmt --check` and `skein lint` are clean on `lib/` and `tests/`, and formatting is idempotent and reads back to the same forms |
| M7.5 ✅ | The runtime side of `skein.ui` (design §9.1). Here: main-thread handoff to an extension, a native extension API (register primitives, post to channels, per §5.5), and a spec of the UI protocol (tree patches one way, semantic events the other). In the `skein-ui` repository (a versioned package, 0.1): the shared Rust engine (retained node tree, flex and grid layout, styling, text shaping, GPU painting, hit-testing, scrolling, animation), the macOS and headless backends, `native-view` nodes, and the Skein widget library with declarative views over atom state; `skein-ui-webview` as an optional package | A widget gallery runs smoothly on macOS; redefining a view at the REPL updates the open window; a restored image reopens its windows; headless tests show no frames drawn in 5 s idle and at most one line repainted per keystroke; on macOS the idle editor averages under 0.5% CPU over a minute |
| M7.6 ✅ | The Skein editor, the first real program: a VS Code-style editor made for Skein, written in Skein on `skein.ui` (pinning a `skein-ui` version), to test the stdlib before it is sealed. It is essentially Xeon Studio rebuilt in Skein (design §9.1): explorer with Git status, project, outline and source-control views; draggable dock tabs; REPL, Problems, Debugger and Inspector; Markdown preview; terminal; API/HTTP workbench and Databases view; AI chat (Claude Code and Codex CLIs) and team chat. Language support starts with Skein only (Common Lisp, Clojure and Racket later, if wanted). Four panels are phased (design §9.1): the API/HTTP workbench follows M9's HTTP client, the Databases view M9's database drivers (SQLite first), the terminal panel pseudo-terminal support in `skein.process`, and team chat comes after M7.6 (it needs no new library); everything else, including Git through the `git` command and AI chat through the `claude`/`codex` CLIs and `skein.json`, is in M7.6. It runs as its own process with the UI engine linked in. It lives in its own repository; until M8's packages exist it loads Skein sources from a local checkout of this one. Text buffers are persistent ropes; highlighting and structure come from M7's comment-keeping reader; the REPL, debugger and evaluation go through the editor protocol (M3) to a separate `skein` process; `skein fmt` and `skein lint` run in it | The stdlib tested by real programs before sealing, and what they needed fixed: Studio itself (with its API workbench, terminal and Databases view) and five test programs (`lang-tests/`: pulse, ledger, quire, chat, loglens), then an interface sweep. **Amended 2026-11-16** (the user): it was "Skein's own development happens in the editor"; the test programs did that check's job for the stdlib, and working in Studio day to day goes on, its findings to the todo |
| M8 ✅ | Hygiene, modules/phases, sealing (after M7.6's done-check; `#lang` was dropped on 2026-11-14); then packages: a project file, dependency resolution, and `skein build` across dependencies, starting with Git and path dependencies only, with a registry later (perhaps its own milestone); module-aware lint rules (unused or missing requires, private access, phase errors, redefining sealed vars); `skein api-diff` (a snapshot of each module's public interface, taken when it is first sealed, and breaking-change detection between versions); a macro stepper that shows hygiene's renaming; a bytecode verifier for compiled code from packages (and crafted images) | Stdlib built as sealed modules |
| M9a ✅ | Before sealing (plan-m9.md): `skein.uri` and `skein.http` (HTTP/1.1 client and server), condition classes for the runtime's errors (raised from Rust with their data), `delay`, `future`, promises and `ex-info`, `match` (pattern matching on data, in core), checked type signatures and `declare`, contracts with blame at module boundaries, the full MOP (`print-object`, `initialize-instance`, `define-method-combination`, protocol sugar, slot readers as generic functions), processes/supervisors, the `skein.stm` library, the full FFI (callbacks from C, struct layouts, typed signatures, unsafe modules), SQLite on it, pseudo-terminals in `skein.process`, a binary format for Skein values, and as needed `skein.uuid`, `skein.csv`, TOML, compression, `skein.zip`, `skein.crypto`, `skein.schedule`, `skein.data` (structural diff); Studio's API workbench, terminal and Databases panels as their libraries land | Studio's API workbench, terminal and Databases view run on them; the new libraries' public functions have checked signatures; a contract violation across modules blames the right side; a supervisor restarts a crashed process while the debugger holds the crash; tier 0 and tier 1 agree on every suite |
| M9b ✅ | After sealing: tree shaking for images (`:shake`, using sealing to prove code unused), and tooling that doesn't shape core: a sampling profiler, source-drift tracking (design §5.1), `skein lsp` (a Language Server Protocol adapter over the editor protocol), `skein new` (project scaffolding), and code coverage if M7 didn't get it | — |
| M10 ✅ | The optimising compiler: the IR (design §8.3), inlining, direct linking of calls through vars, guarded, with recompilation when one is redefined (moved from M8, 2026-11-16; every var, not only sealed modules', since part 3), type inference from M9's signatures, unboxing, escape analysis; tier 1's full calling convention (push-frame and `return_call`), inline caches, and native compilation of every `defn` | Measured wins on `bench/` with tier 0 and tier 1 still agreeing on every suite |
| M11 ✅ | A collector that gathers what lives ([plan-m11.md](../plans/plan-m11.md); **revised 2026-11-18**, the user): objects may move; generational collection with its nursery copied (write barriers), reached through Immix with defragmentation; the runtime's own objects kept in place. Concurrent collection is decided after its numbers; deadlock detection and complex numbers moved to todo.md | On `bench/stash.skn` at 10 carriers: pages in use level at 3× the live bytes or less (they were about 23× and rising), the longest pause under 20 ms (148), under 2% of the time paused (7.7%), requests a second held; every suite passing with objects moved at every collection |
| M11b (parked) | Collecting while the program runs: full collections' pauses, which M11 leaves at 33–56 ms on stash, and under 2% of the time paused. Parked 2026-11-18 (the user): MMTk 0.33's concurrent plan isn't generational and moves nothing (§4.1), and we won't fork mmtk-core or write a plan of our own | Taken up when an MMTk release has a generational plan that marks concurrently. Then, on `bench/stash.skn`: no pause over 20 ms and under 2% of the time paused, the heap and requests a second as M11 left them |
| M12 | Platforms and distribution ([plan-windows.md](../plans/plan-windows.md) for Windows, drafted 2026-11-18): Linux first, then Windows (JIT memory, signals), CI on all three, release builds and installers, `skein` shipped with a base image (bootstrap step 6); WASM as a second target (design §9, open) | Every suite passes on macOS, Linux and Windows; `skein` starts from its bundled image |

### 12.1 M0 status (2026-11-13)

**Built:**
- **`skein-rt`:**
  - tagged values and the object model;
  - the NoGC heap;
  - persistent vector (32-way trie with tail) and HAMT map/set;
  - interned symbols and keywords;
  - structural `=`/`hash`;
  - seq abstraction, printer, namespaces and vars.
- **`skein-vm`:**
  - the reader (`#()`, syntax-quote with qualification and `x#` gensyms, `#lang`, `#_`, `#|…|#`);
  - the seed compiler (kernel forms plus macro expansion; flat closures; `letrec*` fix-ups; `recur` as a backward jump);
  - the register-based bytecode interpreter on heap segmented stacks (proper tail calls, a spare-segment cache, stack-overflow detection, Ctrl-C interrupt at the safepoint poll, Skein backtraces);
  - 103 primitives.
- **`lib/core/core.skn`:** 88 macros and functions, from `defn` and `cond` to `sort-by` and `update-in`.
- **`skein`:** a REPL with multi-line input, plus `skein FILE` and `skein -e EXPR`.
- **Tests:** 6 runtime unit tests, plus 89 Skein checks and REPL-level error/printing tests in `cargo test`.

**Measured** (release build, Apple Silicon, tier 0):
- `(reduce + (range 1000000))`: about 46 ms
- `(fib 27)`: about 31 ms

**Deliberately deferred**, with a clear error where a user would hit it:

| Item | Arrives in |
|---|---|
| Garbage collection (MMTk binding) | M1 |
| Bignums and ratios | — (errors today) |
| Lazy / infinite seqs | — (errors today) |
| Regex literals | — (errors today) |
| Destructuring | M4 (real compiler) |
| Metadata | — (read and dropped) |
| `try`/conditions | M3 |
| Transients | — (`map`/`into` allocate per element; harmless until there's a GC) |

### 12.2 M1 status (2026-11-13)

**Built:**
- **Fibers on heap segmented stacks:** start, yield, park, resume.
- **Work-stealing M:N scheduler:** one carrier per core plus a timer thread.
- **Safepoint protocol, MMTk binding (non-moving Immix) and GC stress mode.**
- **Concurrency API:**
  - `spawn`, `with-scope`, `await`/`@`, `settle`, `cancel!`;
  - `sleep`, `yield`;
  - `chan`, `put!`, `take!`, `offer!`, `poll!`, `close!`, `alts!`, `timeout`;
  - `atom`, `swap!`, `reset!`, `compare-and-set!`;
  - `pmap`, `preduce`.
- **All evaluation runs in fibers.** `eval` is an interpreter intrinsic, so evaluated code can park, and the REPL thread never touches the heap.

**Measured** (release build, Apple M-series, 10 cores; `bench/pingpong.skn`):

| Run | Result |
|---|---|
| 1M fibers alive at once, then 10M messages | ✅ 1.73 GB heap; 4.6 s with `SKEIN_HEAP_MIN=3g` (1 GC); 21 s from the default 64 MB heap (161 GCs, mostly while spawning); with growth proportional to live data (M10), 5.9 s and 7 GCs from the default heap, peak RSS 2.2 → 3.3 GB |
| 1M fibers, `SKEIN_GC_STRESS=20` | ✅ 441 GCs, 68 s |
| 40k fibers, `SKEIN_GC_STRESS=1` | ✅ 65,694 GCs, 2M messages, checksums correct |
| core suite, `SKEIN_GC_STRESS=1` | ✅ 2,032,983 GCs, 0 failures |
| suites ×10 at 1, 2, 3 and 10 carriers | ✅ 0 failures |

**Found along the way:**
- **Run-queue starvation at one carrier:** fixed with a global queue for preempted fibers and a periodic global-first pick (§5.1).
- **Probable fragmentation:** under extreme GC stress (a collection every few milliseconds while 40k fibers were being spawned), MMTk reported 1.1 GB in use for about 70 MB of live data. The likely cause is that non-moving Immix keeps a whole 32 KB block for a few scattered survivors (not yet verified). The fix is opportunistic defragmentation, which moves objects, so the interpreter would have to reload cached pointers after a safepoint. **[open]**

**Deferred** (unchanged from M0, plus):

| Item | Arrives in |
|---|---|
| Dynamic bindings and conveyance | M3, with conditions |
| `try` (tests use `settle` meanwhile) | M3 |
| Error locations (`file:line`) for code loaded through `load-file`, which now runs in Skein | M3 |
| I/O reactor, FFI monitor thread | M7 |
| Generational GC (needs write barriers) | — |

### 12.3 M2 status (2026-11-13)

**Built:**
- **Classes and instances:**
  - builtin class hierarchy (every value has a class);
  - `defclass` with multiple inheritance (C3), slot options `:initform`, `:initarg`, `:reader`, `:required`, `:type` and `:doc`;
  - `make`, and calling a class as its constructor;
  - immutable instances with `with`/`assoc`, keyword and `get` access, structural `=`/`hash`, and printing as `#ns/class{…}`.
- **Generic functions:**
  - `defgeneric`/`defmethod` with multiple dispatch and `eql` specializers;
  - the standard method combination (`:before`, `:after`, `:around`, `call-next-method`, `next-method?`) and the `+ and or list progn max min vector` combinations;
  - `& rest` lambda lists;
  - custom dispatch (multimethod style), `%remove-method`.
- **Live redefinition:** of methods and of classes (including superclasses, with subclasses recomputed); old instances are read through their own layout.
- **Bit operations:** `bit-and`, `bit-or`, and the rest.

**Verified:**
- `tests/skein/clos_test.skn` (91 checks), also under `SKEIN_GC_STRESS=1` (1,379 GCs) and at 1, 2, 3 and 10 carriers.
- `bench/dispatch.skn` (the done-check): 8 fibers, a live `defmethod` seen by all within 167 µs; under `SKEIN_GC_STRESS=1`, 1,895 GCs and still correct.

**Found along the way:** a lost wake-up from M1. A fiber cancelled between
starting a blocking operation and parking slept forever with its cancel
flag set. It showed up as a rare `cargo test` hang once the CLOS suite
loaded the machine, and was reproduced deterministically by cancelling
20,000 freshly spawned fibers blocked on `take!`. The fix is a store-then-
load handshake on PARK/CANCEL with sequentially consistent ordering (see
`cancel` in sched.rs). It has its own commit and regression tests.

**Known gaps:**
- A miss compiles the effective method with `eval`, which takes about 21 µs per new argument-class combination (measured with one primary and one `:before` method).
- Class redefinition invalidates every generic function's cache.

### 12.4 M3 status (2026-11-13)

**Built:**
- the condition system (§5.4, design §5.2);
- dynamic bindings with conveyance;
- resumable runtime errors (`use-value`, `retry`);
- `unwind-protect` cleanups on errors, restarts, cancellation and stack
  overflow;
- the parked-fiber debugger (backtrace with locals, `eval-in-frame`, restart,
  restart frame, abort);
- debugger mode in the terminal REPL;
- the editor protocol server (`skein --server PORT`);
- `file:line` locations for errors in loaded code.

**Verified:**
- `tests/skein/cond_test.skn` (50 checks);
- `crates/skein-vm/tests/editor.rs`, the done-check: over TCP, crash a worker,
  inspect it, fix it with `:eval`, resume it with `:restart-frame`; it
  finishes its whole job;
- the full suite 10/10 times;
- the suites under GC stress (see the note below) and on one carrier.

**Known gaps:**
- Output from editor evaluations goes to the server's stdout (not captured
  per request).
- Frames replaced by tail calls can't be shown or restarted (a debug mode
  without TCO could fix that).
- `Interrupted` (Ctrl-C) aborts the evaluation rather than entering the
  debugger.
- Local names come from the compiler and include out-of-scope locals in the
  same function.
- `SKEIN_GC_STRESS=1` together with a very deep stack is quadratic: every
  forced collection rescans the whole stack. The stack-overflow check in
  `cond_test.skn` (about 16M frames) doesn't finish in reasonable time at
  level 1. The rest of the suite passes at level 1 in under a second, and the
  whole suite passes at level 3 (839 forced collections).

### 12.5 M4 status (2026-11-13)

**Built:**
- the Skein compiler, which is self-hosting and byte-identical to the seed
  (§9);
- destructuring in `let`, `loop`, `fn`, `defn`, `defmacro` and `doseq`;
- compile errors as `CompileError` conditions with `retry` and `use-value`;
- macros that can block;
- tier 1 as a Cranelift accelerator (§8.2), with `jit-stats`.

**Verified:**
- `tests/skein/compiler_test.skn` (the fixpoint and seed agreement);
- `crates/skein/tests/tiers.rs` (the done-check): every suite passes at tier 0
  and tier 1 with identical results;
- tier 1 on one carrier and under `SKEIN_GC_STRESS` 1 and 3.

**Deviations from the plan:**
- **Tier 1 isn't the full calling convention yet.** Calls and returns go
  through the interpreter (§8.2).
- **Deoptimisation is trivial** because frames are shared: there's no
  inlining yet, so nothing needs undoing.
- **No ANF IR or optimiser yet.** The compiler emits the same register
  bytecode as the seed, which is what makes the byte-for-byte check
  possible. The IR and optimisations (design §8.3) are the next step for
  the compiler.

### 12.6 M5 status (2026-11-13)

**Built:**
- images (§7.5): `save-image` and `skein --image`, with `:entry`, `:fibers
  :parked|:none` and `:layered`;
- `image-info`;
- `skein build OUT.skimg [FILE…] [--entry NS/NAME]`, which produces
  reproducible images;
- the restore protocol (design §7.2):
  - `register-resource!`, `on-image-restore` and `ensure-live`;
  - `ResourceLost`, with `reconnect` and `abort` restarts.

**Verified** (`crates/skein/tests/images.rs`, the done-check):
- These fibers carry on after a save and load:
  - a fiber parked on a channel;
  - a sleeping fiber (its timer keeps its remaining time);
  - a `timeout` channel;
  - busy fibers preempted mid-loop;
  - a fiber crashed into the debugger, which is fixed and restarted at its
    frame after the load.
- The saving fiber, and unsaved fibers, end with an error that their joiners
  see.
- Every suite passes in a runtime started from an image: at tier 1, on one
  carrier, and under `SKEIN_GC_STRESS=2` on all carriers and on two. The
  conc and clos suites also pass at level 1.
- **`SKEIN_GC_STRESS=1` feeds on itself.** Each collection bumps attention,
  so every carrier's next poll takes the slow path and requests another
  collection. A 300,000-iteration loop collects 301,237 times at level 1 and
  16 times at level 10. Level 2 already breaks the cycle (1,119 collections
  for the core suite) and is the practical setting for full-suite runs.
- Layered images are about 5% of the base's size, and they refuse a changed
  base.
- `skein build` is byte-for-byte reproducible.
- Damaged images are refused.
- Start-up is 35 ms in release.

**Fixed along the way:**
- **GC workers had a null thread handle.** MMTk's debug build asserts on
  this, and the GC worker died; the fiber that asked for that collection
  then waited forever. Workers now get a real handle.
- **`defmethod` on another namespace's generic function** used to define a
  new local one. It now adds the method to the existing one.

**Deviations from the plan:**
- **Copy-in loading, no code segment, no dirty-page tracking** (§7.5).
- **No tree shaking yet** (`:shake`, design §7.1). An image holds everything
  reachable from the namespaces.
- **Resources are a Skein protocol only.** There are no file, socket or FFI
  handles yet, so nothing in the runtime registers itself.
- **Start-up:** the 50 ms target is met. The floor is now MMTk's
  initialisation, not Skein's.

### 12.7 M6 status (2026-11-13)

**Built** (plan: `docs/plans/plan-m6-m7.md`):
- **Library loading** before modules: `require` with `:as` and `:refer`,
  `ns` `(:require …)`, embedded libraries, and `SKEIN_PATH`.
- **Weak references and cleanups** (§4.5).
- **`Bytes`** and `skein.bytes`.
- **The reactor, the blocking pool, native regions and spare carriers**
  (§5.5).
- **The libraries:**
  - `skein.io` (files, streams, paths), `skein.fs` (walk, glob, watch),
    `skein.sys`;
  - `skein.process`, `skein.time`, `skein.signal`;
  - `skein.net` (TCP, UDP, TLS with rustls);
  - `skein.ffi`, the basic FFI (§5.6).
- **Fiber dumps:** `(fiber-dump)`, and SIGQUIT prints one.

**Verified:**
- **The done-check** (`crates/skein/tests/io.rs`):
  - an echo server holds 10,000 TCP connections open at once on two
    carriers (2.6 s in a debug build);
  - a socket saved in an image comes back as `ResourceLost`, and the
    `reconnect` restart opens a working connection to the same server.
- `tests/skein/lib_test.skn` (122 checks) passes:
  - in process, at tier 0 and tier 1, from an image;
  - under `SKEIN_GC_STRESS=2` and on one carrier.
- Image tests cover weak references, file handles and foreign functions;
  `libs.rs` covers library loading.

**Found and fixed along the way:**
- **Heap corruption under GC stress:** threads outside the carriers held
  heap values outside the GC roots. The rule is in §5.5.
- **GC worker threads had a null thread handle,** which MMTk's debug build
  rejects (this came up in M5).
- **`try` didn't recognise `catch` and `finally`** when a macro's
  syntax-quote had qualified them.
- **`defmethod` on another namespace's generic function** defined a new
  one (this came up in M5).

**Deviations from the plan:**
- **No `io_uring`** (Linux): the reactor is plain `mio`.
- **Time zones** are UTC, local or a fixed offset; there's no tz database
  yet.
- **The FFI's 64-bit results** must fit a fixnum until M7's bignums.

### 12.8 M7 status (2026-11-13)

**Built** (plan and decisions: `docs/plans/plan-m6-m7.md`):
- **The numeric tower** (§2.5): bignums and ratios, promotion in the
  primitives and at tier 1.
- **Equality declarations and owned mutable objects** (design §3.4, §4.2).
- **Sequences** (design §3.3): type-preserving functions, explicit lazy
  streams, transducers.
- **Collections** (§2.4): sorted maps and sets, a persistent queue,
  transients, and `skein.rope`.
- **Text** (§2.6): `skein.string` (graphemes, display widths), regexes
  (`#"…"`), `skein.json`, `skein.edn`, `format`, `skein.format`,
  `skein.pprint`.
- **The rest of the pure stdlib:** `skein.math`, `skein.set`, `skein.walk`,
  `skein.random`, `skein.async` (with dropping and sliding channels),
  `skein.sync`, `skein.cache`, `skein.log`.
- **Tools:** `skein.test` and `skein test`; the comment-keeping reader,
  `skein fmt` and `skein lint` (and the editor's `:format` and `:lint`);
  `doc`, `source`, `apropos`, `find-doc`, `inspect`, `trace`,
  `with-out-str` and `skein.bench`; `skein doc`, `skein check` and
  `skein image info`/`diff`; a rustyline terminal REPL.

**Verified** (the done-check):
- Every suite runs on `skein.test`: seven files, 965 checks, at tier 0 and
  tier 1, on one carrier, from an image, and under `SKEIN_GC_STRESS=2` on
  all carriers and on two (the release build runs that whole matrix in
  about 2½ minutes).
- Arithmetic promoting past 62 bits gives identical results at both tiers
  (`tiers.rs`).
- `lib/`, `tests/` and `bench/` pass `skein fmt --check` (so formatting them
  is idempotent; every rewrite is checked to keep the same tokens) and
  `skein lint`.

**Found and fixed along the way:** NaN compared equal with `==`; `comp` of
three or more functions skipped the first; `cons` forced a stream tail;
`nth` on an infinite stream; `class-of` for sorted collections and streams;
a `loop` that shadowed the list it read; four racy or timing-dependent
tests (they now wait for what they check, or watch their own handles).

**Deviations from the plan:**
- **Sorted collections use natural order only**; `sorted-map-by` waits until
  Skein classes can implement the collection operations.
- **Transient maps and sets copy per change** (the API is final).
- **`cl-format`** is deferred; `format` covers printf directives.
- **Coverage** is deferred to M9's profiler.
- **The lint rules are a static walk**, not the compiler in analysis mode.

### 12.9 M7.5 status (2026-11-13)

**Built** (plan and decisions: `docs/plans/plan-m7.5.md`):
- **Here:** native extensions and the main-thread handoff (§5.7), and the
  UI protocol, version 1 (`docs/internals/ui-protocol.md`). There were also core
  additions `skein.ui` needed: watches on atoms, after-eval hooks, `for`,
  and `require` of extension namespaces.
- **In `skein-ui` 0.1** (`~/projects/skein/skein-ui`):
  - **The engine:** the node tree, themes, taffy layout, damage tracking,
    text fields, scrolling, drag, and the code-editor surface.
  - **Painting:** wgpu, damage-only, with headless PNGs.
  - **Backends:** the desktop backend on winit, with menus, file dialogs,
    the clipboard and file drops; and a headless backend.
  - **`skein.ui`:** declarative views and keyed diffing; the widget kit
    (toggle, checkbox, tabs, split, tree, a virtualized list); the editor
    model; `examples/gallery.skn`.

**Verified** (the done-check):
- **The widget gallery runs smoothly on macOS**, judged by eye by the
  user. Menus, file dialogs, the clipboard and file drops are tested
  against the headless backend's stand-ins (`tests/platform_test.skn`).
- **Redefining a view at the REPL updates the open window.** Forms on the
  `skein-ui` REPL's stdin redefine a view, and the window shows the new
  tree (`tests/repl.rs`).
- **A restored image reopens its windows** (`tests/images.rs`).
- **Headless idle:** no frames in 5 s idle.
- **Headless keystroke:** one keystroke in the editor surface draws one
  frame. It repaints one line (560×19 px) and runs no layout pass.
- **macOS idle CPU:** the idle gallery on its editor tab used 0.25% CPU
  over 60 s (release build; 0.15 s of CPU time).

**Found and fixed along the way:**
- **The scheduler's monitor thread** woke every 10 ms for as long as the
  process lived. It now parks while no carrier is in native code, which
  cut idle CPU from 0.75% to 0.25%.
- **`ask` from an event handler deadlocked.** The answer comes through
  the very event loop the handler blocks, so `ask` now keeps that loop
  running while it waits.
- **Hover didn't reach ancestors**, and text properties didn't inherit.
- **A changed `:selected` didn't restyle until the next redraw.**

**Deviations from the plan:**
- **Built after the done-check, before M7.6:**
  - the editor's multiple carets, clipboard, drag-select and horizontal
    scrolling;
  - popups and tooltips (as OS windows), native context menus and
    dropdowns;
  - IME composition and style transitions;
  - virtualized grids and tab docks;
  - canvases, and self-drawing widgets through CLOS (`ui/Widget`).

  Skein gained destructuring `if-let`/`when-let` and `ext::to_f64` on the
  way.
- **`native-view` and `skein-ui-webview` slip to `skein-ui` 0.2.** Nothing
  needs them before M9's HTTP and database panels.
- **The desktop backend is tested on macOS only.**
- **Not yet drawn:** colour emoji, and `[:image …]` on canvases.
- **Canvas paths are not antialiased** (rectangles and circles are).

---

## 13. Open questions

1. ~~**Structural-only equality and hashing**~~ — decided: structural, with
   declarative per-class options (§2.3, design §3.4).
2. **Multi-shot continuations** (§3.4): keep them out unless a need appears?
3. ~~**Concurrent GC** (§4.1): wait for MMTk, or plan our own?~~ —
   decided 2026-11-18: wait for MMTk (M11b, parked).
4. ~~**Parked fibers nothing can wake**~~ — decided: both. When the GC
   finds a parked fiber nothing can reach, it posts a deadlock event (as Go
   does) and moves the fiber into the debugger's list with a `Deadlock`
   condition instead of freeing it. To be built on M6's finalizers (it was
   M11's; in todo.md since 2026-11-18).
