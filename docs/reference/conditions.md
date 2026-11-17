# Conditions and restarts

[Reference contents](README.md) · [Symbol index](symbols.md)

A condition is an object that describes an event. Handlers choose by class
and can inspect slots such as `:message`, `:location`, and class-specific
details. A restart is a recovery action offered by the code that knows
how to continue.

## Signaling conditions

| Function | Behavior |
| --- | --- |
| `(signal condition)` | Offer a condition to matching dynamic handlers, innermost first. Return `nil` if they all decline by returning normally. |
| `(error x args…)` | Signal an existing condition, construct one from a condition class and initargs, or create a `SimpleError` from a message. An unhandled error enters the debugger or ends the fiber, according to the current debugger setting. |
| `(throw condition)` | Signal through `error`. |
| `(warn x args…)` | Signal a condition, constructing a `Warning` for message arguments, then print a warning if control returns. While it signals, a `muffle-warning` restart is active: invoking it skips the printing, and `warn` returns `nil`. |
| `(muffle-warning)`, `(muffle-warning c)` | From a handler for a warning, invoke the `muffle-warning` restart so that `warn` doesn't print it. Signal an error when no such restart is active. |
| `(condition-message c)`, `(condition-text c)` | Read the message or format it with the source location when available. |
| `(ex-info msg data)`, `(ex-info msg data cause)` | Construct an `ExInfo` condition containing a data map and optional cause. `ex-data`, `ex-message`, and `ex-cause` retrieve details. |

`Condition` is the base class. `SeriousCondition` extends `Condition`,
`Error` extends `SeriousCondition`, and `RuntimeError` extends `Error`.
Built-in conditions include `TypeError`, `ArityError`,
`ArithmeticError`, `IOError`, `ParseError`, and `TimeoutError`.

## Handling and cleanup

| Macro | Behavior |
| --- | --- |
| `(handler-bind [Class handler …] body…)` | Call matching handlers with the condition while the signaling stack is active. A handler can invoke a restart or return normally to let outer handlers run. |
| `(handler-case form (Class [e] body…) …)` | Unwind to the first matching clause and return its body value. With no signal, return the value of `form`. Use `[]` if the clause does not need a binding. |
| `(try body… (catch Class e body…) … (finally cleanup…))` | Handle conditions with unwinding catches and optional cleanup. |
| `(unwind-protect form cleanup…)` | Evaluate the form and run cleanup on normal return or unwinding, including restart transfers and cancellation. |

`handler-bind` handlers are conveyed to spawned fibers. An unwinding
catch established with `handler-case` or `try` applies only in the
fiber where it was established.

```clojure
(try
  (throw (ex-info "Missing quantity" {:field :quantity}))
  (catch ExInfo e (:field (ex-data e)))) ; => :quantity
```

## Restarts and recovery

**Macro** · `(restart-case form (name [params…] body…) …)` evaluates
`form` with named restarts active. A clause can begin with
`:report "description"` after its parameter vector. Invoking that restart
unwinds to the clause and calls it with the supplied arguments. Its result
becomes the value of `restart-case`.

| Function | Behavior |
| --- | --- |
| `(compute-restarts)` | Return a vector of the currently visible restarts. |
| `(find-restart 'name)` | Find the nearest visible restart by name, or return `nil`. |
| `(invoke-restart restart-or-name args…)` | Transfer control to a visible restart, using its object or its quoted name. A missing named restart signals an error. |

Primitive failures can offer `use-value` to substitute for a failed
operation and `retry` to run it again. A handler can use `find-restart`
to check availability before choosing a recovery.

```clojure
(handler-bind [ArithmeticError (fn [_] (invoke-restart 'use-value 41))]
  (+ 1 (/ 1 0))) ; => 42

(handler-bind [Warning (fn [_] (muffle-warning))]
  (warn "Disk almost full")) ; => nil, and nothing is printed
```

See the [reading recovery example](../examples/restarts.skn) for a
program that defines its own restarts.

## Debugging a parked fiber

At the REPL or in an editor session, an unhandled error can park a fiber
with its stack intact. Use `(debugging)` to list parked fibers and
`(backtrace id)` to look at one. `(eval-in-frame id index 'expr)` evaluates
a form with the frame's locals. `debug-restart` invokes an offered
restart, `debug-restart-frame` retries a selected frame, and `debug-abort`
ends the parked fiber. A call the compiler inlined has a frame there too,
marked `:inlined true`, with its locals; restarting it calls the
function's newest definition, and the frame below goes on from that call.
