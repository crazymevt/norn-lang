# The Skein UI protocol, version 1

The contract between `skein.ui` (Skein: state, views, diffing, widgets,
commands) and the `skein-ui` engine (Rust: the retained node tree, layout,
styling, text, painting, input). Design: design §9.1; plan:
`docs/plans/plan-m7.5.md`. This document is versioned with `skein-ui`.

## 1. Principles

- **Thick protocol.** The engine owns everything per-frame: layout,
  painting, hit-testing, scrolling, caret blink, hover and pressed states,
  animations. Skein is never on a frame's critical path.
- **Skein sends patches, the engine sends meanings.** Skein describes the
  tree with *patch batches*; the engine reports *semantic events* (a click
  on node 12, text field 7 changed). Raw pointer motion never crosses.
- **Data, not bytes.** Both directions carry Skein data in one process;
  nothing is serialized.
- **Only what's asked for.** A node lists the events Skein wants from it
  (`:on`); everything else stays in the engine.
- **Idle is idle.** With nothing happening, neither side sends anything,
  and the engine draws nothing (design §9.1's idle rules).

## 2. Transport

- **Skein → engine:** `(skein.ui.native/apply! engine batch)`, a primitive
  that queues the batch and wakes the event loop; it never blocks. A batch
  is a vector of ops, applied **atomically between frames**, in order.
- **Engine → Skein:** a channel given at connection
  (`(skein.ui.native/connect events-chan)` returns the engine handle). The
  engine posts through a `Port` (runtime.md §5.7). It coalesces events
  of the same kind and node (`:scroll`, `:input`, `:resize`) while the
  channel is full, and never drops the others.
- **Handshake:** the first batch is `[[:hello 1]]`; the engine answers
  `{:event :hello :version 1 :features #{…}}`. Additions to a version are
  announced in `:features`; incompatible changes make a new version.
- **Errors:** a bad op rejects its whole batch, and the engine posts
  `{:event :error :batch n :op op :message "…"}`. Nothing is half-applied.

## 3. Nodes

A node has an **id** (a positive integer chosen by Skein, unique per
engine), a **kind**, **props**, a **style** and ordered **children**. A node
exists from `:create` and is shown once inserted under a window's root.

| Kind | Props (beyond the common ones) | Children |
|---|---|---|
| `:box` | — (layout comes from the style) | any |
| `:text` | `:text`, `:wrap` (`true`, `false`, `:ellipsis`) | — |
| `:rich-text` | `:spans`: strings or `[text style]`, a style setting `:font-family` (`:ui`, `:code`), `:font-size`, `:font-weight`, `:font-style`, `:color`, `:background`, `:underline`, `:strike` and `:link` (a click sends `:link`). It wraps to its width; dragging selects and Copy copies. | — |
| `:icon` | `:d` (SVG path data), `:view` (the box it's drawn in, 24), `:stroke` (a width; else filled), `:size`; colour from the style | — |
| `:image` | `:src` (path or bytes), `:fit` (`:contain`, `:cover`, `:fill`) | — |
| `:svg` | `:src`, `:tint` | — |
| `:button` | — (a box that is focusable and has `:pressed`) | any |
| `:text-field` | `:value`, `:placeholder`, `:multiline`, `:password` | — |
| `:scroll` | `:axis` (`:y`, `:x`, `:both`) | one |
| `:list` | `:count`, `:row-height` (a hint) | managed (§7) |
| `:grid` | `:rows`, `:cols`, `:col-widths` | managed (§7) |
| `:editor` | `:config` (§6) | — |
| `:terminal` | `:cols`, `:rows` | — |
| `:canvas` | `:commands` (§8) | — |
| `:native-view` | `:view` (`:webview`, …) and its props (§9) | — |
| `:popup` | `:anchor` node id, `:placement` (`:below`, `:right`, …) | any |

**Common props:**
- `:class`: a keyword or a vector of them, for themes.
- `:style`: a map overriding the theme.
- `:on`: the set of events wanted, e.g. `#{:click :key}`.
- `:focusable`, `:disabled`, `:hidden`, `:tooltip` (text), `:cursor`.
- `:drag` (data to drag) and `:accept` (drop kinds).

A `:popup` is drawn in a small OS window of its own, so it can sit above
native views and outside the parent window.

## 4. Ops

| Op | Meaning |
|---|---|
| `[:hello version]` | Handshake (§2). |
| `[:window id props]` | Create or update a window: `:title`, `:size [w h]`, `:min-size`, `:resizable`, `:root` node id. A popup also has `:owner` (a window id) and `:position [x y]` in the owner's coordinates. It shows as a borderless window above the owner that doesn't take focus, and is dismissed lightly (`:dismiss`, §11), except by a press on its `:anchor` node. `:fit-content true` sizes a window to its content. |
| `[:close-window id]` | Close it (its nodes stay, detached). |
| `[:create id kind props]` | Make a detached node. |
| `[:set id props]` | Merge props; a `nil` value removes one. |
| `[:style id map]` | Merge style properties; a `nil` value removes one. |
| `[:text id string]` | Set a `:text` node's text, or a text field's value. |
| `[:insert parent index id]` | Insert (or re-insert) a node as a child. |
| `[:move parent from to]` | Reorder a child. |
| `[:remove id]` | Remove a node and destroy it with its subtree. |
| `[:theme theme]` | Replace the theme (§5). |
| `[:focus id]` | Move keyboard focus. |
| `[:scroll-to id]` | Scroll a node into view. |
| `[:select id from to]` | Select text in a field or rich text. |
| `[:menu tree]` | The app's menu bar (on macOS the first submenu is the application menu). A tree is a vector of items: `[label command shortcut?]` (command a keyword; shortcut like `"cmd+s"`, `cmd` meaning ctrl off macOS), `[label [items…]]` for a submenu, or `:separator`. The commands `:quit` and `:hide` are the system's own; `:copy` `:cut` `:paste` `:select-all` also act on a focused text field. |
| `[:context-menu req tree window x y]` | Show a native context menu at (x, y) in a window; answered by `{:event :command :req req :command c}` or `{:event :menu-dismissed :req req}`. |
| `[:dialog req spec]` | A file dialog: `{:kind k :title t :directory d :filters [[name [ext…]] …]}`, `k` one of `:open-file`, `:open-files`, `:folder`, `:save-file`; answered by `{:event :dialog :req req :paths […]}` (nil if cancelled). |
| `[:clipboard-set text]`, `[:clipboard-get req]` | The clipboard; get answers `{:event :clipboard :req req :text …}`. |
| `[:stats req]` | Counters (§10). |

A scroll container's `:scroll-x`/`:scroll-y` props set its position, after
layout and clamped. The engine ignores a value it reported itself in a
recent `:scroll` event: that is Skein echoing it back. `:transition ms` (or
`[:prop ms …]`) in a style animates background, colour, border colour and
opacity changes.

Editor, list, grid, terminal, canvas and native-view ops are in §6–§9.

## 5. Styles and themes

**Style properties** are keywords with CSS-like names and meanings:
- **Layout** (taffy): `:display` (`:flex`, `:grid`, `:none`),
  `:flex-direction`, `:flex-grow`, `:flex-shrink`, `:flex-basis`,
  `:align-items`, `:justify-content`, `:gap`, `:padding`, `:margin`,
  `:width`, `:height`, `:min-width`, `:max-width`, `:min-height`,
  `:max-height`, `:grid-template-columns`, `:grid-template-rows`,
  `:position` (`:relative`, `:absolute`), `:inset`.
- **Paint:** `:background`, `:color`, `:border` (`[width color]`),
  `:border-radius`, `:opacity`, `:shadow` (`[x y blur color]`).
- **Text:** `:font-family`, `:font-size`, `:font-weight`, `:font-style`,
  `:line-height`, `:text-align`.
- **Motion:** `:transition` (`{:opacity 150}`: property → milliseconds).

**Values:**
- Lengths are numbers in logical pixels, `[:% n]`, `[:fr n]` (grid
  tracks) or `:auto`.
- Colors are `"#rrggbb"`, `"#rrggbbaa"`, `[r g b a]` or a theme token
  (`:accent`).

**A theme** is
`{:tokens {:accent "#4f8cff" :bg "#1e1e1e" …} :rules [[selector style] …]}`.
- A selector is `[kind class state]`, any part `:*`: `[:button :primary
  :hover]`. Later rules win; a node's own `:style` wins over all.
- States are tracked by the engine: `:hover`, `:pressed`, `:focused`,
  `:disabled`, `:selected`, `:dragging`. Hover and press never cross the
  protocol; they only restyle.

## 6. The editor surface

The text lives in Skein (a rope). The engine shows a viewport onto it and
asks for what it shows; a keystroke moves only the changed lines.

**Ops:**
- `[:editor-config id {:font … :tab-width 2 :gutter true :wrap false}]`
- `[:line-count id n]`
- `[:lines id from [line …]]`
  - a line is `[text spans]`;
  - spans are `[[start end style-key] …]` in characters, style keys
    resolving through the theme (`:keyword`, `:string`, `:bracket-1` …).
- `[:carets id [{:line l :col c :anchor [l c]?} …]]`: several carets;
  `:anchor` makes a selection.
- `[:decorations id [{:line l :from c :to c :kind :squiggle :style :error :message "…"} …]]`:
  squiggles, highlights and inline results (`:kind :inline :text "=> 42"`).
- `[:gutter id [{:line l :marker :breakpoint} …]]`
- `[:reveal id line]`: scroll a line into view.

**Events:**
- `{:event :need-lines :node id :from a :to b}`: lines entering the
  viewport. Skein answers with `:lines`; the engine may draw a line's old
  version or a blank until then.
- `{:event :key :node id …}` (§11): keys go to Skein, which owns the
  keybindings and commands. It edits the rope and answers with the
  changed `:lines` and new `:carets`.
- `{:event :text :node id :text "é"}`: committed text input, after IME
  composition.
- `{:event :click :node id :line l :col c :count n :modifiers #{…}}` and
  `{:event :drag-select :node id :from [l c] :to [l c]}` (coalesced).
- `{:event :viewport :node id :first l :last l}` (coalesced).

The engine draws the carets, the selections and the blink. IME
composition is drawn by the engine until committed.

## 7. Virtualized lists and grids

A list shows `:count` rows but holds nodes only for those near the
viewport.

- `{:event :need-rows :node id :from a :to b}`: Skein creates a subtree per
  row and answers `[:rows id from [node-id …]]`.
- `{:event :rows-released :node id :from a :to b}`: the rows scrolled far
  away; Skein may `:remove` them.
- **A grid** works the same way by cells, with a fast path for text cells:
  `[:cells id [[row col text style-key] …]]`, which needs no nodes.

## 8. Canvases

`:commands` is a vector of drawing commands in the node's coordinates.
The engine redraws a canvas only when its commands change (`[:set id
{:commands …}]`).
- **Shapes:** `[:rect [x y w h] paint]`, `[:rounded-rect [x y w h] r
  paint]`, `[:circle [cx cy] r paint]`, `[:line [x1 y1] [x2 y2] stroke]`,
  `[:polyline [[x y] …] stroke]`, `[:path "M0 0 L10 10 …" paint]` (SVG
  path syntax).
- **Text and images:** `[:text [x y] "…" text-style]`; `[:image [x y w h]
  src]` is not drawn yet.
- **Clipping and transforms:** `[:clip [x y w h] [commands …]]`,
  `[:translate [dx dy] [commands …]]`.

Paints are `{:fill color}` and strokes `{:stroke color :width n}`.

A widget class in Skein (a subclass of `ui/Widget` with `ui/measure` and
`ui/paint` methods, and `ui/widget-events` and `ui/on-event` for input)
produces a canvas node.

## 9. Native views

A `:native-view` reserves its layout box; the backend places a real OS view
there and keeps it in step with layout, scrolling and clipping.

**Webviews** (`:view :webview`, from the optional `skein-ui-webview`
package):
- props: `:url` or `:html`, and `:schemes` (custom URL schemes Skein
  serves);
- ops: `[:webview-eval id js]` and `[:webview-post id data]`;
- events: `{:event :webview-message :node id :data …}` and
  `{:event :webview-request :node id :req r :url …}`, answered by
  `[:webview-respond id r {:status 200 :mime "text/html" :body …}]`.

**Limits** (design §9.1):
- native views draw above everything else, which is why popups are OS
  windows;
- they can't be themed, made translucent or transformed;
- headless rendering shows a placeholder for them.

## 10. Counters

`[:stats req]` answers
`{:event :stats :req req :frames n :area px :layouts n :since-ms t}`: the
frames drawn, the pixels repainted and the layout passes since the
previous request. The idle and keystroke tests use these:
- "no frames in 5 s idle";
- "one keystroke repaints at most one line".

## 11. Events

Every event is a map with `:event` and, where they apply, `:window`, `:node`
and `:time` (milliseconds).

| Event | Fields |
|---|---|
| `:click` | `:button` (`:left`, `:right`, `:middle`), `:count`, `:modifiers`, `:x` `:y` (node coordinates) |
| `:key` | `:key` (`"a"`, `:enter`, `:arrow-left`…), `:code`, `:modifiers` (`#{:cmd :shift :alt :ctrl}`), `:repeat` |
| `:text` | committed text (§6) |
| `:input` | a text field's new `:value` (coalesced per frame) |
| `:submit` | Enter in a single-line text field |
| `:focus`, `:blur` | focus moved |
| `:scroll` | `:x` `:y` scroll position (coalesced) |
| `:command` | `:command` from a menu or a shortcut; with `:node`, the focused node asked for `:command` (an editor's copy and paste) |
| `:context-menu` | a right click (ctrl-click on macOS) on a node asking for it: `:x` `:y` in the window |
| `:dismiss` | a popup should close: a press in its owner, or Escape |
| `:tooltip` | the pointer rested on a node with `:tooltip`: `:node` `:text` `:x` `:y`; `:node nil` hides it |
| `:drag-start`, `:drag`, `:drag-end` | dragging a node that asks for `:drag`, after a 3-pixel threshold: `:drag` has `:dx` `:dy` (since the last, coalesced), `:x` `:y` in the window and `:local-x` `:local-y` in the node |
| `:drag-select` | a drag in an editor: `:anchor [line col]`, `:line`, `:col` |
| `:drop` | `:data` and `:kind`, on the nearest node under the pointer that asks for `:drop` (else the window); files from the OS have `:kind :files` and `:paths` (files dropped together arrive as one event) |
| `:resize` | a window's new `:size` and `:scale` (coalesced) |
| `:moved` | a top-level window's new `:position` on screen (coalesced) |
| `:link` | a click on a link span of rich text: `:href` |
| `:close-requested` | the user asked to close a window (Skein decides) |
| `:window-focus` | `:focused` true or false |
| `:need-lines`, `:viewport`, `:drag-select` | §6 |
| `:need-rows`, `:rows-released` | §7 |
| `:webview-message`, `:webview-request` | §9 |
| `:dialog`, `:clipboard`, `:menu-dismissed`, `:stats`, `:hello`, `:error` | answers to ops |

Keys go to the focused node if it asks for `:key`, else to its window.
Shortcuts bound in a menu become `:command`s instead.

## 12. Lifecycle and images

- Windows and the engine are image resources (M5). After a restore,
  `skein.ui`'s `on-image-restore` connects a new engine and replays its
  current tree, so windows reopen with the same content.
- The engine is per process. Closing the last window doesn't end the
  process: Skein decides (`exit`).
