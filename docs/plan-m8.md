# Plan: M8 — modules, hygiene and packages

Drafted 2026-11-14, for review (the questions are at the end). The
decisions behind it are in design §8.1 and §8.2, and the M8 row of
runtime.md §12. This plan says how to build it and in what order.

**What M8 is for.** Until now a file is a list of forms evaluated one by
one, macros are functions on plain data, and libraries are found on
`NORN_PATH`. M8 makes a file a module, with hygienic macros and phases,
and adds packages. These are what sealing needs: a sealed module is a
compiled unit with known exports, built by a deterministic expander.

**Sealing comes later** (decided 2026-11-14). It waits until the M7.6
done-check has been worked through, since sealing freezes the stdlib.
Three things wait with it, because each needs a baseline taken at sealing
or a sealed module to act on:

- direct linking, inlining and unsealing at the REPL (design §8.1);
- `norn api-diff` (each module's public interface, recorded when it's first
  sealed);
- the lint rule for redefining sealed vars.

M8's done-check (runtime.md §12) is "the stdlib built as sealed modules",
so it splits. M8 is done without sealing when:

- the stdlib, `norn-ui` and `norn-studio` build as modules through the
  hygienic expander;
- `norn-studio` builds from its project file, with `norn-ui` as a
  dependency;
- tier 0 and tier 1 still agree on every suite.

The sealing part's done-check stays as runtime.md has it.

## How things are now

- **Namespaces** are Clojure's. A namespace maps symbols to vars and has
  aliases. `ns` is `in-ns` plus `require`. `require` loads a library once:
  embedded in the binary, or found on `NORN_PATH`.
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
  `norn-ui` and `norn-studio` add their own. All of it has to keep
  compiling, which is why `defmacro` sits on top of the hygienic expander
  (design §8.2) rather than beside it.

## Parts

| Part | Work |
|---|---|
| 8.1 | **Syntax objects:** a datum with a set of scopes and a source location. Identifiers, `syntax`/`quasisyntax`, `datum->syntax`, `syntax->datum`, `syntax-e`, `free-identifier=?` and `bound-identifier=?`. The reader gives locations; the comment-keeping reader already tracks them. |
| 8.2 | **The expander, in Norn:** Racket's sets-of-scopes. It expands to the kernel forms (runtime.md §3.5), with each reference resolved to a local or a var, and the existing compiler takes its output. `defmacro` and syntax-quote work on top of it: a datum macro gets the use-site and introduction scopes, so the stdlib's 83 macros keep working. The REPL top level stays permissive (design §8.2). |
| 8.3 | **Pattern macros:** `syntax-case` and `syntax-rules`, then a core of `syntax-parse`: patterns, syntax classes, `~optional`, `~or` and `~seq`, and error messages that name what was expected and where. |
| 8.4 | **Modules:** one file is one module, with `ns` as its header. A module has exports (its public vars; `defn-` is private and enforced), its requires are resolved when it's expanded, and a registry instantiates each module once. Redefinition at the REPL stays live: modules are open until sealed. |
| 8.5 | **Phases:** `(:require-for-syntax …)` in `ns` and `begin-for-syntax`, so a macro's helpers are loaded for compile time explicitly, and each phase instantiates its own copy of a module. Compiling a module gives the same result whatever else is loaded. |
| 8.6 | **Compiled modules on disk:** each module's expanded, compiled form is cached, keyed by a hash of its source and its dependencies'. A cached module loads without expanding again, and a changed dependency recompiles what depends on it. |
| 8.7 | **The bootstrap and the stdlib:** the seed compiler (Rust, non-hygienic) still loads `core` and the compiler. The expander loads after them, and everything is compiled again through it into the base image (runtime.md §9). The stdlib, `norn-ui` and `norn-studio` move to modules. Hygiene tests cover the classic cases: `or` with a user's `t`, `swap!`-style temporaries, and a shadowed `if`. |
| 8.8 | **Lint rules that need modules:** unused requires, missing requires, using a private var from another module, and phase errors (a macro calling a function that isn't required for syntax). |
| 8.9 | **The macro stepper:** expand a form one step at a time, showing each step's scopes and renames. It's available in the REPL (`macro-steps`), from the CLI (`norn expand --steps`), and as an editor op (`:expand-steps`) for Norn Studio. |
| 8.10 | **Packages:** a project file, Git and path dependencies, a lock file, fetching into a cache, resolution, and `norn build` across dependencies. `norn-ui` and `norn-studio` get project files, and `NORN_PATH` becomes a fallback. |
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
   form.
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
7. **The project file is `norn.edn`** (`:name`, `:version`, `:paths`,
   `:deps`), with `norn.lock` recording each Git dependency's commit. A Git
   dependency names a tag or a commit, a path dependency a folder. Two
   versions of one package in a build are an error showing both chains,
   unless an override in the project file settles it.
8. **Packages stay in M8**, last. They split into their own milestone only
   if M8 runs long.

## Status

**8.1, syntax objects (2026-11-14): done.**

- **`norn.syntax`**, an embedded library:
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
- **`#lang` is gone.** The reader refuses it, and `core.norn` and
  `compiler.norn` no longer start with it.
- **Tests:** `syntax_test.norn`, 55 checks, at tier 0 and tier 1. Seven
  library files, `core.norn` among them, read the same as data and as
  syntax.
- **Speed:** `read-syntax` takes 34 ms on `core.norn` (110,000
  characters), where the plain reader takes 5 ms. Most of it is building
  the syntax objects in Norn. Compiled modules are cached (8.6), so this is
  paid only when a module changes; building them in Rust is the fallback
  if it matters.
- **Scopes are added eagerly,** walking the whole tree. Racket propagates
  them lazily. If the expander's speed needs it, laziness can go in behind
  the same functions.

**8.2, the expander (2026-11-14): done, and the stdlib passes through it
unchanged.**

`lib/compiler/expander.norn` loads at boot after the compiler and becomes
`eval`. Every form goes through it: `NORN_EXPANDER=off` keeps the old path,
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
- **`defsyntax` macros** (in `norn.syntax`) get syntax. A fresh scope is
  added to their input and flipped on their output. `quote-syntax` is a new
  kernel form, and `free-identifier=?` asks the expansion running now.
- **A macro's own errors pass through as they are,** keeping their condition
  and restarts. The expander's own errors say where:
  `e.norn:1:8: Bad binding form in let*: 1`.

Tests: `expander_test.norn` has 24 checks at both tiers, covering the
classic hygiene cases both ways, macros from another namespace,
`free-identifier=?`, and anaphoric `defmacro`. All 1,180 checks of the
suites pass through the expander, as do `norn-ui`'s and `norn-studio`'s. The
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
| Norn Studio's code | 0.44 s | 0.77 s |
| Start-up from source (boot compiles the expander) | 40 ms | 60 ms |
| Start-up from an image | 40 ms | 40 ms |

The compiled-module cache (8.6) takes the loading cost away for anything
unchanged.

**For 8.4:** private vars aren't enforced yet. When they are, a macro's
template must still reach its own namespace's private functions, as
`expander_test` checks, while other code can't.
