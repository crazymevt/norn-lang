# Norn Runtime Architecture (draft 0.1)

Companion to [design.md](design.md). This document covers *how* the Rust
runtime delivers the language's promises: fibers that can be parked and
debugged, fibers saved in images, live redefinition on all cores, and
multicore throughput.

Decisions are marked **[decided]**, **[proposed]** or **[open]**, as in the
design doc.

---

## 0. The three load-bearing decisions

Most of this document follows from these three choices:

1. **Norn frames live in heap-allocated, segmented stacks — not on the
   native stack** (§3). A fiber's stack is ordinary heap data. That makes
   fiber switches cheap, makes stacks visible to the GC precisely, and lets
   the debugger inspect them and images save them.
2. **Rust never calls Norn** (§2.3). Runtime primitives are leaves. Anything
   that needs to call user code (sorting with a comparator, `print-object`,
   reader macros) is written in Norn. This is what makes rule 1 possible:
   there are never Rust frames sandwiched between Norn frames.
3. **One frame format for interpreted and compiled code** (§3.3). The
   bytecode interpreter and Cranelift-compiled code push the same frames, so
   stacks can mix both, the debugger handles them identically, and
   deoptimisation on redefinition is just "resume this frame in the
   interpreter".

---

## 1. Layers

```
┌─────────────────────────────────────────────────────────────────────┐
│ Norn code: stdlib, CLOS/MOP, conditions, compiler, REPL server      │  lib/*.norn
├─────────────────────────────────────────────────────────────────────┤
│ Execution:  bytecode interpreter (tier 0) │ Cranelift JIT/AOT (tier 1)│  norn-vm, norn-jit
├─────────────────────────────────────────────────────────────────────┤
│ Runtime core: values · object model · persistent collections        │
│   fibers & stacks · scheduler · reactor · sync primitives           │  norn-rt
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

A Norn value is one 64-bit word, `#[repr(transparent)] struct Value(u64)`,
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

### 2.3 "Rust never calls Norn" **[decided]**

Rust primitives (`vector-conj`, `string-concat`, `atom-cas`, …) are leaf
functions: they may allocate but never invoke Norn code. Consequences:

- **Equality and hashing are structural and runtime-defined**, so the HAMT in
  Rust can implement `assoc`/`get` without calling back into user code. That
  also guarantees map operations can't deadlock or run user side effects in
  parallel folds. Custom key semantics use a key function. **[open: confirm]**
- **Sorted collections, `sort` with a comparator, `print-object`, reader
  macros and anything else higher-order are written in Norn.**
- **FFI callbacks** are the single exception: they are *continuation
  barriers*. While native code is on the stack, the fiber is pinned to its
  carrier and can't be captured, saved or migrated. Racket's CS backend has
  the same rule.

### 2.4 Collections **[proposed]**

| In Rust (no user callbacks needed) | In Norn |
|---|---|
| Persistent vector (RRB), hash map/set (CHAMP), string, byte-vector, cons | Sorted map/set, lazy streams, transducers, `sort` |
| Transients (with owner-fiber check) | Queues, everything built on the above |

Rust gives them speed while everything runs on the bytecode interpreter.
Once the native compiler is good, moving more into Norn is an option.

---

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
  Norn calls (§3.2) and no assembly context switching.
- **Memory (measured, M1):** a parked fiber costs about 1.7 KB in total
  (fiber object, first segment, closure, waiter). 1M parked fibers use
  1.73 GB of heap. A fiber gets its first segment when it starts running.

### 3.2 Calls compile to "push frame + tail call" **[decided]**

The native stack never holds Norn values across a Norn call.

- **Non-tail call:** spill live values into the current Norn frame, record the
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
in Norn.

Multi-shot continuations (copy on reinstatement) are **[open]**; nothing in
the design needs them yet.

### 3.5 Kernel language **[proposed]**

All of Norn macro-expands to roughly 14 forms, which both tiers implement:

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
  (`norn-rt/src/heap.rs`): parallel (one GC worker per core), stop-the-world,
  and objects never move, so the interpreter's cached code and frame pointers
  stay valid across a GC. Dynamic heap size, 64 MB up to 8 GB by default
  (`NORN_HEAP_MIN`, `NORN_HEAP_MAX`).
- **Next: generational `StickyImmix`** with `sticky_immix_non_moving_nursery`.
  It needs write barriers on stores into possibly-old objects (var roots,
  atoms, fiber, channel and scope fields; §4.4). Stack frames don't need
  them, because frames are scanned through their fiber at every GC.
- **Later: concurrent collection**, as MMTk's concurrent plans mature or
  through our own plan. **[open]**
- **One MMTk mutator per carrier thread**, not per fiber. Fibers allocate from
  their carrier's thread-local buffer.
- **Conses carry a one-word header** (`[layout][car][cdr]`, 24 bytes), because
  MMTk reads every object's size from the object itself. Their pointer tag
  still identifies them without a memory load.

### 4.2 Roots and safepoints **[decided]**

Norn values live only in the heap (stack frames included), so the roots that
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
| reduction budget (2000) used up | yield, if other fibers are waiting |
| stop-the-world requested | save stack top, block until the GC finishes |
| `(gc)` or `NORN_GC_STRESS` | save stack top, request a collection, block |
| fiber cancelled / interrupted | raise `Cancelled` / `Interrupted` |
| work submitted from outside (REPL, cancel) | handle it |

The **safepoint protocol** (`norn-rt/src/safepoint.rs`) counts *running*
participants: carriers executing Norn code, and the timer thread while it
fires timers. Stopping the world waits until none is running. Idle carriers
count as safe.

### 4.3 Rooting in Rust **[decided — revised in M0]**

**Allocation never triggers a collection.** If allocation runs out of room it
takes more memory and sets the poll word, and the collection then happens at
the next safepoint, where every live Norn value is in a heap stack frame.

This removes almost all rooting from Rust. Primitives are leaves (§2.3), so
they contain no safepoints and can hold raw `Value`s across allocation
freely. Handles (`Handle<'scope>`, as in V8's HandleScope) are only needed by
the little Rust code that runs Norn code *and* keeps values across it. In M0
that is only the seed compiler holding forms while it calls a macro.

- **Risk:** a primitive that allocates enormously between safepoints
  (`(vec (range 1e9))`) over-commits the heap. That's acceptable: it's bounded
  by the size of the result, which has to be built anyway.
- **Verified in M1:** MMTk allocates with
  `AllocationOptions { allow_overcommit: true, at_safepoint: false }`. A full
  heap makes it *request* a collection, and the collection then starts
  through our `stop_all_mutators`, which waits for every carrier to reach a
  safepoint.
- **Nested runs are GC-inhibited.** Rust code that re-enters Norn (the seed
  compiler expanding macros, boot) holds values the GC can't see. While such a
  run is active, the carrier's polls don't stop for a GC, which simply waits
  for the run to end. The seed compiler is replaced in M4.
- **GC stress mode:** `NORN_GC_STRESS=N` forces a collection at every N-th
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

- **As built in M1** (`norn-vm/src/sched.rs`):
  - one carrier thread per core (`NORN_THREADS` overrides), plus a timer thread;
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
- **Not built yet:** the I/O reactor (`mio`) and the monitor thread for
  blocking FFI arrive with I/O and FFI (M7). The debugger park reason arrives
  in M3.
- **Preemption** via the reduction budget (§4.2): 2000 reductions per slice.

### 5.2 Dynamic bindings and conveyance **[decided]**

- Each fiber holds a pointer to a **persistent map** of var → value.
  `binding` pushes a new map, and `spawn` copies the pointer (O(1)
  conveyance).
- Each var has a `has-ever-been-dynamically-bound` flag, so reading an
  ordinary global is a single acquire-load of its root.
- Condition handlers and restarts are just dynamic bindings, so they are
  conveyed into children too (design §5.3).

### 5.3 Sync primitives **[M1: atoms, channels, scopes built]**

All shared mutable state is in the heap, guarded by one-word spinlocks inside
the objects (`norn-rt/src/conc.rs`). There are no Rust-side allocations,
finalisers or extra root tables.

| Primitive | Implementation |
|---|---|
| `atom` | `AtomicU64` CAS on a tagged value; `swap!` is a CAS loop written in Norn |
| `var` | root slot (release store / acquire load) + flags |
| `chan` | spinlock + optional ring buffer + FIFO queues of waiters (two lists each) for takers and putters; unbuffered channels hand values over directly. `alts!` locks its channels in address order and parks on all of them with one shared claim. `timeout` is a channel the timer thread closes. |
| scope | children list, pending count, first failure, owner waiter. A failing child cancels its siblings; cancelling the owner cancels the tree. |
| fiber-aware mutex | **not yet** |
| STM `ref` | **not yet** (still open: core or library) |
| dynamic bindings / conveyance (§5.2) | **not yet** (arrive with conditions in M3); a fiber inherits its creator's current namespace |

---

## 6. Generic function dispatch

**[implemented in M2]** — `norn-rt/src/clos.rs` holds the shapes and the hot
path; `lib/core/core.norn` holds the policy.

**Objects.**

```
Class:    [name][supers][cpl][slots][layout][builtin][-][meta][subclasses]
Instance: [class][layout][n][slot0 … slotn-1]          immutable
layout:   Array [k0 … kn-1, d0 … dn-1]                 slot keywords, then defaults
Generic:  [name][methods][state][epoch][nreq][rest][dispatch][combination][lock][meta]
state:    Array [mask, eqls, cache, class-epoch]        immutable, swapped atomically
cache:    Array [nkeys, cap, count, (k0 … kn-1, em) × cap]
```

**Hot path** (in the interpreter; Rust never calls Norn):

1. Load the generic function's `state` (one acquire load).
2. If its class epoch isn't the current global class epoch, it's a miss.
3. For each specialised position in `mask`, the key part is the argument
   itself if it's in that position's `eql` set, otherwise its class.
4. Probe the open-addressed cache (no allocation, no locks).
5. On a hit, the interpreter *continues its call loop* with the cached
   effective method, so the generic function adds no frame of its own.
6. On a miss, it calls `%gf-miss` (Norn) in the same fiber, which computes the
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
Custom-dispatch generic functions compute the dispatch value in Norn and key
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

### 8.2 Tier 1 — Cranelift **[decided]**

- **Input:** the compiler's ANF IR (design §8.3), lowered with the
  push-frame + `return_call` convention (§3.2).
- **When:** `defn` at the REPL compiles to tier 1 eagerly once tier 1
  exists. Hot tier-0 code (via a call counter) gets promoted.
- **How:** `cranelift-codegen` emits position-independent code into our own
  loader (§7.4). The same bytes serve the JIT and image code segments.
- **Inline caches** for generic function calls, slot access (keyed on
  layout) and var reads in sealed modules.

---

## 9. Bootstrap

```
1. norn-rt + norn-vm (Rust) compile.
2. norn-vm contains a small Rust "seed compiler": reader → kernel forms
   (+ non-hygienic defmacro) → bytecode.                 [throwaway, ~2k LOC]
3. Seed compiler loads lib/core/*.norn (stdlib basics, CLOS, conditions).
4. Seed compiler loads lib/compiler/*.norn — the real compiler, in Norn.
5. Real compiler recompiles itself and the stdlib → save base.lim.
6. From here on: `norn` = norn-rt + base.lim. The seed compiler is used only
   to rebuild from scratch (and in CI, to prove we still can).
```

---

## 10. Repository layout

```
./
  Cargo.toml                 workspace
  crates/
    norn-rt/                 values, objects, collections, heap (MMTk binding),
                             fibers, scheduler, reactor, sync, images, FFI
    norn-vm/                 bytecode, interpreter, seed compiler, reader (bootstrap)
    norn-jit/                Cranelift backend (M4)
    norn/                    `norn` binary: REPL, run image, build
  lib/
    core/                    kernel macros, seqs, printing, numbers
    clos/                    classes, generic functions, MOP, method combination
    cond/                    condition system, restarts, debugger protocol
    async/                   scopes, channels, processes, supervisors, parallel
    compiler/                expander, ANF, optimiser, codegen drivers
    repl/                    network REPL server
  bench/                     benchmarks (bench/pingpong.norn: the M1 done-check)
  spikes/
    jit-spike/               platform checks for tier 1 and images (§7.4)
  docs/
  tests/norn/                .norn test suites (run by crates/norn-vm/tests/)
```

---

## 11. Testing strategy

**[proposed]**

- **Concurrency:** the scheduler, channels and atoms are model-checked with
  [`loom`](https://github.com/tokio-rs/loom) (Tokio's permutation tester)
  and/or `shuttle`.
- **GC:** stress mode (collect on every allocation, poison freed memory) runs
  in CI on the whole test suite.
- **Differential:** every `.norn` test runs on tier 0 and tier 1, and the
  results must agree. Images are tested by round-tripping (save → load → run
  the suite).
- **Fuzzing:** the reader, the bytecode verifier and image load (a malformed
  image must not crash the runtime).
- **Miri** on the `unsafe` core of `norn-rt`, wherever the code is
  Miri-compatible.

---

## 12. Revised milestones

These replace design.md §10.3; the order changed so that GC and fibers land
together.

| # | Milestone | Done when |
|---|---|---|
| M0 ✅ | Workspace, `Value`/object model, cons/vector/map/string, reader, printer, seed compiler, tier-0 VM on heap stacks, REPL; NoGC heap | `(reduce + (range 1000000))` at the REPL; macOS JIT spike works |
| M1 ✅ | Fibers, scheduler, safepoints, MMTk GC (non-moving Immix), atoms, channels, scopes | 1M fibers ping-pong across all cores under GC stress |
| M2 ✅ | CLOS in Norn: `defclass`, layouts, generic functions, method combination, dispatch caches | Live `defmethod` while 8 cores call the generic function |
| M3 | Conditions, restarts, parked-fiber debugger, REPL protocol | Fix a crashed fiber from the editor and resume it |
| M4 | Real compiler in Norn, Cranelift tier 1, deopt, self-host | Compiler compiles itself; tier 0 and tier 1 agree on all tests |
| M5 | Images: save/load, layers, parked fibers, `on-image-restore` | Under 50 ms startup; a saved fiber resumes after load |
| M6 | Hygiene, modules/phases, `#lang`, sealing | Stdlib built as sealed modules |
| M7 | Full MOP, STM, processes/supervisors, FFI, gradual types, contracts | — |

### 12.1 M0 status (2026-11-13)

**Built:**
- **`norn-rt`:**
  - tagged values and the object model;
  - the NoGC heap;
  - persistent vector (32-way trie with tail) and HAMT map/set;
  - interned symbols and keywords;
  - structural `=`/`hash`;
  - seq abstraction, printer, namespaces and vars.
- **`norn-vm`:**
  - the reader (`#()`, syntax-quote with qualification and `x#` gensyms, `#lang`, `#_`, `#|…|#`);
  - the seed compiler (kernel forms plus macro expansion; flat closures; `letrec*` fix-ups; `recur` as a backward jump);
  - the register-based bytecode interpreter on heap segmented stacks (proper tail calls, a spare-segment cache, stack-overflow detection, Ctrl-C interrupt at the safepoint poll, Norn backtraces);
  - 103 primitives.
- **`lib/core/core.norn`:** 88 macros and functions, from `defn` and `cond` to `sort-by` and `update-in`.
- **`norn`:** a REPL with multi-line input, plus `norn FILE` and `norn -e EXPR`.
- **Tests:** 6 runtime unit tests, plus 89 Norn checks and REPL-level error/printing tests in `cargo test`.

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

**Measured** (release build, Apple M-series, 10 cores; `bench/pingpong.norn`):

| Run | Result |
|---|---|
| 1M fibers alive at once, then 10M messages | ✅ 1.73 GB heap; 4.6 s with `NORN_HEAP_MIN=3g` (1 GC); 21 s from the default 64 MB heap (161 GCs, mostly while spawning) |
| 1M fibers, `NORN_GC_STRESS=20` | ✅ 441 GCs, 68 s |
| 40k fibers, `NORN_GC_STRESS=1` | ✅ 65,694 GCs, 2M messages, checksums correct |
| core suite, `NORN_GC_STRESS=1` | ✅ 2,032,983 GCs, 0 failures |
| suites ×10 at 1, 2, 3 and 10 carriers | ✅ 0 failures |

**Found along the way:**
- **Run-queue starvation at one carrier:** fixed with a global queue for preempted fibers and a periodic global-first pick (§5.1).
- **Probable fragmentation:** under extreme GC stress (a collection every few milliseconds while 40k fibers were being spawned), MMTk reported 1.1 GB in use for about 70 MB of live data. The likely cause is that non-moving Immix keeps a whole 32 KB block for a few scattered survivors (not yet verified). The fix is opportunistic defragmentation, which moves objects, so the interpreter would have to reload cached pointers after a safepoint. **[open]**

**Deferred** (unchanged from M0, plus):

| Item | Arrives in |
|---|---|
| Dynamic bindings and conveyance | M3, with conditions |
| `try` (tests use `settle` meanwhile) | M3 |
| Error locations (`file:line`) for code loaded through `load-file`, which now runs in Norn | M3 |
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
- `tests/norn/clos_test.norn` (91 checks), also under `NORN_GC_STRESS=1` (1,379 GCs) and at 1, 2, 3 and 10 carriers.
- `bench/dispatch.norn` (the done-check): 8 fibers, a live `defmethod` seen by all within 167 µs; under `NORN_GC_STRESS=1`, 1,895 GCs and still correct.

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

---

## 13. Open questions

1. **Structural-only equality and hashing** (§2.3): confirm, or allow
   user-defined `=`/`hash`, which would push hash maps into Norn.
2. **Multi-shot continuations** (§3.4): keep them out unless a need appears?
3. **Concurrent GC** (§4.1): wait for MMTk, or plan our own?
4. **Parked fibers nothing can wake** are currently collected silently
   (§4.2). Should they instead be reported as a deadlock (as Go does), or
   kept for the debugger (M3)?
