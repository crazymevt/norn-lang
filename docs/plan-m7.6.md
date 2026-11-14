# Plan: M7.6 — the Norn editor

Drafted 2026-11-13; the review questions are settled (below). The decisions behind it are in design
§9.1 (the editor is essentially Xeon Studio rebuilt in Norn; Norn-only
language support first; three panels phased) and the M7.6 row of
runtime.md §11. This plan says how to build it and in what order.

**What M7.6 is for.** It's the first real Norn program, written while the
stdlib can still change (it's sealed at M8). The done-check is that Norn's
own development happens in the editor. Everything the editor needed from
Norn is fixed, or scheduled, before M8.

**Where the work lives:**
- **This repository** gains what the editor needs from Norn:
  - editor-protocol ops;
  - file watching;
  - `norn.markdown`;
  - fixes found along the way.

  It ends with an M7.6 commit like the others.
- **`norn-ui`** goes to 0.2 with the widgets the editor needs (below).
- **The editor, Norn Studio**, in a new repository,
  `~/projects/norn/norn-studio` (git, no remote, MIT). It's Norn code run by
  the `norn-ui` binary,
  pinning a `norn-ui` version. Until M8's packages exist, it loads Norn
  and `norn-ui` from local checkouts by path, as `norn-ui` does now.

**The reference.** Xeon Studio (`~/projects/clojure/xeon-studio`, about 19k
lines of Clojure on JavaFX). Its feature list and keys are the target;
its code is a guide, not a source to translate line by line.

## Architecture

- **Two processes.** The editor is a `norn-ui` process. It starts the
  project's own `norn --server 0` and talks to it over the editor
  protocol, for the REPL, evaluation, the debugger, the inspector,
  completion, and `norn fmt` and `norn lint`. Code under development can
  crash or hang without taking the editor with it.
- **The editor's state is data.** It's in atoms: the open documents
  (persistent ropes), layout, settings and REPL sessions. Views are
  functions of it, as in the gallery. Commands are named functions in one
  registry, which feeds menus, key bindings and a command palette.
- **Norn knowledge comes from Norn.** The editor gets:
  - highlighting and structure from the comment-keeping reader;
  - indentation from `norn fmt`'s rules, including macros' `{:indent n}`
    hints;
  - warnings from `norn lint`;
  - completion, signatures and definitions from the running `norn`;
  - its own lexer, only for highlighting while a form doesn't parse yet.
- **Budgets, as Xeon Studio keeps:**
  - no timers while idle, and no animation that runs forever;
  - a keystroke repaints a line;
  - streaming output (REPL, chat) is coalesced to about 12 redraws a
    second;
  - hidden tabs don't render.

## Part 1 — in this repository: what the editor needs from Norn

| Part | Work |
|---|---|
| 1.1 | **Editor-protocol ops** (`%serve-request`), each with a test in `tests/server.rs`. The ops: `:eval` with `:file` and `:line` (so errors and definitions point at the source); `:load-file`; `:interrupt` (a running evaluation); `:doc` (a var's docstring and arglists, for signature hints); `:definition` (file and line); `:macroexpand`; `:inspect` (structured, navigable values); `:ns-vars` (for outlines of loaded code); `:test` (run `norn.test`, results as data); and pushed `:out`/`:err` events, so printed output streams to the REPL as it happens. |
| 1.2 | **File watching:** already there. `norn.fs/watch` (M6) gives a channel of create, modify and remove events, recursively if asked, and is tested in `lib_test`. Nothing to build. |
| 1.3 | **`norn.markdown`:** CommonMark (headings, emphasis, links, images, lists, block quotes, code blocks, tables) parsed to Norn data, for the Markdown preview and the chats. In the stdlib, since sealing is still ahead. |
| 1.4 | **What the editor turns up**, fixed as found and listed in the status: missing stdlib functions, slow paths (Norn's `sort-by` took about a second for 10,000 maps in a debug build), and protocol gaps. |

## Part 2 — `norn-ui` 0.2: what the editor needs from the UI (done, 2026-11-13)

Built as planned. Along the way, Norn's core gained about 60 of
Clojure's functions and macros it lacked (`case`, `cond->`, `some->`,
`subvec`, `merge-with`, `boolean`, `sequential?` and more). Text nodes
now report their narrowest width correctly, so text wraps inside rows.
`delay`, `future`, promises and `ex-info` are left for later: they need
a design against Norn's fibers and conditions.

| Part | Work |
|---|---|
| 2.1 | **Rich text flow:** wrapped paragraphs of styled spans, links, inline code and embedded blocks, for the Markdown preview and the chats; selectable and copyable. |
| 2.2 | **Icons:** SVG icons drawn on canvases (file types, activity bar, toolbar), with a small bundled set. |
| 2.3 | **Shell widgets:** an activity bar, a collapsible sidebar, a top bar with chips, a status bar, and a command palette (a popup list with fuzzy filtering). |
| 2.4 | **The editor surface, for code:** brackets coloured by depth, with the matching bracket emphasised; completion and signature popups anchored at the caret; find and replace; undo and redo in the model (grouped edits); inline results; and, for large files, incremental highlighting. |
| 2.5 | **Persistence:** window size and position, dock layout, and sidebar state saved and restored. |

## Part 3 — the editor

**Status (2026-11-14): 3.1 to 3.7 done.**
- **3.1, the shell:** built as planned, with a basic Explorer tree (3.6
  adds icons for Git status and the rest). The tools in the docks are
  placeholders until their parts.
- **3.2, documents:** built as planned. Each folder's open documents
  come back with it. Go to definition reads the project for top-level
  `def` forms until 3.4 asks the running `norn` (`:definition`).
- **3.3, Norn editing:** built as planned. The project's `norn --server`
  starts with the folder (the top-bar chip: start, stop, its output), a
  step ahead of 3.4, since lint, completion and signatures come from it.
  Enter and Tab indent by `norn.fmt`'s own rules, run in the editor:
  format the top-level form up to the caret with a probe line, and see
  where the probe lands. ⇧⌘I formats with the project's `norn` (it knows
  the project's macros' `{:indent n}`), or `norn.fmt` in the editor when
  it doesn't run.
- **3.4, the REPL and evaluation:** built as planned. A failed evaluation
  waits in the project's `norn`'s debugger: the console shows the error
  with buttons for its restarts (3.5 adds backtraces, locals and
  evaluating in a frame), and an error at a document's line goes to
  Problems. ⌘⏎ sets up the document's namespace first (its `ns` form)
  if this `norn` hasn't seen it.
- **3.5, the debugger, inspector and tests:** built as planned. The
  Debugger comes forward when a fiber stops, at the first frame of a
  named function. A frame's locals arrive printed (`:backtrace`), so
  they're shown as text, split into names and values. F12 now asks the
  running `norn` first (`:definition`), then reads the project.
- **3.6, the Explorer, Git, Project and Outline:** built as planned, by
  running `git` (always with `--no-optional-locks`: `git status` would
  otherwise rewrite the index, and a commit made in a terminal meanwhile
  would find `index.lock` taken). A diff is a tab of its own, side by
  side, read from `git diff` with all the context. Commits and checkouts
  made outside show, as the folder watch passes `.git/index` and
  `.git/HEAD` through.
- **3.7, Markdown:** built as planned: Source, Split and Preview, the
  preview drawn by `norn-ui` from `norn.markdown`'s data; in Split, the
  block at the caret comes to the top of the preview (by the line each
  top-level block starts on, which `norn.markdown/parse-lines` gives).
  Links open: web addresses in the browser, paths as documents.

On the way:
- `norn-ui` gained:
  - `dock-group` (one group of a dock's layout, placed anywhere, so the
    left, right and bottom docks share drags, and so do editor groups);
  - `dock-dragging?` (to show empty docks as drop targets);
  - close buttons and context menus on dock tabs;
  - `set-title!`, `window-size`, `editor-goto!` and `editor-open-find!`;
  - editor hooks (`editor-hooks!`: the app's keys and typing first) and
    editing by index (`editor-change!`, `editor-insert!`,
    `editor-index`, `editor-pos`, `editor-text-range`);
  - a fix: an editor model whose node leaves the window (a tab hidden)
    forgets it, and gets its carets and decorations again when shown;
    edits to a hidden model sent ops to a removed node, and the engine
    rejected them;
  - `:gutter false` on the editor surface (the REPL's input has no line
    numbers);
  - tree rows with a badge (a Git status letter) and an icon colour of
    their own; `markdown` taking `:block-id` (an id for each top-level
    block, to measure and scroll to); `editor-set-text!` taking a function applied in the same
    change;
  - a fix: `focus!` and an editor's updates wait for a render in
    progress (a reentrant lock), as they name nodes of the last rendered
    tree; arriving just after a render removed one, the engine rejected
    them;
  - a fix: a window's renders are serialized. A `refresh!` while the
    render loop was rendering the same window sent the engine two diffs
    against one old tree, and later ops named nodes that were gone.
- Norn gained:
  - `norn.test/run-files` taking directories (every `*_test.norn` under
    one), as `norn test` does, so the editor protocol's `:test` can too;
  - `norn FILE.norn ARGS…`, which runs the file with its arguments, as
    the usage line always said (it refused them);
  - `tree-seq`, and `get-in` with a default;
  - `norn.markdown/parse-lines`: the parse, and each top-level block's
    first line;
  - `doseq` with several bindings, `:let` and `:when`, as `for` has
    (it silently ignored all but the first binding);
  - `io/canonical`, a path with its symbolic links resolved, as
    file-system events report paths (macOS's `$TMPDIR` is under one).

| Part | Work |
|---|---|
| 3.1 | **The shell:** a window with the activity bar (Explorer, Project, Outline, Source Control), the sidebar, docks for tools (left, right, bottom; drag tabs between them; ⌘J), the top bar (project, branch, a chip for the running `norn`), the status bar, the command registry and palette (⇧⌘P), settings (⌘,), and Light and Dark themes. |
| 3.2 | **Documents:** open, save and save-all, dirty markers, tabs (drag between editor groups), reload on outside changes, ⌘F find and replace, ⇧⌘F find in files (in parallel fibers), go to file (⌘P), and go to definition. |
| 3.3 | **Norn editing:** highlighting with depth-coloured brackets, indentation on Enter and Tab, ⇧⌘I to format (`:format`), lint warnings as squiggles and in Problems, completion (open documents plus the REPL), signature hints, ⌘/ to comment, and structural editing (balanced insertion, wrap and splice, forward and backward by form). |
| 3.4 | **The REPL and evaluation:** start and stop the project's `norn --server` (the top-bar chip). A REPL console with history and multi-line input, where output streams. ⌘⏎ evaluates the top-level form, with its result inline; ⇧⌘⏎ evaluates the file. Errors go to Problems. |
| 3.5 | **The debugger and inspector:** fibers parked in the debugger, with their condition, restarts, backtrace and locals, and evaluate-in-frame. The Inspector panel navigates values (`:inspect`). The tests panel runs `norn.test` and shows the results. |
| 3.6 | **Explorer, Git, Project and Outline:** a file tree with icons and Git status (colours and letters, dots on folders, ignored files hidden or dimmed), Source Control (stage, unstage, commit, and a side-by-side diff), the Project view, and the Outline (top-level forms of the document). |
| 3.7 | **Markdown:** Source, Split and Preview modes (⇧⌘V), with the preview drawn natively from `norn.markdown`, following the caret. |
| 3.8 | **AI chat:** Claude Code (stream-json) and Codex (app-server) through `norn.process` and `norn.json`, rendered as rich text. Agent mode can read and edit documents and drive the REPL, with approval; Chat mode can't make changes. |
| 3.9 | **Team chat**, last in M7.6, after the done-check's work: Xeon Studio's team of agents, with plans, tasks in Git worktrees, reviews and merges. |
| 3.10 | **Done-check** (runtime.md §11, M7.6): Norn's own development happens in the editor for a working stretch: editing `lib/` and `tests/`, the REPL, the debugger, formatting and linting. What it needed is fixed or scheduled before M8. Also measured: the idle editor under 0.5% CPU, and a keystroke repainting one line. |

Phased, as design §9.1 says: the API/HTTP workbench (after M9's HTTP
client), the Databases view (after M9's drivers) and the terminal panel
(after pseudo-terminals in `norn.process`).

## Decisions (the user, 2026-11-13)

1. **The name:** `norn-studio`, the app "Norn Studio", in
   `~/projects/norn/norn-studio` beside `norn-ui`. (The checkouts moved
   from `~/projects/loom` to `~/projects/norn` on 2026-11-13, and
   `loom-lang` became `norn-lang`.)
2. **Team chat** (about 4,000 lines in Xeon Studio, and not needed by the
   done-check) **is built last in M7.6**, after the done-check's work, so
   it never blocks the rest.
3. **`norn.markdown` goes in the stdlib** (part 1.3), since sealing is
   still ahead.
