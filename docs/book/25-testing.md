# 25. Testing

[Contents](README.md) · Previous: [24. Command-line programs](24-command-line-programs.md) · Next: [26. Tools and performance](26-tools-and-performance.md) <!-- nav -->

Norn ships its test tools in the standard library. `norn.test` gives you
`deftest` and `is`, fixtures, and a runner that `norn test` drives from the
shell. `norn.property` generates inputs, finds a failing case, and shrinks
it to the smallest one it can. A definition can carry executable examples in
its metadata, and `norn check` runs the formatter, the linter, the tests, and
those examples in one command. This chapter builds a small shop project and
tests it each of these ways.

## A first test

Tests live in files that end in `_test.norn`, usually under a `tests/` folder
beside `src/`. Start with a project that has one module. Its `norn.edn` says
where the source is (see [Chapter 8](08-modules-and-projects.md)):

```edn
{:name shop
 :version "0.1.0"
 :paths ["src"]}
```

This is `src/shop/cart.norn`. A cart is a vector of lines, and each line is
a map with a SKU, a quantity, and a unit price in cents.

```clojure
(ns shop.cart)

(defn line-total
  "The price of one cart line: quantity times unit price, in cents."
  [{:keys [qty price]}]
  (* qty price))

(defn total
  "The sum of every line in cart."
  [cart]
  (reduce + 0 (map line-total cart)))

(defn add-item
  "Add qty of sku at price to cart, merging with an existing line."
  [cart sku qty price]
  (when-not (pos? qty)
    (error "add-item: quantity must be positive, got" qty))
  (if-let [i (first (keep-indexed (fn [i l] (when (= sku (:sku l)) i)) cart))]
    (update-in cart [i :qty] + qty)
    (conj cart {:sku sku :qty qty :price price})))
```

The test file is a module too. It requires `norn.test` for the testing
forms and the module under test. Save this as `tests/cart_test.norn`:

```clojure
(ns shop.cart-test
  (:require [norn.test :refer [deftest is testing]]
            [shop.cart :as cart]))

(deftest totals
  (testing "an empty cart"
    (is (= 0 (cart/total []))))
  (testing "several lines"
    (is (= 1100 (cart/total [{:qty 2 :price 500} {:qty 1 :price 100}])))))

(deftest adding
  (let [c (-> [] (cart/add-item "pen" 2 150) (cart/add-item "pen" 1 150))]
    (is (= 1 (count c)) "the same sku merges into one line")
    (is (= 3 (:qty (first c)))))
  (is (thrown? Error (cart/add-item [] "pen" 0 150))))
```

`deftest` defines a function of no arguments and registers it with the
namespace, so the runner can find it. `is` checks one claim. `testing`
labels the checks inside it, and the label shows up when one of them fails.

Run the tests from the project root:

```sh
norn test
```

```text
tests/cart_test.norn                        5 checks  ok
2 tests, 5 checks: 5 passed, 0 failed, 0 errors
```

With no paths, `norn test` runs every `*_test.norn` file under `tests/`. It
reads `norn.edn` first, so `shop.cart` resolves from `src/`. The exit status
is 0 when everything passes and 1 otherwise, which is what a CI job needs.

A test is a function, so you can call it. That's rarely what you want,
because the checks inside it then report to no test, but it shows there is
nothing magic about `deftest`:

```clojure
(fn? totals) ; => true
```

## What `is` checks

`is` looks at the shape of the form you give it.

An `(= expected actual)` form compares, and a failure shows both values. Put
the expected value first. You can give more than one actual value, and each
must equal the expected one:

```clojure
(is (= 6 (+ 1 2 3) (* 2 3)))
```

A `(thrown? Class body…)` form expects the body to signal a condition of that
class, or of a subclass. `(thrown-with-msg? Class regex body…)` also matches
the condition's message against a regex:

```clojure
(is (thrown? ArithmeticError (/ 1 0)))
(is (thrown-with-msg? Error #"must be positive" (cart/add-item [] "pen" -1 150)))
```

The condition classes come from the built-in hierarchy described in
[Chapter 11](11-conditions-and-restarts.md). `Error` matches any error, so
prefer the narrowest class you mean.

Anything else must be truthy. A failure of a plain form reports the form and
the value it produced:

```clojure
(is (every? pos? (map :qty [{:qty 1} {:qty 4}])))
```

An optional second argument to `is` is a message, which the report prints
under the failing test's name. Use it when the form alone doesn't say why
the check matters.

`are` writes one `is` per group of values. The vector names the template's
parameters, and the values that follow fill them in, two at a time here:

```clojure
(require '[norn.test :refer [are]])

(deftest line-totals
  (are [expected line] (= expected (cart/line-total line))
    0    {:qty 0 :price 250}
    250  {:qty 1 :price 250}
    750  {:qty 3 :price 250}))
```

Unlike Clojure's `is`, Norn's returns `nil`. Don't build logic on its
result. Its job is to report.

## Reading a failure

Suppose someone adds a discount test before writing the discount, and a
second test passes a quantity as a string:

```clojure
(ns shop.broken-test
  (:require [norn.test :refer [deftest is testing]]
            [shop.cart :as cart]))

(deftest discount
  (testing "a cart over $10"
    (testing "gets 10% off"
      (is (= 990 (cart/total [{:qty 11 :price 100}])) "discount applied"))))

(deftest crashes
  (is (= 1 (cart/total [{:qty "two" :price 100}]))))
```

`norn test tests/broken_test.norn` prints each problem in full, then the
per-file line and the totals:

```text
FAIL in discount (tests/broken_test.norn:8)
  a cart over $10 › gets 10% off
  discount applied
  form:     (= 990 (cart/total [{:price 100, :qty 11}]))
  expected: 990
  actual:   1100
ERROR in crashes (tests/broken_test.norn:11)
  form:     (= 1 (cart/total [{:price 100, :qty "two"}]))
  error:    Expected a number, got String: "two"
tests/broken_test.norn                      2 checks  1 failed, 1 error
2 tests, 2 checks: 0 passed, 1 failed, 1 error
```

The two kinds of problem differ:

- A **failure** is a check that ran and came out false. The report names the
  test and, in parentheses, the file and line of the `is`. Then it shows the
  `testing` labels, outermost first, the message, the form, and the expected
  and actual values.
- An **error** is a condition that nobody handled while the check ran. `is`
  catches it, records it, and the test goes on to its next check. The report
  shows the condition's message in place of the values.

An error outside any `is`, in the test body itself, ends that test. The
runner records it as one error with the message "the test raised an error"
and moves on to the next test. That holds even when the code running the
tests has a `handler-case` around `run-ns`: while a test runs, the runner
sets the caller's handlers aside, so the error is the test's. In both cases the error doesn't reach the
debugger, so a broken test can't park a fiber and hang the run. A plain
`signal` that nobody handles still returns `nil`, as it would outside a
test.

## Tests at the REPL

`norn test` is a shell command, but the runner is a library, and while you
work you usually run tests from the REPL (see
[Chapter 12](12-live-programming.md)). `run-tests` runs the current
namespace's tests, or the namespaces you name, prints the same report, and
returns the totals:

```clojure
(in-ns 'shop.cart-test)
(norn.test/run-tests)
; prints: shop.cart-test                              8 checks  ok
; prints: 3 tests, 8 checks: 8 passed, 0 failed, 0 errors
```

`run-ns` returns the results as data instead of printing them. Each result
is a map with the test's `:name`, its `:pass`, `:fail` and `:error` counts,
its `:events`, and how long it took in `:ms`:

```clojure
(def results (norn.test/run-ns 'shop.broken-test))

(mapv #(select-keys % [:name :pass :fail :error]) results)
; => [{:name "discount" :pass 0 :fail 1 :error 0}
;     {:name "crashes" :pass 0 :fail 0 :error 1}]
```

The REPL loop for a failing test is short. Change the function, evaluate the
new definition, and run the tests again. Redefining a test with `deftest`
replaces it in place, so it keeps its position in the run order:

```clojure
(in-ns 'shop.broken-test)

(deftest discount
  (testing "no discount yet"
    (is (= 1100 (cart/total [{:qty 11 :price 100}])))))

(mapv first (norn.test/tests 'shop.broken-test)) ; => [discount crashes]
(def results (norn.test/run-ns 'shop.broken-test))
(:fail (first results)) ; => 0
```

`run-file` loads a test file and runs it, and `run-files` runs several, as
`norn test` does. Both return data. Checks in a file that sit outside any
`deftest` still count, under a test named `<load>`, so a file of plain
top-level checks is a valid test file.

## Fixtures

A fixture sets things up around tests. It is a function that receives the
test, as a function of no arguments, and calls it. `use-fixtures` installs
fixtures for the current namespace: `:each` fixtures wrap every test, and
`:once` fixtures wrap the whole namespace's run.

Say the shop looks prices up in a catalog held in a dynamic var (dynamic
vars are covered in [Chapter 17](17-shared-state.md)). An `:each` fixture
binds a small catalog for every test, so each test sees the same starting
point:

```clojure
(ns shop.catalog-test
  (:require [norn.test :refer [deftest is use-fixtures]]))

(def ^:dynamic *catalog* nil)
(def log (atom []))

(defn price-of [sku] (get *catalog* sku))

(use-fixtures :once
  (fn [run]
    (swap! log conj :open)
    (try (run) (finally (swap! log conj :close)))))

(use-fixtures :each
  (fn [run]
    (binding [*catalog* {"pen" 150 "pad" 300}]
      (run))))

(deftest known-prices
  (is (= 150 (price-of "pen"))))

(deftest unknown-prices
  (is (nil? (price-of "ink"))))

(:pass (first (norn.test/run-ns 'shop.catalog-test))) ; => 1
@log ; => [:open :close]
```

The `:once` fixture uses `try` and `finally` for its cleanup. When a test
raises an error, the runner unwinds out of it, so cleanup that sits after
`(run)` without `finally` would be skipped. Write fixtures that free
resources, such as temporary directories or database connections, this way.

Fixtures run in the order you list them, the first outermost. Calling
`use-fixtures` again for the same kind replaces that kind's fixtures, which
is what you want when you re-evaluate the file at the REPL.

## Choosing what runs

`norn test` takes files and folders. A folder stands for every
`*_test.norn` file under it, in sorted order.

```sh
norn test tests/cart_test.norn
norn test tests/ more-tests/
norn test --only 'parts|adding'
```

`--only` takes a regular expression and runs only the tests whose names
match it. Files still load, so their `<load>` checks still count.

Test files run in parallel, each on its own fibers (see
[Chapter 15](15-fibers-and-scopes.md)). Within a file, tests run one after
another, in the order you defined them. A namespace whose tests share no
state they change can call `(norn.test/parallel-tests!)` at the top level,
and then its tests run concurrently, one fiber each. `norn test --serial`
turns all of that off and runs everything one at a time, which helps when
tests share an outside resource, or when output from different files is
interleaving.

A check inside a fiber that a test spawns reports to that test, because the
dynamic bindings that route reports carry into spawned fibers:

```clojure
(in-ns 'shop.cart-test)

(deftest in-parallel
  (with-scope [s]
    (let [a (spawn s (cart/total [{:qty 1 :price 5}]))
          b (spawn s (is (= 10 (cart/total [{:qty 2 :price 5}]))))]
      (is (= 5 (await a)))
      (await b))))

(select-keys (last (norn.test/run-ns 'shop.cart-test)) [:name :pass])
; => {:name "in-parallel" :pass 2}
```

## Reports for CI

Besides the default report, the runner writes TAP and JUnit XML. `--tap`
replaces the report on standard output with TAP version 13, one line per
test, with failures as `#` comments:

```sh
norn test --tap
```

```text
TAP version 13
1..4
not ok 1 - shop.broken-test/discount
  # FAIL in discount (tests/broken_test.norn:8)
  #   a cart over $10 › gets 10% off
  #   discount applied
  #   form:     (= 990 (cart/total [{:price 100, :qty 11}]))
  #   expected: 990
  #   actual:   1100
  # 
not ok 2 - shop.broken-test/crashes
  # ERROR in crashes (tests/broken_test.norn:11)
  #   form:     (= 1 (cart/total [{:price 100, :qty "two"}]))
  #   error:    Expected a number, got String: "two"
  # 
ok 3 - shop.cart-test/totals
ok 4 - shop.cart-test/adding
```

`--junit FILE` writes JUnit XML to a file, one `testsuite` per test file
and one `testcase` per test, while the usual report still goes to standard
output. Most CI systems read one of these two formats.

## Properties

Example-based tests check the cases you thought of. Property-based tests
check a claim against cases a generator makes up, which finds the ones you
didn't think of. `norn.property` provides the generators, a runner, and
`defproperty`, which makes a test that `norn test` runs like any other.

The shop needs to split a bill between several people. Here's a first
version, in `src/shop/money.norn`:

```clojure
(ns shop.money)

(defn split-evenly
  "Split total cents into n parts that add up to total."
  [total n]
  (vec (repeat n (quot total n))))

(split-evenly 900 3) ; => [300 300 300]
```

The claim worth testing is that the parts add up to the total. A property
states it once, for any total and any number of people:

```clojure
(ns shop.money-test
  (:require [norn.test :refer [deftest is]]
            [norn.property :as p]
            [shop.money :as money]))

(p/defproperty parts-add-up
  [total (p/naturals)
   n (p/integers :min 1 :max 10)]
  (= total (reduce + 0 (money/split-evenly total n))))
```

The bindings look like `let`, but each value is a generator. `p/naturals`
generates integers from 0 up, and `p/integers` with `:min` and `:max`
generates integers in that range. The body is the claim. A case fails if
the body returns `false` or `nil`, or signals an error.

Run it with `norn test tests/money_test.norn` and the property fails:

```text
FAIL in parts-add-up (tests/money_test.norn:6)
  form:     (for-all [total (p/naturals) n (p/integers :min 1 :max 10)] (= total (reduce + 0 (money/split-evenly total n))))
  expected: "the property to hold"
  actual:   "falsified with {:n 2, :total 1} (seed 750010872893859933, case 4; rerun with :seed 750010872893859933)"
tests/money_test.norn                       1 check   1 failed, 0 errors
1 test, 1 check: 0 passed, 1 failed, 0 errors
```

The first failing case was larger. The runner then **shrinks** it: it
tries simpler versions of the case and keeps each one that still fails,
until none of its edits fail any more. One cent between two people is about
as small as this bug gets, and it makes the problem plain. `quot` drops the
remainder.

Each run starts from a random seed, and the report prints it. Pass the seed
back to rerun exactly the same cases. `p/check` runs a property outside the
test runner and returns the outcome as a map, which is handy at the REPL:

```clojure
(def adds-up
  (p/for-all [total (p/naturals) n (p/integers :min 1 :max 10)]
    (= total (reduce + 0 (money/split-evenly total n)))))

(select-keys (p/check adds-up :seed 1) [:pass? :args :original])
; => {:pass? false :args {:total 1 :n 2} :original {:total 2 :n 8}}
```

`:original` is the case that failed first, and `:args` is the shrunk one.
`p/for-all` makes a property value, and `defproperty` is a `deftest` that
calls `p/check` on one. In `defproperty`, options go in a map before the
bindings, for example `{:runs 500 :seed 1}`. The default is 100 runs, with
sizes growing from small to large over the run.

Fix the function so the first parts get the extra cents:

```clojure
(in-ns 'shop.money)

(defn split-evenly
  "Split total cents into n parts that add up to total. The first parts
  get the extra cents."
  [total n]
  (let [base (quot total n)
        extra (rem total n)]
    (mapv (fn [i] (if (< i extra) (inc base) base)) (range n))))

(split-evenly 1000 3) ; => [334 333 333]

(in-ns 'shop.money-test)
(:pass? (p/check adds-up :seed 1)) ; => true
```

The property passes. Ask what else should be true. The parts should be as
even as possible, never more than a cent apart. And what about a refund,
which is a negative total? Widen the generator to `p/integers` and see:

```clojure
(:pass? (p/check (p/for-all [total (p/naturals) n (p/integers :min 1 :max 10)]
                   (let [parts (money/split-evenly total n)]
                     (<= (- (apply max parts) (apply min parts)) 1)))))
; => true

(:args (p/check (p/for-all [total (p/integers) n (p/integers :min 1 :max 10)]
                  (= total (reduce + 0 (money/split-evenly total n))))
                :seed 1))
; => {:total -1 :n 2}
```

`(rem -1 2)` is `-1`, so no part gets the extra cent. The property found an
input the function was never meant for. Whether to support it is a design
decision. Here the shop rejects it, and an ordinary test says so:

```clojure
(in-ns 'shop.money)

(defn split-evenly
  "Split total cents into n parts that add up to total. The first parts
  get the extra cents."
  [total n]
  (when (neg? total)
    (error "split-evenly: total must not be negative, got" total))
  (let [base (quot total n)
        extra (rem total n)]
    (mapv (fn [i] (if (< i extra) (inc base) base)) (range n))))

(in-ns 'shop.money-test)

(deftest negative-totals
  (is (thrown? Error (money/split-evenly -1 2))))

(mapv :fail (norn.test/run-ns 'shop.money-test)) ; => [0 0]
```

### Generators

A generator is a value. `p/generate` draws one from it, which is the
quickest way to see what a generator makes. Give it a size and a seed to
get the same value each time:

```clojure
(p/generate (p/elements [:small :medium :large]) 30 3) ; => :large
(p/generate (p/vectors (p/integers) :max 5) 10 1) ; => [-8 -1]
(p/generator? (p/strings)) ; => true
```

The building blocks cover the usual values: `integers`, `naturals`,
`floats`, `booleans`, `chars`, `strings`, `keyword`, `symbol`, and
`byte-strings`. Collections come from `vectors`, `lists`, `sets`, `maps`
and `tuple`, which take generators for their items. `constant`, `elements`,
`one-of` and `frequency` choose among values or generators.

To make a generator for your own data, transform an existing one. `fmap`
applies a function to each generated value. `bind` uses a generated value
to choose the next generator. `such-that` keeps only values that pass a
predicate, so use it for rare exclusions, not to filter most values out:

```clojure
(require '[shop.cart :as cart])

(def cart-line
  (p/fmap (fn [[sku qty price]] {:sku sku :qty qty :price price})
          (p/tuple (p/elements ["pen" "pad" "ink"])
                   (p/integers :min 1 :max 20)
                   (p/integers :min 0 :max 10000))))

(def carts (p/vectors cart-line :max 8))

(every? #(contains? % :sku) (p/generate carts 30 5)) ; => true

(:pass? (p/check (p/for-all [c carts]
                   (= (cart/total c) (reduce + 0 (map cart/line-total c)))))) ; => true
```

A binding can use an earlier one, as in
`[n (p/naturals) v (p/vectors (p/integers) :max n)]`. For trees, such as
nested data for a codec, `p/recursive` takes a function from a generator of
smaller trees to a generator of branches, and a generator of leaves:

```clojure
(def nested
  (p/recursive (fn [inner] (p/vectors inner :max 3))
               (p/integers :min 0 :max 9)))

(require '[norn.edn :as edn])

(:pass? (p/check (p/for-all [x nested] (= x (edn/read-string (pr-str x))))))
; => true
```

That last property, "writing then reading gives the value back," is the
most useful one to write for any format or codec. The standard library's
own codecs are tested this way.

Shrinking works for every generator, including ones you build with `fmap`
and `bind`, with no shrinking code of your own. The runner records the
random numbers each case drew and shrinks those, and every generator gives
simpler values for smaller numbers. Generated values shrink toward 0, empty
collections, `false`, the first choice, and `\a`.

## Examples in docstrings

A definition can carry examples in its attribute map, the map that may
follow the docstring. Each example is a pair: an expression and the value it
should equal.

```clojure
(in-ns 'shop.cart)

(defn total
  "The sum of every line in cart."
  {:examples [[(total []) 0]
              [(total [{:qty 1 :price 100} {:qty 2 :price 5}]) 110]]}
  [cart]
  (reduce + 0 (map line-total cart)))
```

`doc` shows them, so a reader sees how to call the function:

```clojure
(doc total)
; prints: -------------------------
; prints: shop.cart/total
; prints: ([cart])
; prints:   The sum of every line in cart.
; prints:   Examples:
; prints:     (total []) ;=> 0
; prints:     (total [{:price 100, :qty 1} {:price 5, :qty 2}]) ;=> 110
```

`norn check` evaluates each example in the definition's namespace and
compares the two sides with `=`. Examples are documentation first. They
show typical calls, and checking them keeps them true. They don't replace
tests: the first `split-evenly` would have passed an example of
`(split-evenly 900 3)`.

## `norn check`

`norn check` runs every check a project has, in order:

1. `norn fmt --check`, which reports files the formatter would change.
2. `norn lint`, which reports unresolved names, unused requires, and
   similar problems.
3. `norn test`, over `tests/`.
4. The documentation examples of every definition loaded by then.

Each step runs even when an earlier one fails, and the command exits 1 if
any step found a problem. Suppose the shop project has an unused `are`
left in a `:refer`, and an example on `split-evenly` that expects the extra
cent at the end. `norn check` reports:

```text
== fmt
ok
== lint
./tests/cart_test.norn:1: unused-require: norn.test/are is referred but never used
FAILED
== test
tests/cart_test.norn                        5 checks  ok
tests/money_test.norn                       3 checks  ok
5 tests, 8 checks: 8 passed, 0 failed, 0 errors
ok
== doc examples
  shop.money/split-evenly: (split-evenly 1000 3) gave [334 333 333], expected [333 333 334]
FAILED
```

Only loaded definitions have their examples checked, and the tests are what
load them. A module that no test requires has its examples skipped. Give
every module at least one test file, and `norn check` covers it. The
formatter and linter are covered in
[Chapter 26](26-tools-and-performance.md).

## Summary

- Tests go in `*_test.norn` files. `deftest` registers a test, `is` checks a
  claim, `testing` labels checks, and `are` repeats a template over values.
- `(is (= expected actual))` shows both values on failure. `thrown?` and
  `thrown-with-msg?` expect a condition. An error inside `is` is recorded
  and the test continues.
- `norn test` runs `tests/` in parallel by file, with `--only`, `--serial`,
  `--tap` and `--junit FILE`. Its exit status says whether everything
  passed.
- At the REPL, `run-tests` prints a report and `run-ns`, `run-file` and
  `run-files` return results as data.
- `use-fixtures` wraps tests with `:each` and `:once` fixtures. Put cleanup
  in `finally`.
- `norn.property` checks claims against generated cases, shrinks failures,
  and prints a seed that reproduces them. Build generators for your own
  data with `fmap`, `bind`, `tuple` and `recursive`.
- `:examples` in a definition's attribute map document typical calls.
  `norn check` runs them along with the formatter, the linter, and the tests.

---

[Contents](README.md) · Previous: [24. Command-line programs](24-command-line-programs.md) · Next: [26. Tools and performance](26-tools-and-performance.md) <!-- nav -->
