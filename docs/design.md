# Norn — Language Design (draft 0.1)

> A general-purpose Lisp that takes CLOS, the condition system and images from
> Common Lisp; immutable data and identity/state separation from Clojure;
> hygienic macros and modules from Racket — on a runtime built from day one
> for many cores.

Status: **draft for discussion**. Every section marks decisions as
**[decided]** (recommended, unlikely to change) or **[open]** (needs a call).

---

## 1. Design principles

1. **Values by default, identities on purpose.** Data is immutable and freely
   shareable across threads. Anything that changes over time is an explicit
   reference type (atom, var, channel, process…). This is what makes
   "highly parallel" tractable in a dynamic language.
2. **Everything is live.** Every definition — functions, classes, methods,
   macros — can be redefined at the REPL while the program runs on all cores,
   and running code picks it up safely.
3. **Errors are conversations, not crashes.** The CL condition system with
   restarts, extended to concurrency: a failing fiber *pauses* and asks
   (a handler, a supervisor, or you at the REPL) what to do.
4. **The image is a first-class artifact; the source is the truth.** Images
   make startup and live development fast; reproducible builds still come
   from source modules.
5. **Concurrency in the runtime from the first commit.** Retrofitting
   thread-safety (Python's GIL, OCaml multicore, SBCL threads) is the
   most expensive mistake a runtime can make. We don't make it.

### What we deliberately leave behind

| From | Leaving behind | Why |
|---|---|---|
| Common Lisp | Lisp-2, mutable conses everywhere, `nil` = `()` = false pun, global symbol-interned packages | Hostile to concurrency and to hygiene |
| Clojure | JVM startup/stack traces, no TCO (`recur`), lazy-seq + dynamic-binding surprises, no restarts, single-dispatch protocols, no method combination | We control the VM |
| Racket | Weak shared-memory parallelism (places/futures), no images | Core goals of Norn |

---

## 2. Syntax and reader

**[decided]** S-expressions with Clojure's literal data syntax, plus a few
CL/Racket conveniences.

```clojure
42  3/4  1.5e3  #c(1 2)          ; full numeric tower: fixnum, bignum, ratio, float, complex
"utf-8 string"  \a  \newline     ; immutable UTF-8 strings, chars
:keyword  ::ns-local  'symbol
(list 1 2)  [1 2 3]  {:a 1}  #{1 2}
#"regex"   #(+ % 1)              ; regex, short lambda
^{:doc "metadata"} x
#| block comment |#   #; (datum comment)
```

- **[decided] Lisp-1.** One namespace for functions and values (Clojure,
  Racket). Hygiene makes the CL Lisp-2 argument moot.
- **[decided] Truthiness:** `nil` and `false` are falsey; everything else
  (including `()`, `0`, `""`) is truthy. `()` is not `nil`.
- **[decided] Extensible reader, but scoped.** CL-style reader macros are
  allowed only through a module's `#lang` / readtable declaration — never
  globally mutated — so one library can't break another's parsing.
- **[decided] Syntax objects carry source locations** everywhere, so errors,
  the debugger and editor tooling always point at real source.

---

## 3. Data model

### 3.1 Immutable core **[decided]**

- Persistent vectors (RRB-trees: O(log n) concat/slice), hash maps & sets
  (HAMT/CHAMP), sorted maps & sets, lists, queues.
- **Transients** for fast local building (`transient` / `persistent!`),
  checked to be confined to the fiber that created them.
- Value equality `=` on all immutable data; `identical?` for identity.
- Everything — including built-ins like `Integer`, `Vector`, `String` — has a
  class, so you can `defmethod` on anything (§4).

### 3.2 Identities (reference types) **[decided]**

| Type | Semantics | Use |
|---|---|---|
| `var` | Late-bound global with root value + fiber-local dynamic bindings | All top-level definitions; redefinable |
| `atom` | Lock-free CAS cell, `swap!` / `reset!` | Uncoordinated shared state |
| `ref` + `dosync` | MVCC software transactional memory | Coordinated multi-cell updates (**[open]**: core or `norn.stm` library) |
| `volatile` | Unsynchronized cell, fiber-confined | Hot local state |
| `chan` | CSP channel, buffered/unbuffered | Pipelines, coordination |
| `process` | Fiber + mailbox (actor) | Isolated stateful services |

### 3.3 Sequences and laziness **[open]**

Clojure's pervasive lazy seqs interact badly with dynamic binding, resource
scopes and parallelism (work leaks out of the scope it was meant to run in).
Proposal: core sequence functions on concrete collections are **eager and
type-preserving** (`(map inc [1 2])` → `[2 3]`); laziness is explicit via
`lazy` streams; heavy pipelines use **transducers**, which also parallelise
(§6.6).

---

## 4. Object system: generic functions & CLOS

Note: CLOS is Common Lisp's. Clojure's analogues (protocols and
multimethods) are folded in here as special cases.

### 4.1 The basics **[decided]**

```clojure
(defclass shape () ())

(defclass circle (shape)
  ((radius :type real :reader radius)))

(defclass rect (shape)
  ((w :reader w)
   (h :reader h)))

(defgeneric area (s))
(defmethod area ((c circle)) (* pi (radius c) (radius c)))
(defmethod area ((r rect))   (* (w r) (h r)))

;; Multiple dispatch
(defgeneric collide (a b))
(defmethod collide ((a circle) (b rect)) ...)
(defmethod collide ((a rect) (b circle)) (collide b a))

;; Standard method combination
(defmethod area :around ((s shape))
  (log/trace "area" s)
  (call-next-method))

(defmethod save :before ((d document)) (validate d))
```

- Multiple inheritance with C3 linearization (CL's CPL is close; C3 is
  more predictable).
- Specializers: classes, `(eql value)`, and **custom dispatch**
  (`(defgeneric render (x) (:dispatch (fn [x] (:format x))))`) — which gives
  Clojure multimethods for free.
- Method combinations: `standard` (`:before :after :around`), `+`, `and`,
  `or`, `list`, `progn`, plus `define-method-combination`.
- **Protocols** are sugar: a named bundle of generic functions you can
  `extend` to existing classes, including built-ins.

### 4.2 Instances are values by default **[decided]**

This is the big divergence from CLOS, and it's what lets objects cross
threads freely.

```clojure
(def c (make circle :radius 3))     ; or (circle :radius 3)
(radius c)                          ; => 3
(def c2 (with c :radius 4))         ; functional update, c unchanged
(:radius c)                         ; instances also behave like read-only maps
```

Mutable objects are opt-in and **owned**:

```clojure
(defclass connection-pool ()
  ((conns :mutable true))
  (:mutable))                        ; instances have an owner fiber
```

- A mutable instance belongs to the fiber that created it. Access from another
  fiber signals `ownership-error` in safe mode; ownership can be
  explicitly transferred (`(send! ch obj)` transfers, `(share obj)` wraps it in
  a lock).
- Checks compile away under `(declare (optimize (safety 0)))`.

### 4.3 Meta-object protocol **[decided]**

Full MOP: classes are instances of `standard-class`; generic functions of
`standard-generic-function`; customise via `compute-applicable-methods`,
`compute-effective-method`, `slot-value-using-class`, metaclasses, etc.
Like SBCL, the runtime keeps a fast path that is used whenever the slow MOP
hooks haven't been specialised.

### 4.4 Live redefinition under concurrency **[decided]**

- **Dispatch caches** are per generic function, read lock-free, replaced
  copy-on-write. Each GF has an epoch; compiled call sites keep an inline
  cache tagged with that epoch, so a check costs one load and one compare.
  `defmethod` bumps the epoch → every core sees the new method on its next
  call.
- **Class redefinition** keeps CL semantics: each instance header points at a
  class *version*; on first access after a redefinition the instance is
  migrated (`update-instance-for-redefined-class`) under a per-object CAS.
  Because instances are immutable from the program's point of view, migrating
  them in place is invisible.

---

## 5. Live programming: REPL, conditions, debugger

### 5.1 The REPL **[decided]**

- Vars are late-bound: redefining `foo` at the REPL affects every caller on
  every core immediately (Clojure semantics).
- **Network REPL protocol** (in the spirit of Swank/nREPL) is part of the
  runtime, not a library: eval, completion, docs, inspect, xref (who-calls,
  who-specialises), macroexpand, list/attach fibers, profile.
- **Source drift tracking:** the REPL records definitions entered
  interactively that don't match any file, so `(unsaved-definitions)` tells you
  what your image knows that your repo doesn't.

### 5.2 Condition system **[decided]**

The full CL condition system — `signal`, `handler-bind`, `handler-case`,
`restart-case`, `invoke-restart` — so handlers run *before* the stack unwinds
and can choose a recovery strategy.

```clojure
(defn parse-entry [line]
  (restart-case (parse line)
    (skip-entry [] nil)
    (use-value [v] v)))

;; The policy is chosen far away from where the error happens —
;; and it is conveyed into every fiber spawned inside.
(handler-bind [(parse-error (fn [e] (invoke-restart 'skip-entry)))]
  (pmap parse-entry lines))
```

### 5.3 Conditions × concurrency **[decided]**

- Handlers and restarts are dynamic bindings, so they are **conveyed** to
  child fibers at spawn.
- When a condition goes unhandled in a fiber, the fiber **parks with its stack
  intact** instead of dying. Its scope/supervisor is notified and, in
  development, the REPL receives a debugger event: inspect frames, eval in a
  frame, redefine the broken function, pick a restart (including
  `retry-frame`), and the fiber continues. All other fibers keep running.
- In production, a policy (per scope or per supervisor) picks a default
  restart, e.g. `abort-fiber` → propagate to the parent scope.

---

## 6. Concurrency and parallelism

### 6.1 Runtime model **[decided]**

- **Fibers** (green threads) are the unit of concurrency: a few hundred bytes
  each, growable segmented stacks, millions per process.
- **M:N work-stealing scheduler** with one carrier OS thread per core.
- **Preemptive:** fibers yield at safepoints (function entry and loop
  back-edges, using a reduction counter as in BEAM), so a runaway loop can't
  starve the scheduler — and you can interrupt it from the REPL.
- **Async I/O underneath** (io_uring / kqueue / IOCP): code is written
  in direct style, with no `async`/`await` colouring. Blocking FFI calls pin
  their carrier, and the scheduler adds a compensating thread (as Go does).
- **Delimited continuations** are the underlying primitive; fibers,
  generators and effect-style handlers are built on them. Undelimited
  `call/cc` is not provided.

### 6.2 Structured concurrency **[decided]**

Fibers live inside scopes; a scope can't exit until its children finish.
Errors propagate upward and cancellation propagates downward.

```clojure
(defn fetch-all [urls]
  (with-scope [s]
    (let [tasks (mapv #(spawn s (http/get %)) urls)]
      (mapv await tasks))))      ; if one fails, siblings are cancelled
```

`spawn-detached` exists for the rare fiber that should outlive its scope; it
must be attached to a supervisor.

### 6.3 Shared state

Atoms, vars, STM refs (§3.2). Immutable values make reads free; the only
synchronisation is on the identity.

### 6.4 Channels (CSP) **[decided]**

```clojure
(let [c (chan 64)]
  (spawn (doseq [x (range 100)] (put! c x)) (close! c))
  (select
    [c v]         (handle v)
    [(timeout 1000)] (log/warn "slow")))
```

### 6.5 Processes and supervision **[decided]**

Erlang-style processes (fiber + mailbox + pattern-matching `receive`),
links and monitors, and supervisors in the standard library — for
long-lived, fault-isolated services.

```clojure
(defprocess counter [n]
  (receive
    [:inc]        (recur (inc n))
    [:get from]   (do (send from n) (recur n))))

(def sup (supervisor {:strategy :one-for-one}
           [(child counter 0)]))
```

### 6.6 Data parallelism **[decided]**

Fork/join on the same work-stealing pool:

```clojure
(pmap f xs)                              ; parallel map, order-preserving
(preduce + 0 xs)                         ; parallel tree reduction
(into [] (comp (filter odd?) (map sq)) (parallel xs))   ; parallel transducers
```

Persistent vectors and maps split in O(log n), which is what makes parallel
folds over them cheap.

### 6.7 Memory and GC **[open: implementation]**

- Thread-local allocation buffers, generational, **concurrent + parallel**
  collection.
- Immutability is a big GC win: immutable objects can only point to *older*
  objects (except at construction), so old→young write barriers fire rarely.
- Recommendation: build on **MMTk** (Rust GC toolkit: Immix and friends) rather
  than writing a concurrent collector from scratch. Start with stop-the-world
  parallel Immix at safepoints, then move to concurrent collection.

---

## 7. Images

### 7.1 What an image is **[decided]**

A snapshot of the heap (code, classes, generic functions, vars, data), and
optionally parked fibers, in an mmap-able, relocatable format. Loading means
mapping the file, which gives instant startup.

```clojure
(save-image "app.lim"
  :entry  #'myapp/main
  :fibers :parked          ; :none | :parked
  :shake  true)            ; tree-shake unreachable code for deployment
```

```sh
norn --image app.lim
```

### 7.2 Images and concurrency **[decided]**

- Saving brings every fiber to a safepoint (stop-the-world), then writes the
  heap.
- Because fibers are heap objects with heap-allocated stacks, **parked fibers
  can be saved and resumed**, which SBCL's `save-lisp-and-die` can't do with
  threads running.
- External resources (sockets, files, FFI handles) are marked dead on save. On
  restore, the runtime calls the `on-image-restore` generic function, and any
  fiber that touches a dead handle gets a `resource-lost` condition with
  `reconnect` / `abort` restarts.

### 7.3 Layered images **[decided]**

`base.lim` (runtime + compiler + stdlib) → `deps.lim` → `app.lim`. Each layer
stores only its delta. This makes dev images fast to save, and deployment
images small.

### 7.4 Images vs reproducibility **[decided]**

Source modules are the source of truth; `norn build` produces images
deterministically from source. Dev images are "pets" (source-drift tracking,
§5.1, keeps them honest); build images are "cattle".

---

## 8. Modules, macros, compilation

### 8.1 Modules and namespaces **[decided]**

- One file = one module = one namespace. Symbols are just names; vars live in
  namespaces (Clojure's model, not CL's global symbol interning).
- `#lang` line (optional, Racket-style) selects reader + base language, so
  DSLs and alternate syntaxes are first-class.

```clojure
#lang norn
(ns myapp.core
  (:require [norn.async :as a]
            [myapp.db :refer [query]]))
```

- **Dev mode:** namespaces are open and every call goes through its var
  (fully live).
- **Release mode:** modules are *sealed*, which enables direct linking,
  inlining and dead-code elimination (Clojure's direct linking, but per module
  and reversible: unsealing at the REPL deoptimises).

### 8.2 Macros **[decided]**

- **Hygienic by default:** Racket's sets-of-scopes model, `syntax-parse` for
  macros with real error messages.
- **`defmacro` for quick work:** CL-style, operating on datums with
  auto-gensym (`x#`, as in Clojure syntax-quote); implemented on top of the
  hygienic expander, so the two mix.
- **Phase separation** (Racket) for module compilation, so separate
  compilation is deterministic. The REPL top level is deliberately more
  permissive ("the top level is hopeless" — we embrace it there and nowhere
  else).

### 8.3 Compilation pipeline **[open: implementation]**

```
reader → expander (hygiene, phases) → core forms → Norn IR (CPS/ANF)
      → optimiser (inlining, type inference, unboxing, escape analysis)
      → backend: bytecode interpreter  |  native via Cranelift (JIT & AOT)
```

- Every `defn` at the REPL is compiled to native code (Cranelift compiles fast
  enough for interactive use), as SBCL does, rather than interpreted.
- **[decided] Proper tail calls are guaranteed** (no `recur` needed, though it
  is kept as an assertion that a call is in tail position).

### 8.4 Types **[decided]**

Dynamically typed with **optional, gradual annotations**: CL-style
`declare` for optimisation, and Typed-Racket-style checked signatures and
**contracts** at module boundaries, with blame.

```clojure
(defn dot ^double [^doubles a ^doubles b]
  (declare (optimize (speed 3)))
  ...)
```

---

## 9. Interop

- C FFI (`defcfun`, structs, callbacks) with pinning integrated into the GC
  and blocking calls integrated into the scheduler (§6.1).
- **[open]** Whether to target a second host later (e.g. WASM), or stay with
  the native runtime only.

---

## 10. Implementation strategy

### 10.1 Host **[decided: A — own runtime in Rust]**

Detailed runtime architecture: [runtime.md](runtime.md).

| Option | Pros | Cons |
|---|---|---|
| **A. Own runtime in Rust** (MMTk GC, Cranelift JIT/AOT) — *recommended* | Full control: images, parked-fiber snapshots, TCO, preemption, restarts that keep stacks | Biggest effort; GC + scheduler are serious work |
| B. Host on JVM (virtual threads) | GC, JIT, threads and ecosystem for free | Images (CRaC is partial), no TCO, slow startup, can't own the fiber stack format — breaks §5.3 and §7.2 |
| C. Host on Chez Scheme (as Racket does) | Superb compiler, continuations, TCO | Stop-the-world GC, limited thread story |
| D. Host on BEAM | World-class concurrency & supervision | Poor shared-memory parallel performance; no shared mutable state at all |

Only option A delivers all of: images with fibers, debugger-parked fibers,
and full-speed multicore. B is the fastest way to a usable language, but it
would compromise three of the five principles.

### 10.2 Bootstrapping (assuming A) **[proposed]**

The compiler is written in Norn and lives in the base image (like SBCL).
The Rust core holds only the runtime.

```
norn-rt  (Rust)   GC, scheduler, object layout, primitives, image I/O, FFI, Cranelift glue
norn0    (Rust)   tiny interpreter for core forms; exists only to bootstrap
compiler (Norn)   expander + optimiser + codegen; compiled by norn0, then by itself
stdlib   (Norn)
→ base.lim
```

### 10.3 Milestones **[superseded — see [runtime.md §12](runtime.md#12-revised-milestones)]**

| # | Milestone | Done when |
|---|---|---|
| M0 | Reader, printer, persistent data structures, `norn0` interpreter, basic REPL | `(reduce + (range 1e6))` at a REPL |
| M1 | **Fibers + scheduler + safepoints + atoms + channels** (concurrency from day one) | 1M fibers ping-ponging across all cores |
| M2 | Generic functions, `defclass`, method combination, dispatch caches | Shapes example + live `defmethod` under load |
| M3 | Condition system, restarts, parked-fiber debugger over the REPL protocol | Fix a crashed fiber from the editor and resume it |
| M4 | Compiler in Norn → Cranelift; self-host; proper tail calls | Compiler compiles itself |
| M5 | Images: save/load, layering, `on-image-restore`, parked fibers | Sub-50ms startup of a saved app image |
| M6 | Hygienic macros + modules/phases, `#lang`, sealing | Stdlib rebuilt as sealed modules |
| M7 | MOP completeness, STM, processes/supervisors, FFI, gradual types, contracts | — |

---

## 11. Open questions (summary)

1. ~~**Host**~~ — decided: own Rust runtime (§10.1, [runtime.md](runtime.md)).
2. **Laziness:** eager type-preserving core + explicit lazy streams, or
   Clojure-style lazy-by-default — §3.3.
3. **STM:** in core or as a library — §3.2.
4. **Mutable objects:** enforce ownership checks (proposal), or trust the
   programmer like CL — §4.2.
5. **Primary audience:** what should the first real program written in Norn be?
   (It will shape stdlib priorities: web services, data processing, games,
   tooling…)
