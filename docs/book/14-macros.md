# 14. Macros

[Contents](README.md) · Previous: [13. Contracts](13-contracts.md) · Next: [15. Fibers and scopes](15-fibers-and-scopes.md) <!-- nav -->

A macro is a function that runs while your code is being compiled. It
receives forms, the unevaluated code of its arguments, and returns new
code that the compiler compiles in its place. Macros are how Lisp grows
new syntax: `when`, `cond`, `->`, `defn`, and `with-open` are all macros
written in Skein. This chapter covers `defmacro`, syntax quote, hygiene
and auto-gensyms, how to look at what a macro produces, compile-time
phases, the pattern-based hygienic macros of `skein.syntax`, and when a
macro is the right tool.

## Code is data

Skein code is made of Skein data: a call is a list, a binding vector is a
vector, a name is a symbol. [Chapter 3](03-values-and-expressions.md)
showed that quoting a form gives you that data instead of evaluating it:

```clojure
'(if (ready?) (launch) nil) ; => (if (ready?) (launch) nil)
(first '(if (ready?) (launch) nil)) ; => if
```

A macro takes such data and returns more of it. Here is `unless`, the
opposite of `when`: it runs its body when the test is false. A function
couldn't do this, because a function's arguments are evaluated before it
runs, and the point of `unless` is to evaluate the body only sometimes.

```clojure
(defmacro unless [test & body]
  (list 'if test nil (cons 'do body)))

(unless false :ran) ; => :ran
(unless true (println "never printed")) ; => nil
```

`defmacro` looks like `defn`, with a docstring, an optional attribute
map, and one or more arities. The difference is when it runs and what it
receives. When the compiler meets `(unless false :ran)`, it calls the
macro with the forms `false` and `:ran`, not their values, and compiles
whatever the macro returns:

```clojure
(macroexpand-1 '(unless (ready?) (launch))) ; => (if (ready?) nil (do (launch)))
```

`macroexpand-1` shows one step of expansion: the macro is called, and its
result is returned as data rather than compiled. Nothing in the
expansion runs, which is why `ready?` and `launch` don't need to exist.

## Syntax quote

Building code with `list` and `cons` gets hard to read quickly. *Syntax
quote*, written with a backquote, lets you write a template that looks
like the code it produces. Inside the template, `~` (unquote) inserts a
value and `~@` (unquote-splicing) inserts the items of a sequence.

```clojure
(defmacro unless [test & body]
  `(if ~test nil (do ~@body)))

(macroexpand-1 '(unless (ready?) (launch) (cheer)))
; => (if (ready?) nil (do (launch) (cheer)))
```

Here is the difference between `~` and `~@` on ordinary values:

```clojure
(let [x 6 xs [1 2]]
  `(+ ~x ~xs ~@xs)) ; => (skein.core/+ 6 [1 2] 1 2)
```

The `+` came back as `skein.core/+`. Syntax quote *qualifies* each symbol
in the template with the namespace it resolves to, or the current
namespace if it doesn't resolve to anything. Special forms such as `if`
and `do` stay as they are.

```clojure
`map ; => skein.core/map
`if ; => if
`not-defined-anywhere ; => user/not-defined-anywhere
```

Qualification is the first half of how Skein's macros avoid surprising
their users, covered in the next section but one. First, a way to see
what macros do.

## Looking at expansions

Three tools show what a macro call becomes:

- `macroexpand-1` expands the outermost call once.
- `macroexpand` repeats that until the outermost form is no longer a
  macro call. It doesn't expand macro calls nested inside.
- `macro-steps` prints every step of the full expansion, inner calls
  included, with the names each step introduced.

```clojure
(macroexpand '(-> order (assoc :paid true) save!))
; => (save! (assoc order :paid true))
(macroexpand '(when ready (launch)))
; => (if ready (do (launch)))
```

`macro-steps` prints. Here it shows `when-not` expanding in a single step:

```clojure
(macro-steps '(when-not done (work)))
; prints: 1. skein.core/when-not (defmacro)
; prints:    (when-not done (work))
; prints: => (if done nil (do (work)))
; prints:    introduced: if do
; prints: Fully expanded:
; prints:    (if done nil (do (work)))
```

From the shell, `skein expand` does the same for an expression or a whole
file. It prints each top-level form fully expanded, and with `--steps` it
shows each step as `macro-steps` does:

```text
$ skein expand -e '(when-let [x (f)] (g x))'
;; <expr>:1:1
(let* [temp__14__auto__ (f)] (if temp__14__auto__ (do (let* [x temp__14__auto__] (g x)))))
```

`let*` and `fn*` are the primitive binding forms that `let` and `fn`
expand to. The odd name `temp__14__auto__` is a generated local, which
brings us to hygiene.

## Hygiene

A macro inserts code into someone else's program, and two kinds of name
collision can happen there. Skein guards against both.

### A macro's local capturing the user's name

`square` multiplies a value by itself. A first try evaluates its argument
twice, which is wrong when the argument has side effects:

```clojure
(defmacro square [x]
  `(* ~x ~x))

(println (square (do (print "eval ") 3)))
; prints: eval eval 9
```

The fix is to evaluate the argument once into a local. But what to call
the local? Any fixed name could collide with a name in the user's
argument. Inside a syntax quote, a symbol ending in `#` is an
*auto-gensym*: each one becomes a fresh name that can't appear anywhere
else, and every `v#` in the same template becomes the same fresh name.

```clojure
(defmacro square [x]
  `(let [v# ~x]
     (* v# v#)))

(println (square (do (print "eval ") 3)))
; prints: eval 9
(let [v 4] (square v)) ; => 16
```

The expansion binds a generated name, not `v`. Its exact spelling changes
from run to run, so this checks its properties instead of printing it:

```clojure
(let [[_ [local value] _] (macroexpand-1 '(square n))]
  [(symbol? local) (not= 'v local) value]) ; => [true true n]
```

If you forget the `#` and write a plain `v`, syntax quote qualifies it to
`user/v`, and `let` refuses to bind a qualified name. The mistake is an
error at expansion rather than a silent capture:

```clojure
(defmacro broken-square [x]
  `(let [v ~x] (* v v)))

(eval '(broken-square 3)) ; !! Error
```

For a name that must be the same across two templates, call `gensym`
yourself and unquote the result.

### The user's local capturing a macro's name

The other collision goes the opposite way: the macro's expansion calls a
function, and the user has a local of the same name where the macro is
used. Qualification prevents it. The expansion calls `user/helper`, the
var, so a local `helper` doesn't get in the way.

```clojure
(defn helper [] :the-real-helper)

(defmacro call-helper []
  `(helper))

(let [helper (fn [] :a-local)]
  [(helper) (call-helper)]) ; => [:a-local :the-real-helper]
```

Skein goes further than qualification. Its expander tracks where every
identifier in a template came from (Racket's *sets of scopes*), so a name
in a macro's template means what it meant where the macro was defined.
One consequence is that a macro exported by a module can expand into a
call to that module's private functions, even though the code it expands
into lives in another module. [Phases](#phases) below has an example.

### Capturing a name on purpose

Sometimes capture is the point. An *anaphoric* macro binds a name for the
user's code to use, such as `it` for the value of a test. Write the name
as `~'it`: the unquote of a quoted symbol, which inserts the plain symbol
`it` without qualification.

```clojure
(defmacro if-it [test then else]
  `(let [~'it ~test]
     (if ~'it ~then ~else)))

(if-it (get {:a 3} :a) (* it 10) :none) ; => 30
```

Use this sparingly and document it. A reader of `(if-it … it …)` has to
know where `it` comes from.

## Code that runs at expansion time

A macro's body is ordinary Skein code, run by the compiler. It can inspect
its arguments, compute, and decide what to produce. This macro accepts
only a symbol and turns a mistake into a clear error when the call is
compiled:

```clojure
(defmacro var-name [sym]
  (when-not (symbol? sym)
    (throw (ex-info "var-name takes a symbol" {:got sym})))
  (str sym))

(var-name total) ; => "total"
(eval '(var-name 42)) ; !! ExInfo
```

The error happens when the call is compiled, before any of the
surrounding code runs. That's why the example uses `eval`: it compiles
the form when the line runs, so the check can see the error.

A macro can also do work once, at compile time, that a function would do
on every call. Here `bit-mask` turns a list of flag keywords into an
integer while the code is compiled, so the compiled code holds only the
number:

```clojure
(def flag-bits {:read 1 :write 2 :exec 4})

(defmacro bit-mask [& flags]
  (reduce + (map flag-bits flags)))

(bit-mask :read :exec) ; => 5
(macroexpand-1 '(bit-mask :read :write)) ; => 3
```

This works in a script and at the REPL, where every form runs as soon as
it's compiled, so `flag-bits` already has its value when `bit-mask`
expands. In a module, macros and the code they call at compile time need
a little more care.

## Phases

A module compiles as a unit before it runs (see
[Chapter 8](08-modules-and-projects.md)). So when a macro in a module
expands, the module's ordinary definitions haven't run yet: `flag-bits`
would have no value. Skein makes this explicit with *phases*. Code that
runs when the module runs is at phase 0. Code that macros run while the
module compiles is at phase 1, and it has its own instance of everything
it uses.

`skein.core` is available at every phase. For anything else a macro's body
calls, you have two choices:

- Put helper definitions inside `(begin-for-syntax …)`. They're defined at
  compile time, for the module's macros.
- Require a library for compile time with `(:require-for-syntax …)` in the
  `ns` form. It takes the same specs as `:require`.

Here is a module that does both. Its macros call `skein.string` and a
compile-time helper, and `log` expands into a call to a private function.
(As in the earlier chapters, the example writes the module to a file and
loads it. Module files are covered in
[Chapter 8](08-modules-and-projects.md).)

```clojure
(require '[skein.io :as io])

(io/mkdirs! "lib")
(add-lib-path! "lib")

(io/write-text "lib/flags.skn" "
(ns flags
  (:require-for-syntax [skein.string :as str]))

(begin-for-syntax
  (def flag-bits {:read 1 :write 2 :exec 4}))

(defmacro bit-mask [& flags]
  (reduce + (map flag-bits flags)))

(defmacro const-name [sym]
  (keyword (str/upper-case (name sym))))

(defn- note [label value]
  [label value])

(defmacro log [form]
  `(let [v# ~form] (note '~form v#)))
")

(require '[flags :as flags])

(flags/bit-mask :write :exec) ; => 6
(flags/const-name max-size) ; => :MAX-SIZE
(flags/log (+ 1 2)) ; => [(+ 1 2) 3]
```

`log` expands into `(note …)` in the `user` namespace, and `note` is
private to `flags`. The call works because the identifier `note` in the
template still refers to the binding where the template was written.

Without the `:require-for-syntax`, the macro body's call to
`str/upper-case` is an error when the module loads, and the message says
what to do:

```clojure
(io/write-text "lib/flags2.skn" "
(ns flags2
  (:require [skein.string :as str]))

(defmacro const-name [sym]
  (keyword (str/upper-case (name sym))))
")

(handler-case (require 'flags2)
  (Error [e] (condition-message e)))
; => "lib/flags2.skn:6:13: str/upper-case isn't available at compile time here: define it inside begin-for-syntax, or require its module with :require-for-syntax"
```

A module required for syntax is a separate instance from the same module
required normally, with its own state. An atom defined in
`begin-for-syntax` counts things during compilation and is gone by the
time the program runs. This separation is what makes a module compile the
same way every time, whatever else happens to be loaded. The REPL top
level is deliberately more relaxed, so you can define a helper and a
macro that uses it one after the other, as the previous section did.

One more rule applies in modules: define a macro before the code that uses
it. The compiler expands each use as it reaches it, so a macro has to be
known by then. A call written above the macro's definition is an error
when the module loads, naming the macro, and `skein lint` reports it as
`macro-order`.

## Hygienic macros with skein.syntax

`defmacro` works on plain data and leans on syntax quote's qualification
and auto-gensyms. `skein.syntax` offers a second style, from Racket and
Scheme: macros written as patterns and templates over *syntax objects*,
which carry each piece of code's scopes and source location. They're
hygienic without any `#` suffixes, and they can report malformed calls at
the exact place in the source.

### syntax-rules

`defsyntax` defines a hygienic macro from a transformer. The simplest
transformer is `syntax-rules`: a list of literal identifiers (often
empty) followed by clauses, each a pattern and a template.

```clojure
(require '[skein.syntax :as syn
           :refer [defsyntax syntax-rules syntax-parser syntax define-syntax-class]])

(defsyntax swap-pair
  (syntax-rules ()
    [(_ a b) [b a]]))

(swap-pair 1 2) ; => [2 1]
```

The pattern `(_ a b)` matches a call with two arguments. `_` matches the
macro's own name, and `a` and `b` are *pattern variables*. In the
template, each pattern variable is replaced by what it matched, and
everything else stays as written, with the meaning it has here.

Clauses are tried in order, so a macro can handle several shapes. `&`
matches the rest of a list, and in a template, `& more` splices it back.
This `my-or` returns its first truthy argument and evaluates the rest only
as needed. Its local `t` never captures the user's `t`, and no gensym is
needed:

```clojure
(defsyntax my-or
  (syntax-rules ()
    [(_) nil]
    [(_ a) a]
    [(_ a & more) (let [t a] (if t t (my-or & more)))]))

(let [t 5] (my-or false t)) ; => 5
(my-or nil false 3) ; => 3
(my-or) ; => nil
```

In a pattern, `p ...` matches zero or more of `p`, and in a template it
repeats `p` once per match:

```clojure
(defsyntax tagged
  (syntax-rules ()
    [(_ tag x ...) [[tag x] ...]]))

(tagged :n 1 2 3) ; => [[:n 1] [:n 2] [:n 3]]
```

`macroexpand-1` and `macroexpand` expand hygienic macros like any other,
as in `(macroexpand-1 '(swap-pair 1 2)) ; => [2 1]`. `macro-steps` shows
hygienic expansions too, and marks them:

```clojure
(macro-steps '(swap-pair 1 2))
; prints: 1. user/swap-pair (hygienic)
; prints:    (swap-pair 1 2)
; prints: => [2 1]
; prints: Fully expanded:
; prints:    [2 1]
```

### syntax-parse and good errors

A macro's users make mistakes, and the error they get should point at
their mistake, not at something deep inside the expansion. `syntax-parser`
makes a transformer whose patterns say what each part should be:

- `x:class` matches a part with a *syntax class*: `id` (an identifier),
  `expr`, `str`, `number`, `integer`, `nat`, `keyword`, `boolean`, `char`,
  or one you define.
- `(~describe "what" p)` names a pattern for error messages.
- `(~optional p)` matches `p` or nothing; `(~or p …)` matches any of the
  patterns; `(~literal id)` matches an identifier that means the same as
  `id`; `(~seq p …)` matches several parts in a row.
- `p ...+` matches one or more.

The template is written with `syntax`. Here is `range-loop`, which counts
a name from one number up to (not including) another:

```clojure
(defsyntax range-loop
  (syntax-parser
    [(_ (~describe "a binding [name from to]" [i:id from:expr to:expr]) body ...+)
     (syntax (let [end to]
               (loop [i from]
                 (when (< i end)
                   body ...
                   (recur (inc i))))))]))

(range-loop [i 0 3] (print i))
; prints: 012
```

`end` holds the upper bound, so the user's expression is evaluated once.
Because the macro is hygienic, the template's `end` is not the user's
`end`, even when the user's code has one:

```clojure
(let [end 2]
  (range-loop [i 0 end] (print [i end])))
; prints: [0 2][1 2]
```

A malformed call is a `SyntaxError` that says what was expected:

```clojure
(eval '(range-loop [1 0 3] (print 1))) ; !! skein.syntax/SyntaxError

(handler-case (eval '(range-loop [i 0] (print i)))
  (Error [e] (condition-message e)))
; => "range-loop: expected a binding [name from to]"
```

When the call comes from a source file, the message starts with the
file, line, and column of the part that's wrong:

```text
Error: report.skn:28:13: range-loop: expected a binding [name from to]
```

When no clause matches, the error describes the part that got furthest
into the syntax, so it points at the actual mistake rather than at the
whole call.

`(~optional p)` leaves its pattern variables unmatched when the part is
missing, and the template form `(~? t u)` gives `t` when its variables
matched and `u` otherwise:

```clojure
(defsyntax let-default
  (syntax-parser
    [(_ [name:id (~optional default:expr)] body ...)
     (syntax (let [name (~? default :none)] body ...))]))

[(let-default [x 5] (* x 2)) (let-default [y] [y])] ; => [10 [:none]]
```

The brackets matter here. Matching tries the greediest way first, so
without them, in `(let-default y [y])` the optional default would take
`[y]` and leave the body empty.

### Syntax classes

`define-syntax-class` names a pattern, with a description for errors, so
several macros can share it. Here a binding pair is a two-item vector of a
name and an expression. `my-let*` binds each pair in turn, one `let` per
pair, by calling itself on the rest:

```clojure
(define-syntax-class binding-pair "a binding [name value]"
  [n:id v:expr])

(defsyntax my-let*
  (syntax-parser
    [(_ [] body ...+) (syntax (do body ...))]
    [(_ [b:binding-pair & more] body ...+)
     (syntax (let b (my-let* [& more] body ...)))]))

(my-let* [[a 1] [b (+ a 1)]] (* a b)) ; => 2

(handler-case (eval '(my-let* [[a 1] [2 3]] a))
  (Error [e] (condition-message e)))
; => "my-let*: expected a binding [name value]"
```

`syntax-parse` runs the same kind of clauses on a syntax object inside
your own transformer function, and `syntax-case` and `syntax` offer the
plainer Scheme-style matching. `skein doc skein.syntax` lists them, along
with the functions for working with syntax objects directly:
`syntax-e`, `syntax->datum`, `datum->syntax`, `syntax-where`, and the
rest.

### Which style to use

Both kinds of macro mix freely, and each can expand into the other.
`defmacro` is quick and familiar to anyone who knows Clojure or Common
Lisp, and fine for small macros with simple arguments. Reach for
`syntax-parser` when a macro has a shape users can get wrong, such as
binding vectors, clauses, or options, since it gives them errors at the
right place for almost no extra code.

## Validated core forms

The core macros people most often get wrong are checked the same way:
`let`, `loop`, `fn`, `defn`, `cond`, `case`, and `ns`. The checks live in
`skein.validate`, and the expander runs them before the macro itself, so a
malformed call gets an error that says what was expected:

```clojure
(handler-case (eval '(let [x] (inc x)))
  (Error [e] (condition-message e)))
; => "let: expected a binding vector [name value …]"
(handler-case (eval '(cond (= 1 1)))
  (Error [e] (condition-message e)))
; => "cond: expected a result after the test"
```

A quick check of the call's shape passes almost every well-formed call,
so validation costs very little. Your own macros get the same quality of
error from `syntax-parser`.

## Formatting hints

`skein fmt` lays out a macro call like a function call unless it knows the
macro has a body. Say so with `:indent` in the macro's attribute map: the
number of arguments that come before the body.

```clojure
(defmacro retrying
  "Run body, trying again up to n times when it signals an Error."
  {:indent 1}
  [n & body]
  `(loop [left# ~n]
     (let [r# (handler-case {:ok (do ~@body)}
                (Error [e#] (if (> left# 1) ::again (throw e#))))]
       (if (= r# ::again) (recur (dec left#)) (:ok r#)))))

(retrying 3
  (+ 1 2)) ; => 3
```

With `{:indent 1}`, the formatter indents the body by two spaces under
`(retrying 3`, as it does for `when` or `let`. The hint is recorded when
the `defmacro` runs, so it applies wherever the macro is loaded, as when
an editor formats code through `skein --server`
([Chapter 12](12-live-programming.md)). `skein fmt` run from the shell
doesn't load the files it formats, but it reads the hints from the
`defmacro` forms in them, so a macro defined in any of the files it's
given gets its hint in all of them. Macros whose names start with `def` or
`with-` get body indentation without a hint, everywhere.

## When to write a macro

A macro is code that writes code, which makes it harder to read, test,
and debug than a function. Write a function when you can. Reach for a
macro when you need something a function can't do:

- **Control evaluation.** `unless`, `retrying`, and `my-or` decide whether
  and how often to evaluate their arguments. A function always receives
  values already computed.
- **Bind names.** `range-loop` and `let-default` introduce locals for the
  body to use.
- **Work at compile time.** `bit-mask` computes its result once, while
  compiling.
- **Remove repetition a function can't.** A family of similar
  definitions, such as a `def` per entry in a table, is a job for a macro
  that expands into several `def`s.

Some habits keep macros pleasant to use:

- Keep the macro thin. Put the real work in a function and have the macro
  expand into a call to it, passing the body as `(fn [] …)` when needed.
  The function can be tested, redefined, and read on its own.
- Evaluate each argument once, binding it to an auto-gensym or a
  hygienic local.
- Check the shape of the call with `syntax-parser`, or at least throw a
  clear error from a `defmacro`.
- Look at the expansion with `macroexpand-1`, `macro-steps`, or
  `skein expand --steps` whenever a macro surprises you.

Remember that a macro isn't a value. You can't pass `unless` to `map` or
store it in a map, and calling one as if it were a function is an error.

```clojure
(eval '(map unless [true false])) ; !! Error
```

## Summary

- A macro is a function from forms to forms that runs at compile time.
  `defmacro` defines one; it receives its arguments unevaluated.
- Syntax quote builds templates: `~` inserts a value and `~@` splices a
  sequence. Symbols are qualified with their namespace, and `name#` makes
  a fresh local.
- Skein's macros are hygienic: a macro's locals don't capture the user's
  names, and the user's locals don't capture the macro's. `~'name`
  captures deliberately.
- `macroexpand-1`, `macroexpand`, `macro-steps`, and `skein expand
  --steps` show what a macro produces.
- In a module, a macro body runs at phase 1. Its helpers go in
  `begin-for-syntax` and its libraries in `(:require-for-syntax …)`. The
  REPL top level is more relaxed.
- `skein.syntax` gives hygienic pattern macros: `defsyntax` with
  `syntax-rules` or `syntax-parser`, syntax classes, and errors that point
  at the malformed part.
- Write a macro only when a function can't do the job, and keep it thin.

---

[Contents](README.md) · Previous: [13. Contracts](13-contracts.md) · Next: [15. Fibers and scopes](15-fibers-and-scopes.md) <!-- nav -->
