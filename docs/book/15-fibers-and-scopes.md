# 15. Fibers and scopes

[Contents](README.md) · Previous: [14. Macros](14-macros.md) · Next: [16. Channels](16-channels.md) <!-- nav -->

Norn runs concurrent work in fibers: lightweight threads that the runtime
schedules for you. This chapter shows how to start fibers, wait for their
results, and keep them organized with scopes, so that no fiber is ever
forgotten and a failure in one is never silently lost. It then covers
cancellation, sleeping and time limits, promises and futures, fibers that
live outside a scope, parallel `pmap` and `preduce`, and what a child fiber
inherits from the code that started it. Channels, the usual way for fibers
to talk to each other, are the subject of [Chapter 16](16-channels.md).

## Fibers

A fiber is a function call that runs on its own. It has its own stack and
runs independently of the code that started it. You start one with `spawn`
and get its result with `await`:

```clojure
(with-scope [s]
  (let [a (spawn s (+ 20 1))
        b (spawn s (* 3 7))]
    (+ (await a) (await b)))) ; => 42
```

Ignore `with-scope` for a moment; the next section is about it. Inside it,
`(spawn s (+ 20 1))` creates a fiber that computes `(+ 20 1)` and returns
immediately with a handle to that fiber. The two fibers run at the same time
as each other and as the code that spawned them. `(await a)` waits until
fiber `a` has finished and returns its value.

A fiber handle is an ordinary value. `fiber?` recognizes one, `done?` asks
whether it has finished, and `deref` (or `@`) is another way to wait for it:

```clojure
(with-scope [s]
  (let [f (spawn s (reduce + (range 1000)))]
    [(fiber? f) @f (done? f)])) ; => [true 499500 true]
```

Fibers are cheap. Each one starts with a small stack that grows as needed,
so you can run tens of thousands of them without thinking about it. This
starts ten thousand and adds up what they return:

```clojure
(with-scope [s]
  (let [fibers (mapv (fn [i] (spawn s (* i i))) (range 10000))]
    (reduce + (map await fibers)))) ; => 333283335000
```

### Direct style

Fibers are scheduled onto a small pool of operating system threads called
carriers, normally one per core. `carrier-count` tells you how many there are,
and the `NORN_THREADS` environment variable overrides the number. Several
fibers run truly in parallel, one on each carrier.

When a fiber waits, for a timer, a channel, a file, or a socket, it parks:
the runtime sets it aside and its carrier runs other fibers until the wait is
over. You write the waiting code in direct style, as ordinary sequential
calls. There is no `async` keyword and no special kind of function that may
wait; any function can call `sleep` or read from a socket, and it parks only
the fiber it runs in.

`(sleep ms)` parks the current fiber for that many milliseconds. Four fibers
that each sleep for 100 ms finish together, about 100 ms after they start,
not 400:

```clojure
(defn elapsed-ms [thunk]
  (let [start (nano-time)]
    (thunk)
    (quot (- (nano-time) start) 1000000)))

(elapsed-ms
  (fn []
    (with-scope [s]
      (dotimes [_ 4] (spawn s (sleep 100))))))
```

`nano-time` reads a monotonic clock in nanoseconds, and `elapsed-ms` turns two
readings into milliseconds. The exact number depends on the machine, so the
book doesn't show it, but it is a little over 100. As a check:

```clojure
(< (elapsed-ms (fn [] (with-scope [s] (dotimes [_ 4] (spawn s (sleep 100))))))
   300) ; => true
```

Scheduling is also preemptive. A fiber that computes for a long time without
waiting is interrupted at safepoints (function entries and loop back-edges)
so that other fibers get their turn. A runaway loop can't starve the rest of
the program, and, as you'll see under cancellation, it can still be stopped.
`(yield)` gives up the carrier voluntarily; you rarely need it.

## Scopes

Every fiber you have seen so far was started inside `with-scope`. That is
the normal way to start fibers in Norn, and the idea behind it is called
structured concurrency: concurrent work has a shape that matches the shape of
the code.

`(with-scope [s] body…)` creates a scope, binds it to `s`, and runs the body.
`spawn` puts each new fiber inside the scope you give it. The important rule
is that the `with-scope` form does not return until every fiber in its scope
has finished. The body's value is the value of the whole form:

```clojure
(def log (atom []))

(with-scope [s]
  (spawn s (sleep 30) (swap! log conj :slow))
  (spawn s (swap! log conj :fast))
  :body-done) ; => :body-done

@log ; => [:fast :slow]
```

The body finished long before the slow fiber, but `with-scope` waited for
it, so by the time you look at `log` both fibers have run. (`atom` makes a
cell that fibers can share, and `swap!` updates it; [Chapter
17](17-shared-state.md) covers them.)

This rule is what makes concurrent code manageable. When a function
that uses `with-scope` returns, all the work it started is over. Nothing it
spawned is still running in the background, still holding a file, or still
about to fail where nobody is looking. You can read a function that uses
fibers the way you read one that doesn't.

Here is a function that loads several records at once. `load-record`
stands in for something slow, like a database query; the sleep makes the
later ids finish first:

```clojure
(defn load-record [id]
  (sleep (* 10 (- 4 id)))
  {:id id :name (str "record " id)})

(defn load-all [ids]
  (with-scope [s]
    (mapv await (mapv (fn [id] (spawn s (load-record id))) ids))))

(load-all [1 2 3])
; => [{:id 1 :name "record 1"} {:id 2 :name "record 2"} {:id 3 :name "record 3"}]
```

The first `mapv` spawns one fiber per id and collects the handles in a
vector. The second awaits them in that order, so the results come back in
the order of the ids, whatever order the fibers finished in. This pattern,
spawn everything and then await everything, covers a large share of
everyday concurrency.

### Passing a scope around

A scope is a value. You can pass it to a helper, and any fiber can spawn
into a scope it can see, including a fiber that is itself in the scope. The
scope waits for all of them:

```clojure
(defn start-pair [s x]
  [(spawn s (inc x)) (spawn s (dec x))])

(with-scope [s]
  (let [[a b] (start-pair s 10)
        c (spawn s (await (spawn s (* 2 (await a)))))]
    [(await a) (await b) (await c)])) ; => [11 9 22]
```

Scopes nest, too. A function that uses `with-scope` internally can be
called from a fiber in another scope; its scope finishes inside the outer
fiber's lifetime, so the tree of fibers always matches the call tree.

One detail: the body of `with-scope` itself runs in a fiber of the new
scope, not in the fiber that entered `with-scope`. Usually you can't tell the
difference, because bindings are carried over (see the last section), but
`current-fiber` shows it:

```clojure
(= (current-fiber) (with-scope [s] (current-fiber))) ; => false
```

## When a fiber fails

A fiber that raises an error it doesn't handle ends with that error. If
you `await` it, the error is raised again in the waiting fiber, with its
original class:

```clojure
(with-scope [s]
  (let [f (spawn s (/ 1 0))]
    (await f))) ; !! ArithmeticError
```

Inside a scope, a failure has a wider effect. When a fiber in a scope
fails and nothing handles the error, the scope cancels all of its other
fibers and `with-scope` raises that failure. Errors travel up the tree to the code that started the work,
and cancellation travels down to everything else that work started:

```clojure
(def finished (atom []))

(handler-case
  (with-scope [s]
    (spawn s (sleep 5000) (swap! finished conj :slow))
    (spawn s (sleep 10) (error "lost the connection"))
    :unreachable)
  (Error [e] (condition-message e))) ; => "lost the connection"

@finished ; => []
```

The slow fiber would have run for five seconds. Instead, the failure of its
sibling cancels it after 10 ms, `with-scope` raises the error, and the
`handler-case` (from [Chapter 11](11-conditions-and-restarts.md)) catches it.
The slow fiber never got to update `finished`. An error in the body of
`with-scope` has the same effect: the scope cancels its fibers and raises the
error.

This is the right default because a failed part usually means a failed
whole. If `load-all` can't load one of the records, the caller should hear
about it, and the other queries shouldn't keep a database busy for a result
nobody will use.

### Containing a failure

Sometimes a failure is an expected outcome, and you want the others to carry
on. Handle it inside the fiber, where it happens, and return a value that
says what went wrong:

```clojure
(defn lookup-record [id]
  (when (= id 2) (error "no record" id))
  {:id id})

(defn try-load-all [ids]
  (with-scope [s]
    (mapv await
          (mapv (fn [id]
                  (spawn s (handler-case {:ok (lookup-record id)}
                             (Error [e] {:failed id :why (condition-message e)}))))
                ids))))

(try-load-all [1 2 3])
; => [{:ok {:id 1}} {:failed 2 :why "no record 2"} {:ok {:id 3}}]
```

Each fiber returns a map, either `{:ok …}` or `{:failed …}`, and none of
them fails, so the scope succeeds.

The handler can also go where the fiber is awaited. An error that `await`
raises in the scope's body belongs to that code, and if it handles the
error, the scope doesn't raise it again:

```clojure
(with-scope [s]
  (let [f (spawn s (/ 1 0))]
    (handler-case (await f)
      (ArithmeticError [e] :no-ratio)))) ; => :no-ratio
```

The same goes for `settle`, which waits for a fiber without raising and
returns `{:value v}` or `{:error message}`. Once `settle` has handed you the
error, it's yours:

```clojure
(with-scope [s]
  (let [f (spawn s (error "boom"))]
    [(settle f) :carried-on])) ; => [{:error "boom"} :carried-on]
```

What fails the scope is a failure nobody waits for. While the body is
still running, the scope holds such a failure back, since the body may yet
await or settle that fiber. As soon as the body waits for anything else, or
ends, the scope cancels the rest and raises it:

```clojure
(handler-case
  (with-scope [s]
    (spawn s (error "boom"))
    :carried-on)
  (Error [e] [:scope-raised (condition-message e)])) ; => [:scope-raised "boom"]
```

`settle`'s `:error` is the message as a string, not the condition object.

## Cancellation

`(cancel! f)` asks fiber `f` to stop. If the fiber is parked, it stops right
away; if it is computing, it stops at its next safepoint. Either way it
unwinds its stack the way an error would, so its `finally` clauses and
`unwind-protect` cleanups run:

```clojure
(def events (atom []))

(def worker
  (spawn-detached
    (try
      (sleep 10000)
      (swap! events conj :woke-up)
      (finally (swap! events conj :cleaned-up)))))

(sleep 10)
(cancel! worker)
(settle worker) ; => {:error "Cancelled"}
@events ; => [:cleaned-up]
```

`spawn-detached` starts a fiber that belongs to no scope; it gets its own
section below. Here it lets the top level cancel the worker directly. The
worker was asleep, so the cancellation woke it, it ran its `finally` clause,
and it ended. Awaiting a cancelled fiber raises an error whose message is
`"Cancelled"`:

```clojure
(handler-case (await worker)
  (Error [e] (condition-message e))) ; => "Cancelled"
```

Cancellation is not a condition that handlers can see. A `handler-case` or
`catch` inside the cancelled fiber doesn't stop it; only cleanups run. That
way a fiber can't refuse to be cancelled by accident, for instance with a
`(catch Error e …)` meant for something else.

A fiber stuck in a computation is cancelled too, because preemption gives
the runtime a chance to stop it at a safepoint:

```clojure
(def spinner (spawn-detached (loop [n 0] (recur (inc n)))))
(sleep 5)
(cancel! spinner)
(settle spinner) ; => {:error "Cancelled"}
```

Cancellation also follows the tree. Cancelling a fiber that is waiting in
`with-scope` cancels every fiber in that scope, and every fiber in their
scopes, all the way down:

```clojure
(def inner (atom nil))

(def outer
  (spawn-detached
    (with-scope [s]
      (reset! inner (spawn s (sleep 10000)))
      (sleep 10000))))

(sleep 10)
(cancel! outer)
[(settle outer) (settle @inner)] ; => [{:error "Cancelled"} {:error "Cancelled"}]
```

Inside a scope, cancelling one fiber stops just that fiber. A fiber stopped
with `cancel!` ends quietly: the scope doesn't count it as a failure, and
the other fibers carry on:

```clojure
(with-scope [s]
  (let [slow (spawn s (sleep 10000) :slow)
        quick (spawn s (sleep 10) :quick)]
    (cancel! slow)
    [(settle slow) (await quick)])) ; => [{:error "Cancelled"} :quick]
```

That lets a scope's body start several fibers, take the first answer, and
cancel the rest. Awaiting a cancelled fiber still raises `Cancelled`, as
shown above, and cancelling the fiber waiting in `with-scope` still stops
everything in the scope.

## Sleeping and time limits

You have already used `sleep`. Because it parks only the fiber that calls
it, sleeping is cheap: a thousand fibers asleep use no carrier time at all.

Norn has no special form for "give up after so many milliseconds". You
build time limits from the pieces in this chapter and the next. The most
direct way is a future, which the next section introduces properly: `deref`
on a future accepts a time limit and a value to return when time runs out.

```clojure
(defn with-time-limit [ms thunk]
  (let [work (future (thunk))
        result (deref work ms ::timed-out)]
    (when (= result ::timed-out)
      (future-cancel work))
    result))

(with-time-limit 1000 (fn [] (+ 1 2))) ; => 3
(with-time-limit 20 (fn [] (sleep 10000) :finished)) ; => :user/timed-out
```

`::timed-out` is a keyword qualified with the current namespace, `user`, so
it can't be confused with a value the work might return. When the time
runs out, `with-time-limit` cancels the work, so nothing is left running.

The other common way uses a `timeout` channel with `alts!`, which waits for
whichever of several things happens first. [Chapter 16](16-channels.md)
covers it.

## Promises and futures

A fiber's result is available only to code that holds its handle. Two small
types cover the other cases where a value arrives later.

### Promises

A promise is a box that is filled once. `(promise)` makes an empty one,
`(deliver p x)` fills it, and `deref` waits until it is filled. Any number
of fibers can wait on the same promise, and all of them wake when it's
delivered:

```clojure
(def ready (promise))

(with-scope [s]
  (let [waiters (mapv (fn [i] (spawn s (+ i @ready))) [1 2 3])]
    (sleep 10)
    (deliver ready 100)
    (mapv await waiters))) ; => [101 102 103]
```

Only the first `deliver` counts. It returns the promise, and later ones
return `nil` and change nothing. `realized?` asks whether a value has
arrived, and `deref` with a time limit gives up after that many milliseconds:

```clojure
(realized? ready) ; => true
(deliver ready 200) ; => nil
@ready ; => 100

(deref (promise) 10 :nothing-yet) ; => :nothing-yet
```

Promises are handy when the fiber that produces a value isn't the one that
started the work, or when one value unblocks many fibers at once. You'll see
them again in [Chapter 18](18-actors-and-supervision.md), where `ask` uses a
promise to carry an actor's reply.

### Futures

`(future body…)` runs the body in a fiber of its own and returns a future.
`deref` waits for the body's value, and raises the same condition if the
body failed:

```clojure
(def answer (future (sleep 10) (* 6 7)))
(future-done? answer) ; => false
@answer ; => 42
(future-done? answer) ; => true

(def broken (future (/ 1 0)))
@broken ; !! ArithmeticError
```

A future is not in any scope you can see; it sits in a scope of its own
that nobody joins. Its failure is reported only when you deref it, and
cancelling it with `future-cancel` affects nothing else. That is what made
it suitable for `with-time-limit`. Unlike a fiber, a future accepts a time
limit on `deref`, as shown above. `future-cancelled?` tells you whether a
future was cancelled.

Prefer `with-scope` and `spawn` when you have a group of fibers whose work
belongs together. Reach for a future when you want one value computed on the
side, possibly abandoned.

## Detached fibers

`spawn-detached` starts a fiber that belongs to no scope. Nothing waits for
it, and its failure cancels nothing. You've used it above to get a fiber
the top level could cancel directly. Like any fiber, a detached one can be
awaited or settled:

```clojure
(def side-job (spawn-detached (error "bad input")))
(settle side-job) ; => {:error "bad input"}
```

When a detached fiber fails with nobody waiting for it, the runtime
prints a line such as `Fiber 57 failed: bad input` on standard error, so
the failure isn't lost entirely. (A future's fiber, and the one
`norn.async/pipeline` returns, keep their failure for whoever awaits them
instead.)
And when your program's main code finishes, the process exits without
waiting for detached fibers that are still running.

Use detached fibers sparingly. They give up the guarantees that make scopes
pleasant: nothing ensures they finish, nobody hears about their failures
unless someone settles them, and they can hold resources indefinitely. A
fiber that should outlive the code that started it, such as a long-running
service, belongs under a supervisor, which watches it and restarts it when
it fails. [Chapter 18](18-actors-and-supervision.md) shows how.

## Parallel map and reduce

Spawning one fiber per item is right when each item waits on something,
such as a network request. For pure computation over a large collection,
`pmap` and `preduce` split the work into chunks, a few per carrier, and
process the chunks in parallel.

`(pmap f coll)` is a parallel `map`. It always returns a vector, in the
order of the input:

```clojure
(pmap (fn [n] (* n n)) (range 10)) ; => [0 1 4 9 16 25 36 49 64 81]
```

`(preduce f init coll)` reduces each chunk in parallel and then combines the
chunk results with `f`. Each chunk starts from `init`, so `f` must be
associative and `init` must be its identity: `+` with `0`, `*` with `1`,
`max` with a value smaller than any item, `merge-with +` with `{}`:

```clojure
(preduce + 0 (range 1000001)) ; => 500000500000
```

Both requirements matter, because the chunk results are combined with the
same `f`. To count words in parallel, make each word into a small map, and
let `f` merge two maps:

```clojure
(defn count-words [words]
  (preduce (fn [a b] (merge-with + a b))
           {}
           (map (fn [w] {w 1}) words)))

(count-words ["to" "be" "or" "not" "to" "be"])
; => {"to" 2 "be" 2 "or" 1 "not" 1}
```

A reducing function such as `(fn [m word] (merge-with + m {word 1}))`, which
takes a map and a word, works with `reduce` but not with `preduce`: when the
chunks are combined, it would be handed a map where it expects a word.

`pmap` and `preduce` are built on `with-scope`, so they behave like the
code you've written in this chapter: if `f` fails on any item, the other
chunks are cancelled and the error is raised by `pmap` itself:

```clojure
(pmap (fn [n] (/ 100 n)) [1 2 0 4]) ; !! ArithmeticError
```

Parallelism has a cost: starting fibers, splitting the input into a vector,
and putting the results back together. For a cheap `f` over a small
collection, plain `map` is faster. Measure before you switch;
[Chapter 26](26-tools-and-performance.md) shows how.

## What a child inherits

A fiber starts with a copy of the dynamic environment of the code that
spawned it. Norn calls this conveyance. Dynamic vars rebound with `binding`
keep their values in child fibers:

```clojure
(def ^:dynamic *request-id* nil)

(defn log-line [msg] (str "[" *request-id* "] " msg))

(binding [*request-id* "r-17"]
  (with-scope [s]
    (await (spawn s (log-line "loaded the cart"))))) ; => "[r-17] loaded the cart"
```

The copy is taken when the fiber is spawned. Rebinding the var afterwards,
in the parent, doesn't change what an already running child sees.
[Chapter 17](17-shared-state.md) has more on dynamic vars.

Conveyance matters most for the condition system. Handlers established with
`handler-bind` are dynamic bindings, so they're conveyed too: a handler set
up around a scope runs, in the child fiber, when the child signals. That
means a handler in the parent can choose a restart that the child offers.
Here the parser offers a `use-value` restart, and one handler, outside the
scope, decides for every fiber that bad input counts as zero:

```clojure
(defn parse-count [s]
  (restart-case
    (if (re-matches #"\d+" s)
      (parse-long s)
      (error "not a count:" s))
    (use-value [v] v)))

(handler-bind [Error (fn [c] (invoke-restart 'use-value 0))]
  (with-scope [s]
    (mapv await (mapv (fn [x] (spawn s (parse-count x))) ["4" "many" "2"]))))
; => [4 0 2]
```

The fiber parsing `"many"` signalled an error; the conveyed handler ran in
that fiber, invoked the restart there, and the fiber went on to return `0`.
No fiber failed, so the scope succeeded.

`handler-case` handlers are not conveyed. A `handler-case` works by
unwinding the stack to the point where it was established, and that point
is on another fiber's stack, which a child can't unwind. A child's error
reaches a parent's `handler-case` only the way you saw earlier: the child
fails, and the scope or `await` raises the error again in the parent.
[Chapter 11](11-conditions-and-restarts.md) explains the difference between
the two kinds of handler.

Two more things are conveyed. A child starts in its parent's current
namespace. And it inherits the debugger mode: at the REPL and in an editor,
a fiber whose error nobody handles parks with its stack intact, waiting for
you to inspect it, fix the code, and pick a restart, and fibers spawned from
there do the same. Programs run with `norn file.norn` don't park; their
failing fibers end with the error, as in every example here. [Chapter
12](12-live-programming.md) shows the debugger at work on parked fibers.

## Summary

- A fiber runs a piece of code on its own. `spawn` starts one in a scope,
  `await` (or `@`) waits for its value, and waiting parks only that fiber.
  There is no `async` coloring.
- `with-scope` returns only when every fiber in its scope has finished, so a
  function that uses it leaves nothing running behind it.
- When a fiber in a scope fails and nobody handles the error, the scope
  cancels the others and raises the failure. Contain an expected failure by
  handling it inside the fiber, or where you `await` or `settle` it.
- `cancel!` stops a fiber at its next wait or safepoint, running its
  cleanups; handlers can't intercept it. Cancelling one fiber of a scope
  stops only that fiber; cancelling a parent cancels its whole tree.
- Build time limits from a future and `deref` with a timeout, or from
  `alts!` and `timeout` channels.
- Promises carry one value to any number of waiting fibers; futures compute
  a value on the side, report failure only when dereferenced, and can be
  abandoned.
- `spawn-detached` gives up the scope's guarantees; use it rarely, and put
  long-lived fibers under a supervisor.
- `pmap` and `preduce` parallelize pure work over collections. Children
  inherit bindings, `handler-bind` handlers, the namespace, and the debugger
  mode, but not `handler-case` handlers.

---

[Contents](README.md) · Previous: [14. Macros](14-macros.md) · Next: [16. Channels](16-channels.md) <!-- nav -->
