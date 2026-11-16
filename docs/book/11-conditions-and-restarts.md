# 11. Conditions and restarts

[Contents](README.md) · Previous: [10. Protocols and extension](10-protocols-and-extension.md) · Next: [12. Live programming](12-live-programming.md) <!-- nav -->

Programs meet situations they can't handle on their own: a file that isn't
there, a line of input that doesn't parse, a division by zero. Most languages
give you exceptions. Code throws, the stack unwinds to the nearest matching
`catch`, and whatever was happening at the point of the error is gone.

Norn has exceptions, in the form of `try` and `catch`, but underneath them is
the Common Lisp condition system, which separates three questions that
exceptions answer all at once. *Signaling* says that something happened.
*Handling* decides what to do about it. *Restarting* carries out a recovery
that the low-level code offered. A handler runs before the stack unwinds,
while the code that signaled is still waiting, so it can choose to continue
that code in a way the code itself made possible.

This chapter starts with the familiar parts, signaling errors and catching
them, then moves on to handlers that don't unwind, conditions that aren't
errors, restarts, and how to design a library whose callers decide how to
recover. [Chapter 12](12-live-programming.md) shows the same machinery from
the REPL, where an unhandled error stops in the debugger instead of ending
your program.

## Signaling an error

`error` signals an error. Its simplest form takes a message, and any extra
arguments are joined onto it with spaces:

```clojure
(defn check-age [age]
  (when (neg? age)
    (error "Age can't be negative:" age))
  age)

(check-age 30) ; => 30
(check-age -1) ; !! Error
```

The thing that `error` signals is a *condition*, an instance of a class like
any other. A message alone makes a `SimpleError`. You can read the message
back with `condition-message`, and `condition-text` adds where the error
happened, as `file:line`, when that's known:

```clojure
(handler-case (check-age -1)
  (Error [e] [(class-name (class-of e)) (condition-message e)]))
; => [norn.core/SimpleError "Age can't be negative: -1"]
```

`handler-case` is the basic way to catch a condition. It's explained in a
moment; here it catches any `Error` and returns the condition's class and
message.

The runtime signals errors in the same way. Dividing by zero, calling a
function with the wrong number of arguments, or adding a string to a number
each signal a condition of a specific class, with slots that describe what
went wrong:

```clojure
(handler-case (/ 1 0)
  (ArithmeticError [e] (condition-message e)))
; => "Divide by zero"

(handler-case (+ 1 "two")
  (TypeError [e] [(:expected e) (:value e)]))
; => ["a number" "two"]

(handler-case (nth [:a :b] 5)
  (IndexError [e] (:index e)))
; => 5
```

## Condition classes

Conditions form a class hierarchy, and handlers choose what to handle by
class. The top of it looks like this:

- `Condition` is the root. Every condition has a `:message` and a
  `:location`.
  - `Warning` is for something worth reporting that isn't an error.
  - `SeriousCondition` is for situations that need handling.
    - `Error` is the class you'll catch most often.
      - `SimpleError` is what `error` makes from a message.
      - `RuntimeError` covers mistakes in the program that the runtime
        notices: `TypeError` (`:expected`, `:value`), `ArityError`
        (`:function`, `:given`), `UnboundVarError` (`:var`), `IndexError`
        (`:index`), `ArithmeticError`, and `OwnershipError` from
        [Chapter 10](10-protocols-and-extension.md).
      - `IOError` covers the outside world (`:path` or `:address`, and a
        `:reason` keyword): `FileNotFound`, `PermissionDenied`, `FileExists`,
        `ConnectionError` and `TlsError`.
      - `TimeoutError` (`:after`) is for anything that waited too long.
      - `ParseError` (`:source`, `:line`, `:column`) is for text that isn't
        what it should be: source code, EDN, JSON, URIs, HTTP messages.
      - `ExInfo` is what `ex-info` makes, described below.

The [conditions reference](../reference/conditions.md) and the library
chapters list more. Since handlers match subclasses, a handler for `IOError`
catches a missing file and a refused connection, and a handler for `Error`
catches almost anything.

Your own conditions are classes too. Subclass `Error` (or another condition
class) with `defclass`, and give it slots for what a handler will need to
know:

```clojure
(defclass insufficient-funds (Error)
  ((balance :required true)
   (requested :required true)))
```

Signal it by passing the class and its slot values to `error`. Include a
`:message` for people reading logs:

```clojure
(defn withdraw [balance amount]
  (if (> amount balance)
    (error insufficient-funds
           :message (str "Can't withdraw " amount " from " balance)
           :balance balance
           :requested amount)
    (- balance amount)))

(handler-case (withdraw 100 250)
  (insufficient-funds [e] {:short-by (- (:requested e) (:balance e))}))
; => {:short-by 150}
```

A handler that reads slots doesn't need to parse the message, and it keeps
working when someone rewords it. That's the reason to define a class instead
of signaling a `SimpleError` with a careful message.

`error` also accepts a condition you've already made, and `throw` is another
name for it, for readers coming from Clojure. Given a string, `throw` makes
a `SimpleError`.

## Catching conditions

### handler-case

`handler-case` evaluates a form. If the form signals a condition that
matches one of its clauses, the stack unwinds back to the `handler-case`,
and that clause's body gives the result. If nothing is signaled, the form's
value is the result:

```clojure
(defn safe-divide [a b]
  (handler-case (/ a b)
    (ArithmeticError [e] :undefined)
    (TypeError [e] :not-a-number)))

(safe-divide 10 2)   ; => 5
(safe-divide 10 0)   ; => :undefined
(safe-divide 10 "x") ; => :not-a-number
```

Each clause is a class, a vector naming the condition, and a body. Use `[]`
when the body doesn't need the condition. When several clauses match, the
first one listed wins, so put specific classes before general ones:

```clojure
(handler-case (/ 1 0)
  (ArithmeticError [] :arithmetic)
  (Error [] :some-error)) ; => :arithmetic
```

`ignore-errors` is the bluntest form: it returns `nil` if its body signals
any `Error`.

```clojure
(ignore-errors (/ 1 0)) ; => nil
(ignore-errors (+ 1 2)) ; => 3
```

### try, catch and finally

Clojure's `try` works as Clojure programmers expect. It is built from
`handler-case`, plus `unwind-protect` for `finally`:

```clojure
(def log (atom []))

(defn load-setting [k settings]
  (try
    (swap! log conj [:loading k])
    (or (get settings k) (error "No setting" k))
    (catch Error e
      (swap! log conj [:failed (condition-message e)])
      :default)
    (finally
      (swap! log conj [:done k]))))

(load-setting :port {:port 8080}) ; => 8080
(load-setting :host {})           ; => :default
@log
; => [[:loading :port] [:done :port]
;     [:loading :host] [:failed "No setting :host"] [:done :host]]
```

There is no `Exception` or `Throwable` class in Norn. Catch `Error` for
errors, or `Condition` for anything at all.

### ex-info

When you want to attach data to an error without defining a class, use
`ex-info`, as in Clojure. It makes an `ExInfo` condition from a message, a
map, and optionally a cause:

```clojure
(defn lookup-user [id]
  (throw (ex-info "User not found" {:id id :table :users})))

(try
  (lookup-user 42)
  (catch ExInfo e
    [(ex-message e) (ex-data e)]))
; => ["User not found" {:id 42 :table :users}]
```

The third argument records what caused this error, which you read with
`ex-cause`. It lets you wrap a low-level error in one that makes sense at
your level, without losing the original:

```clojure
(defn read-port [text]
  (try
    (/ 65536 (count text))
    (catch ArithmeticError e
      (throw (ex-info "Empty port setting" {:text text} e)))))

(try (read-port "")
  (catch ExInfo e
    [(ex-message e) (class-name (class-of (ex-cause e)))]))
; => ["Empty port setting" norn.core/ArithmeticError]
```

`ex-info` is good for one-off errors in application code. In a library,
define condition classes: callers can handle them by class, and the slots
document what each error carries.

### Cleaning up

`unwind-protect` runs cleanup code however its form is left: by returning,
by an error unwinding past it, by a restart, or by the fiber being cancelled.
`finally` is the same thing in `try` clothing.

```clojure
(def open-files (atom #{}))

(defn with-pretend-file [name f]
  (swap! open-files conj name)
  (unwind-protect
    (f name)
    (swap! open-files disj name)))

(with-pretend-file "a.txt" (fn [n] (count n))) ; => 5
(ignore-errors (with-pretend-file "b.txt" (fn [n] (/ 1 0)))) ; => nil
@open-files ; => #{}
```

For real files, sockets and other resources, `with-open`
([Chapter 19](19-files-and-streams.md)) does this for you.

## Handlers that don't unwind

Everything so far unwinds: by the time a `catch` body runs, the code that
signaled is gone. `handler-bind` is different. Its handlers are functions
that run *at the point of the signal*, on top of the stack, while the
signaling code is still waiting for `error` to return. A handler can then do
one of three things:

- Return normally. This *declines*: the condition goes on to the next
  handler outward, as if this one weren't there.
- Transfer control somewhere else, such as invoking a restart (next section)
  or unwinding to an outer `handler-case`.
- Signal a new condition of its own.

Declining makes `handler-bind` the right tool for watching errors without
deciding their fate. Here a handler logs every error on its way out, and an
outer `handler-case` still catches it:

```clojure
(def seen (atom []))

(handler-case
  (handler-bind [Error (fn [e] (swap! seen conj (condition-message e)))]
    (withdraw 10 20))
  (insufficient-funds [e] :refused)) ; => :refused

@seen ; => ["Can't withdraw 20 from 10"]
```

The handler ran first, because handlers are tried innermost first, and
returned, so the search continued to the `handler-case`. While a handler
runs, only the handlers outside it are active, so a handler that signals an
error doesn't call itself.

`handler-bind` takes pairs of a class and a function, like `let` bindings,
and the handlers are active for its whole body. Where a handler sits matters.
A condition is offered to the innermost handler first, then outward, and the
first handler that transfers control ends the search. So a `handler-bind`
inside an `ignore-errors` sees each error before `ignore-errors` swallows it:

```clojure
(ignore-errors
  (handler-bind [ArithmeticError (fn [e] (swap! seen conj :math))]
    (/ 1 0))) ; => nil

@seen ; => ["Can't withdraw 20 from 10" :math]
```

With the two the other way around, the `handler-bind` would never hear of
the error: `ignore-errors` is innermost, and it unwinds as soon as its
handler is called.

## Conditions that aren't errors

Not every condition is a problem. `signal` offers a condition to the active
handlers and, if they all decline, returns `nil`. The program carries
on. That makes conditions a way for low-level code to tell interested
callers about events, without knowing who they are:

```clojure
(defclass progress (Condition)
  ((done :required true)
   (total :required true)))

(defn copy-items [items]
  (loop [i 0 out []]
    (if (< i (count items))
      (do (signal (progress :done i :total (count items)))
          (recur (inc i) (conj out (nth items i))))
      out)))

(def quiet (copy-items [:a :b]))
quiet ; => [:a :b]

(def updates (atom []))

(def watched
  (handler-bind [progress (fn [p] (swap! updates conj (:done p)))]
    (copy-items [:a :b :c])))

watched  ; => [:a :b :c]
@updates ; => [0 1 2]
```

With no handler, `copy-items` runs quietly. A caller who wants a progress bar
binds a handler, and nothing in `copy-items` changes.

`warn` signals a `Warning`. If no handler transfers control, it prints the
warning and returns `nil`:

```clojure
(def printed (with-out-str (warn "Cache is nearly full")))

printed ; => "WARNING: Cache is nearly full\n"
```

A `handler-bind` handler can see warnings as they're signaled, to count or
log them. To silence one, the handler calls `muffle-warning`. While `warn`
signals, it offers a restart of that name, and invoking it skips the
printing. The code after the `warn` carries on, and `warn` returns `nil`:

```clojure
(with-out-str
  (handler-bind [Warning (fn [_] (muffle-warning))]
    (warn "Cache is nearly full"))) ; => ""
```

The difference between `signal` and `error` is what happens when no handler
takes the condition. `signal` returns `nil`. `error` hands the condition to
the debugger, which is the subject of the end of this chapter.

## Restarts

So far a handler's choices are to decline or to unwind to some point it
knows about. Restarts give it more choices, provided by the code that
signaled. A restart is a named way to continue, established with
`restart-case` around the risky code:

```clojure
(defn parse-qty [s]
  (restart-case
    (or (parse-long s)
        (error "Not a quantity:" (pr-str s)))
    (use-zero [] :report "Use 0 for this quantity." 0)
    (use-qty [n] :report "Use a quantity you supply." n)))
```

`restart-case` evaluates its first form with the restarts available. A
restart clause looks like a function: a name, a parameter vector, an
optional `:report` string, and a body. If nobody invokes a restart, the
form's value is returned as usual:

```clojure
(parse-qty "12") ; => 12
```

A handler chooses a restart with `invoke-restart`, giving its name and
arguments. Control unwinds to the `restart-case`, and the restart's body
gives its value:

```clojure
(handler-bind [Error (fn [e] (invoke-restart 'use-zero))]
  (parse-qty "twelve")) ; => 0

(handler-bind [Error (fn [e] (invoke-restart 'use-qty 1))]
  (mapv parse-qty ["3" "lots" "5"])) ; => [3 1 5]
```

Look at the second example. The handler is outside `mapv`, and the restart
is inside `parse-qty`, deep in the call. After the restart, `parse-qty`
returned 1 and `mapv` carried on with `"5"`. Unwinding went only as far as
the restart, not out to the handler. That is what exceptions can't do: the
recovery happens where the problem was, but the decision is made far away,
by code that knows the policy.

`compute-restarts` lists the restarts available at the moment, innermost
first, each a map with a `:name` and `:report`. `find-restart` looks one up
by name, returning `nil` when it isn't available, so a handler can check
before it chooses:

```clojure
(handler-bind [Error (fn [e]
                       (swap! updates conj (mapv :name (compute-restarts)))
                       (if (find-restart 'use-zero)
                         (invoke-restart 'use-zero)
                         (invoke-restart 'use-qty -1)))]
  (parse-qty "?")) ; => 0

(last @updates) ; => [use-zero use-qty]
```

Invoking a restart that isn't available is an error. The `:report` strings
are what the debugger shows when it lists restarts, so write them as short
instructions.

`with-simple-restart` is a shorthand for the common case of one restart that
takes no arguments and returns `nil`:

```clojure
(defn risky-step [n]
  (with-simple-restart skip-step "Skip this step."
    (when (odd? n) (error "Step failed:" n))
    n))

(handler-bind [Error (fn [_] (invoke-restart 'skip-step))]
  (mapv risky-step [1 2 3 4])) ; => [nil 2 nil 4]
```

## Recovering from runtime errors

The errors the runtime signals come with two restarts already in place:

- `use-value` makes the failed operation return a value you supply.
- `retry` runs the failed operation again.

These are established at the failing operation itself, the division, the
call, the variable lookup, so a handler can patch a single step of a larger
computation:

```clojure
(handler-bind [ArithmeticError (fn [_] (invoke-restart 'use-value 0))]
  (+ 1 (/ 10 0) 2)) ; => 3
```

Only the division was replaced. The additions around it ran as written.

`retry` is for when the handler can fix the cause. Here the handler defines a
missing function the first time, then retries the call that failed:

```clojure
(declare discount)

(defn price-after [p] (- p (discount p)))

(handler-bind [UnboundVarError
               (fn [e]
                 (defn discount [p] (quot p 10))
                 (invoke-restart 'retry))]
  (price-after 200)) ; => 180
```

This looks odd in a program, and you won't write it often in one. At the
REPL it's routine: the debugger offers these restarts for every runtime
error, and you fix the code and pick `retry`.
[Chapter 12](12-live-programming.md) shows that workflow.

## Designing recoverable APIs

Restarts change how you design code that can fail. Instead of picking one
error policy, a function offers the reasonable ways to continue, and lets
its callers pick. Here is a small example worked through: parsing order
lines of the form `item,quantity`.

```clojure
(require '[norn.string :as string])

(defclass bad-order (Error)
  ((line :required true)))

(defn parse-order [line]
  (restart-case
    (let [[item qty] (string/split line #",")
          n (when qty (parse-long (string/trim qty)))]
      (if (and (seq item) n (pos? n))
        {:item item :qty n}
        (error bad-order :message (str "Bad order line: " (pr-str line)) :line line)))
    (skip-order [] :report "Leave this line out." nil)
    (use-order [order] :report "Use this order in its place." order)))

(defn parse-orders [lines]
  (vec (keep parse-order lines)))
```

`parse-order` knows how a line can be wrong and what the alternatives are.
It doesn't know whether a bad line should stop everything, be dropped, or be
fixed up. `parse-orders` doesn't decide either; `keep` drops the `nil` a
skipped line gives.

```clojure
(def lines ["widget,3" "gadget,x" "sprocket, 2" ",4"])
```

The caller sets the policy. With no handler, a bad line is an error, which
is the right default:

```clojure
(parse-orders lines) ; !! bad-order
```

A lenient importer skips bad lines:

```clojure
(handler-bind [bad-order (fn [_] (invoke-restart 'skip-order))]
  (parse-orders lines))
; => [{:item "widget" :qty 3} {:item "sprocket" :qty 2}]
```

Another keeps going but records what it skipped, using the condition's slot:

```clojure
(def rejected (atom []))

(handler-bind [bad-order (fn [e]
                           (swap! rejected conj (:line e))
                           (invoke-restart 'skip-order))]
  (parse-orders lines))
; => [{:item "widget" :qty 3} {:item "sprocket" :qty 2}]

@rejected ; => ["gadget,x" ",4"]
```

A third repairs what it can, here giving an order with a bad quantity a
quantity of one, and skipping the rest:

```clojure
(defn repair [e]
  (let [item (first (string/split (:line e) #","))]
    (if (seq item)
      (invoke-restart 'use-order {:item item :qty 1})
      (invoke-restart 'skip-order))))

(handler-bind [bad-order repair]
  (parse-orders lines))
; => [{:item "widget" :qty 3} {:item "gadget" :qty 1} {:item "sprocket" :qty 2}]
```

And a strict caller who wants a report rather than a partial result uses
`handler-case`, which unwinds:

```clojure
(handler-case (parse-orders lines)
  (bad-order [e] {:error (condition-message e)}))
; => {:error "Bad order line: \"gadget,x\""}
```

Four policies, and `parse-order` didn't change for any of them. Some
guidelines from this example:

- Signal a condition class of your own, with slots for what a handler needs
  to decide. Here `:line` let handlers log and repair without parsing a
  message.
- Offer restarts that cover the real alternatives, named for what they do,
  with `:report` text. Common shapes are skip this item, use this value
  instead, and try again.
- Put the restarts at the level where continuing makes sense: around one
  line, not around the whole file.
- Keep the default strict. With no handler, the error should stop the
  program, or stop in the debugger at the REPL, where someone can see it.

Handlers from `handler-bind` are conveyed to the fibers a computation
spawns, so a policy set at the top applies to work done in parallel too.
`handler-case` and `restart-case` are not conveyed, since they unwind to a
point on their own fiber's stack. [Chapter 15](15-fibers-and-scopes.md)
covers this in detail.

```clojure
(handler-bind [bad-order (fn [_] (invoke-restart 'skip-order))]
  (pmap parse-order lines))
; => [{:item "widget" :qty 3} nil {:item "sprocket" :qty 2} nil]
```

## When nothing handles an error

If `error` finds no handler that transfers control, it calls the debugger.
What that means depends on where the code runs.

In a script run with `norn FILE.norn`, or with `norn -e`, the fiber unwinds,
running its cleanups, and ends with the error. For the main program, Norn
prints the error with a trace of the calls and exits with status 1:

```text
$ norn crash.norn
before
Error: crash.norn:3: Divide by zero
  at user/f
  at norn.core/%with-bindings*
  at norn.core/%with-bindings*
  at norn.expander/eval-reader/fn/fn
  at norn.core/%with-bindings*
  at norn.core/%unwind-protect*
  at norn.core/load-file
```

At the REPL, or in an editor connected to Norn, the fiber stops where it
is instead, with its whole stack intact, and waits. You can look at its
frames and local variables, fix the code, and choose one of the restarts
that were available when it stopped, including your own. That's the subject
of [Chapter 12](12-live-programming.md).

## Summary

- `error` signals an error condition, from a message, a class with slot
  values, or a condition object. `throw` and `ex-info` work as in Clojure.
- Conditions are class instances. Handlers choose by class, and read slots
  such as `:expected`, `:path` or your own, instead of parsing messages.
- `handler-case` and `try`/`catch` unwind to the handler. `finally` and
  `unwind-protect` run cleanup however a form is left.
- `handler-bind` handlers run before unwinding, at the point of the signal.
  They can decline, invoke a restart, or unwind.
- `signal` and `warn` report conditions that need not stop the program.
- `restart-case` offers named ways to continue, and `invoke-restart`
  chooses one from a handler. Runtime errors offer `use-value` and `retry`.
- Design libraries to signal specific conditions and offer restarts, and let
  callers choose the policy with handlers.

---

[Contents](README.md) · Previous: [10. Protocols and extension](10-protocols-and-extension.md) · Next: [12. Live programming](12-live-programming.md) <!-- nav -->
