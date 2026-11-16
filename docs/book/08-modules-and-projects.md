# 8. Modules and projects

[Contents](README.md) · Previous: [7. Text](07-text.md) · Next: [9. Classes and generic functions](09-classes-and-generic-functions.md) <!-- nav -->

A program of any size is split across files. This chapter covers how Norn
organizes code: namespaces, the modules that define them, `require` and
its options, private definitions, reloading a module while the program
runs, where Norn looks for modules, and projects, which describe a
program's source folders and the packages it depends on in a `norn.edn`
file.

## Namespaces

Every definition lives in a *namespace*. A namespace maps names to vars,
and a var holds a value: a function, a constant, a class. The REPL starts
in the namespace `user`, and `current-ns` tells you where you are.

```clojure
(current-ns) ; => user
```

A symbol with a slash, such as `norn.string/upper-case`, is
*qualified*: the part before the slash names a namespace and the part
after it names a var there. An unqualified symbol is looked up in the
current namespace, then in `norn.core`, whose names every namespace can
use without asking.

```clojure
(norn.string/upper-case "hi") ; => "HI"
(namespace 'norn.string/upper-case) ; => "norn.string"
(name 'norn.string/upper-case) ; => "upper-case"
```

The qualified call works only because `norn.string` is already loaded.
This chapter's checks run as one script whose first line loads it. In a
fresh REPL, you load a library with `require` before using it, as the
rest of this chapter shows.

Namespace names are dotted, from the general to the specific:
`norn.string`, `shop.money`, `my-app.text-utils`. Unlike Common Lisp's
packages, Norn's namespaces hold vars, not symbols. A symbol is a plain
name, and two namespaces can each define `total` without any conflict.

## Modules

A *module* is a source file that begins with an `ns` form. One file is one
module is one namespace. The `ns` form names the namespace and declares
what the module needs:

<!-- skip -->
```clojure
(ns shop.money
  (:require [norn.string :as str]
            [shop.tax :refer [rate]]))
```

Norn finds a module from its name: `shop.money` lives in `shop/money.norn`
under one of the folders Norn searches, and `my-app.text-utils` in
`my-app/text-utils.norn`. Dots become folder separators, and the rest of
the name, hyphens included, is the file name as written. The file's `ns`
form must name the same module: if `shop/money.norn` says `(ns
shop.cash)`, requiring `shop.money` is an error that says which namespace
the file defines.

A module compiles as a unit. Because Norn reads the whole file before it
runs the definitions, a function can call another that appears later in
the file. A script, a file without `ns`, runs one form at a time, top to
bottom, so in a script (or at the REPL) a function can't mention a name
that doesn't exist yet. Here is the script behavior. The error happens
when the form is compiled, so to catch it, the example hands the form to
`eval`.

```clojure
(eval '(defn early [] (not-defined-yet))) ; !! norn.compiler/CompileError
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
(require '[norn.io :as io] '[norn.string :as str])

(defn write-module! [path & forms]
  (io/mkdirs! (io/parent path))
  (io/write-text path (str (str/join "\n\n" (map pr-str forms)) "\n")))
```

Now the first module, `shop.money`, in `lib/shop/money.norn`. It formats
prices kept as whole cents. `total` calls `cents->text`, which the file
defines after it.

```clojure
(write-module! "lib/shop/money.norn"
  '(ns shop.money
     "Prices, kept as whole cents."
     (:require [norn.string :as str]))

  '(println "loading shop.money")

  '(defn total
     "The sum of some prices in cents, as text."
     [prices]
     (cents->text (reduce + 0 prices)))

  '(defn- cents->text [cents]
     (format "$%d.%02d" (quot cents 100) (rem cents 100))))
```

The string after the module's name is its docstring, which says what the
module is for. It's optional, as a function's is, and `norn doc
shop.money` prints it above the module's definitions.

The `lib` folder isn't one Norn searches yet. `add-lib-path!` adds it.
(You'll see the other places Norn looks in
[Where Norn looks for modules](#where-norn-looks-for-modules).)

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
module Norn can't find.

```clojure
(require '[shop.money :refer [no-such-thing]]) ; !! Error
(require 'shop.no-such-module) ; !! Error
```

A plain symbol with no vector, as in `(require 'norn.sys)`, loads the
module without an alias. You can still use its fully qualified names.
`require` takes any number of specs, so one call can load several
modules.

```clojure
(require 'norn.sys '[norn.format :as fmt])

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
(write-module! "lib/shop/money.norn"
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
reload each of them. In a project, `norn dev` reloads each module as you
save it, while the program runs. [Chapter 12](12-live-programming.md)
shows it, and how reloading fits into the REPL workflow, along with
redefining single functions from an editor.

## Names from norn.core

Every module can use the names in `norn.core` without requiring it. When
a module wants one of those names for itself, it says so with
`(:refer-core …)` in its `ns` form. `:exclude` gives up core names, so the
module can define its own. `:rename` keeps a core function under another
name.

```clojure
(write-module! "lib/shop/catalog.norn"
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

Outside `shop.catalog`, `get` still means `norn.core/get`. The rename
affects only the module that asks for it.

A module can also define a core name without the `:refer-core` clause.
The definition then applies everywhere in the module, including uses
written before it. `norn lint` warns about this, since it is usually an
accident. Say what you mean with `(:refer-core :exclude [name])`.

## Where Norn looks for modules

When you require `shop.money`, Norn looks for it in this order:

1. The libraries built into the `norn` binary: `norn.core` and the
   `norn.*` standard libraries. A file of yours can't take the place of
   one of them.
2. The folders added with `add-lib-path!`, in the order they were added.
   When you run inside a project (below), Norn adds the project's source
   folders and its packages' folders here before your code runs.
3. The folders listed in the `NORN_PATH` environment variable, separated
   by colons.
4. The current directory.

The first match wins. If there is none, `require` signals an error that
names the module.

`NORN_PATH` is a quick way to share a folder of modules between scripts
without setting up a project:

```sh
NORN_PATH=$HOME/norn-lib:/opt/shared/norn norn report.norn
```

A file's location has to match the namespace its `ns` form names. Norn
finds `shop/money.norn` because you asked for `shop.money`, and the file
should then define `shop.money`. Keep the two in step, or a `require`
loads the file without defining the namespace you asked for.

## Projects

A project is a folder with a `norn.edn` file. The file is EDN, Norn's
data syntax, holding a map. Here is a small project, `tally`, that counts
the words in text files. `norn new tally` makes this layout, with a `main`
and a test to start from, a README, a `.gitignore`, and a Git repository:

```text
tally/
  norn.edn
  src/
    tally/
      main.norn
  tests/
    main_test.norn
```

```edn
{:name tally
 :version "0.1.0"
 :paths ["src"]
 :main tally.main/main}
```

- `:name` and `:version` identify the project, and show up in `norn deps`
  output and in packaged releases.
- `:paths` lists the source folders, relative to `norn.edn`. It defaults
  to `["src"]`, so you can leave it out when that's your layout.
- `:test-paths` lists the test folders, and defaults to `["tests"]`. They
  are on the library path too, after everything else, so test files can
  share helper modules ([Chapter 25](25-testing.md)). A package's test
  folders aren't.
- `:main` names the entry function, as a fully qualified symbol. `norn
  build` and `norn package` start the program there.
- `:deps` and `:overrides`, described below, declare the packages the
  project uses.

`src/tally/main.norn` holds the `tally.main` module:

```clojure
(ns tally.main
  (:require [norn.io :as io]
            [norn.string :as str]))

(defn word-count [text]
  (count (remove str/blank? (str/split text #"\s+"))))

(defn main [& paths]
  (doseq [p paths]
    (println p (word-count (io/read-text p))))
  0)
```

Whenever `norn` starts, it looks for a project: a `norn.edn` in the folder
of the file it runs (or, with no file, the current folder), or in the
nearest folder above it that has one. When it finds one, it puts the project's `:paths` on the library
search path before running anything. So from anywhere inside `tally/`,
the REPL, `norn -e`, and any script can require `tally.main` without
`add-lib-path!`. `norn run` calls `:main` with the arguments you give it:

```sh
cd tally
norn run notes.txt
norn -e '(require (quote tally.main)) (tally.main/main "notes.txt")'
norn build tally.nimg
norn --image tally.nimg notes.txt
```

`main` returns 0, which becomes the program's exit status when it runs
with `norn run` or from an image. [Chapter 27](27-images-and-deployment.md) covers
`norn build`, images, and `norn package` in detail.

Tests go in a `tests` folder, in files whose names end in `_test.norn`.
`norn test` with no arguments runs that folder.
[Chapter 25](25-testing.md) covers testing.

The `tally.main` block above ran as part of this chapter's checks. In a
script, an `ns` form switches to the namespace it names, so switch back
to `user` before going on.

```clojure
(in-ns 'user)
```

## Dependencies

A project lists the packages it uses under `:deps`. A *package* is
another project used as a dependency. Its own `norn.edn` gives its source
paths and its own dependencies. (A folder without a `norn.edn` counts as a
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
  `norn.edn` that names it. This is the way to work on two projects side
  by side.
- `{:git "url" :tag "tag"}` uses a Git repository at a tag.
- `{:git "url" :sha "commit"}` uses a specific commit.

Give exactly one of `:tag` and `:sha` for a Git dependency. Norn checks
`norn.edn` when it reads it and explains what's wrong with a malformed
entry.

`norn deps` resolves the dependencies, fetches any Git packages it
doesn't have yet (with the `git` command), and lists what it found, with
the chain of packages that brought each one in:

```text
$ norn deps
tally 0.1.0 (/home/you/code/tally)
  textkit 0.2.0: path /home/you/code/textkit
  colors 1.2.0: git https://example.com/colors.git v1.2.0 (8819e44747c0), for tally → textkit
Wrote norn.lock.
```

Here `colors` came in through `textkit`, which depends on it. You don't
have to run `norn deps` before running a program: any `norn` started in a
project resolves its dependencies first and puts every package's source
paths on the search path after the project's own. `norn deps` is how you
see the result, and how you fetch packages ahead of time.

Git packages are cached in a folder of their own, one folder per package
and commit: `NORN_DEPS` if you set it, else a `deps` folder in Norn's
cache folder (`~/Library/Caches/norn` on macOS, `~/.cache/norn`
elsewhere).

### norn.lock

A tag can move: someone can delete it and push it again on a different
commit. So that a build doesn't change under you, `norn deps` writes the
commit of every Git package to `norn.lock`, beside `norn.edn`:

```edn
;; Written by norn: the commit of each Git package in the build (see norn deps).
{:git {colors {:git "https://example.com/colors.git", :sha "8819e44747c0daf7377e339a6b23f0db8ed4e217", :tag "v1.2.0"}}}
```

From then on, a locked package is used at its recorded commit, and only a
new or changed dependency is looked up again. Commit `norn.lock` with the
project so everyone builds the same code. To move to whatever the tags
point at now, run `norn deps --update`, which looks every Git package up
again and rewrites the lock file.

### Two versions of one package

Norn uses one copy of each package in a build. If two paths through the
dependencies ask for different versions of the same package, two folders,
or two commits, resolution stops and shows both chains:

```text
Two versions of colors in the build:
  git https://example.com/colors.git v1.2.0 (8819e44747c0), by tally → colors
  path /home/you/code/colors, by tally → textkit → colors
Add colors to :overrides in /home/you/code/tally/norn.edn to choose one.
```

Settle it in the project's `norn.edn`. An entry in `:overrides` is used
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
second load fast, Norn caches each module's compiled form in its cache
folder. The cache key covers the `norn` build, the module's source, and
the modules it requires, so a change anywhere recompiles what depends on
it, and you never need to clear the cache by hand. Set `NORN_CACHE` to
another folder to move it, or to `off` to turn it off. `norn build` always
compiles from source.

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
  one module is one namespace, and `a.b-c` lives in `a/b-c.norn`.
- A module compiles as a unit, so it can refer to definitions later in the
  file. Scripts and the REPL run one form at a time.
- `require` loads a module once. `:as` gives it an alias, `:refer` brings
  in chosen names, and `:reload` loads it again, replacing definitions in
  place so running code picks them up.
- `defn-` and `^:private` keep a definition inside its module. A var,
  `#'ns/name`, still reaches it when you need to.
- `(:refer-core :exclude […] :rename {…})` lets a module take a
  `norn.core` name for its own.
- Norn searches the built-in libraries, then `add-lib-path!` folders
  (including a project's paths and packages), then `NORN_PATH`, then the
  current directory.
- `norn new` makes a project, and `norn run` calls its `:main`.
- A project's `norn.edn` gives its `:paths`, `:test-paths`, `:main`, and
  `:deps`: folders
  or Git repositories at a tag or commit. `norn deps` resolves them and
  `norn.lock` pins each Git package to a commit; `:overrides` settles
  version conflicts.

---

[Contents](README.md) · Previous: [7. Text](07-text.md) · Next: [9. Classes and generic functions](09-classes-and-generic-functions.md) <!-- nav -->
