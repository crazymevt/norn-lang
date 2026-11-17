# Plan: M8 — modules, hygiene and packages

Drafted 2026-11-14, for review (the questions are at the end). The
decisions behind it are in design §8.1 and §8.2, and the M8 row of
runtime.md §12. This plan says how to build it and in what order.

**What M8 is for.** Until now a file is a list of forms evaluated one by
one, macros are functions on plain data, and libraries are found on
`SKEIN_PATH`. M8 makes a file a module, with hygienic macros and phases,
and adds packages. These are what sealing needs: a sealed module is a
compiled unit with known exports, built by a deterministic expander.

**M8 is done (2026-11-16):** modules, packages and the verifier earlier,
and sealing last, its baseline recorded in `api/` (see Status).

**Sealing comes later** (decided 2026-11-14). It waits until the M7.6
done-check has been worked through, since sealing freezes the stdlib.
Three things wait with it, because each needs a baseline taken at sealing
or a sealed module to act on:

- direct linking, inlining and unsealing at the REPL (design §8.1);
- `skein api-diff` (each module's public interface, recorded when it's first
  sealed);
- the lint rule for redefining sealed vars.

M8's done-check (runtime.md §12) is "the stdlib built as sealed modules",
so it splits. M8 is done without sealing when:

- the stdlib, `skein-ui` and `skein-studio` build as modules through the
  hygienic expander;
- `skein-studio` builds from its project file, with `skein-ui` as a
  dependency;
- tier 0 and tier 1 still agree on every suite.

The sealing part's done-check stays as runtime.md has it.

## How things are now

- **Namespaces** are Clojure's. A namespace maps symbols to vars and has
  aliases. `ns` is `in-ns` plus `require`. `require` loads a library once:
  embedded in the binary, or found on `SKEIN_PATH`.
- **Macros** are functions marked as macros, run on plain data.
  - Syntax-quote qualifies symbols with their namespace and turns `x#` into
    gensyms. That's Clojure's level of hygiene: a macro's free names can't
    be captured by the user's.
  - Nothing stops a macro's own bindings capturing the user's, or a local
    from shadowing a name a macro relies on.
- **There are no phases.** A macro calls whatever is loaded at the time.
  So a file's meaning can depend on what else was loaded first, and
  compiling it again elsewhere can differ.
- **The compiler** resolves symbols through the current namespace as it
  compiles each form. It's a byte-for-byte port of the Rust seed.
- The stdlib has 83 macros (`defmacro`), and 32 libraries begin with `ns`.
  `skein-ui` and `skein-studio` add their own. All of it has to keep
  compiling, which is why `defmacro` sits on top of the hygienic expander
  (design §8.2) rather than beside it.

## Parts

| Part | Work |
|---|---|
| 8.1 | **Syntax objects:** a datum with a set of scopes and a source location. Identifiers, `syntax`/`quasisyntax`, `datum->syntax`, `syntax->datum`, `syntax-e`, `free-identifier=?` and `bound-identifier=?`. The reader gives locations; the comment-keeping reader already tracks them. |
| 8.2 | **The expander, in Skein:** Racket's sets-of-scopes. It expands to the kernel forms (runtime.md §3.5), with each reference resolved to a local or a var, and the existing compiler takes its output. `defmacro` and syntax-quote work on top of it: a datum macro gets the use-site and introduction scopes, so the stdlib's 83 macros keep working. The REPL top level stays permissive (design §8.2). |
| 8.3 | **Pattern macros:** `syntax-case` and `syntax-rules`, then a core of `syntax-parse`: patterns, syntax classes, `~optional`, `~or` and `~seq`, and error messages that name what was expected and where. |
| 8.4 | **Modules:** one file is one module, with `ns` as its header. A module has exports (its public vars; `defn-` is private and enforced), its requires are resolved when it's expanded, and a registry instantiates each module once. Redefinition at the REPL stays live: modules are open until sealed. |
| 8.5 | **Phases:** `(:require-for-syntax …)` in `ns` and `begin-for-syntax`, so a macro's helpers are loaded for compile time explicitly, and each phase instantiates its own copy of a module. Compiling a module gives the same result whatever else is loaded. |
| 8.6 | **Compiled modules on disk:** each module's expanded, compiled form is cached, keyed by a hash of its source and its dependencies'. A cached module loads without expanding again, and a changed dependency recompiles what depends on it. |
| 8.7 | **The bootstrap and the stdlib:** the seed compiler (Rust, non-hygienic) still loads `core` and the compiler. The expander loads after them, and everything is compiled again through it into the base image (runtime.md §9). The stdlib, `skein-ui` and `skein-studio` move to modules. Hygiene tests cover the classic cases: `or` with a user's `t`, `swap!`-style temporaries, and a shadowed `if`. |
| 8.8 | **Lint rules that need modules:** unused requires, missing requires, using a private var from another module, and phase errors (a macro calling a function that isn't required for syntax). |
| 8.9 | **The macro stepper:** expand a form one step at a time, showing each step's scopes and renames. It's available in the REPL (`macro-steps`), from the CLI (`skein expand --steps`), and as an editor op (`:expand-steps`) for Skein Studio. |
| 8.10 | **Packages:** a project file, Git and path dependencies, a lock file, fetching into a cache, resolution, and `skein build` across dependencies. `skein-ui` and `skein-studio` get project files, and `SKEIN_PATH` becomes a fallback. |
| 8.11 | **A bytecode verifier:** it checks compiled code from the module cache and from images built elsewhere before it runs: register and constant bounds, jump targets, arities, and the instruction set. |

Later, after the M7.6 done-check: **sealing** (direct linking, inline
caches for var reads, unsealing that deoptimises), `api-diff` with its
baseline, and the sealed-var lint rule.

**Order.** 8.1 → 8.2 → 8.7's bootstrap (the stdlib compiles through the
expander with `defmacro` unchanged, and every suite passes) → 8.4 → 8.5 →
8.6 → 8.3 → 8.8 and 8.9 → 8.10 → 8.11. The expander replaces macroexpansion
under everything, so it lands and proves itself on the existing code before
modules and phases change what that code means.

## Decisions (the user, 2026-11-14)

The draft's proposals, accepted:

1. **`#lang` is dropped** from the design unless a need appears. Modules,
   hygiene and phases all work from the `ns` form, and the reader refuses
   `#lang`.
2. **A module is expanded as a whole**, so it can refer to names defined
   later in it, and gives sealing a complete unit. The REPL stays form by
   form. (Amended 2026-11-14: "as a whole" is its names declared first,
   then its forms expanded in order, which gives both. Expanding every
   form before running any, in two passes, is dropped unless sealing
   turns out to need it.)
3. **Each phase instantiates its own copy of a module** (Racket), so
   compiling a module gives the same result whatever else is loaded. The
   REPL top level stays permissive.
4. **`syntax-parse` in M8 is its core:** patterns, syntax classes,
   `~optional`, `~or`, `~seq`, and error messages. The rest comes when it's
   needed. `syntax-case` and `syntax-rules` come with it.
5. **The stdlib's macros stay `defmacro`.** Only those whose errors users hit
   most are rewritten with `syntax-parse`: `let`, `fn`, `defn`, `loop`,
   `cond`, `case` and `ns`.
6. **Compiled modules are cached on disk in M8** (8.6).
7. **The project file is `skein.edn`** (`:name`, `:version`, `:paths`,
   `:deps`), with `skein.lock` recording each Git dependency's commit. A Git
   dependency names a tag or a commit, a path dependency a folder. Two
   versions of one package in a build are an error showing both chains,
   unless an override in the project file settles it.
8. **Packages stay in M8**, last. They split into their own milestone only
   if M8 runs long.

## Status

**8.1, syntax objects (2026-11-14): done.**

- **`skein.syntax`**, an embedded library:
  - `Syntax`, a class with identity equality, holding the datum, the
    scopes and the place;
  - `syntax-e`, `syntax->datum`, `datum->syntax`, `syntax->list`,
    `identifier?`, and the place's parts (`syntax-line`, `syntax-where`…);
  - scopes, which are integers: `new-scope`, and `add-scope`,
    `remove-scope` and `flip-scope` over a whole tree;
  - `bound-identifier=?`;
  - `read-syntax` and `read-all-syntax`.

  `free-identifier=?` needs binding resolution, so it comes with the
  expander (8.2).
- **The reader records places** when asked (`%read-syntax`): a tree beside
  the form, giving each part its line, column, offset and span.
  - Lines and columns count characters, not bytes.
  - `'x` and the other prefixes give the marker its own place.
  - A map's or set's parts keep their places, though the map reorders them.
  - Metadata and `#_` leave no trace.
  - A form the reader rewrites (syntax-quote, `#(…)`) gives all its parts
    its own place.
- **`#lang` is gone.** The reader refuses it, and `core.skn` and
  `compiler.skn` no longer start with it.
- **Tests:** `syntax_test.skn`, 55 checks, at tier 0 and tier 1. Seven
  library files, `core.skn` among them, read the same as data and as
  syntax.
- **Speed:** `read-syntax` takes 34 ms on `core.skn` (110,000
  characters), where the plain reader takes 5 ms. Most of it is building
  the syntax objects in Skein. Compiled modules are cached (8.6), so this is
  paid only when a module changes; building them in Rust is the fallback
  if it matters.
- **Scopes are added eagerly,** walking the whole tree. Racket propagates
  them lazily. If the expander's speed needs it, laziness can go in behind
  the same functions.

**8.2, the expander (2026-11-14): done, and the stdlib passes through it
unchanged.**

`lib/compiler/expander.skn` loads at boot after the compiler and becomes
`eval`. Every form goes through it: `SKEIN_EXPANDER=off` keeps the old path,
for comparison. It gives the compiler kernel forms with names resolved,
using Racket's sets of scopes:

- **Bindings:** a binding form adds one scope. Of two bindings of a name with
  the same scopes, the later wins, so `(let* [x 1 x (inc x)] x)` is still 2.
- **Locals** become uninterned symbols with the user's names. The compiler
  matches locals by identity, so hygiene needs no renaming the user can
  see. The debug info names them with plain symbols, so the debugger and
  `eval-in-frame` are as before.
- **Globals** resolve in the namespace whose scope the name carries (code
  read in a namespace carries its scope). A hygienic macro's template
  therefore means what it meant where the macro was defined, its own
  private functions included.
- **`defmacro` macros** get plain data, as before; their result takes the
  call's scopes. Parts of the result that are the very data the macro was
  given go back to being the syntax they were.
- **`defsyntax` macros** (in `skein.syntax`) get syntax. A fresh scope is
  added to their input and flipped on their output. `quote-syntax` is a new
  kernel form, and `free-identifier=?` asks the expansion running now.
- **A macro's own errors pass through as they are,** keeping their condition
  and restarts. The expander's own errors say where:
  `e.skn:1:8: Bad binding form in let*: 1`.

Tests: `expander_test.skn` has 24 checks at both tiers, covering the
classic hygiene cases both ways, macros from another namespace,
`free-identifier=?`, and anaphoric `defmacro`. All 1,180 checks of the
suites pass through the expander, as do `skein-ui`'s and `skein-studio`'s. The
linter knows `quote-syntax`, and resolves names defined earlier in the
same file under another namespace.

**What it costs, and why.** The old path only looked at the head of each
list; the expander visits every part. Three changes cut the cost:

- **Lazy wrapping:** code arrives as data, and a raw syntax object wraps
  its parts only when looked at.
- **Lazy scopes** (pending scope changes).
- **A fast path for `defmacro` steps whose arguments are plain data with
  the call's scopes,** the usual case: the macro's own work, and no more.

The remaining costs:

| | Old path | Expander |
|---|---|---|
| `fmt`, `lint`, `markdown`, `format` from source | 55 ms | 101 ms |
| Skein Studio's code | 0.44 s | 0.77 s |
| Start-up from source (boot compiles the expander) | 40 ms | 60 ms |
| Start-up from an image | 40 ms | 40 ms |

The compiled-module cache (8.6) takes the loading cost away for anything
unchanged.

**For 8.4:** private vars aren't enforced yet. When they are, a macro's
template must still reach its own namespace's private functions, as
`expander_test` checks, while other code can't.

**8.4, modules (2026-11-14): done.**

- **A file that starts with `(ns …)` is a module.**
  - Before its forms run, the names its defining forms introduce are
    declared in its namespace (`def`, `defn`, `defn-`, `defmacro`,
    `defclass`, `defgeneric`, `declare`, `defsyntax`, `deftest`,
    `defcfun`). So a function may call one defined further down.
    `register-definer!` adds a library's own defining macro.
  - The forms are then read and evaluated in order, as syntax with places.
    Errors inside a module say where (`mt/bad.skn:4:8: …`).
  - Two things wait for phases (8.5): expanding every form before running
    any, and so evaluating macros apart from their helpers.
  - A file without `ns` is a script, read form by form as before.
- **Private vars exist, and are enforced.**
  - `defn-` and `(def ^:private x …)` mark a var private. The reader now
    keeps metadata as a property of the syntax it reads (`syntax-property`)
    rather than dropping it.
  - Naming another namespace's private var is an expansion error:
    `mt.a/helper is private to mt.a`.
  - A namespace's own macros may use its private helpers. A hygienic
    template resolves in its own namespace. A `defmacro`'s result carries
    its namespace's *access scope*, which grants that access without
    changing where names resolve, and doesn't take part in binding.
  - `#'ns/private` still reaches one on purpose, as in Clojure.
  - `ns-publics` leaves private vars out, so `:refer` can't take them. The
    new `ns-interns` lists them all.
- **The registry:** `module-info` gives a module's path, the modules it
  requires, and what it defines. `(require 'm :reload)` loads a module
  again, redefining it in place: modules stay open until sealed.
  `add-lib-path!` adds a folder to the library search, for projects (and
  for packages, 8.10).
- **`read-syntax` is now lazy too.** A form read from source is raw. Its
  parts get their places from the reader's location tree when looked at,
  so loading through syntax costs little more than reading data.
- **The linter** knows modules: forward references in a module aren't
  unresolved.
- **The test runner** finds a file's namespace by reading forms, not with a
  regex, so a `(ns …)` in a comment no longer confuses it.
- **Tests:** `module_test.skn`, 18 checks at both tiers. All suites pass,
  as do `skein-ui`'s and `skein-studio`'s, with no private access to fix.
- **Seen in passing:** `start_up_from_an_image_is_fast` (under 50 ms)
  fails when the whole suite runs in parallel. Alone it takes 40 ms and
  passes. The test's threshold or its isolation needs a look.

**8.5, phases (2026-11-14): done, strict in modules, permissive at the
REPL.**

- **The measurement first.** Of every macro in `lib/skein`, `skein-ui`,
  `skein-studio` and the tests, one called a non-core helper while expanding
  (`skein.test/are`, using `skein.walk`). None used another macro of its own
  module. So Racket's strictness cost one line of migration, plus one
  test's helper that strict phases rightly refused.
- **Instances.** A module's compile-time instance is a namespace of its
  own, `name$1` (`name$2` one more up; `$`, since
  `@` ends a symbol for the reader). Loading it runs the module's
  source again into that namespace; requiring from a namespace at shift n
  loads the library's instance at shift n.
  - `skein.core`, `skein.syntax`, `skein.compiler` and `skein.expander` are the
    language itself: one instance, shared by every phase. A library with no
    source (a native extension's) has one instance too.
- **Kernel form `(for-syntax e)`:** e is expanded one phase up, its names
  resolved in compile-time instances. `defmacro` and `defsyntax` wrap
  their transformers in it.
- **Getting helpers to compile time:**
  - `begin-for-syntax` defines them in the module's compile-time instance;
  - `(:require-for-syntax spec…)` in `ns` (or `require-for-syntax`)
    requires a library there.
- **Resolving names one phase up:**
  - a namespace written in full (`skein.walk/…`) means that phase's
    instance;
  - a name from the language itself means the same at every phase, however
    the module required it, so `syn/datum->syntax` needs no second require.
- **In a module,** a compile-time name not found one phase up is an error
  that says what to do: `helper isn't available at compile time here:
  define it inside begin-for-syntax, or require its module with
  :require-for-syntax`, with the place.
- **At the REPL and in scripts** (no `ns`), such a name is taken from run
  time, as decided ("the top level is hopeless").
- **What it guarantees.** Compile-time code works on its own instances, so
  compiling a module neither depends on nor changes run-time state: a
  counter bumped while compiling isn't the run-time one (`phase_test`).
  Dynamic lookups (`resolve`, `find-ns`) still see every namespace, as
  Racket's namespace reflection does.
- **Not done: expanding a module in two passes** (every form expanded
  before any runs, decision 2). Modules declare their definitions up front
  and expand form by form. That gives forward references, and with phases a
  form's compilation can't depend on earlier forms' run-time effects. The
  two passes matter for the compiled-module cache (8.6), which wants a
  module as one unit, so they're proposed for 8.6. A few stdlib macros look
  things up dynamically while expanding (`resolve`), and these would need
  care there.
- **Tests:** `phase_test.skn`, 12 checks at both tiers. All suites pass, as
  do `skein-ui`'s and `skein-studio`'s. The linter knows `begin-for-syntax`.

**8.6, the compiled-module cache (2026-11-14): done.**

- **What's cached:** a module's expanded code, unit by unit (each compiled
  top-level form, with the namespace it was compiled in), in a file in the
  cache folder (`SKEIN_CACHE`, `off` for none; else the system's cache
  folder).
- **The key:** a hash of the build (runtime and embedded libraries), the
  module's name, place and source, and the keys of the modules its `ns`
  form requires, at both phases. A change anywhere recompiles what depends
  on it.
- **On a hit:** the source is read only as data (for the definitions index
  and error places), and the cached units are compiled and run. Nothing is
  expanded again.
- **Hygiene across processes.** Expanded code holds three things that don't
  print as themselves, and each is encoded:
  - **locals** (uninterned symbols), by identity;
  - **syntax objects** from hygienic templates, whose scopes become their
    namespace's, its access scope, or a number standing for one fresh scope
    throughout the module;
  - **vars as constants** (in `defmethod`'s expansion), by name.
- **Anything else that doesn't read back as written** (a function as a
  constant) leaves the module uncached rather than cached wrong;
  `*cache-stats*` counts it.
- **Housekeeping:**
  - the folder keeps at most 1,000 entries, dropping the oldest;
  - a damaged entry is ignored and rewritten;
  - `skein build` compiles fresh, so the same sources still give the same
    image (an image is the cache for its own start-up), and an image finds
    its cache folder on the machine it runs on (`%restore-hooks`).
- **Compile-time instances are named `name$1`, not `name@1`:** `@` ends a
  symbol for the reader, so code naming one couldn't be written and read
  back.
- **`(require 'm :reload)` now drops every phase's instance of `m`,** so a
  dependent's macros don't run against a stale compile-time instance.
- **Studio's code loads in 0.54 s from a warm cache.** That compares with
  0.77 s through the uncached expander and 0.47 s before it. The first load
  after a change takes 1.3 s, writing 39 entries.
- **Tests:** `cache_test.skn`, 15 checks at both tiers: hits; recompiling
  on a change to the module or to a dependency; hygiene and phases surviving
  the cache; an uncacheable module; a damaged entry; no cache folder. The
  whole suite passes twice against one cache, the second run entirely from
  it.
- **`start_up_from_an_image_is_fast`** now runs alone among the image tests
  (a lock), and passes in full runs.
- **Two-pass expansion is still not done,** and it isn't needed for the
  cache. Recording units in order makes the cached module one unit, which
  is what sealing needs too. The two passes would change when run-time
  forms run, which the stdlib's dynamic lookups at expansion time depend
  on. Proposed: drop them from decision 2, unless sealing turns out to
  need them. **Decided (the user, 2026-11-14): dropped**, unless they
  turn out to be worth having.

**8.7, the bootstrap (2026-11-14): done, with nothing to rebuild.**

- **The base image already is what the expander makes.** `core`, the
  compiler, `skein.syntax` and the expander are compiled at boot before the
  expander is installed. `skein.expander/compare-expanded` compiles each
  form of a source both ways and lists the forms whose code differs: for
  all four (481 forms in `core` alone) the list is empty. So compiling them
  again through the expander would give the same image, and the seed path
  stays as it is. `%expander-source` gives the expander's text for the
  check.
- **The comparison does notice a difference** (a test: a form using
  `quote-syntax`, which only the expander knows).
- **The stdlib, `skein-ui` and `skein-studio` are already modules:** each
  file starts with `ns`, so since 8.4 each loads as one.
- **Hygiene's classic cases** are tested: `or` with the user's `t`, a
  temporary in nested macros, a free name the user shadows, and a private
  helper reached from another namespace. A shadowed `if` can't happen: a
  local doesn't shadow a special form (as in Clojure), so a template's `if`
  is always the special form.

**8.3, pattern macros (2026-11-14): done.**

- **In `skein.syntax`:**
  - `syntax-case` (with fenders and literals), `syntax` (templates, with
    `...`, `& x` and `(~? t u)`) and `syntax-rules`;
  - `syntax-parse` and `syntax-parser`, with syntax classes (`x:id`,
    `x:expr`, `x:str`, `x:nat`… and `define-syntax-class`), `~literal`,
    `~or`, `~optional`, `~seq`, `~describe` and `...+`, and `attr` for a
    pattern variable's match in plain code;
  - `SyntaxError`, which `syntax-error` signals at a piece of syntax's
    place.
- **The `~` forms read as `(unquote x)`** (`~` is unquote for the reader),
  and the matcher recognises them that way, so no reader change.
- **The matcher backtracks** (written in continuation style, since there's
  no lazy concatenation). A failure records how far into the syntax it got;
  when nothing matches, the error is the furthest one, at that part's
  place, naming what was expected there: `my-let: expected identifier`,
  `p.skn:1:15: …`.
- **The core forms' shapes are checked (decision 5).** Instead of
  rewriting `let`, `loop`, `fn`, `defn`, `cond`, `case` and `ns` as
  `syntax-parse` macros, they stay the `defmacro`s they were, and
  `skein.validate` (an embedded library, loaded the first time a macro
  expands) checks each call first:
  - `(let [a 1 b] a)` → `let: expected a binding vector [name value …]`
  - `(fn f [x 1] x)` → `fn: expected a parameter (a name, &, a vector or a map)`
  - `(cond (= 1 1) :yes :else)` → `cond: expected a result after the test`
  - `(ns foo (:requir [x]))` → `ns: expected an ns clause, (:require …) or (:require-for-syntax …)`
    (a mistyped clause used to be ignored).

  A quick look at each call's shape passes almost every call, and only one
  it doesn't goes through the `syntax-parse` pattern. Uncached, this costs
  about 5% of expansion (134 ms against 129 ms to expand `skein.lint`,
  `skein.markdown` and `skein.test`; matching every call cost 80%). A cached
  module isn't expanded, so it costs nothing there.
- **A module-cache bug, found by it:** a module loaded while another form
  was expanding (as `skein.validate` is) recorded its forms under that
  form's namespace, so it broke when loaded from the cache. Running and
  loading now use the current namespace; `cache_test` checks it.
- **The linter knows hygienic macros:** it evaluates `defsyntax`,
  `define-syntax-class` and `begin-for-syntax`, expands a hygienic macro's
  call through the expander, and reports a call the macro refuses as a
  `:macro` warning. A local passed to such a call counts as used.
- **Image builds stay reproducible:** the validators are keyed by name
  (keyed by var, the map's order followed identity hashes).
- **Tests:** `pattern_test.skn`, 40 checks at both tiers, and a sixth
  test in `cache_test`.

**8.8, lint rules that need modules (2026-11-14): done.**

- **Four rules in `skein.lint`.** The last three apply in modules:
  - `:private`: a full name for another module's private var (`(var …)`
    reaches one on purpose, and isn't reported);
  - `:unused-require`: a require whose alias and refers are never used,
    or a referred name that isn't. A require with neither may be for its
    effect, so it isn't reported, and the message says to require
    without `:as` for that;
  - `:missing-require`: a full name from a namespace the module doesn't
    require, which resolves only because something else loaded it (the
    language's own namespaces need no require);
  - `:phase`: compile-time code using a run-time name, such as a
    `defmacro` calling a function of its own module or one from a library
    required only for run time. The message says what to do: use
    `begin-for-syntax`, or `:require-for-syntax`.
- **How it knows a phase.** `defmacro` and `defsyntax` wrap their
  transformers in `for-syntax`, so the linter analyzes those as
  compile-time code, along with `begin-for-syntax`. A name there is looked
  up in the module's compile-time instance (`name$1`, made when the
  linter evaluates the `ns` form and `begin-for-syntax`); the language's
  namespaces count at every phase. A template's names are quoted, so
  `` `(helper ~x) `` is fine.
- **Uses are counted from the source as written,** templates included:
  syntax-quote resolves aliases when it's read, so an alias used only in
  a template counts.
- **Findings fixed:**
  - in skein-lang: unused refers in eight test files (`deftest`,
    `testing`, `are`, `syntax-parse`), and `skein.string` in `skein.check`;
  - in skein-ui: one unused refer;
  - in Studio: 13 requires kept only for their effect, now written
    without their unused alias, and two unused refers.

  There were no `:phase`, `:private` or `:missing-require` findings in
  any of the three.
- **Tests:** `module_test`'s linter test now has 12 checks covering each
  rule and the cases each must not report.

**8.9, the macro stepper (2026-11-14): done (Studio's view of it is for
later).**

- **`skein.expand/macro-steps`** expands a form fully, recording each macro
  step as the expander takes it, outermost first, through a hook in the
  expander (`*on-step*`). What a macro's own code expands while it runs
  isn't a step of the form's expansion, so the hook is off inside it.
  Each step gives:
  - the macro;
  - whether it's hygienic;
  - where the call is;
  - the call before and after;
  - the names it introduced.

  The full expansion comes last.
- **What a step introduced.** A hygienic macro's names are exactly the
  identifiers its introduction scope marks after the flip, and the step
  reports that scope. A defmacro works on data, so its names are found by
  name: those in its result that its arguments didn't hold. A name it
  introduces that the call also wrote (`aif`'s `it`) isn't listed.
- **Renames.** In the full expansion, locals that share a name are
  numbered apart, which is what hygiene did:
  `(let [t 5] (my-or false t))` →
  `(let* [t#1 5] (let* [t#2 false] (if t#2 t#2 t#1)))`, with
  "t#1, t#2: different locals, each written t". A template's syntax shows
  as the `quote-syntax` it came from.
- **Where it's available:**
  - **The REPL:** `(macro-steps '(…))` prints the steps.
  - **The command line:** `skein expand [--steps] FILE | -e EXPR` prints
    each top-level form's full expansion, or every step with `--steps`. A
    file's forms run after they're shown (a module's names are declared
    first, as loading does), so later forms see their macros; an
    expression is only expanded.
  - **Editors:** the `:expand-steps` op returns the steps with forms as
    text, plus the whole thing as `:text`. Studio doesn't show it yet.
- **Tests:**
  - `expander_test`: 13 checks covering the steps, introduced names,
    scopes, renames and text;
  - the editor protocol test (`:expand-steps`);
  - a CLI test (`skein expand --steps` on a module that uses another's
    macro, and `-e`).

**8.10, packages (2026-11-14): done, with Git and path packages
(decision 7).**

- **`skein.edn`**, a project's file, has these keys:
  - `:name` and `:version`;
  - `:paths`: its source folders (default `["src"]`);
  - `:deps`: each `{:path "folder"}`, `{:git "url" :tag "tag"}` or
    `{:git "url" :sha "commit"}`;
  - `:overrides`;
  - `:main`: `skein build`'s entry.

  A package is a project used as a dependency. A folder without a
  `skein.edn` is a package whose source is in `src`. Paths are relative to
  the `skein.edn` that names them.
- **Resolution** (`skein.deps/resolve-project`) walks the dependencies
  from the project. One version of a package reached two ways is fine. Two
  versions (two folders, or two commits) are an error showing the chain to
  each:

  ```
  Two versions of c in the build:
    path …/c1, by clash → a → c
    path …/c2, by clash → b2 → c
  Add c to :overrides in …/skein.edn to choose one.
  ```

  An override is used wherever the package is asked for.
- **`skein.lock`**, beside `skein.edn`, records the commit of every Git
  package in the build, transitive ones included:
  - a locked package stays at its commit when its tag moves;
  - only a new or changed one is looked up (`git ls-remote`);
  - `skein deps --update` looks them all up again;
  - a short `:sha` is resolved to the full commit.
- **Fetching** clones a Git package with the `git` command into a cache:
  `SKEIN_DEPS`, else the module cache folder's `deps`, one folder per
  package and commit. A clone is made beside the target and renamed into
  place, so a half-done fetch is never used.
- **`skein` uses the project it finds** (in the folder of the file it runs,
  else the current one, or a folder above). The project's paths, then its
  packages', go on the library path, ahead of `SKEIN_PATH`, which is now the
  fallback. `skein deps` lists the packages and the chain that brought each
  in, and writes `skein.lock`. In a project, `skein build OUT.skimg` with no
  files loads `:main`'s namespace and makes `:main` the entry, so a
  project builds across its packages.
- **skein-ui and Studio have project files.**
  - skein-ui's `skein.edn` is a package whose source is `lib`. Its Rust
    engine is still the `skein-ui` binary, which embeds the same source.
    Packages carry Skein source; native parts come with the binary that
    runs them.
  - Studio's `skein.edn` uses skein-ui by path (`../skein-ui`), with
    `:main studio.main/main`, a new entry.
  - Studio's scripts no longer set `SKEIN_PATH`.
  - `skein-ui build studio.skimg` in Studio's folder builds an image (65,289
    objects, 3.7 MB) that starts Studio. **That's M8's second done-check
    item.**
- **Tests:**
  - `deps_test.skn`, 22 checks at both tiers: finding and reading
    projects; path and Git packages; the lock across a moved tag and
    `--update`; a diamond; two versions with both chains; an override; a
    short commit;
  - a CLI test: a script run from another folder, `skein deps`, and
    `skein build` from `:main`.
- **Not done:** a registry, and version ranges. A dependency names one
  version, and conflicts are settled by an override (runtime.md has the
  registry later).

**Sealing (2026-11-16): done. The standard library is sealed: its
baseline is recorded in `api/`.**

- A sealed flag per module: `sealed?`, `seal!`, `unseal!`. The standard
  library is sealed as it loads (core, the compiler, `skein.syntax` and the
  expander when the expander installs); `skein build` seals what it loaded.
- Redefining a var of a sealed module unseals it, warning once. The
  `sealed` lint rule reports definitions after `in-ns` into a sealed
  module other than the file's own.
- `skein api-diff` (`skein.api-diff`): each module's interface, recorded
  under `api/`, and the changes since, breaking or compatible.
  `api_diff_test.skn` checks the standard library once `api/` exists.
- **Decided (the user, 2026-11-16):** direct linking, inlining and
  deoptimisation move to M10, where inlining pays for them; redefinition
  unseals with a warning rather than failing; release builds seal a
  project's modules; baselines are files under `api/`, checked by a test.
- **The baseline (2026-11-16).** Taken before the M7.6 working stretch,
  the user deciding, after five test programs (pulse, ledger, quire, chat,
  loglens, in `lang-tests/`) and an interface sweep found what they could:
  65 modules recorded in `api/`. Recording is deterministic (the same at
  both tiers). `api_diff_test.skn`, run by `skein test`, `skein check` and
  cargo's tier comparison, fails on a breaking change until it's recorded
  on purpose (`skein api-diff --record`, and the change to `api/`
  committed); additions are compatible.

**8.11, the bytecode verifier (2026-11-14): done.**

- **Why.** The interpreter and the tier-1 compiler trust code completely.
  They use register, constant, upvalue and jump operands without checks,
  read a template's captures from the creating frame, and resume a parked
  fiber wherever its frames say. `crates/skein-vm/src/verify.rs` checks
  code from elsewhere before any of it can run.
- **What a proto must satisfy, for the template that owns it:**
  - its fields are what they should be;
  - it has at most 255 registers, and its parameters fit them;
  - every opcode is known;
  - every operand is in range:
    - registers below the count;
    - constants inside the array, a var for `GETVAR`/`DEFVAR` and a
      template for `CLOSURE`;
    - upvalues below the template's count;
    - jump targets inside the code;
    - a call's arguments inside the registers;
  - a new closure's captures name registers and upvalues of the creating
    frame;
  - nothing falls off the end of the code.

  Templates' captures must match their upvalue count, and closures must
  have room for their template's upvalues.
- **Where it runs:**
  - **Images, as they load:** every template, closure, proto and parked
    fiber. A proto may not carry a tier-1 entry (native code is per
    process). A fiber's frames must fit their segments, run one of their
    function's arities, resume inside its code (after an instruction, when
    a value is delivered there), and end. A failure refuses the image:
    "… is not a valid Skein image: its code: …".
  - **`%make-template`,** which the Skein compiler assembles every function
    with, and which any Skein code can call: a template is checked when
    it's made, which is when its protos become runnable. So the compiler's
    output is checked as well as crafted code. The Rust seed compiler,
    which compiles core at boot, is trusted.
  - **Not the module cache:** it holds expanded source, which this
    process's compiler compiles again, so no bytecode crosses there.
- **`SETUP`'s closure is only known at run time,** so the interpreter
  checks it there: a missing upvalue is an ordinary error, with a
  use-value restart. Tier 1 now hands `SETUP` (letrec only) to the
  interpreter rather than writing the upvalue unchecked.
- **Cost:** none measurable. Expanding and compiling `skein.lint`,
  `skein.markdown`, `skein.test` and `skein.deps` takes about 177 ms with or
  without it, and loading an image 0.03 s.
- **Tests:**
  - `verify_test.skn`, 14 checks at both tiers: good code runs; each
    fault is refused when the template is made, with what and where; and
    `SETUP` is checked when it runs;
  - an image test: a saved proto damaged two ways (a native entry, too few
    registers) with the checksum made right again, refused with the
    reason.
