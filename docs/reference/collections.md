# Values, collections, and sequences

[Reference contents](README.md) · [Symbol index](symbols.md)

## Numbers and equality

| Function | Behavior |
| --- | --- |
| `(+ x…)`, `(- x y…)`, `(* x…)`, `(/ x y…)` | Arithmetic. Integers grow to arbitrary precision. Exact division can produce a normalized ratio. A floating-point operand produces floating-point arithmetic. |
| `(= x more…)` | Structural equality for values. Sequential collections compare by elements in order. Integers, ratios, and floats compare in their own categories. |
| `(== x more…)` | Numeric equality across number categories. A float compares with an integer or ratio by its exact value, so `(== 9007199254740993 9007199254740992.0)` and `(== 1/3 0.3333333333333333)` are false. NaN is unequal to itself under `==`. |
| `(< x more…)`, `(<= x more…)`, `(> x more…)`, `(>= x more…)` | Numeric ordering tests, exact across number categories as for `==`. `compare` and `sort` use the same order, with NaN after every other number. |
| `(identical? x y)` | Runtime identity test. |
| `(hash x)` | Hash a value. Values that are equal under `=` have the same hash. |

With `=`, NaNs compare equal and the two signed zeros compare equal.
Atoms, vars, functions, and other identities compare by identity.
See [classes](objects.md#class-definition-and-slots) for instance equality.

```clojure
[(+ 4611686018427387903 1)
 (/ 6 4)
 (= 1 1.0)
 (== 1 1.0)
 (= [1 2] '(1 2))
 (= ##NaN ##NaN)] ; => [4611686018427387904 3/2 false true true true]
```

## Persistent collections

Persistent operations such as `assoc` and `conj` return a new value.
Existing references to the input still see the original collection.

| Type | Constructor or literal | Conjoining an item |
| --- | --- | --- |
| List | `(list 1 2)` or `'(1 2)` | Prepends. |
| Vector | `[1 2]` or `(vector 1 2)` | Appends. |
| Map | `{:x 1}` or `(hash-map :x 1)` | Adds or replaces the entry from a pair such as `[:x 1]`. |
| Set | `#{1 2}` or `(hash-set 1 2)` | Adds the member if absent. |
| Sorted map or set | `(sorted-map :x 1)` or `(sorted-set 2 1)` | Adds in key or member order. |
| Queue | `(queue 1 2)` | Appends. `peek` and `pop` access the front. |

```clojure
[(conj [1 2] 3)
 (conj '(1 2) 3)
 (assoc {:x 1} :x 2)
 (peek (conj (queue 1 2) 3))] ; => [[1 2 3] (3 1 2) {:x 2} 1]
```

### Lookup and update

| Function | Behavior |
| --- | --- |
| `(get coll key)`, `(get coll key default)` | Lookup by map key, set member, or index into a vector, string, or byte sequence. Missing entries yield the default or `nil`. An entry whose value is `nil` still counts as present. |
| `(nth coll i)`, `(nth coll i default)` | Access by zero-based position. An index outside the collection signals `IndexError` when no default is supplied. |
| `(contains? coll key)` | Test for a map key, set member, or valid vector index. |
| `(assoc coll key val …)`, `(dissoc m key…)`, `(disj s member…)` | Add or replace entries, remove map entries, or remove set members. |
| `(get-in m ks)`, `(assoc-in m ks v)`, `(update-in m ks f args…)` | Read or update nested values along a path of keys. |
| `(update m k f args…)` | Apply `f` to the current value at `k` and any extra arguments, then store the result in the returned value. |
| `(subvec v start)`, `(subvec v start end)` | A vector slice with inclusive start and exclusive end. |
| `(keys m)`, `(vals m)` | The keys or values of a map. Hash maps and sets have no defined iteration order. |

```clojure
(let [m {:profile {:visits 2} :present nil}]
  [(get m :present :missing)
   (get m :absent :missing)
   (get-in (update-in m [:profile :visits] inc) [:profile :visits])
   (nth [10 20] 5 :missing)]) ; => [nil :missing 3 :missing]
```

### Transients

| Function | Behavior |
| --- | --- |
| `(transient coll)` | Create a mutable builder from a persistent vector, hash map, or hash set. |
| `(conj! t x)`, `(assoc! t k v)`, `(dissoc! t k)`, `(disj! t x)`, `(pop! t)` | Update a transient of a supported kind. Use the returned builder for subsequent updates. |
| `(persistent! t)` | Finish the builder and return a persistent value. |

A transient belongs to the fiber that created it. Using it from another
fiber or after `persistent!` signals an error.

```clojure
(let [t (transient [1 2])]
  (persistent! (conj! t 3))) ; => [1 2 3]
```

## Sequence traversal

`(seq coll)` returns a sequence view or `nil` when there are no items.
`first` returns the first item, or `nil` if empty. `rest` returns an empty
list when the tail is exhausted, while `next` returns `nil`. Views of maps
yield entries as key-value pairs.

```clojure
[(seq []) (first []) (rest [1]) (next [1]) (count [nil])] ; => [nil nil () nil 1]
```

### Eager transforms

| Function | Behavior |
| --- | --- |
| `(map f coll)`, `(map f c1 c2)`, `(map f c1 c2 c3)` | Apply `f` to items, with multiple collections traversed side by side until one runs out. |
| `(filter pred coll)`, `(remove pred coll)` | Select items for which the predicate is truthy or falsey, respectively. |
| `(keep f coll)` | Keep non-`nil` results of applying `f`. |
| `(take n coll)`, `(drop n coll)` | Keep or skip the first `n` items. |

For a single ordinary input collection, these operations run immediately
and preserve vectors, lists, and sets. On a map, selection operations such
as `filter` and `take` return a map. Computations such as `map` and `keep`
return a vector of results. For strings or finite ranges these operations
return vectors. Use `mapv` or `filterv` when the result must be a vector.

```clojure
[(map inc '(1 2))
 (map inc [1 2])
 (map first {:a 1})
 (filter (fn [[_ v]] (pos? v)) {:a 1 :b -1})
 (map + [1 2 3] [10 20])] ; => [(2 3) [2 3] [:a] {:a 1} [11 22]]
```

### Streams and ranges

`lazy` wraps a collection in a stream, and `lazy-seq` delays running its
body until the stream is needed. `iterate`, `(repeat x)`, and `(range)`
create unbounded streams. The forms `(range end)`, `(range start end)`,
and `(range start end step)` produce finite ranges with an exclusive end.
`(repeat n x)` produces a vector.

Single-input operations such as `map`, `filter`, and `take` return streams
when given a stream. Use `first` to read one item or `nth` to reach a specific item, or bound the
stream with `take` before calling `vec`, `count`, or printing. Those
operations consume the entire result. Keep lazy bodies free of side effects
when several fibers may realize the same stream, since more than one fiber
can run a cell's computation.

```clojure
[(vec (range 2 8 2))
 (vec (take 4 (map inc (iterate #(* 2 %) 1))))
 (vec (take 3 (repeat :tick)))] ; => [[2 4 6] [2 3 5 9] [:tick :tick :tick]]
```

### Reduction and transducers

| Function | Behavior |
| --- | --- |
| `(reduce f coll)`, `(reduce f init coll)` | Accumulate items with a step function. With no initial value, start with the first item, or call `(f)` for an empty input. |
| `(reduced x)` | Wrap a result to stop a reduction early. |
| `(into to from)`, `(into to xf from)` | Add items to the destination with `conj`, optionally through a transducer. |
| `(transduce xf f coll)`, `(transduce xf f init coll)` | Apply a transducer to a reducing function, reduce, then run its completion step. |
| `(completing f)`, `(completing f cf)` | Add a completion arity to a step function, using identity or `cf` to finish. |
| `(sequence xf coll)` | Run a transducer on the input and return a vector. |

`(map f)`, `(filter pred)`, and `(take n)` return transducers when the
collection argument is omitted. Combine them with `comp`. The order in
`comp` is the order in which values pass through the steps.

```clojure
(into [] (comp (filter odd?) (map inc) (take 2)) (range 10)) ; => [2 4]
(transduce (map inc) + 0 [1 2 3]) ; => 9
```
