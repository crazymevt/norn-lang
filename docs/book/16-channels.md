# 16. Channels

[Contents](README.md) · Previous: [15. Fibers and scopes](15-fibers-and-scopes.md) · Next: [17. Shared state](17-shared-state.md) <!-- nav -->

Fibers that only compute and return a value need nothing more than `spawn`
and `await`. Fibers that cooperate while they run, one producing values as
another consumes them, need a way to pass values between them. In Skein that
way is the channel. This chapter shows how to make channels and send values
through them, how closing works, how buffers change who waits for whom, how
to wait on several channels at once and give up after a time limit, and how
to build producer-consumer arrangements and pipelines. It ends with
`skein.async`, a library of ready-made channel connectors.

The core functions here are in `skein.core`, so you can use them without a
`require`. Channels are the CSP model (communicating sequential processes)
that Clojure's core.async also follows; if you know core.async, most of this
chapter will look familiar, and the differences are pointed out as they come.

## A channel between two fibers

`(chan)` makes a channel. `(put! ch x)` sends a value, and `(take! ch)`
receives one:

```clojure
(def greetings (chan))

(with-scope [s]
  (spawn s (put! greetings "hello"))
  (take! greetings)) ; => "hello"
```

The channel made by `(chan)` is unbuffered: it holds no values at all. A
`put!` on it parks the sending fiber until another fiber takes the value,
and a `take!` parks the receiver until another fiber puts one. The two
fibers meet, the value passes from one to the other, and both carry on. This
meeting is called a rendezvous, and it is the reason the example needs a
second fiber. With only one fiber, `put!` would wait forever for a taker
that never comes.

`>!` and `<!` are other names for `put!` and `take!`. In core.async, `>!` and
`<!` work only inside `go` blocks and the `!!` forms block a thread. Skein has
no such split. Every fiber can park, so there is one `put!` and one `take!`,
and they work everywhere.

## Closing a channel

A producer usually has a finite amount to send. When it's finished, it
closes the channel with `close!`, and that is how the consumer learns there
is no more. On a closed channel, `take!` returns `nil` once any values still
in it have been taken, and `put!` returns `false` instead of sending:

```clojure
(def mailbox (chan 2))

(put! mailbox :first) ; => true
(close! mailbox)
(closed? mailbox) ; => true
(take! mailbox) ; => :first
(take! mailbox) ; => nil
(put! mailbox :too-late) ; => false
```

Because `nil` means "closed", you can't send `nil` itself:

```clojure
(put! (chan 1) nil) ; !! RuntimeError
```

If you need to send "nothing", send a keyword such as `:none` or wrap values
in a vector or map.

Here is the full pattern for a producer and a consumer. The producer sends
the squares of 1 to `n` and closes the channel. The consumer takes values
until it sees `nil`:

```clojure
(defn squares [n]
  (let [out (chan)]
    (with-scope [s]
      (spawn s
        (try
          (doseq [i (range 1 (inc n))]
            (put! out (* i i)))
          (finally (close! out))))
      (loop [result []]
        (if-some [value (take! out)]
          (recur (conj result value))
          result)))))

(squares 5) ; => [1 4 9 16 25]
```

Two details are worth copying. The producer closes the channel in a
`finally` clause, so the channel is closed even if producing fails or the
producer is cancelled. And the consumer uses `if-some`, not `if-let`: it
must stop at `nil` but keep going for `false`, which is a value you might
send.

A producer parked in `put!` when its channel is closed wakes up, and its
`put!` returns `false`. That gives a consumer a way to say "stop": it closes
the channel it's reading from, and the producer, seeing `false`, ends.

```clojure
(def numbers (chan))

(def counter
  (spawn-detached
    (loop [i 0]
      (if (put! numbers i)
        (recur (inc i))
        [:stopped-after i]))))

[(take! numbers) (take! numbers) (take! numbers)] ; => [0 1 2]
(close! numbers)
(await counter) ; => [:stopped-after 3]
```

The counter would count forever. After three values, the consumer closes the
channel; the counter's next `put!` returns `false`, and it stops.

## Buffers

`(chan n)` makes a channel with a buffer of `n` values. A `put!` on it
returns at once while there is room in the buffer, and parks only when the
buffer is full. A `take!` returns a buffered value at once.

A buffer lets a producer get ahead of its consumer, but only by `n` values.
Once the buffer is full, the producer waits. This is called back-pressure:
a slow consumer slows its producer down, instead of letting values pile up
without limit. You can watch it happen:

```clojure
(def pipe (chan 2))
(def sent (atom 0))

(def producer
  (spawn-detached
    (dotimes [i 5]
      (put! pipe i)
      (swap! sent inc))))

(sleep 200)
@sent ; => 2
```

The producer has put two values into the buffer and is parked on the third.
Each `take!` makes room for one more:

```clojure
[(take! pipe) (take! pipe) (take! pipe)] ; => [0 1 2]
(sleep 200)
@sent ; => 5
```

Sometimes a producer must never wait, because it is reading a sensor, or
handling user input, or because it would rather lose values than fall
behind. Two buffer policies make puts that never park. With `:dropping`, a
full channel discards the new value. With `:sliding`, it discards the oldest
one to make room:

```clojure
(let [ch (chan 2 :dropping)]
  (doseq [x [1 2 3 4]] (put! ch x))
  [(poll! ch) (poll! ch) (poll! ch)]) ; => [1 2 nil]

(let [ch (chan 2 :sliding)]
  (doseq [x [1 2 3 4]] (put! ch x))
  [(poll! ch) (poll! ch) (poll! ch)]) ; => [3 4 nil]
```

`poll!` is a `take!` that doesn't wait; the next section explains it. Note
that a `put!` on a dropping or sliding channel returns `true` even when the
value is dropped: the put succeeded, the policy decided what to keep. A
dropping or sliding channel needs a buffer of at least one, and `(chan n
:fixed)` is the same as `(chan n)`.

Which buffer should you use? Start with unbuffered channels. They make the
timing of your program easier to see, since every exchange is a meeting. Add
a small fixed buffer when profiling shows fibers waiting for each other on a
busy channel. Use dropping and sliding channels when losing values is an
acceptable answer to overload. Unlike core.async, a channel doesn't take a
transducer; to transform the values on a channel, use
`skein.async/transduce-chan`, at the end of this chapter.

## Without waiting: offer! and poll!

`(offer! ch x)` puts a value only if it can do so immediately, because a
taker is waiting or there is room in the buffer. It returns `true` if it
sent the value and `false` if not. `(poll! ch)` takes a value only if one
is available, and returns `nil` otherwise:

```clojure
(def slot (chan 1))
(offer! slot :a) ; => true
(offer! slot :b) ; => false
(poll! slot) ; => :a
(poll! slot) ; => nil
(offer! (chan) :x) ; => false
```

The last line offers to an unbuffered channel that no fiber is waiting on,
so there is no one to hand the value to. These two are useful at the edges
of a program, where a fiber must check a channel and go on with other work,
and in tests.

## Choosing between channels: alts!

A fiber often needs to wait for whichever of several things happens first: a
result or a timeout, a message or a shutdown signal, a slot on one of several
output channels. `alts!` waits on several channel operations at once and
performs exactly one of them.

`(alts! ops)` takes a vector of operations. A channel by itself means "take
from it", and a vector `[ch v]` means "put `v` on it". When one of the
operations can proceed, `alts!` performs it and returns a vector of two
things: the result and the channel it used. For a take, the result is the
value taken (or `nil` if the channel is closed); for a put, it is `true` or
`false`, as from `put!`:

```clojure
(def fast (chan))
(def never (chan))

(with-scope [s]
  (spawn s (sleep 10) (put! fast :fast))
  (let [[v port] (alts! [never fast])]
    [v (= port fast)])) ; => [:fast true]
```

Nothing ever arrives on `never`, but that doesn't matter: `alts!` waits on
both channels, and after 10 ms the put on `fast` completes the take from
`fast`. Comparing the returned channel with the ones you passed tells you
which operation happened. The other operation is abandoned; `alts!`
never performs more than one.

### Timeouts

`(timeout ms)` returns a channel that closes by itself after `ms`
milliseconds. Taking from it waits until then and returns `nil`. Combined
with `alts!`, it puts a time limit on any channel operation:

```clojure
(defn take-within [ch ms]
  (let [[v port] (alts! [ch (timeout ms)])]
    (if (= port ch) v ::timed-out)))

(def quiet (chan))
(take-within quiet 20) ; => :user/timed-out

(def busy (chan 1))
(put! busy 42)
(take-within busy 20) ; => 42
```

Comparing `port` with `ch` tells you which operation completed. Checking the
value alone wouldn't: a closed `ch` and an expired timeout both give `nil`.

A timeout channel is an ordinary value, so one timeout can bound a whole
loop rather than each step. This collects values from a channel for at most
50 ms, stopping early if the channel closes:

```clojure
(defn collect-for [ch ms]
  (let [deadline (timeout ms)]
    (loop [acc []]
      (let [[v port] (alts! [ch deadline])]
        (if (and (= port ch) (some? v))
          (recur (conj acc v))
          acc)))))

(def readings (chan 10))
(doseq [r [3 1 4]] (put! readings r))
(collect-for readings 50) ; => [3 1 4]
```

The three readings are taken right away. Then the loop waits on both
channels until the deadline closes, 50 ms after `collect-for` started, and
returns what it has.

### Default values and priority

With `:default v`, `alts!` doesn't wait at all. If no operation can proceed
immediately, it returns `[v :default]`:

```clojure
(alts! [(chan) (chan)] :default :nothing-ready) ; => [:nothing-ready :default]
```

When more than one operation is ready, `alts!` picks one at random, so that
no channel is starved because it happens to come first in the vector. With
`:priority true`, it picks the first ready operation in the order given.
Here both channels hold a value, and priority makes the choice predictable:

```clojure
(let [urgent (chan 1) normal (chan 1)]
  (put! normal :routine)
  (put! urgent :alarm)
  (first (alts! [urgent normal] :priority true))) ; => :alarm
```

A put works in `alts!` the same way. This sends a value to whichever of two
workers' inboxes has room first:

```clojure
(let [inbox-a (chan) inbox-b (chan 1)
      [ok port] (alts! [[inbox-a :job] [inbox-b :job]])]
  [ok (= port inbox-b) (poll! inbox-b)]) ; => [true true :job]
```

Nobody is taking from `inbox-a`, but `inbox-b` has a free buffer slot, so the
put to `inbox-b` happens and the other doesn't.

## Producers, consumers, and pipelines

Channels and scopes work well together. The scope owns the fibers, and the
channels connect them. Two arrangements come up again and again.

### A pool of workers

To process many items with a fixed number of fibers, put the items on a
jobs channel and start a few workers that take from it. Each worker puts its
results on a results channel:

```clojure
(defn process-all [f items n-workers]
  (let [jobs (chan)
        results (chan)]
    (with-scope [s]
      ;; feed the jobs, then close the channel
      (spawn s
        (try
          (doseq [x items] (put! jobs x))
          (finally (close! jobs))))
      ;; the workers, in a scope of their own, so that the results
      ;; channel closes when the last of them is done
      (spawn s
        (try
          (with-scope [w]
            (dotimes [_ n-workers]
              (spawn w
                (loop []
                  (when-some [x (take! jobs)]
                    (put! results (f x))
                    (recur))))))
          (finally (close! results))))
      ;; collect
      (loop [acc []]
        (if-some [r (take! results)]
          (recur (conj acc r))
          acc)))))

(sort (process-all (fn [x] (* x x)) (range 10) 3))
; => (0 1 4 9 16 25 36 49 64 81)
```

Each worker loops until the jobs channel is closed and empty. The inner
`with-scope` returns when all three workers have stopped, and only then does
the `finally` close the results channel, which ends the collecting loop. The
results arrive in whatever order the workers finish, so the example sorts
them before showing them.

Notice what the outer scope buys you. If `f` fails on some item, that worker
fails, its scope cancels the other workers and fails, which fails the outer
scope, which cancels the feeder and the collector. `process-all` raises the
error, and no fiber is left parked on a channel that nobody will ever use
again.

### A pipeline of stages

A pipeline passes values through a series of stages, each a fiber that takes
from one channel and puts on the next. A small helper makes a stage:

```clojure
(defn stage [s f in]
  (let [out (chan)]
    (spawn s
      (try
        (loop []
          (when-some [v (take! in)]
            (put! out (f v))
            (recur)))
        (finally (close! out))))
    out))

(defn source [s xs]
  (let [out (chan)]
    (spawn s
      (try
        (doseq [x xs] (put! out x))
        (finally (close! out))))
    out))

(defn drain [ch]
  (loop [acc []]
    (if-some [v (take! ch)]
      (recur (conj acc v))
      acc)))
```

Each stage returns its output channel, so stages chain with `->>`:

```clojure
(with-scope [s]
  (->> (source s ["3" "1" "4"])
       (stage s parse-long)
       (stage s (fn [n] (* n n)))
       drain)) ; => [9 1 16]
```

All three stages run at once. While the last stage squares the first number,
the middle one can already be parsing the second. Closing flows down the
pipeline: the source closes its channel when it runs out, each stage closes
its own output when its input is closed, and `drain` stops at the end.

Failure flows the other way, through the scope:

```clojure
(defn parse-strictly [text]
  (or (parse-long text) (error "not a number:" text)))

(handler-case
  (with-scope [s]
    (->> (source s ["3" "one" "4"])
         (stage s parse-strictly)
         (stage s (fn [n] (* n n)))
         drain))
  (Error [e] (condition-message e))) ; => "not a number: one"
```

### Sharing mutable objects

Most values you send on channels are immutable, so the sender and the
receiver can both use them freely. A mutable object, one made from a class
declared `(:mutable)` or a buffer from `skein.buffer/mutable`, belongs to the
fiber that made it, and other fibers get an `OwnershipError` if they touch
it. Sending it on a channel hands it over: the fiber that takes it becomes
its owner. [Chapter 10](10-protocols-and-extension.md) covers ownership.

## The skein.async library

`skein.async` builds common connections out of the primitives in this
chapter. Load it with an alias; several of its names (`map`, `filter`,
`reduce`, `into`, `merge`) are the same as core functions, so don't refer
them:

```clojure
(require '[skein.async :as a])
```

Every connector in it starts detached fibers that run until their source
channel closes. They aren't in your scopes, so close the sources when you
are done with them; a connector whose source never closes stays parked.

### Collections in and out

`to-chan!` makes a channel that holds a collection's items and is closed
after the last. `onto-chan!` puts a collection's items on a channel you
supply and then closes it (pass `false` as a third argument to leave it open).
Going the other way, `a/into` takes everything from a channel until it closes
and adds it to a collection, and `a/reduce` reduces over a channel's values:

```clojure
(a/into [] (a/to-chan! [1 2 3])) ; => [1 2 3]
(a/into #{} (a/to-chan! [:a :b :a])) ; => #{:a :b}
(a/reduce + 0 (a/to-chan! (range 101))) ; => 5050

(let [ch (chan 10)]
  (await (a/onto-chan! ch [:x :y]))
  (a/into [] ch)) ; => [:x :y]
```

`onto-chan!` returns the fiber doing the putting, so you can wait until it
has finished. `a/into` and `a/reduce` block the calling fiber until the
channel closes.

### Transforming

`a/map` and `a/filter` return a new channel carrying the transformed values,
closed when the source closes. `transduce-chan` runs the values through any
transducer from [Chapter 6](06-sequences-and-transducers.md), including ones
that stop early:

```clojure
(a/into [] (a/map inc (a/to-chan! [1 2 3]))) ; => [2 3 4]
(a/into [] (a/filter odd? (a/to-chan! (range 10)))) ; => [1 3 5 7 9]

(a/into []
  (a/transduce-chan (comp (filter even?) (map (fn [n] (* n n))) (take 3))
                    (a/to-chan! (range 100)))) ; => [0 4 16]
```

`a/map`, `a/filter` and `transduce-chan` accept `:buffer n` to give the
output channel a buffer. `a/pipe` copies everything from one channel to
another and closes the second when the first closes.

### Combining and splitting

`a/merge` takes a collection of channels and returns one channel carrying
all their values, closed once every input has closed. The values interleave
in no particular order. `a/split` does the opposite with a predicate: it
returns two channels, one for the values that pass and one for the rest.

```clojure
(sort (a/into [] (a/merge [(a/to-chan! [1 3]) (a/to-chan! [2 4])])))
; => (1 2 3 4)

(let [[evens odds] (a/split even? (a/to-chan! (range 6)))]
  (with-scope [s]
    (let [e (spawn s (a/into [] evens))
          o (spawn s (a/into [] odds))]
      [(await e) (await o)]))) ; => [[0 2 4] [1 3 5]]
```

The two outputs of `split` are unbuffered, and the splitter puts each value
on one of them in turn, so both must be read at the same time, as here. A
consumer that reads all of `evens` first would wait forever, while the
splitter waits for someone to take an odd number.

### Parallel work in order

`(a/pipeline n to f from)` applies `f` to each value from `from`, several
at a time, and puts the results on `to` in the order the values arrived. It
closes `to` when `from` closes. It is the channel counterpart of `pmap`, for
work that waits:

```clojure
(defn slow-double [x] (sleep (- 30 (* 10 (mod x 3)))) (* 2 x))

(let [out (chan)]
  (a/pipeline 3 out slow-double (a/to-chan! (range 6)))
  (a/into [] out)) ; => [0 2 4 6 8 10]
```

The values take different amounts of time, but the results come out in
input order. At most `n` calls of `f` run at once.

`pipeline` returns a fiber that ends when the pipeline does. If `f` fails,
the results for the values before the failing one still reach `to`; then
the pipeline stops. The calls still running are cancelled, nothing more is
taken from `from`, and `to` is closed, so a consumer sees the results end
early. Await the fiber to find out why: it raises the error `f` failed
with. The fiber is yours to await, so its failure is kept for you, as a
future's is, not printed as a detached fiber's would be.

```clojure
(let [out (chan)
      p (a/pipeline 3 out (fn [x] (/ 10 x)) (a/to-chan! [5 2 0 1]))
      results (a/into [] out)]
  [results (handler-case (await p) (ArithmeticError [e] :divided-by-zero))])
; => [[2 5] :divided-by-zero]
```

When a failure is an expected outcome for some values, handle it inside
`f` and turn it into a value instead, as in
[Chapter 15](15-fibers-and-scopes.md#containing-a-failure).

### One source, many readers

`a/mult` copies every value from a source channel to any number of taps.
`(a/tap m ch)` adds a tap and returns it. A tap is closed when the source
closes (pass `false` as a third argument to keep it open), and `a/untap`
removes one:

```clojure
(let [source (chan)
      m (a/mult source)
      audit (a/tap m (chan 10))
      totals (a/tap m (chan 10))]
  (await (a/onto-chan! source [5 10 20]))
  [(a/into [] audit) (a/reduce + 0 totals)]) ; => [[5 10 20] 35]
```

Add the taps before values start to flow; a value that arrives while there
are no taps is dropped. The mult puts each value on every tap in turn, so a
slow tap holds up all the others. Give taps buffers, or make a tap a
`:dropping` channel if it may fall behind.

`a/pub` routes values by topic instead. `(a/pub ch topic-fn)` computes each
value's topic, and `(a/sub p topic ch)` subscribes a channel to one topic:

```clojure
(let [events (chan)
      p (a/pub events :type)
      clicks (a/sub p :click (chan 10))
      key-presses (a/sub p :key (chan 10))]
  (await (a/onto-chan! events [{:type :click :x 1}
                               {:type :key :key "a"}
                               {:type :scroll}
                               {:type :click :x 2}]))
  [(mapv :x (a/into [] clicks)) (mapv :key (a/into [] key-presses))])
; => [[1 2] ["a"]]
```

The `:scroll` event has no subscribers, so it goes nowhere. `a/unsub`
removes a subscription. `skein doc skein.async` lists the whole library.

## Summary

- `(chan)` makes an unbuffered channel, where each `put!` waits for a
  `take!`. `(chan n)` adds a buffer, which gives back-pressure when full;
  `:dropping` and `:sliding` buffers never make a putter wait.
- `close!` ends a channel. Takes then drain the buffer and return `nil`,
  and puts return `false`, including a put already waiting. You can't put
  `nil`.
- Close channels in a `finally`, and read with `if-some` or `when-some`, so
  that `false` is not mistaken for the end.
- `offer!` and `poll!` try an operation without waiting.
- `alts!` performs the first of several takes and puts that can proceed and
  returns `[value channel]`; it chooses at random among ready operations
  unless `:priority true`, and `:default` makes it not wait.
- `(timeout ms)` is a channel that closes after a delay; with `alts!` it
  bounds a single operation or a whole loop.
- Put the fibers of a worker pool or a pipeline in a scope, so that one
  failure stops all of them and reaches the caller.
- `skein.async` adds collection adapters, `map`, `filter`,
  `transduce-chan`, `merge`, `split`, `pipeline`, `mult` and `pub`. They run
  in detached fibers, so close their sources. If `pipeline`'s `f` fails, it
  closes `to`, and the fiber it returns raises the error.

---

[Contents](README.md) · Previous: [15. Fibers and scopes](15-fibers-and-scopes.md) · Next: [17. Shared state](17-shared-state.md) <!-- nav -->
