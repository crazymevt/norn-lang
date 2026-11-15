# Command-line tools

[Documentation home](README.md)

The examples use `norn` on your `PATH`. From the repository root, you can
also substitute `./target/release/norn` after [building it](getting-started.md#build-and-start-the-repl).

| Command | Behavior |
| --- | --- |
| `norn` | Start the REPL in `user`. |
| `norn -e 'EXPR'` | Evaluate forms and print the last result. |
| `norn FILE.norn [ARGS…]` | Run a source file. `norn.sys/program-args` returns the arguments after the file, and `norn.sys/args` returns the full process argument vector, starting with the program name. Use [`norn.cli`](reference/libraries.md#command-line-programs) to parse them. |
| `norn --server [PORT]` | Start the REPL and listen on loopback for [editor protocol](ui-protocol.md) clients. The default port is 7888. |
| `norn doc NAME…` | List a namespace, look up a qualified symbol, or look up an unqualified name in `norn.core`. |
| `norn expand --steps -e 'EXPR'` | Show macro expansion steps. This command also accepts a source filename in place of `-e 'EXPR'`. |
| `norn fmt [PATH…]` | Format source files. Add `--check` to report files that need changes. |
| `norn lint [PATH…]` | Report source diagnostics, including unresolved names and unused imports. |
| `norn test [PATH…]` | Discover and run tests. Options include `--only REGEX`, `--serial`, `--tap`, and `--junit FILE`. |
| `norn check [PATH…]` | Run formatting checks, lint, tests, and examples registered in the metadata of loaded definitions. |
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

Put `norn.edn` at the project root. `:paths` defaults to `["src"]`, and
`:main` names the entry function for `norn build` when no source files are
given. For example, `app.main` goes in `src/app/main.norn`.

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

## Images

An image saves a Norn runtime's heap. You can save a development session with
`(save-image "dev.nimg")` and start it again with `norn --image dev.nimg`.
Use `norn image info dev.nimg` to inspect it and `norn image diff A.nimg B.nimg`
to compare two images. Native handles such as open files and sockets need to
be reopened or reconnected after restore.

Build a runnable image from a project with `norn build app.nimg`, or specify
a source file and entry function explicitly.

```sh
norn build app.nimg src/app/main.norn --entry app.main/main
norn --image app.nimg arg1 arg2
```

An entry function receives one string argument per command-line argument after
the image path. An integer return value supplies the process exit status,
clamped to 0–255. Other successful return values give exit status 0.

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

