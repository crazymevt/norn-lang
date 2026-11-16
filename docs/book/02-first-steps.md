# 2. First steps

[Contents](README.md) · Previous: [1. Introduction](01-introduction.md) · Next: [3. Values and expressions](03-values-and-expressions.md) <!-- nav -->

This chapter gets Norn running and shows the three ways you'll use it: the
interactive REPL, one-off expressions from the shell, and source files. It
ends with a small program that reads a file, and with the commands that find
documentation for any function.

## Building Norn

Norn builds with a stable Rust toolchain. From the root of the repository:

```sh
cargo build --release -p norn --bin norn
```

This produces `target/release/norn`, a single executable. The standard
libraries are compiled into it, so you can copy it anywhere. The rest of the
book writes `norn` as if it were on your `PATH`. Either use the full path,
or link it into a folder that is on your `PATH`:

```sh
ln -s "$PWD/target/release/norn" ~/.local/bin/norn
```

Check that it works:

```sh
norn -e '(+ 1 2)'
```

```text
3
```

## The REPL

Run `norn` with no arguments to start the REPL (read-eval-print loop):

```text
Norn 0.0.1 (fibers on 10 carriers)
Ctrl-C interrupts a running evaluation; Ctrl-D exits.
user=>
```

The banner tells you how many carriers the scheduler started: one operating
system thread per core. `user=>` is the prompt. `user` is the current
namespace, the place where your definitions go.

Type an expression and press Return. The REPL reads it, evaluates it, and
prints the result:

```text
user=> (+ 2 3)
5
user=> (* (+ 2 3) 4)
20
user=> (str "Norn" " " "REPL")
"Norn REPL"
```

Every expression in Norn is either an atom, such as `42` or `"text"`, or a
list in parentheses. In a list, the first element says what to do and the
rest are its arguments, so `(+ 2 3)` calls `+` with `2` and `3`. Nesting
works the way you expect: the inner `(+ 2 3)` is evaluated first, and its
value becomes an argument of `*`.

```clojure
(+ 2 3) ; => 5
(* (+ 2 3) 4) ; => 20
(str "Norn" " " "REPL") ; => "Norn REPL"
```

Use `def` to give a value a name and `defn` to define a function:

```clojure
(def width 3)
(defn area [w h] (* w h))

(area width 4) ; => 12
```

At the REPL, `def` and `defn` print the var they defined, such as
`#'user/area`. A var is the named, global cell that holds a definition.
Because every call to `area` looks the function up through its var, you can
redefine `area` at any time and every caller uses the new version from its
next call on:

```clojure
(defn area [w h] (* w h 1.0))

(area width 4) ; => 12.0
```

This is the heart of working in Norn: you build a program up a definition
at a time, trying each piece as you go, without restarting anything.
[Chapter 12](12-live-programming.md) covers that way of working in depth,
including what happens when an evaluation fails. For now, know that an
error at the REPL opens the debugger with a `debug[N]>` prompt; type `:a`
to abort and get back to `user=>`.

Press Ctrl-C to interrupt an evaluation that's taking too long, and Ctrl-D
to leave the REPL.

## Evaluating from the shell

`norn -e` evaluates the forms you give it and prints the value of the last
one. It's handy for quick checks and in shell scripts:

```sh
norn -e '(reduce + (range 1000000))'
```

```text
499999500000
```

Forms that print do so as they run, before the final value:

```sh
norn -e '(println "computing") (* 6 7)'
```

```text
computing
42
```

## Running a file

Save this as `hello.norn`:

```clojure
(require '[norn.sys :as sys])

(defn greet [name]
  (str "Hello, " name "!"))

(doseq [name (sys/program-args)]
  (println (greet name)))
```

and run it with the arguments it should greet:

```sh
norn hello.norn Ada Grace
```

```text
Hello, Ada!
Hello, Grace!
```

A few things are new here. `require` loads a library, in this case
`norn.sys`, and `:as sys` lets you refer to its functions with the short
prefix `sys/`. `sys/program-args` returns the command-line arguments that
come after the file name, as a vector of strings. `doseq` runs its body
once for each item, binding `name` to each in turn.

The functions you use most, such as `str`, `println`, `map` and `reduce`,
live in `norn.core`, which every file can use without a `require`.

When something goes wrong, Norn prints the error with the file and line it
happened on, and exits with status 1:

```sh
norn -e '(/ 1 0)'
```

```text
Error: <string>:1: Divide by zero
  at <toplevel>
  ...
```

A file that doesn't parse reports where the reader gave up:

```text
Error: bad.norn:2: EOF while reading, expected )
```

## A first module

`hello.norn` is a script: its forms run from top to bottom. A file that
begins with an `ns` form is a *module* instead. A module names its
namespace, declares what it requires in one place, and can refer to
functions defined further down the file. Most programs are modules.

Here is a module that prints the five most common words in a text file.
Save it as `wc.norn`:

```clojure
(ns wc
  (:require [norn.io :as io]
            [norn.string :as str]
            [norn.sys :as sys]))

(defn word-counts [text]
  (->> (str/split (str/lower-case text) #"[^\p{L}']+")
       (remove str/blank?)
       frequencies
       (sort-by (fn [[word n]] [(- n) word]))
       (take 5)))

(defn report [path]
  (doseq [[word n] (word-counts (io/read-text path))]
    (println (str/pad-left (str n) 4) word)))
```

Try it on some text, here written to a file first:

```clojure
(io/write-text "story.txt"
               (str "The cat sat on the mat.\n"
                    "The dog sat on the cat.\n"
                    "A bird watched the dog.\n"))

(report "story.txt")
; prints:    5 the
; prints:    2 cat
; prints:    2 dog
; prints:    2 on
; prints:    2 sat
```

Read `word-counts` from top to bottom. The `->>` macro threads a value
through a series of steps, passing each result as the last argument of the
next step, so it reads as a pipeline:

1. `str/lower-case` the text, then `str/split` it on every run of
   characters that aren't letters or apostrophes. `#"…"` is a regular
   expression literal.
2. `remove` any blank strings the split left behind.
3. `frequencies` counts how often each word occurs, giving a map from word
   to count.
4. `sort-by` orders the map's entries by a key computed for each one. The
   key `[(- n) word]` sorts by count, highest first, then alphabetically.
5. `take 5` keeps the first five entries.

In `report`, `doseq` destructures each `[word n]` pair as it binds it. You'll
see much more of `->>`, destructuring and sequence functions in Chapters
[4](04-functions-and-control-flow.md) and
[6](06-sequences-and-transducers.md).

To make it a program, add a last line that calls `report` with the first
command-line argument:

<!-- skip -->
```clojure
(report (first (sys/program-args)))
```

Run it with `norn wc.norn story.txt`. A module can also be loaded by name
from other code: `(require '[wc :as wc])` looks for `wc.norn` in the
library search path. [Chapter 8](08-modules-and-projects.md) explains
search paths and projects.

## Finding your way around

Norn's documentation is built into the runtime. At the REPL, `doc` shows a
function's argument lists and docstring:

```text
user=> (doc map)
-------------------------
norn.core/map
([f] [f coll] [f c1 c2] [f c1 c2 c3])
  Apply f to each item (or, with several collections, to items side by
  side). Type-preserving for vectors, lists and sets; a map gives a vector
  of results; a stream gives a stream. (map f) is a transducer.
```

`apropos` finds names that contain a string, among the namespaces that are
loaded. It leaves out the runtime's own names, the ones starting with `%`:

```clojure
(apropos "range") ; => [norn.core/range]
```

`source` shows the definition of a library function:

```text
user=> (source frequencies)
(defn frequencies [coll]
  (reduce (fn [m x] (assoc m x (inc (get m x 0)))) {} coll))
```

From the shell, `norn doc` does the same without starting a REPL. Give it a
namespace to list what the namespace offers, or a qualified name for one
function:

```sh
norn doc norn.string
norn doc norn.string/upper-case
```

```text
-------------------------
norn.string/upper-case
([s])
  s in upper case.
```

The [standard library reference](../reference/libraries.md) lists every
bundled namespace with a line about what it's for, and the
[symbol index](../reference/symbols.md) covers `norn.core`.

## Summary

- `cargo build --release -p norn --bin norn` builds a single `norn`
  executable with the standard libraries inside.
- `norn` starts the REPL; `norn -e '…'` evaluates and prints the last value;
  `norn FILE.norn ARGS…` runs a file.
- A list is a call: the first element is the operation, the rest are its
  arguments.
- `def` and `defn` create vars. Redefining one takes effect on the next
  call everywhere.
- A file that starts with `ns` is a module; it declares its requirements and
  can refer to later definitions.
- `doc`, `apropos` and `source` at the REPL, and `norn doc` in the shell,
  show the documentation built into the runtime.

---

[Contents](README.md) · Previous: [1. Introduction](01-introduction.md) · Next: [3. Values and expressions](03-values-and-expressions.md) <!-- nav -->
