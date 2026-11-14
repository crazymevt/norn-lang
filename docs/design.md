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
42  3/4  1.5e3  #c(1 2)          ; numeric tower: fixnum, bignum, ratio, float (complex later, as a library)
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
  allowed only through a module's own readtable declaration (in its `ns`
  form, now that `#lang` is dropped) — never globally mutated — so one
  library can't break another's parsing. None exists yet.
- **[decided] Syntax objects carry source locations** everywhere, so errors,
  the debugger and editor tooling always point at real source.

---

## 3. Data model

### 3.1 Immutable core **[decided]**

- Persistent vectors (RRB-trees: O(log n) concat/slice), hash maps & sets
  (HAMT/CHAMP), sorted maps & sets, lists, queues.
- **Transients** for fast local building (`transient` / `persistent!`),
  checked to be confined to the fiber that created them.
- Value equality `=` on all immutable data; `identical?` for identity. See
  §3.4.
- Everything — including built-ins like `Integer`, `Vector`, `String` — has a
  class, so you can `defmethod` on anything (§4).

### 3.2 Identities (reference types) **[decided]**

| Type | Semantics | Use |
|---|---|---|
| `var` | Late-bound global with root value + fiber-local dynamic bindings | All top-level definitions; redefinable |
| `atom` | Lock-free CAS cell, `swap!` / `reset!` | Uncoordinated shared state |
| `ref` + `dosync` | MVCC software transactional memory | Coordinated multi-cell updates (**[decided]**: the `norn.stm` library, not the core) |
| `volatile` | Unsynchronized cell, fiber-confined | Hot local state |
| `chan` | CSP channel, buffered/unbuffered | Pipelines, coordination |
| `process` | Fiber + mailbox (actor) | Isolated stateful services |

### 3.3 Sequences and laziness **[decided, 2026-11-13]**

Clojure's pervasive lazy seqs interact badly with dynamic binding, resource
scopes and parallelism (work leaks out of the scope it was meant to run in).
So:

- **Core sequence functions are eager and type-preserving.** They run when
  called, inside the caller's bindings and scope, and return the kind of
  collection they were given:

  | Input | `map`, `filter`, `remove`, `take`, `drop`… return |
  |---|---|
  | vector | vector: `(map inc [1 2])` → `[2 3]` |
  | list | list, in the same order |
  | set | set |
  | map | functions that select items (`filter`, `remove`, `take`, `drop`, `take-while`, `drop-while`, `distinct`) give a map. Functions that compute new items (`map`, `keep`, `mapcat`, `map-indexed`, `interpose`) give a vector, since the results needn't be entries; `into {}` covers map-to-map |
  | range, string or other seqable | vector |

- **Laziness is explicit.** A `lazy` stream is a value you create on purpose:
  `(lazy coll)`, or a generator such as `(iterate f x)`, `(repeat x)`,
  `(cycle xs)`, `(range)` with no end, or a line stream over a file. Sequence
  functions on a stream return a stream, so laziness spreads only from a
  stream you made. `vec`, `into` and `reduce` realise one.
- **Heavy pipelines use transducers**, which also parallelise (§6.6).

*Status:* built in M7.
- Streams are a heap kind realized by the interpreter (runtime.md §5.4).
  `cons` keeps a stream tail unrealized.
- `first`, `rest`, `next`, `seq` and `nth` realize one cell at a time, so
  they work on infinite streams.
- Anything else that needs a stream's items (`vec`, `count`, `=`,
  printing) realizes all of it.
- With one argument fewer, the sequence functions are transducers: `into`,
  `transduce`, `sequence`, `completing`, `cat`, `reduced`.
- Two fibers realizing the same cell may both run its thunk; the first
  result is kept. Keep thunks free of side effects.

### 3.4 Equality and hashing **[decided, 2026-11-13]**

`=` and `hash` are defined by the runtime, structurally, and never run user
code. So a hash-map operation can't block, deadlock or cause side effects,
even inside a parallel fold (runtime.md §2.3).

- **Values compare by structure.**
  - Strings and keywords compare by contents; maps compare by entries; sets
    by members.
  - Sequential collections are equal when their elements are:
    `(= [1 2] '(1 2))`.
  - Instances are equal when their class is the same and their slots are.
- **Identity types compare by identity:** atoms, vars, channels, fibers,
  scopes, functions, generic functions and classes.
- **Classes can change the rule declaratively**, and the runtime reads the
  declaration (no user code runs):
  - `(defclass Account () (…) (:equality :identity))`: instances compare by
    identity, as entities do. This is the expected rule for mutable objects
    (§4.2).
  - The slot option `:compare false` leaves a slot (a cache, a timestamp)
    out of both `=` and `hash`.
- **Anything more exotic uses a key function**, e.g. case-insensitive keys:
  `(group-by str/lower-case xs)`, or a map keyed by `(key-fn x)`.
- **`=` is an equivalence relation:** NaN equals NaN, and `-0.0` equals
  `0.0`. So every value can be found again as a map key or set member. `==`
  keeps IEEE semantics (NaN ≠ NaN).
- **Numbers** (with the M7 numeric tower):
  - integers are equal whatever their representation (a bignum that fits is
    stored as a fixnum);
  - ratios are normalised (`2/4` is `1/2`, `2/1` is `2`);
  - `=` keeps the categories apart: `(= 1 1.0)` and `(= 1/2 0.5)` are
    false. `==` compares numerically across them.
- **Across kinds:** maps never equal sets; strings never equal vectors of
  characters; instances never equal maps.
- **Redefined classes:** an instance made before a `defclass` redefinition
  compares with newer ones through the class's current layout. A slot it
  lacks counts as that slot's default.

*Status:* built in M7: structural equality, identity types, number
categories, reflexive NaN, the numeric-tower rules (runtime.md §2.5), and
the `:equality` and `:compare` declarations, which the runtime reads from a
policy kept in each class.

---

## 4. Object system: generic functions & CLOS

Note: CLOS is Common Lisp's. Clojure's analogues (protocols and
multimethods) are folded in here as special cases.

**Status (M2):** §4.1, §4.2 (immutable instances) and §4.4 are implemented;
see [runtime.md §6](runtime.md#6-generic-function-dispatch). Not yet built:
`define-method-combination`, protocol sugar, mutable owned instances, the
full MOP (§4.3), `print-object`, and `initialize-instance`. Slot readers are
plain functions for now, not generic functions.

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
- **Every value has a class.** The builtin classes are capitalised vars in
  `norn.core`, so they never clash with functions of the same name:
  `Object`, `Number`, `Integer`, `Float`, `String`, `Symbol`, `Keyword`,
  `Null`, `Boolean`, `Character`, `Collection`, `Sequential`, `List`,
  `Vector`, `Seq`, `Map`, `Set`, `Function`, `GenericFunction`, `Class`,
  `Var`, `Fiber`, `Channel`, `Atom`, `Scope`. User classes inherit from
  `StandardObject`. `(defmethod describe ((x Integer)) …)` extends a builtin
  class directly.
- Specializers: classes, `(eql value)`, and **custom dispatch**
  (`(defgeneric render (x) (:dispatch :format))`), which gives Clojure
  multimethods for free:

  ```clojure
  (defmethod render :html [x] …)        ; dispatch value, then [params]
  (defmethod render [:md :fancy] [x] …) ; vector dispatch values work too
  (defmethod render :default [x] …)
  ```
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

Mutable objects are opt-in and **owned** **[decided, 2026-11-13]**:

```clojure
(defclass connection-pool ()
  ((conns :mutable true) (size))
  (:mutable))                        ; instances have an owner fiber

(set-slot! pool :conns [])           ; only settable slots
(update-slot! pool :conns conj c)
```

- **Declaring.** `(:mutable)` on the class, and `:mutable true` on each slot
  that can be set; `set-slot!` and `update-slot!` write them. A mutable class
  always compares by identity (§3.4): a hash that changed while the object
  sat in a map would lose it.
- **Ownership states:**
  - *owned* by a fiber: the creator at first;
  - *free*: the next fiber to touch the object claims it (compare-and-swap);
  - *shared*: see below.

  Fibers, not threads, are owners, since fibers migrate between carriers.
- **Checks.** A read or write by a fiber other than the owner signals
  `OwnershipError`. The exception: the debugger, inspector and printer read
  without a check, since they only observe. Checks are always on until
  `(declare (optimize (safety 0)))` exists (M9/M10), which removes them.
- **Hand-off.**
  - `(release! obj)` makes an object free.
  - A mutable object put directly on a channel is released automatically, and
    the receiver claims it on first touch.
  - An object nested inside other data is released explicitly before sending.
- **Owners that go away.**
  - When the owning fiber finishes, its objects become free. Nothing is
    tracked: an access just sees that the owner is done.
  - When an image is restored, objects whose owner wasn't saved are free.
- **Sharing.** `(share obj)` makes an object shared. Every access then takes a
  per-object lock that parks the fiber instead of spinning. `(locking obj …)`
  makes a multi-slot update atomic. Deadlocks are left to deadlock detection
  (runtime M11).
- **Why check at all.** Races on slots can't corrupt memory: a slot is one
  aligned word and the GC doesn't move objects. Ownership turns silent logic
  races into clear errors, for one load and compare per access.
- **Cost:** a hidden owner word, on instances of mutable classes only.

*Status:* built in M7 (`norn-vm/src/owned.rs`). Details decided while
building:
- A shared object's single-slot reads don't take its lock; only changes
  do. A read is one atomic word; use `locking` when several reads must
  agree.
- Bulk views (`keys`, `vals`, `seq`, the printer) only observe, and aren't
  checked.
- `owner` reports the owning fiber, `:shared` or nil.
- `with` on a mutable object makes a copy owned by the caller.
- `OwnershipError` is a subclass of `RuntimeError`.
- Subclasses of a `(:mutable)` class are mutable too.

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
- **Class redefinition** keeps the class object (and so every reference to it)
  and gives it a new *layout*. Instances are never rewritten. Each one keeps
  the layout it was made with and is read *through* it, as the class is now
  defined:
  - a slot the class added reads as that slot's initform, evaluated at
    redefinition time;
  - a slot the class removed is gone;
  - `with` produces an instance in the current layout.

  (As built in M2. This replaces the in-place migration first proposed here:
  for immutable values there is nothing to migrate.)

---

## 5. Live programming: REPL, conditions, debugger

**Status (M3):** §5.2 and §5.3 are implemented, along with the debugger and an
editor protocol (a subset of §5.1). See [runtime.md §5.4](runtime.md). Not
yet built: xref, profiling, source-drift tracking, and output capture for
editor evaluations.

### 5.1 The REPL **[decided]**

- Vars are late-bound: redefining `foo` at the REPL affects every caller on
  every core immediately (Clojure semantics).
- **Network REPL protocol** (in the spirit of Swank/nREPL) is part of the
  runtime, not a library: eval, completion, docs, inspect, xref (who-calls,
  who-specialises), macroexpand, list/attach fibers, profile.
  - **M3 builds the core of it** (`norn --server PORT`). Messages are Norn
    data, one map per line, so any editor can speak it with a socket and a
    small reader.
  - **Ops:**
    - M3: `:eval`, `:debugging`, `:backtrace`, `:restart`,
      `:restart-frame`, `:abort`, `:eval-in-frame`, `:complete`;
    - M7: `:format` and `:lint`;
    - M7.6:
      - `:eval` with `:file` and `:line` (definitions and errors point at
        the source);
      - `:load-file`, and `:interrupt` (a running request, by its id);
      - `:doc` and `:definition` (a symbol's docstring, arglists, file and
        line);
      - `:macroexpand` (`:all` for a full expansion);
      - `:inspect` (`:code`, or `:target`, a part's id, with `:offset` and
        `:limit`) and `:inspect-clear`;
      - `:ns-vars`, and `:test` (run test files: the results as data).
  - **Pushed events:** `{:event :debug …}` and `{:event :resumed …}`. Since
    M7.6, what a request prints streams as `{:event :out :id … :text …}`.
  - A request that fails or is interrupted is still answered with its
    `:id`.
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
(handler-bind [parse-error (fn [e] (invoke-restart 'skip-entry))]
  (pmap parse-entry lines))
```

As built (M3):
- **Constructs:** `try`/`catch`/`finally` (Clojure style) expands to
  `handler-case` + `unwind-protect`. Also `ignore-errors`, `catch-tag`/`throw-tag`,
  `with-simple-restart`, `signal`, `warn`.
- **Condition classes** are CLOS classes: `Condition` > `SeriousCondition` > `Error` >
  `SimpleError`/`RuntimeError`, and `Warning`. Define your own with `defclass`.
- **Every runtime error is resumable.** Errors raised inside the runtime
  (divide by zero, unbound symbol, wrong arity, a failed `await`…) are
  signalled as `RuntimeError` at the failing operation, with two restarts:
  - `use-value`: the operation returns that value;
  - `retry`: the operation runs again, e.g. after defining the missing
    function.

### 5.3 Conditions × concurrency **[decided]**

- Handlers and restarts are dynamic bindings, so they are **conveyed** to
  child fibers at spawn.
  - **As built:** bindings, `handler-bind` handlers and the debugger mode are
    conveyed.
  - `handler-case` handlers and `restart-case` restarts are *not*: they unwind
    to a point on the establishing fiber's stack, which another fiber can't
    reach.
- When a condition goes unhandled in a fiber, the fiber **parks with its stack
  intact** instead of dying. Its scope/supervisor is notified and, in
  development, the REPL receives a debugger event: inspect frames, eval in a
  frame, redefine the broken function, pick a restart (including
  `retry-frame`), and the fiber continues. All other fibers keep running.
  - **As built:** this happens when `*debugger*` is `:park`, which the REPL
    and editor evaluations bind and their spawned fibers inherit. Otherwise
    the fiber unwinds (running cleanups) and ends with the error.
  - "Retry frame" is `debug-restart-frame` (`:f N` at the REPL). It calls the
    frame's function again with its original arguments, using the function's
    *newest* definition.
  - A frame replaced by a successful tail call is gone and can't be
    restarted. A tail call that *fails* keeps its frame.
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
optionally parked fibers, in an mmap-able, relocatable format. Image files
use the extension `.nimg`. Loading means
mapping the file, which gives instant startup.

```clojure
(save-image "app.nimg"
  :entry  #'myapp/main
  :fibers :parked          ; :none | :parked
  :shake  true)            ; tree-shake unreachable code for deployment
```

```sh
norn --image app.nimg
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

`base.nimg` (runtime + compiler + stdlib) → `deps.nimg` → `app.nimg`. Each layer
stores only its delta. This makes dev images fast to save, and deployment
images small.

### 7.4 Images vs reproducibility **[decided]**

Source modules are the source of truth; `norn build` produces images
deterministically from source. Dev images are "pets" (source-drift tracking,
§5.1, keeps them honest); build images are "cattle".

### 7.5 As built (M5)

Images work as described above, with these details and gaps (runtime.md §7.5):

- **Resources.** Code that owns an external resource calls
  `(register-resource! r)`. After a restore, `(on-image-restore r)` runs for
  each registered resource: a method that reconnects returns true. Code that
  uses a resource calls `(ensure-live r)`. On a resource that is still dead,
  it signals `ResourceLost` with the restarts `reconnect` (calls
  `(reconnect r)`) and `abort`.
- **The saving fiber** doesn't continue in the image: it ends there with an
  error, like every fiber that `:fibers :none` leaves out.
- **`:entry`** gets the command-line arguments. An integer result becomes the
  process's exit status.
- **Layers** use `(save-image f :layered true)`, which saves a delta against
  the image the process started from.
- **`norn build`** writes images without fibers and byte-for-byte
  reproducibly.
- **`:shake`** (tree shaking) is not built yet.

---

## 8. Modules, macros, compilation

### 8.1 Modules and namespaces **[decided]**

- One file = one module = one namespace. Symbols are just names; vars live in
  namespaces (Clojure's model, not CL's global symbol interning).
- ~~`#lang` line (optional, Racket-style) selects reader + base language~~:
  dropped (2026-11-14) unless a need appears. Modules, hygiene and phases
  all work from the `ns` form.

```clojure
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

**Status (M4):**
- **The compiler is written in Norn and self-hosts.** It emits the same
  register bytecode as the seed (no IR or optimiser yet).
- **Tier 1 is a Cranelift backend over the bytecode** with native fast
  paths. See [runtime.md §8.2](runtime.md) and §9.
- **Destructuring** is a set of macros (`destructure`).

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

### 9.1 UI: `norn.ui` **[decided, 2026-11-13]**

The UI is drawn by Norn's own engine, JavaFX-style: a retained tree of real
controls that we draw on the GPU and style with CSS-like themes. The Norn
editor is its first user. (The reference point is the author's Xeon Studio
editor, built on JavaFX and RichTextFX.)

- **A package, not the stdlib.** `norn.ui` and its backends live in their
  own repository, `norn-ui`, versioned from 0.1 with semver from the
  start; the editor pins a version. The widget API will change far faster
  than the language and shouldn't be sealed at M8, the core shouldn't
  carry a GPU stack, and backends need their own release cycle.
- **The runtime gains two general hooks, nothing UI-specific:**
  - *main-thread handoff*: the runtime can give the process's first thread
    to an extension (macOS requires the event loop there);
  - *a native extension API*: a Rust crate can register primitives and
    post messages into channels, following the rule for threads outside
    the carriers (runtime.md §5.5).

  Both serve other uses too (audio, game loops). Until loadable extensions
  have a stable ABI, linking is static: the `norn-ui` repository builds its
  own `norn` binary from the runtime crates plus the UI crates. Loadable
  extensions come later, perhaps with M9's full FFI.
- **Own drawing, with the OS for the rest.** The OS still supplies the
  menu bar, file dialogs, clipboard, IME, accessibility, drag and drop and
  notifications. Native widgets (AppKit, GTK, Win32) were rejected: they
  can't take the themed look, the editor, terminal and grid would be
  custom views anyway, and each backend would become a whole toolkit.
- **A thick protocol.** One shared Rust UI engine, the same on every
  platform, owns:
  - the retained node tree: containers, text, shapes, images, and
    controls such as the code-editor surface and a virtualized grid;
  - layout (flex and grid, probably `taffy`), styling, text shaping and
    layout, GPU painting, hit-testing, scrolling, caret blink and
    animation.

  Norn (`norn.ui`) owns declarative views over atom state, tree diffing,
  event handlers, commands and the widget library.
  - Norn sends tree patches. Rust sends semantic events over channels (a
    click on node N; text field N changed). Raw pointer motion never
    crosses.
  - The editor surface asks Norn for the lines in its viewport, so a
    keystroke sends only the changed lines and their style spans; the rope
    stays Norn data.
  - A thin design, with Norn computing layout and display lists every
    frame, was rejected: it puts Norn on every frame's critical path before
    M10's optimiser.
- **Text** shaping and layout (font fallback, bidi, line breaking) live in
  the shared engine (`parley` or `cosmic-text`), never per backend.
- **Backends are platform layers only:** windows, the GPU surface, the
  event loop, menus, dialogs, IME and accessibility. macOS first, then
  Linux and Windows (M12), plus a *headless* backend that renders to images
  and replays scripted input for tests and CI, built early.
- **Idle must be idle:**
  - there is no render loop: drawing happens only on an event, a state
    change or a running animation;
  - only the damaged area is repainted;
  - nothing animates forever, and the display's frame timer runs only while
    an animation is active;
  - the caret blink repaints only the caret rectangle, and stops when the
    window loses focus and after a while without typing;
  - hidden or minimised windows draw nothing;
  - the engine asks for the low-power (integrated) GPU;
  - streaming updates are coalesced to at most one layout pass per frame.

  The engine counts frames drawn, area repainted and layout passes, so
  headless tests can assert, say, "no frames in 5 s idle" and "one
  keystroke repaints at most one line".
- **Every widget, webviews included.** A `native-view` node reserves a
  rectangle, and the backend places a real OS view there, keeping its
  position and clipping in step with layout and scrolling.
  - Webviews use `wry` (WKWebView, WebKitGTK, WebView2), with two-way
    JavaScript↔Norn messages on channels and custom URL schemes. They're a
    separate optional package, `norn-ui-webview`, so apps that don't use
    one don't link WebKit.
  - Other OS views (PDF, video, maps) come the same way.
  - Custom controls can be written in Norn on a canvas node, with drawing
    commands and damage tracking.
  - Known limits:
    - native views sit above our drawing, so our popups (menus,
      completion, tooltips) are separate small OS windows;
    - native views can't be themed, made translucent or animated with
      transforms;
    - focus moving between our controls and a native view needs care;
    - headless screenshots show a placeholder for them.
- **Processes.** The editor is its own Norn process with the UI engine
  linked in; the code being edited runs in a separate `norn` process over
  the editor protocol. A crash or runaway loop in user code can't freeze the
  editor, and the language runtime never loads a GPU stack. Other people's
  GUI apps run `norn.ui` in their own process.
- **Live programming applies to UIs too.** Redefining a view function at
  the REPL updates the open window. Windows are image resources, so a
  restored image reopens them through `on-image-restore`.
- **Scope of `norn.ui` 0.1**, drawn from Xeon Studio (about 18k lines of
  Clojure):
  - window, title bar, menus (app and context), dialogs, tooltips, themes;
  - resizable splits, dock areas with tabs that drag between them, an
    activity bar, a top bar with chips, a status bar;
  - buttons, toggles, dropdowns, text fields, a tree with icons and
    coloured labels, virtualized lists and grids;
  - the code-editor surface: syntax colours, brackets coloured by depth,
    inline results, squiggles, a gutter, completion and signature popups;
  - a REPL console, rich text flow (Markdown, chat), a cell-grid terminal
    view, SVG icons.
- **The editor is essentially Xeon Studio rebuilt in Norn** (decided
  2026-11-13), on `norn.ui`, with its full feature set:
  - the explorer with Git status, and the project, outline and
    source-control views;
  - dock areas with tabs that drag between them;
  - the REPL, Problems, Debugger and Inspector;
  - Markdown preview;
  - the terminal;
  - the API/HTTP workbench and the Databases view;
  - AI chat (the Claude Code and Codex CLIs) and team chat.

  Language support starts with Norn only. Common Lisp, Clojure and Racket
  support (Swank, nREPL, their indentation and highlighting) can come
  later, if wanted. So `norn.ui` 0.1 needs everything in the scope list
  above: the terminal's cell-grid view, the virtualized grid, rich text
  flow for Markdown and chat, and the code-editor surface.
- **Phased panels** (decided 2026-11-13; team chat added 2026-11-14).
  M7.6's editor ships without four panels; its done-check is unchanged
  (Norn's own development happens in the editor). Three of them arrive when
  their library lands, so the editor exercises that library too:
  - the API/HTTP workbench follows M9's HTTP client;
  - the Databases view follows M9's database drivers (SQLite first);
  - the terminal panel follows pseudo-terminal support in `norn.process`
    (in M9's library list; small, so it can come earlier if convenient).

  Team chat needs no new library: it uses the `claude` and `codex`
  command-line tools, `norn.json`, and Git worktrees through `git`. It
  comes after M7.6.

  Everything else is in M7.6: the explorer, Git status and source control
  (through the `git` command), project and outline views, dock tabs, the
  REPL, Problems, Debugger and Inspector, Markdown preview, and AI chat (the
  `claude` and `codex` command-line tools plus `norn.json`).
  `norn.ui` 0.1 still includes the terminal cell-grid view and the
  virtualized grid, as general widgets, but they can land in a later 0.x
  release if M7.5 runs long.

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
→ base.nimg
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
2. ~~**Laziness**~~ — decided: eager, type-preserving core with explicit
   lazy streams (§3.3).
3. ~~**STM**~~ — decided: a library, `norn.stm` (§3.2).
4. ~~**Mutable objects**~~ — decided: opt-in, owned by a fiber, checked
   (§4.2).
5. ~~**Primary audience**~~ — decided: Norn is a general-purpose language,
   developed by one person to start with. Its first real program is a
   VS Code-style editor made for Norn, written in Norn (runtime.md §12,
   M7.6), on `norn.ui`, a versioned package in its own repository (§9.1).
   M7.5 adds the runtime hooks it needs.
6. ~~**The editor's UI technology**~~ — decided: our own GPU drawing of a
   retained, themed control tree (JavaFX-style), with OS views such as
   webviews embeddable; `norn.ui` is a separate package (§9.1).
7. ~~**License**~~ — decided: MIT.
8. ~~**Versioning**~~ — decided: semantic versioning per module, enforced
   by `norn api-diff`; a breaking change needs a major version and a
   deprecation period of at least one minor release.
9. ~~**Complex numbers**~~ — decided: later, as a library type rather than
   part of the core numeric tower.
