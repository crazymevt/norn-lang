# 8. Modules and projects

[Contents](README.md) · Previous: [7. Text](07-text.md) · Next: [9. Classes and generic functions](09-classes-and-generic-functions.md) <!-- nav -->

A program of any size is split across files. This chapter covers how Skein
organizes code: namespaces, the modules that define them, `require` and
its options, private definitions, reloading a module while the program
runs, where Skein looks for modules, and projects, which describe a
program's source folders and the packages it depends on in a `skein.edn`
file.

## Namespaces

Every definition lives in a *namespace*. A namespace maps names to vars,
and a var holds a value: a function, a constant, a class. The REPL starts
in the namespace `user`, and `current-ns` tells you where you are.

```clojure
(current-ns) ; => user
```

A symbol with a slash, such as `skein.string/upper-case`, is
*qualified*: the part before the slash names a namespace and the part
after it names a var there. An unqualified symbol is looked up in the
current namespace, then in `skein.core`, whose names every namespace can
use without asking.

```clojure
(skein.string/upper-case "hi") ; => "HI"
(namespace 'skein.string/upper-case) ; => "skein.string"
(name 'skein.string/upper-case) ; => "upper-case"
```

The qualified call works only because `skein.string` is already loaded.
This chapter's checks run as one script whose first line loads it. In a
fresh REPL, you load a library with `require` before using it, as the
rest of this chapter shows.

Namespace names are dotted, from the general to the specific:
`skein.string`, `shop.money`, `my-app.text-utils`. Unlike Common Lisp's
packages, Skein's namespaces hold vars, not symbols. A symbol is a plain
name, and two namespaces can each define `total` without any conflict.

## Modules

A *module* is a source file that begins with an `ns` form. One file is one
module is one namespace. The `ns` form names the namespace and declares
what the module needs:

<!-- skip -->
```clojure
(ns shop.money
  (:require [skein.string :as str]
            [shop.tax :refer [rate]]))
```

Skein finds a module from its name: `shop.money` lives in `shop/money.skn`
under one of the folders Skein searches, and `my-app.text-utils` in
`my-app/text-utils.skn`. Dots become folder separators, and the rest of
the name, hyphens included, is the file name as written. The file's `ns`
form must name the same module: if `shop/money.skn` says `(ns
shop.cash)`, requiring `shop.money` is an error that says which namespace
the file defines.

A module compiles as a unit. Because Skein reads the whole file before it
runs the definitions, a function can call another that appears later in
the file. A script, a file without `ns`, runs one form at a time, top to
bottom, so in a script (or at the REPL) a function can't mention a name
that doesn't exist yet. Here is the script behavior. The error happens
when the form is compiled, so to catch it, the example hands the form to
`eval`.

```clojure
(eval '(defn early [] (not-defined-yet))) ; !! skein.compiler/CompileError
```

`declare` tells the compiler that a name will be defined later. Use it
in a script for the rare pair of functions that call each other. In a
module you don't need it.

```clojure
(declare later)
(defn early [] (later))
(defn later [] :done)
(early) ; => :done
```

### Writing a module

The examples in this chapter need module files on disk. In your own work
you'd create them in an editor. Here, a small helper writes each file from
quoted forms, so the code is shown as code and the checks can run it. The
helper prints each form with `pr-str` and puts a blank line between them.
Quoting drops comments and metadata such as `^:private`, so the examples
avoid them.

```clojure
(require '[skein.io :as io] '[skein.string :as str])

(defn write-module! [path & forms]
  (io/mkdirs! (io/parent path))
  (io/write-text path (str (str/join "\n\n" (map pr-str forms)) "\n")))
```

Now the first module, `shop.money`, in `lib/shop/money.skn`. It formats
prices kept as whole cents. `total` calls `cents->text`, which the file
defines after it.

```clojure
(write-module! "lib/shop/money.skn"
  '(ns shop.money
     "Prices, kept as whole cents."
     (:require [skein.string :as str]))

  '(println "loading shop.money")

  '(defn total
     "The sum of some prices in cents, as text."
     [prices]
     (cents->text (reduce + 0 prices)))

  '(defn- cents->text [cents]
     (format "$%d.%02d" (quot cents 100) (rem cents 100))))
```

The string after the module's name is its docstring, which says what the
module is for. It's optional, as a function's is, and `skein doc
shop.money` prints it above the module's definitions.

The `lib` folder isn't one Skein searches yet. `add-lib-path!` adds it.
(You'll see the other places Skein looks in
[Where Skein looks for modules](#where-skein-looks-for-modules).)

```clojure
(add-lib-path! "lib")
```

## Requiring modules

`require` loads a module and runs its top-level forms, once. At the REPL
or in a script, you quote its argument, because `require` is a function
and the spec is data. In an `ns` form, the specs go unquoted in a
`(:require …)` clause and mean the same thing.

```clojure
(require '[shop.money :as money])
; prints: loading shop.money

(money/total [199 250]) ; => "$4.49"
```

The spec `[shop.money :as money]` loads the module and gives it an
*alias*: `money/total` now means `shop.money/total` in this namespace. A
second `require` of a loaded module does nothing, so it doesn't print the
loading message again.

```clojure
(require '[shop.money :as money])
; prints:
```

`require` loads each module at most once even when several fibers ask
for it at the same moment: one loads it and the others wait. Two modules
that require each other are an error, reported as a circular require.

### :as and :refer

A spec is a vector holding a namespace name and options. `:as` adds an
alias. `:refer` brings named public vars into the current namespace, so
you can use them without a prefix. `:refer :all` brings in every public
var.

```clojure
(require '[shop.money :refer [total]])

(total [1000 1]) ; => "$10.01"
```

Prefer `:as`. A prefix at every call site tells the reader where a
function comes from, and short aliases keep the code compact. Save
`:refer` for a few names used constantly, like `deftest` and `is` in a
test file, and use `:refer :all` sparingly: it makes a module's names
depend on everything another module happens to export.

Referring to a name that isn't public is an error, as is requiring a
module Skein can't find.

```clojure
(require '[shop.money :refer [no-such-thing]]) ; !! Error
(require 'shop.no-such-module) ; !! Error
```

A plain symbol with no vector, as in `(require 'skein.sys)`, loads the
module without an alias. You can still use its fully qualified names.
`require` takes any number of specs, so one call can load several
modules.

```clojure
(require 'skein.sys '[skein.format :as fmt])

(fmt/ordinal 3) ; => "3rd"
```

### Looking inside namespaces

`ns-publics` returns a map from name to var of a namespace's public
definitions, and `ns-interns` includes the private ones. `ns-aliases`
shows the aliases a namespace has set up. These are maps, so sort their
keys when you want a stable order.

```clojure
(sort (keys (ns-publics 'shop.money))) ; => (total)
(sort (keys (ns-interns 'shop.money))) ; => (cents->text total)
(get (ns-aliases 'user) 'money) ; => shop.money
```

`resolve` tells you which var a symbol means in the current namespace.

```clojure
(resolve 'money/total) ; => #'shop.money/total
(resolve 'total) ; => #'shop.money/total
(resolve 'nothing-here) ; => nil
```

## Private definitions

`defn-` defines a private function. `(def ^:private name value)` defines
a private var of any kind. A private definition is available by its plain
name everywhere inside its own module, and nowhere else.

`cents->text` is private to `shop.money`, so calling it from `user` is an
error. Like the forward reference earlier, the check happens when the call
is compiled.

```clojure
(handler-case (eval '(money/cents->text 5))
  (Error [e] (condition-message e)))
; => "shop.money/cents->text is private to shop.money"
```

Privacy is about the interface, not security. When you need to reach a
private function, from a test for example, go through its var: `#'name` is
shorthand for `(var name)`, and calling a var calls its value.

```clojure
(#'shop.money/cents->text 5) ; => "$0.05"
(private? #'shop.money/cents->text) ; => true
```

Keep the public surface of a module small. Everything public is something
other modules may come to rely on, so make helpers private by default and
make a function public when you mean it to be used.

## Reloading a module

During development you change a module and want the running program to
pick up the change without a restart. `:reload` loads a module again even
if it's already loaded. Its definitions are replaced in place: every var
keeps its identity and gets the new value, so code that already calls the
function gets the new version on its next call.

Here is a function in `user` that uses `money/total`:

```clojure
(defn checkout [cart] (str "Total: " (money/total cart)))

(checkout [250 250]) ; => "Total: $5.00"
```

Now change `shop.money` so that totals come out in cents, and drop the
private helper:

```clojure
(write-module! "lib/shop/money.skn"
  '(ns shop.money)

  '(defn total [prices]
     (str (reduce + 0 prices) " cents")))

(require '[shop.money :reload])

(checkout [250 250]) ; => "Total: 500 cents"
(total [1 2]) ; => "3 cents"
```

`checkout` wasn't touched, and neither was the name `total` that `user`
referred earlier. Both see the new definition because they go through the
var.

Names the new source no longer defines are removed from the namespace.
Code already compiled against such a name keeps calling the old function,
but new code can't refer to it.

```clojure
(sort (keys (ns-interns 'shop.money))) ; => (total)
```

`:reload` can also follow all the specs, `(require 'a 'b :reload)`, to
reload each of them. In a project, `skein dev` reloads each module as you
save it, while the program runs. [Chapter 12](12-live-programming.md)
shows it, and how reloading fits into the REPL workflow, along with
redefining single functions from an editor.

## Names from skein.core

Every module can use the names in `skein.core` without requiring it. When
a module wants one of those names for itself, it says so with
`(:refer-core …)` in its `ns` form. `:exclude` gives up core names, so the
module can define its own. `:rename` keeps a core function under another
name.

```clojure
(write-module! "lib/shop/catalog.skn"
  '(ns shop.catalog
     (:refer-core :exclude [get] :rename {get core-get}))

  '(def items {:apple 120 :pear 95})

  '(defn get
     "The price of item, or 0 for one we don't sell."
     [item]
     (core-get items item 0)))

(require '[shop.catalog :as catalog])

(catalog/get :pear) ; => 95
(catalog/get :plum) ; => 0
```

Outside `shop.catalog`, `get` still means `skein.core/get`. The rename
affects only the module that asks for it.

A module can also define a core name without the `:refer-core` clause.
The definition then applies everywhere in the module, including uses
written before it. `skein lint` warns about this, since it is usually an
accident. Say what you mean with `(:refer-core :exclude [name])`.

## Where Skein looks for modules

When you require `shop.money`, Skein looks for it in this order:

1. The libraries built into the `skein` binary: `skein.core` and the
   `skein.*` standard libraries. A file of yours can't take the place of
   one of them.
2. The folders added with `add-lib-path!`, in the order they were added.
   When you run inside a project (below), Skein adds the project's source
   folders and its packages' folders here before your code runs.
3. The folders listed in the `SKEIN_PATH` environment variable, separated
   by colons.
4. The current directory.

The first match wins. If there is none, `require` signals an error that
names the module.

`SKEIN_PATH` is a quick way to share a folder of modules between scripts
without setting up a project:

```sh
SKEIN_PATH=$HOME/skein-lib:/opt/shared/skein skein report.skn
```

A file's location has to match the namespace its `ns` form names. Skein
finds `shop/money.skn` because you asked for `shop.money`, and the file
should then define `shop.money`. Keep the two in step, or a `require`
loads the file without defining the namespace you asked for.

Skein's own libraries come first, so a module of yours named as one of
them, say `skein.csv`, is never loaded. Names under `skein.` belong to the
Skein project: its standard library and its first-party packages, such as
`skein.ui` and `skein.html`. Name your modules after your project, as
`shop.money` is. `skein lint` warns about a module named under `skein.`
(`reserved`) or named as a library that comes with Skein (`shadowed`).

## Projects

A project is a folder with a `skein.edn` file. The file is EDN, Skein's
data syntax, holding a map. Here is a small project, `tally`, that counts
the words in text files. `skein new tally` makes this layout, with a `main`
and a test to start from, a README, a `.gitignore`, and a Git repository:

```text
tally/
  skein.edn
  src/
    tally/
      main.skn
  tests/
    main_test.skn
```

```edn
{:name tally
 :version "0.1.0"
 :paths ["src"]
 :main tally.main/main}
```

- `:name` and `:version` identify the project, and show up in `skein deps`
  output and in packaged releases.
- `:paths` lists the source folders, relative to `skein.edn`. It defaults
  to `["src"]`, so you can leave it out when that's your layout.
- `:test-paths` lists the test folders, and defaults to `["tests"]`. They
  are on the library path too, after everything else, so test files can
  share helper modules ([Chapter 25](25-testing.md)). A package's test
  folders aren't.
- `:main` names the entry function, as a fully qualified symbol. `skein
  build` and `skein package` start the program there.
- `:deps` and `:overrides`, described below, declare the packages the
  project uses.

`src/tally/main.skn` holds the `tally.main` module:

```clojure
(ns tally.main
  (:require [skein.io :as io]
            [skein.string :as str]))

(defn word-count [text]
  (count (remove str/blank? (str/split text #"\s+"))))

(defn main [& paths]
  (doseq [p paths]
    (println p (word-count (io/read-text p))))
  0)
```

Whenever `skein` starts, it looks for a project: a `skein.edn` in the folder
of the file it runs (or, with no file, the current folder), or in the
nearest folder above it that has one. When it finds one, it puts the project's `:paths` on the library
search path before running anything. So from anywhere inside `tally/`,
the REPL, `skein -e`, and any script can require `tally.main` without
`add-lib-path!`. `skein run` calls `:main` with the arguments you give it:

```sh
cd tally
skein run notes.txt
skein -e '(require (quote tally.main)) (tally.main/main "notes.txt")'
skein build tally.skimg
skein --image tally.skimg notes.txt
```

`main` returns 0, which becomes the program's exit status when it runs
with `skein run` or from an image. [Chapter 27](27-images-and-deployment.md) covers
`skein build`, images, and `skein package` in detail.

Tests go in a `tests` folder, in files whose names end in `_test.skn`.
`skein test` with no arguments runs that folder.
[Chapter 25](25-testing.md) covers testing.

The `tally.main` block above ran as part of this chapter's checks. In a
script, an `ns` form switches to the namespace it names, so switch back
to `user` before going on.

```clojure
(in-ns 'user)
```

## Dependencies

A project lists the packages it uses under `:deps`. A *package* is
another project used as a dependency. `skein new textkit --lib` starts
one: a `skein.edn` with no `:main`, the module `textkit` in
`src/textkit.skn`, and a test of it. Its own `skein.edn` gives its source
paths and its own dependencies. (A folder without a `skein.edn` counts as a
package whose source is in `src`.) Each dependency is either a folder or a
Git repository:

```edn
{:name tally
 :version "0.1.0"
 :deps {textkit {:path "../textkit"}
        colors {:git "https://example.com/colors.git" :tag "v1.2.0"}
        parsers {:git "https://example.com/parsers.git"
                 :sha "4f1c2b9e0d7a6c5b4a3f2e1d0c9b8a7f6e5d4c3b"}}
 :main tally.main/main}
```

- `{:path "folder"}` uses a folder on disk. The path is relative to the
  `skein.edn` that names it. This is the way to work on two projects side
  by side.
- `{:git "url" :tag "tag"}` uses a Git repository at a tag.
- `{:git "url" :sha "commit"}` uses a specific commit.

Give exactly one of `:tag` and `:sha` for a Git dependency. Skein checks
`skein.edn` when it reads it and explains what's wrong with a malformed
entry.

`skein deps` resolves the dependencies, fetches any Git packages it
doesn't have yet (with the `git` command), and lists what it found, with
the chain of packages that brought each one in:

```text
$ skein deps
tally 0.1.0 (/home/you/code/tally)
  textkit 0.2.0: path /home/you/code/textkit
  colors 1.2.0: git https://example.com/colors.git v1.2.0 (8819e44747c0), for tally → textkit
Wrote skein.lock.
```

Here `colors` came in through `textkit`, which depends on it. You don't
have to run `skein deps` before running a program: any `skein` started in a
project resolves its dependencies first and puts every package's source
paths on the search path after the project's own. `skein deps` is how you
see the result, and how you fetch packages ahead of time.

Git packages are cached in a folder of their own, one folder per package
and commit: `SKEIN_DEPS` if you set it, else a `deps` folder in Skein's
cache folder (`~/Library/Caches/skein` on macOS, `~/.cache/skein`
elsewhere).

### skein.lock

A tag can move: someone can delete it and push it again on a different
commit. So that a build doesn't change under you, `skein deps` writes the
commit of every Git package to `skein.lock`, beside `skein.edn`:

```edn
;; Written by skein: the commit of each Git package in the build (see skein deps).
{:git {colors {:git "https://example.com/colors.git", :sha "8819e44747c0daf7377e339a6b23f0db8ed4e217", :tag "v1.2.0"}}}
```

From then on, a locked package is used at its recorded commit, and only a
new or changed dependency is looked up again. Commit `skein.lock` with the
project so everyone builds the same code. To move to whatever the tags
point at now, run `skein deps --update`, which looks every Git package up
again and rewrites the lock file.

### Two versions of one package

Skein uses one copy of each package in a build. If two paths through the
dependencies ask for different versions of the same package, two folders,
or two commits, resolution stops and shows both chains:

```text
Two versions of colors in the build:
  git https://example.com/colors.git v1.2.0 (8819e44747c0), by tally → colors
  path /home/you/code/colors, by tally → textkit → colors
Add colors to :overrides in /home/you/code/tally/skein.edn to choose one.
```

Settle it in the project's `skein.edn`. An entry in `:overrides` is used
wherever the package is asked for, whatever version the asker named:

```edn
{:name tally
 :deps {textkit {:path "../textkit"}
        colors {:git "https://example.com/colors.git" :tag "v1.2.0"}}
 :overrides {colors {:git "https://example.com/colors.git" :tag "v1.2.0"}}}
```

Only the project's own `:overrides` count. A package's overrides don't
apply when it's used as a dependency, so the project at the top always has
the last word.

## Compiled modules are cached

Loading a module means reading, expanding, and compiling it. To make the
second load fast, Skein caches each module's compiled form in its cache
folder. The cache key covers the `skein` build, the module's source, and
the modules it requires, so a change anywhere recompiles what depends on
it, and you never need to clear the cache by hand. Set `SKEIN_CACHE` to
another folder to move it, or to `off` to turn it off. `skein build` always
compiles from source.

The language itself is cached there too. The first time a `skein` starts,
it boots the core library, the compiler and the expander from source, and
saves the result as a base image beside the compiled modules. Every start
after that loads the image instead, which is more than twice as fast. A
rebuilt `skein` makes its own image the next time it starts, and replaces
the old one. Set `SKEIN_BASE_IMAGE=off` to always boot from source.

## Other ns clauses

Two more clauses belong to later chapters:

- `(:require-for-syntax …)` loads a module for use at compile time, by
  macros. [Chapter 14](14-macros.md) explains phases.
- `(:unsafe)` lets a module use the raw foreign-function interface, which
  can crash the process if misused. [Chapter 28](28-foreign-functions.md)
  covers it.

## Summary

- A namespace maps names to vars. A module is a file that starts with
  `ns`, and a docstring after its name says what it's for; one file is
  one module is one namespace, and `a.b-c` lives in `a/b-c.skn`.
- A module compiles as a unit, so it can refer to definitions later in the
  file. Scripts and the REPL run one form at a time.
- `require` loads a module once. `:as` gives it an alias, `:refer` brings
  in chosen names, and `:reload` loads it again, replacing definitions in
  place so running code picks them up.
- `defn-` and `^:private` keep a definition inside its module. A var,
  `#'ns/name`, still reaches it when you need to.
- `(:refer-core :exclude […] :rename {…})` lets a module take a
  `skein.core` name for its own.
- Skein searches the built-in libraries, then `add-lib-path!` folders
  (including a project's paths and packages), then `SKEIN_PATH`, then the
  current directory.
- `skein new` makes a project, and `skein run` calls its `:main`.
- A project's `skein.edn` gives its `:paths`, `:test-paths`, `:main`, and
  `:deps`: folders
  or Git repositories at a tag or commit. `skein deps` resolves them and
  `skein.lock` pins each Git package to a commit; `:overrides` settles
  version conflicts.

---

[Contents](README.md) · Previous: [7. Text](07-text.md) · Next: [9. Classes and generic functions](09-classes-and-generic-functions.md) <!-- nav -->
