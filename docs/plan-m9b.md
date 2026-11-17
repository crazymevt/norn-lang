# Plan: M9b — after sealing

Drafted 2026-11-16; its proposals were accepted the same day (see
Decisions, at the end). **Done 2026-11-17,** with Zed's extension moved
to todo.md (the user's choice: decision 4 and the done-check's "or Zed"
are met by VS Code). M9b is the part of M9 that waited for
sealing (plan-m9.md): what needs a sealed module, and tooling that doesn't
shape core. runtime.md §12's row lists tree shaking, a sampling profiler,
source-drift tracking, `norn lsp`, `norn new` and coverage.

**Already done, in M8:** `norn api-diff`, the lint rule for redefining
sealed vars, and `norn new` (which grew `--lib` from the ledger). So M9b is
five parts, and one they share.

## How things are now

- **Images** (design §7, runtime.md §7.5): `save-image` and `norn build`
  write everything reachable from the namespaces. Studio's image is 65,289
  objects, 3.7 MB; a `norn package --cli` archive carries its image beside
  the `norn` binary. `:shake` is in design §7.1's example but not built.
- **Sealing:** the standard library is sealed as it loads, and `norn build`
  seals the modules it loaded, so a release image is sealed throughout:
  nothing in it will be redefined, which is what lets shaking drop code.
- **Stacks:** every frame, tier 0 or tier 1, is on the fiber's heap stack
  (jit.rs: tier 1 keeps the frame layout), so the debugger's `backtrace`
  walks both. A sampler can walk them the same way.
- **Bytecode** has no counters. M7 deferred coverage to "M9's profiler".
- **The editor protocol** (design §5.1) has `:doc`, `:definition`,
  `:complete`, `:lint`, `:format`, `:macroexpand`, `:inspect` and the
  debugger's ops. It has no xref (who-calls) and no profiling, both of
  which §5.1 lists.
- **Source drift** (design §5.1, §7.4): nothing records what was evaluated
  but never saved.
- **Editors:** Studio speaks the editor protocol. Other editors have nothing.

## Parts

| # | Part |
| --- | --- |
| 0 | **References (xref):** for every compiled function, the vars it refers to, read from its bytecode's constants (and tier 1's, which shares them). `(callers var)` and `(callees var)` at the REPL, `:who-calls` and `:calls` editor ops, and the reachability walk 1 needs. Built once, kept current as definitions change (a redefinition updates its entries). |
| 1 | **Tree shaking:** `(save-image f :shake true)` and `norn build --shake` (and `norn package`'s, from a `:shake true` in `:package`): the image keeps what's reachable from the entry (and the REPL's needs when there's no entry), and drops the rest of the sealed modules: unreferenced vars, their functions and constants, generic functions' methods for classes nothing reachable makes, and the compiler and expander when nothing reachable evaluates code. A report says what went and how much it saved. Works only on sealed modules; an unsealed one is kept whole. |
| 2 | **A sampling profiler:** every N µs (1000 by default), each carrier, at its next safepoint, records the stack of the fiber it's running into a buffer (no sampling thread touches a stack it doesn't own). `norn.prof`: `(profile body…)` and `profile-start!`/`profile-stop!`, giving a report (the hottest functions, self and total, and a call tree) and folded stacks for flame-graph tools (speedscope, inferno). A `:profile` editor op, so Studio can show it. Both tiers. |
| 3 | **Coverage:** a mode in which the compiler adds a counter per source line it compiles (a `COUNT` instruction, at tier 0: coverage runs at tier 0). `norn test --coverage` reports each file's lines hit and missed, and writes lcov (`--lcov FILE`) for editors and CI. |
| 4 | **Source-drift tracking:** each definition evaluated from the REPL or an editor is recorded with its source text and the file it says it's from. `(unsaved-definitions)` lists the ones whose file doesn't hold that text now (never saved, or saved differently), and an `:unsaved` editor op gives the same, so Studio can mark them. |
| 5 | **`norn lsp`:** a Language Server Protocol adapter on standard input and output, in Norn (`norn.lsp`), over a project's `norn` as the editor protocol: diagnostics (lint), hover (`:doc`), completion, go to definition, references (0's `:who-calls`), document symbols (the outline), formatting, and evaluating a form (a command). For VS Code, Zed, Helix and Neovim, with a page on setting each up. |

**Order.** 0 first: 1 and 5 build on it, and it's small. Then 1 (it's the
part that needed sealing, and it changes what `norn package` ships), 2 and
3 (the measuring tools M10 will want), 4, and 5 last (it uses 0, 2's op
shape and 4's op).

**Done-check (proposed):**

- `norn package` with `:shake true` makes Studio's and the five
  `lang-tests` programs' archives smaller, and every one still passes its
  tests run from the shaken image.
- `(profile …)` names the hot function in loglens's report (its per-entry
  parse), at both tiers, and its folded stacks load in speedscope.
- `norn test --coverage` on `tests/norn` reports per-file line coverage,
  and the lcov file loads in an editor's coverage view.
- `(unsaved-definitions)` lists a function redefined at the REPL and not
  saved, and stops listing it once it's saved.
- `norn lsp` serves VS Code (or Zed) a Norn project: diagnostics, hover,
  completion, go to definition and references work.
- Tier 0 and tier 1 agree on every suite, and `api_diff_test` passes (M9b
  adds to the interface; it breaks nothing).

## Decisions (the user, 2026-11-16)

1. **`:shake` keeps what's reachable, plus `:keep`,** and warns, naming the
   call, when reachable code calls `eval`, `resolve`, `requiring-resolve`,
   `load-string` or `require` with a computed name. It doesn't guess from
   strings.
2. **The profiler reports as text and folded stacks,** 1 ms by default
   (`:interval-us`). A Studio panel comes with Studio's own work.
3. **Coverage runs at tier 0;** tier 1 counters can come with M10.
4. **`norn lsp` is checked with VS Code and Zed;** Helix and Neovim are
   documented. (2026-11-17: checked in VS Code; Zed's extension moved to
   todo.md, the user's choice.)
5. **Xref** (not asked; the draft's proposal stands): function bodies and
   macros' expansions as compiled, and a generic function counts its
   methods.

## Status

- **0, references: done.** `norn.xref` (`callees`, `callers`,
  `references`, `reachable`), read from compiled code each time rather
  than kept as an index: the whole table takes about 3 ms for the ~800
  loaded vars that refer to something, so nothing needs updating when a
  definition changes. A `%fn-vars` primitive walks a function's
  prototypes' constants (nested ones included), and `%fn-captured` a
  closure's captured values; data counts through the functions in it, a
  generic function through its methods, and a contract's checking wrapper
  as the function it checks. The `:who-calls` and `:calls` editor ops,
  `xref_test`, chapter 12 and the reference. Found on the way: `#'lib/f`
  of a checked function gave its wrapper var (fixed: `var` never wraps).
- **1, tree shaking: done.** `norn.shake` plans it, `save-image :shake
  :keep` and `norn build --shake --keep` do it, and `norn package` takes
  `:shake` and `:keep` in `:package`. What's dropped is names: the saver
  leaves dropped vars and namespaces out of the namespace tables (and
  aliases to them), and everything only those names kept goes with them,
  as the save follows references. Generic functions keep only the methods
  for classes something kept makes (a fixpoint: a method counts only once
  its classes are live); the saver swaps each one's methods for a
  stand-in's while every fiber is paused, so the running process never
  sees it. Symbols and keywords nothing saved refers to are left out (a
  first walk without the tables): that was most of what modules loaded
  but unreached left behind. With nothing that evaluates, the compiler
  goes and names are dropped by var; with something that does, the
  language is kept and modules are kept or dropped whole (a module loaded
  later may use any name). Lookups by computed name are warned about.
  To make that possible, nothing the runtime does on its own path evaluates
  any more: an image's start calls `%image-restored`, `%has-entry?` and
  `%run-entry-with-args` directly (a `RootTask::Call`, no compiling);
  `norn image info`/`diff` ask the image for `%image-summary` the same way
  (so they work on shaken images, and no longer list `norn.image` itself);
  effective methods are closures rather than eval'd code (as fast on
  cache hits, about 30% faster on misses; `eval` only for a combination
  whose operator is a macro); the tag readers and `definition`'s docs
  load libraries through `%require-lazily`, which finds `require` by
  name, and the shake loads those libraries beforehand. A missing root
  function now ends its task with an error instead of panicking a
  carrier. Results: the five `lang-tests` programs' images are 63–70%
  smaller and behave identically (each one's commands run against both);
  Studio's is 34% smaller (5.9 → 3.9 MB) and starts, with one warning
  (its fallback signature help resolves any `norn.core` name; keeping
  `norn.core` costs most of the saving: 6%). `crates/norn-vm/tests/
  shake.rs` checks every name the Rust side looks up is kept;
  `images.rs` builds and runs shaken images.
- **2, profiler: done.** `norn.prof`: `profile` (prints the report),
  `sample`, `start!`/`stop!` (not `profile-start!`: the namespace says
  it), `hottest`, `report` (self and total, then the call tree, recursion
  folded) and `folded` (speedscope, inferno, flamegraph.pl); the `:profile`
  editor op. A timer thread bumps an epoch and the attention word; each
  carrier samples its own stack on its next poll's slow path (tier 1
  bails to it too, so both tiers). A sample at a function's entry counts
  for its caller: native calls have no frames, so otherwise their time
  went to whatever function was entered next (loglens showed `reset!` at
  15%, really `norn.io/read-line`'s reading). Anonymous functions' code
  is now named for where it is (`user/f/fn`), in both compilers: in
  backtraces and the debugger too (closures still print as `#<fn fn>`);
  `backtrace`'s `:internal?` covers `norn.core`'s anonymous code as well
  (Studio picks the first frame that isn't internal, and the REPL's banner
  hides them, as it hid them when they were bare `fn`); the book's
  examples are refreshed. Done-check: profiling loglens's
  report names `loglens.logs/parse-request`, the per-entry parse, at 40%
  (tier 0) and 37% (tier 1); its folded stacks are the collapsed format
  speedscope reads (not opened in speedscope here). For M10, the same
  profile: contract wrappers (`%wrap-for/fn`) are about 10% self,
  `norn.regex/details` 7–10%.
- **3, coverage: done,** a little differently from the plan: no `COUNT`
  instruction. With coverage on, the expander puts `(norn.core/%cover!
  id)` before the first form starting on each line (inside the form
  around it; a name or constant alone on a line too, in an expression's
  place), id a counter in Rust made as the line is compiled. So nothing
  changed in the bytecode, the verifier or the JIT, and it runs at either
  tier (the decision said tier 0 because a new instruction would have
  needed tier 1's support). A `def`'s value and a `letrec*` init stay
  bare `fn*` forms (contracts and letrec* need them). Counted: files
  under the current folder but not `_test.norn` files, and in a Norn
  checkout the standard library (`<norn.json>` is `lib/norn/json.norn`);
  those skip the module cache. `norn test --coverage` turns it on first
  thing (in the CLI, before the project loads); `--lcov FILE`; `norn.cover`
  (`start!`, `counts`, `summary`, `report`, `lcov`). Found on the way: a
  `defmacro`'s result gave its constants the call's place (an interned
  `:else` can't be traced by identity), so a branch like `:else "large"`
  had no line of its own; a constant written once among the arguments now
  keeps its place (`syn/%atom-place`, and the same in the precise path).
  Done-check: `norn test --coverage` on `tests/norn` reports the standard
  library (68 modules, 6,723 of 7,485 lines, 89.8%), every check passing;
  its lcov file is consistent (LF/LH match its DA lines). Not opened in
  an editor's coverage view here (no lcov tools on this machine).
- **4, source drift: done.** The REPL and the `:eval` op bind
  `*evaluated-from*` to their reader's name; a definition noted from it is
  kept in `%evaluated` (name, file, line, text, form) until a file defines
  it again (`load-file`, `require :reload`, `norn dev`). A definition
  retyped at the REPL is compared with the file it came from. `(unsaved-
  definitions)` reads each file now: saved when it has a definition of
  the name that reads the same (`=` on the forms, so layout doesn't
  matter), or, failing that (an auto-gensym, a regex), the evaluated text
  with whitespace squeezed; else `:changed`, `:not-in-file`, `:no-file`
  or `:file-missing`. The `:unsaved` op gives the same. `defmethod`s
  aren't tracked (they name no var). Found on the way: editor requests
  were known by their id alone, though each editor numbers its own, so
  with two editors connected `:interrupt` could stop the other's
  evaluation and output could reach the wrong one. The server now numbers
  its connections and adds `:conn`; requests are `[conn id]`, output
  events carry `:conn`, and a connection drops others' output.
- **5, `norn lsp`: done, checked in VS Code (as Antigravity IDE,
  2026-11-17: `editors/vscode`, docs/plan-vscode.md); Zed's extension
  moved to todo.md.**
  `norn.lsp` runs in the project's own Norn (`norn lsp`) and answers
  through the editor protocol's ops in-process (`%serve-request`):
  diagnostics from `norn lint` on every change, hover (`:doc`),
  completion (`:complete`), definition (`:definition`), references
  (`:who-calls`, pointed at where each caller names it), document symbols
  (read with `norn.syntax`), formatting (`norn fmt`, one edit), and the
  `norn.eval` command (the top-level form at a position, through `:eval`
  with its file and line, so drift tracking sees it). A file's module is
  required when opened and again when saved. Positions are UTF-16, as
  LSP's default. New primitives: `%stdin-read` (exact bytes, the same
  buffer as `%stdin-line`) and `%write-stdout`, and `*out*` bound to
  `:stderr` (what code prints mustn't reach the protocol). Checked: a
  JSON-RPC conversation in `crates/norn/tests/lsp.rs`, and Emacs's eglot
  (Emacs 31 is on this machine; the test runs it when Emacs 29+ is
  there): connect, hover, definition, references, diagnostics,
  completion, formatting, outline, shutdown. VS Code and Zed aren't
  installed here, and both need an extension to start a language server;
  `docs/editors.md` sets up Emacs (tested), Neovim and Helix (from their
  documentation), and says what the VS Code and Zed extensions need.
- **Interfaces recorded:** `norn api-diff --record` for the five new
  modules (`norn.xref`, `norn.shake`, `norn.prof`, `norn.cover`,
  `norn.lsp`) and M9b's additions to `norn.core` (`unsaved-definitions`,
  `*evaluated-from*`, `save-image`'s `:shake` and `:keep`) and
  `norn.image` (`info-text` of a summary): 70 modules in `api/`.
