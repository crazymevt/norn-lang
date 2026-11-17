# 13. Contracts

[Contents](README.md) · Previous: [12. Live programming](12-live-programming.md) · Next: [14. Macros](14-macros.md) <!-- nav -->

Skein is dynamically typed: a function accepts whatever you pass it, and a
wrong value shows up as an error somewhere inside, often far from the
mistake. A *contract* states what a function expects and what it returns,
and Skein checks it where one module calls another. When a check fails, the
error says which side broke the agreement. This chapter covers checked
signatures such as `^Integer`, blame, contracts built from predicates with
`defcontract`, the `*check-contracts*` setting, and what the checks cost.

## A first signature

A signature annotates a function's parameters and its result with types.
A parameter's type goes before its name. The result's type goes before
the parameter vector. A type here is a class, such as `Integer`,
`String`, or `Keyword`.

```clojure
(defn restock ^Integer [^Integer on-hand ^Integer delivered]
  (+ on-hand delivered))
```

Read this as: `restock` takes two integers and returns an integer. The
`^` is Skein's metadata syntax (see [Chapter 3](03-values-and-expressions.md)),
so a signature is ordinary code with a few annotations, and a function
without them is still a valid function.

Contracts are checked on calls that cross a module boundary: from one
module into another, or from the REPL into a module. A call from inside
the function's own namespace isn't checked. So to see a check, the
function has to live in a module. The examples in this chapter write
small modules into a `lib` folder and load them. (Module files and
`add-lib-path!` are covered in [Chapter 8](08-modules-and-projects.md).)

```clojure
(require '[skein.io :as io] '[skein.string :as str])

(io/mkdirs! "lib")
(add-lib-path! "lib")

(io/write-text "lib/inventory.skn" "
(ns inventory)

(defn restock ^Integer [^Integer on-hand ^Integer delivered]
  (+ on-hand delivered))

(defn pallets ^Integer [^Integer units ^Integer per-pallet]
  (/ units per-pallet))
")

(require '[inventory :as inv])
```

A call with the right types runs as usual.

```clojure
(inv/restock 10 5) ; => 15
```

A call with a wrong type stops before `restock` runs, with a
`ContractError`:

```clojure
(inv/restock 10 "5") ; !! ContractError
```

At the REPL, the error reads:

```text
Error: inventory/restock: its 2nd argument should be Integer, got "5", blaming user
```

The message names the function, the argument, the contract, the value
that broke it, and who is to blame. The same facts are on the condition
itself, so a handler can use them:

```clojure
(handler-case (inv/restock 10 "5")
  (ContractError [e]
    [(:function e) (:position e) (:expected e) (:value e)]))
; => ["inventory/restock" "its 2nd argument" "Integer" "5"]
```

`ContractError` is a kind of `TypeError`: a value of the wrong kind,
caught by any handler for `TypeError` or `Error`. The condition machinery
of [Chapter 11](11-conditions-and-restarts.md) applies to it as to any
other error.

## Blame

Without the contract, the same mistake fails inside `+`, which is the
first place that notices a string. That error is true but unhelpful: it
points at `restock`, which did nothing wrong. The contract moves the error
to the boundary and assigns *blame*:

- A bad argument blames the caller. `restock` said what it accepts, and
  the caller passed something else.
- A bad result blames the function's own module. The caller did its part,
  and the function returned something it promised not to.

The idea comes from Racket's contract system. In a program with many
modules, knowing which side of a boundary broke the agreement tells you
which code to fix.

Here is a module that calls `inventory`. It processes delivery lines whose
quantities come from a text file, and it forgets to parse them.

```clojure
(io/write-text "lib/orders.skn" "
(ns orders
  (:require [inventory :as inv]))

(defn receive [on-hand line]
  (inv/restock on-hand (:qty line)))
")

(require 'orders)

(handler-case (orders/receive 10 {:item :bolts :qty "5"})
  (ContractError [e] (:blame e)))
; => "orders (lib/orders.skn)"
```

The REPL called `orders/receive`, but the blame goes to `orders`, the
module that passed the string to `inventory`, along with its file. When
the caller is the REPL, the blame says `user`.

Now the other direction. `pallets` divides, and in Skein dividing two
integers that don't divide evenly gives a ratio (see
[Chapter 3](03-values-and-expressions.md)), not an integer. The function
breaks its own promise:

```clojure
(inv/pallets 12 4) ; => 3
(inv/pallets 10 4) ; !! ContractError

(handler-case (inv/pallets 10 4)
  (ContractError [e] [(:position e) (:value e) (:blame e)]))
; => ["its result" 5/2 "inventory"]
```

```text
Error: inventory/pallets: its result should be Integer, got 5/2, blaming inventory
```

The caller passed two integers, as asked, so the result's failure blames
`inventory`. The fix belongs there: round, or use `quot`, or change the
signature to say `Number`.

## Calls inside a module aren't checked

A module's calls to its own functions go straight to the function, with
no check. Here `inventory2` calls its own `restock` with a string:

```clojure
(io/write-text "lib/inventory2.skn" "
(ns inventory2)

(defn restock ^Integer [^Integer on-hand ^Integer delivered]
  (+ on-hand delivered))

(defn restock-from-text [on-hand text]
  (restock on-hand text))
")

(require 'inventory2)

(inventory2/restock-from-text 10 "5") ; !! TypeError
```

The error is a `TypeError` from `+`, not a `ContractError`. This is
deliberate. A module is one unit, written and changed together, and its
own calls are trusted. The boundary is where code from different authors,
or written at different times, meets, so that's where checking pays off.
It also means an annotated function costs nothing extra when its own
module calls it.

This is what *gradual* typing means in Skein. You add signatures where
they help, usually on a module's public functions, and leave the rest
alone. There is no static type checker: signatures are checked when calls
happen, not when code is compiled. If you want every call checked, even
inside modules, see [Choosing which calls check](#choosing-which-calls-check).

## The type language

A type in a signature is one of these:

| Type | Accepts |
| --- | --- |
| A class: `Integer`, `String`, `Keyword`, `Vector`, `Map`, a class you defined | An instance of the class or a subclass |
| `Any` | Anything |
| `(? T)` | `nil`, or a `T` |
| `(U A B …)` | An `A`, or a `B`, … |
| `(Vec T)` | A vector whose every item is a `T` |
| `(Map K V)` | A map whose keys are `K`s and values `V`s |
| `(-> A … R)` | A function taking `A …` and returning `R` |

The built-in classes include `Number` (with `Integer`, `Ratio`, and
`Float` below it), `String`, `Character`, `Keyword`, `Symbol`, `Boolean`,
`Function`, and the collection classes `Sequential` (lists and vectors),
`Vector`, `List`, `Map`, and `Set`. [Chapter 9](09-classes-and-generic-functions.md)
covers built-in classes and the class hierarchy.

Here is a module that uses each form:

```clojure
(io/write-text "lib/catalog.skn" "
(ns catalog)

(defclass Item () ((sku :initarg :sku) (price :initarg :price)))

(defn label ^String [^(? String) name]
  (or name (str :unnamed)))

(defn sku-text ^String [^(U Integer String) sku]
  (str sku))

(defn total ^Integer [^(Vec Integer) prices]
  (reduce + 0 prices))

(defn price-of ^Integer [^(Map Keyword Integer) prices ^Keyword item]
  (get prices item 0))

(defn adjust-all ^(Vec Integer) [^(-> Integer Integer) f ^(Vec Integer) prices]
  (mapv f prices))

(defn price ^Integer [^Item item]
  (:price item))

(defn sum ^Integer [^Integer first-price & ^Integer more]
  (apply + first-price more))
")

(require '[catalog :as cat])
```

A `(? String)` parameter takes a string or `nil`:

```clojure
(cat/label nil) ; => ":unnamed"
(cat/label "Bolt") ; => "Bolt"
(cat/label :bolt) ; !! ContractError
```

A union accepts any of its members, and the error message lists them:

```clojure
(cat/sku-text 1042) ; => "1042"
(cat/sku-text "B-1042") ; => "B-1042"
(handler-case (cat/sku-text :b1042)
  (ContractError [e] (:expected e))) ; => "Integer or String"
```

`(Vec T)` and `(Map K V)` check the collection's kind and every element.
A list is not a vector, even when its items are right:

```clojure
(cat/total [100 250]) ; => 350
(cat/total [100 "250"]) ; !! ContractError
(cat/total '(100 250)) ; !! ContractError
(cat/price-of {:bolt 25 :nut 10} :nut) ; => 10
(cat/price-of {"bolt" 25} :bolt) ; !! ContractError
```

A class you define works like a built-in one. A map with the same keys
isn't an instance of `Item`:

```clojure
(cat/price (make cat/Item :sku 1 :price 25)) ; => 25
(handler-case (cat/price {:sku 1 :price 25})
  (ContractError [e] (:expected e))) ; => "Item"
```

The types after `&` apply to each of the rest arguments:

```clojure
(cat/sum 1 2 3) ; => 6
(handler-case (cat/sum 1 2 "3")
  (ContractError [e] (:position e))) ; => "its 3rd argument"
```

A multi-arity function gives each arity its own signature, and a call is
checked against the arity it uses. A parameter or result without a type
isn't checked.

Only these forms are types. A predicate such as `even?` isn't one, and
writing `^even?` is an error when the module loads:

```clojure
(io/write-text "lib/oops.skn" "
(ns oops)

(defn half [^even? n] (quot n 2))
")

(require 'oops) ; !! TypeError
```

The message points you to `defcontract`, which takes predicates. It's
covered below.

## Functions as arguments

`(-> Integer Integer)` describes a function. Skein can't look inside a
function to check it, so instead it checks the function's *use*. The
function that crosses the boundary is wrapped, and each call to the
wrapper checks the arguments and the result.

```clojure
(cat/adjust-all inc [100 250]) ; => [101 251]
(cat/adjust-all 5 [100]) ; !! ContractError
```

Passing `5` fails immediately, since it isn't a function. A function that
returns the wrong thing fails when it's called:

```clojure
(handler-case (cat/adjust-all (fn [p] (* p 1.1)) [100])
  (ContractError [e] [(:position e) (:value e) (first (str/split (:blame e) #" "))]))
; => ["its result" 110.00000000000001 "user"]
```

The blame names a module and, when it has one, its file; here the module
is `user`: the function you passed in returned a float where
the contract asked for an integer, so the code that gave it is at fault.
If `adjust-all` itself called `f` with a string, the blame would be
`catalog`, the module that used the function against its contract. A
function a module gives back works the other way round: a bad result
blames that module, and a bad argument blames the code that called it.

Contracts on functions work the same way when a module hands a function
out. A function you get from a module, such as `cat/total` passed as a
value to `map`, is the checked wrapper, so its contract goes wherever it
goes. (The var is still the function's own: `#'cat/total` is
`catalog`'s var, not the wrapper.)

```clojure
(map cat/total [[1 2] [3 4]]) ; => [3 7]
(map cat/total [[1 2] ["3"]]) ; !! ContractError
```

## defcontract and the combinators

Types are classes, but many useful conditions aren't: a positive integer,
a non-empty string, a vector of keywords. `defcontract` gives a function a
contract built from predicates and combinators. It uses `fn-of`, which
takes a vector of argument contracts and a result contract.

```clojure
(io/write-text "lib/pricing.skn" "
(ns pricing)

(defn discount [price percent]
  (- price (quot (* price percent) 100)))

(defcontract discount
  (fn-of [pos-int? (all-of int? #(<= 0 % 100))] nat-int?))
")

(require 'pricing)

(pricing/discount 200 10) ; => 180
(pricing/discount 200 150) ; !! ContractError
(pricing/discount -5 10) ; !! ContractError
```

Any one-argument predicate is a contract, and so is any class. The
combinators build bigger contracts from smaller ones:

| Combinator | Accepts |
| --- | --- |
| `(nilable c)` | `nil`, or what `c` accepts |
| `(any-of c …)` | What any of the contracts accepts |
| `(all-of c …)` | What every one of the contracts accepts |
| `(vec-of c)` | A vector of items `c` accepts |
| `(map-of kc vc)` | A map with keys `kc` accepts and values `vc` accepts |
| `(fn-of [c …] r)` | A function of arguments `c …` returning `r` |

`fn-of` takes a `:rest` option for the arguments after the fixed ones.

The `(U …)` of a signature is `any-of`, `(? T)` is `nilable`, and so on:
signatures are a shorthand for the most common contracts. Use the
combinators when a predicate says what you mean.

Contracts are values. `contract-name` shows what one says, in the words
the error messages use, and `contract?` tells you whether a value is one.

```clojure
(contract-name (fn-of [pos-int? (nilable string?)] (vec-of keyword?)))
; => "(fn-of [pos-int? nil or string?] a vector of keyword?)"
(contract-name (any-of int? String)) ; => "int? or String"
(contract-name (map-of keyword? int?)) ; => "a map of keyword? to int?"
(contract? (vec-of int?)) ; => true
```

Because they're values, you can name a contract with `def` and reuse it:

```clojure
(def percent (all-of int? #(<= 0 % 100)))

(contract? percent) ; => true
(contract-name (nilable String)) ; => "nil or String"
```

An anonymous predicate shows up as `fn` in messages. Give it a name with
`defn` when the message matters.

## Redefining a function keeps its contract current

A checked wrapper reads the function's contract on every call. When you
redefine the function, from the REPL or by reloading its module, the
contract that applies is the one on the new definition. Redefine it
without types and the check goes away. Here `in-ns` switches to
`inventory` so the `defn` replaces the module's function, as an editor
does when you evaluate a form in a file:

```clojure
(in-ns 'inventory)
(defn restock [on-hand delivered] [on-hand delivered])
(in-ns 'user)

(inv/restock 10 "5") ; => [10 "5"]
```

[Chapter 12](12-live-programming.md) covers redefining code in a running
program.

## Choosing which calls check

`*check-contracts*` chooses which calls check contracts:

- `:boundaries`, the default, checks calls from other modules and the
  REPL.
- `:all` checks every call, including a module's calls to its own
  functions. It is a debugging aid for tracking down a bad value inside a
  module.
- `:none` checks nothing.

The setting is read when code is *expanded*, which happens when a module
loads or a form is compiled, not when the call runs. To change how a
module's code checks, bind the variable around loading it. Here
`inventory2`, reloaded with `:all`, catches its own bad call and blames
itself:

```clojure
(binding [*check-contracts* :all]
  (require 'inventory2 :reload))

(handler-case (inventory2/restock-from-text 10 "5")
  (ContractError [e] (:blame e)))
; => "inventory2 (lib/inventory2.skn)"
```

Binding the variable around a call that is already compiled changes
nothing, since the decision was made when the call was expanded. Wrap the
loading instead, or `eval` a form inside the binding:

```clojure
(binding [*check-contracts* :none]
  (eval '(cat/total [1 "2"]))) ; !! TypeError
```

The compiled-module cache keeps code expanded under each setting apart,
so a module compiled with `:none` is never loaded into a program running
with `:boundaries`.

Release builds check signatures too. Checking at boundaries is part of
what a signature means, so a program behaves the same in development and
in production. `:none` is there for a build that explicitly chooses
otherwise.

## What checks cost

An unchecked call into another module takes a few nanoseconds to tens of
nanoseconds. When a signature's types are classes or predicates, its
checks are written into the calling code itself, where the compiler can
inline them: a checked call then costs about 16 nanoseconds with tier 1
and 150 without it. Other contracts, such as a collection's or a
function's, are checked by a wrapper around the call, at about 0.3 to 0.8
microseconds. That's nothing for a function that reads a file or talks to
a database, and noticeable for a tiny function called millions of times
in a loop. Some contracts cost more than others:

- A class or a predicate is one test.
- `(Vec T)`, `(Map K V)`, `vec-of`, and `map-of` check every element, so
  their cost grows with the collection. They check it in place: the
  function receives the very collection the caller passed, not a copy.

- `(-> …)` and `fn-of` wrap the function, so each later call through it
  is checked, and the function the callee receives isn't `identical?` to
  the one passed in.

Some practical advice follows from this:

- Put signatures on a module's public functions, where other code calls
  in. That's where they catch mistakes and where blame is most useful.
- Leave a module's private helpers unannotated. Their calls are never
  checked under the default setting anyway.
- A function that takes a large vector and is called in a hot loop may be
  better served by `Vector` than `(Vec Integer)`, checking the elements
  once where they enter the program.
- Don't use contracts for validating input from users or files. A
  `ContractError` says a programmer broke an agreement. Data from outside
  the program needs its own checks and its own error messages.

The standard libraries follow this advice: many of their public functions
carry signatures, so a wrong argument to one of them is reported at your
call, blaming your code.

```clojure
(require '[skein.csv :as csv])

(handler-case (csv/write-csv "not rows")
  (ContractError [e] [(:function e) (:expected e)]))
; => ["skein.csv/write-csv" "Sequential"]
```

`(source name)` shows a function's definition, signature included. `doc`
shows the parameter names but not their types.

## Summary

- A signature puts a type before each parameter and before the parameter
  vector for the result: `(defn f ^Integer [^String s] …)`.
- Types are classes, `Any`, `(? T)`, `(U A B)`, `(Vec T)`, `(Map K V)`,
  and `(-> A R)`. Predicates aren't types; use `defcontract` for them.
- Contracts are checked on calls from another module or the REPL. A
  module's own calls aren't checked.
- A failed check signals `ContractError` with `:function`, `:position`,
  `:expected`, `:value`, and `:blame`. A bad argument blames the caller,
  and a bad result blames the function's module.
- `defcontract` with `fn-of`, `nilable`, `any-of`, `all-of`, `vec-of`, and
  `map-of` builds contracts from predicates. Contracts are values.
- `*check-contracts*` (`:boundaries`, `:all`, `:none`) is read when code
  is expanded, so bind it around loading a module.
- A checked call with plain types costs tens of nanoseconds, its checks
  inlined where it's called; others well under a microsecond. Collection
  contracts check every element in place, and function contracts wrap
  the function.

---

[Contents](README.md) · Previous: [12. Live programming](12-live-programming.md) · Next: [14. Macros](14-macros.md) <!-- nav -->
