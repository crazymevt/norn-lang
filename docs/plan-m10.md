# Plan: M10 — the optimising compiler

Drafted 2026-11-16; its proposals were accepted the same day (see
Decisions, at the end). runtime.md §12's row:
the IR (design §8.3), inlining, direct linking to sealed modules' vars and
deoptimisation when one is unsealed (moved from M8), type inference from
M9's signatures, unboxing, escape analysis; tier 1's full calling
convention (push-frame and `return_call`), inline caches, and native
compilation of every `defn`. Done when there are "measured wins on
`bench/` with tier 0 and tier 1 still agreeing on every suite".

## How things are now

- **The compiler** (`lib/compiler/compiler.norn`, 517 lines, in Norn)
  turns the expander's kernel forms straight into register bytecode, one
  pass, no IR. It knows nothing about types or what a var holds: every
  global call reads its var and calls through it.
- **Tier 1** (`jit.rs`, 398 lines) translates a proto's bytecode to
  Cranelift on first run (`NORN_TIER=1`). It runs moves, constants, var
  reads, branches, loops and a few fixnum primitives natively and hands
  every other instruction back to the interpreter: calls, returns,
  closures, `def`. Frames stay in the heap, so mixed stacks, the debugger,
  images and the profiler need nothing special. It gains 3–10× on loops
  and arithmetic, 2× on calls (perf survey), nothing on library work.
- **Sealing** (M8) fixes the standard library's modules, and `norn
  build` seals a release's own. Nothing uses it yet to link directly or
  inline: redefining a sealed var unseals its module with a warning.
- **Signatures** (M9) are checked at module boundaries by a wrapper:
  0.6–0.8 µs a call. `norn.string`, `norn.math`, `norn.regex` and
  `norn.bytes` stay unsigned until M10 makes checks cheap (todo).
  `(declare (optimize …) (type …))` is recorded and unused.
- **Measured costs** the survey left for M10 (perf-survey.md, B3–B5,
  B7): `try` with no error 1.2 µs, `binding` 400 ns, making an instance
  1.8 µs (a map: 70 ns), an operation on a library collection 0.7–2 µs,
  `mapv`/`into` 90–160 ns an item at tier 1. M9b's profiler adds:
  contract wrappers about 10% of loglens's report.
- **Tools M9b left for this:** `norn.prof` (where time goes, both
  tiers), `norn.xref` (what calls what), coverage, and the benchmarks in
  `bench/` (`micro.norn`, `workloads.norn`).

## Parts

| # | Part |
| --- | --- |
| 0 | **A baseline.** `bench/` grows the programs M9b's lang-tests made (loglens's report, ledger's import and report, quire's build, chat's fan-out) beside the micro and workload suites, and a `bench/compare` that runs everything at both tiers and diffs against a recorded baseline (`bench/baseline.edn`), so each later part shows its win or its cost. |
| 1 | **The IR.** An IR (design §8.3) between the expander and code generation, in Norn (as built: a tree in `norn.compiler`, see Status): names resolved, calls and primitives explicit, closures with their captures, `let`/`loop`/`if`/`do` as structure. The compiler lowers kernel forms to it and generates the same bytecode from it as now. Nothing gets faster; every later part works on the IR. Checked by the suites at both tiers and by compiling the standard library both ways and comparing behaviour. |
| 2 | **Tier 1's calling convention.** Calls and returns stay native: push-frame + `return_call` (runtime.md §3.2), so a call from tier-1 code to tier-1 code doesn't go through the interpreter, closures are made natively, and tail calls stay proper. Frames keep their layout. Every `defn` compiles at tier 1 when it's defined, and hot tier-0 code is promoted by a call counter. |
| 3 | **Direct linking and deoptimisation.** A call from a sealed module to a sealed module's var compiles to the function itself, not a var read, and the compiled code is recorded as depending on that var. Redefining it (which unseals, with the warning it gives now) marks its dependents stale: they go back to calling through the var the next time they run, and recompile. |
| 4 | **Inlining.** Small functions (a size budget) from sealed modules and private functions within a module are inlined at the IR level, core primitives (`inc`, `first`, `nth`, `count`…) by kind, and a contract's checks at the call site instead of through the wrapper, dropping checks the types prove. Then sign `norn.string`, `norn.math`, `norn.regex` and `norn.bytes` (the todo item). |
| 5 | **Types and unboxing.** Types inferred over the IR from signatures, `declare`, literals, primitives' result types and guards already passed; calls specialised on them (fixnum and float arithmetic, known classes' slots, vector access). Floats unboxed in tier 1 within a function. A guard that fails deoptimises to the general code. |
| 6 | **Escape analysis and the cheap paths.** Allocations that don't escape a function are removed or kept on the frame: `handler-case`'s result vector and handler closures, keyword arguments' maps, `binding`'s map. That's most of `try` and `binding` (B3). With it, the runtime's own fast paths: instances of classes with no `initialize-instance` methods of their own skip the generic function (B4), library collections dispatch straight to their methods (B5), and inline caches for generic calls, slot reads (keyed on layout) and var reads. |
| 7 | **Sequence functions.** `map`, `filter`, `reduce`, `into` and transducers over vectors and ranges compile to loops when the step functions are known: the inlined step, no per-item closure call (B7). |

**Order.** 0 first (everything after is measured against it). 1 next:
2–7 work on it. Then 2 (the calling convention: inlining and types gain
little while every call leaves native code), 3 and 4 (linking, then
inlining, which needs to know what a call reaches), 5, 6, and 7 last (it
uses inlining and types). Each part is merged on its own, with the
baseline compared.

**Done-check (proposed):**

- `bench/compare` shows wins over the baseline at tier 1 on calls (the
  survey's call rows 3× faster or better), `try` and `binding` (2×),
  making an instance (5×), library collections (3×) and `mapv`/`into`
  per item (2×), and loglens's report faster, with no workload slower
  than its baseline by more than 5%.
- Tier 0 and tier 1 agree on every suite, and on every sibling project's.
- Redefining a sealed function at the REPL still takes effect in its
  callers (direct-linked or inlined), with the warning.
- The debugger, backtraces, `restart-frame`, images, the profiler and
  coverage work the same on optimised code.
- `norn.string`, `norn.math`, `norn.regex` and `norn.bytes` carry checked
  signatures, and their thinnest functions are no slower than before.

## Decisions (the user, 2026-11-16)

1. **Tier 1 keeps compiling bytecode.** The IR's optimisations show up in
   the bytecode it generates, with type facts passed alongside for tier 1
   (unboxing, guards). One path, proven, and every optimisation helps both
   tiers. The design's IR-to-Cranelift backend stays a possibility for
   later.
2. **Tier 1 becomes the default when M10 ends, if it earns it:** it wins
   on the baseline and agrees with tier 0 on every suite and every
   sibling project. `NORN_TIER=0` stays.
3. **Direct linking and inlining, with recompilation:** code that links
   to or inlines a sealed var depends on it, and redefining the var
   (which unseals its module, with the warning) makes its dependents
   recompile (part 3).
4. **M9b stays open** until `norn lsp` has VS Code and Zed extensions,
   checked in those editors (todo).

## Status

- **0, the baseline: done.** `bench/programs.norn` has five programs made
  from the libraries with generated data, modelled on the lang-tests
  programs: a log report (loglens), a ledger (ledger), a static site
  (quire), a chat fan-out (chat), and Mandelbrot (the mandelbrot
  project's escape-time kernel). The suites print their results as data
  with `--edn`; `bench/compare.norn` runs micro, workloads and programs at
  both tiers, each in its own process, and either records
  `bench/baseline.edn` (`--record`, `--suites` to redo some) or prints
  each case's speed-up against it. The programs run on one carrier
  (`NORN_THREADS=1`): the chat fan-out takes 4.5 ms on one and about 64 ms
  on ten, and swung between 12 and 59 ms run to run, which would hide
  every other change (todo). Run to run, the rest repeat within about 7%
  at tier 0 and 1% at tier 1. **SBCL as a reference:**
  `bench/mandelbrot.lisp` is the same kernel in SBCL (typed doubles,
  `(speed 3)`), with a checksum matching Norn's (11,251 interior pixels,
  591,574 for the escape counts): 2.0 ms against Norn's 365–390 ms at
  tier 0 and 293 ms at tier 1, about 180× and 145× slower. Boxed floats
  and calls through the interpreter are most of that: parts 2 and 5.
  Baseline recorded at `b4a8bc2`.
- **1, the IR: done,** a little differently from the plan: a tree, not
  ANF. `analyze` turns a kernel form into nodes (`:const :local :global
  :if :do :def :fn :let :letrec :recur :call`): locals resolved to
  bindings `{:id :sym}` (matched by identity, as before), globals to
  their vars, macros expanded, a `def`'s var interned before its value
  (so it can refer to itself), collection literals made calls to
  `vector`/`hash-map`/`hash-set`, anonymous functions named
  (`outer/fn`). `generate` walks the tree as the one-pass compiler walked
  forms, allocating registers and constants in the same order, with
  locals and captures known by binding id. ANF would name every
  intermediate value, which changes register allocation: the tree keeps
  the bytecode the same byte for byte, which is how this part is checked.
  `compiler_test` (byte-identical to the seed on `core.norn` and the
  compiler, and the self-hosting fixpoint) passes unchanged, and every
  function of the standard library and the expander (1,683) compiles to
  the same code as before (`%signature`, auto-gensym names aside). Passes
  that need named intermediates (escape analysis) can name them on the
  tree. Its cost: compiling and loading `http.norn` takes 46 ms against
  the baseline's 43 (tier 0), about 7% more, for the tree it builds.
- **2, tier 1's calling convention: done,** with one change from the plan:
  no eager compiling of every `defn`. Native functions use Cranelift's
  `tail` convention. A call is a helper that works out the callee as the
  interpreter does (vars, generic dispatch, classes, any arity), pushes a
  closure's frame in the heap as `push_frame` does and `return_call`s into
  its code, or calls a primitive and goes on; a return pops and
  `return_call`s back into the caller at its resume point. `CLOSURE` runs
  natively. The interpreter's state stays the source of truth (the helpers
  keep it), so native code can stop anywhere; a primitive that fails or
  parks stops it at that instruction without being called again
  (`PENDING_FLAG`), a due safepoint with `POLL_FLAG`. Instead of compiling
  each `defn` when it's defined, a proto is compiled once it has been
  entered 100 times (calls and loop turns; `NORN_JIT_THRESHOLD`):
  compiling on first run, as before, made `norn -e` start in 1.07 s at
  tier 1 (0.54 s at 100; tier 0: 0.11 s) and loading `http.norn` 0.65× the
  baseline (1.34× at 100). **Measured** (tier 1 against the baseline): a
  closure call 1.41×, another module's function 1.26×, `try` 1.15×,
  `binding` 1.12×, making an instance 1.10×; the programs 1.04–1.17×
  (Mandelbrot 293 → 249 ms; SBCL is 2.2 ms); no tier-0 change beyond noise.
  A plain call is at par (18 ns): two helper calls cost what the
  interpreter's call did. Getting under that means pushing frames inline
  for callees known at compile time, which part 3's direct linking
  provides. The empty loop reads 0.88× (4.8 against 4.3 ns a turn) since
  the switch to the `tail` convention.
  **Found on the way:** (1) a primitive tail-called at the bottom of a run
  ran twice (its value sent native code back to the TAILCALL, which the
  interpreter ran again): an `alts!` that had already handed its value
  over then parked forever (conc_test hung about one run in fourteen under
  load); now the value returns through the `RETURN` that follows, with a
  regression check in conc_test. (2) Time slices were 2000 reductions
  only, which gave fast native code a fraction of a loaded carrier's time
  (prof_test missed its busy fiber about one run in four in the full
  suite): a slice now also lasts at least 0.5 ms (runtime.md §5.1).
