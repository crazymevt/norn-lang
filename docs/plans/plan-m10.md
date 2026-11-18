# Plan: M10 — the optimising compiler

Drafted 2026-11-16; its proposals were accepted the same day (see
Decisions, at the end). **Done 2026-11-17** (Status's last entry, "M10:
done"); what's left for later is listed there. runtime.md §12's row:
the IR (design §8.3), inlining, direct linking to sealed modules' vars and
deoptimisation when one is unsealed (moved from M8), type inference from
M9's signatures, unboxing, escape analysis; tier 1's full calling
convention (push-frame and `return_call`), inline caches, and native
compilation of every `defn`. Done when there are "measured wins on
`bench/` with tier 0 and tier 1 still agreeing on every suite".

## How things are now

- **The compiler** (`lib/compiler/compiler.skn`, 517 lines, in Skein)
  turns the expander's kernel forms straight into register bytecode, one
  pass, no IR. It knows nothing about types or what a var holds: every
  global call reads its var and calls through it.
- **Tier 1** (`jit.rs`, 398 lines) translates a proto's bytecode to
  Cranelift on first run (`SKEIN_TIER=1`). It runs moves, constants, var
  reads, branches, loops and a few fixnum primitives natively and hands
  every other instruction back to the interpreter: calls, returns,
  closures, `def`. Frames stay in the heap, so mixed stacks, the debugger,
  images and the profiler need nothing special. It gains 3–10× on loops
  and arithmetic, 2× on calls (perf survey), nothing on library work.
- **Sealing** (M8) fixes the standard library's modules, and `skein
  build` seals a release's own. Nothing uses it yet to link directly or
  inline: redefining a sealed var unseals its module with a warning.
- **Signatures** (M9) are checked at module boundaries by a wrapper:
  0.6–0.8 µs a call. `skein.string`, `skein.math`, `skein.regex` and
  `skein.bytes` stay unsigned until M10 makes checks cheap (todo).
  `(declare (optimize …) (type …))` is recorded and unused.
- **Measured costs** the survey left for M10 (perf-survey.md, B3–B5,
  B7): `try` with no error 1.2 µs, `binding` 400 ns, making an instance
  1.8 µs (a map: 70 ns), an operation on a library collection 0.7–2 µs,
  `mapv`/`into` 90–160 ns an item at tier 1. M9b's profiler adds:
  contract wrappers about 10% of loglens's report.
- **Tools M9b left for this:** `skein.prof` (where time goes, both
  tiers), `skein.xref` (what calls what), coverage, and the benchmarks in
  `bench/` (`micro.skn`, `workloads.skn`).

## Parts

| # | Part |
| --- | --- |
| 0 | **A baseline.** `bench/` grows the programs M9b's lang-tests made (loglens's report, ledger's import and report, quire's build, chat's fan-out) beside the micro and workload suites, and a `bench/compare` that runs everything at both tiers and diffs against a recorded baseline (`bench/baseline.edn`), so each later part shows its win or its cost. |
| 1 | **The IR.** An IR (design §8.3) between the expander and code generation, in Skein (as built: a tree in `skein.compiler`, see Status): names resolved, calls and primitives explicit, closures with their captures, `let`/`loop`/`if`/`do` as structure. The compiler lowers kernel forms to it and generates the same bytecode from it as now. Nothing gets faster; every later part works on the IR. Checked by the suites at both tiers and by compiling the standard library both ways and comparing behaviour. |
| 2 | **Tier 1's calling convention.** Calls and returns stay native: push-frame + `return_call` (runtime.md §3.2), so a call from tier-1 code to tier-1 code doesn't go through the interpreter, closures are made natively, and tail calls stay proper. Frames keep their layout. Every `defn` compiles at tier 1 when it's defined, and hot tier-0 code is promoted by a call counter. |
| 3 | **Direct linking and deoptimisation.** A call from a sealed module to a sealed module's var compiles to the function itself, not a var read, and the compiled code is recorded as depending on that var. Redefining it (which unseals, with the warning it gives now) marks its dependents stale: they go back to calling through the var the next time they run, and recompile. |
| 4 | **Inlining.** Small functions (a size budget) from sealed modules and private functions within a module are inlined at the IR level, core primitives (`inc`, `first`, `nth`, `count`…) by kind, and a contract's checks at the call site instead of through the wrapper, dropping checks the types prove. Then sign `skein.string`, `skein.math`, `skein.regex` and `skein.bytes` (the todo item). |
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
- `skein.string`, `skein.math`, `skein.regex` and `skein.bytes` carry checked
  signatures, and their thinnest functions are no slower than before.

## Decisions (the user, 2026-11-16)

1. **Tier 1 keeps compiling bytecode.** The IR's optimisations show up in
   the bytecode it generates, with type facts passed alongside for tier 1
   (unboxing, guards). One path, proven, and every optimisation helps both
   tiers. The design's IR-to-Cranelift backend stays a possibility for
   later.
2. **Tier 1 becomes the default when M10 ends, if it earns it:** it wins
   on the baseline and agrees with tier 0 on every suite and every
   sibling project. `SKEIN_TIER=0` stays.
3. **Direct linking and inlining, with recompilation:** code that links
   to or inlines a sealed var depends on it, and redefining the var
   (which unseals its module, with the warning) makes its dependents
   recompile (part 3).
4. **M9b stays open** until `skein lsp` has VS Code and Zed extensions,
   checked in those editors (todo).
5. **Every var is linked, behind a guard** (part 3, decided when it
   started): sealing is a set of namespace names, and code at the REPL or in
   a script is never sealed, though the done-check's call rows measure it.
   A guard (one compare) makes linking safe for any var, and a
   redefinition takes effect at once; sealing stays for api-diff and
   inlining.
6. **Inlining, part 4 (decided when it started):** in the compiler's IR,
   guarded (decision 1), for any small `defn` (as decision 5 for
   linking), and inlined calls show in backtraces and the debugger as
   frames of their own.
7. **Signing skein.string, skein.math, skein.regex and skein.bytes waits for
   part 5** (decided in part 4): with the checks at the call site,
   skein.string signed was still 10–25 ns slower a call at tier 1 for its
   thinnest functions (and 50–90 ns at tier 0); part 5's types are to
   drop the checks they prove.
8. **Signatures are speculation, part 5 (decided when it started):**
   within its module a function's signature isn't checked, so tier 1
   assumes its parameters' types, checks them once on entry, and when a
   check fails stops there and compiles the function again without the
   assumption. No change in behaviour.
9. **Slot reads of known classes move to part 6** (decided when part 5
   started), with its inline caches keyed on layout, which cover known
   and unknown classes alike.
10. **Signing's "no slower" is measured at tier 1** (decided when part 5
    started), the default when M10 ends: at tier 0 the checks types can't
    prove still cost a primitive call each.
11. **skein.core's types are fixed when compiling** (decided in part 5.3):
    the compiler relies on what its type predicates and primitives with
    fixed result types give, as they are when compiling (while each var
    still holds what it held when the compiler was loaded). Redefining
    one (which unseals skein.core with a warning) affects code compiled
    after it, not tests already decided.
12. **Part 6 fixes the runtime's protocols, not escape analysis first**
    (decided when it started): profiling showed `try`, `binding`, making
    an instance and library collections spend their time and allocate in
    core.skn's own protocol functions (`%bind-map`, `%handler-case`,
    `%with-handlers`, `%make-from`, `%collection-op`), which a compiler
    pass over user code wouldn't reach. Escape analysis only where user
    code still allocates (keyword arguments' maps), if needed after.
13. **Inline caches for generic calls and slot reads** (decided when part
    6 started); var reads stay as part 3 left them (a load and a
    compare).
14. **Part 7's fast paths first** (decided when it started): the sequence
    functions build into transients with direct loops over vectors and
    ranges, and tier 1 runs fixnum `rem`/`quot`/`mod` and calls of closures
    held in locals natively. Compiling `map`/`filter`/`into` to loops with
    the step inlined needs skein.core's functions to have IR (the seed
    compiles core); later, if still needed.

## Status

- **0, the baseline: done.** `bench/programs.skn` has five programs made
  from the libraries with generated data, modelled on the lang-tests
  programs: a log report (loglens), a ledger (ledger), a static site
  (quire), a chat fan-out (chat), and Mandelbrot (the mandelbrot
  project's escape-time kernel). The suites print their results as data
  with `--edn`; `bench/compare.skn` runs micro, workloads and programs at
  both tiers, each in its own process, and either records
  `bench/baseline.edn` (`--record`, `--suites` to redo some) or prints
  each case's speed-up against it. The programs run on one carrier
  (`SKEIN_THREADS=1`): the chat fan-out takes 4.5 ms on one and about 64 ms
  on ten, and swung between 12 and 59 ms run to run, which would hide
  every other change (todo). Run to run, the rest repeat within about 7%
  at tier 0 and 1% at tier 1. **SBCL as a reference:**
  `bench/mandelbrot.lisp` is the same kernel in SBCL (typed doubles,
  `(speed 3)`), with a checksum matching Skein's (11,251 interior pixels,
  591,574 for the escape counts): 2.0 ms against Skein's 365–390 ms at
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
  `compiler_test` (byte-identical to the seed on `core.skn` and the
  compiler, and the self-hosting fixpoint) passes unchanged, and every
  function of the standard library and the expander (1,683) compiles to
  the same code as before (`%signature`, auto-gensym names aside). Passes
  that need named intermediates (escape analysis) can name them on the
  tree. Its cost: compiling and loading `http.skn` takes 46 ms against
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
  entered 100 times (calls and loop turns; `SKEIN_JIT_THRESHOLD`):
  compiling on first run, as before, made `skein -e` start in 1.07 s at
  tier 1 (0.54 s at 100; tier 0: 0.11 s) and loading `http.skn` 0.65× the
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
- **3, direct linking: done,** for every var behind a guard (decision 5),
  not only sealed modules' with recorded dependents. Tier 1 compiles a call
  whose callee was loaded by `GETVAR` (or `SELF`) against the closure the
  var holds then: a guard compares the register with it, and native code
  writes the callee's frame itself and jumps into its code. Returns pop
  inline. A redefinition fails the guard, so the new function is called at
  once (a loop already running too), and the caller recompiles against it
  (`relink`; 8 at most per proto). The interpreter's state lags behind
  inline frames; every helper syncs it first. **Measured** (tier 1 against
  the baseline): a plain call 2.4× (17.5 → 7.2 ns), another module's 3.6×,
  `(fib 27)` 11.5 → 5.3 ms; the programs 1.09–1.29×; the rope workloads
  1.38×; compiling and loading `http.skn` 1.43×. That meets the
  done-check's 3× for calls across modules, not yet for calls within one
  (2.4×): the rest is inlining (part 4) and the arguments' boxing (part 5).
  The code is 15% larger than part 2's, and compiling takes longer: at
  `SKEIN_JIT_THRESHOLD=1` the full suite starves prof_test's busy fiber now
  and then (compiles aren't preemptible), which the default threshold
  avoids. `tests/skein/link_test.skn` checks redefinition (in a running
  loop too, and of a sealed module's function), arities, tail calls and
  deep recursion at both tiers, and that tier 1 linked and relinked.
  conc_test's debugger check now counts its own fiber's entry, not the
  process's (other suites park fibers in the debugger beside it).
- **4, inlining: done,** with signing moved to part 5 (decision 7). The
  compiler inlines calls of small functions (24 IR nodes, a fixed arity,
  no captures or self-calls) behind `GUARD`, a new instruction; the
  compiler keeps their IR by var. Inlined calls show in error traces,
  the debugger (`:inlined` frames, with their locals; restarting one
  calls the function's newest definition and its caller goes on) and
  profiles; where the inlined function would have tail-called a closure,
  it isn't shown, as its frame would have been replaced. Tier 1 gained
  native paths for `count`, `nth`, `first`, `identical?`, `nil?`,
  `some?`, `not` and the type predicates. A contract whose tests are
  classes or predicates is checked by plain code inlined at the call
  site, guarded by a stamp of its types (`f%stamp`), with %wrap-for's
  wrapper the fallback. **Measured** (tier 1 against the baseline): a
  plain call 3.6× (4.8 ns), a checked call across modules 26.7× (15.6
  ns), the programs 1.12–1.40×, the rope workloads 2.3–2.6×. That meets
  the done-check's 3× for calls. **Costs:** compiling and loading
  `http.skn` 67 ms at tier 0 against 45 (51 at tier 1, 0.96× the
  baseline); the printing workloads (str pieces, JSON write) read
  0.84–0.9× in the full suite run, but not alone (the same as part 3's
  binary there), so that is the state the cases before them leave.
  skein.core isn't inlined (the seed compiles it at boot). Tests:
  `tests/skein/inline_test.skn`, and prof_test's inlined-calls check.
- **5, types and unboxing: started (2026-11-17).** At the start, tier 1:
  Mandelbrot 255 ms (125× SBCL), a float `*` 30 ns, the empty loop 4.7
  ns a turn. Tier 1 can be entered at any instruction (the interpreter
  goes back into native code after each one it runs), so every
  instruction loads and stores the frame's registers: values can't stay
  in machine registers, and floats can't stay unboxed. In four steps:
  - **5.1, registers in SSA: done.** Native code is entered only at some
    instructions (the first, the one after each that can stop native
    code, a loop's head after a safepoint stop); at any other pc it stops
    at once and the interpreter runs on to one (`reg_flow` in jit.rs).
    Between entries a register's value is a Cranelift variable: reads
    don't go back to the frame. Writes still go through to it: deferring
    them (each stop writing back what changed) was tried, and was slower
    where loops call generic code (Mandelbrot 273 ms against 251: every
    float operation's call wrote back a dozen registers), while a store is
    cheap and reloading what was just stored is what cost. So the frame
    stays exact (GC, debugger, backtraces as before); the write-back
    machinery stays for 5.2's unboxed floats, the one kind of register
    whose writes are deferred. `SKEIN_JIT_SSA=0` enters anywhere, as
    before, for comparison. **Measured** (tier 1): the empty loop 4.7 →
    2.5 ns a turn (1.7× the baseline), a plain call 4.8 → 2.9 ns (5.9×),
    the programs 1.2–1.47× (Mandelbrot unchanged: its floats are 5.2's).
    The suites pass at tier 1, at `SKEIN_JIT_THRESHOLD=1` and with
    `SKEIN_JIT_SSA=0`. **Costs:** code 10% larger (16.1 MB for
    everything at `SKEIN_JIT_THRESHOLD=1`), and compiling everything 10%
    longer (5.2 → 5.7 s): entry stubs, and Cranelift building the SSA.
  - **5.2, floats in tier 1: done.** Narrower than planned: the analysis
    tracks one type, float (FloFlow in jit.rs), which is what unboxing
    needs; fixnum and kind facts (to drop tag checks) are left for 5.3,
    where the IR's types arrive. `+ - * / inc dec zero? < > <= >= == =`
    run natively on floats and a float with a fixnum, matching
    numeric.rs (comparisons exact: a fixnum converted only within 2^53;
    `=` true of two NaNs, never of a float and a fixnum), and `+ - *` with
    three arguments when the first or second is a float. A register that
    surely holds a float (a constant, a copy, or a call of those with a
    float argument, assumed to give a float; the greatest fixpoint, so a
    loop's float variables are) is an f64, boxed only when something
    needs the value itself (a generic call, a return, a stop, an edge
    into code that doesn't know it's a float and reads it: a liveness
    pass). A flag per register says whether it has its box, and copies
    share one (a must-alias pass), so `identical?` holds as at tier 0.
    Where an assumption fails (`*` redefined), native code stops after
    the call, entries that find a non-float refuse, and after four such
    failures the function is compiled without assumptions (decision 8's
    mechanism; signatures' parameters come in 5.3). **Measured** (tier 1
    against the baseline): Mandelbrot 293 → 30 ms (9.7×, 14× SBCL), float
    `*` 30 → 3.4 ns (8.7×); nothing else moved beyond noise. Tests:
    `tests/skein/arith_test.skn` (every operator on edge inputs against
    the primitive through `apply`, three arguments, identity, and `*`
    redefined under a compiled float loop, in a process of its own).
    **Found on the way:** (1) the analysis's back-edges must start
    optimistic, or a loop's head loses its floats; (2) f64 values live
    across many blocks are kept on the stack by the register allocator
    (a probe loop reading a var each turn: 11 ns against the benchmark's
    3.4); moving them across calls as integer bits made it worse, so it's
    left; (3) **lint and group-by were mostly GC:** with
    `SKEIN_HEAP_MIN=1g`, linting http.skn took 80–93 ms against 510–560
    (6×), group-by 70 against 200. See "The heap's growth" below.
- **The heap's growth: done (2026-11-17),** outside the plan's parts.
  MMTk's dynamic heap size (MemBalancer) left only a few MB above live
  data, since Skein traces fast against how much it allocates: with the
  workloads' 65 MB live, linting http.skn collected about 100 times a
  run. A trigger of Skein's own (`ProportionalTrigger` in heap.rs) lets the
  heap grow to live × (1 + `SKEIN_HEAP_GROWTH`%), 100 by default, as Go's
  GOGC; MemBalancer stays as `SKEIN_HEAP_GROWTH=balancer`. Tried 50, 100,
  200: lint with the data live 139, 124, 110 ms (peak RSS 183, 233, 376
  MB, against 137 before); 100 chosen. **Measured** (against the
  baseline, tier 0 / tier 1): lint 5.4× / 5.9×, into a map 7.8× / 11.4×,
  group-by 3.7× / 4.9×, the printing workloads (JSON write, EDN pr-str,
  bin encode, which read 0.84–0.9× through M10: GC, not code) about 1.5×,
  the log report 1.15× / 1.44×, the ledger 1.12× / 1.34×; 1M fibers then
  10M messages 22.5 → 5.9 s (163 → 7 GCs, peak RSS 2.2 → 3.3 GB);
  `skein -e nil` unchanged (79 MB). The suites pass at both tiers; under
  `SKEIN_GC_STRESS=2` io_test's 30 s timing limit on reading 50,000 lines
  was overrun once in the full run (passing alone, before and after).
  - **5.3, types in the IR: done, narrower than planned.** compiler.skn
    knows what kinds of value a node gives as it generates (runtime.md
    §8.2, "Types"): from literals, locals, an inlined function's
    arguments and body, skein.core's predicates and primitives (decision
    11) and tests passed; an `if` with a pure test the types decide
    generates only its branch. That's what drops a contract's checks at a
    call site where they're proven: the result check, when the inlined
    callee's body shows it (`blank?`'s is a boolean, `trim`'s a string),
    and a literal argument's. The stamp test each call made (part 4) moved
    to when the wrapper is defined, with the wrappers registered by
    callee and replaced when its contract changes (runtime.md). **Not
    done:** signatures' and `(declare (type …))`'s types, and passing
    types to tier 1 with the proto (parameters' speculation, decision
    8); tier 1's float analysis covers what Mandelbrot needed, and
    signing didn't need the rest. **Measured** (four of skein.string's
    thinnest functions signed, arguments of unknown type, tier 1): part
    4 measured 6–26 ns more a call signed; after 5.1 it was 3–11; now
    1–2 (`blank?` 77 against 75 ns unsigned, `includes?` 57 / 56, `trim`
    45 / 44, `starts-with?` 139 / 138): what's left is a predicate per
    argument of unknown type. At tier 0, 30–60 ns less a call than
    before (decision 10). The micro suite's checked call across modules:
    41× the baseline at tier 1 (32× before), 5.4× at tier 0 (3.7×). Compiling `http.skn` 4% longer (35.9 → 37.6
    ms at tier 0). Tests: `tests/skein/types_test.skn` (decided tests
    keep their meaning; a loop's locals change kind; a test with effects
    still runs).
  - **5.4, signing: done.** skein.string, skein.math, skein.regex and
    skein.bytes carry signatures: what each function's primitives already
    take (`^String` where nothing else works), nilable or either-of where
    the function takes those, result types where they're certain, none
    where a function takes anything (`upper-case`, `pad-left`, `join`:
    `str` of it). `tests/skein/signed_test.skn` is part 4's probe as a
    test: every public function, 0–4 arguments of edge values, called as
    itself (no checks) and from another module (its call-site checks);
    where the function gave a value the checked call must give the same,
    where it failed the checked call must fail. It found signatures too
    narrow, since widened: the regex functions take a string pattern
    (and `pattern` a regex), the statistics functions bytes (percentile
    too, an M9 signature; `api/` recorded again for it, the only change
    api-diff calls breaking). What the signatures refuse that gave a
    value only by accident is listed in the test with why (`ends-with?`
    of a vector, `sum` of an empty string, `truncate`'s three-argument
    form of a character, `finish-decoder` of something not a map).
    **Measured** (tier 1, arguments of unknown type): `blank?`,
    `includes?`, `trim`, `starts-with?` signed and not within run-to-run
    noise (±2 ns; part 4: 6–26 ns more). The suites pass at both tiers
    and at `SKEIN_JIT_THRESHOLD=1`, and the sibling projects' (lang-tests'
    five, skein-html, skein-sdl3, skein-raylib, skein-ui, cats, skein-studio)
    as before signing: skein-studio's chat test fails the same with the
    binary before (a fake Claude client's reply never comes).
  **Done-check (proposed):** Mandelbrot at tier 1 10× its baseline or
  better (under 30 ms); float arithmetic 5×; the empty loop 2×; the four
  libraries signed with their thinnest functions no slower at tier 1
  (decision 10); the tiers agree on every suite; the debugger,
  backtraces, `restart-frame`, the profiler and coverage as before.
  **Where it stands:** Mandelbrot 9.5–9.8× (29.8–30.9 ms: at the line,
  not past it); float `*` 8.7×; the empty loop 1.7× (short of 2×: the
  safepoint's loads each turn); signing as above; the suites agree. The
  same Mandelbrot in SBCL takes 2.0 ms and in Clojure (`bench/mandelbrot.clj`,
  primitive hints and unchecked arithmetic, after the JVM's warm-up) 2.0
  ms: Skein's tier 1 is 15× either. What's left is per operation in the
  loop: a var read and a callee check on each arithmetic call, the
  safepoint, floats kept on the stack.

- **6, the cheap paths: started (2026-11-17).** At the start (tier 1):
  `try` with no error 959 ns (1.39× the baseline), `binding` 300 ns
  (1.30×), making an instance 1,381 ns (1.28×), `count` of a library
  collection 499 ns (1.27×), `nth` 1,379 ns (1.56×), a generic function
  with one method 53 ns (0.98×), a slot read 14 ns (0.94×). Where the time
  goes (skein.prof): `binding`, two thirds in `%bind-map` (a Skein loop of
  assoc and marking each var dynamic); `try`, the same for `*handlers*`,
  plus an atom as the catch tag, a closure per catch clause and an `[:ok
  v]` vector; an instance, `%make-from` (a map of the initargs, each
  checked against the slots with `some`, initform closures); a library
  collection, the primitive handing the call to `%collection-op`
  (variadic, a `case` on a keyword) which calls the generic function. In
  steps (decisions 12, 13):
  - **6.1, `binding` and `try`: done.** `%bind-map` is a primitive
    (variadic: `binding` no longer builds a vector for it; one vector
    still taken, as cached expansions may hold). `handler-case`'s clauses
    are data in the handler list, `[class nil owner tag i]`, which `signal`
    throws to itself (no closure per clause); a normal value comes back as
    it is, a caught condition as `[%caught i c]` (no `[:ok v]` per try);
    `%with-handlers` conses its entries on without `reduce` or `reverse*`
    and binds without a thunk of its own. (The module cache's key has the
    build id: no expansion of the old protocol meets the new one.)
    **Measured** (tier 1 against the baseline): `binding` 300 → 196 ns
    (2.00×; 2.41× at tier 0), `try` with no error 959 → 506 ns (2.64×;
    1.84× at tier 0), an error signalled and caught 3,452 → 2,553 ns
    (1.70×).
  - **6.2, making an instance: done.** `%make-fast` makes a plain
    `(Class :k v…)` natively: a standard class, every pair naming a slot
    (the last given wins, as before), each slot not given with a constant
    default or none and not required. A literal `:initform` is recorded as
    `:initvalue` too. Anything else (an initform to run, a required slot
    missing, an unknown initarg) returns nil and `%make-from` does it as
    before, calling initforms in the fiber (a primitive calling them would
    keep their conditions from its handlers) and saying what's wrong.
    **Measured:** 1,381 → 225 ns (7.86×; 7.15× at tier 0). Tests:
    `tests/skein/make_test.skn`.
  - **6.3, library collections: done.** `count`, and `nth` without a
    default, hand the call straight to `collection-count` and
    `collection-nth` (the rest, and `nth` with a default, still through
    `%collection-op`). skein.buffer's `nth` read three slots by making the
    instance a map (`instance->map`) each call: now it reads them.
    **Measured:** `count` of a Buffer 499 → 222 ns (2.85×, short of 3×:
    what's left is the generic function's dispatch, 6.4's), `nth` 1,379 →
    602 ns (3.58×).
  - **6.4, generic calls and slot reads: done,** not as per-site caches
    that change as the program runs (cells written from several carriers
    at once would need care), but as part 3's linking and a native read:
    (1) the dispatch cache's probe compared its keys with `equal` and hashed
    them with `hash`, each setting up a frame (an allocation) every call:
    classes now by identity first. (2) Tier 1 links a call of a generic
    function whose dispatch cache holds one entry (one class, one argument
    position, no eql specializers) to that entry's effective method, behind
    a guard on the function, the argument being an instance of that class,
    and the function's method epoch and the class epoch; the frame is
    pushed inline as part 3's are. A failed guard makes the call the
    general way and relinks (part 3's limit). (3) `(:k x)` with a keyword
    constant and one argument reads natively (`kw_get`) from an instance
    that isn't mutable, or a map; anything else (a mutable instance, whose
    owner is checked; other values) goes the general way. **Measured**
    (tier 1 against the baseline): a generic function with one method 53 →
    11.5 ns (4.5×), with `:before` and `:after` 2.0×, an inherited method
    (two classes seen: not linked) 1.6×; a slot read 14 → 5.2 ns (2.5×), a
    map read 31 → 22 ns; a Buffer's `count` 3.06×. Tests: link_test's
    linked generic calls (methods redefined and added, a :before, the class
    redefined, other classes at the site) and keyword reads (a mutable
    instance from another fiber, a class redefined).
  - **6.5, keyword arguments' maps: done.** A map pattern without `:as`
    reads each key from what it's given (`%kw-get`): from a keyword-
    argument seq (a rest parameter's) by searching it, the last pair with
    the key winning as `hash-map`'s would, an odd count `hash-map`'s
    error; from anything else as `get`. No map is made. With `:as`, the
    whole map is bound, so `%as-map` makes one, now a primitive (it was
    `(apply hash-map x)`, and `apply` stopped tier-1 code each call).
    **Measured** (tier 1): keyword options with two given 255 → 141 ns
    (1.93×), with `:or` 2.08×, none given 1.77×; an options map 1.97×,
    destructuring a vector and a map 1.99×.
  **Where it stands** (tier 1, the full comparison): `try` 2.57×,
  `binding` 2.96×, making an instance 8.15×, a Buffer's `count` 3.45× and
  `nth` 4.01×: the done-check's 2×, 5× and 3× met. The log report 1.48×,
  the ledger 1.63×.

- **7, sequence functions: started (2026-11-17).** At the start (tier 1,
  per item): `mapv inc` 75 ns (1.18× the baseline), `into []` with `(comp
  (map inc) (filter even?))` 114 ns (1.46×), `reduce +` 12.6 ns (1.97×).
  A direct loop into a transient made `mapv` 17.9 ns. The transducer path
  spends its time in calls of closures held in locals (the call helper),
  persistent `conj`'s copying, and `even?`'s `rem` taking the generic
  division. In steps (decision 14):
  - **7.1, the library: done.** `%eager` (behind `map`, `filter`,
    `mapv` and the rest on a collection) and `into` build into a
    transient when they start from an empty vector (`%conj!`, conj! as a
    reducing function); `mapv` and `filterv` loop directly over vectors
    and ranges. **Measured:** `mapv inc` 75 → 18.3 ns an item (4.8×).
    **Found on the way:** a transient only pays from an empty vector.
    Skein's `transient` copies a vector into a new buffer (Clojure's
    shares it), so `into` onto a vector with items costs their count each
    time: `for`, which `into`s onto what it has built, went quadratic, and
    the ledger program took 201 ms against 126. A map's or set's transient
    wraps the persistent one (`conj!` assocs into it), only overhead. So
    `into` uses one only onto an empty vector.
  - **7.2, tier 1: done.** `quot`, `rem`, `mod`, `even?` and `odd?` on
    fixnums natively (on the raw 2n values: `srem` gives the tagged
    remainder; `mod` adds the divisor when the signs differ; `quot`'s one
    overflow, the least fixnum over -1, and a zero divisor go to the
    primitive). arith_test checks them on its edge inputs. **Measured:**
    `into []` through `(comp (map inc) (filter even?))` 102 → 56 ns an item
    (2.97×): `even?`'s `rem` had been the generic division.
  - **7.3, tier 1: done.** A call whose callee came from a local or an
    upvalue (a function passed in) writes the callee's frame itself when
    it's a closure with a fixed arity for the arguments (found at run
    time) and the frame fits, as a linked call does, and enters its code;
    anything else goes the general way. At every unlinked call it made the
    code 13% larger and the chat and site programs 3–4% slower (calls
    through vars that couldn't be linked gained only checks), so it's only
    where the callee came from a local. **Measured:** a closure call 1.44
    → 1.85×, `into` through a transducer 3.19×; the programs level or
    slightly faster (the log report 1.5%); code 0.2% larger. Tests:
    link_test's local closure calls (an upvalue, several arities, a rest
    parameter, a primitive, a closure compiled late, past a segment's
    end, an error).
- **The done-check: met (2026-11-17),** decision 2 since too (below).
  - **Speed** (`bench/compare.skn`, tier 1): Mandelbrot 29.1 ms, 10.0×
    the baseline (SBCL and Clojure 2.0 ms: Skein 14×); the empty loop 1.85
    ns a turn, 2.30× (from 1.7×: below); float `*` 9.9×; calls 5.4–7.5×,
    the checked one 47.7×; `try` 2.87×, `binding` 3.36×, making an
    instance 8.24×, a library collection's `count` and `nth` 3.8× and
    4.4×, `mapv` 5.3× and `into` 3.0× an item; the programs 1.32–1.68× and
    Mandelbrot's 10×; nothing marked slower (making a map 0.93×, within
    micro's noise).
  - **The empty loop**, 2.58 → 2.0 ns (runtime.md, tier 1's smaller
    things): a comparison and the `JMPF` testing it fused, through the
    `MOVE` an `if` leaves between them; no store of a temporary nothing
    reads after; plain loads of var roots and the attention epoch; and,
    most of it, GETVAR's two tests and the safepoint's two as two branches
    each, not their `and` (Cranelift had made each test a boolean: five
    instructions more, four times a turn). Tried and slower: var addresses
    as constants (four instructions to make each, against one load) and
    the safepoint's pointers loaded once an entry (spilled across calls).
  - **The tiers agree:** the suites at tier 0, tier 1 and
    `SKEIN_JIT_THRESHOLD=1`; every sibling project at both tiers but
    skein-studio's chat test (fails as before M10). Quire's two tier-1
    failures were its tests': "serving" checks the page once after the
    first reload, which can come from the files copied just before the
    watch starts (at tier 0 it arrives within a millisecond of the edit,
    too soon to be the edit's; tier 1 moves the timing), and "building"
    compares zips whose entries keep DOS times (2 s apart), so builds
    either side of a boundary differ. Each passed in reruns. (Fixed
    2026-11-17, in lang-tests/quire: the zip's entries dated at a fixed
    time, and "serving" waiting for the edit's title.) Studio's shell-in-the-editor and skein-ui's editor
    tests passed alone and in full runs.
  - **The tools on optimised code** (opt_tools_test, both tiers): the
    debugger stopped in a compiled float loop shows its unboxed float as
    a value, evaluates in the frame and restarts it with a new
    definition; in compiled code with an inlined call, the inlined frame
    and its caller's float so far; a value given for a failed call; an
    image saved after compiling runs. (`%proto-parts` now says whether a
    proto is `:native`, so the test checks it is testing compiled code;
    the functions are called through `apply`, as a small one called
    directly is inlined and its own code never runs.) prof_test and
    cover_test pass at both tiers, and ledger's coverage report is the
    same at both with `--serial` (in parallel, tier 1's hit counts vary
    by one or two run to run with the scheduling; the lines hit don't).
  - **Decision 2, tier 1 as the default: not yet** (done after
    background compilation, below). It wins on the
    baseline and agrees everywhere, but starting costs: `skein -e nil`
    takes 1.0 s against 0.10 at tier 0 (258 protos compiled, 2.9 MB), and
    a small project's tests 1.5 s more (ledger 0.43 → 2.05 s, skein-html
    0.5 → 2.1; the whole skein-lang suite about even, 16.4 → 17.1 s).
    `SKEIN_JIT_OPT=none` gives 0.81 s; a threshold of 1,000, 0.41 s. The
    cost is compiling on the carrier that's waiting to run the code.
- **Background compilation: done (2026-11-17)**, the user's choice for
  decision 2's start-up cost (runtime.md, tier 1's compilation). A proto
  that reaches the threshold is queued (its native word in the queued
  range, still counting entries) and runs in the interpreter until the
  compile thread installs its code (a compare-and-swap from the queued
  word; code for a proto changed meanwhile is dropped). The thread is a
  running safepoint participant only while it compiles; the queue is a GC
  root. The hottest queued is compiled first. First in, first out, the
  micro suite's first cases were timed before their code was compiled,
  behind the 350-odd functions start-up and loading queue (1.1 s of
  compiling). Newest first, opt_tools_test's warmed function waited past
  5 s behind newer ones in the full suite. `skein.bench/run` waits for the
  compile thread after warming up (its promise: timed compiled), and
  `jit-stats` has `:pending`. `SKEIN_JIT_SYNC=1` compiles on the carrier as
  before. **Measured:** `skein -e nil` 0.11 s at tier 1 (0.10 at tier 0);
  tests at tier 1 now faster than at tier 0 (ledger 0.57 → 0.43 s,
  skein-html 0.62 → 0.49, the skein-lang suite 17.6 → 15.0); the benchmarks
  as before (Mandelbrot 10.1×, the empty loop 2.3×, float `*` 10.5×;
  making a map read 0.86×, but the binary before this reads 75–86 ns on
  it run to run, as this one does). Tests: the suites at tier 0, tier 1,
  threshold 1 synchronous (everything compiled from its first call) and
  threshold 1 in the background (a stress test: about 2,000 installs
  racing running code), all passing; opt_tools_test's `warm!` calls its
  function until it's compiled, link_test's `many` waits for `:pending` 0
  (and calls through `apply`, with `twice` made not to inline, so its
  check of relinking sees a linked call). Every sibling project as before
  at both tiers.
  **Found on the way:** chat's test of state staying consistent sometimes
  waits out its 5 s timeout: under load the server stops answering one
  client's commands (it still hears the room) — at tier 0 too (1 round in
  30 of a probe of that test at each tier); tier 1 only moves the timing.
  The test passes regardless. Chat's or the runtime's (todo).
- **Decision 2: tier 1 is the default (2026-11-17, the user's go-ahead).**
  It won on the baseline, agrees with tier 0 on every suite and sibling
  project, and with background compilation starts as fast. `enabled()`
  in jit.rs now reads `SKEIN_TIER=0` as the way off, and anything else as
  on. The benchmark scripts print the tier they ran at (`jit-stats`), and
  ext.rs's GC-stress runs ask for tier 0 by name. The README and book
  chapter 26 say so.

- **M10: done (2026-11-17).** Every part (0–7), the done-check and
  decision 2: tier 1 is the default, compiled in the background, starting
  as fast as tier 0 (0.11 s against 0.10 for `skein -e nil`). Against the
  baseline at tier 1: Mandelbrot 10×, calls 5.4–7.5× (a checked one
  48×), `try` 2.9×, `binding` 3.4×, making an instance 8.2×, a library
  collection's `count` and `nth` 3.8× and 4.4×, `mapv` 5.3× an item; the
  programs 1.5–2.6× (the ledger 2.6×, the site 1.9×, the log report
  1.65×); nothing slower beyond noise. The suites pass at tier 0, tier 1
  and with everything compiled from its first call
  (`SKEIN_JIT_THRESHOLD=1 SKEIN_JIT_SYNC=1`: 333 tests, 3,455 checks; in
  that mode prof_test's rate check failed once, the flake below, and
  passed alone three times), and so does the whole cargo run. Tier 1 agrees with tier 0 on every sibling project (as at the
  done-check). **Left for after M10** (each here or in todo.md): the
  smaller speed items (`next` of a vector, a lazy sequence's step, `nth`
  of a non-ASCII string, `instance?` and `compare-and-set!` natively,
  contracts on collections and functions through the generic wrapper);
  inlining's limits untuned and tier 1's code size; the printing
  workloads' reading in a full comparison run; a primitive's error inside
  a nested run skipping `catch` (todo); chat's kicked clients (todo);
  waking fibers on other carriers (todo). Checking changes since:
  `lang-tests/run.skn` runs the five lang-tests programs (now in this
  repository) at all three tiers, against recorded outputs.

## After the parts (2026-11-16–17)

Work done after part 4 and alongside parts 5–7, and the gaps found on
the way: each says whether it's done. M10's close is the last entry of
Status, above.

- **Compile time: done (2026-11-17).** Inlining had made compiling
  slower: `http.skn` compiled and loaded in 67 ms at tier 0 (45 before
  part 4). A profile put most of it in the compiler's state, an atom of
  maps (`swap!`/`update` on every `emit!`, `alloc!`, `upd-ar!`), and in
  `konst!`'s linear scan of the constants. The state is now mutable:
  transient vectors used as records (the state, each function context,
  each arity, with the arity being generated kept on the state) and as
  buffers that grow in place (code, constants, locals, debug info,
  upvalues), and constants are found by an index on `%identity-hash`
  (checked with `identical?`, so equal constants stay apart as the seed
  keeps them; the GC doesn't move objects). **Measured:** 67 → 35 ms at
  tier 0 (1.23× the baseline) and 55 → 24–28 ms at tier 1 (2.0×);
  `skein -e nil` starts in 0.09 s at tier 0 (0.11). `gen-inline!` fell
  from about a quarter of compiling to 13%; caching inlined bodies or
  wrappers (registers differ per call site) wasn't worth it after that.
  Every function of the standard library, core and the expander (2,319)
  compiles to the same code as before at both tiers (with `SKEIN_CACHE=off`:
  a module loaded from the cache has its forms' shared strings apart, so
  their constants aren't shared, with either compiler). compiler_test
  checks constants are shared by identity. **Seen on the way:** group-by
  and frequencies read about 7% slower than with the old compiler, with
  the same code: the old one's garbage at boot grew the heap, so later
  work collected less often. With `SKEIN_HEAP_MIN=1g` both run it in 70 ms
  (49 at tier 1) against 200 (170): that workload is mostly GC, and the
  heap's growth policy is worth a look (part 6's allocations, or MMTk's
  dynamic heap size).
- **skein.core isn't inlined.** The Rust seed compiles it at boot, which
  keeps no IR, so `second`, `not`, `some?` and the like are linked (part
  3) but never inlined. Options: compile core with `skein.compiler` once
  it's installed, at image-build time (boot cost is then the build's,
  not the user's), or record IR for core's small functions by analysing
  core.skn after boot.

**Deferred to part 5 (with the user's agreement):**
  **Measured (2026-11-17): not worth it now.** An experiment recorded IR
  for core's 157 small functions after boot (expanded and analyzed, kept
  against the closures the vars hold: 257 ms), so code compiled after
  could inline them: the programs didn't move (log report and ledger 2%
  either way, site 1%). Their time isn't in calls of small core
  functions. A profile shows where it is: `reduce`'s loop (whose
  per-item primitives, `reduced?`, `unreduced`, `seq`, `next`, a string's
  `nth`, went through tier 1's general call: `reduced?` and `unreduced`
  are now native, and `reduce` over a vector went 11.2 → 4.6 ns an item,
  though the programs reduce over strings and lazy sequences) and the
  generic contract wrapper (`%wrap-for/fn`, 10–18% of the log report's
  and the ledger's samples by itself). The profiler also charges a
  function without a loop to its caller (samples are taken at entries
  and loop turns), so a step function's time shows as `reduce`'s.
- **Signing skein.string, skein.math, skein.regex and skein.bytes** (todo).
  skein.string was signed in part 4 and reverted: a probe of 1,519 calls
  on edge inputs showed no change in behaviour (with `replace`'s pattern
  nilable, as it must be for a function replacement), but its thinnest
  functions were slower a call: at tier 1 `blank?` 65 → 79 ns,
  `includes?` 46 → 72, `trim` 38 → 44, `starts-with?` 98 → 115; at tier
  0 50–90 ns more (`includes?` 138 → 225). Each check is a var read, a
  call and a branch through the frame's registers in memory: about 4–6
  ns at tier 1 and a full primitive call at tier 0. Part 5 is to drop
  the checks types prove (literal arguments too: the wrapper's tests on
  a `LOADK` are decidable when compiling). The signatures were reverted
  before part 4's commit, so they aren't in git history: they followed
  what each function accepts (`^String` where the primitives already
  reject anything else, `^(? String)` for `blank?` and `ends-with?`,
  `^(U String Character)` for `includes?`, `index-of` and `width`, only
  a result type where a function takes anything, as `upper-case` and
  `pad-left`). The probe was a scratch file; write it as a test when
  signing.
- **Values in registers.** Tier 1 keeps registers in the heap frame, so
  every instruction loads and stores; checks and arithmetic pay for it.
  Unboxed floats and SSA values are part 5's. Mandelbrot is still about
  117× SBCL (floats boxed, float arithmetic through primitive calls).

**Smaller gaps:**

- **The empty loop was 0.88× the baseline** at tier 1 (4.8 against 4.3 ns
  a turn) after part 2's switch to the `tail` calling convention: 2.30×
  at M10's done-check.
- **Printing workloads** (`str` of 100,000 pieces, JSON write, EDN
  `pr-str`, bin encode) read 0.84–0.9× in the full comparison run, at
  both tiers, but match part 3's binary when run alone: something the
  cases before them leave (heap, GC) — worth one look with
  `bench/compare.skn --only` and the profiler.
- **Contracts that aren't classes or predicates** (collections, `fn-of`,
  `defcontract` with a closure, a rest argument's) still go through
  `%wrap-for`'s generic wrapper, 0.1–0.8 µs a call. **Classes and keyword
  arguments: done (2026-11-17).** A class without a skein.core predicate
  (`Decimal`, `Ref`, `Collection`) went the generic way, and so did every
  function taking keyword arguments (variadic: `json/read-string`,
  `csv/read-maps`): 17 of the benchmark programs' call-site wrappers. A
  class contract now keeps its class (`:of-class`), checked at the call
  site with `instance?` through the class's var; a variadic arity without
  a rest contract is written with its fixed arguments alone (inlined) and
  with the rest through `apply`. **Measured** (a call's cost through the
  wrapper, beyond the function's own): `json/read-string` 111 → 2 ns,
  `math/mean` 207 → 26, `decimal/parse` 173 → 49 (`instance?` is still
  a primitive call at tier 1). The programs, with `reduced?` and
  `unreduced` native too: the log report 104.9 → 97.4 ms (1.53 → 1.65×
  the baseline), the ledger 122.8 → 117.2 (1.67 → 1.75×), the site level;
  less than the profile's 10–18% for `%wrap-for/fn` promised, which was
  mostly time in primitives (JSON parsing) charged to the frame that
  reached the next safepoint.
  inline_test checks a class contract, nil-or-a-class, keyword arguments
  with and without more, and a count no arity takes.
- **Per-item primitives: done (2026-11-17).** Primitives code calls once
  an item went through tier 1's general call: now native are `count` and
  `nth` of an all-ASCII string, `seq` and `next` of a list, `deref` of an
  atom, and `=` of identical values, of differing immediates
  (characters, booleans, nil) and of keywords. `swap!` and `vswap!` have
  fixed arities up to two arguments for f (each went through `apply`
  twice). **Measured** (tier 1, an item): `vswap!` 234 → 25 ns, `swap!`
  78 → 22, a `first`/`next` loop over a list 11.4 → 2.6, `nth` over an
  ASCII string 40.6 → 4.6, `=` of keywords 41 → 18, `reduce` over a list
  19.6 → 14.2 and over a string 28.5 → 18.3. The programs: the ledger
  117 → 79 ms (1.75 → 2.60× the baseline), the site 28.6 → 23.4 (1.52 →
  1.86×), chat 1.48×, the log report level (1.65×); the rope workloads
  3.0 → 4.95× and 2.9 → 4.4×, `swap!` 5.0×, `vswap!` 6.1×. `skein
  api-diff` now counts a variadic arity split into fixed ones and a later
  variadic one (as `swap!`'s) as taking every count it took. Still general: `next` of a
  vector (a seq made each step: 29 ns), a lazy sequence's step (44 ns
  for `(map inc v)`), `nth` of a non-ASCII string. Test:
  tests/skein/items_test.skn (edges against fixed answers: non-ASCII,
  out of range, mixed kinds, NaN, lazy tails, swap!'s arities, watches,
  contention).
- **The profiler's attribution: fixed (2026-11-17).** Samples were taken
  only at a function's entry and a loop's turn, so a function with no loop
  of its own, and the time of a primitive (JSON parsing, say), were
  charged to wherever the next safepoint was: the caller's loop, or the
  next function's caller. That put 18% of the log report under
  `%wrap-for/fn` and most of the ledger's CSV parsing under `reduce`. A
  sample is now also taken as a function returns (the interpreter's
  RETURN polls when attention has changed, and tier 1's RETURN bails to
  it then: a load and a compare, nothing measurable on a call), and as a
  primitive returns with one due (`call_prim`, for the calling frame).
  The programs' profiles now lead with JSON parsing and regexes (the log
  report) and the CSV parser's step function (the ledger); `reduce`'s
  own share fell from 2,394 and 5,025 samples to 176 and 158. prof_test's
  where-the-time-goes checks both cases at both tiers (each failed
  before: every sample was the caller's).
- **Sequence functions over one collection: done (2026-11-17).** Skein's
  `map`, `filter` and the rest are eager over collections, but went
  through a transducer (`%eager`: the step closure, `%conj!`, reduce),
  where `mapv` and `filterv` looped directly. Now `map`, `filter`, `keep`
  (and `mapv`, `filterv`) share direct loops: by index over a vector,
  range or string, a walk of the seq for anything else. `complement` has
  fixed arities (`remove`'s went through `apply` an item), and `for`'s
  innermost binding gives one value a step (or a skip, or a stop),
  collected in a transient, not a one-item vector poured into a
  persistent one. **Measured** (tier 1, an item): `map inc` over a vector
  40.1 → 17.2 ns, a list 48.8 → 25.7, a range 50.5 → 28.7; `filter`
  38.4 → 31.9; `remove` 118.5 → 33.6; `keep` 43.0 → 25.9; `for` over a
  vector 165.0 → 33.8; `(reduce + (map inc v))` 45.0 → 21.2. Test:
  items_test's sequence-functions (each kind of collection, `for`'s
  modifiers at each level), passing on the binary before too.
- **The shaken-image hang: found and fixed (2026-11-17); not M10's.**
  images.rs's shaken-image test hung now and then in a full `cargo
  test`: its `skein --image full.skimg` printed its first line, then spun
  on every core. A watcher rerunning the suite caught it twice (runs 5
  and 6); with the heap capped, SIGQUIT's dump showed one fiber
  signalling without end: `error` → `signal` → `throw-tag` (no catch) →
  `%runtime-error` → `error` …, in namespace skein.time. The cause: the
  reader calls a tag's reader (#inst's) *nested*, from Rust, and that
  loaded skein.time the first time. The cache lookup `slurp`s the module's
  entry inside a `try`, but a nested run can't catch a primitive's error:
  it returns to Rust past its frames. So when the entry was missing (other
  tests' processes prune the shared cache, oldest first, past 1000
  entries), the load was abandoned mid-way, and the reader went on in its
  namespace and dynamic bindings, with a handler-case's handler whose
  catch was gone. Fixed in three places: a failed nested run puts the
  dynamic environment and namespace back, and runs its frames'
  cleanups (`unwind-protect`, `finally`), as unwinding does; a library
  load forgets a failed load in `finally`, not `catch` (so a later
  `require` loads it again instead of waiting for it forever); and the
  cache looks for a missing entry, not catching it. Tests: cache_test's
  a-module-first-loaded-by-a-tag-reader, module_test's
  a-load-that-fails-in-a-tag-reader (both hung before). Left as it was:
  inside a nested run, a primitive's error still skips `catch` (todo). **Done
  since (2026-11-17, todo):** it's signalled to the handlers inside the
  run.
- **The profiler's timer threads: fixed (2026-11-17).** A profile's timer
  thread ran while any profile was active; one whose profile stopped and
  another started within its sleep went on too, so every later profile
  in a process sampled faster (prof_test's own run: 394, 473, 552, 621
  samples for the same 100 ms). Each thread now stops when its profile's
  generation passes. prof_test's profiles-one-after-another checks the
  rate stays put; where-the-time-goes compiles its functions first.
- **Inlining's limits are untuned:** 24 IR nodes, 96 for a contract
  wrapper, three calls deep, no inlining past 200 registers.
- **Code size:** part 3's linked calls made tier 1's code 15% larger (4.8
  MB for everything at `SKEIN_JIT_THRESHOLD=1`).

**Flaky tests seen under load (not regressions, each passing alone):**

- prof_test's sampling at `SKEIN_JIT_THRESHOLD=1` in the full suite (the
  busy fiber starved: compiles aren't preemptible); never at the default.
- errors_test's refused connection to port 1 (another error than
  "refused" when the other suites use many sockets).
- http_test's stale pooled connection (a 100 ms idle timeout racing the
  load).
- actor_test's supervisor strategies and module_test's cyclic loads
  (both timing: `eventually`, sleeps and timeouts), once at tier 0 in part
  6.4's full run; passing alone and in two full runs after. **Timing
  taken out (2026-11-17):** actor_test's `eventually` tries for 20 s, not
  2 (its only way to fail was a slow machine), and the cyclic modules
  tell each other their loads have begun through promises instead of
  sleeping 200 ms. conc_test's cancelled-cleanup check (CI's failure on
  x86-64 Linux, the same kind: a 5 ms sleep) waits for its child too.
- One tier-1 suite failure in part 2 (1 run in 34), never captured or
  seen again.
