# 5. Collections

[Contents](README.md) · Previous: [4. Functions and control flow](04-functions-and-control-flow.md) · Next: [6. Sequences and transducers](06-sequences-and-transducers.md) <!-- nav -->

Most Skein programs are built from a few collection types: lists, vectors,
maps, and sets, plus sorted maps and sets and queues. All of them are
immutable. This chapter shows what each one is for, how to read from and
update them, how to work with nested data, how collections compare and
hash, and how transients let you build a collection quickly in a loop.
It ends with three small libraries for whole-structure work: `skein.set`,
`skein.walk`, and `skein.data`.

The functions that walk a collection item by item, such as `map`,
`filter`, and `reduce`, are the subject of [Chapter
6](06-sequences-and-transducers.md). This chapter is about the
collections themselves.

## Persistent, immutable collections

No operation changes a Skein collection. `conj`, `assoc`, and the other
"update" functions return a *new* collection, and the original stays
exactly as it was.

```clojure
(def a [1 2 3])
(def b (conj a 4))
a ; => [1 2 3]
b ; => [1 2 3 4]
```

This would be expensive if every update copied the whole collection. It
doesn't. Skein's collections are *persistent data structures*: trees in
which the new version shares almost all of its structure with the old one
and only the path to the change is new. Adding to a million-item vector
allocates a few small nodes, not a million slots.

Immutability is what lets you pass a collection anywhere, to another
function or another fiber, without worrying that it will change under
you. It is also why equality can compare collections by their contents:
a value that can't change can't stop being equal.

## Lists

A list is a chain of items, written `(…)`. Because a list in code is a
call, you quote a literal list, or build one with `list`. `conj` and
`cons` add to the front, `first` and `peek` read the front, and `rest`
and `pop` drop it.

```clojure
'(1 2 3) ; => (1 2 3)
(list 1 2 3) ; => (1 2 3)
(conj '(2 3) 1) ; => (1 2 3)
(cons 0 '(1 2)) ; => (0 1 2)
[(peek '(1 2 3)) (pop '(1 2 3))] ; => [1 (2 3)]
```

Lists are good for stacks and for building code (see [Chapter
14](14-macros.md)). Reaching the nth item walks the chain, so for data
that you index into or append to, use a vector.

## Vectors

A vector is an indexed sequence, written `[…]`. It is the collection you
will use most. `conj` adds at the end, `nth` and `get` read by position
in near-constant time, and a vector called as a function looks up an
index.

```clojure
(def colors [:red :green :blue])
(conj colors :violet) ; => [:red :green :blue :violet]
(nth colors 1) ; => :green
(colors 2) ; => :blue
(get colors 10) ; => nil
(get colors 10 :none) ; => :none
```

`get` returns `nil` (or a default) for an index that isn't there. `nth`
and calling the vector signal an `IndexError` instead, unless you give
`nth` a default.

```clojure
(nth colors 10) ; !! IndexError
(nth colors 10 :none) ; => :none
```

`assoc` replaces the item at an index. The index can also be the vector's
length, which appends. `peek` and `pop` work at the end, so a vector is
also a stack. `subvec` takes a slice, with the end exclusive, and `rseq`
walks a vector backwards.

```clojure
(assoc colors 0 :crimson) ; => [:crimson :green :blue]
(assoc colors 3 :violet) ; => [:red :green :blue :violet]
[(peek colors) (pop colors)] ; => [:blue [:red :green]]
(subvec [0 1 2 3 4] 1 3) ; => [1 2]
(rseq colors) ; => (:blue :green :red)
```

## Maps

A map associates keys with values, written `{key value …}`. Any value can
be a key, but keywords are the usual choice. Here is an inventory that
the rest of the chapter builds on.

```clojure
(def inventory
  {:pen      {:price 3/2 :stock 120 :tags #{:office :writing}}
   :notebook {:price 4   :stock 35  :tags #{:office :paper}}
   :ink      {:price 7/4 :stock 0   :tags #{:writing}}})
```

There are three ways to look up a key: `get`, calling the map, and
calling a keyword. All three return `nil` for a missing key, or a default
if you give one. Keyword lookup is the common style when the key is a
known keyword; `get` works for any key and reads more clearly when the key
is computed.

```clojure
(def pen (:pen inventory))
(:price pen) ; => 3/2
(pen :stock) ; => 120
(get pen :color "black") ; => "black"
```

A `nil` result can mean the key is missing or that its value is `nil`.
When the difference matters, ask `contains?`, or use `find`, which returns
the entry as a `[key value]` pair.

```clojure
(contains? {:note nil} :note) ; => true
(get {:note nil} :note :missing) ; => nil
(find pen :price) ; => [:price 3/2]
```

`assoc` adds or replaces entries, `dissoc` removes them, and `merge`
combines maps, with later maps winning. `merge-with` combines the values
of keys that appear in more than one map.

```clojure
(assoc pen :color "black" :stock 100) ; => {:price 3/2 :stock 100 :tags #{:office :writing} :color "black"}
(dissoc pen :tags) ; => {:price 3/2 :stock 120}
(merge {:a 1 :b 2} {:b 20 :c 30}) ; => {:a 1 :b 20 :c 30}
(merge-with + {:pens 10 :ink 2} {:pens 5}) ; => {:pens 15 :ink 2}
```

`update` applies a function to the value at a key. Extra arguments go to
the function after the old value. It is the idiomatic way to change one
field based on its current value.

```clojure
(update pen :stock - 20) ; => {:price 3/2 :stock 100 :tags #{:office :writing}}
(update {} :count (fnil inc 0)) ; => {:count 1}
```

`keys` and `vals` return a map's keys and values, `select-keys` keeps only
some entries, `zipmap` builds a map from keys and values, and
`update-vals` and `update-keys` transform every value or key.

```clojure
(set (keys inventory)) ; => #{:pen :notebook :ink}
(select-keys pen [:price :stock]) ; => {:price 3/2 :stock 120}
(zipmap [:a :b :c] [1 2 3]) ; => {:a 1 :b 2 :c 3}
(update-vals inventory :stock) ; => {:pen 120 :notebook 35 :ink 0}
```

A hash map has no defined order. `keys`, `vals`, and printing may list
entries in any order, and that order can differ between two equal maps.
The example above wraps `keys` in `set` for that reason. When you need
order, use a sorted map, or sort the keys.

When you treat a map as a sequence, each item is an *entry*, a two-item
vector of key and value. That is why `(first {:a 1})` is `[:a 1]` and why
`into` can build a map from pairs. `reduce-kv` reduces over a map with
the key and value as separate arguments, and over a vector with each
index and item.

```clojure
(first {:a 1}) ; => [:a 1]
(into {} [[:a 1] [:b 2]]) ; => {:a 1 :b 2}
(reduce-kv (fn [total _ {:keys [price stock]}]
             (+ total (* price stock)))
           0
           inventory) ; => 320
(reduce-kv (fn [acc i x] (+ acc (* i x))) 0 [5 6 7]) ; => 20
```

## Sets

A set holds distinct values, written `#{…}`. `conj` adds a member (adding
one that is already there changes nothing), `disj` removes one, and
`contains?` tests membership. A set called as a function returns the
member, or `nil`.

```clojure
(def tags (:tags pen))
(conj tags :office) ; => #{:office :writing}
(disj tags :office) ; => #{:writing}
(contains? tags :paper) ; => false
(tags :writing) ; => :writing
(set [3 1 3 2 1]) ; => #{1 2 3}
```

Calling a set returns the member itself, so a set containing `nil` or
`false` would give a false answer for a member it holds. Use `contains?`
when that can happen.

The `skein.set` library has the set algebra: `union`, `intersection`,
`difference`, `subset?`, `superset?`, `select`, and a few functions on
*relations* (sets of maps) such as `index`, `project`, and `join`.

```clojure
(require '[skein.set :as set])

(apply set/union (map :tags (vals inventory))) ; => #{:office :writing :paper}
(set/intersection (:tags pen) (get-in inventory [:ink :tags])) ; => #{:writing}
(set/difference #{1 2 3} #{2}) ; => #{1 3}
(set/subset? #{:office} (:tags pen)) ; => true
```

`set/index` groups a set of maps by the values of some keys, which is a
quick way to look records up by a field.

```clojure
(def staff #{{:name "Ada" :dept :eng}
             {:name "Grace" :dept :eng}
             {:name "Edsger" :dept :math}})

(get (set/index staff [:dept]) {:dept :math}) ; => #{{:name "Edsger" :dept :math}}
```

Run `skein doc skein.set` for the rest.

## Sorted maps and sets

`sorted-map` and `sorted-set` keep their keys in order. They support
everything a hash map or set does, and `first`, `last`, and traversal
follow the order. They are equal to a hash map or set with the same
contents.

```clojure
(def scores (sorted-map "carol" 72 "alice" 91 "bob" 85))
scores ; => {"alice" 91 "bob" 85 "carol" 72}
(first scores) ; => ["alice" 91]
(keys scores) ; => ("alice" "bob" "carol")
(= scores {"alice" 91 "bob" 85 "carol" 72}) ; => true
(into (sorted-set) [5 3 9 1]) ; => #{1 3 5 9}
```

`subseq` and `rsubseq` return the entries within a range, ascending or
descending, and `rseq` walks the whole collection backwards.

```clojure
(subseq scores >= "b") ; => [["bob" 85] ["carol" 72]]
(rsubseq scores < "c") ; => [["bob" 85] ["alice" 91]]
(subseq (sorted-set 1 3 5 7 9) > 2 <= 7) ; => [3 5 7]
```

The order is always the natural one, given by `compare`: numbers by
value, strings and keywords alphabetically, vectors item by item. There
is no `sorted-map-by` for a custom order yet. Keys must be mutually
comparable, so a sorted map cannot mix strings and keywords as keys.

```clojure
(sorted-map [2026 3 1] :launch [2025 12 24] :party) ; => {[2025 12 24] :party [2026 3 1] :launch}
(sorted-map :b 1 "a" 2) ; !! RuntimeError
```

Because `compare` orders numbers across categories, a sorted collection
treats `1` and `1.0` as the same key, where a hash map keeps them apart.
The first one in stays: `(sorted-set 1 1.0)` is `#{1}`. Avoid mixing
integers and floats as keys.

## Queues

A queue is first-in, first-out: `conj` adds at the back, `peek` reads the
front, and `pop` removes it. Make one with `queue`, or write a literal
with `#queue`.

```clojure
(def line (queue :ada :grace))
(def line (conj line :edsger))
line ; => #queue [:ada :grace :edsger]
(peek line) ; => :ada
(pop line) ; => #queue [:grace :edsger]
```

A queue is a persistent value like the others. Its class is `Queue`, and
it's sequential, so it equals a vector or list with the same items:
`(= (queue 1 2) [1 2])` is true. For a queue that fibers share, with blocking when it is empty, use a channel ([Chapter
16](16-channels.md)).

## Nested data

Real data nests: maps of maps, maps of vectors of maps. Skein has a
function for each kind of nested access, taking a *path*: a vector of the
keys (or indexes) to follow.

`get-in` reads along a path, with an optional default.

```clojure
(get-in inventory [:pen :stock]) ; => 120
(get-in inventory [:stapler :stock] 0) ; => 0
(get-in {:orders [{:id 1} {:id 2}]} [:orders 1 :id]) ; => 2
```

`assoc-in` sets the value at a path, creating maps along the way when keys
are missing. `update-in` applies a function at a path.

```clojure
(assoc-in {} [:pen :stock] 5) ; => {:pen {:stock 5}}
(get-in (update-in inventory [:pen :stock] - 20) [:pen :stock]) ; => 100
```

These compose well with `->`, which threads the inventory through each
step. Here is a day of business: twenty pens sold, ink restocked and
tagged, notebooks discontinued.

```clojure
(def after
  (-> inventory
      (update-in [:pen :stock] - 20)
      (assoc-in [:ink :stock] 50)
      (update-in [:ink :tags] conj :refill)
      (dissoc :notebook)))

after ; => {:pen {:price 3/2 :stock 100 :tags #{:office :writing}}
      ;     :ink {:price 7/4 :stock 50 :tags #{:writing :refill}}}
```

`inventory` itself is unchanged. You now have both versions, and they
share every part that didn't change. Keeping the "before" around costs
almost nothing, which makes undo, auditing, and comparing states cheap.

## Equality and hashing

`=` compares collections by their contents, all the way down.

- Sequential collections (vectors, lists, queues, and sequences) are equal when
  they have equal items in the same order, whatever their type. A vector
  equals a list with the same items.
- Maps are equal when they have the same entries, and sets when they have
  the same members, regardless of order or of hash versus sorted.
- Different kinds never compare equal: a map never equals a set, and a
  string never equals a vector of characters.

```clojure
(= [1 2 3] '(1 2 3)) ; => true
(= {:a 1 :b 2} {:b 2 :a 1}) ; => true
(= #{1 2} (sorted-set 2 1)) ; => true
(= [] {}) ; => false
(= [1 2] [1 2.0]) ; => false
```

The last line follows from the number rules in [Chapter
3](03-values-and-expressions.md): `2` and `2.0` are not `=`, so neither
are collections containing them.

`hash` agrees with `=`: equal values have equal hashes. So any value can
be a map key or set member, including a collection, and lookup works by
contents.

```clojure
(= (hash [1 2]) (hash '(1 2))) ; => true
(get {[0 0] :origin} [0 0]) ; => :origin
(get {{:x 1} :found} {:x 1}) ; => :found
(contains? #{#{:a}} #{:a}) ; => true
```

Equality and hashing are defined by the runtime and never run your code,
so a map lookup can't block or have side effects, even inside a parallel
computation.

Not everything compares by contents. *Identities*, things that have
state or behavior rather than being plain values, compare by identity:
two atoms are equal only if they are the same atom, and the same goes for
functions, vars, channels, and fibers.

```clojure
(= (atom 1) (atom 1)) ; => false
(let [x (atom 1)] (= x x)) ; => true
```

`identical?` asks whether two references are the same object in memory.
You rarely need it with values; equal values are interchangeable whether
or not they are identical.

Instances of your own classes compare by their slots, like maps, unless
the class says otherwise. [Chapter
10](10-protocols-and-extension.md) covers the options.

## Transients

Sometimes you build a large collection one item at a time, in a loop, and
nobody else will see it until it's done. Each persistent `conj` allocates
a new version that is thrown away on the next step. A *transient* is a
mutable builder for that situation: you convert a collection with
`transient`, update it with the `!` versions of the functions, and turn
it back into a persistent value with `persistent!`.

```clojure
(defn squares [n]
  (loop [i 0
         acc (transient [])]
    (if (< i n)
      (recur (inc i) (conj! acc (* i i)))
      (persistent! acc))))

(squares 6) ; => [0 1 4 9 16 25]
```

The updating functions are `conj!`, `assoc!`, `dissoc!`, `disj!`, and
`pop!`. Three rules apply:

1. Always use the value that an update returns, as the loop above does
   with `acc`. Don't call `conj!` and ignore the result.
2. After `persistent!`, the transient is finished. Using it again signals
   an error.
3. A transient belongs to the fiber that created it. Using it from any
   other fiber signals an `OwnershipError`.

```clojure
(def builder (transient []))
(def done (persistent! (conj! builder 1)))
done ; => [1]
(conj! builder 2) ; !! RuntimeError
(def builder (transient []))
@(future (conj! builder 1)) ; !! OwnershipError
```

The third rule is what makes transients safe in a language built around
many fibers: mutation is allowed exactly where only one fiber can see it.
([Chapter 15](15-fibers-and-scopes.md) introduces fibers, and `future`
runs its body in a new one.)

Use transients as a local optimization, inside one function, after
measuring. In this build only transient vectors are faster than ordinary
updates. Transient maps and sets have the full API, but they still copy
on each change, so building a map with `assoc!` is no faster than with
`assoc`. While you build, `get`, `nth`, `contains?`, and `count` read the
items so far:

```clojure
(let [t (assoc! (transient {:a 1}) :b 2)]
  [(get t :a) (count t) (contains? t :b)]) ; => [1 2 true]
```

## Working on whole structures with skein.walk

Sometimes you want to change something everywhere in a nested structure:
every string key, every ratio, every map with a certain key. `skein.walk`
does this without you writing the recursion.

`postwalk` calls a function on every node of a structure, children before
parents, and rebuilds the structure from the results. Here it turns every
ratio in the inventory into a float, for a report:

```clojure
(require '[skein.walk :as walk])

(walk/postwalk #(if (ratio? %) (double %) %) inventory) ; => {:pen {:price 1.5 :stock 120 :tags #{:office :writing}}
                                                        ;     :notebook {:price 4 :stock 35 :tags #{:office :paper}}
                                                        ;     :ink {:price 1.75 :stock 0 :tags #{:writing}}}
```

`prewalk` visits parents first, which suits changes that should happen
before the children are visited, such as pruning a subtree. Both functions
visit map entries as `[key value]` vectors, so a function that tests
`map?` sees whole maps.

```clojure
(walk/prewalk (fn [x] (if (and (map? x) (contains? x :tags)) (dissoc x :tags) x))
              inventory) ; => {:pen {:price 3/2 :stock 120}
                         ;     :notebook {:price 4 :stock 35}
                         ;     :ink {:price 7/4 :stock 0}}
```

A few common walks come ready-made. `keywordize-keys` and `stringify-keys`
convert map keys between strings and keywords, which is handy at the
border with JSON ([Chapter 20](20-data-formats.md)). `postwalk-replace`
replaces values by a lookup map.

```clojure
(walk/keywordize-keys {"pen" {"price" 1.5 "stock" 120}}) ; => {:pen {:price 1.5 :stock 120}}
(walk/postwalk-replace {:office :work} [:office #{:office :home}]) ; => [:work #{:work :home}]
```

## Comparing structures with skein.data

`=` tells you whether two values are equal. When they aren't, `diff`
from `skein.data` tells you how. It returns three things: what is only in
the first value, what is only in the second, and what they share.

```clojure
(require '[skein.data :as data])

(data/diff {:a 1 :b 2} {:a 1 :b 3 :c 4}) ; => [{:b 2} {:b 3 :c 4} {:a 1}]
```

Maps compare by key and recurse into values, sets by member, and
sequential collections by index (with `nil` in the positions that agree).
A part with nothing in it is `nil`. Applied to the inventory before and
after the day's business, `diff` reports exactly what changed:

```clojure
(let [[before-only after-only _] (data/diff inventory after)]
  [before-only after-only]) ; => [{:pen {:stock 120}
                            ;      :ink {:stock 0}
                            ;      :notebook {:price 4 :stock 35 :tags #{:office :paper}}}
                            ;     {:pen {:stock 100}
                            ;      :ink {:stock 50 :tags #{:refill}}}]
(data/diff [1 2 3] [1 2 4]) ; => [[nil nil 3] [nil nil 4] [1 2]]
```

`diff` is especially useful in tests, where "expected this, got that"
is much easier to read as the three-way split.

## Summary

- Skein's collections are immutable and persistent: updates return new
  versions that share structure with the old, so keeping old versions is
  cheap and sharing a collection is always safe.
- Lists add at the front; vectors index and add at the end; maps look up
  by key; sets hold distinct members; sorted maps and sets keep natural
  order; queues are first-in, first-out.
- Maps, vectors, sets, and keywords can be called as lookup functions.
  `get` returns `nil` or a default for missing entries; `contains?` and
  `find` tell a missing key from a `nil` value.
- `get-in`, `assoc-in`, and `update-in` work along a path through nested
  data, and chain well with `->`.
- `=` compares by contents: sequential collections by items in order,
  maps by entries, sets by members. `hash` agrees with `=`, so any value
  can be a key. Atoms, functions, and other identities compare by identity.
- Transients are fiber-owned builders for loops that construct a
  collection. Use the returned value, finish with `persistent!`, and
  prefer them for vectors in this build.
- `skein.set` has set algebra, `skein.walk` transforms nested structures,
  and `skein.data/diff` explains how two values differ.

---

[Contents](README.md) · Previous: [4. Functions and control flow](04-functions-and-control-flow.md) · Next: [6. Sequences and transducers](06-sequences-and-transducers.md) <!-- nav -->
