# Norn for VS Code

Norn in VS Code and editors built on
it (Antigravity, Cursor, VSCodium): highlighting, and what `norn lsp`
serves a project.

- **Diagnostics** as you type: `norn lint`'s warnings.
- **Hover:** a definition's arguments and docstring.
- **Completion** of the names the file's namespace can see.
- **Go to Definition** and **Find All References**, across the project
  and its packages.
- **Outline:** the file's definitions.
- **Format Document:** `norn fmt`'s layout.
- **Norn: Evaluate Top-Level Form** (Cmd+Enter on macOS, Ctrl+Enter
  elsewhere): the form around the cursor, its value shown.

## Setting up

The extension runs `norn lsp` in each Norn project you open a file of:
the nearest folder above the file with `norn.edn`. `norn` must be on the
PATH, or set **norn.path** to the binary.

**Norn: Show Output** shows what the project's code prints, and with
**norn.trace.server** set, the messages between the editor and the server.
**Norn: Restart Language Server** starts the servers again.

## Building it

```
npm install
npm test           # runs the extension in Antigravity IDE on a test project
npm run package    # norn.vsix
```

Install the `.vsix` with the editor's **Extensions: Install from VSIX…**,
or `antigravity-ide --install-extension norn.vsix` (`code` for VS Code).

The grammar is derived from VS Code's Clojure grammar, converted from
[atom/language-clojure](https://github.com/atom/language-clojure) (MIT).
