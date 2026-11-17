# 1. Introduction

[Contents](README.md) · Next: [2. First steps](02-first-steps.md) <!-- nav -->

This book teaches you to write programs in Skein. It starts with the
expressions you type at the REPL and ends with a signed application you can
hand to someone who has never heard of Skein. Along the way it covers the
data model, functions and sequences, classes and generic functions, the
condition system, concurrency, the standard libraries, testing and
deployment.

This chapter says what Skein is, what ideas shape it, and how the rest of the
book is organized. (Until 2026-11-17 Skein was called Norn: an older note or
program that says `norn` means `skein`.)

## What Skein is

Skein is a general-purpose Lisp. Programs are written as nested lists, and
code is data that macros can transform. Beyond that common ground, Skein
borrows deliberately from three older Lisps:

- From **Clojure**, it takes the literal syntax for data (`[1 2]`, `{:a 1}`,
  `#{:x}`), immutable persistent collections, and the separation of values
  from identities: data never changes, and anything that changes over time
  is an explicit reference such as an atom or a channel.
- From **Common Lisp**, it takes CLOS-style classes and generic functions
  with multiple dispatch and method combination, the condition system with
  restarts, and images that save a running heap to a file.
- From **Racket**, it takes hygienic macros and a module system with phases,
  so macros can't capture the names of the code that uses them and modules
  compile the same way every time.

It runs on its own runtime, written in Rust: a bytecode interpreter with an
optional native-code tier, a work-stealing scheduler that runs millions of
lightweight fibers across every core, and a parallel garbage collector. The
compiler is written in Skein and compiles itself.

Here is a small taste. It reads some orders, keeps the paid ones, and totals
them by category, using a transducer so no intermediate collections are
built:

```clojure
(def orders
  [{:category :books :amount 20 :paid true}
   {:category :games :amount 30 :paid true}
   {:category :books :amount 15 :paid true}
   {:category :games :amount 10 :paid false}])

(defn totals [orders]
  (transduce (filter :paid)
             (completing (fn [acc {:keys [category amount]}]
                           (update acc category (fnil + 0) amount)))
             {}
             orders))

(totals orders) ; => {:books 35 :games 30}
```

And here is the same work spread across fibers, one per category, inside a
scope that waits for all of them:

```clojure
(defn total-for [category]
  (->> orders
       (filter #(and (:paid %) (= category (:category %))))
       (map :amount)
       (reduce + 0)))

(with-scope [s]
  (let [books (spawn s (total-for :books))
        games (spawn s (total-for :games))]
    [(await books) (await games)])) ; => [35 30]
```

If one of those fibers had failed, the scope would have cancelled the other
and passed the failure to the caller. Nothing leaks out of a scope.

## The ideas behind Skein

Five principles explain most of Skein's design decisions. You'll meet each of
them in detail later; it helps to know them up front.

**Values by default, identities on purpose.** Numbers, strings, collections,
and even instances of your own classes are immutable values. `assoc` on a
map returns a new map; the old one is unchanged and can be shared freely
between fibers on different cores. When something really must change, you
use an identity type made for it: an atom, a ref, a channel, an actor, or a
mutable object with an owner. This is what makes a dynamic language safe to
run in parallel.

**Everything is live.** Functions, classes, methods and macros can all be
redefined while the program runs, on every core at once. Calls go through
vars, so the next call sees the new definition. Redefining a class keeps its
identity, and existing instances are read through the new layout.

**Errors are conversations, not crashes.** Skein has Common Lisp's condition
system. Code that detects a problem signals a condition. Code further up the
stack decides what to do about it, and it can decide *before* the stack
unwinds, by choosing a restart that the low-level code offered. During
development an unhandled error doesn't kill anything: the fiber parks with
its stack intact, and you can inspect it, fix the function, and resume.

**Concurrency is in the runtime.** Fibers cost a few hundred bytes and are
scheduled across all cores. I/O never blocks a core: files run on a blocking
pool and sockets on an event reactor, while your code stays in direct style
with no `async` coloring. Structured concurrency, channels, software
transactional memory and Erlang-style actors are all built on the same
fibers.

**The image is an artifact; the source is the truth.** You can save a
running heap, parked fibers included, and start it again in tens of
milliseconds. Deployable images are built reproducibly from source modules,
so an image is a cache of your program, never the only copy of it.

## What Skein leaves behind

If you know another Lisp, some habits won't carry over. The most important
differences:

- `nil`, `false` and the empty list are three different things. Only `nil`
  and `false` are false; `()` is true.
- Sequence functions such as `map` and `filter` are eager and keep the type
  of their input: `(map inc [1 2])` returns the vector `[2 3]`. Laziness is
  something you ask for with an explicit stream.
- Tail calls are proper. You can write a loop as a self-call in tail
  position; `recur` still exists and checks that a call really is in tail
  position.
- Instances of classes are immutable values that compare by their slots,
  unless the class declares otherwise.
- Handlers established with `handler-bind`, and dynamic bindings, are
  conveyed to the fibers you spawn.

[Appendix 29](29-skein-for-lispers.md) collects these differences for
Clojure and Common Lisp programmers.

## How this book is organized

The book has five parts.

**Part I, The language**, covers what you need for everyday programs:
expressions and values ([Chapter 3](03-values-and-expressions.md)),
functions and control flow ([Chapter 4](04-functions-and-control-flow.md)),
collections ([Chapter 5](05-collections.md)), sequences and transducers
([Chapter 6](06-sequences-and-transducers.md)), text
([Chapter 7](07-text.md)), and modules and projects
([Chapter 8](08-modules-and-projects.md)).

**Part II, Objects, errors, and the live system**, covers classes and
generic functions ([Chapter 9](09-classes-and-generic-functions.md)),
protocols and extending the language's own operations
([Chapter 10](10-protocols-and-extension.md)), conditions and restarts
([Chapter 11](11-conditions-and-restarts.md)), working with a running
program and its debugger ([Chapter 12](12-live-programming.md)), checked
signatures and contracts ([Chapter 13](13-contracts.md)), and macros
([Chapter 14](14-macros.md)).

**Part III, Concurrency**, covers fibers and structured scopes
([Chapter 15](15-fibers-and-scopes.md)), channels
([Chapter 16](16-channels.md)), shared state
([Chapter 17](17-shared-state.md)), and actors with supervision
([Chapter 18](18-actors-and-supervision.md)).

**Part IV, Working with the world**, covers the standard libraries for
files and streams ([Chapter 19](19-files-and-streams.md)), data formats
([Chapter 20](20-data-formats.md)), the operating system
([Chapter 21](21-the-operating-system.md)), networking and HTTP
([Chapter 22](22-networking-and-http.md)), databases
([Chapter 23](23-databases.md)), and command-line programs
([Chapter 24](24-command-line-programs.md)).

**Part V, Shipping**, covers testing ([Chapter 25](25-testing.md)), tools
and performance ([Chapter 26](26-tools-and-performance.md)), images and
deployment ([Chapter 27](27-images-and-deployment.md)), and calling C
([Chapter 28](28-foreign-functions.md)).

Read Part I in order. After that, the chapters mostly stand alone, and each
one links back to what it builds on.

## Reading the examples

Code examples show their results in comments:

```clojure
(+ 1 2) ; => 3
```

`; =>` shows the value an expression returns. A line beginning
`; prints:` shows what an expression writes to standard output:

```clojure
(println "Hello," "Skein")
; prints: Hello, Skein
```

and `; !!` names the class of condition an expression signals:

```clojure
(/ 1 0) ; !! ArithmeticError
```

Within a chapter, examples build on each other, so a function defined in
one block is available in the blocks that follow it. Every example in the
book is run against Skein by `docs/book/check.py`, so the results you see
are the results you'll get. To run them yourself:

```sh
python3 docs/book/check.py                     # every chapter
python3 docs/book/check.py docs/book/05-collections.md
```

Shell commands appear in `sh` blocks, and output from programs and the REPL
appears in plain text blocks.

This book teaches; it doesn't list everything. When you want the complete
set of functions in a library, or every option of a form, use the
[language reference](../reference/README.md) and the `skein doc` command,
which reads the documentation built into the runtime you're using.

## Summary

- Skein is a Lisp that combines Clojure's data model, Common Lisp's objects,
  conditions and images, and Racket's hygienic macros and modules.
- Its runtime is built for many cores: fibers, a work-stealing scheduler, a
  parallel collector, and non-blocking I/O.
- Data is immutable; change goes through explicit identity types.
- Definitions are live, errors park instead of crashing during development,
  and images make startup fast.
- Sequences are eager and type-preserving; `()` is truthy; tail calls are
  proper.
- Examples show results with `; =>`, output with `; prints:`, and
  conditions with `; !!`, and all of them are tested.

---

[Contents](README.md) · Next: [2. First steps](02-first-steps.md) <!-- nav -->
