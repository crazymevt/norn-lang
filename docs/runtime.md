# Loom Runtime Architecture (draft 0.1)

Companion to [design.md](design.md). This document covers *how* the Rust
runtime delivers the language's promises: fibers that can be parked and
debugged, fibers saved in images, live redefinition on all cores, and
multicore throughput.

Decisions are marked **[decided]**, **[proposed]** or **[open]**, as in the
design doc.

---

## 0. The three load-bearing decisions

Most of this document follows from these three choices:

1. **Loom frames live in heap-allocated, segmented stacks — not on the
   native stack** (§3). A fiber's stack is ordinary heap data. That makes
   fiber switches cheap, makes stacks visible to the GC precisely, and lets
   the debugger inspect them and images save them.
2. **Rust never calls Loom** (§2.3). Runtime primitives are leaves. Anything
   that needs to call user code (sorting with a comparator, `print-object`,
   reader macros) is written in Loom. This is what makes rule 1 possible:
   there are never Rust frames sandwiched between Loom frames.
3. **One frame format for interpreted and compiled code** (§3.3). The
   bytecode interpreter and Cranelift-compiled code push the same frames, so
   stacks can mix both, the debugger handles them identically, and
   deoptimisation on redefinition is just "resume this frame in the
   interpreter".

---

## 1. Layers

```
┌─────────────────────────────────────────────────────────────────────┐
│ Loom code: stdlib, CLOS/MOP, conditions, compiler, REPL server      │  lib/*.loom
├─────────────────────────────────────────────────────────────────────┤
│ Execution:  bytecode interpreter (tier 0) │ Cranelift JIT/AOT (tier 1)│  loom-vm, loom-jit
├─────────────────────────────────────────────────────────────────────┤
│ Runtime core: values · object model · persistent collections        │
│   fibers & stacks · scheduler · reactor · sync primitives           │  loom-rt
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

A Loom value is one 64-bit word, `#[repr(transparent)] struct Value(u64)`,
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

### 2.3 "Rust never calls Loom" **[decided]**

Rust primitives (`vector-conj`, `string-concat`, `atom-cas`, …) are leaf
functions: they may allocate but never invoke Loom code. Consequences:

- **Equality and hashing are structural and runtime-defined**, so the HAMT in
  Rust can implement `assoc`/`get` without calling back into user code. That
  also guarantees map operations can't deadlock or run user side effects in
  parallel folds. Custom key semantics use a key function. **[open: confirm]**
- **Sorted collections, `sort` with a comparator, `print-object`, reader
  macros and anything else higher-order are written in Loom.**
- **FFI callbacks** are the single exception: they are *continuation
  barriers*. While native code is on the stack, the fiber is pinned to its
  carrier and can't be captured, saved or migrated. Racket's CS backend has
  the same rule.

### 2.4 Collections **[proposed]**

| In Rust (no user callbacks needed) | In Loom |
|---|---|
| Persistent vector (RRB), hash map/set (CHAMP), string, byte-vector, cons | Sorted map/set, lazy streams, transducers, `sort` |
| Transients (with owner-fiber check) | Queues, everything built on the above |

Rust gives them speed while everything runs on the bytecode interpreter.
Once the native compiler is good, moving more into Loom is an option.

---

## 3. Stacks, frames and fibers

### 3.1 Segmented heap stacks **[decided]**

This follows Chez Scheme's design. Each fiber's stack is a chain of **stack
segments**, which are ordinary heap objects (class `StackSegment`).

```
 Fiber ──► current segment (2 KiB initial, doubling to 64 KiB)
            ┌──────────────────────────┐
            │ frame: fn, resume, size  │ ◄─ fp
            │        slots …           │
            │ frame …                  │
            │ underflow frame ─────────┼──► previous segment ─► …
            └──────────────────────────┘
```

- **Overflow:** at function entry, the frame size is compared against the
  segment's limit. On overflow, a new segment is allocated and linked under
  an underflow frame. Deep recursion is therefore bounded only by the heap
  (with a configurable limit that signals `stack-exhausted`).
- **Fiber switch** means saving the frame pointer into the old fiber and
  loading the next fiber's. There are no registers to save between Loom calls
  (§3.2) and no assembly context switching.
- **Memory:** a parked fiber costs its fiber object plus a 2 KiB segment.
  1M fibers ≈ 2–3 GB. Fibers that have never run hold no segment, so spawning
  1M tasks that run in waves is far cheaper.

### 3.2 Calls compile to "push frame + tail call" **[decided]**

The native stack never holds Loom values across a Loom call.

- **Non-tail call:** spill live values into the current Loom frame, record the
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
in Loom.

Multi-shot continuations (copy on reinstatement) are **[open]**; nothing in
the design needs them yet.

### 3.5 Kernel language **[proposed]**

All of Loom macro-expands to roughly 14 forms, which both tiers implement:

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

- **M0 runs on its own NoGC heap** (`loom-rt/src/heap.rs`: bump allocation
  from 4 MiB zeroed chunks, never freed), behind the same `Mutator` API that
  the MMTk binding will implement in M1. The rule that allocation never
  collects (§4.3) was kept from the first commit.
- **First real collector: `StickyImmix`** (generational Immix:
  stop-the-world, parallel, mostly non-moving, defragments opportunistically).
- **Later: concurrent collection**, as MMTk's concurrent plans mature or
  through our own plan. **[open]**
- **One MMTk mutator per carrier thread**, not per fiber. Fibers allocate from
  their carrier's thread-local buffer.

### 4.2 Roots and safepoints **[decided]**

Because Loom values only live in heap stack segments and in carrier
"registers" saved at safepoints, the root set is small:

- The fiber registry (every live fiber → its stack segments).
- Global tables: namespaces → vars, the class registry, interned keywords.
- Each carrier's saved register block.
- The Rust handle stack (§4.3).

A **safepoint poll** is a decrement-and-branch on a per-carrier `poll` word,
placed at function entry and loop back-edges. The same poll serves three
purposes:

| `poll` reaches ≤ 0 because… | Action |
|---|---|
| reduction budget used up (normal) | yield fiber → preemption |
| GC requested (set to `i64::MIN`) | park carrier for stop-the-world |
| REPL interrupt / image save / debugger | park, then handle request |

### 4.3 Rooting in Rust **[decided — revised in M0]**

**Allocation never triggers a collection.** If allocation runs out of room it
takes more memory and sets the poll word, and the collection then happens at
the next safepoint, where every live Loom value is in a heap stack frame.

This removes almost all rooting from Rust. Primitives are leaves (§2.3), so
they contain no safepoints and can hold raw `Value`s across allocation
freely. Handles (`Handle<'scope>`, as in V8's HandleScope) are only needed by
the little Rust code that runs Loom code *and* keeps values across it. In M0
that is only the seed compiler holding forms while it calls a macro.

- **Risk:** a primitive that allocates enormously between safepoints
  (`(vec (range 1e9))`) over-commits the heap. That's acceptable: it's bounded
  by the size of the result, which has to be built anyway.
- **To verify in M1:** MMTk must allow allocation without polling for GC
  (over-commit on allocation failure). If it can't, primitives that
  allocate in a loop will need handles.
- **GC stress mode** (collect at every safepoint, poisoning freed memory)
  still runs the whole test suite in CI.

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

- **Work stealing** uses `crossbeam-deque`. A newly spawned fiber goes to the
  local deque (LIFO slot for the most recent, for cache locality, as in
  Go/Tokio).
- **Fiber states:** `New → Runnable ⇄ Running → {Parked(reason), Done}`. Park
  reasons: channel, I/O, timer, scope-join, lock, **debugger** (an unhandled
  condition: the fiber waits with its stack intact).
- **Blocking FFI:** the fiber enters `InNative`. If the monitor thread sees a
  carrier stay in native code for more than about 50 µs, it starts a spare
  carrier so the other cores stay busy (Go's sysmon approach).
- **Preemption** via the reduction budget (§4.2). Defaults to about 4000
  reductions, tunable per scope.

### 5.2 Dynamic bindings and conveyance **[decided]**

- Each fiber holds a pointer to a **persistent map** of var → value.
  `binding` pushes a new map, and `spawn` copies the pointer (O(1)
  conveyance).
- Each var has a `has-ever-been-dynamically-bound` flag, so reading an
  ordinary global is a single acquire-load of its root.
- Condition handlers and restarts are just dynamic bindings, so they are
  conveyed into children too (design §5.3).

### 5.3 Sync primitives in Rust **[proposed]**

| Primitive | Implementation |
|---|---|
| `atom` | `AtomicU64` CAS on a tagged value, with a write barrier |
| `var` | root slot (release store / acquire load) + flags + watchers |
| `chan` | lock-protected ring buffer, plus parked-fiber wait queues for putters and takers; `select` registers on several and the first winner cancels the rest |
| fiber-aware mutex | parks the fiber instead of blocking the carrier |
| STM `ref` | MVCC in Loom on top of atoms and the locks (if kept in core) |

---

## 6. Generic function dispatch

**[proposed]**

- A generic function object holds an atomically swappable pointer to an
  immutable **dispatch cache**, itself a Loom persistent map: our own data
  structures, used for our own runtime.
- Cache key: the tuple of layouts (not classes — this also catches obsolete
  instances) of the *specialised* argument positions only. Cache value: the
  compiled **effective method** (method combination already applied).
- **Lookup tiers** (borrowed from PCL/SBCL "dfun states"): one entry →
  monomorphic check; 2–8 → linear probe; more → hash. Custom-dispatch generic
  functions key the cache on the dispatch value.
- **Redefinition** (`defmethod`, `defclass`, MOP changes) computes the
  invalidation set, swaps in empty caches and bumps the generic function's
  epoch. Tier-1 call-site inline caches compare the epoch, so a fiber on
  another core sees the change on its next call. No locks are taken on the
  read path.

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
1. loom-rt + loom-vm (Rust) compile.
2. loom-vm contains a small Rust "seed compiler": reader → kernel forms
   (+ non-hygienic defmacro) → bytecode.                 [throwaway, ~2k LOC]
3. Seed compiler loads lib/core/*.loom (stdlib basics, CLOS, conditions).
4. Seed compiler loads lib/compiler/*.loom — the real compiler, in Loom.
5. Real compiler recompiles itself and the stdlib → save base.lim.
6. From here on: `loom` = loom-rt + base.lim. The seed compiler is used only
   to rebuild from scratch (and in CI, to prove we still can).
```

---

## 10. Repository layout

```
loom-lang/
  Cargo.toml                 workspace
  crates/
    loom-rt/                 values, objects, collections, heap (MMTk binding),
                             fibers, scheduler, reactor, sync, images, FFI
    loom-vm/                 bytecode, interpreter, seed compiler, reader (bootstrap)
    loom-jit/                Cranelift backend (M4)
    loom/                    `loom` binary: REPL, run image, build
  lib/
    core/                    kernel macros, seqs, printing, numbers
    clos/                    classes, generic functions, MOP, method combination
    cond/                    condition system, restarts, debugger protocol
    async/                   scopes, channels, processes, supervisors, parallel
    compiler/                expander, ANF, optimiser, codegen drivers
    repl/                    network REPL server
  spikes/
    jit-spike/               platform checks for tier 1 and images (§7.4)
  docs/
  tests/loom/                .loom test suites (run by crates/loom-vm/tests/)
```

---

## 11. Testing strategy

**[proposed]**

- **Concurrency:** the scheduler, channels and atoms are model-checked with
  [`loom`](https://github.com/tokio-rs/loom) (Tokio's permutation tester —
  yes, same name) and/or `shuttle`.
- **GC:** stress mode (collect on every allocation, poison freed memory) runs
  in CI on the whole test suite.
- **Differential:** every `.loom` test runs on tier 0 and tier 1, and the
  results must agree. Images are tested by round-tripping (save → load → run
  the suite).
- **Fuzzing:** the reader, the bytecode verifier and image load (a malformed
  image must not crash the runtime).
- **Miri** on the `unsafe` core of `loom-rt`, wherever the code is
  Miri-compatible.

---

## 12. Revised milestones

These replace design.md §10.3; the order changed so that GC and fibers land
together.

| # | Milestone | Done when |
|---|---|---|
| M0 ✅ | Workspace, `Value`/object model, cons/vector/map/string, reader, printer, seed compiler, tier-0 VM on heap stacks, REPL; NoGC heap | `(reduce + (range 1000000))` at the REPL; macOS JIT spike works |
| M1 | Fibers, scheduler, safepoints, `StickyImmix`, atoms, channels, scopes | 1M fibers ping-pong across all cores under GC stress |
| M2 | CLOS in Loom: `defclass`, layouts, generic functions, method combination, dispatch caches | Live `defmethod` while 8 cores call the generic function |
| M3 | Conditions, restarts, parked-fiber debugger, REPL protocol | Fix a crashed fiber from the editor and resume it |
| M4 | Real compiler in Loom, Cranelift tier 1, deopt, self-host | Compiler compiles itself; tier 0 and tier 1 agree on all tests |
| M5 | Images: save/load, layers, parked fibers, `on-image-restore` | Under 50 ms startup; a saved fiber resumes after load |
| M6 | Hygiene, modules/phases, `#lang`, sealing | Stdlib built as sealed modules |
| M7 | Full MOP, STM, processes/supervisors, FFI, gradual types, contracts | — |

### 12.1 M0 status (2026-11-13)

**Built:**
- **`loom-rt`:**
  - tagged values and the object model;
  - the NoGC heap;
  - persistent vector (32-way trie with tail) and HAMT map/set;
  - interned symbols and keywords;
  - structural `=`/`hash`;
  - seq abstraction, printer, namespaces and vars.
- **`loom-vm`:**
  - the reader (`#()`, syntax-quote with qualification and `x#` gensyms, `#lang`, `#_`, `#|…|#`);
  - the seed compiler (kernel forms plus macro expansion; flat closures; `letrec*` fix-ups; `recur` as a backward jump);
  - the register-based bytecode interpreter on heap segmented stacks (proper tail calls, a spare-segment cache, stack-overflow detection, Ctrl-C interrupt at the safepoint poll, Loom backtraces);
  - 103 primitives.
- **`lib/core/core.loom`:** 88 macros and functions, from `defn` and `cond` to `sort-by` and `update-in`.
- **`loom`:** a REPL with multi-line input, plus `loom FILE` and `loom -e EXPR`.
- **Tests:** 6 runtime unit tests, plus 89 Loom checks and REPL-level error/printing tests in `cargo test`.

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

---

## 13. Open questions

1. **Structural-only equality and hashing** (§2.3): confirm, or allow
   user-defined `=`/`hash`, which would push hash maps into Loom.
2. **Multi-shot continuations** (§3.4): keep them out unless a need appears?
3. **Concurrent GC** (§4.1): wait for MMTk, or plan our own?
4. **MMTk over-commit** (§4.3): confirm that MMTk can allocate without
   polling for GC. This is the first thing to check in M1.
