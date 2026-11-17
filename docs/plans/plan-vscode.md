# Plan: a VS Code extension for Skein

Drafted 2026-11-17, for M9b's open item (todo.md, plan-m9b.md decision
4): `skein lsp` checked in VS Code. It's checked in Antigravity IDE, a VS
Code fork (1.107, the user's choice), which runs VS Code extensions and
installs them from Open VSX or a `.vsix` file. Zed's extension is a plan
of its own.

## Status

- **Parts 0–3, 5 and 6: done (2026-11-17).** `editors/vscode`:
  `extension.js` (the language client, one per project, its commands),
  `repl.js` (part 5), `debug.js` (part 6, an inline debug adapter),
  `edn.js` (the editor protocol's data), the grammar and the language
  configuration. Node 26 from Homebrew. `npm test` (test/run.js) runs
  test/suite.js in Antigravity with a fresh profile, and checks, through
  VS Code's own commands:
  - **The language server:** hover, definition, references, a
    diagnostic, completion, formatting, the outline, evaluation, and a
    second project with a server of its own.
  - **The REPL:** a form's value, and a definition.
  - **The debugger:** an error stops the session at
    `shapes.skn:7: odd: 3`, the call stack has `geo.shapes/half` at its
    definition's line with `x=3` among its locals, evaluating `(* x 2)`
    in the frame gives 6, and redefining `half` then restarting the
    frame gives the call's value. Stop aborts a second failure.

  It takes 5 s and passes three runs in three. `crates/skein/tests/lsp.rs`
  runs it with `SKEIN_VSCODE_TEST=1` (it opens a window). The
  editor protocol's `:backtrace` gives each frame's locals as `:vars`
  too, for the Variables view. `npm run package` makes `skein.vsix` (718
  KB unbundled: esbuild would shrink it, later).
- **Found on the way:** with two projects open, the language client would
  register the server's `skein.eval` command twice and fail to start the
  second, so the extension leaves the server's commands unregistered and
  calls them itself. `vscode-languageclient` 11 needs a log output
  channel.
- **Part 4 (the hand check and the documents): documents done**
  (editors.md's VS Code section). The hand check in Antigravity, with the
  `.vsix` installed, is the user's to try.

## How things are now

- **`skein lsp`** (`lib/skein/lsp.skn`, editors.md) serves a project over
  standard input and output: diagnostics (lint, on every change), hover,
  completion (triggered by `/` too), definition, references, document
  symbols, formatting (one whole-file edit), and the `skein.eval` command,
  with arguments `[uri position]`, which evaluates the top-level form
  there. It answers with the value and shows it with `window/showMessage`.
  Positions are UTF-16, as VS Code's are. It must run in the project
  folder (the one with `skein.edn`) to see the project's paths and
  packages. What the project's code prints goes to standard error.
- **Checked** with a JSON-RPC conversation (`crates/skein/tests/lsp.rs`)
  and Emacs's eglot (`tests/lsp/eglot-check.el`). VS Code starts a
  language server only through an extension, and none exists.
- **On this machine:** Antigravity IDE 2.7.0 (`/Applications/Antigravity
  IDE.app`, command line `…/Contents/Resources/app/bin/antigravity-ide`,
  which takes `--install-extension`, `--extensionDevelopmentPath` and
  `--extensionTestsPath`). It ships VS Code's built-in Clojure grammar.
  There's no Node or npm, which building an extension needs:
  `vscode-languageclient` comes from npm, and `@vscode/vsce` makes the
  `.vsix`. At run time the editor's own extension host runs the
  extension, with no Node needed.

## Parts

| # | Part |
| --- | --- |
| 0 | **The project:** `editors/vscode/` in skein-lang (versioned with `skein lsp`, beside a later `editors/zed/`). `package.json` (publisher `skein`, name `skein`, the engine `^1.90.0`), plain JavaScript (`extension.js`, CommonJS: no build step but `npm install`), `vscode-languageclient` its one dependency, `.vscodeignore`, a README (the Marketplace page) and the licence. `npm run package` makes `skein-<version>.vsix` with `vsce`; its version follows skein's. |
| 1 | **The language:** `.skn` files are language `skein`. A language configuration covers `;` line comments, brackets `() [] {}`, auto-closing pairs (`"` too), Lisp words (`-`, `?`, `!`, `*`, `/`, `.`, `<`, `>` and `=` inside names, so double-click and completion take the whole name), and indentation that follows open brackets. A TextMate grammar `source.skn`, started from VS Code's Clojure grammar (MIT, credited), with Skein's own defining forms highlighted as definitions (`defn`, `defn-`, `def`, `defmacro`, `defclass`, `defgeneric`, `defmethod`, `defcontract`, `deftest`, `defproperty`, `ns`), keywords, characters, `#"regex"`, `^Type` hints and `#_` comments. |
| 2 | **The client:** `vscode-languageclient`'s `LanguageClient`, with `{command: skein.path, args: ["lsp"], options: {cwd}}`. The cwd is the nearest folder with `skein.edn` above the file opened, or the workspace folder. One client per project root, so a multi-root workspace or a monorepo (skein-libs) gets one each, made the first time one of its files opens. Settings: `skein.path` ("skein"), `skein.trace.server` (off/messages/verbose). An output channel "Skein" holds the server's standard error (what code prints, `log!`). If `skein` can't be started, one error message names `skein.path`. Commands: **Skein: Evaluate Top-Level Form** (`skein.evalTopLevel`, Cmd/Ctrl+Enter in Skein files) sends `workspace/executeCommand` `skein.eval` with the active document's URI and the cursor (the client also registers `skein.eval` itself, which forwards arguments as given, so the palette's needs its own name); **Skein: Restart Language Server**; **Skein: Show Output**. Formatting is VS Code's own (Format Document, or format on save). |
| 3 | **Checked in Antigravity, automatically:** a test the editor runs (`--extensionDevelopmentPath` and `--extensionTestsPath`, with a fresh `--user-data-dir` and `--extensions-dir`, so the user's profile isn't touched), on a fixture project (the eglot check's), through VS Code's own commands: a diagnostic for an unresolved name appears and goes when it's fixed (`languages.getDiagnostics`); hover shows a docstring (`vscode.executeHoverProvider`); completion offers a namespace's names after `/`; definition goes into another file of the project; references lists a caller; the outline lists the file's definitions; formatting gives the edit `skein fmt` would; `skein.evalTopLevel` returns a form's value. `editors/vscode/test/run.sh` runs it, given the editor's command (Antigravity's by default, VS Code's `code` if that's what there is). A Rust test beside `lsp.rs` runs it when an editor and the built extension are found, as the eglot check runs when Emacs is there. |
| 4 | **Checked by hand, and documented:** the `.vsix` installed into Antigravity (`--install-extension`), opened on lang-tests/ledger and skein-libs (several roots), each feature tried once, and a screenshot of each for the record. editors.md's VS Code section says how to install it and what it does; plan-m9b.md and todo.md record the check. |

**Order.** 0 to 2 are one piece of work (a small extension). 3 checks it,
and 4 is the hand check and the documents. Then 5 and 6 (below), checked
the same way.

## Done-check (proposed)

- In Antigravity, a Skein project gets highlighting, diagnostics as you
  type, hover, completion, definition, references, the outline,
  formatting and evaluation, each tried by hand on lang-tests/ledger.
- `editors/vscode/test/run.sh` passes in Antigravity, and the Rust test
  runs it when it can.
- `npm run package` makes a `.vsix` that installs with `antigravity-ide
  --install-extension`.
- With the REPL started, a form evaluated from a file shows its value
  inline, and an error stops in VS Code's debugger. There its frames,
  their locals, evaluation in a frame, Restart Frame and the restarts
  work.

## A REPL and the debugger (parts 5 and 6)

Parts 0–4 give what `skein lsp` serves, plus evaluating a top-level form.
That form runs in the language server's own Skein, and its value pops up as
a message. They give no REPL and no debugger: an error comes back as text.
Both come from the other protocol, `skein --server` (Chapter 12): an
evaluation streams its output, and a failure arrives as a `:debug` event
the editor can act on (`:eval-in-frame`, `:restart-frame`, the
restarts, abort).

| # | Part |
| --- | --- |
| 5 | **A REPL, "jack-in":** **Skein: Start REPL** opens an integrated terminal running `skein --server <free port>` in the project, the terminal REPL itself (with its own `debug[N]>` prompt), and the extension connects to its port. **Evaluate Top-Level Form** and **Evaluate Selection** then go to that process when it's running. Values show inline beside the form (a decoration), output goes to the terminal, and evaluating a form from a file passes `:file` and `:line`, so `unsaved-definitions` sees it. The language server stays a process of its own: it does the analysis, and the REPL holds the program's state. |
| 6 | **The debugger, through VS Code's Debug Adapter Protocol:** an adapter inside the extension (no separate process) turns a `:debug` event into a stop ("exception", with the condition's message and class). Frames come from `backtrace`, locals become variables (their printed text, with the inspector for drilling in), the Debug Console evaluates in the selected frame (`:eval-in-frame`), and Restart Frame is `:restart-frame` (DAP has it). Continue offers the restarts as a pick list (`use-value` asking for the value, `retry`, `abort`), and Stop aborts. **What it can't do:** Skein's debugger stops on conditions, not on lines, so there are no line breakpoints and no stepping. And frames carry no source line yet, so selecting one opens its function's definition, not the line it stopped at; frames would need a pc → line table for that (a runtime addition). |

Parts 5 and 6 need parts 0–2 first, and are checked the same way (an
Antigravity test, then a hand check).

## Decisions (the user, 2026-11-17)

1. **Node is installed with Homebrew** for building and testing.
2. **`editors/vscode/` in skein-lang.**
3. **Plain JavaScript.**
4. **A local `.vsix` first;** Open VSX later.
5. **Parts 5 and 6 are in this plan:** a REPL and the debugger.

## Still to decide

1. **Does Antigravity count for M9b's "checked in VS Code"?** (below)

Proposals the user has answered (kept for the record):

1. **Node for building:** install it with Homebrew (`brew install
   node`). It's needed only to build and test the extension, not to use
   it.
2. **Where it lives:** `editors/vscode/` in skein-lang (proposed:
   versioned with `skein lsp`, one repository to check out), or a
   repository of its own, as Marketplace extensions usually are.
3. **Plain JavaScript or TypeScript:** JavaScript (proposed: about 150
   lines, no compile step), or TypeScript with esbuild, as Microsoft's
   samples are.
4. **Publishing:** a local `.vsix` for now (proposed). Open VSX, where
   Antigravity installs from, needs the user's account and an access
   token. The VS Code Marketplace needs a publisher of its own (Azure
   DevOps).
5. **Does Antigravity count for M9b's "checked in VS Code"?** It's VS Code
   1.107 underneath and runs the same extension. Proposed: yes, recorded
   as checked in Antigravity. A later check in VS Code itself would run
   the same script with `code`.
