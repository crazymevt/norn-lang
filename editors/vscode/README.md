# Skein for VS Code

Skein in VS Code and editors built on
it (Antigravity, Cursor, VSCodium): highlighting, and what `skein lsp`
serves a project.

- **Diagnostics** as you type: `skein lint`'s warnings.
- **Hover:** a definition's arguments and docstring.
- **Completion** of the names the file's namespace can see.
- **Go to Definition** and **Find All References**, across the project
  and its packages.
- **Outline:** the file's definitions.
- **Format Document:** `skein fmt`'s layout.
- **Skein: Evaluate Top-Level Form** (Cmd+Enter on macOS, Ctrl+Enter
  elsewhere): the form around the cursor, its value shown.

## Setting up

The extension runs `skein lsp` in each Skein project you open a file of:
the nearest folder above the file with `skein.edn`. `skein` must be on the
PATH, or set **skein.path** to the binary.

**Skein: Show Output** shows what the project's code prints, and with
**skein.trace.server** set, the messages between the editor and the server.
**Skein: Restart Language Server** starts the servers again.

## Building it

```
npm install
npm test           # runs the extension in Antigravity IDE on a test project
npm run package    # skein.vsix
```

Install the `.vsix` with the editor's **Extensions: Install from VSIX…**,
or `antigravity-ide --install-extension skein.vsix` (`code` for VS Code).

The grammar is derived from VS Code's Clojure grammar, converted from
[atom/language-clojure](https://github.com/atom/language-clojure) (MIT).
