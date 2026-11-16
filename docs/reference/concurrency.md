# Concurrency and state

[Reference contents](README.md) · [Symbol index](symbols.md)

## Fibers and scopes

| Form | Behavior |
| --- | --- |
| `(with-scope [s] body…)` | Run the body in a new scope and wait for its children. Return the body's value when the scope succeeds. An error nobody handled cancels the other fibers in the scope and propagates to the caller. The binding must be a vector of one name. |
| `(spawn s body…)` | Schedule a fiber inside scope `s` and return its handle. |
| `(spawn-detached body…)` | Schedule a fiber outside a scope and return its handle. |
| `(await fiber)` | Wait for the fiber's result or signal its failure. |
| `(settle fiber)` | Wait and return `{:value v}` or `{:error message}`. |
| `(cancel! fiber)` | Request cancellation of a fiber. A fiber cancelled before it starts never runs its body. |
| `(sleep ms)`, `(yield)` | Park a fiber for a duration in milliseconds, or yield execution. |

```clojure
(with-scope [s]
  (let [a (spawn s (+ 20 1))
        b (spawn s (* 3 7))]
    (+ (await a) (await b)))) ; => 42
```

A scope fails only for an error nobody handled. A fiber stopped with
`cancel!` ends quietly, so the body can cancel one fiber (the losers of a
race, say) and carry on. An error that `await` or `settle` has received
isn't raised again by the scope: the code that awaited it has it. While the
body is running, a failure nobody has waited for yet is held back, so the
body can still await or settle that fiber; the scope fails with it as soon
as the body waits for anything else, or ends. Cancelling the fiber waiting
in `with-scope` cancels the whole scope.

```clojure
(with-scope [s] (cancel! (spawn s (sleep 1000))) :ok) ; => :ok
(with-scope [s] (settle (spawn s (error "x"))) :ok) ; => :ok
(with-scope [s] (spawn s (error "x")) :ok) ; raises x
```

`(pmap f coll)` runs the mapping in parallel and returns a vector in
input order. `(preduce f init coll)` reduces chunks in parallel. Supply
an associative operation and its identity as `init`.

## Channels

| Function | Behavior |
| --- | --- |
| `(chan)`, `(chan n)` | Make an unbuffered channel or a channel with capacity `n` (0 or more; a negative size is a `TypeError`). |
| `(chan n :dropping)`, `(chan n :sliding)` | Use a full buffer policy that drops the incoming item or the oldest item, respectively. |
| `(put! ch x)`, `(>! ch x)` | Suspend until the item can be sent. Return `true` on success and `false` when closed. Sending `nil` signals an error. |
| `(take! ch)`, `(<! ch)` | Suspend until an item is available or the channel closes. Return `nil` after a closed channel's buffer is drained. |
| `(offer! ch x)`, `(poll! ch)` | Attempt a send or receive without waiting. |
| `(close! ch)`, `(closed? ch)` | Close a channel or test whether it is closed. |
| `(timeout ms)` | Return a channel that closes after the given delay. |
| `(alts! ops options…)` | Try several operations and return `[result channel]` for the one selected. |

In `alts!`, an operation is a channel to take from or `[channel value]`
to put. With `:default value`, an immediate miss returns
`[value :default]`. With `:priority true`, ready operations are checked
in the order given.

```clojure
(let [ch (chan 1)]
  (put! ch :ready)
  (close! ch)
  [(first (alts! [ch])) (take! ch) (put! ch :late)]) ; => [:ready nil false]
```

See the [producer and consumer](../examples/channels.norn) for communication
between fibers. `norn.async` provides channel combinators and `norn.actor`
provides mailbox-based processes and supervision.

## Atoms and dynamic vars

| Form | Behavior |
| --- | --- |
| `(atom x)` | Create a cell that can be shared across fibers. |
| `(deref a)`, `@a` | Read the current value of an atom. `deref` also works on vars and other dereferenceable values. |
| `(reset! a x)` | Store and return `x`. |
| `(swap! a f args…)` | Replace the value with the result of applying `f` to the old value and any extra arguments. Return the new value. |
| `(compare-and-set! a old new)` | Update if the current value is identical to `old`, returning whether the update happened. |
| `(binding [var-name expr …] body…)` | Give a var a temporary value within a dynamic extent. Spawned fibers inherit those bindings. |

An atom update function may run again under contention. Keep side effects
outside the function.

```clojure
(let [counter (atom 0)]
  (with-scope [s]
    (dotimes [_ 4]
      (spawn s (swap! counter inc))))
  @counter) ; => 4
```

## Transactions over refs

Use [`norn.stm`](libraries.md#concurrency) when several cells must change
together. `ref` creates a cell, `dosync` runs a transaction, and
`alter` or `ref-set` records a change to be committed. A transaction may
retry after a conflict. Put side effects in an `after-commit` callback
when they must run after a successful commit.

```clojure
(require '[norn.stm :as stm])

(let [from (stm/ref 20)
      to (stm/ref 0)]
  (stm/dosync
    (stm/alter from - 7)
    (stm/alter to + 7))
  [@from @to]) ; => [13 7]
```
