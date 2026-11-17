# Editors

[Documentation home](README.md)

Skein talks to editors in two ways:

- **`skein --server`** speaks Skein's own editor protocol, which Skein Studio
  uses: evaluation with the debugger, the inspector, tests, and more
  ([Chapter 12](book/12-live-programming.md#editors-and-skein---server)).
- **`skein lsp`** speaks the Language Server Protocol, which most editors
  have a client for. This page sets it up.

## What `skein lsp` does

Run in a project, `skein lsp` is that project's Skein: its `:paths` and
packages are on the library path, as with `skein run`. It talks to the
editor on standard input and output, and gives it:

| Feature | What it shows |
| --- | --- |
| Diagnostics | `skein lint`'s warnings as you type: unresolved names, unused locals, wrong arities, and the rest. |
| Hover | A definition's arguments and docstring. |
| Completion | Names visible in the file's namespace. |
| Go to definition | Where a function, macro or var is defined, in the project or its packages. |
| References | Where each function that refers to a name does so (`skein.xref`'s callers). |
| Outline | The file's definitions (document symbols). |
| Formatting | `skein fmt`'s layout for the whole file. |
| `skein.eval` command | Evaluates the top-level form at a position and shows its value. |

A file's module is loaded when it's opened and loaded again when it's
saved, so hover, definition and references know what it defines. What its
code prints goes to standard error, which editors keep in their log for
the server. Definitions evaluated with `skein.eval` are an editor's
evaluations, so `unsaved-definitions` lists them until the file holds
them ([Chapter 12](book/12-live-programming.md#what-you-havent-saved)).

Every editor below needs to know two things: that `.skn` files are a
language of their own, and that `skein lsp` serves it. Start the editor in
the project folder, the one with `skein.edn`, or one under it.

## Emacs

Emacs 29 and later include eglot. In your init file:

```elisp
(define-derived-mode skein-mode lisp-data-mode "Skein")
(add-to-list 'auto-mode-alist '("\\.skn\\'" . skein-mode))
(with-eval-after-load 'eglot
  (add-to-list 'eglot-server-programs '(skein-mode . ("skein" "lsp"))))
(add-hook 'skein-mode-hook #'eglot-ensure)
```

`M-.` goes to a definition and `M-?` lists references. `eglot-format-buffer`
formats, and Flymake shows the diagnostics. This setup is the one Skein's
tests run (`tests/lsp/eglot-check.el`).

## Neovim

Neovim 0.11 and later configure language servers themselves. In
`init.lua`:

```lua
vim.filetype.add({ extension = { skn = "skein" } })
vim.lsp.config("skein", {
  cmd = { "skein", "lsp" },
  filetypes = { "skein" },
  root_markers = { "skein.edn", ".git" },
})
vim.lsp.enable("skein")
```

For highlighting, Clojure's syntax is close:
`vim.treesitter.language.register("clojure", "skein")`.

This setup and Helix's follow each editor's documentation, but Skein's
tests don't run them.

## Helix

In `languages.toml` (in `~/.config/helix/`):

```toml
[language-server.skein]
command = "skein"
args = ["lsp"]

[[language]]
name = "skein"
scope = "source.skn"
file-types = ["skn"]
roots = ["skein.edn"]
comment-token = ";"
indent = { tab-width = 2, unit = "  " }
language-servers = ["skein"]
grammar = "clojure"
```

## VS Code (and Antigravity, Cursor, VSCodium)

`editors/vscode` is the extension. Build it and install it:

```text
cd editors/vscode
npm install
npm run package                      # skein.vsix
code --install-extension skein.vsix   # or antigravity-ide, or Install from VSIX…
```

It needs `skein` on the PATH, or **skein.path** set to it. Open a file of a
project and it runs `skein lsp` there (in the nearest folder with
`skein.edn`): everything above, with **Format Document** for formatting.

- **Skein: Evaluate Top-Level Form or Selection** (Cmd+Enter on macOS,
  Ctrl+Enter elsewhere) evaluates the form around the cursor.
- **Skein: Start REPL** runs `skein --server` in a terminal in the project:
  the REPL, which you can type at too. From then on evaluations go to it,
  their values show beside the forms, and what they print goes to the
  **Skein Evaluation** output.
- **An error in the REPL's evaluations stops in VS Code's debugger.** The
  call stack is the fiber's frames, with each frame's locals. The Debug
  Console evaluates in the selected frame. **Restart Frame** calls the
  frame's function again, in its newest definition: fix it, evaluate the
  fix, then restart. **Continue** offers the condition's restarts, and
  **Stop** aborts the evaluation. Skein's debugger stops on conditions, not
  lines, so there are no breakpoints and no stepping, and a frame opens
  its function's definition rather than the line it stopped at.

`npm test` checks all of this in Antigravity IDE (or the editor
`EDITOR_BIN` names) on a test project. `crates/skein/tests/lsp.rs` runs it
with `SKEIN_VSCODE_TEST=1`. It opens a window for a few seconds.

## Zed

Zed starts a language server only through an extension. None exists yet.
One whose `language_server_command` returns `skein lsp`, with Clojure's
grammar, is enough.

## Checking it by hand

The protocol is plain enough to try without an editor. Each message is a
`Content-Length` header, a blank line, and JSON:

```text
Content-Length: 58

{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
```

`skein lsp` answers with its capabilities. `crates/skein/tests/lsp.rs` holds
a whole conversation, which is a good model for a new client.
