# Plan: Norn becomes Skein

Drafted 2026-11-17. On 2026-11-06 another project began calling itself
Norn ([norn-lang/norn](https://github.com/norn-lang/norn), a coordination
language for people and AI agents). The repos went public on 2026-11-17,
so nothing outside them depends on the name yet. The language takes the
name Skein instead: "skein-lang" is free on GitHub, and `skein-lang`,
`skein-rt` and `skein-vm` are free on crates.io (`skein` itself is the hash
function's). Other projects share the word Skein (the hash function, and
kormie/Skein, an AI-agent language at an early stage), but none is an
established language. The user deleted the earlier Skein's repos, so
every `skein-*` name under crazymevt is free.

**When:** once [crazymevt/norn-lang#2](https://github.com/crazymevt/norn-lang/pull/2)
(x86-64 Linux) has merged, with nothing else in flight. The rename touches
almost every file, so any open branch would conflict with it.

## Decisions (the user, 2026-11-17)

1. **Source files are `.skn`.** The project file `norn.edn` becomes
   `skein.edn`.
2. **The repos are renamed in place, under crazymevt:** norn-lang →
   skein-lang, norn-html → skein-html, norn-ui → skein-ui, norn-raylib →
   skein-raylib. GitHub redirects the old URLs, and issues, pull requests
   and history carry over.
3. **A clean break.** Skein knows only its own names: `.skn`, `skein.*`,
   `SKEIN_*` and `skein.edn`. It doesn't read `.norn`, `norn.edn` or
   `NORN_*`, and has no deprecation path.

## What the name is in (measured 2026-11-17)

| Repo | `.norn` files | Lines naming norn | Where it lives |
| --- | --- | --- | --- |
| norn-lang | 177 | 5,865 | GitHub |
| norn-studio | 45 | 425 | local only |
| norn-libs/norn-ui | 19 | 302 | GitHub |
| norn-libs/norn-raylib | 8 | 91 | GitHub |
| norn-libs/norn-sdl3 | 6 | 67 | local only |
| norn-libs/norn-html | 8 | 50 | GitHub |
| mandelbrot | 5 | 44 | local only |
| cats | 6 | 25 | local only |
| rouge-demo | 6 | 24 | local only |

Within norn-lang, the name appears in:
- **Rust:** the crates (`norn`, `norn-rt`, `norn-vm`, and `jit-spike`'s
  dependencies), `norn_rt::`/`norn_vm::` paths, types (`Norn`,
  `NornSlot`, …), the binary `norn`, `CARGO_BIN_EXE_norn`.
- **The language:** 75 `norn.*` namespaces (`norn.core` and the whole
  standard library), `%`-primitives that name it, the EDN tags
  `#norn/decimal`, `#norn/date`, `#norn/time`, `#norn/date-time` and
  `#norn/zoned`, and the `api/` records of every sealed module.
- **The environment:** 36 `NORN_*` variables (`NORN_TIER`, `NORN_PATH`,
  `NORN_CACHE`, `NORN_HEAP_*`, `NORN_JIT_*`, …), the module cache folder,
  and the image format: its magic `b"NORNIMG\0"` and the `.nimg`
  extension.
- **The tools:** the VS Code extension (its id, language id,
  `source.norn` grammar scope, settings `norn.path`, the `.vsix` name),
  `norn lsp`, and the CLI's usage text.
- **Prose:** the README, docs/ (the book, *The Norn Book*, the reference,
  the internals and plans), the lang-tests' READMEs.

## How

Every word containing "norn" is the name or a compound of it (`norn_rt`,
`NORN_TIER`, `NornSlot`): no other English word contains it. So the
rewrite is mechanical and keeps case: `norn` → `skein`, `Norn` → `Skein`,
`NORN` → `SKEIN`, in file contents and in file and folder names. These
need choices of their own:

| Today | Becomes | Why |
| --- | --- | --- |
| `.norn` | `.skn` | Decision 1 |
| `norn.edn` | `skein.edn` | Decision 1 |
| `.nimg` | `.skimg` | `.simg` is Singularity's container format |
| `b"NORNIMG\0"` | `b"SKEINIMG"` | still 8 bytes; old images are refused, as any other format change |
| `#norn/decimal` … | `#skein/decimal` … | Saved data that uses the old tags won't read, by decision 3 |
| *The Norn Book* | *The Skein Book* | |

The plans and the todo list are rewritten too, with one line in the
README and the book's introduction: Skein was called Norn until
2026-11-17. The predecessor called Skein (the earlier implementation,
whose repos are deleted) is history the plans already describe.

### Steps

1. **skein-lang (norn-lang), on a branch.** Run the rewrite script, rename
   the files and folders, then fix by hand what it can't decide (the image
   extension, the tags in recorded data, the README's history line).
   Check:
   - `cargo build` with no warnings, and `cargo test`;
   - every suite at tier 0 and tier 1, and with everything compiled from
     its first call;
   - `skein check`;
   - `skein api-diff --record`: every module's name changed, so this is a
     new baseline, not a diff;
   - `lang-tests/run.skn --record`, then a diff of `expected/` that shows
     only the name changing;
   - the VS Code extension's test, and a rebuilt `.vsix`;
   - `tools/check-linux.sh` in Colima.
2. **The libraries:** skein-html, skein-ui, skein-raylib and skein-sdl3 the
   same way, each on its own branch.
   - skein-html gets a new tag (v0.3.0: its namespaces changed).
   - ledger and quire in lang-tests move to `skein-html` at that tag, and
     their recordings are taken again.
   - skein-ui's Rust host crate (`norn-ui-host`) is renamed too.
3. **GitHub:** `gh repo rename` for the four repos, new descriptions,
   updated `origin` remotes. Then push skein-lang through a PR, so CI runs
   on the renamed tree on both platforms before it merges.
4. **The local-only projects:** norn-studio → skein-studio, and cats,
   mandelbrot and rouge-demo, each renamed and run.
5. **The folders:**
   - On the Mac: `~/projects/norn` → `~/projects/skein`,
     `norn-libs` → `skein-libs`, and `norn-lang` → `skein-lang`.
   - On the Fedora laptop: `~/Projects/norn-lang`, with its remote updated.
   - The app's presets `norn` and `norn-ssh`, and Claude's memory for this
     project, which is filed under the old folder's path, follow the
     folders.

**Done when:**
- nothing in any of these repos names Norn, except the README's history
  line;
- every check above passes on macOS, Linux arm64 (CI) and the Fedora
  laptop (x86-64);
- the GitHub repos have their new names, with the old URLs redirecting.
