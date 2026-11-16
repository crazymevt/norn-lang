# 29. Appendix: Norn for Lispers

[Contents](README.md) · Previous: [28. Foreign functions](28-foreign-functions.md) <!-- nav -->

If you already write Clojure or Common Lisp, most of Norn will look
familiar, and a few things will surprise you. This appendix lists the
differences that matter in practice, with a pointer to the chapter that
covers each one. Read the section for the Lisp you know.

## For Clojure programmers

Norn's reader syntax, core function names and data model come from
Clojure, so your hands already know most of it. The differences are in
what happens when the code runs.

### Sequences are eager and keep their type

This is the change you'll notice first. `map`, `filter`, `remove`, `take`
and the rest run immediately and return the same kind of collection they
were given:

```clojure
(map inc [1 2 3]) ; => [2 3 4]
(map inc '(1 2 3)) ; => (2 3 4)
(filter odd? #{1 2 3}) ; => #{1 3}
(filter (fn [[_ v]] (odd? v)) {:a 1 :b 2}) ; => {:a 1}
```

Functions that compute new items from a map, such as `map` and `keep`,
return a vector, since the results needn't be entries. Ranges and strings
give vectors too:

```clojure
(map inc (range 3)) ; => [1 2 3]
(map identity "ab") ; => [\a \b]
```

So `mapv` and `filterv` are rarely needed, and there's no chunking or
realization-order puzzle to think about: work happens inside the caller's
dynamic bindings, `try` blocks and scopes.

Laziness is still available, but you ask for it. `lazy` turns a collection
into a stream, and generators such as `iterate`, `repeat`, `cycle` and
`(range)` with no end return streams. Sequence functions such as `map`,
`filter`, `take`, `partition` and `interpose` return a stream when given
one ([Chapter 6](06-sequences-and-transducers.md) has the details):

```clojure
(take 3 (iterate inc 0)) ; => (0 1 2)
(->> (lazy [1 2 3 4]) (map inc) (filter even?) vec) ; => [2 4]
```

Transducers work as in Clojure, and `cat` and `mapcat` stop early on
`reduced`. See [Chapter 6](06-sequences-and-transducers.md).

### `()` is not `nil`

The empty list is a value of its own. It's truthy, and it isn't equal to
`nil`. `seq` still returns `nil` for an empty collection, so the
`(when (seq xs) …)` idiom works unchanged:

```clojure
[(if () :truthy :falsey) (= nil ()) (seq ())] ; => [:truthy false nil]
```

### Tail calls are proper

A call in tail position doesn't grow the stack, so mutual recursion and
self-calls work for any depth. `recur` remains, as an assertion that the
call is in tail position:

```clojure
(defn count-down [n acc]
  (if (zero? n) acc (count-down (dec n) (inc acc))))

(count-down 1000000 0) ; => 1000000
```

### Classes and generic functions replace records, types and multimethods

There's no `defrecord`, `deftype` or `defmulti`. Use `defclass` for data
with a name, and `defgeneric` with `defmethod` for polymorphism. Instances
are immutable values that compare by their slots and can be read with
keywords, much like records:

```clojure
(defclass point () ((x :initform 0) (y :initform 0)))

(let [p (point :x 1 :y 2)]
  [(:x p) (= p (point :x 1 :y 2)) (:y (with p :y 5))]) ; => [1 true 5]
```

Generic functions dispatch on the classes of *all* required arguments, not
just the first, and support `:before`, `:after` and `:around` methods. A
`(:dispatch f)` option gives you multimethod-style dispatch on a computed
value. `defprotocol`, `extend-type` and `extend-protocol` exist, as sugar
over generic functions. See Chapters
[9](09-classes-and-generic-functions.md) and
[10](10-protocols-and-extension.md).

### Errors are conditions, with restarts

`try`, `catch`, `finally`, `throw`, `ex-info` and `ex-data` work as you
expect. Underneath, they're built on a condition system, which adds
handlers that run *before* the stack unwinds and restarts that let a
handler choose how the failing code continues:

```clojure
(handler-bind [ArithmeticError (fn [_] (invoke-restart 'use-value 0))]
  (+ 1 (/ 1 0))) ; => 1
```

Conditions are classes: you catch `ArithmeticError` or `FileNotFound`, not
an exception type from a host platform. `throw` accepts a string too, which
becomes a `SimpleError`. See [Chapter 11](11-conditions-and-restarts.md).

### Fibers instead of threads

There are no Java threads, thread pools or `core.async` macros. Every
fiber is cheap, blocking operations such as `take!` and file reads park the
fiber instead of a thread, and you write plain direct-style code. Spawn
fibers inside a scope that waits for them:

```clojure
(with-scope [s]
  (let [a (spawn s (* 6 7))]
    (await a))) ; => 42
```

`future`, `promise` and `deliver` exist and are built on fibers. Channels
come with `put!`, `take!`, `alts!` and `timeout` as functions, not as
macros that rewrite your code. Atoms work as in Clojure; refs and `dosync`
live in the `norn.stm` library. See Part III, starting with
[Chapter 15](15-fibers-and-scopes.md).

### Other differences

- **No Java.** There are no Java classes or interop forms. Their work is
  done by Norn's libraries: `norn.math/round` rather than `Math/round`,
  `norn.sys/getenv` rather than `System/getenv`, `sleep` rather than
  `Thread/sleep`. When you write one of the common ones out of habit,
  the error says what to use:

  ```clojure
  (condition-message (try (eval '(Math/round 2.5)) (catch Error e e)))
  ; => "Unable to resolve symbol: Math/round in this context (Norn has no Java classes: try norn.math/round)"
  ```
- **Dynamic binding.** `binding` works on any var; you don't need to mark
  it `^:dynamic`. Bindings are conveyed to the fibers you spawn.
- **Metadata.** The reader's `^:private`, `^:dynamic` and `^{:doc …}` work
  on definitions. `meta` on a var gives what's recorded about it (`:doc`,
  `:arglists`, `:file`, `:line`, `:macro`…), but values carry no metadata
  yet: `meta` of a vector is nil, and there's no `with-meta`.
- **`defonce`** works as in Clojure, and takes a docstring: reloading a
  file keeps what a `defonce` holds. `bound?` tells you whether a var has
  a value.
- **Tagged literals.** `#inst` and `#uuid` read as in Clojure (a UUID is
  its text), and Norn's decimals and dates print and read as
  `#norn/decimal "12.30"` and `#norn/date "2026-11-03"`. `*tag-readers*`
  takes the place of `*data-readers*`, and `edn/read-string` takes
  `:readers` and `:default`. `*print-readably*` tells a `print-object`
  method whether it's printing for `pr` or for `print`.
- **Maps don't keep their order.** There's no array map: even a small map
  literal lists its keys in hash order, so `{:a 1 :b 2 :c 3}` may print
  with `:c` before `:b`. Use a vector of pairs, or a sorted map, where
  order matters.
- **Numbers.** Integers promote to bignums automatically, there's no `+'`,
  and `(= 1 1.0)` is false while `(== 1 1.0)` is true. Mixed comparisons
  between floats and exact numbers are exact.
- **Splitting keeps trailing empty strings.** `norn.string/split` and
  `split-lines` return every part, including an empty one at the end,
  where Clojure drops it. Use `(remove str/blank? …)` when you don't want
  them.
- **Type hints are contracts.** `^Integer` on a parameter isn't a
  performance hint: it's a checked signature, enforced on calls from other
  modules, with blame. See [Chapter 13](13-contracts.md).
- **Macros are hygienic.** `defmacro` and syntax quote work as you're used
  to, including `x#` auto-gensyms, but identifiers in a template keep the
  meaning they had where the macro was defined. See
  [Chapter 14](14-macros.md).
- **Startup is fast**, and you can save a whole running heap as an image.
  See [Chapter 27](27-images-and-deployment.md).

## For Common Lisp programmers

Norn has the parts of Common Lisp that are hardest to find elsewhere:
generic functions with method combination, the condition system, and
images. It wraps them in Clojure's syntax and data model.

### One namespace, Clojure syntax

Norn is a Lisp-1: functions and values share one namespace, so there's no
`funcall` or `#'` to pass a function. Write `(apply f args)` or `(f x)`.
In Norn, `#'name` reads as `(var name)`, the var that holds a global
definition.

Vectors `[ ]`, maps `{ }` and sets `#{ }` have literal syntax. Function
parameters go in a vector, and `let` takes a flat binding vector:

```clojure
(let [x 2
      y (* x 10)]
  (+ x y)) ; => 22
```

Some everyday translations:

| Common Lisp | Norn |
| --- | --- |
| `progn` | `do` |
| `labels`, `flet` | `letfn` |
| `eq`, `eql` | `identical?` (rarely needed) |
| `equal`, `equalp` | `=` |
| `(loop …)`, `dolist` | `loop`/`recur`, `doseq`, `for`, `dotimes`, sequence functions |
| `setf` on a place | a new value: `assoc`, `update`, `with`; or an atom |
| `defparameter`, `defvar` | `def`, and `binding` for a dynamic extent |
| packages | namespaces, one per file |
| `format` directives | `format` with printf-style `%s %d %.2f` |

Symbols are case-sensitive, and `Foo` and `foo` are different names.

### `nil`, `false` and `()` are three values

There's a real boolean `false`, and the empty list isn't false. Only
`nil` and `false` are falsey:

```clojure
[(if () :yes :no) (if 0 :yes :no) (if false :yes :no)] ; => [:yes :yes :no]
```

### Data doesn't change

Lists, vectors, maps and sets are persistent: operations return new
collections, and the old ones stay as they were. There's no `setf`, no
`rplaca`, and no destructive `nreverse`. Code that would update a place in
Common Lisp computes a new value instead, or keeps the changing state in an
explicit identity such as an atom.

```clojure
(let [m {:count 1}
      m2 (update m :count inc)]
  [m m2]) ; => [{:count 1} {:count 2}]
```

### CLOS, mostly as you know it

`defclass`, `defgeneric` and `defmethod` behave like their CLOS namesakes:
multiple inheritance (with C3 linearization), multiple dispatch, `eql`
specializers, `:before`, `:after` and `:around` methods,
`call-next-method`, and built-in method combinations such as `+`, `and`
and `list`. There's a metaobject protocol with `compute-applicable-methods`
and `compute-effective-method`.

The large difference is that instances are immutable values by default.
`with` returns an updated copy, and two instances with the same class and
slot values are `=`. A class that really needs mutable state declares
`(:mutable)`; its instances then belong to one fiber at a time, and other
fibers get an `OwnershipError` rather than a data race.

```clojure
(defclass account () ((balance :initform 0 :reader balance)))

(let [a (make account :balance 10)]
  [(balance (with a :balance 15)) (balance a)]) ; => [15 10]
```

`make` constructs an instance, and calling the class as a function does the
same. `make-instance` and `initialize-instance` are there for the MOP.
See Chapters [9](09-classes-and-generic-functions.md) and
[10](10-protocols-and-extension.md).

### The condition system, conveyed to fibers

`signal`, `error`, `warn`, `muffle-warning`, `handler-bind`, `handler-case`,
`restart-case`, `invoke-restart`, `find-restart`, `compute-restarts`,
`unwind-protect`, `ignore-errors` and `with-simple-restart` are all here,
and they work as in Common Lisp:

```clojure
(defn parse-or-restart [s]
  (restart-case (or (parse-long s) (error "Not a number"))
    (use-zero [] 0)))

(handler-bind [Error (fn [_] (invoke-restart 'use-zero))]
  (mapv parse-or-restart ["1" "x" "3"])) ; => [1 0 3]
```

Conditions are classes in an ordinary hierarchy: `Condition`,
`SeriousCondition`, `Error`, `Warning`, and runtime errors such as
`TypeError` and `ArithmeticError` with slots you can read. Every error the
runtime signals offers `use-value` and `retry`. Two additions matter:

- `handler-bind` handlers are conveyed to child fibers, so a recovery
  policy set up around a parallel operation applies inside it.
- An unhandled error at the REPL parks the fiber in the debugger with its
  stack intact while every other fiber keeps running.

See Chapters [11](11-conditions-and-restarts.md) and
[12](12-live-programming.md).

### Macros

`defmacro` is available and familiar, but it's built on a hygienic
expander: names in a syntax-quote template refer to what they meant where
the macro was written, so you rarely need `gensym` (and `x#` generates one
for you when you do). Modules have phases, so compile-time helpers are
declared with `begin-for-syntax` or required with `:require-for-syntax`.
There are no reader macros yet. See [Chapter 14](14-macros.md).

### Images

`save-image` writes the heap to a `.nimg` file and `norn --image` starts
from it, like `save-lisp-and-die` but without dying, and with parked fibers
saved too. Production images come from `norn build`, which builds them
reproducibly from source. See [Chapter 27](27-images-and-deployment.md).

### What's missing on purpose

Multiple values, `loop` the macro, `tagbody` and `go`, special variables
declared with `defvar` (any var can be bound dynamically), symbol property
lists, and global reader macros. Tail calls are guaranteed, so recursion
replaces most explicit iteration constructs.

## Summary

- From Clojure: sequence functions are eager and type-preserving, laziness
  is explicit, `()` is truthy, tail calls are proper, and classes with
  generic functions replace records, types and multimethods.
- Errors are conditions with restarts; `try` and `catch` are built on them.
- Fibers replace threads; channels and scopes are plain functions and
  macros.
- From Common Lisp: one namespace, Clojure's literal syntax, immutable data
  and instances, and a real `false`.
- CLOS, the condition system and images are present, extended for
  concurrency: handlers are conveyed to fibers, and unhandled errors park.

---

[Contents](README.md) · Previous: [28. Foreign functions](28-foreign-functions.md) <!-- nav -->
