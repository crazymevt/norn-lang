# Syntax and evaluation

[Reference contents](README.md) · [Symbol index](symbols.md)

## Reader notation

The reader turns source text into forms. The evaluator gives those forms
meaning. Spaces, newlines, and commas separate forms.

| Source | Read as |
| --- | --- |
| `nil`, `true`, `false` | The null value and booleans. |
| `42`, `3/4`, `1.5e3`, `##Inf`, `##-Inf`, `##NaN` | Integers, ratios, and floats. |
| `"line\n"`, `\a`, `\newline` | A string and two character literals. |
| `item`, `app/item` | Unqualified and namespace-qualified symbols. |
| `:item`, `:app/item`, `::item`, `::s/item` | Keywords. The double colon qualifies the keyword with the current namespace, or with the namespace an alias stands for: after `(require '[skein.string :as s])`, `::s/item` reads as `:skein.string/item`. An unknown alias is a read error. |
| `(f x)`, `[x y]`, `{:x x}`, `#{x y}` | A list, vector, map, and set. A map written with the same key twice, or a set with the same item twice, is a read error (`hash-map` and `set` take repeats: the last value of a key stays). |
| `'form`, `@ref`, `#'name` | Reader shorthand for `(quote form)`, `(deref ref)`, and `(var name)`. |
| `` `form ``, `~x`, `~@xs` | Syntax quote, unquote, and unquote-splicing in a template. |
| `#(+ % 1)`, `#(vector %1 %2 %&)` | Anonymous function literals. `%` is the first argument and `%&` collects rest arguments. |
| `#"\d+"` | A compiled regular expression. |
| `#bytes "6869"`, `#queue [1 2]` | A byte sequence from hexadecimal text and a FIFO queue from literal data. |
| `#skein/decimal "12.30"`, `#skein/date "2026-11-03"`, `#inst "…"`, `#uuid "…"` | Tagged literals: the value the tag's reader in `*tag-readers*` makes from the form. Skein's decimals and times print so with `pr-str`. |
| `^:private name`, `^{:doc "Text"} name` | Metadata on the following form. |
| `; text`, `#\| text \|#`, `#_ form`, `#; form` | A line comment, a nestable block comment, and two ways to skip the next form. |

## Evaluation and quotation

Numbers, strings, characters, keywords, booleans, `nil`, and `()` evaluate
to themselves. A symbol resolves to a local binding or a var's value.
Vectors, maps, and sets in code evaluate their elements. In a nonempty
list, a special form or macro in the first position can control evaluation.
In an ordinary call, the operator and arguments are evaluated before the
operator is called. Functions share the same binding rules as other values.

**Special form** · `(quote form)` returns `form` as data. **Special form** ·
`(var name)` returns a var rather than its value.

```clojure
[(quote (+ 1 2)) [(+ 1 2)] (var? #'inc)] ; => [(+ 1 2) [3] true]
```

Keywords, maps, vectors, and sets can also be called. A keyword looks itself
up in the argument, a map looks up an argument as a key, a vector takes an
index, and a set returns a matching member or `nil`. Use `contains?` to
test membership when a set may contain `nil` or `false`.

## Branching and sequencing

| Form | Behavior |
| --- | --- |
| `(if test then)`, `(if test then else)` | Evaluate the test and one branch. The absent else branch yields `nil`. Only `nil` and `false` are falsey. |
| `(do body…)` | Evaluate the forms in order and return the last value, or `nil` for an empty body. |
| `(when test body…)`, `(when-not test body…)` | Run the body conditionally, otherwise return `nil`. |
| `(cond test expr …)`, `(case expr value result …)` | Select the first truthy test, or select by matching a literal value. A final expression in `case` supplies the default. With no match and no default, `case` signals an error. |
| `(and expr…)`, `(or expr…)` | Short-circuit and return the selected operand's value. |
| `(if-let [binding expr] then else)`, `(if-some [binding expr] then else)` | Evaluate the expression once. `if-let` binds a truthy value. `if-some` binds any non-`nil` value. |
| `(-> x form…)`, `(->> x form…)` | Insert each previous result as the next call's first or last argument, respectively. |

```clojure
[(if () :truthy :falsey)
 (if-some [x false] [:bound x] :missing)
 (or nil :ready)
 (-> {:count 1} (update :count inc) :count)] ; => [:truthy [:bound false] :ready 2]
```

## Binding and functions

| Form | Behavior |
| --- | --- |
| `(let [binding init …] body…)` | Bind local names in order. An initializer can use earlier bindings. |
| `(fn [params…] body…)` | Make a function. The parameter vector may end with `& rest-name`. |
| `(fn ([params…] body…) …)` | Make a multi-arity function. An optional name after `fn` is visible inside the function. |
| `(def name expr)` | Set the root value of a namespace var and return the var. |
| `(defonce name expr)` | Like `def`, but only when the var has no value yet, so reloading the file keeps it. A docstring can follow the name. |
| `(defn name [params…] body…)` | Bind a function to a var. A docstring and attribute map can follow the name. Multi-arity syntax follows `fn`. |
| `(defn- name [params…] body…)` | Define a private function in its module. |
| `(letfn [(name [params…] body…) …] body…)` | Define mutually visible local functions. |

`let` and function parameters support destructuring. A vector pattern can
bind positions, remaining items with `&`, and the whole value with `:as`.
A map pattern can bind keys with `:keys`, defaults with `:or`, and the
whole value with `:as`. A missing key without a default binds `nil`.

```clojure
(defn label
  ([person] (label "Name" person))
  ([prefix {:keys [name] :or {name "unknown"}}]
   (str prefix ": " name)))

[(label {:name "Ada"}) (label "User" {})] ; => ["Name: Ada" "User: unknown"]
```

## Iteration and tail calls

**Macro** · `(loop [binding init …] body…)` establishes a target for the
**special form** `(recur expr…)`. Each `recur` argument provides the next
value for a binding. `recur` targets the innermost loop or the current
function arity and requires matching arity and tail position. The compiler
also supports proper tail calls for ordinary function calls in tail position.

Use `doseq` or `dotimes` to iterate for effects. Use `for` to collect the
results of a comprehension into a vector. In the bindings of either,
`:let [name value …]` binds more names, `:when test` skips an item, and
`:while test` stops the binding before it at the first item that fails the
test, as in Clojure. An endless sequence can be cut short that way.

```clojure
(loop [n 4 acc 1]
  (if (zero? n) acc (recur (dec n) (* acc n)))) ; => 24
(for [x [1 2 3] :when (odd? x)] (* x x)) ; => [1 9]
(for [x (range) :while (< x 3)] x) ; => [0 1 2]
```

## Pattern matching

**Macro** · `(match value pattern result …)` evaluates the value once and
returns the result of the first matching clause. Insert `:when guard`
between a pattern and its result to impose an additional test. A symbol
binds a local, `_` matches anything, and repeated names require equal values.
Vector patterns match sequential values, and map patterns require the
specified keys and match their values. A failed match signals `MatchError`.

```clojure
(match {:op :add :args [3 4]}
  {:op :add :args [a b]} (+ a b)
  _ :unknown) ; => 7
```

## Namespaces and modules

**Macro** · `(ns name docstring? clauses…)` enters a namespace. A source
file beginning with `ns` is compiled as a module and supports forward
references. The optional docstring says what the module is for; `skein doc`
shows it. In a script
without `ns`, top-level forms are evaluated in order. Modules have access to
`skein.core` names by default. Use `(:refer-core :exclude [name…])` or
`(:refer-core :rename {old new})` to resolve naming conflicts.

**Function** · `(require spec…)` loads a library once, returning `nil`. A
spec is a namespace symbol or a vector with options. `:as` adds an alias,
and `:refer` brings named public vars into scope. `:refer :all` brings in all
public vars. `:reload` reloads the library and replaces definitions in place.
Private definitions are accessible by ordinary names only inside their module.

The module `my.app` is searched as `my/app.skn` under a library root. Skein
supports bundled libraries, project paths, the `SKEIN_PATH` environment
variable, and paths added with `add-lib-path!`. The file's `ns` form must
name the module: requiring `my.app` from a file that says `(ns my.other)`
is an error.

```clojure
(ns reference.syntax-example
  "An example module."
  (:require [skein.string :as s :refer [join]]))

(s/upper-case (join " " ["hello" "skein"])) ; => "HELLO SKEIN"
```

## Macros and expansion

**Macro** · `(defmacro name [params…] body…)` defines a transformer whose
arguments are unevaluated forms. Its result becomes code in the caller.
Syntax quote builds a template, `~` inserts a value, and `~@` splices a
sequence of forms. Template identifiers retain their binding context.
An identifier ending in `#` inside the template generates a fresh local.

```clojure
(defmacro with-value [expr]
  `(let [value# ~expr]
     [value# value#]))

(let [value 3]
  (with-value (inc value))) ; => [4 4]
```

A macro body runs at compile time. Use `begin-for-syntax` for helper
definitions that run at that phase, and `(:require-for-syntax …)` in `ns`
for compile-time imports. `(for-syntax form)` resolves a form one phase up. See [`skein.syntax`](libraries.md#language-development)
for syntax objects and structured syntax patterns. Use `macroexpand-1`,
`macroexpand`, or [`skein expand`](../tooling.md) to examine expanded code.
The first two expand `defmacro` and `defsyntax` macros alike and return
data; `skein expand` also shows how a hygienic macro's names were kept apart.

In a module, a macro must be defined before its first use. A call above
the macro's definition is an error when the module loads, and `skein lint`
reports it as `:macro-order`.

## Function contracts

Annotate a function's parameters and result to check calls across module
boundaries. A parameter annotation precedes the name. A result annotation
precedes the parameter vector. An argument mismatch signals
`ContractError` blaming the caller, while a result mismatch blames the
defining module. Calls made within the defining module go unchecked.

```clojure
(defn sum ^Integer [^Integer a ^Integer b]
  (+ a b))

(sum 2 3) ; => 5
```

Annotations can also express optional values `^(? T)`, unions
`^(U A B)`, vectors `^(Vec T)`, and functions `^(-> A R)`. Use
`(defcontract name (fn-of [arg-contract…] result-contract))` to attach a
contract built from classes, predicates, or combinators such as `nilable`,
`any-of`, `all-of`, `vec-of`, and `map-of`.

A collection contract, `^(Vec T)` or `^(Map K V)`, checks the collection in
place and passes on the argument itself. Only a function type, `^(-> A R)`
or `fn-of`, wraps the value, so that each later call is checked. Blame for
such a call names a module too: a function passed in as an argument blames
the module that passed it for a bad result, and the receiving function's
module for a bad argument; a function given back as a result blames the
giving module for a bad result, and the caller for a bad argument.
