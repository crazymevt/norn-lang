# 26. Tools and performance

[Contents](README.md) · Previous: [25. Testing](25-testing.md) · Next: [27. Images and deployment](27-images-and-deployment.md) <!-- nav -->

This chapter covers the command-line tools you use every day on a project:
`norn fmt` for layout, `norn lint` for likely mistakes, `norn doc` and
docstrings, and `norn expand` for macros. Then it turns to speed: how to
measure with `time` and `norn.bench`, what the two execution tiers do, which
operations are cheap and which are expensive in this release, and the
habits that keep hot code fast.

Each tool is also a library, so the examples below call the library
functions directly. The shell commands do the same work over files.

## One style: `norn fmt`

Norn has one code style, and `norn fmt` applies it. There are no options to
configure. The formatter keeps your line breaks and fixes everything else:
spacing inside a line, indentation, trailing whitespace, and blank lines.
Here is a file written in a hurry, `src/report/messy.norn`:

```text
(ns report.messy)
(defn   scale [xs factor]
    (map (fn [x]
     (* x factor))
             xs   ))


(defn  label [{:keys [name  unit]}]
  (str name
  " (" unit ")" ))
```

`norn fmt --check` lists the files it would change and exits 1. `norn fmt`
with no flag rewrites them:

```sh
norn fmt --check
norn fmt src/report/messy.norn
```

```text
not formatted: ./src/report/messy.norn
formatted src/report/messy.norn
```

The file now reads:

```text
(ns report.messy)
(defn scale [xs factor]
  (map (fn [x]
         (* x factor))
       xs))

(defn label [{:keys [name unit]}]
  (str name
       " (" unit ")"))
```

The rules are few:

- No space just inside brackets, so closing brackets join the line before.
- One space between items on a line. Commas stay where you put them.
- No trailing whitespace, at most one blank line in a row, and one newline
  at the end of the file.
- A form with a body, such as `defn`, `let`, `when`, `fn` or `deftest`,
  indents its later lines by two. So does any form whose name starts with
  `def` or `with-`.
- Any other call lines up its later lines with its first argument when that
  argument is on the head's line, and indents by one when it isn't.

The formatter never changes what the code means. The same tokens come out
in the same order. `norn.fmt/format-string` formats text, and
`norn.fmt/formatted?` asks whether text is already in the style:

```clojure
(require '[norn.fmt :as fmt])

(fmt/format-string "(let [x 1\n   y 2]\n      (+ x y ))\n")
; => "(let [x 1\n      y 2]\n  (+ x y))\n"

(fmt/formatted? "(+ 1 2)\n") ; => true
```

Your own macros follow the same rules. A macro named `with-something` gets
body indentation automatically. For any other name, declare how many
arguments come before the body with `{:indent n}` in the macro's attribute
map:

```clojure
(defmacro retrying
  "Run body, retrying it up to n times."
  {:indent 1}
  [n & body]
  `(loop [i# 1]
     (let [r# (try {:ok (do ~@body)} (catch Error e# (if (< i# ~n) nil (throw e#))))]
       (if r# (:ok r#) (recur (inc i#))))))

(fmt/format-string "(retrying 3\n(println 1)\n(println 2))\n")
; => "(retrying 3\n  (println 1)\n  (println 2))\n"
```

The hint is recorded when the macro is defined, so it applies where the
macro is loaded, as in a REPL or an editor session that has loaded your
code. `norn fmt` run from the shell doesn't load the files it formats, but
it reads the hints from the `defmacro` forms in them, so it knows your
project's macros as well as the standard library's.

## Likely mistakes: `norn lint`

`norn lint` reads each file, expands its macros, and reports problems it can
find without running your code. Here's a module with four of them:

```text
(ns report.summary
  (:require [norn.string :as str]
            [norn.json :as json]))

(defn mean [xs]
  (let [n (count xs)
        total (reduce + 0 xs)]
    (/ (reduce + 0 xs) n)))

(defn describe [xs]
  (str "mean " (mean xs 2) ", max " (apply mx xs)))

(defn shout [s] (str/upper-case s))
```

```sh
norn lint
```

```text
./src/report/summary.norn:5: unused: unused local total
./src/report/summary.norn:10: arity: mean called with 2 arguments; it takes 1
./src/report/summary.norn:10: unresolved: unresolved symbol mx
./src/report/summary.norn:1: unused-require: norn.json is required but never used (for its effect alone, require it without :as)
```

Each warning gives the file, the line of the top-level form it's in, a
kind, and a message. With no paths, `norn lint` lints the current directory,
and it exits 1 when it finds anything. The kinds include:

- `unresolved`: a name that is no local, no definition, and no var.
- `unused`: a `let` or `loop` binding that nothing uses. Names that start
  with `_` are exempt.
- `arity`: a call with a number of arguments that none of the function's
  arities takes.
- `unused-require` and `missing-require`: a require nothing uses, or a full
  name from a namespace the module doesn't require.
- `deprecated` and `private`: a use of a definition marked deprecated, or of
  another module's private var.
- `shadow`, `def-in-fn`, `method-shape`, `macro`, `macro-order` and
  `phase`, for definitions that hide special forms, `def` inside a
  function, a `defmethod` that doesn't fit its generic function, a macro
  call that fails to expand, a call to a macro defined further down the
  module, and compile-time code that uses a run-time name.

The linter evaluates only the forms that change how later forms read and
expand: `ns`, `in-ns`, `require`, `defmacro`, and the syntax definitions.
So requiring a module loads it, but your functions never run.
`norn.lint/lint-string` returns the warnings as data, which is what an
editor uses to underline them:

```clojure
(require '[norn.lint :as lint])

(lint/lint-string "(defn f [x] (let [y 1] (g x)))" "f.norn")
; => [{:kind :unresolved :message "unresolved symbol g" :line 1 :file "f.norn"}
;     {:kind :unused :message "unused local y" :line 1 :file "f.norn"}]
```

## Documentation: `norn doc` and docstrings

[Chapter 2](02-first-steps.md) introduced `doc`, `source` and `apropos` at
the REPL and `norn doc` in the shell. They read the same definitions your
code makes, so a docstring you write is documentation at once. Docstrings
are Markdown. After the docstring, an attribute map can say more:

- `:since` names the version that added the definition.
- `:deprecated` marks it and says what to use instead.
- `:see-also` lists related names.
- `:examples` holds executable examples, which `norn check` runs (see
  [Chapter 25](25-testing.md#examples-in-docstrings)).

```clojure
(ns report.stats)

(defn median "The middle value of xs." [xs] (nth (sort xs) (quot (count xs) 2)))

(defn average
  "The arithmetic mean of `xs`, a nonempty collection of numbers."
  {:since "0.2" :see-also [median]}
  [xs]
  (/ (reduce + 0 xs) (count xs)))

(defn mean
  "Old name for `average`."
  {:deprecated "use average"}
  [xs]
  (average xs))

(doc average)
; prints: -------------------------
; prints: report.stats/average
; prints: ([xs])
; prints:   Since 0.2
; prints:   The arithmetic mean of `xs`, a nonempty collection of numbers.
; prints:   See also: median
```

`mean` still works, but its documentation says it's deprecated, and
`norn lint` reports each call to it from another module:

```text
report/use.norn:2: deprecated: s/mean is deprecated: use average
```

Deprecating a name this way, and keeping it for a release, gives callers
time to move.

## Macro expansion: `norn expand`

When a macro does something you don't expect, look at what it expands to.
`norn expand` prints each top-level form fully expanded, and `--steps`
shows each macro step on the way, outermost first, with the names each
step introduced:

```sh
norn expand --steps -e '(when-let [x (first xs)] (inc x))'
```

```text
;; <expr>:1:1
1. norn.core/when-let (defmacro) at <expr>:1:1
   (when-let [x (first xs)] (inc x))
=> (let* [temp__14__auto__ (first xs)]
     (norn.core/when temp__14__auto__ (norn.core/let [x temp__14__auto__] (inc x))))
   introduced: let* temp__14__auto__ norn.core/when norn.core/let
2. norn.core/when (defmacro) at <expr>:1:1
   (norn.core/when temp__14__auto__ (norn.core/let [x temp__14__auto__] (inc x)))
=> (if temp__14__auto__ (do (norn.core/let [x temp__14__auto__] (inc x))))
   introduced: if do
3. norn.core/let (defmacro) at <expr>:1:1
   (norn.core/let [x temp__14__auto__] (inc x))
=> (let* [x temp__14__auto__] (inc x))
   introduced: let*
Fully expanded:
   (let* [temp__14__auto__ (first xs)]
     (if temp__14__auto__ (do (let* [x temp__14__auto__] (inc x)))))
```

Give it a file name in place of `-e` to expand a whole file. At the REPL,
`macroexpand` and `macro-steps` do the same.
[Chapter 14](14-macros.md) covers macros and hygiene in full.

## Measuring

Measure before you optimize, and measure again after. The quickest tool is
`time`, which evaluates an expression, prints how long it took, and returns
its value:

```clojure
(time (reduce + (range 100000))) ; => 4999950000
```

```text
Elapsed time: 11.752583 msecs
```

One run of an expression tells you little. The first run may include
compiling, and a collection in the middle of it can double the time.
`norn.bench` runs an expression many times. It warms up first, for 200 ms
by default, so tier-1 code is compiled. Then it times batches of runs until
its budget, 1 second by default, is spent, and reports the mean time per
run, the standard deviation over the batches, the number of runs, and how
many garbage collections happened while it measured:

```clojure
(require '[norn.bench :as bench])

(defn fib [n] (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))

(def r (bench/bench (fib 15) :ms 100 :warmup-ms 20))

(set (keys r)) ; => #{:mean-ns :stddev-ns :min-ns :runs :batches :gcs}
```

`bench` prints one line, such as:

```text
(fib 20): 2.2 ms ± 450.1 µs (232 runs, 0 GCs)
```

`bench` is a macro over `bench/run`, which takes a function of no
arguments and returns the same map without printing. `bench/report` turns
that map into the line. Use `run` when you compare several variants in a
script:

```clojure
(def xs (vec (range 1000)))

(def results
  (into {}
        (map (fn [[label f]] [label (bench/run f :ms 50 :warmup-ms 10)]))
        [["reduce" (fn [] (reduce + 0 xs))]
         ["loop" (fn [] (loop [i 0 acc 0]
                          (if (< i (count xs)) (recur (inc i) (+ acc (nth xs i))) acc)))]]))

(every? pos? (map :mean-ns (vals results))) ; => true
```

A few rules make the numbers worth having:

- Benchmark a whole job you care about, not only one operation. The
  operation that looks slow in isolation may not matter in the job.
- Watch the GC count. A result with many collections is measuring the
  collector as much as your code.
- Look at the spread. When the standard deviation is a large part of the
  mean, something else on the machine is competing, and you should run
  again.
- Compare tiers. If tier 1 barely changes a number, the time is spent in a
  primitive, an algorithm, or the collector, not in interpreted code.

The repository has two benchmark suites you can run and read as examples:
`bench/micro.norn` times single operations, and `bench/workloads.norn`
times whole jobs.

## Two tiers

Norn compiles your code to bytecode, and runs it in one of two ways:

- **Tier 0**, the default, is a bytecode interpreter. It starts running
  code at once, with nothing to compile, and it runs everywhere, including
  platforms that forbid generating code at run time.
- **Tier 1** compiles each function to native code with Cranelift the first
  time it runs. Turn it on with the environment variable `NORN_TIER=1`.
  Tier 1 targets 64-bit ARM macOS and 64-bit ARM and x86 Linux.

```sh
NORN_TIER=1 norn bench/workloads.norn
```

Both tiers lay out stack frames the same way. Tier-1 code can hand any
instruction it doesn't handle back to the interpreter, so the debugger,
restarts, parked fibers and images work identically. Images don't store
tier-1 machine code. A restored image compiles functions again as they
run.

Tier 1 speeds up what your own code does: calls, branches, loops, var
reads, and fixnum arithmetic. It doesn't speed up work done inside the
runtime's primitives, such as reading JSON, sorting, or matching a regex,
or the time the collector takes. The performance survey
([docs/perf-survey.md](../perf-survey.md)) measured both tiers on an Apple
M4:

| Operation (ns each) | Tier 0 | Tier 1 |
| --- | ---: | ---: |
| An empty loop iteration | 35 | 5 |
| A call with one argument | 54 | 18 |
| Fixnum `+` and `<` | 54 | 5 |
| Float `*` | 74 | 33 |
| A generic function with one method | 111 | 69 |
| A slot read `(:x p)` | 52 | 14 |

| Workload (ms per run) | Tier 0 | Tier 1 |
| --- | ---: | ---: |
| JSON: write / read 1 MB | 1.6 / 4.1 | 2.0 / 5.0 |
| Sort 100,000 integers | 52 | 50 |
| Spawn and await 10,000 fibers | 21 | 4.7 |
| 10,000 messages through a channel | 1.6 | 0.6 |

Tight loops and call-heavy code gain 3 to 10 times. Library-heavy work
barely moves. When this chapter was written, the author measured
`(fib 20)` at 2.2 ms at tier 0 and 0.9 ms at tier 1 on the same kind of
machine, while other work was running. Floats are still boxed, which is why
float arithmetic gains less than fixnum arithmetic.

Tier 0 stays the default until tier 1 has seen more use. Try tier 1 on your
workload, and keep it if it helps. Your code doesn't change either way.

## Where the time goes

The rest of the survey's numbers describe this release. Most of the costs
are in the implementation, not the interface, and later releases plan to
lower them. They're still worth knowing when you write a hot loop today.
These rows are from the survey, at tier 0, unless noted:

| Operation | Cost | Compare with |
| --- | ---: | --- |
| A call with keyword options, two given | 356 ns | an options map: 191 ns |
| Making a class instance | 2,219 ns | making a map: 67–109 ns |
| `binding` a dynamic var | 563 ns | reading one: 36 ns |
| `try` when nothing goes wrong | 1,580 ns | a plain call: 54 ns |
| An error signalled and caught | 5,216 ns | |
| A call with a checked signature, from another module | 790 ns | after the survey's fix; unchecked: 74 ns |
| `nth` on a library collection (a `norn.buffer` Buffer) | 2,957 ns | `nth` on a vector: 13–52 ns |
| `mapv inc`, per item | 152 ns | |

The author's own runs of `bench/micro.norn`, on a busy machine, gave
numbers 1.5 to 2 times higher across the board, with the same ratios.

Two costs show up in whole programs more than in micro-benchmarks:

- **The collector.** In this release, each collection marks the whole live
  heap, the standard library included, and takes several milliseconds.
  Building a 100,000-entry map with `into {}` triggered 45 collections and
  took over half a second in the survey. Work that allocates a lot is
  dominated by this. A generational collector is planned.
- **Sequence functions.** `mapv`, `filter`, and `into` with a transducer
  cost 90–160 ns per item at tier 1. That's fine for most code and
  noticeable in an inner loop over millions of items.

## Practical advice

The habits below matter in hot code: inner loops, per-request work, and
anything that runs millions of times. Elsewhere, write the clearest code.
For each one, measure your own case before and after.

### Build large collections with transients

A persistent vector's `conj` returns a new vector and leaves the old one
intact, which costs an allocation each time. A transient is a private,
mutable version of a collection that you build up and then freeze with
`persistent!` ([Chapter 5](05-collections.md) introduces them). Inside one
function, nothing else can see the difference:

```clojure
(defn squares [n]
  (loop [i 0 v (transient [])]
    (if (< i n)
      (recur (inc i) (conj! v (* i i)))
      (persistent! v))))

(squares 5) ; => [0 1 4 9 16]
```

Building a 100,000-item vector this way took 9.6 ms at tier 0 against
13.7 ms with `conj`, and 2.3 ms against 6.2 ms at tier 1, with fewer
collections at both tiers (the author's measurements). `(into [] coll)` and
`vec` already build vectors efficiently, so reach for a transient when you
build in a loop of your own.

### Fuse sequence steps with transducers

Norn's sequence functions are eager ([Chapter 6](06-sequences-and-transducers.md)).
A chain of `map`, `filter` and `reduce` builds a whole intermediate
collection at each step. A transducer runs every step on each item in one
pass, with no intermediate collections:

```clojure
(def nums (vec (range 100000)))

(->> nums (map inc) (filter even?) (reduce +)) ; => 2500050000

(transduce (comp (map inc) (filter even?)) + 0 nums) ; => 2500050000
```

The two give the same answer. In the author's measurements, the transducer
took 39 ms against 72 ms at tier 0, and caused no collections against four.
Use `into` with a transducer to build a collection the same way:
`(into [] (comp (map inc) (filter even?)) nums)`.

### Keep hot loops away from checked boundaries

A function with a checked signature, such as `^Integer`, is checked when
code in another module calls it ([Chapter 13](13-contracts.md)). Calls
within the module aren't checked by default. The check is the point of a
signature, since it blames the right caller, but at about 0.6–0.8 µs a call
it adds up in a loop that crosses the boundary once per item. Give the
checked function the whole collection instead of one item at a time, so
the loop runs inside its module and the check happens once.

### Hoist `try`, `binding`, and keyword options out of loops

`try` costs over a microsecond even when nothing goes wrong, and `binding`
about half a microsecond. Put one around the loop, not inside it:

```clojure
(defn parse-all [strs]
  (try
    (mapv parse-long strs)
    (catch Error e
      (error "parse-all: bad input" (condition-message e)))))

(parse-all ["1" "22" "333"]) ; => [1 22 333]
```

When a function called in a hot loop takes keyword options, each call
builds a map of them. Pass the options as a map built once outside the
loop, or give the hot path a positional arity.

### Prefer maps for small, short-lived records

Making a class instance goes through `initialize-instance` and costs about
2 µs in this release, against about 100 ns for a map. Classes are the right
choice for data with behavior and identity ([Chapter 9](09-classes-and-generic-functions.md)).
For records you make by the million and throw away, such as intermediate
results in a parser, a map is cheaper. Reading a slot costs no more than reading a
map key, so it's only construction that costs.

### Allocate less, and know when you're measuring the collector

Since a collection marks the whole heap, the cheapest work is the work that
doesn't allocate. Reuse what you can, prefer `reduce` and transducers over
chains that build intermediate collections, and avoid building large maps
entry by entry in a hot path. When `bench` reports many GCs for one
variant, compare the GC counts as well as the times.

## Summary

- `norn fmt` applies Norn's one style and keeps your line breaks;
  `--check` reports files that need it. Name body macros `with-…`, or give
  them `{:indent n}`.
- `norn lint` reports unresolved names, unused locals and requires, wrong
  arities, deprecated uses and more, without running your functions.
- Docstrings are Markdown, and attribute maps add `:since`, `:deprecated`,
  `:see-also` and `:examples`. `norn doc` and `doc` show them.
- `norn expand --steps` shows macro expansion one step at a time.
- `time` gives one rough number. `norn.bench` warms up, runs many times,
  and reports the mean, the spread, and the GC count.
- Tier 0 interprets bytecode and is the default. `NORN_TIER=1` compiles
  functions with Cranelift and speeds up calls, loops and arithmetic by 3–10
  times, but not primitives or the collector.
- In hot code: build with transients, fuse steps with transducers, keep
  checked boundaries, `try` and `binding` out of inner loops, and prefer
  maps for short-lived records.

---

[Contents](README.md) · Previous: [25. Testing](25-testing.md) · Next: [27. Images and deployment](27-images-and-deployment.md) <!-- nav -->
