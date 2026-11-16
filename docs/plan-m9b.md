# Plan: M9b — after sealing

Drafted 2026-11-16; its proposals were accepted the same day (see
Decisions, at the end). M9b is the part of M9 that waited for
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
   documented.
5. **Xref** (not asked; the draft's proposal stands): function bodies and
   macros' expansions as compiled, and a generic function counts its
   methods.

## Status

Not started.
