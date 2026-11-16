# 18. Actors and supervision

[Contents](README.md) · Previous: [17. Shared state](17-shared-state.md) · Next: [19. Files and streams](19-files-and-streams.md) <!-- nav -->

Scopes suit work that starts, finishes, and returns a result. Some parts of
a program are not like that. A connection pool, a session store, or a job
queue runs for as long as the program does, keeps state of its own, and
serves requests from many other fibers. When such a part fails, you want it
to start again in a known good state, without taking the rest of the program
down. `norn.actor` provides this, in the style of Erlang: processes that own
their state and communicate only by messages, links and monitors that let
processes react to each other's failures, and supervisors that restart what
crashed. This chapter builds a small key-value store with it, step by step,
and ends by putting the store under supervision.

A process here is a Norn process, a fiber with a mailbox. It has nothing to
do with the operating system's processes, which `norn.process` runs
([Chapter 21](21-the-operating-system.md)).

```clojure
(require '[norn.actor :as a])
```

## A first process

A process is a fiber that runs a function and has a mailbox. Other fibers
put messages in the mailbox with `send`, and the process takes them out with
`receive`. A process's state lives in the arguments of its function, and it
moves to a new state by looping with `recur`.

`defprocess` defines such a function. Here is the store:

```clojure
(a/defprocess store [data]
  (a/receive
    [:put k v] (recur (assoc data k v))
    [:delete k] (recur (dissoc data k))
    [:get k reply] (do (deliver reply (get data k))
                       (recur data))
    [:stop] :stopped))
```

`defprocess` is `defn` with a `loop` around the body, so `recur` runs the body
again with new values for the parameters. `receive` waits for a message and
compares it with each pattern in turn, as `match` does ([Chapter
4](04-functions-and-control-flow.md)). The first clause that matches binds its
names, here `k` and `v`, and runs its body. A `[:put k v]` message makes a
new map and loops with it. A `[:stop]` message returns `:stopped` instead of
looping, which ends the process.

`spawn-process` starts a process. It takes the function and its arguments,
and returns the process:

```clojure
(def db (a/spawn-process store {}))

(a/process? db) ; => true
(a/alive? db) ; => true
```

`send` puts a message in a process's mailbox and returns the message. It
never waits; the message is handled when the process gets to it:

```clojure
(a/send db [:put :color "blue"]) ; => [:put :color "blue"]
(a/send db [:put :size 3])
(a/send db [:delete :size])
```

Messages are ordinary values, usually vectors that start with a keyword
naming the request. Because values are immutable, a message can't change
after it is sent, and sender and receiver never share anything that could
change under them.

### Asking for an answer

`send` is one way. To get an answer, a process needs somewhere to put it.
The `[:get k reply]` clause expects a promise ([Chapter
15](15-fibers-and-scopes.md#promises)) as the last element of the message,
and delivers the value to it. `ask` does the rest of the work: it adds a
fresh promise to the end of the message you give it, sends it, and waits for
the delivery:

```clojure
(a/ask db [:get :color]) ; => "blue"
(a/ask db [:get :size]) ; => nil
```

A process handles its messages one at a time, in the order they arrived from
each sender. The `:get` was sent after the `:put` and `:delete`, so its
answer reflects them.

`ask` works from anywhere: from the top level, from a fiber, or from another
process. It waits at most 5000 milliseconds by default, or as long as its
third argument says. A process that doesn't answer in time raises a
`TimeoutError`, whose `:after` slot holds the limit:

```clojure
(a/ask db [:no-such-request] 20) ; !! TimeoutError

(handler-case (a/ask db [:no-such-request] 20)
  (TimeoutError [e] (:after e))) ; => 20
```

Why did the store not answer? No clause of its `receive` matches
`[:no-such-request reply]`, so the message is still in its mailbox, unread.
The next section explains why that is useful.

## Receiving selectively

`receive` doesn't necessarily take the next message. It takes the first message in
the mailbox that some clause matches, and leaves the others where they are,
in order, for a later `receive`. This is selective receive, and it lets a
process wait for one particular message while others queue up.

`run-in-process` is handy for trying this out. It runs a function as a
process of its own, waits for it, and returns its value. `self` is the
process running the current code:

```clojure
(a/run-in-process
  (fn []
    (a/send (a/self) [:b 2])
    (a/send (a/self) [:c 3])
    (a/send (a/self) [:a 1])
    [(a/receive [:a x] x)
     (a/receive [:b y] y)
     (a/receive other other)])) ; => [1 2 [:c 3]]
```

The process sent itself three messages. The first `receive` wanted an `:a`
message and skipped the other two to find it. The second found `[:b 2]`. The
third had a pattern, a bare name, that matches anything, so it took the
oldest remaining message.

A clause can have a guard: `:when` and an expression after the pattern,
which must also be true for the clause to match. And `:after ms` at the end
of a `receive` gives up waiting after that many milliseconds, running the
expression that follows:

```clojure
(a/run-in-process
  (fn []
    (a/send (a/self) [:temperature 31])
    (a/receive
      [:temperature t] :when (> t 30) [:hot t]
      [:temperature t] [:fine t]))) ; => [:hot 31]

(a/run-in-process
  (fn []
    (a/receive
      [:reply value] value
      :after 20 :no-reply))) ; => :no-reply
```

Patterns can be maps too, and every other pattern that `match` accepts.

Selective receive has a price. Messages that no clause ever matches stay in
the mailbox forever, and each `receive` looks past them again. A long-lived
process should usually end its `receive` with a clause that takes anything,
and log or ignore what it didn't expect. Our store doesn't, which is why the
unanswered request is still sitting in its mailbox.

## How processes end

A process ends when its function returns, when it fails, or when it is told
to exit. Each end has a reason, which `exit-reason` reports (it's `nil` while
the process runs). `await-exit` waits for the process to end and returns the
reason; with a time limit, it returns `:timeout` if the process is still
running then:

```clojure
(a/send db [:stop])
(a/await-exit db) ; => :normal
(a/alive? db) ; => false
```

Returning normally gives the reason `:normal`. A message sent to a process
that has ended is dropped without an error, since a sender can't know in
general whether the receiver is still there.

When a process fails, its reason is the condition it failed with, the very
object:

```clojure
(def fragile (a/spawn-process (fn [] (a/receive [:divide n] (quot 100 n)))))
(a/send fragile [:divide 0])
(class-name (class-of (a/await-exit fragile))) ; => norn.core/ArithmeticError
```

The runtime also prints the failure on standard error, as for any fiber that
fails with nobody waiting for it.

`(exit! p)` ends a process from outside, with the reason `:killed`, or with
the reason you give as a second argument:

```clojure
(def sleeper (a/spawn-process (fn [] (a/receive :never-sent :ok))))
(a/exit! sleeper)
(a/exit-reason sleeper) ; => :killed
```

### Processes and fibers

A process is a fiber, started outside any scope, like `spawn-detached`. That
is deliberate: a process lives as long as it needs to, independent of the
code that started it, and the tools in the rest of this chapter take the
place of the scope's guarantees.

Only the process's own fiber is the process. A fiber that a process spawns
is not part of it, and `self` and `receive` don't work there. That includes
the body of `with-scope`, which runs in a fiber of its own:

```clojure
(a/run-in-process (fn [] (with-scope [s] (a/self)))) ; !! SimpleError
```

A process can use scopes for work that fans out and comes back, as long as
it receives its messages outside them.

## Names

Passing a process around works, but a service that the whole program uses
is easier to find by name. `(register! name p)` registers a process, and
`send`, `ask`, `exit!` and `whereis` accept the name in place of the
process:

```clojure
(def settings (a/spawn-process store {:theme :light}))
(a/register! :settings settings)

(identical? settings (a/whereis :settings)) ; => true
(a/send :settings [:put :theme :dark])
(a/ask :settings [:get :theme]) ; => :dark
```

A name belongs to one living process at a time; registering a name that a
living process already has is an error. When the process ends, it loses its
name, and `whereis` returns `nil`:

```clojure
(a/exit! :settings)
(a/whereis :settings) ; => nil
```

A `send` to a name nobody has is dropped, like a send to an ended process.
`exit!` with an unknown name is an error.

## Links and monitors

Processes fail. The question is who finds out. Norn gives you two ways to
find out, with different meanings.

### Links

A link ties two processes together: when either ends abnormally, the other
ends too, with the same reason. `link!` links the current process to another,
and `spawn-link` starts a process already linked to the current one. Links
are for processes that only make sense together, such as a connection and
the process parsing its data. If one of them fails, the pair stops as a
unit, and nothing is left half-working.

A normal end doesn't travel along links. Only failures, and exits with
reasons other than `:normal`, do.

A process can choose to hear about a linked process's end instead of
sharing it. After `(trap-exits!)`, a linked process's end arrives as a
message, `[:EXIT p reason]`:

```clojure
(a/run-in-process
  (fn []
    (a/trap-exits!)
    (let [helper (a/spawn-link (fn [] (a/receive :go (quot 1 0))))]
      (a/send helper :go)
      (a/receive
        [:EXIT who reason] [(identical? who helper)
                            (class-name (class-of reason))]))))
; => [true norn.core/ArithmeticError]
```

The helper failed. Without `trap-exits!`, the process running this function
would have ended too. With it, the process got a message saying who ended
and why, and carried on. This is how supervisors, below, learn about their
children.

An `exit!` aimed at a process that traps exits also arrives as an `[:EXIT
from reason]` message rather than ending it, so the process can clean up
before it stops. The reason `:kill` is the exception: `(exit! p :kill)`
always ends the process, with the reason `:killed`.

### Monitors

A monitor is one-way and gentler. `(monitor p)` asks for a message when `p`
ends, for any reason, and returns a reference that identifies this monitor.
The message is `[:DOWN ref p reason]`. Nothing else happens to the watcher:

```clojure
(a/run-in-process
  (fn []
    (let [job (a/spawn-process (fn [] (a/receive :go :finished)))
          ref (a/monitor job)]
      (a/send job :go)
      (a/receive
        [:DOWN r _ reason] :when (= r ref) reason)))) ; => :normal
```

Use a monitor when you depend on a process but your fate isn't tied to it:
a client that wants to know when the server it's talking to goes away, or a
pool keeping track of its workers. If the process has already ended when you
start monitoring it, the `:DOWN` message comes at once.

## Supervisors

A supervisor is a process whose only job is to start other processes, its
children, and to restart them when they end. It links to each child and traps
exits, so a child's failure is a message to it, not its own failure.

### A supervised store

You describe each child with `child`: an id, then the function and its
arguments. When the id is a keyword, the child is registered under that name,
so the rest of the program can reach it without knowing which process it is
at the moment. `supervisor` takes options and a vector of children, starts
them in order, and returns the supervisor process:

```clojure
(def sup (a/supervisor {:strategy :one-for-one}
                       [(a/child :kv store {})]))

(mapv first (a/which-children sup)) ; => [:kv]
```

`which-children` asks the supervisor for its children, as `[id process]`
pairs. The supervisor starts its children in its own fiber, after
`supervisor` has returned, and it answers questions only once it has started
them, so `which-children` is also a way to wait until the children are up.

To see a restart, the store needs a way to crash. Here is a version with a
bug: a clause that divides by a number from the message, whatever it is.

```clojure
(a/defprocess store [data]
  (a/receive
    [:put k v] (recur (assoc data k v))
    [:delete k] (recur (dissoc data k))
    [:get k reply] (do (deliver reply (get data k))
                       (recur data))
    [:average k n reply] (do (deliver reply (quot (get data k) n))
                             (recur data))
    [:stop] :stopped))
```

Redefining `store` replaces the function in the var `store`, but the
running supervisor holds the function it was given, the old one. So stop it
and start a fresh one. This time pass the var, `#'store`, instead of the
function. A var can be called like the function it holds, and the supervisor
then looks up the newest definition each time it starts the child:

```clojure
(a/stop-supervisor! sup)

(def sup (a/supervisor {} [(a/child :kv #'store {:visits 0})]))
(a/which-children sup)
(a/send :kv [:put :visits 10])
(a/ask :kv [:average :visits 2]) ; => 5
```

`stop-supervisor!` stops the children, the last started first, and then the
supervisor. `{}` as options means the default strategy, `:one-for-one`.

Now ask for an average over zero visits. The division fails, and the store
process crashes. The `ask` gets no answer and times out:

```clojure
(def before (a/whereis :kv))

(a/ask :kv [:average :visits 0] 100) ; !! TimeoutError
(class-name (class-of (a/await-exit before))) ; => norn.core/ArithmeticError
```

The supervisor heard about the crash and started the child again, from its
original arguments. By the time `await-exit` returns, the news of the crash
is already in the supervisor's mailbox, so asking the supervisor for its
children waits until it has dealt with the crash. Then a new process has the
name `:kv`, with the starting state, `{:visits 0}`:

```clojure
(mapv first (a/which-children sup)) ; => [:kv]
(identical? before (a/whereis :kv)) ; => false
(a/ask :kv [:get :visits]) ; => 0
```

That is the Erlang idea known as "let it crash". The store didn't need code
to recover from a bad request. It failed, and it came back in a known good
state, its initial one. The rest of the program kept running throughout, and
code that used the name `:kv` reached the new process without any change.

Notice what was lost: the state that the crashed process held. A supervisor
restores a process, not its data. Keep state that must survive a crash
somewhere a crash can't reach, such as a database ([Chapter
23](23-databases.md)), or in a simpler process that is less likely to fail.

### Restart strategies

With several children, the `:strategy` option says which of them to restart
when one ends:

- `:one-for-one` restarts only the child that ended. Use it when the
  children are independent.
- `:one-for-all` stops all the other children and restarts them all. Use it
  when the children depend on each other so much that none can carry on
  without the rest.
- `:rest-for-one` restarts the child that ended and every child started after
  it. Use it when later children depend on earlier ones, such as a cache
  process started after the store it caches.

You can see the difference by crashing the middle one of three children and
comparing the processes before and after:

```clojure
(a/defprocess worker [n]
  (a/receive
    [:get reply] (do (deliver reply n) (recur n))
    [:crash] (quot n 0)))

(defn crash-the-middle-one [strategy]
  (let [sup (a/supervisor {:strategy strategy}
                          [(a/child :first worker 1)
                           (a/child :middle worker 2)
                           (a/child :last worker 3)])
        before (mapv second (a/which-children sup))]
    (a/send :middle [:crash])
    (a/await-exit (second before))
    (let [after (mapv second (a/which-children sup))]
      (a/stop-supervisor! sup)
      (mapv (fn [old new] (if (identical? old new) :kept :restarted))
            before after))))

(crash-the-middle-one :one-for-one) ; => [:kept :restarted :kept]
(crash-the-middle-one :one-for-all) ; => [:restarted :restarted :restarted]
(crash-the-middle-one :rest-for-one) ; => [:kept :restarted :restarted]
```

### Restart kinds and limits

Not every child should come back every time. A map of options before the
function gives a child its `:restart` kind:

- `:permanent`, the default, is always restarted, even after a normal end.
- `:transient` is restarted only after an abnormal end, a crash or an exit
  with a reason other than `:normal`.
- `:temporary` is never restarted.

```clojure
(a/defprocess task [n]
  (a/receive
    [:finish] :done
    [:crash] (quot n 0)))

(def kinds (a/supervisor {} [(a/child :always task 1)
                             (a/child :on-failure {:restart :transient} task 2)
                             (a/child :never {:restart :temporary} task 3)]))

(def originals (mapv second (a/which-children kinds)))
(a/send :always [:finish])
(a/send :on-failure [:finish])
(a/send :never [:crash])
(doseq [p originals] (a/await-exit p))

(mapv first (a/which-children kinds)) ; => [:always]
(a/stop-supervisor! kinds)
```

All three children ended. The permanent one came back, although it finished
normally. The transient one finished normally, so it stays finished. The
temporary one crashed, and stays gone.

A child that crashes every time it starts would be restarted forever, so a
supervisor gives up after too many restarts: more than `:max-restarts`
(default 3) within `:max-ms` milliseconds (default 5000). It then stops all
its children and ends itself, with the reason `:too-many-restarts`:

```clojure
(def strict (a/supervisor {:max-restarts 2 :max-ms 10000}
                          [(a/child :flaky worker 0)]))
(a/which-children strict)

(dotimes [i 3]
  (let [p (a/whereis :flaky)]
    (a/send p [:crash])
    (a/await-exit p)
    (when (< i 2) (a/which-children strict))))
```

The loop crashed the child three times. After each of the first two crashes,
it waited for the supervisor to restart the child. Those two restarts were
within the limit. The third crash was one too many:

```clojure
(a/await-exit strict 1000) ; => :too-many-restarts
(a/whereis :flaky) ; => nil
```

### Shutting down cleanly

When a supervisor stops a child, during `stop-supervisor!` or a restart of
several children, it doesn't kill it outright. It calls `exit!` with the
reason `:shutdown` and waits up to `:shutdown-ms` milliseconds (default 5000,
set per child like `:restart`) for the child to end. Only then does it kill
it. A child that traps exits receives `[:EXIT supervisor :shutdown]`, and can
finish what it was doing:

```clojure
(def saved (atom nil))

(a/defprocess careful [data]
  (a/trap-exits!)
  (a/receive
    [:put k v] (recur (assoc data k v))
    [:ping reply] (do (deliver reply :pong) (recur data))
    [:EXIT _ :shutdown] (reset! saved data)))

(def service (a/supervisor {} [(a/child :careful careful {})]))
(a/which-children service)
(a/ask :careful [:ping]) ; => :pong
(a/send :careful [:put :draft "chapter 18"])
(a/stop-supervisor! service)

@saved ; => {:draft "chapter 18"}
```

`trap-exits!` is called each time the body loops, which is harmless. The
`:ping` is there so the example can wait until the process is running and
trapping exits; a process that is told to shut down before it has called
`trap-exits!` is ended at once.

### Children that come and go

The children a supervisor starts with needn't be all it ever has. A server
that starts a process per connection, or a monitor that watches a list of
sites someone can add to, gets new work while it runs. `start-child!` gives
a running supervisor another child, started after the others and supervised
like them. It returns the child's process, once the child is up.
`stop-child!` stops one, as the supervisor stops its children, and forgets
it, so it isn't restarted:

```clojure
(def pool (a/supervisor {} []))

(a/start-child! pool (a/child :w1 worker 1))
(a/start-child! pool (a/child :w2 worker 2))
(mapv first (a/which-children pool)) ; => [:w1 :w2]

(a/stop-child! pool :w1) ; => true
(mapv first (a/which-children pool)) ; => [:w2]
(a/whereis :w1) ; => nil
```

A child whose id the supervisor already has is an error, as is one that
can't start, and the supervisor carries on as it was. `stop-child!` with an
id the supervisor doesn't have returns `false`:

```clojure
(a/start-child! pool (a/child :w2 worker 3)) ; !! Error
(a/stop-child! pool :w1) ; => false
(a/stop-supervisor! pool)
```

### Supervision trees

A supervisor is a process, so it can be the child of another supervisor.
`supervise` is the function a supervisor process runs, and `(child id
a/supervise options children)` makes a supervisor a child. The result is a
tree: workers at the leaves, supervisors above them, each restarting its own
part.

```clojure
(def app
  (a/supervisor {:strategy :one-for-one}
                [(a/child :storage a/supervise {:strategy :rest-for-one}
                          [(a/child :db store {})
                           (a/child :db-cache store {})])
                 (a/child :jobs worker 0)]))

(a/which-children app)
(def storage (a/whereis :storage))
(mapv first (a/which-children storage)) ; => [:db :db-cache]
```

A crash in `:db` restarts `:db` and `:db-cache`, which the storage supervisor
handles on its own; `:jobs`, under the top supervisor, isn't touched. If the
storage supervisor itself gives up, the top supervisor sees its child end and
restarts the whole storage subtree. Failures are handled as close to where
they happen as possible, and escalate only when local restarts don't help.

Stopping the top supervisor stops the whole tree:

```clojure
(a/stop-supervisor! app)
[(a/whereis :db) (a/whereis :storage) (a/whereis :jobs)] ; => [nil nil nil]
```

## Crashes and the debugger

A crashed process tells its links and monitors that it has ended, and then
its condition goes on to the debugger, as any fiber's unhandled condition
does. In a program run with `norn file.norn`, that means the error is printed
and the fiber ends, which is what you've seen in this chapter.

At the REPL, or from an editor, unhandled conditions park the fiber in the
debugger with its stack intact ([Chapter 12](12-live-programming.md)). For
a supervised process, both things happen at once: the supervisor restarts
the child, so the service keeps working, and the crashed process waits in the
debugger, so you can look at exactly what went wrong.

This example binds `*debugger*` to `:park`, the setting the REPL uses, while
it starts a supervisor, so the supervisor and its children inherit it. The
`wait-for` helper polls until a function returns something truthy, because
the crashed fiber reaches the debugger a moment after its supervisor hears of
the crash:

```clojure
(defn wait-for [f]
  (loop [n 0]
    (or (f) (when (< n 200) (sleep 10) (recur (inc n))))))

(def watched
  (binding [*debugger* :park]
    (a/supervisor {} [(a/child :calc #'store {:total 10})])))

(a/which-children watched)
(def broken (a/whereis :calc))
(a/send :calc [:average :total 0 (promise)])
(a/await-exit broken)
(a/which-children watched)

(a/ask :calc [:get :total]) ; => 10

(def parked (wait-for (fn [] (first (debugging)))))
(:condition parked) ; => "Divide by zero"
```

The new `:calc` answers questions while the old one is parked. `debugging`
lists the fibers waiting in the debugger, and `backtrace` shows a parked
fiber's frames. The crashed process's own function is among them:

```clojure
(boolean (some (fn [frame] (= 'user/store (:fn frame)))
               (backtrace (:fiber parked)))) ; => true
```

When you've seen enough, `debug-abort` ends the parked fiber, running its
cleanups. Aborting it doesn't trigger another restart, because the
supervisor already replaced it:

```clojure
(debug-abort (:fiber parked))
(wait-for (fn [] (empty? (debugging)))) ; => true
(a/stop-supervisor! watched)
```

At the REPL you would do the same with the debugger's commands. This is the
live-programming loop applied to a running service: the service stays up,
you inspect the failure, and you fix the function with `defprocess`. Because
the child was given as the var `#'store`, the next restart uses the fix. A
process that is already running keeps looping in the definition it started
with, until it ends.

## Processes, scopes, or channels?

Norn gives you three ways to organize concurrent work, and they fit
together:

- Use **scopes** ([Chapter 15](15-fibers-and-scopes.md)) for work that
  starts, finishes, and returns: handling a request, loading several things
  at once, a parallel computation. A failure goes to the caller.
- Use **channels** ([Chapter 16](16-channels.md)) for streams of values
  between fibers: pipelines, producers and consumers, fan-in and fan-out.
- Use **processes** for long-lived components that own state and serve
  requests, and put them under supervisors so that their failures are
  repaired as well as reported.

A process can use scopes and channels inside its handlers, and a scope's
fibers can `send` to and `ask` processes. `norn doc norn.actor` lists
everything in the library.

## Summary

- A process is a fiber with a mailbox. `defprocess` defines its loop,
  `spawn-process` starts it, and its state lives in its arguments, changed
  with `recur`.
- `send` puts a message in a mailbox without waiting. `ask` adds a reply
  promise to the message and waits for the answer, raising `TimeoutError` if
  none comes.
- `receive` takes the first message that a clause matches, with optional
  `:when` guards and an `:after` timeout, and leaves the rest in order.
- A process ends with a reason: `:normal`, the condition it failed with,
  `:killed`, or one given to `exit!`. `await-exit` waits for it. Names from
  `register!` let others find a process and are freed when it ends.
- Links share failure between processes; `trap-exits!` turns it into
  `[:EXIT p reason]` messages. Monitors deliver `[:DOWN ref p reason]` and
  nothing more.
- Supervisors restart their children by strategy (`:one-for-one`,
  `:one-for-all`, `:rest-for-one`) and kind (`:permanent`, `:transient`,
  `:temporary`), give up after too many restarts, and nest into trees.
- A restarted process starts from its original arguments; keep state that
  must survive elsewhere.
- At the REPL, a crashed process waits in the debugger while its supervisor
  has already replaced it.

---

[Contents](README.md) · Previous: [17. Shared state](17-shared-state.md) · Next: [19. Files and streams](19-files-and-streams.md) <!-- nav -->
