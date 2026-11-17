# Norn documentation

Norn is a Lisp with persistent collections, hygienic macros, multiple dispatch,
conditions and restarts, and lightweight fibers. These pages describe the
language and tools in this checkout.

| Start here | What you'll find |
| --- | --- |
| [Getting started](getting-started.md) | Build Norn, use the REPL, and write a module. |
| [The Norn Book](book/README.md) | Learn the language and its libraries, chapter by chapter, from the REPL to a packaged app. |
| [Examples](examples/README.md) | Run complete programs for data transformation, generic functions, recovery, and concurrency. |
| [Language reference](reference/README.md) | Browse syntax and behavior by topic. |
| [Symbol index](reference/symbols.md) | Look up a core name or special form. |
| [Standard library](reference/libraries.md) | Find a namespace and look up its public API. |
| [Command-line tools](tooling.md) | Run scripts, format, lint, test, manage dependencies, and build images. |
| [Editors](editors.md) | Set up an editor with `norn lsp`: Emacs, Neovim, Helix, and what VS Code and Zed need. |

## Building Norn

For how Norn is made, rather than how to use it:

| Folder | What's in it |
| --- | --- |
| [internals/](internals/) | The [language design](internals/design.md), the [runtime architecture](internals/runtime.md) (with the milestones, §12), the [editor protocol](internals/design.md#51-the-repl-decided), the [UI protocol](internals/ui-protocol.md), and the [performance survey](internals/perf-survey.md) taken before sealing. |
| [plans/](plans/) | Each milestone's plan, as drafted, decided and done: [M6 and M7](plans/plan-m6-m7.md), [M7.5](plans/plan-m7.5.md), [M7.6](plans/plan-m7.6.md), [M8](plans/plan-m8.md), [M9](plans/plan-m9.md), [M9b](plans/plan-m9b.md), [M10](plans/plan-m10.md), and [the VS Code extension](plans/plan-vscode.md). [todo.md](plans/todo.md) holds what's still open. |
