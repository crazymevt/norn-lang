# Command-line tools

[Documentation home](README.md)

The examples use `norn` on your `PATH`. From the repository root, you can
also substitute `./target/release/norn` after [building it](getting-started.md#build-and-start-the-repl).

| Command | Behavior |
| --- | --- |
| `norn` | Start the REPL in `user`. |
| `norn -e 'EXPR'` | Evaluate forms and print the last result. |
| `norn FILE.norn [ARGS…]` | Run a source file. `norn.sys/program-args` returns the arguments after the file, and `norn.sys/args` returns the full process argument vector, starting with the program name. Use [`norn.cli`](reference/libraries.md#command-line-programs) to parse them. |
| `norn --server [PORT]` | Start the REPL and listen on loopback for [editor protocol](design.md#51-the-repl-decided) clients. The default port is 7888. |
| `norn doc NAME…` | List a namespace, look up a qualified symbol, or look up an unqualified name in `norn.core`. |
| `norn expand --steps -e 'EXPR'` | Show macro expansion steps. This command also accepts a source filename in place of `-e 'EXPR'`. |
| `norn fmt [PATH…]` | Format source files. Add `--check` to report files that need changes. |
| `norn lint [PATH…]` | Report source diagnostics, including unresolved names and unused imports. |
| `norn test [PATH…]` | Discover and run tests. Options include `--only REGEX`, `--serial`, `--tap`, and `--junit FILE`. |
| `norn check [PATH…]` | Run formatting checks, lint, tests, and examples registered in the metadata of loaded definitions. |
| `norn new NAME [--dir DIR] [--app \| --lib] [--no-git]` | Make a new project, ready to run, test and package. See [Projects and dependencies](#projects-and-dependencies). |
| `norn run [ARGS…]` | Call the project's `:main` with the arguments. Its integer result is the exit status. |
| `norn dev [ARGS…]` | Run the project's `:main`, and reload each module that's saved while it runs. See [Live reloading](#live-reloading). |
| `norn api-diff [--record] [MODULE…]` | Compare each sealed module's public interface with the one recorded under `api/`: breaking changes, and compatible ones. `--record` records them. See [Sealed modules](#sealed-modules). |
| `norn package [--cli] [--app] [--out DIR]` | Package the project for release: a command-line archive and, on macOS, an app. See [Packaging](#packaging). |

## Write and run a test

Save this example as `greeting_test.norn`, then run
`norn test greeting_test.norn`. The runner recognizes files ending in
`_test.norn` when searching directories.

```clojure
(ns greeting-test
  (:require [norn.test :refer [deftest is testing]]))

(defn greeting [who]
  (str "Hello, " who "!"))

(deftest greets-the-caller
  (testing "includes the supplied name"
    (is (= "Hello, Ada!" (greeting "Ada")))))
```

### Properties

`norn.property` tests a claim against many generated values. A
`defproperty` is a test like `deftest`: it runs 100 cases, with sizes
growing from small to large. When a case fails, it shrinks the
arguments to the simplest failing ones and reports them with a seed.
Pass that seed in the options map to rerun the same cases.

```clojure
(ns codec-test
  (:require [norn.property :as p] [norn.edn :as edn]))

(p/defproperty edn-round-trips
  [x (p/vectors (p/one-of (p/integers) (p/strings) (p/keyword)))]
  (= x (edn/read-string (pr-str x))))

;; Options go before the bindings.
(p/defproperty short-vectors {:runs 20 :seed 9}
  [v (p/vectors (p/integers))]
  (< (count v) 3))
;; FAIL: falsified with {:v [0 0 0]} (seed 9, case 2; rerun with :seed 9)
```

A binding can use an earlier one, as in
`[n (p/naturals) v (p/vectors (p/integers) :max n)]`. A case also fails
when its body signals a condition. `p/check` runs a `p/for-all` property
and returns the result as a map, and `p/generate` draws one value.

Generators:

- Numbers and simple values: `integers` (`:min`, `:max`), `naturals`,
  `floats`, `booleans`, `chars`, `strings` (`:unicode`, `:min`, `:max`),
  `keyword`, `symbol`, `byte-strings`.
- Collections: `vectors`, `lists`, `sets`, `maps`, `tuple`, and
  `recursive` for trees.
- Choices: `constant`, `elements`, `one-of`, `frequency`.
- Combinators: `fmap`, `bind`, `such-that`.

Generated values shrink toward 0, empty collections, `false`, the first
choice, and `\a`.

### Executable examples

A definition can include executable examples in its attribute map. Each
pair contains an expression and an expected expression. `norn check`
evaluates both in the definition's namespace and compares them with `=`.
Markdown code fences are separate from this metadata.

```clojure
(defn twice
  "Return twice the input."
  {:examples [[(twice 3) 6]]}
  [n]
  (+ n n))
```

## Projects and dependencies

`norn new my-app` makes a project in a new folder `my-app`: a `norn.edn`, a
`main` in `src/my-app/main.norn`, a test in `tests/main_test.norn`, a
README and a `.gitignore`, and a Git repository unless the folder is
already in one (`--no-git` skips it). `--app` sets the project up to
package as a macOS app as well as a command-line archive. In the folder,
`norn run` calls `main`, `norn test` runs the test, and `norn package`
packages it. The name is lower-case words joined by hyphens. `--lib`
makes a library for other projects to use instead: no `:main`, the module
`NAME` in `src/NAME.norn`, and its test in `tests/NAME_test.norn`.

Put `norn.edn` at the project root. `:paths` defaults to `["src"]`, and
`:main` names the entry function for `norn build` when no source files are
given. For example, `app.main` goes in `src/app/main.norn`. `:test-paths`
defaults to `["tests"]`. Those folders are on the library path after the
rest, so test files can share helper modules (`tests/app/fixtures.norn` is
`app.fixtures`); a dependency's test folders aren't.

```edn
{:name sample-app
 :version "0.1.0"
 :paths ["src"]
 :main app.main/main}
```

Add dependencies under `:deps`. A dependency can point to a folder with
`{:path "../shared"}` or to a Git repository with `:git` and either
`:tag` or `:sha`. Paths are relative to the project file that declares them.
Run `norn deps` to resolve dependencies and write `norn.lock`, which pins Git
dependencies to commits. `norn deps --update` resolves them again.

## Live reloading

`norn dev` runs the project's `:main`, as `norn run` does, and watches the
project's `:paths`. Each time you save a module, it loads it again:

- Calls go through vars, so code that's already running uses the new
  definitions the next time it calls them. A loop already running keeps
  its own body until it ends.
- A module that defines macros reloads the modules that use it too, since
  they expanded the old macros.
- A file that doesn't read, such as one with a missing bracket, isn't
  loaded. The old code keeps running, and `norn dev` prints the error.
- Reloading a module evaluates its definitions again, so state defined
  with `def` starts over. Define it with `defonce` to keep it.
- `(norn.dev/after-reload! key f)` calls `(f modules)` after each reload,
  so a program can redraw.
- When `:main` has returned, the next save runs it again.
- Ctrl-C (or SIGTERM) stops it. A program that listens for the signal
  itself (`norn.signal`) gets it too, and `norn dev` waits for it to finish
  shutting down; one that doesn't is cancelled.

## Sealed modules

A sealed module's interface is fixed: `norn api-diff` records it, and the
optimising compiler (M10) will link and inline calls to it. The standard
library is sealed as it loads. `norn build`, and so `norn package`, seals
the modules it loaded before saving the image. The REPL, `norn run` and
`norn dev` leave a project's modules open.

- `(sealed? ns)`, `(seal! ns)` and `(unseal! ns)` ask and change it.
- Redefining a var of a sealed module, after `(in-ns 'norn.string)` say,
  unseals the module, with a warning the first time: sealing is
  reversible, so live programming goes on. `norn lint` reports such
  definitions as `sealed`.
- `norn api-diff --record` writes each module's interface to `api/NAME.edn`:
  a function's parameter lists and checked signature, a macro's or generic
  function's parameter lists, a class's superclasses and slots, and which
  vars are dynamic. `norn api-diff` compares the modules with those files.
  Additions are compatible; a name or an arity removed, a kind or a
  signature's types changed, a slot or superclass removed are breaking,
  and make it exit with status 1. The standard library's interfaces are
  recorded in `api/` at the root of the checkout, and `api_diff_test.norn`
  checks the library against them: a breaking change fails it until
  it's meant, recorded again and committed.
- In a project, the modules are the project's own (every module under its
  `:paths`) unless you name some; in the Norn checkout, the standard
  library's.

## Images

An image saves a Norn runtime's heap. You can save a development session with
`(save-image "dev.nimg")` and start it again with `norn --image dev.nimg`.
Use `norn image info dev.nimg` to inspect it and `norn image diff A.nimg B.nimg`
to compare two images. The diff lists the namespaces and public vars added,
removed or changed. It compares functions by their code and other values by
their printed form, but an atom only by being there: what an atom holds is
state, which changes as a program runs. Native handles such as open files and
sockets need to be reopened or reconnected after restore.

Build a runnable image from a project with `norn build app.nimg`, or specify
a source file and entry function explicitly.

```sh
norn build app.nimg src/app/main.norn --entry app.main/main
norn --image app.nimg arg1 arg2
```

An entry function receives one string argument per command-line argument after
the image path, including a leading `-e`. An integer return value supplies the
process exit status, clamped to 0–255. Other successful return values give
exit status 0. To evaluate an expression in an image that has an entry
function, put `-e` first: `norn -e '(count (all-ns))' --image app.nimg`. An
image without an entry function also takes `norn --image dev.nimg -e 'EXPR'`.

## Packaging

`norn package` turns a project into something you can give to someone who
doesn't have Norn. It builds the project's image, with `:main` as the entry
function, using the same `norn` you run it with. Then it puts the image, a
copy of that `norn` under the app's name, the app's resources, and any
native helpers its packages declare into the `dist/` folder.

- A command-line archive, `NAME-VERSION-OS-ARCH.tar.gz`, holds `bin/NAME`
  and `lib/NAME/`, which contains the image, the resources, and the helpers.
- A macOS app, `Display Name.app`, comes zipped for sharing. Its
  `Info.plist` and icon come from `norn.edn`.

The app's program is a copy of `norn` itself. When `norn` runs under another
name and finds an image of that name beside it, it runs that image. Every
argument goes to `:main`, including a leading `-e`, and the program runs in
the caller's folder. It finds the image through a symbolic link too, so
`bin/NAME` can be linked into a folder on `PATH`. Inside the app,
`norn.sys/app-resources` returns the folder that holds the image and the
resources. For apps that use Norn UI, `NORN_UI_HOST` points at the bundled
renderer.

`norn.edn` describes the package:

```clojure
{:name mandelbrot
 :version "0.1.0"
 :main mandelbrot.viewer/main
 :package {:id "dev.norn.mandelbrot"      ; the app's identifier (macOS)
           :display-name "Mandelbrot"     ; its name in Finder and the Dock
           :icon "icon.png"               ; a PNG (made into .icns), or .icns
           :resources ["assets"]          ; files and folders to include
           :targets [:app :cli]}}         ; what to make (default [:cli])
```

`--cli` and `--app` choose what to make instead of `:targets`, and `--out`
sets the output folder. A package that needs a native program declares it
under `:helpers`, with candidate paths in its folder. `{os}` and `{arch}`
in a path are filled in, and the first file that exists is bundled. Norn UI
declares its renderer this way:

```clojure
:helpers {"norn-ui-host" ["target/release/norn-ui-host" "bin/{os}-{arch}/norn-ui-host"]}
```

The app is signed ad hoc, which needs no Apple Developer account. It runs
on the Mac that built it. Another Mac blocks a downloaded copy the first
time it opens. To open it there, try to open the app, then go to System
Settings > Privacy & Security and choose **Open Anyway**. Alternatively,
run `xattr -dr com.apple.quarantine "Mandelbrot.app"` before opening it.
Public releases need a Developer ID signature and notarization so that
macOS opens them without asking.

