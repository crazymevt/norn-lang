# Plan: M7.6 — the Skein editor

Drafted 2026-11-13; the review questions are settled (below). The decisions behind it are in design
§9.1 (the editor is essentially Xeon Studio rebuilt in Skein; Skein-only
language support first; four panels phased) and the M7.6 row of
runtime.md §11. This plan says how to build it and in what order.

**What M7.6 is for.** It's the first real Skein program, written while the
stdlib can still change (it's sealed at M8). The done-check is that Skein's
own development happens in the editor. Everything the editor needed from
Skein is fixed, or scheduled, before M8.

**Where the work lives:**
- **This repository** gains what the editor needs from Skein:
  - editor-protocol ops;
  - file watching;
  - `skein.markdown`;
  - fixes found along the way.

  It ends with an M7.6 commit like the others.
- **`skein-ui`** goes to 0.2 with the widgets the editor needs (below).
- **The editor, Skein Studio**, in a new repository,
  `~/projects/skein/skein-studio` (git, no remote, MIT). It's Skein code run by
  the `skein-ui` binary,
  pinning a `skein-ui` version. Until M8's packages exist, it loads Skein
  and `skein-ui` from local checkouts by path, as `skein-ui` does now.

**The reference.** Xeon Studio (`~/projects/clojure/xeon-studio`, about 19k
lines of Clojure on JavaFX). Its feature list and keys are the target;
its code is a guide, not a source to translate line by line.

## Architecture

- **Two processes.** The editor is a `skein-ui` process. It starts the
  project's own `skein --server 0` and talks to it over the editor
  protocol, for the REPL, evaluation, the debugger, the inspector,
  completion, and `skein fmt` and `skein lint`. Code under development can
  crash or hang without taking the editor with it.
- **The editor's state is data.** It's in atoms: the open documents
  (persistent ropes), layout, settings and REPL sessions. Views are
  functions of it, as in the gallery. Commands are named functions in one
  registry, which feeds menus, key bindings and a command palette.
- **Skein knowledge comes from Skein.** The editor gets:
  - highlighting and structure from the comment-keeping reader;
  - indentation from `skein fmt`'s rules, including macros' `{:indent n}`
    hints;
  - warnings from `skein lint`;
  - completion, signatures and definitions from the running `skein`;
  - its own lexer, only for highlighting while a form doesn't parse yet.
- **Budgets, as Xeon Studio keeps:**
  - no timers while idle, and no animation that runs forever;
  - a keystroke repaints a line;
  - streaming output (REPL, chat) is coalesced to about 12 redraws a
    second;
  - hidden tabs don't render.

## Part 1 — in this repository: what the editor needs from Skein

| Part | Work |
|---|---|
| 1.1 | **Editor-protocol ops** (`%serve-request`), each with a test in `tests/server.rs`. The ops: `:eval` with `:file` and `:line` (so errors and definitions point at the source); `:load-file`; `:interrupt` (a running evaluation); `:doc` (a var's docstring and arglists, for signature hints); `:definition` (file and line); `:macroexpand`; `:inspect` (structured, navigable values); `:ns-vars` (for outlines of loaded code); `:test` (run `skein.test`, results as data); and pushed `:out`/`:err` events, so printed output streams to the REPL as it happens. |
| 1.2 | **File watching:** already there. `skein.fs/watch` (M6) gives a channel of create, modify and remove events, recursively if asked, and is tested in `lib_test`. Nothing to build. |
| 1.3 | **`skein.markdown`:** CommonMark (headings, emphasis, links, images, lists, block quotes, code blocks, tables) parsed to Skein data, for the Markdown preview and the chats. In the stdlib, since sealing is still ahead. |
| 1.4 | **What the editor turns up**, fixed as found and listed in the status: missing stdlib functions, slow paths (Skein's `sort-by` took about a second for 10,000 maps in a debug build), and protocol gaps. |

## Part 2 — `skein-ui` 0.2: what the editor needs from the UI (done, 2026-11-13)

Built as planned. Along the way, Skein's core gained about 60 of
Clojure's functions and macros it lacked (`case`, `cond->`, `some->`,
`subvec`, `merge-with`, `boolean`, `sequential?` and more). Text nodes
now report their narrowest width correctly, so text wraps inside rows.
`delay`, `future`, promises and `ex-info` are left for later: they need
a design against Skein's fibers and conditions.

| Part | Work |
|---|---|
| 2.1 | **Rich text flow:** wrapped paragraphs of styled spans, links, inline code and embedded blocks, for the Markdown preview and the chats; selectable and copyable. |
| 2.2 | **Icons:** SVG icons drawn on canvases (file types, activity bar, toolbar), with a small bundled set. |
| 2.3 | **Shell widgets:** an activity bar, a collapsible sidebar, a top bar with chips, a status bar, and a command palette (a popup list with fuzzy filtering). |
| 2.4 | **The editor surface, for code:** brackets coloured by depth, with the matching bracket emphasised; completion and signature popups anchored at the caret; find and replace; undo and redo in the model (grouped edits); inline results; and, for large files, incremental highlighting. |
| 2.5 | **Persistence:** window size and position, dock layout, and sidebar state saved and restored. |

## Part 3 — the editor

**Status (2026-11-14): 3.1 to 3.8 done.**
- **3.1, the shell:** built as planned, with a basic Explorer tree (3.6
  adds icons for Git status and the rest). The tools in the docks are
  placeholders until their parts.
- **3.2, documents:** built as planned. Each folder's open documents
  come back with it. Go to definition reads the project for top-level
  `def` forms until 3.4 asks the running `skein` (`:definition`).
- **3.3, Skein editing:** built as planned. The project's `skein --server`
  starts with the folder (the top-bar chip: start, stop, its output), a
  step ahead of 3.4, since lint, completion and signatures come from it.
  Enter and Tab indent by `skein.fmt`'s own rules, run in the editor:
  format the top-level form up to the caret with a probe line, and see
  where the probe lands. ⇧⌘I formats with the project's `skein` (it knows
  the project's macros' `{:indent n}`), or `skein.fmt` in the editor when
  it doesn't run.
- **3.4, the REPL and evaluation:** built as planned. A failed evaluation
  waits in the project's `skein`'s debugger: the console shows the error
  with buttons for its restarts (3.5 adds backtraces, locals and
  evaluating in a frame), and an error at a document's line goes to
  Problems. ⌘⏎ sets up the document's namespace first (its `ns` form)
  if this `skein` hasn't seen it.
- **3.5, the debugger, inspector and tests:** built as planned. The
  Debugger comes forward when a fiber stops, at the first frame of a
  named function. A frame's locals arrive printed (`:backtrace`), so
  they're shown as text, split into names and values. F12 now asks the
  running `skein` first (`:definition`), then reads the project.
- **3.6, the Explorer, Git, Project and Outline:** built as planned, by
  running `git` (always with `--no-optional-locks`: `git status` would
  otherwise rewrite the index, and a commit made in a terminal meanwhile
  would find `index.lock` taken). A diff is a tab of its own, side by
  side, read from `git diff` with all the context. Commits and checkouts
  made outside show, as the folder watch passes `.git/index` and
  `.git/HEAD` through.
- **3.7, Markdown:** built as planned: Source, Split and Preview, the
  preview drawn by `skein-ui` from `skein.markdown`'s data; in Split, the
  block at the caret comes to the top of the preview (by the line each
  top-level block starts on, which `skein.markdown/parse-lines` gives).
  Links open: web addresses in the browser, paths as documents.
- **3.8, AI chat:** built as planned, by porting Xeon Studio's sessions:
  Claude Code over its CLI's stream-json (Skein Studio's tools as an
  in-process MCP server, "sdk"), Codex over its app-server (the tools as
  dynamic tools). Agent mode reads and edits documents and drives the
  project's `skein` (the REPL, debugger, inspector, tests), asking before
  each change unless Approve for Me is on; Chat mode refuses changes.
  The tests use fake CLIs (written in Skein) that speak both protocols;
  the real Claude Code and Codex haven't been run against it yet.
- **3.10, the done-check, the measured part (2026-11-14):**
  - **A keystroke repaints a line:** the edited line and the caret's
    place in the status bar, about 11,900 square pixels (a line is
    10,374), down from 35,000 to 64,000. Three things had been
    repainting too: the status bar's Ln/Col changed width (it reflowed
    the bar), every text field repainted after any layout, and
    completion offered the word being typed, so its popup closed and
    opened again on every key. `tests/budgets_test.skn` holds it.
  - **The idle editor under 0.5% CPU:** 0.0% (no CPU time in 30
    seconds), and its project `skein` too; it had been 0.47%. Skein's
    idle carriers woke ten times a second each to look for work, though
    everything that gives them work wakes them; now only a spare carrier
    wakes on a timer (to retire).
  - **Speed in use:** the Markdown preview and scrolling stalled in a
    debug build of `skein-ui` (5.8 seconds to show a 222-line document's
    preview; half a second a scroll step). A release build is 10 to 20
    times faster (0.3 seconds; 25 to 30 ms a step), so `bin/skein-studio`
    runs the release build when it's the newer one. The preview's first
    layout (0.25 seconds in release) lays out every block, in view or
    not: one to improve if it bothers. Also found in use and fixed in
    `skein-ui`: the wheel scrolled boxes whose overflow was only hidden
    (moving a whole layout), text was shaped again on every paint (now
    cached), and a Markdown code block's longest line widened the whole
    preview (now it scrolls sideways, its text `:white-space :pre`).
    Then: the preview told the app every scroll position, so each wheel
    event re-rendered the whole window (170 ms with a long preview) and
    pushed an older position back; the preview now scrolls by itself, and
    `ui/memo` keeps its elements until its text changes (a re-render with
    it showing: 19 ms). Scroll containers and editors have scroll bars.
    And a race: a new node's handlers were registered after its ops went
    out, so an editor's first request for its lines could be lost, leaving
    it blank (switching a Markdown document back to Source showed it).
    And the desktop backend laid out and drew the window for every wheel
    event; a trackpad sends far more than there are frames, so the
    backlog kept scrolling for seconds after the fingers stopped. Wheel
    deltas are now summed and applied once per batch of events. Scroll
    bars drag (and a press on the track jumps there). Then the real
    culprit for the wheel: every change of the hovered node restyled
    every ancestor's whole subtree (and each node's style was resolved
    from the root again), so with a long preview one pointer move took
    700 ms, and macOS held the wheel's events meanwhile and sent them
    summed into one jump. Now only the nodes whose hover state changed
    are restyled, going down only while styles change, each style
    resolved once. Big single wheel steps (a mouse's) glide over about
    150 ms. `SKEIN_UI_FRAMES=1` reports slow steps, event handling and
    waits for a surface. Also: a popup's size or position arriving
    after it closed brought back half its entry, and every render then
    failed ("rendering window 2 failed").

    The AI Chat in use, with the real CLIs:
    - `io/file?` didn't follow symbolic links, and `~/.local/bin/claude` is one,
      so Studio said to install Claude Code. Codex comes inside the ChatGPT
      app; Studio looks there too.
    - Streamed replies drew over themselves: a rich-text node wasn't measured
      again when its `:spans` changed.
    - The chat gained model and effort choices, from the models each CLI
      offers. Claude Code restarts with them, resuming; Codex takes them
      with each turn. It also gained a usage line: the context used, and
      what's left of the five-hour and weekly limits.
    - Restarting to resume raced the old process for the conversation, so a
      restart now waits for the old one to end, and a conversation that
      can't be resumed starts anew.
    - Two crashes. Children with the same key were made into one node, put
      in two places. And the engine's batch check didn't count removals, so
      the bad batch passed and panicked. Both are fixed.
    - A transition stalled while events kept coming.
    - A dock's tabs that didn't fit shrank and wrapped. Now they scroll
      sideways, and the chosen one comes into view.
  - **The working stretch: amended (2026-11-16, the user).** M7.6 is done.
    The done-check's purpose, the stdlib tested by real use before it's
    sealed, was met by Studio and five test programs (`lang-tests/`:
    pulse, ledger, quire, chat and loglens) and an interface sweep, whose
    findings are fixed (the todo lists them). Skein's own development in
    Studio goes on from here as everyday work, and what it turns up goes
    in the todo.

On the way:
- `skein-ui` gained:
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
  - a fix: an editor's reveal before its first layout scrolled as if the
    view were a line short (a newly opened document hid its first line);
    it now waits for the layout;
  - text fields repaint after a layout only when their caret or scroll
    moved; `SKEIN_UI_DAMAGE=1` (headless) prints what each frame repaints,
    and `SKEIN_UI_OPS=1` the ops each render sends;
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
- Skein gained:
  - idle carriers that sleep until woken (an idle process uses no CPU);
  - `skein.test/run-files` taking directories (every `*_test.skn` under
    one), as `skein test` does, so the editor protocol's `:test` can too;
  - `skein FILE.skn ARGS…`, which runs the file with its arguments, as
    the usage line always said (it refused them);
  - `tree-seq`, and `get-in` with a default;
  - `skein.markdown/parse-lines`: the parse, and each top-level block's
    first line;
  - `doseq` with several bindings, `:let` and `:when`, as `for` has
    (it silently ignored all but the first binding);
  - `io/canonical`, a path with its symbolic links resolved, as
    file-system events report paths (macOS's `$TMPDIR` is under one).

| Part | Work |
|---|---|
| 3.1 | **The shell:** a window with the activity bar (Explorer, Project, Outline, Source Control), the sidebar, docks for tools (left, right, bottom; drag tabs between them; ⌘J), the top bar (project, branch, a chip for the running `skein`), the status bar, the command registry and palette (⇧⌘P), settings (⌘,), and Light and Dark themes. |
| 3.2 | **Documents:** open, save and save-all, dirty markers, tabs (drag between editor groups), reload on outside changes, ⌘F find and replace, ⇧⌘F find in files (in parallel fibers), go to file (⌘P), and go to definition. |
| 3.3 | **Skein editing:** highlighting with depth-coloured brackets, indentation on Enter and Tab, ⇧⌘I to format (`:format`), lint warnings as squiggles and in Problems, completion (open documents plus the REPL), signature hints, ⌘/ to comment, and structural editing (balanced insertion, wrap and splice, forward and backward by form). |
| 3.4 | **The REPL and evaluation:** start and stop the project's `skein --server` (the top-bar chip). A REPL console with history and multi-line input, where output streams. ⌘⏎ evaluates the top-level form, with its result inline; ⇧⌘⏎ evaluates the file. Errors go to Problems. |
| 3.5 | **The debugger and inspector:** fibers parked in the debugger, with their condition, restarts, backtrace and locals, and evaluate-in-frame. The Inspector panel navigates values (`:inspect`). The tests panel runs `skein.test` and shows the results. |
| 3.6 | **Explorer, Git, Project and Outline:** a file tree with icons and Git status (colours and letters, dots on folders, ignored files hidden or dimmed), Source Control (stage, unstage, commit, and a side-by-side diff), the Project view, and the Outline (top-level forms of the document). |
| 3.7 | **Markdown:** Source, Split and Preview modes (⇧⌘V), with the preview drawn natively from `skein.markdown`, following the caret. |
| 3.8 | **AI chat:** Claude Code (stream-json) and Codex (app-server) through `skein.process` and `skein.json`, rendered as rich text. Agent mode can read and edit documents and drive the REPL, with approval; Chat mode can't make changes. |
| 3.9 | **Team chat:** moved out of M7.6 to the phased panels (below) on 2026-11-14. |
| 3.10 | **Done-check** (runtime.md §11, M7.6): Skein's own development happens in the editor for a working stretch: editing `lib/` and `tests/`, the REPL, the debugger, formatting and linting. What it needed is fixed or scheduled before M8. Also measured: the idle editor under 0.5% CPU, and a keystroke repainting one line. |

Phased, as design §9.1 says: the API/HTTP workbench (after M9's HTTP
client), the Databases view (after M9's drivers), the terminal panel
(after pseudo-terminals in `skein.process`), and team chat (after M7.6; it
needs no new library). The first three were built on 2026-11-15: the
Databases view (SQLite), the API workbench (`.http` collections,
environments, a Builder) and the terminal panel (an xterm emulator in Skein,
drawn by the editor element). Team chat is Xeon Studio's team of agents, with
plans, tasks in Git worktrees, reviews and merges.

## Decisions (the user, 2026-11-13)

1. **The name:** `skein-studio`, the app "Skein Studio", in
   `~/projects/skein/skein-studio` beside `skein-ui`. (The checkouts moved
   from `~/projects/loom` to `~/projects/skein` on 2026-11-13, and
   `loom-lang` became `skein-lang`.)
2. **Team chat** (about 4,000 lines in Xeon Studio, and not needed by the
   done-check) was to be built last in M7.6. On 2026-11-14 it **moved to
   the phased panels**: it comes after M7.6, so M7.6 ends with the
   done-check.
3. **`skein.markdown` goes in the stdlib** (part 1.3), since sealing is
   still ahead.
