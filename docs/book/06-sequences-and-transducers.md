# 6. Sequences and transducers

[Contents](README.md) · Previous: [5. Collections](05-collections.md) · Next: [7. Text](07-text.md) <!-- nav -->

Most work on collections is a pipeline: select some items, transform
them, group or sort them, and summarize the result. This chapter covers
the functions that do that. It starts with the sequence abstraction that
lets one set of functions work on every collection, then explains Skein's
rule that these functions are eager and return the kind of collection
they were given. After a tour of the common toolkit and the `for` and
`doseq` comprehensions, it turns to laziness, which in Skein you ask for
explicitly, and to transducers, which build pipelines that make no
intermediate collections.

The examples use a small sales log. Each sale is a map.

```clojure
(def sales
  [{:day :mon :item "pen"      :qty 3 :price 3/2}
   {:day :mon :item "ink"      :qty 1 :price 7/4}
   {:day :tue :item "pen"      :qty 5 :price 3/2}
   {:day :tue :item "notebook" :qty 2 :price 4}
   {:day :wed :item "pen"      :qty 1 :price 3/2}
   {:day :wed :item "ink"      :qty 4 :price 7/4}])

(defn revenue [{:keys [qty price]}] (* qty price))
```

## One interface for every collection

Vectors, lists, maps, sets, strings, and ranges are stored differently,
but they can all be read the same way: one item after another. `seq`
gives you that view. It returns `nil` for an empty collection, and
`first`, `rest`, and `next` walk the items.

```clojure
(seq [1 2 3]) ; => (1 2 3)
(seq "hey") ; => (\h \e \y)
(seq {:a 1}) ; => ([:a 1])
(seq []) ; => nil
[(first sales) (count (rest sales))] ; => [{:day :mon :item "pen" :qty 3 :price 3/2} 5]
```

A map's items are its entries, `[key value]` vectors, and a string's items
are its characters. Every sequence function accepts any collection, and
`nil` as an empty one, because each of them starts from this view.

The idiom for "does this have any items?" is `(seq coll)`, which is
truthy exactly when there is a first item. `empty?` asks the opposite
question.

```clojure
(if (seq sales) :has-sales :no-sales) ; => :has-sales
(empty? (filter #(> (:qty %) 10) sales)) ; => true
```

## Eager and type-preserving

The core sequence functions, `map`, `filter`, `remove`, `take`, `drop`,
and the rest, run **when you call them**, and they return **the same
kind of collection they were given**.

```clojure
(map inc [1 2 3]) ; => [2 3 4]
(map inc '(1 2 3)) ; => (2 3 4)
(map inc #{1 2 3}) ; => #{2 3 4}
(filter odd? [1 2 3 4 5]) ; => [1 3 5]
(filter odd? '(1 2 3 4 5)) ; => (1 3 5)
```

A vector in, a vector out. A list in, a list in the same order out. A set
in, a set out. For maps there is a split. Functions that *select* items,
such as `filter`, `remove`, `take`, and `drop`, return a map, since the
items they keep are still entries. Functions that *compute* new items,
such as `map`, `keep`, and `mapcat`, return a vector, since the results
need not be entries. Use `into {}` when you want a map back from those.

```clojure
(filter (fn [[_ v]] (pos? v)) {:a 1 :b -2 :c 3}) ; => {:a 1 :c 3}
(map (fn [[k v]] [k (* 10 v)]) {:a 1}) ; => [[:a 10]]
(into {} (map (fn [[k v]] [k (* 10 v)])) {:a 1 :b 2}) ; => {:a 10 :b 20}
```

Strings, finite ranges, and other collections give vectors.

```clojure
(map int "abc") ; => [97 98 99]
(map #(* % %) (range 5)) ; => [0 1 4 9 16]
```

With several collections, `map` walks them side by side and stops at the
shortest. When you need a vector whatever the input, `mapv` and `filterv`
always return one.

```clojure
(map + [1 2 3] [10 20 30 40]) ; => [11 22 33]
(map vector [:a :b] [1 2]) ; => [[:a 1] [:b 2]]
(mapv inc '(1 2)) ; => [2 3]
```

It takes any number of collections, so `(apply map vector rows)` turns
rows into columns:

```clojure
(apply map vector [[1 2] [3 4] [5 6] [7 8]]) ; => [[1 3 5 7] [2 4 6 8]]
```

This is a deliberate difference from Clojure, where these functions return
lazy sequences. Lazy results are convenient, but the work they defer runs
whenever someone eventually reads the result, which may be outside the
dynamic bindings, the resource scope, or the fiber the code was written
for. An eager result is computed right there, in the caller's context.
Here is the difference, using a dynamic variable (covered in [Chapter
17](17-shared-state.md)):

```clojure
(def ^:dynamic *unit* "kg")
(defn label-all [xs] (map #(str % *unit*) xs))

(binding [*unit* "lb"] (label-all [1 2])) ; => ["1lb" "2lb"]
```

The result is ready before `binding` ends, so it sees `"lb"`. The lazy
version of the same call, which you'll meet later in this chapter, does
its work only when read, after `binding` has ended:

```clojure
(def later (binding [*unit* "lb"] (label-all (lazy [1 2]))))
(vec later) ; => ["1kg" "2kg"]
```

In Skein you get the second behavior only when you ask for it.

## The toolkit

There are many sequence functions, and you'll pick them up by using them.
This section groups the common ones by what they do, using the sales log.
`skein doc skein.core` lists them all, and the [collections
reference](../reference/collections.md#sequence-traversal) describes the
traversal rules.

### Selecting and transforming

`filter` keeps items a predicate is true of, and `remove` keeps the rest.
`map` transforms each item. `keep` transforms and drops `nil` results in
one step, and `mapcat` transforms each item into a collection and joins
the results.

```clojure
(map revenue sales) ; => [9/2 7/4 15/2 8 3/2 7]
(map :item (filter #(= :tue (:day %)) sales)) ; => ["pen" "notebook"]
(map :item (remove #(= "pen" (:item %)) sales)) ; => ["ink" "notebook" "ink"]
(keep :discount [{:discount 5} {} {:discount 2}]) ; => [5 2]
(mapcat (fn [{:keys [item qty]}] (repeat qty item)) (take 2 sales)) ; => ["pen" "pen" "pen" "ink"]
(map-indexed (fn [i s] [i (:item s)]) (take 2 sales)) ; => [[0 "pen"] [1 "ink"]]
```

`take` and `drop` keep or skip a number of items from the front.
`take-while` and `drop-while` do the same up to the first item that fails
a test, and `split-at` and `split-with` give you both halves at once.

```clojure
(map :day (take-while #(= :mon (:day %)) sales)) ; => [:mon :mon]
(count (drop-while #(= :mon (:day %)) sales)) ; => 4
(split-at 2 [1 2 3 4 5]) ; => [[1 2] [3 4 5]]
```

`distinct` drops repeats anywhere, keeping first occurrences, and
`dedupe` drops only consecutive repeats.

```clojure
(distinct (map :item sales)) ; => ["pen" "ink" "notebook"]
(dedupe (map :day sales)) ; => [:mon :tue :wed]
```

### Grouping and counting

`frequencies` counts how often each value appears. `group-by` collects
items into a map keyed by the result of a function, with a vector of
items under each key.

```clojure
(frequencies (map :item sales)) ; => {"pen" 3 "ink" 2 "notebook" 1}
(update-vals (group-by :item sales) #(reduce + (map revenue %))) ; => {"pen" 27/2 "ink" 35/4 "notebook" 8}
```

That second line is a small report: revenue per item. `group-by` made a
map from item to its sales, and `update-vals` replaced each list of sales
by its total.

`partition` splits a collection into groups of a fixed size, dropping a
short last group. `partition-all` keeps it. `partition-by` starts a new
group each time a function's result changes, which suits data that is
already ordered, like the days in the log.

```clojure
(partition 2 (range 7)) ; => [[0 1] [2 3] [4 5]]
(partition-all 3 (range 7)) ; => [[0 1 2] [3 4 5] [6]]
(map (comp :day first) (partition-by :day sales)) ; => [:mon :tue :wed]
(map count (partition-by :day sales)) ; => [2 2 2]
```

### Ordering

`sort` sorts by natural order (`compare`), and `sort-by` sorts by a key
function. Both take an optional comparator, such as `>` for descending,
and both always return a vector. They are stable: items with equal keys
keep their original order.

```clojure
(sort [3 1 2]) ; => [1 2 3]
(sort > [3 1 2]) ; => [3 2 1]
(map :item (sort-by revenue > sales)) ; => ["notebook" "pen" "ink" "pen" "ink" "pen"]
```

To sort by several keys, return a vector from the key function. Vectors
compare item by item, so `(juxt :item :qty)` sorts by item, then by
quantity.

```clojure
(map (juxt :item :qty) (sort-by (juxt :item :qty) sales)) ; => [["ink" 1] ["ink" 4] ["notebook" 2] ["pen" 1] ["pen" 3] ["pen" 5]]
```

`reverse` reverses a collection, and `max-key` and `min-key` find the item
with the largest or smallest key.

```clojure
(:item (apply max-key revenue sales)) ; => "notebook"
```

### Searching and testing

`some` returns the first truthy result of a function, or `nil`. With a
set as the function it finds the first member present. `every?`,
`not-any?`, and `not-every?` test all items.

```clojure
(some #(when (= "notebook" (:item %)) (:day %)) sales) ; => :tue
(some #{"ink" "stapler"} (map :item sales)) ; => "ink"
(every? #(pos? (:qty %)) sales) ; => true
(not-any? #(> (:qty %) 10) sales) ; => true
```

To find the first item that matches, combine `first` and `filter`. The
filter runs over the whole collection, which is fine for small data;
for a large or unbounded input, see laziness and transducers below.

```clojure
(:qty (first (filter #(= "ink" (:item %)) sales))) ; => 1
```

### Combining

`concat` joins collections end to end, `interleave` alternates their
items, and `interpose` puts a separator between items.

```clojure
(concat [1 2] [3] '(4 5)) ; => (1 2 3 4 5)
(interleave [:a :b :c] [1 2 3]) ; => [:a 1 :b 2 :c 3]
(apply str (interpose ", " (distinct (map :item sales)))) ; => "pen, ink, notebook"
```

## Reducing

`reduce` is the general tool for turning a collection into a single
result. It calls a function with an accumulator and each item in turn,
and the function's result becomes the next accumulator.

```clojure
(reduce + (map :qty sales)) ; => 16
(reduce (fn [totals {:keys [item] :as sale}]
          (update totals item (fnil + 0) (revenue sale)))
        {}
        sales) ; => {"pen" 27/2 "ink" 35/4 "notebook" 8}
```

The second form computes the same per-item report as before, in one pass.
With an initial value, `reduce` starts from it. Without one, it starts
from the first item, and on an empty collection it calls the function
with no arguments, which is why `(reduce + [])` is `0`.

```clojure
(reduce + []) ; => 0
(reduce max [3 9 2]) ; => 9
```

To stop early, return `(reduced x)` from the function. The reduction ends
at once with `x`. Here it finds the day on which cumulative revenue
first passes 15:

```clojure
(reduce (fn [total sale]
          (let [t (+ total (revenue sale))]
            (if (> t 15) (reduced (:day sale)) t)))
        0
        sales) ; => :tue
```

`reductions` returns every intermediate accumulator, which is handy for
running totals.

```clojure
(reductions + (map :qty sales)) ; => (3 4 9 11 12 16)
```

`into` is a reduction too: it adds every item of one collection to
another with `conj`. It is the usual way to change a collection's type.

```clojure
(into #{} (map :item sales)) ; => #{"pen" "ink" "notebook"}
(into (sorted-map) (frequencies (map :day sales))) ; => {:mon 2 :tue 2 :wed 2}
```

## Comprehensions: for and doseq

`for` builds a collection from one or more bindings. With several
bindings it nests them, the rightmost varying fastest, like nested loops.
`:let` binds more names and `:when` skips combinations. Bindings
destructure. `for` is eager and returns a vector.

```clojure
(for [x [1 2 3] y [:a :b]] [x y]) ; => [[1 :a] [1 :b] [2 :a] [2 :b] [3 :a] [3 :b]]
(for [{:keys [day item]} sales :when (= item "ink")] day) ; => [:mon :wed]
(for [x (range 6) :let [sq (* x x)] :when (even? x)] sq) ; => [0 4 16]
```

`:while` stops the binding before it at the first item that fails its
test, as in Clojure, so it can cut an endless sequence short:

```clojure
(for [x (range) :while (< x 3)] x) ; => [0 1 2]
```

`doseq` has the same binding syntax, but runs its body for its effects and
returns `nil`. Use it for printing, writing, and sending: anything where
you want the action, not a collection.

```clojure
(doseq [[day group] (sort (group-by :day sales))]
  (println day (reduce + (map revenue group))))
; prints: :mon 25/4
; prints: :tue 31/2
; prints: :wed 17/2
```

`run!` is the function form of a one-binding `doseq`: `(run! println xs)`.

## Explicit laziness

Eager functions need a finite input. Sometimes you want to describe a
sequence that is unbounded, or expensive to produce in full, and consume
only as much as you need. For that, Skein has *streams*: lazy sequences
whose items are computed when they are first read.

### Making streams

You create a stream on purpose. The generators are:

- `(range)` with no arguments: `0`, `1`, `2`, … forever.
- `(iterate f x)`: `x`, `(f x)`, `(f (f x))`, …
- `(repeat x)` and `(cycle coll)`: one value, or a collection's items,
  forever.
- `(repeatedly f)`: the results of calling `f` again and again.
- `(lazy coll)`: an existing collection, read as needed.
- `lazy-seq`, for writing your own generator (below).

Note the difference between `(range)` and `(range 10)`. A finite range is
an ordinary collection, and eager functions on it return vectors. Likewise
`(repeat 3 x)` returns a vector of three items, not a stream.

### Reading streams

`first`, `rest`, `next`, `nth`, and `seq` realize a stream one item at a
time, so they work on infinite streams. The sequence functions, `map`,
`filter`, `remove`, `keep`, `take`, `drop`, `take-while`, `drop-while`,
`mapcat`, `map-indexed`, `distinct`, `dedupe`, `interpose`, `partition`,
`partition-all`, `partition-by`, `reductions`, and `flatten`, return a
stream when given one, and `interleave` and `concat` do when any of their
inputs is one: laziness spreads, but only from a stream you made.

```clojure
(def naturals (iterate inc 0))
(first naturals) ; => 0
(nth naturals 100) ; => 100
(nth (iterate #(* 2 %) 1) 64) ; => 18446744073709551616
(stream? (map inc naturals)) ; => true
(vec (take 5 (filter odd? naturals))) ; => [1 3 5 7 9]
```

Anything else that needs all the items realizes the whole stream: `vec`,
`into`, `reduce`, `count`, `=`, and printing. On an infinite stream these
never finish, so bound it first with `take` or `take-while`:

```clojure
(vec (take 3 (partition 2 (range)))) ; => [[0 1] [2 3] [4 5]]
(vec (take 4 (reductions + (range)))) ; => [0 1 3 6]
```

`realize` turns a stream into a vector, and `doall` and `dorun` realize a
stream in place, keeping or discarding it.

```clojure
(realize (take 4 (cycle [:a :b]))) ; => [:a :b :a :b]
(vec (take-while #(< % 30) (map #(* % %) naturals))) ; => [0 1 4 9 16 25]
```

A stream realizes an item only when something reads it, and remembers it
after that. You can watch this happen with a function that logs what it
computes:

```clojure
(def seen (atom []))
(def tens (map (fn [x] (swap! seen conj x) (* 10 x)) (lazy [1 2 3 4])))
@seen ; => []
(first tens) ; => 10
@seen ; => [1]
(vec tens) ; => [10 20 30 40]
@seen ; => [1 2 3 4]
```

Defining `tens` computed nothing. Reading the first item computed one.
`vec` computed the rest, and a second `vec` would compute nothing new.

That deferred work is the reason to keep the functions you pass to a
stream free of side effects. The example above logs only to show what
happens. In real code, the work runs whenever the stream is read, which
may be in another binding context or another fiber, and if two fibers
read the same unrealized item at the same moment, both may run its
computation (the first result is kept).

### Writing a generator with lazy-seq

`lazy-seq` wraps a body that produces a sequence, and runs the body only
when the stream is first read. Combined with `cons`, which keeps a lazy
tail unrealized, it lets a function describe an infinite sequence in terms
of itself.

```clojure
(defn fibs
  ([] (fibs 0 1))
  ([a b] (lazy-seq (cons a (fibs b (+ a b))))))

(vec (take 10 (fibs))) ; => [0 1 1 2 3 5 8 13 21 34]
(nth (fibs) 90) ; => 2880067194370816120
(take-while #(< % 50) (fibs)) ; => (0 1 1 2 3 5 8 13 21 34)
```

Each call to `fibs` returns immediately with an unrealized stream. The
recursive call inside happens only when someone reads past `a`.

`tree-seq` is a ready-made lazy walk of a tree, depth first. You tell it
which nodes have children and how to get them.

```clojure
(tree-seq vector? seq [1 [2 [3]] 4]) ; => ([1 [2 [3]] 4] 1 [2 [3]] 2 [3] 3 4)
```

## Transducers

A pipeline like this is clear, but each step builds a whole collection
for the next one to consume:

```clojure
(->> sales
     (filter #(= "pen" (:item %)))
     (map revenue)
     (reduce +)) ; => 27/2
```

`filter` builds a vector of pen sales, `map` builds a vector of revenues,
and `reduce` adds them. For three steps over six items that doesn't
matter. For long pipelines over large data, the intermediate collections
cost time and memory. *Transducers* describe the steps without the
collections, so the same work runs in a single pass.

### Transducers from sequence functions

Call a sequence function without its collection and you get a transducer:
a description of one step. `comp` chains transducers, and the order in
`comp` is the order items flow through the steps, the same order as in
the `->>` pipeline.

```clojure
(def pen-revenue
  (comp (filter #(= "pen" (:item %)))
        (map revenue)))
```

Nothing has run yet: `pen-revenue` is a recipe. You apply it to data with
one of these:

| Form | Result |
| --- | --- |
| `(into to xf coll)` | Items from `coll`, through `xf`, added to `to` |
| `(transduce xf f init coll)` | Items through `xf`, reduced with `f` |
| `(sequence xf coll)` | Items through `xf`, as a vector |
| `(eduction xf coll)` | Items through `xf` (a vector in this build) |

```clojure
(transduce pen-revenue + 0 sales) ; => 27/2
(into [] pen-revenue sales) ; => [9/2 15/2 3/2]
(into #{} (map :day) sales) ; => #{:mon :tue :wed}
(sequence (comp (map :qty) (filter odd?)) sales) ; => [3 1 5 1]
```

One transducer works with any of them, and with any kind of destination.
`(into {} xf coll)` builds a map, `(into #{} xf coll)` a set, and so on.
The same recipe can also run over a channel: `skein.async/transduce-chan`
applies a transducer to the values flowing through one ([Chapter
16](16-channels.md)).

These sequence functions have transducer forms when called without a
collection: `map`, `filter`, `remove`, `keep`, `mapcat`, `map-indexed`,
`take`, `drop`, `take-while`, `drop-while`, `distinct`, `dedupe`,
`interpose`, and `partition-all`. `cat` is a transducer that joins
nested collections. `partition`, `partition-by`, and `keep-indexed` have
no transducer form.

```clojure
(into [] cat [[1 2] [3] [4 5]]) ; => [1 2 3 4 5]
(into [] (comp (map :day) (dedupe)) sales) ; => [:mon :tue :wed]
(into [] (partition-all 4) (range 10)) ; => [[0 1 2 3] [4 5 6 7] [8 9]]
```

### Stopping early

A transducer that needs only part of its input, such as `take` or
`take-while`, stops the whole process when it has enough. Items after that
point are never read, so transducers work on unbounded streams too.

```clojure
(into [] (comp (map (fn [x] (println "saw" x) x)) (take 2)) [1 2 3 4])
; prints: saw 1
; prints: saw 2
(into [] (comp (partition-all 2) (take 3)) (range)) ; => [[0 1] [2 3] [4 5]]
(transduce (comp (filter odd?) (map #(* % %)) (take 4)) + (iterate inc 0)) ; => 84
```

The last line sums the squares of the first four odd numbers, drawn from
an infinite stream, without building any intermediate collection.

### Reducing functions

The `f` in `transduce` is a *reducing function*, the same kind of
function you pass to `reduce`, with two optional extra arities:

- `(f)` returns the initial value, used when you don't pass `init`.
- `(f result)` is the *completion* step, called once at the end to
  finish the result.

`+`, `conj`, and `max` already have the arities they need. For a function
of your own, write the arities, or use `completing` to add a completion
step to a two-argument function.

```clojure
(defn count-rf
  ([] 0)
  ([n] n)
  ([n _] (inc n)))

(transduce (filter #(= "ink" (:item %))) count-rf sales) ; => 2
(transduce (map :item) (completing conj #(apply str (interpose "/" %))) [] (take 3 sales)) ; => "pen/ink/pen"
```

### Writing a transducer

A transducer is a function that takes a reducing function and returns a
new one, which does its own work and then calls the one it wraps. Here is
a transducer that drops every item whose quantity is below a minimum:

```clojure
(defn min-qty [n]
  (fn [rf]
    (fn
      ([] (rf))
      ([result] (rf result))
      ([result sale]
       (if (>= (:qty sale) n)
         (rf result sale)
         result)))))

(into [] (comp (min-qty 3) (map :item)) sales) ; => ["pen" "pen" "ink"]
```

The three arities pass initialization and completion through unchanged
and filter in the step. In practice you rarely need to write one; the
built-in transducers combined with `comp` cover most pipelines. It is
useful to know the shape, because it explains why `comp` reads left to
right: each transducer wraps the one after it, so the leftmost step sees
each item first.

### When to use which

Use the eager functions and `->>` by default. They are clear, their
results print, and on small and medium data they are fast. Reach for a
transducer when a pipeline over large data spends its time building
intermediate collections, when the same recipe should run over different
sources, or when you want early termination over an unbounded stream.
Reach for a stream when the input is unbounded or expensive and you'll
consume only part of it, and keep the work in it free of side effects.
[Chapter 26](26-tools-and-performance.md) has more on performance.

## Summary

- `seq` gives every collection, and `nil`, the same item-by-item view, so
  one set of sequence functions works on all of them. `(seq coll)` is the
  test for "has items".
- Sequence functions are eager and type-preserving: vectors give vectors,
  lists give lists, sets give sets. On maps, selecting functions give a
  map and computing functions give a vector.
- `filter`, `map`, `keep`, `group-by`, `frequencies`, `partition-by`,
  `sort-by`, and `some` cover most data shaping. `reduce`, with `reduced`
  for early exit, covers summarizing.
- `for` builds a vector from nested bindings with `:let` and `:when`;
  `doseq` does the same for effects.
- Laziness is explicit. `(range)`, `iterate`, `repeat`, `cycle`, `lazy`,
  and `lazy-seq` make streams; sequence functions keep them lazy; `vec`,
  `into`, `reduce`, and `count` realize them.
- A sequence function called without its collection is a transducer.
  `comp` chains them in pipeline order, and `into`, `transduce`, and
  `sequence` run them in one pass, with early termination.

---

[Contents](README.md) · Previous: [5. Collections](05-collections.md) · Next: [7. Text](07-text.md) <!-- nav -->
