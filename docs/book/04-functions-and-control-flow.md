# 4. Functions and control flow

[Contents](README.md) · Previous: [3. Values and expressions](03-values-and-expressions.md) · Next: [5. Collections](05-collections.md) <!-- nav -->

This chapter covers the forms you write in nearly every function: local
names with `let`, functions with `fn` and `defn`, several arities and
rest arguments, destructuring, closures, and functions that take or return
other functions. Then it turns to control flow: conditionals, the
binding conditionals such as `if-let`, the threading macros, loops and
proper tail calls, and pattern matching with `match`.

The examples share a small shopping cart, built up as the chapter goes.
Each line is a map, and prices are exact ratios so the totals come out
exact.

```clojure
(def cart
  [{:item "pen"      :qty 3 :price 3/2}
   {:item "notebook" :qty 1 :price 4}
   {:item "ink"      :qty 2 :price 7/4}])
```

## Local names with let

`let` gives names to values for the length of its body. The bindings come
in a vector of name and value pairs, evaluated in order, and each value can
use the names bound before it. The body's last form is the result.

```clojure
(let [line  (first cart)
      qty   (:qty line)
      price (:price line)]
  (* qty price)) ; => 9/2
```

A `let` name is not a variable. It is bound once and never changes, so
there is no assignment to look out for while you read the body. Binding
the same name again in a later pair, or in an inner `let`, *shadows* the
earlier one: the inner name hides the outer one, and the outer value is
untouched.

```clojure
(let [x 1
      x (+ x 10)]
  x) ; => 11
(let [x 1]
  [(let [x 2] x) x]) ; => [2 1]
```

When you need something that changes over time, you use a reference type
such as an atom, covered in [Chapter 17](17-shared-state.md).

## Functions

`fn` makes a function. It takes a vector of parameter names and a body,
and the body's last form is the return value. There is no `return`
statement.

```clojure
(fn [line] (* (:qty line) (:price line)))
((fn [line] (* (:qty line) (:price line))) (first cart)) ; => 9/2
```

The first form makes a function and throws it away. The second makes one
and calls it at once, because it sits in the first position of a list.

`defn` makes a function and gives it a global name in one step. A
docstring can follow the name; `doc` shows it.

```clojure
(defn line-total
  "The cost of one cart line: quantity times unit price."
  [line]
  (* (:qty line) (:price line)))

(line-total (second cart)) ; => 4
(map line-total cart) ; => [9/2 4 7/2]
```

`(defn name …)` is shorthand for `(def name (fn name …))`. The function
is an ordinary value held in a var, which is why you can pass
`line-total` to `map` like any other argument.

For short functions there is a reader shorthand, `#(…)`. Inside it, `%`
is the first argument, `%1`, `%2`, and so on are positional arguments, and
`%&` collects any rest arguments.

```clojure
(#(* % %) 7) ; => 49
(#(str %1 " x " %2) "pen" 3) ; => "pen x 3"
(map #(:item %) cart) ; => ["pen" "notebook" "ink"]
```

Use `#(…)` for a one-line function whose meaning is obvious. Once you
need a name for the argument to make the code clear, switch to `fn`.
`#(…)` forms do not nest.

### Several arities

A function can have several bodies, one for each number of arguments it
accepts. Each body is a list starting with its own parameter vector. A
common use is supplying defaults: the short arity calls the long one.

```clojure
(defn greet
  ([] (greet "world"))
  ([name] (greet "Hello" name))
  ([greeting name] (str greeting ", " name ".")))

(greet) ; => "Hello, world."
(greet "Ada") ; => "Hello, Ada."
(greet "Welcome" "Ada") ; => "Welcome, Ada."
```

Calling a function with a number of arguments none of its bodies accepts
signals an `ArityError`.

```clojure
(greet "a" "b" "c") ; !! ArityError
```

### Rest arguments

A parameter vector can end with `&` and one more name. That name is bound
to a list of the remaining arguments, or `nil` when there are none.

```clojure
(defn tag [label & items]
  [label items])

(tag :a) ; => [:a nil]
(tag :a 1 2 3) ; => [:a (1 2 3)]
```

`apply` goes the other way: it calls a function with the items of a
collection as separate arguments. Any arguments before the collection are
passed first.

```clojure
(apply + [1 2 3]) ; => 6
(apply max 4 [1 9 2]) ; => 9
(apply str "a" ["b" "c"]) ; => "abc"
```

If the rest parameter is a map pattern (see the next section), the
remaining arguments are read as keyword and value pairs. This is the usual
way to accept named options.

```clojure
(defn cart-total [cart & {:keys [tax] :or {tax 0}}]
  (let [subtotal (apply + (map line-total cart))]
    (+ subtotal (* subtotal tax))))

(cart-total cart) ; => 12
(cart-total cart :tax 1/10) ; => 66/5
```

## Destructuring

Data in Skein is mostly vectors and maps, and functions often need a few
parts of them. *Destructuring* lets a binding name those parts directly.
It works anywhere a name is bound: `let`, function parameters, `loop`,
`for`, `doseq`, and the binding conditionals later in this chapter.

### Vector patterns

A vector in a binding position matches a sequential value by position.
`&` binds the rest, `:as` binds the whole value, and `_` is the
conventional name for a position you don't need. Missing positions bind
`nil`.

```clojure
(let [[a b] [1 2 3]] [a b]) ; => [1 2]
(let [[a b & more :as all] [1 2 3 4]]
  {:a a :b b :more more :all all}) ; => {:a 1 :b 2 :more (3 4) :all [1 2 3 4]}
(let [[_ second-item] ["x" "y"]] second-item) ; => "y"
(let [[a b] [1]] [a b]) ; => [1 nil]
```

Patterns nest, and a vector pattern also matches lists, strings, and
ranges, since it only reads items by position.

```clojure
(let [[x [y z]] [1 [2 3]]] (+ x y z)) ; => 6
(let [[first-char] "skein"] first-char) ; => \s
```

### Map patterns

A map in a binding position reads values out of a map by key. The
general form pairs a binding with the key to read. Usually the binding
name matches a keyword key, and `:keys` says that more briefly.

```clojure
(let [{item :item qty :qty} (first cart)] [item qty]) ; => ["pen" 3]
(let [{:keys [item qty]} (first cart)] [item qty]) ; => ["pen" 3]
```

`:or` supplies defaults for keys that are missing, and `:as` binds the
whole map. `:strs` and `:syms` work like `:keys` for string and symbol
keys.

```clojure
(let [{:keys [item qty] :or {qty 1} :as line} {:item "pad"}]
  [item qty line]) ; => ["pad" 1 {:item "pad"}]
(let [{:strs [name]} {"name" "Ada"}] name) ; => "Ada"
```

A default applies only when the key is absent. A key that is present with
the value `nil` binds `nil`.

```clojure
(let [{:keys [qty] :or {qty 1}} {:qty nil}] qty) ; => nil
```

Destructuring in a parameter vector makes small functions read like a
description of their input. Here is `line-total` again, rewritten:

```clojure
(defn line-total [{:keys [qty price]}]
  (* qty price))

(line-total (first cart)) ; => 9/2
```

Patterns combine. A map pattern can contain a vector pattern and the other
way round.

```clojure
(let [{[first-line] :lines} {:lines cart}]
  (:item first-line)) ; => "pen"
(let [[{:keys [item]} & others] cart]
  [item (count others)]) ; => ["pen" 2]
```

## Closures

A function can use names from the scope where it was created, and it keeps
them after that scope is gone. A function that does this is a *closure*.

```clojure
(defn price-over [limit]
  (fn [line] (> (:price line) limit)))

(def pricey? (price-over 3/2))
(pricey? (first cart)) ; => false
(filter pricey? cart) ; => [{:item "notebook" :qty 1 :price 4} {:item "ink" :qty 2 :price 7/4}]
```

`price-over` returns a new function each time it is called, each
remembering its own `limit`. Because `let` names never change, a closure
captures a value, not a variable that might be updated later. A closure
that needs private changing state captures an atom:

```clojure
(defn make-counter []
  (let [n (atom 0)]
    (fn [] (swap! n inc))))

(def next-id (make-counter))
(next-id) ; => 1
(next-id) ; => 2
```

## Functions that work on functions

Functions are values, so a function can take functions as arguments and
return new ones. You have already used `map`, `filter`, and `reduce`,
which take a function and apply it across a collection. ([Chapter
6](06-sequences-and-transducers.md) covers those in depth.) A few more
build new functions from old ones:

| Function | Result |
| --- | --- |
| `(comp f g)` | A function that calls `g`, then `f` on the result |
| `(partial f a)` | `f` with its first arguments filled in |
| `(juxt f g)` | A function returning `[(f x) (g x)]` |
| `(complement f)` | A function returning the logical opposite of `f` |
| `(constantly x)` | A function that ignores its arguments and returns `x` |
| `(fnil f x)` | `f`, with a `nil` first argument replaced by `x` |
| `(every-pred p q)`, `(some-fn p q)` | Combined predicates |
| `(memoize f)` | `f`, remembering each result by its arguments |

```clojure
((comp inc line-total) (first cart)) ; => 11/2
((partial * 2) 21) ; => 42
(map (juxt :item line-total) cart) ; => [["pen" 9/2] ["notebook" 4] ["ink" 7/2]]
(filter (complement pricey?) cart) ; => [{:item "pen" :qty 3 :price 3/2}]
((fnil inc 0) nil) ; => 1
((every-pred pos? even?) 2 4 6) ; => true
```

`comp` applies its functions right to left, as mathematical composition
does: `((comp f g) x)` is `(f (g x))`.

### Local functions: named fn and letfn

A function made with `fn` can have a name of its own, written after `fn`.
The name is visible only inside the function, which lets an anonymous
function call itself.

```clojure
((fn countdown [n] (if (zero? n) [:liftoff] (cons n (countdown (dec n))))) 3) ; => (3 2 1 :liftoff)
```

`letfn` binds several local functions at once, and each can call the
others, including ones defined after it.

```clojure
(letfn [(ev? [n] (if (zero? n) true (od? (dec n))))
        (od? [n] (if (zero? n) false (ev? (dec n))))]
  [(ev? 10) (od? 7)]) ; => [true true]
```

## Making decisions

### if, when, and do

`(if test then else)` evaluates `test`, then exactly one branch. Leave
out the else branch and a false test gives `nil`. Remember that only
`nil` and `false` are false ([Chapter 3](03-values-and-expressions.md)).

```clojure
(if (empty? cart) :empty :has-items) ; => :has-items
(if (empty? []) :empty) ; => :empty
```

Each branch of `if` is one form. When a branch needs several steps, use
`do`, or use `when` if there is no else branch. `when` runs its whole body
when the test is true and returns `nil` otherwise. `when-not` and `if-not`
invert the test.

```clojure
(when (> (count cart) 2)
  (println "big order")
  :big)
; prints: big order
(when-not (empty? cart) :proceed) ; => :proceed
```

### cond, case, and condp

`cond` takes test and result pairs and returns the result of the first
test that is true. By convention the last test is `:else`, which is a
keyword and so always true. With no match, `cond` returns `nil`.

```clojure
(defn shipping [total]
  (cond
    (>= total 50) 0
    (>= total 10) 5
    :else 8))

(map shipping [60 12 3]) ; => [0 5 8]
```

`case` compares a value against constants written in the code, which
says more clearly than `cond` what is being compared. Several constants can share a
result by grouping them in a list. A final lone expression is the default.
Without a default, a value that matches nothing signals an error, which
catches missing cases early.

```clojure
(defn day-kind [day]
  (case day
    (:sat :sun) :weekend
    :mon        :back-to-work
    :weekday))

(map day-kind [:sun :mon :wed]) ; => [:weekend :back-to-work :weekday]
(case :purple :red 1 :blue 2) ; !! SimpleError
```

The constants are not evaluated, so they can be numbers, strings,
keywords, `nil`, and even vectors, but not names of other values.

`condp` sits in between: it calls a predicate with each test value and
the expression, and returns the first matching result.

```clojure
(condp < 7
  10 :large
  5  :medium
  :small) ; => :medium
```

That reads as "the first of `(< 10 7)`, `(< 5 7)`" and, since `(< 5 7)` is
true, gives `:medium`.

### and and or

`and` and `or` short-circuit, and they return one of their operands
rather than a boolean. `and` returns the first false value, or the last
value if all are true. `or` returns the first true value, or the last value
if none is.

```clojure
(and 1 2 3) ; => 3
(and 1 nil 3) ; => nil
(or nil false :fallback) ; => :fallback
(or (:discount (first cart)) 0) ; => 0
```

The last line is a common idiom: `or` supplies a default for a missing
value.

### Testing and binding together

A frequent pattern is "compute something; if there is a result, use it".
`if-let` and `when-let` do both: they bind the value to a name and take
the branch for a true value. The binding can destructure.

```clojure
(defn find-line [cart item]
  (first (filter #(= item (:item %)) cart)))

(if-let [line (find-line cart "ink")]
  (line-total line)
  0) ; => 7/2
(when-let [{:keys [qty]} (find-line cart "stapler")]
  qty) ; => nil
```

`if-let` treats `false` as "no result". When `false` is a legitimate
value, use `if-some` and `when-some`, which take the else branch only for
`nil`.

```clojure
(if-let [x false] [:found x] :none) ; => :none
(if-some [x false] [:found x] :none) ; => [:found false]
```

`when-first` binds the first item of a collection and runs its body only
when the collection is not empty.

```clojure
(when-first [line cart] (:item line)) ; => "pen"
(when-first [line []] (:item line)) ; => nil
```

## Threading macros

Nested calls read inside out. `(reduce + (map line-total (filter pricey?
cart)))` has to be read from the middle. The *threading macros* let you
write the same thing as a pipeline, top to bottom.

`->>` inserts each result as the **last** argument of the next form.
Sequence functions take their collection last, so `->>` suits them.

```clojure
(->> cart
     (filter pricey?)
     (map line-total)
     (reduce +)) ; => 15/2
```

`->` inserts each result as the **first** argument. Functions on a single
value, such as `assoc` and `update`, take that value first, so `->` suits
them. A bare symbol or keyword in the pipeline is called with one
argument.

```clojure
(-> (first cart)
    (assoc :qty 10)
    (update :price * 2)
    line-total) ; => 30
```

Neither macro does anything at run time. They rearrange code before it is
compiled, and `macroexpand` shows the result.

```clojure
(macroexpand '(-> x (f a) g)) ; => (g (f x a))
```

Three relatives cover other cases:

- `some->` and `some->>` stop and return `nil` as soon as a step gives
  `nil`, so a missing value doesn't reach a function that can't take it.
- `cond->` and `cond->>` take test and form pairs and apply only the forms
  whose test is true.
- `as->` names the threaded value, for pipelines where it moves between
  positions.

```clojure
(some-> cart (get 10) :item) ; => nil
(defn apply-code [total code]
  (cond-> total
    (= code :half)   (/ 2)
    (= code :ten-off) (- 10)))
[(apply-code 40 :half) (apply-code 40 :ten-off) (apply-code 40 nil)] ; => [20 30 40]
(as-> cart c
  (map :qty c)
  (apply + c)
  (* c 2)) ; => 12
```

## Loops and recursion

Skein has no `for (i = 0; …)` loop that updates a counter. Repetition is
either a sequence function such as `map` or `reduce`, which is the usual
choice, or recursion.

### loop and recur

`loop` looks like `let`, but it also marks a point you can jump back to.
`recur` jumps there with new values for the bindings. The loop ends when
a branch returns something other than a `recur`.

```clojure
(defn total-qty [cart]
  (loop [lines cart
         acc   0]
    (if (empty? lines)
      acc
      (recur (rest lines) (+ acc (:qty (first lines)))))))

(total-qty cart) ; => 6
```

`recur` also works directly in a function body, where it jumps back to the
start of the function with new arguments. It must be in *tail position*:
the last thing the loop or function does before returning. The compiler
rejects a `recur` anywhere else, and one with the wrong number of values.

```clojure
(defn sum-to [n]
  (loop [i 0 acc 0]
    (if (> i n) acc (recur (inc i) (+ acc i)))))

(sum-to 100) ; => 5050
```

For effects rather than a value, `dotimes` repeats a body a fixed number
of times and `while` repeats it while a test holds. `doseq`, for walking a
collection, is in [Chapter 6](06-sequences-and-transducers.md).

```clojure
(dotimes [i 3] (println "step" i))
; prints: step 0
; prints: step 1
; prints: step 2
```

### Proper tail calls

Clojure needs `recur` because the JVM cannot reuse a stack frame for an
ordinary call. Skein can. **Every call in tail position is a proper tail
call**: it replaces the caller's frame instead of adding one. So a
function can call itself, or call another function that calls it back,
any number of times, and the stack does not grow.

```clojure
(declare my-odd?)

(defn my-even? [n] (if (zero? n) true (my-odd? (dec n))))
(defn my-odd? [n] (if (zero? n) false (my-even? (dec n))))

(my-even? 1000000) ; => true
```

A million mutual calls, no stack growth. (The `declare` is there because
this example runs as a script, where each top-level form is compiled in
order and `my-even?` would otherwise refer to a name not yet defined. In a
module, a file beginning with `ns`, functions can refer to names defined
later in the file. See [Chapter 8](08-modules-and-projects.md).)

`recur` is still worth using in a self-recursive loop: it says to the
reader, and to the compiler, that the jump must be a tail call, and the
compiler reports an error if it isn't. A plain tail call carries no such
check. `trampoline` exists for code ported from Clojure; in Skein you don't
need it.

Calls that are *not* in tail position do use stack. Skein's stacks live on
the heap and grow as needed, so ordinary recursion goes deep:

```clojure
(defn depth [n]
  (if (zero? n) 0 (inc (depth (dec n)))))

(depth 100000) ; => 100000
```

That is a hundred thousand nested frames, because `inc` runs after the
recursive call returns. The limit is per fiber and large, a couple of
million frames for a function like this, but it exists. A fiber that
reaches it ends with `Stack overflow`. That is not a condition you can
handle; it ends the fiber outright. For unbounded input, write the
recursion so the call is in tail position, usually by carrying an
accumulator as `total-qty` did, or use a sequence function.

## Pattern matching

`match` takes a value and a series of pattern and result pairs. It tries
each pattern in turn and evaluates the result of the first that matches,
with the pattern's names bound. It shines on tagged data, where the shape
of a value says what it means.

```clojure
(defn evaluate [expr]
  (match expr
    [:num n]   n
    [:add a b] (+ (evaluate a) (evaluate b))
    [:mul a b] (* (evaluate a) (evaluate b))
    [:neg a]   (- (evaluate a))))

(evaluate [:add [:num 2] [:mul [:num 3] [:num 4]]]) ; => 14
```

Each pattern describes a shape:

| Pattern | Matches |
| --- | --- |
| `_` | Anything |
| `name` | Anything, bound to `name`. The same name twice requires equal values. |
| `nil`, `true`, `42`, `"s"`, `\c`, `:k` | That value |
| `'sym`, `'(a b)` | That literal data |
| `[p1 p2 & rest]` | A sequential value with those parts, `rest` bound to what's left |
| `{:k p}` | A map with those keys, whose values match |
| `(:or p1 p2)` | Any of the patterns (each must bind the same names) |
| `(:as p name)` | `p`, with the whole value also bound to `name` |
| `(:? pred p)` | A value `pred` is true of, which also matches `p` |
| `:else` | Anything, as a last clause |

A vector pattern requires exactly the number of items it lists, unless it
ends with `& rest`. A map pattern requires the keys it names, and ignores
other keys.

```clojure
(defn describe [event]
  (match event
    {:type :click :pos [x y]} (str "click at " x "," y)
    {:type :key :key k}       (str "key " k)
    [a a]                     :pair-of-equals
    (:? string? s)            (str "text: " s)
    :else                     :unknown))

(describe {:type :click :pos [3 4] :time 99}) ; => "click at 3,4"
(describe {:type :key :key \q}) ; => "key q"
(describe [7 7]) ; => :pair-of-equals
(describe "hello") ; => "text: hello"
(describe 42) ; => :unknown
```

Put `:when` and a guard after a pattern to add a test that can use the
pattern's names.

```clojure
(defn size-class [n]
  (match n
    (:? integer? k) :when (> k 100) :large
    (:? integer? k)                 :small
    _                               :not-a-number))

(map size-class [500 5 "x"]) ; => [:large :small :not-a-number]
```

When no clause matches, `match` signals a `MatchError` naming the value.
That makes a missing case loud rather than a silent `nil`.

```clojure
(evaluate [:div [:num 1] [:num 2]]) ; !! MatchError
```

A clause's result is in tail position, so `recur` works inside `match`.
One detail matters when you recurse over the rest of a sequence: in
`[x & more]`, `more` is `nil` when nothing is left, and the pattern `[]`
does not match `nil`. Match the end with `(:or nil [])`.

```clojure
(defn sum-items [xs acc]
  (match xs
    (:or nil []) acc
    [x & more]   (recur more (+ acc x))))

(sum-items [1 2 3 4] 0) ; => 10
```

`match` works on data: vectors, lists, maps, and scalars. To choose
behavior by the class of a value, use generic functions, covered in
[Chapter 9](09-classes-and-generic-functions.md).

## Summary

- `let` binds names to values for its body. Names never change; a later
  binding of the same name shadows the earlier one.
- `fn` makes a function and `defn` names one. Functions can have several
  arities and a `&` rest parameter; `apply` spreads a collection into
  arguments, and a map pattern after `&` takes keyword options.
- Destructuring with vector and map patterns works in every binding form.
  `:keys`, `:or`, and `:as` cover most map needs.
- Functions close over the values in scope where they were made.
  `comp`, `partial`, `juxt`, and friends build new functions from old ones.
- `if`, `when`, `cond`, `case`, and `condp` choose; `and` and `or` return
  an operand; `if-let`, `when-let`, `if-some`, and `when-some` test and
  bind at once.
- `->` and `->>` turn nested calls into pipelines; `some->` and `cond->`
  handle missing values and optional steps.
- `loop` and `recur` express iteration, and every tail call is a proper
  tail call, so mutual recursion runs in constant stack. Non-tail
  recursion is deep but bounded.
- `match` picks a clause by the shape of data and signals `MatchError`
  when nothing matches.

---

[Contents](README.md) · Previous: [3. Values and expressions](03-values-and-expressions.md) · Next: [5. Collections](05-collections.md) <!-- nav -->
