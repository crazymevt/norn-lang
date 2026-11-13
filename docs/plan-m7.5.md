# Plan: M7.5 — the runtime side of `norn.ui`, and `norn-ui` 0.1

Approved 2026-11-13. The decisions behind it are in design §9.1
(a separate `norn-ui` package, own GPU drawing, a thick protocol, idle
rules, embeddable OS views, the editor's scope). This plan says how to build
it and in what order.

**Where the work lives:**
- **This repository** gains two general hooks and a written protocol. That
  is all of M7.5 here; it ends with an M7.5 commit like the others.
- **`~/projects/loom/norn-ui`** (new; git, no remote, MIT) holds the engine,
  the backends and the widget library, versioned from 0.1. This session
  builds it too, so both sides can change together while the protocol
  settles. Until M8's packages exist it uses this repository by path.

**Stack** (decided 2026-11-13): `winit` (windows, events), `wgpu` (Metal on
macOS), `taffy` (flex and grid layout), `parley` (text shaping and layout,
with `fontique` for fonts), `accesskit` (accessibility), `muda` (menus),
`rfd` (file dialogs), `arboard` (clipboard). All are cross-platform, so
Linux and Windows (M12) are mostly backend glue.

## Part 1 — in this repository

| Part | Work |
|---|---|
| 1.1 | **The native extension API** (`norn_vm::ext`). A Rust crate implements `Extension`: a name, primitives to register in a namespace it owns (`norn.ui.native` for the UI), and a start hook. Native objects are `Handle` values (M6) with cleanups. Threads outside the carriers post to channels with `ext::post(chan, build)`, where `build` makes the Norn value inside `delivering` on the shared feeder mutator (runtime.md §5.5), so the rule for foreign threads is enforced by the API rather than by care. Primitive tables become extensible at start-up (today they're one static table). |
| 1.2 | **Main-thread handoff.** A builder for embedding: `Norn::builder().extension(e).run_on_main(f)` starts the carriers, loads the image or core, runs the entry or REPL on a carrier, and gives the process's first thread to `f` (the UI's event loop; macOS requires it). The terminal REPL's stdin reading moves off the main thread. Shutdown: `f` returning ends the process the way `exit` does; `exit` from Norn wakes `f` to return. |
| 1.3 | **The UI protocol spec** (`docs/ui-protocol.md`), the contract between `norn.ui` (Norn) and the engine (Rust). See the outline below. |
| 1.4 | **Done-check here:** a test-only extension crate registers primitives, posts from a foreign thread at a high rate under `NORN_GC_STRESS=2`, and runs a fake event loop through `run_on_main`; images saved with extension handles report `ResourceLost` as M6's sockets do. Docs: runtime.md §5.7 (extensions) and the M7.5 status. |

### The protocol, in outline (1.3 writes it in full)

- **Transport:** Norn data on channels in one process, never serialized.
  Norn → engine: a channel of *batches* (vectors of patch ops, applied
  atomically between frames). Engine → Norn: a channel of events.
- **Nodes:** integer ids chosen by Norn; a kind (`:box`, `:text`, `:image`,
  `:scroll`, `:canvas`, `:editor`, `:grid`, `:native-view`, …), properties,
  a style map (CSS-like property names as Norn keywords, no CSS text), and
  ordered children.
- **Patch ops:** `[:create id kind props]`, `[:set id props]`,
  `[:insert parent index id]`, `[:remove id]`, `[:move parent from to]`,
  `[:text id string]`, `[:style id map]`, `[:theme map]`, and window ops
  (`[:window id props]`, `[:close id]`).
- **Events:** semantic only: `{:event :click :node id :button :left}`,
  `:input` (text field changed), `:key` (with the focused node and
  modifiers), `:focus`, `:scroll` (position, coalesced), `:resize`,
  `:command` (menus, shortcuts), `:close-requested`, `:drop`. Raw pointer
  motion never crosses; hover and pressed states are engine-side styles.
- **The editor surface:** the engine asks for lines
  (`{:event :need-lines :node id :from a :to b}`); Norn answers with
  `[:lines id from [[text spans] …]]`, spans being `[start end style]`. A
  keystroke sends only the changed lines. Carets, selections and
  decorations (squiggles, inline results) are ops of their own.
- **Counters:** `[:stats]` asks for frames drawn, area repainted and layout
  passes, for the idle and keystroke tests.
- **Versioning:** a `[:hello version]` handshake; the spec is versioned with
  `norn-ui`.

## Part 2 — `norn-ui` 0.1

Built headless-first: every engine feature is testable without a window,
and the macOS backend comes once the engine draws correctly to images.

| Part | Work |
|---|---|
| 2.1 | **Scaffold:** workspace (`norn-ui-engine`, `norn-ui-headless`, `norn-ui-macos`, the `norn-ui` binary = runtime + extension), `lib/norn/ui/*.norn`, CI-able tests, MIT. |
| 2.2 | **Engine core:** the node tree applying patch batches, styles and themes, `taffy` layout, hit-testing, scrolling, damage regions, frame and layout counters. Tested with layout assertions, no GPU. |
| 2.3 | **Text:** `parley` shaping and line layout, font fallback, caret and selection geometry, the glyph cache. A default UI font and a code font are bundled (OFL-licensed) so output is the same on every machine; system fonts are used for fallback. |
| 2.4 | **Painting:** a `wgpu` pipeline for rects (rounded, bordered), glyph runs, images, clipping and simple shadows; damage-only repaint. Headless rendering to PNG with golden-image tests (a small per-pixel tolerance). |
| 2.5 | **`norn.ui` in Norn:** declarative views over atoms, diffing to patches, event handlers, commands and focus; redefining a view at the REPL re-renders open windows; windows are image resources (`on-image-restore` reopens them). |
| 2.6 | **The macOS backend:** windows on the handed-off main thread, the `wgpu` surface (low-power GPU preferred), IME, clipboard, menus, dialogs, drag and drop; the idle rules (no render loop, frame timer only while animating, caret blink limits, nothing drawn when hidden). |
| 2.7 | **Widgets for the gallery:** buttons, toggles, dropdowns, text fields, splits, tabs (draggable between docks), a tree, virtualized lists and grids, scroll views, tooltips and popups (small OS windows), the code-editor surface, themes. |
| 2.8 | **`native-view` nodes** and the optional `norn-ui-webview` package (`wry`), with JavaScript ↔ Norn messages on channels. |
| 2.9 | **Done-check** (runtime.md §12, M7.5): the widget gallery runs smoothly on macOS; redefining a view at the REPL updates the open window; a restored image reopens its windows; headless: no frames in 5 s idle, one keystroke in the editor surface repaints at most one line; macOS: the idle editor averages under 0.5% CPU over a minute. |

## Decisions

- **App code is declarative** (the user, 2026-11-13): a view is a function
  from state to a tree of plain data (`[:column {:gap 8} [:text …]
  [:button {:on-click f} …]]`); changing the atom or redefining the view
  re-renders, and `norn.ui` diffs trees into patches. **CLOS is for
  extension points:** widgets that measure and paint themselves on a
  canvas (`ui/widget` subclasses with `ui/measure` and `ui/paint`
  methods), multiple-dispatch interactions such as `ui/accept-drop`, and
  commands. Themes are data.
- **The four questions below take the recommended answers** (the user
  approved the plan as drafted): headless CI on `wgpu`'s software adapter;
  themes as Norn data with CSS-like property names; Inter and JetBrains
  Mono bundled; webviews may slip to 0.2.

## Questions settled at review (recommendations taken)

1. **Headless rendering in CI:** the same `wgpu` path on a software adapter
   (lavapipe on Linux) rather than a separate CPU renderer, so tests check
   the real painter. *Recommended.*
2. **Theme format:** Norn data with CSS-like property names (no CSS
   parser); a theme is a map from selectors (node kind, class, state) to
   style maps. *Recommended.*
3. **Bundled fonts:** Inter for the UI and JetBrains Mono for code (both
   OFL). *Recommended*; the alternative is system fonts only, which makes
   golden images machine-dependent.
4. **Scope of 2.8:** webviews could slip to `norn-ui` 0.2 if M7.5 runs long;
   `native-view` itself stays in 0.1.
