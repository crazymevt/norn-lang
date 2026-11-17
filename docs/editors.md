# Editors

[Documentation home](README.md)

Norn talks to editors in two ways:

- **`norn --server`** speaks Norn's own editor protocol, which Norn Studio
  uses: evaluation with the debugger, the inspector, tests, and more
  ([Chapter 12](book/12-live-programming.md#editors-and-norn---server)).
- **`norn lsp`** speaks the Language Server Protocol, which most editors
  have a client for. This page sets it up.

## What `norn lsp` does

Run in a project, `norn lsp` is that project's Norn: its `:paths` and
packages are on the library path, as with `norn run`. It talks to the
editor on standard input and output, and gives it:

| Feature | What it shows |
| --- | --- |
| Diagnostics | `norn lint`'s warnings as you type: unresolved names, unused locals, wrong arities, and the rest. |
| Hover | A definition's arguments and docstring. |
| Completion | Names visible in the file's namespace. |
| Go to definition | Where a function, macro or var is defined, in the project or its packages. |
| References | Where each function that refers to a name does so (`norn.xref`'s callers). |
| Outline | The file's definitions (document symbols). |
| Formatting | `norn fmt`'s layout for the whole file. |
| `norn.eval` command | Evaluates the top-level form at a position and shows its value. |

A file's module is loaded when it's opened and loaded again when it's
saved, so hover, definition and references know what it defines. What its
code prints goes to standard error, which editors keep in their log for
the server. Definitions evaluated with `norn.eval` are an editor's
evaluations, so `unsaved-definitions` lists them until the file holds
them ([Chapter 12](book/12-live-programming.md#what-you-havent-saved)).

Every editor below needs to know two things: that `.norn` files are a
language of their own, and that `norn lsp` serves it. Start the editor in
the project folder, the one with `norn.edn`, or one under it.

## Emacs

Emacs 29 and later include eglot. In your init file:

```elisp
(define-derived-mode norn-mode lisp-data-mode "Norn")
(add-to-list 'auto-mode-alist '("\\.norn\\'" . norn-mode))
(with-eval-after-load 'eglot
  (add-to-list 'eglot-server-programs '(norn-mode . ("norn" "lsp"))))
(add-hook 'norn-mode-hook #'eglot-ensure)
```

`M-.` goes to a definition and `M-?` lists references. `eglot-format-buffer`
formats, and Flymake shows the diagnostics. This setup is the one Norn's
tests run (`tests/lsp/eglot-check.el`).

## Neovim

Neovim 0.11 and later configure language servers themselves. In
`init.lua`:

```lua
vim.filetype.add({ extension = { norn = "norn" } })
vim.lsp.config("norn", {
  cmd = { "norn", "lsp" },
  filetypes = { "norn" },
  root_markers = { "norn.edn", ".git" },
})
vim.lsp.enable("norn")
```

For highlighting, Clojure's syntax is close:
`vim.treesitter.language.register("clojure", "norn")`.

This setup and Helix's follow each editor's documentation, but Norn's
tests don't run them.

## Helix

In `languages.toml` (in `~/.config/helix/`):

```toml
[language-server.norn]
command = "norn"
args = ["lsp"]

[[language]]
name = "norn"
scope = "source.norn"
file-types = ["norn"]
roots = ["norn.edn"]
comment-token = ";"
indent = { tab-width = 2, unit = "  " }
language-servers = ["norn"]
grammar = "clojure"
```

## VS Code (and Antigravity, Cursor, VSCodium)

`editors/vscode` is the extension. Build it and install it:

```text
cd editors/vscode
npm install
npm run package                      # norn.vsix
code --install-extension norn.vsix   # or antigravity-ide, or Install from VSIX…
```

It needs `norn` on the PATH, or **norn.path** set to it. Open a file of a
project and it runs `norn lsp` there (in the nearest folder with
`norn.edn`): everything above, with **Format Document** for formatting.

- **Norn: Evaluate Top-Level Form or Selection** (Cmd+Enter on macOS,
  Ctrl+Enter elsewhere) evaluates the form around the cursor.
- **Norn: Start REPL** runs `norn --server` in a terminal in the project:
  the REPL, which you can type at too. From then on evaluations go to it,
  their values show beside the forms, and what they print goes to the
  **Norn Evaluation** output.
- **An error in the REPL's evaluations stops in VS Code's debugger.** The
  call stack is the fiber's frames, with each frame's locals. The Debug
  Console evaluates in the selected frame. **Restart Frame** calls the
  frame's function again, in its newest definition: fix it, evaluate the
  fix, then restart. **Continue** offers the condition's restarts, and
  **Stop** aborts the evaluation. Norn's debugger stops on conditions, not
  lines, so there are no breakpoints and no stepping, and a frame opens
  its function's definition rather than the line it stopped at.

`npm test` checks all of this in Antigravity IDE (or the editor
`EDITOR_BIN` names) on a test project. `crates/norn/tests/lsp.rs` runs it
with `NORN_VSCODE_TEST=1`. It opens a window for a few seconds.

## Zed

Zed starts a language server only through an extension. None exists yet.
One whose `language_server_command` returns `norn lsp`, with Clojure's
grammar, is enough.

## Checking it by hand

The protocol is plain enough to try without an editor. Each message is a
`Content-Length` header, a blank line, and JSON:

```text
Content-Length: 58

{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
```

`norn lsp` answers with its capabilities. `crates/norn/tests/lsp.rs` holds
a whole conversation, which is a good model for a new client.
