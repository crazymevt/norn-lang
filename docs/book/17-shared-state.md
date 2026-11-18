# 17. Shared state

[Contents](README.md) · Previous: [16. Channels](16-channels.md) · Next: [18. Actors and supervision](18-actors-and-supervision.md) <!-- nav -->

Skein's values don't change, so fibers can share them freely: a vector one
fiber reads can't be modified under it by another. Programs still need
things that change over time, such as a counter, a table of sessions, or the
balance of an account. Skein keeps change in a few kinds of reference: an
identity that holds one value at a time and moves from value to value in
controlled ways. This chapter covers atoms, the everyday reference; dynamic
vars, which carry context rather than shared state; refs and transactions in
`skein.stm`, for changes that must touch several references at once; the
coordination tools in `skein.sync`; and caches from `skein.cache`.

## Atoms

An atom holds one value. `(atom x)` makes one, and `deref`, or its reader
shorthand `@`, reads the current value:

```clojure
(def hits (atom 0))
@hits ; => 0
```

There are two ways to change it. `reset!` stores a new value outright.
`swap!` takes a function and replaces the value with the result of calling
that function on the old value. Any extra arguments are passed after the old
value. Both return the new value:

```clojure
(reset! hits 10) ; => 10
(swap! hits inc) ; => 11
(swap! hits + 5) ; => 16
```

`swap!` is the one you'll use most. It is safe when many fibers update the
same atom at once. A hundred fibers each adding one a hundred times always
give ten thousand:

```clojure
(def total (atom 0))

(with-scope [s]
  (dotimes [_ 100]
    (spawn s (dotimes [_ 100] (swap! total inc)))))

@total ; => 10000
```

Atoms usually hold more than a number. An atom holding a map is a small
in-memory database, and `swap!` with `assoc`, `update` or `update-in` changes
part of it. Here is a store of account balances, which this chapter will
come back to:

```clojure
(def bank (atom {:alice 100 :bob 50}))

(defn deposit! [who amount]
  (swap! bank update who + amount))

(deposit! :bob 25) ; => {:alice 100 :bob 75}
```

Because the update function sees the whole map and returns a whole new map,
an update that touches several keys is still one atomic change. Other fibers
see the map before the transfer or after it, never halfway:

```clojure
(defn transfer! [from to amount]
  (swap! bank (fn [accounts]
                (-> accounts
                    (update from - amount)
                    (update to + amount)))))

(transfer! :alice :bob 30) ; => {:alice 70 :bob 105}
```

For many programs that is all the shared state they need: one atom, holding
a map, changed with pure functions.

### How swap! works

`swap!` reads the current value, calls your function on it, and then stores
the result only if the atom still holds the value it read. If another fiber
changed the atom in the meantime, `swap!` reads the new value and calls the
function again. This is called optimistic concurrency: no fiber waits for a
lock, and a conflict costs a recomputation.

The consequence is a rule: the function you pass to `swap!` may run more
than once, so it must not have side effects. Don't print, write a file, or
send a message from it. Compute the new value, and do the side effect after
`swap!` returns, using the value it returns.

When you need the old value as well as the new one, `swap-vals!` and
`reset-vals!` return both, as `[old new]`:

```clojure
(def ticket (atom 41))
(swap-vals! ticket inc) ; => [41 42]
(reset-vals! ticket 0) ; => [42 0]
```

A common use is taking a value out of an atom and leaving something else
behind, such as draining a queue of pending work, in one step:

```clojure
(def pending (atom [:email :report :backup]))

(let [[old _] (reset-vals! pending [])]
  old) ; => [:email :report :backup]

@pending ; => []
```

### compare-and-set!

`swap!` is built on `compare-and-set!`, which you can also call directly.
`(compare-and-set! a old new)` stores `new` only if the atom currently holds
`old`, and returns whether it did. "Holds `old`" means the very same object,
not an equal one:

```clojure
(def state (atom :idle))
(compare-and-set! state :idle :running) ; => true
(compare-and-set! state :idle :running) ; => false
@state ; => :running

(def pair (atom [1 2]))
(compare-and-set! pair [1 2] [3 4]) ; => false
```

Keywords and small integers are always the same object, so the first
example behaves as you'd hope. The second fails because the vector `[1 2]`
written in the call is a new vector, equal to the one in the atom but not the
same object. That is the right test for `swap!`'s purpose: it checks whether
anyone has stored anything since it read the value. Use `compare-and-set!`
with values you read from the atom, or with keywords as state markers, as
the first example does.

### Watches

`(add-watch a key f)` asks to be told about changes. After every change
through `swap!`, `reset!`, `swap-vals!` or `reset-vals!`, Skein calls `(f key
a old new)`. `remove-watch` takes the watch away again:

```clojure
(def temperature (atom 20))
(def readings (atom []))

(add-watch temperature :log
  (fn [_key _atom old new]
    (swap! readings conj [old new])))

(reset! temperature 22)
(swap! temperature + 3)
(remove-watch temperature :log)
(reset! temperature 0)

@readings ; => [[20 22] [22 25]]
```

The watch runs in the fiber that made the change, after the change. A raw
`compare-and-set!` doesn't call watches. Atoms in Skein don't have
validators; if you need one, refs, below, do.

### Volatiles

`volatile!`, `vswap!` and `vreset!` exist for code that comes from Clojure,
where a volatile is a faster, unsynchronized box. In Skein a volatile is an
atom, so these behave exactly like `atom`, `swap!` and `reset!` and are no
faster:

```clojure
(let [v (volatile! 0)]
  (vswap! v + 2)
  @v) ; => 2
```

## Dynamic vars

A var, the thing `def` creates, normally holds one value for the whole
program. `binding` gives a var a different value for the dynamic extent of
its body: during the body and anything it calls, in this fiber and in the
fibers it spawns. When the body ends, the old value is back:

```clojure
(def ^:dynamic *indent* "")

(defn line [text] (str *indent* text))

(line "top") ; => "top"
(binding [*indent* "    "]
  (line "nested")) ; => "    nested"
(line "top again") ; => "top again"
```

A binding is not shared state. It belongs to the fiber that made it (and
the fibers it spawns), so another fiber running `line` at the same moment
still sees `""`. That makes dynamic vars good for context, such as an output
stream, a request id, a depth limit or a setting, that code deep in a call
chain needs without every function in between passing it along. Several of
Skein's own settings work this way: `with-out-str` rebinds `*out*`, and
`skein.stm` reads `*max-retries*`. [Chapter
15](15-fibers-and-scopes.md#what-a-child-inherits) shows how bindings are
conveyed to child fibers.

In Skein, `binding` works on any var. The `^:dynamic` metadata and the
`*earmuffs*` naming convention don't change behavior; they tell readers which
vars are meant to be rebound, so use them. There is no `set!` on a bound
var. To change a var's root value for everyone, use `alter-var-root`, which
works like `swap!` on the var. Reach for it rarely; redefining a var with
`def` at the REPL is the usual way to change one during development.

```clojure
(def ^:dynamic *limit* 10)
(alter-var-root #'*limit* * 2) ; => 20
*limit* ; => 20
```

## Coordinated change: skein.stm

One atom holding a map handles many-key updates within that map. Sometimes
state is split across several references, though. The accounts may belong
to different modules, or be created at different times, or you may want
fibers working on unrelated accounts not to contend with each other. Changing
two atoms one after the other leaves a moment when one has changed and the
other hasn't, and another fiber can see it.

`skein.stm` provides software transactional memory for this case. A ref is
a reference like an atom, and a transaction changes any number of refs all
at once, or not at all:

```clojure
(require '[skein.stm :as stm])

(def checking (stm/ref 100))
(def savings (stm/ref 500))

(defn move! [from to amount]
  (stm/dosync
    (stm/alter from - amount)
    (stm/alter to + amount)))

(move! savings checking 50) ; => 150
[@checking @savings] ; => [150 450]
```

`dosync` runs its body as a transaction and returns the body's value.
`alter` is the transaction's `swap!`: it sets the ref to `(f current-value
args…)`. `ref-set` sets a ref to a value outright. Outside a transaction,
`@` reads a ref's current value; you can't change a ref there:

```clojure
(stm/alter checking + 1) ; !! SimpleError
```

### What a transaction guarantees

A transaction sees one consistent moment. Every ref it reads shows its value
as of the moment the transaction began, plus the transaction's own changes,
whatever other fibers commit meanwhile. When the body finishes, the
transaction commits: it checks whether another transaction has changed any
ref it altered since it began, and if not, it makes all its changes visible
together.

If another transaction got there first, there is a conflict, and the
transaction runs again from the start, at a new moment, with the new values.
That is why many fibers can move money between accounts without any total
ever going wrong:

```clojure
(def accounts (mapv (fn [_] (stm/ref 100)) (range 4)))

(with-scope [s]
  (dotimes [i 200]
    (spawn s
      (move! (nth accounts (mod i 4)) (nth accounts (mod (+ i 1) 4)) 10))))

(reduce + (map deref accounts)) ; => 400
```

Two hundred transfers between four accounts ran at the same time. Some of
them conflicted and ran again, but every one of them happened exactly once
in the end, and money was neither created nor lost.

The cost of this is the same rule as for `swap!`, and more important here:
the body of `dosync` may run more than once. You can see it happen by making
two transactions overlap on purpose:

```clojure
(def balance (stm/ref 0))
(def runs (atom 0))

(with-scope [s]
  (spawn s
    (stm/dosync
      (swap! runs inc)                ; a side effect, to count the runs
      (let [seen @balance]
        (sleep 300)
        (stm/alter balance + 1))))
  (sleep 100)
  (stm/dosync (stm/alter balance + 100)))

[@runs @balance] ; => [2 101]
```

The first transaction read `balance` and went to sleep. While it slept, the
second one added 100 and committed. When the first woke up and tried to
change `balance`, its view was out of date, so it ran again, this time seeing
100 and adding 1. The `swap!` on `runs` happened twice. Had it been a
deposit to an outside system, it would have happened twice too.

So keep side effects out of transactions. `io!` marks code that must not
run in one, and refuses to:

```clojure
(defn send-receipt! [amount]
  (stm/io! (str "receipt for " amount)))

(send-receipt! 5) ; => "receipt for 5"
(stm/dosync (send-receipt! 5)) ; !! SimpleError
```

When a side effect belongs to a transaction, register it with
`after-commit`. The function runs once, after the transaction commits, and
not at all if it fails:

```clojure
(def receipts (atom []))

(stm/dosync
  (stm/alter checking - 20)
  (stm/after-commit (fn [] (swap! receipts conj [:withdrew 20 :left @checking]))))

@receipts ; => [[:withdrew 20 :left 130]]
```

### Failure and validation

An error inside a transaction ends it without committing. None of its
changes become visible:

```clojure
(handler-case
  (stm/dosync
    (stm/alter checking - 1000)
    (error "card declined"))
  (Error [e] (condition-message e))) ; => "card declined"

@checking ; => 130
```

A ref can have a validator, a function that every new value must satisfy.
A transaction that would give the ref a value its validator refuses fails
at commit with an error, and again nothing changes:

```clojure
(def wallet (stm/ref 10 :validator (fn [n] (>= n 0))))

(handler-case (stm/dosync (stm/alter wallet - 25))
  (Error [_] :refused)) ; => :refused

@wallet ; => 10
```

Refs hold values, not mutable objects. A transaction that runs twice can
undo its changes to refs, but not a change made inside a mutable object, so
`ref`, `alter` and `ref-set` refuse a `(:mutable)` object unless it has been
shared ([Chapter 10](10-protocols-and-extension.md)). Watches work on refs as
on atoms, and run after the commit.

A transaction belongs to the fiber that runs it. A fiber spawned inside
`dosync` is not part of the transaction, and `alter` there raises the same
"not in a transaction" error as outside one. Do the concurrent work first,
then change the refs in one transaction with the results.

### commute and ensure

Two more operations tune how transactions conflict.

`commute` is like `alter` for functions whose order doesn't matter, such as
`+` or `conj` onto a set. Two transactions that commute the same ref don't
conflict: at commit, Skein applies the function again to the ref's newest
value. A shared counter is the typical use:

```clojure
(def page-views (stm/ref 0))

(with-scope [s]
  (dotimes [_ 100]
    (spawn s (stm/dosync (stm/commute page-views inc)))))

@page-views ; => 100
```

With `alter`, those hundred transactions would conflict with one another and
retry. With `commute`, they don't. The value `commute` returns inside the
transaction is the transaction's own view; the committed value may differ,
because other increments landed first.

`ensure` handles the opposite problem. A transaction conflicts only over
refs it changes. If it reads a ref, makes a decision based on it, and changes
a different ref, another transaction can change the first ref meanwhile, and
both commit. This is called write skew. The classic example is two doctors
on call, each allowed to go off call only if the other stays on:

```clojure
(defn go-off-call [me other read-other]
  (stm/dosync
    (when (and @me (read-other other))
      (sleep 200)
      (stm/ref-set me false))))

(defn both-try [read-other]
  (let [alice (stm/ref true)
        bob (stm/ref true)]
    (with-scope [s]
      (spawn s (go-off-call alice bob read-other))
      (spawn s (go-off-call bob alice read-other)))
    [@alice @bob]))

(both-try deref) ; => [false false]
```

Each transaction read the other doctor's ref as `true` and changed only its
own, so neither conflicted with the other, and now nobody is on call. Reading
the other ref with `ensure` instead of `deref` makes the transaction
conflict with any transaction that changes it. One of the two then runs
again, sees that the other doctor has gone, and stays:

```clojure
(count (filter true? (both-try stm/ensure))) ; => 1
```

Which doctor stays depends on which transaction commits first.

`skein doc skein.stm` lists the rest: `in-transaction?`, `ref?`, `stm-stats`
for counting commits and retries, the `*max-retries*` limit, and the
`:max-history` option of `ref`.

### Atoms or refs?

Start with atoms. One atom holding a map covers most shared state, it's
clear to read, and every update is a single atomic step. Use refs when the
state that must change together lives in several references and putting it
into one map would be awkward, or when fibers updating unrelated parts of a
big map contend so much that their `swap!`s keep retrying. Transactions cost
more than atom updates, and their retries make side effects tricky, so use
them where the coordination pays for itself.

## Coordinating fibers: skein.sync

Atoms and refs protect data. Sometimes what you need to coordinate is the
fibers themselves: how many may do something at once, or when they may go
on. `skein.sync` provides the classic tools, built on channels and atoms, so
waiting in them parks a fiber and never blocks a carrier.

```clojure
(require '[skein.sync :as sync])
```

### Semaphores

A semaphore holds a number of permits. `acquire!` takes one, parking until
one is free, and `release!` gives it back. `with-permit` does both around a
body, releasing even if the body fails. Use it to limit how many fibers use
something at once, such as connections to a server:

```clojure
(def connections (sync/semaphore 2))
(def in-use (atom 0))
(def most-in-use (atom 0))

(with-scope [s]
  (dotimes [_ 6]
    (spawn s
      (sync/with-permit connections
        (swap! most-in-use max (swap! in-use inc))
        (sleep 100)
        (swap! in-use dec)))))

@most-in-use ; => 2
(sync/available connections) ; => 2
```

Six fibers wanted a connection, but never more than two held one at a time.
`try-acquire!` takes a permit only if one is free now and returns whether it
did.

### Latches and barriers

A latch opens once, after a given number of `count-down!` calls. Fibers
waiting in `await-latch` park until then. It's the tool for "wait until
these n things are ready":

```clojure
(def ready (sync/latch 3))
(def started (atom #{}))

(with-scope [s]
  (doseq [service [:db :cache :queue]]
    (spawn s
      (sleep 5)
      (swap! started conj service)
      (sync/count-down! ready)))
  (sync/await-latch ready)
  @started) ; => #{:db :cache :queue}
```

`await-latch` with a second argument gives up after that many milliseconds
and returns `false`. `latch-count` says how many count-downs are left.

A barrier is for fibers that work in phases. `(sync/barrier n)` makes one
for `n` fibers, and each fiber that calls `arrive!` parks until all `n` have
arrived. Then they all go on, and the barrier resets for the next phase:

```clojure
(def phase-log (atom []))
(def step (sync/barrier 3))

(with-scope [s]
  (dotimes [i 3]
    (spawn s
      (swap! phase-log conj [:load i])
      (sync/arrive! step)
      (swap! phase-log conj [:compute i]))))

(mapv first @phase-log) ; => [:load :load :load :compute :compute :compute]
```

The fibers ran in no particular order, but no fiber started computing until
all three had loaded.

### Rate limits

`(sync/rate-limiter rate burst)` is a token bucket: it refills at `rate`
tokens a second and holds at most `burst`. `take-token!` takes a token,
sleeping until one is available, and `try-take!` takes one only if it can
right away:

```clojure
(def api-limit (sync/rate-limiter 1 3))

(mapv (fn [_] (sync/try-take! api-limit)) (range 4)) ; => [true true true false]
```

The bucket starts full, so three requests go through at once, and the fourth
must wait about a second for the next token.

### Doing something once

`(sync/once f)` returns a function that calls `f` the first time it's
called and returns that same result every time after. Fibers that call it
while `f` is still running wait for it, so `f` runs once even when many
fibers race to be first:

```clojure
(def loads (atom 0))
(def load-config (sync/once (fn [] (swap! loads inc) {:port 8080})))

(with-scope [s]
  (dotimes [_ 10] (spawn s (load-config))))

[(load-config) @loads] ; => [{:port 8080} 1]
```

If `f` fails, every call raises that same error; it isn't tried again.

A delay is the same idea as a value. `(delay body…)` doesn't run its body
until the first `deref` (or `force`), then keeps the result. Fibers that
deref it while it's being computed wait for it:

```clojure
(def settings (delay (swap! loads inc) {:theme :dark}))

(realized? settings) ; => false
@settings ; => {:theme :dark}
@settings ; => {:theme :dark}
(realized? settings) ; => true
@loads ; => 2
```

`loads` went from 1 to 2: the body ran once, at the first `deref`. Like
`once`, a delay whose body fails raises the same condition on every `deref`.

## Caches: skein.cache

`memoize`, in `skein.core`, wraps a function so it remembers its results by
argument list. It remembers every result forever, which is right for a small
set of arguments and wrong for, say, user ids:

```clojure
(def squarings (atom 0))
(def slow-square (memoize (fn [n] (swap! squarings inc) (* n n))))

[(slow-square 12) (slow-square 12) @squarings] ; => [144 144 1]
```

`skein.cache` adds caches with limits. `(cache/lru n)` holds at most `n`
entries and evicts the least recently used one when it needs room.
`(cache/ttl ms)` forgets each entry `ms` milliseconds after it was stored.
Both are used with the same functions: `store!` adds an entry, `lookup`
finds one (a hit counts as a use, for an LRU cache), and `through` looks a
key up and, on a miss, computes the value with a function and stores it:

```clojure
(require '[skein.cache :as cache])

(def recent (cache/lru 2))
(cache/store! recent :a 1)
(cache/store! recent :b 2)
(cache/lookup recent :a) ; => 1
(cache/store! recent :c 3)

[(cache/lookup recent :a) (cache/lookup recent :b) (cache/lookup recent :c)]
; => [1 nil 3]
```

Looking up `:a` made it recently used, so storing `:c` evicted `:b`.
`lookup` takes a third argument to return instead of `nil` on a miss, which
matters when `nil` is a value you cache. `evict!`, `clear!` and `size` do
what their names say.

A TTL cache suits values that go stale, such as a remote configuration:

```clojure
(def remote-config (cache/ttl 200))

(cache/through remote-config :flags (fn [_] #{:beta})) ; => #{:beta}
(cache/lookup remote-config :flags) ; => #{:beta}
(sleep 300)
(cache/lookup remote-config :flags :expired) ; => :expired
```

`memo-lru` and `memo-ttl` combine the two ideas: they memoize a function in
a bounded cache.

```clojure
(def lookups (atom 0))
(def find-user
  (cache/memo-lru (fn [id] (swap! lookups inc) {:id id}) 1000))

[(find-user 7) (find-user 7) @lookups] ; => [{:id 7} {:id 7} 1]
```

A cache keeps its entries in an atom, so any fiber can use it. It doesn't
make fibers wait for each other, though: two fibers that miss on the same key
at the same moment both compute the value, and the second result stored
wins. For an expensive computation that must run only once per key, store a
`delay` in the cache and deref it.

## Mutable objects

Everything in this chapter shares values through references. Skein also has
mutable objects, instances of classes declared `(:mutable)` and buffers from
`skein.buffer`, which change in place. They belong to one fiber at a time:
other fibers get an `OwnershipError` until the object is handed over, with
`release!` or by sending it on a channel, or made shared with `share`, after
which `locking` guards changes to it. [Chapter
10](10-protocols-and-extension.md) covers them. Prefer the references in this
chapter for state that several fibers use; use mutable objects for hot, local
data such as pixel buffers.

## Summary

- Values never change, so sharing them is free. Change lives in references:
  atoms, vars, and refs.
- `swap!` updates an atom atomically with a pure function, which may run more
  than once; keep side effects out of it. One atom holding a map is often all
  the shared state a program needs.
- `compare-and-set!` compares by identity. Watches observe changes made
  through `swap!` and `reset!`. Volatiles are atoms in Skein.
- `binding` gives a var a value for a dynamic extent in one fiber and its
  children; use dynamic vars for context, not for sharing.
- `skein.stm` refs change together in `dosync` transactions, which retry on
  conflict. Keep side effects out, or put them in `after-commit`. Use
  `commute` for order-independent updates and `ensure` against write skew.
- `skein.sync` coordinates fibers: semaphores, latches, barriers, rate
  limiters, and `once`. `delay` computes a value once, on first use.
- `memoize` remembers everything; `skein.cache` gives LRU and TTL caches and
  bounded memoization. Concurrent misses can compute a value twice.

---

[Contents](README.md) · Previous: [16. Channels](16-channels.md) · Next: [18. Actors and supervision](18-actors-and-supervision.md) <!-- nav -->
