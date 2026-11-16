# 12. Live programming

[Contents](README.md) · Previous: [11. Conditions and restarts](11-conditions-and-restarts.md) · Next: [13. Contracts](13-contracts.md) <!-- nav -->

Norn is built to be changed while it runs. You start one process, load your
code into it, and then keep working inside it: try an expression, redefine
a function, and call it again, without restarting. When something fails, the
failing fiber stops where it is and waits, with its stack and local variables
intact, so you can look around, fix the code, and let it carry on.

This chapter covers that way of working. It starts with the REPL and what
happens when you redefine code that is already running. Then it spends most
of its time on the debugger: reading what it shows, inspecting frames,
fixing and resuming, and handling fibers that fail in the background. It
ends with the tools for finding things out (`doc`, `source`, `apropos`,
`inspect`, `trace`), with `norn --server`, which lets an editor do all of
this, and with saving a session as an image.

[Chapter 2](02-first-steps.md) covered starting the REPL. The debugger is
built on conditions and restarts, which [Chapter 11](11-conditions-and-restarts.md)
explains; this chapter assumes you've read it.

## The REPL

Run `norn` with no arguments to start the REPL. It prints a banner, then a
prompt with the current namespace:

```text
$ norn
Norn 0.0.1 (fibers on 10 carriers)
Ctrl-C interrupts a running evaluation; Ctrl-D exits.
user=> (defn greet [name] (str "Hello, " name))
#'user/greet
user=> (greet "Ada")
"Hello, Ada"
user=> (defn greet [name] (str "Welcome, " name "."))
#'user/greet
user=> (greet "Ada")
"Welcome, Ada."
```

Each input is read, evaluated in its own fiber, and its value printed with
`prn`, so strings show their quotes. The number of carriers, the threads
that run fibers, depends on your machine. A form can span several lines;
the REPL waits until the brackets balance. Your history is kept in
`~/.norn_history`.

The prompt shows the current namespace. `in-ns` changes it, and definitions
then go into that namespace:

```text
user=> (require '[shop.prices :as prices])
nil
user=> (prices/with-tax 100)
120
user=> (in-ns 'shop.prices)
shop.prices
shop.prices=> (with-tax 10)
12
shop.prices=> (in-ns 'user)
user
```

When you've edited a file, load it again with
`(require 'shop.prices :reload)` or `(load-file "src/shop/prices.norn")`.
Its definitions replace the old ones, and everything that calls them uses
the new versions from then on. [Chapter 8](08-modules-and-projects.md) covers
`require` and search paths.

Loading a file again evaluates every `def` in it, so an atom it defines
starts over. Define state with `defonce` instead. It gives the name a value
only the first time, so what it holds survives a reload:

```clojure
(defonce visits (atom 0))
(swap! visits inc)
(defonce visits (atom 0))
@visits ; => 1
```

A long or stuck evaluation can be stopped with Ctrl-C. The REPL interrupts
the evaluation's fiber and shows where it was:

```text
user=> (defn spin [] (recur))
#'user/spin
user=> (spin)
Error: Interrupted
  at user/spin
  at norn.core/%with-bindings*
  at norn.core/%with-bindings*
  at fn
  at norn.core/%with-bindings*
user=> (+ 1 1)
2
```

Ctrl-D at the `user=>` prompt exits. At a `debug[N]>` prompt it aborts the
fiber, as `:a` does, and then exits.

## Redefining code that is running

Every top-level name in Norn is a var, and a call to a function by its name
looks the var up at the time of the call. So when you redefine a function,
every later call, from anywhere, gets the new definition. That includes code
already running in other fibers.

Here is a worker that takes messages from one channel and puts replies on
another. ([Chapter 16](16-channels.md) covers channels; `take!` waits for a
value, and returns `nil` once the channel is closed.)

```clojure
(defn handle [msg] (str "v1: " msg))

(def inbox (chan))
(def outbox (chan))

(def worker
  (spawn-detached
    (loop [handled 0]
      (if-some [msg (take! inbox)]
        (do (put! outbox (handle msg))
            (recur (inc handled)))
        handled))))

(put! inbox "first")
(take! outbox) ; => "v1: first"
```

The worker is now waiting for its next message. Redefine `handle` while it
waits, and the next message gets the new behavior:

```clojure
(defn handle [msg] (str "v2: " msg "!"))

(put! inbox "second")
(take! outbox) ; => "v2: second!"

(close! inbox)
(await worker) ; => 2
```

The worker's loop never stopped. It called `handle` by name, so it found the
new definition on its next call. Generic functions behave the same way: a
new `defmethod` applies on the next call, as
[Chapter 10](10-protocols-and-extension.md) showed, and so does a
redefined class.

Two things don't change under a running fiber. The body of a `loop` that is
already executing keeps running the code it was compiled from, which is why
the worker above does its real work in a separate, named function. And a
function that calls itself by its own name inside `defn` calls itself
directly, not through the var. So when you expect to change a piece of long-running code, put that piece in its own top-level function and call it by name.

## Reloading as you save

In a project, `norn dev` does the reloading for you. It runs the project's
`:main`, as `norn run` does, and each time you save one of the project's
modules it loads that module again. The running program picks up the
change the way the worker above did:

```sh
cd tally
norn dev notes.txt
```

A module that defines macros reloads the modules that use it too, since
they expanded the old macros. A file saved with a mistake that stops it
reading, such as a missing bracket, isn't loaded: the program keeps
running the old code, and `norn dev` prints the error with its line. When
`:main` has finished, as a command-line program's does, the next save
runs it again, which makes `norn dev` a quick way to try a program as you
edit it.

Reloading evaluates a module's definitions again, so state kept with
`def` starts over. Keep it with `defonce`, shown earlier. A program that
draws, such as a Norn UI app, can redraw after each reload:
`(norn.dev/after-reload! ::redraw (fn [modules] (redraw!)))` registers a
function that `norn dev` calls with the names of the modules it
reloaded. Outside `norn dev`, nothing calls it.

## When an evaluation fails

At the REPL, an error that no handler takes doesn't end the evaluation. The
fiber that signaled it parks: it stops where it is, keeps its stack, and
waits for you. Here a report divides by the length of an empty list:

```text
user=> (defn average [xs] (/ (reduce + xs) (count xs)))
#'user/average
user=> (defn report [label xs] (str label ": " (average xs)))
#'user/report
user=> (report "empty" [])
Fiber 5 is waiting in the debugger: Divide by zero
  Restarts:
    0: [use-value] Return a value from the failed operation instead
    1: [retry] Try the failed operation again (e.g. after defining or fixing something)
  Frames:
    2: fn {}
    6: user/average {xs []}
    7: user/report {xs [], label "empty"}
    10: fn {r #<Reader>, f (report "empty" [])}
  :r N [args…]  invoke restart N    :f N  restart frame N (newest definition)
  :bt  frames again    :a  abort    anything else is evaluated (e.g. redefine a function)
debug[5]>
```

The banner has four parts.

- **The first line** names the fiber, here fiber 5, and the condition's
  message. The fiber's number identifies it in every debugger command.
- **Restarts** are the restarts that were available when the error was
  signaled, numbered from 0, with their `:report` text. Runtime errors
  always offer `use-value` and `retry`, as Chapter 11 described. Restarts
  established with `restart-case` in your own code appear here too.
- **Frames** are the calls on the fiber's stack, innermost first, each with
  its local variables. The numbers have gaps because Norn's internal frames
  are hidden. `user/average` was running with `xs` bound to `[]`, called
  from `user/report`. The last frame is the REPL itself, evaluating the form
  you typed, and frames named `fn` are anonymous functions.
- **The command line** lists what you can type at the `debug[5]>` prompt.

The prompt changes to `debug[5]>` to show that you're talking to the
debugger for fiber 5. The commands are short:

| Command | Effect |
| --- | --- |
| `:r N args…` | Invoke restart `N`, passing the arguments. |
| `:f N` | Restart frame `N`: call its function again, in its newest definition, with the same arguments. |
| `:bt` | Show the banner again. |
| `:a` | Abort: unwind the fiber, running its cleanups, and end it. |

Anything else you type is evaluated as at the normal prompt, in a fiber of
its own. That is how you inspect and fix things before choosing a command.

## Looking around

`eval-in-frame` evaluates a form with one frame's local variables bound. It
takes the fiber number, the frame number from the banner, and a quoted form.
`backtrace` returns a fiber's frames as data, numbered the same way:

```text
debug[5]> (eval-in-frame 5 7 'label)
"empty"
debug[5]> (eval-in-frame 5 6 '(count xs))
0
debug[5]> (nth (backtrace 5) 6)
{:fn user/average, :locals {xs []}, :internal? false}
```

The form can be any expression. It sees the frame's locals and the current
namespace's definitions, so you can call your own functions on the values
that caused the trouble. Values inside the frame are shown as they were at
the moment of the error; instances of mutable classes are read without
ownership checks, since the debugger only looks.

`(debugging)` lists every fiber waiting in the debugger, which is useful
when there is more than one:

```text
debug[5]> (debugging)
[{:fiber 5, :condition "Divide by zero", :restarts [{:report "Return a value from the failed operation instead", :name "use-value"} {:report "Try the failed operation again (e.g. after defining or fixing something)", :name "retry"}]}]
```

## Fixing and continuing

Now fix the bug. Redefine `average` at the debug prompt, then restart frame
6, the call to `average`:

```text
debug[5]> (defn average [xs] (if (empty? xs) 0 (/ (reduce + xs) (count xs))))
#'user/average
debug[5]> :f 6
"empty: 0"
user=> (report "two" [1 2])
"two: 3/2"
```

`:f 6` called `average` again with its original argument, `[]`, using the
definition you entered. It returned 0 to `report`, which finished
normally, and the original evaluation printed its result. The prompt went
back to `user=>`. Nothing had to be run again from the start: the frames
above the restarted one, here `report` and the REPL's, carried on where they
were.

This is the core of the workflow. When something breaks deep in a long
computation, you don't lose the work done so far. You fix the function that
failed and restart its frame.

A frame can be restarted only while it's on the stack. A function whose last
act was a call to another function, a tail call, has handed its frame to
that call, so it no longer appears in the list.

### Choosing a restart

Sometimes you'd rather supply an answer than fix the code. `:r` invokes a
restart by number. Here `rate` was declared but never given a value:

```text
user=> (declare rate)
#'user/rate
user=> (defn total [price] (* price (+ 1 rate)))
#'user/total
user=> (total 100)
Fiber 5 is waiting in the debugger: Unbound var: #'user/rate
  Restarts:
    0: [use-value] Return a value from the failed operation instead
    1: [retry] Try the failed operation again (e.g. after defining or fixing something)
  Frames:
    2: fn {}
    6: user/total {price 100}
    9: fn {r #<Reader>, f (total 100)}
  :r N [args…]  invoke restart N    :f N  restart frame N (newest definition)
  :bt  frames again    :a  abort    anything else is evaluated (e.g. redefine a function)
debug[5]> :r 0 1/10
110
```

The failed operation was reading `rate`. `use-value` with `1/10` made that
read return `1/10`, and `total` finished with it. Nothing was defined, so the
next call fails the same way. This time, define the var and choose `retry`,
which reads it again:

```text
user=> (total 100)
Fiber 8 is waiting in the debugger: Unbound var: #'user/rate
  ...
debug[8]> (def rate 1/5)
#'user/rate
debug[8]> :r 1
120
user=> (total 50)
60
```

(The `...` stands for the rest of the banner, which is the same as before.)

When you'd rather give up, `:a` aborts the fiber. Its cleanups run, and the
REPL prints the error as a script would:

```text
user=> (/ 1 0)
Fiber 13 is waiting in the debugger: Divide by zero
  ...
debug[13]> :a
Error: Divide by zero
  at <toplevel>
  at norn.core/%with-bindings*
  at norn.core/%with-bindings*
  at fn
  at norn.core/%with-bindings*
user=>
```

If something you evaluate at the debug prompt fails too, a second debugger
opens for that new fiber, with its own `debug[N]>` prompt. Deal with it, and
you're back at the first one.

### Your own restarts

The restarts your code establishes show up in the same list, with their
`:report` text. Here is the order parser from Chapter 11, saved as
`orders.norn` with `(ns orders …)` at the top. A bad line stops in the
debugger, and the restarts offered are the ones `parse-order` defined:

```text
user=> (load-file "orders.norn")
#'orders/parse-orders
user=> (in-ns 'orders)
orders
orders=> (parse-orders ["widget,3" "gadget,x" "sprocket,2"])
Fiber 12 is waiting in the debugger: Bad order line: "gadget,x"
  Restarts:
    0: [skip-order] Leave this line out.
    1: [use-order] Use this order in its place.
  Frames:
    2: fn {}
    5: fn {x "gadget,x", y nil, acc [{:item "widget", :qty 3}]}
    9: orders/parse-orders {lines ["widget,3" "gadget,x" "sprocket,2"]}
    12: fn {r #<Reader>, f (parse-orders ["widget,3" "gadget,x" "sprocket,2"])}
  :r N [args…]  invoke restart N    :f N  restart frame N (newest definition)
  :bt  frames again    :a  abort    anything else is evaluated (e.g. redefine a function)
debug[12]> :r 1 {:item "gadget" :qty 1}
[{:item "widget", :qty 3} {:item "gadget", :qty 1} {:item "sprocket", :qty 2}]
```

`:r 1` took the argument `{:item "gadget" :qty 1}`, which `use-order`
returned in place of the bad line, and parsing went on to the third line.
`parse-order` isn't among the frames because it ended in a tail call to
`restart-case`; the frame named `fn` with `x` bound to the line is the
function inside `keep` that called it.

This is where restart design pays off. The restarts you write for handlers
in code are the same ones you pick from by hand at the REPL. The `:report`
strings are the menu.

## Fibers that fail on their own

The REPL binds the debugger mode for its evaluations, and fibers they spawn
inherit it. So a fiber started at the REPL parks on an unhandled error too,
even after the evaluation that started it has finished. If it parks while
the REPL is waiting on an evaluation, it gets the `debug[N]>` prompt, and
the evaluation goes on meanwhile. If it parks between evaluations, the
REPL shows its banner before it runs your next input, and the fiber keeps
waiting until you deal with it. For a fiber like that, use the functions
behind the commands. They take the fiber number:

| Function | Same as |
| --- | --- |
| `(debug-restart id n args…)` | `:r n args…` |
| `(debug-restart-frame id i)` | `:f i` |
| `(debug-abort id)` | `:a` |

```text
user=> (defn crunch [n] (/ 100 n))
#'user/crunch
user=> (def job (spawn-detached (crunch 0)))
#'user/job
user=> (sleep 200)
Fiber 5 is waiting in the debugger: Divide by zero
  Restarts:
    0: [use-value] Return a value from the failed operation instead
    1: [retry] Try the failed operation again (e.g. after defining or fixing something)
  Frames:
    2: fn {}
    6: user/crunch {n 0}
  (it keeps waiting; use (debug-restart 5 n), (debug-restart-frame 5 i) or (debug-abort 5))
nil
user=> (eval-in-frame 5 6 'n)
0
user=> (debug-restart 5 0 :skipped)
:ok
user=> (await job)
:skipped
```

The background fiber failed before `(sleep 200)` was read, so its banner
appeared before that evaluation ran. `debug-restart` resumed it with `use-value`, and the
fiber's result became `:skipped`. Every other fiber kept running the whole
time; only the one that failed was stopped.

A fiber spawned inside a scope is a common case of a fiber that parks while
the REPL waits. The evaluation waits for the scope to finish, and the scope
can't finish while its fiber waits in the debugger, so the REPL gives that
fiber the prompt:

```text
user=> (with-scope [s] (spawn s (crunch 0)) (await (spawn s 5)))
Fiber 6 is waiting in the debugger: Divide by zero
  Restarts:
    0: [use-value] Return a value from the failed operation instead
    1: [retry] Try the failed operation again (e.g. after defining or fixing something)
  Frames:
    2: fn {}
    6: user/crunch {n 0}
  (the evaluation goes on meanwhile)
  :r N [args…]  invoke restart N    :f N  restart frame N (newest definition)
  :bt  frames again    :a  abort    anything else is evaluated (e.g. redefine a function)
debug[6]> :r 0 :skipped
5
user=>
```

Once the fiber is resumed, the scope finishes and the evaluation prints its
value. Ctrl-C instead interrupts the evaluation, which cancels the scope and
the fiber with it: the prompt says the fiber is no longer in the debugger,
and `(debugging)` doesn't list it.

## The debugger outside the REPL

What happens to an unhandled error depends on `*debugger*`. The REPL and
editor evaluations bind it to `:park`, and every fiber they spawn inherits
that. Anywhere else it's unset, and a fiber with an unhandled error unwinds
and ends, which is why `norn script.norn` prints the error and exits.
`*debugger*` can also be a function, which is called with the condition
before the fiber ends; test runners use this to record failures.

The functions the debugger uses are ordinary Norn functions, so you can call
them from your own code and tools: `debugging` lists waiting fibers,
`backtrace` returns frames as data, and `eval-in-frame`, `debug-restart`,
`debug-restart-frame` and `debug-abort` do what the commands do.

## Finding things out

A live system can answer questions about itself. These are the tools you'll
use most at the REPL.

`doc` prints a function's argument lists and docstring. `source` prints its
definition. `apropos` finds names in every loaded namespace by substring or
regex, and `find-doc` searches docstrings:

```text
user=> (require '[norn.string :as string])
nil
user=> (doc string/split)
-------------------------
norn.string/split
([s pattern] [s pattern limit])
  The parts of s between matches of pattern (a string, character or regex).
  With limit, at most that many parts; the last holds the rest. An empty
  string pattern splits into characters.
nil
user=> (apropos #"^split")
[norn.core/split-at norn.core/split-with norn.string/split norn.string/split-lines]
user=> (find-doc "whitespace")
[norn.string/blank? norn.string/trim norn.string/triml norn.string/trimr]
user=> (source frequencies)
(defn frequencies [coll]
  (reduce (fn [m x] (assoc m x (inc (get m x 0)))) {} coll))
nil
```

`definition` returns what Norn knows about a definition as data, including
the file and line it came from, which is how editors jump to a definition.
`doc` and `source` work for code loaded from files, evaluated by an
editor, or typed at the terminal prompt. For a definition you typed,
`source` shows it as you typed it, comments and line breaks included.
`meta` gives the same record as a map, as in `(:arglists (meta #'map))`.

```clojure
(defn area "The area of a w by h rectangle." [w h] (* w h))

(:doc (definition 'area))     ; => "The area of a w by h rectangle."
(:arglists (definition 'area)) ; => ([w h])
```

`inspect` describes a value for looking inside it, as a map with its type
and its parts. Editors use it to show values you can open and explore:

```clojure
(inspect {:name "Ada" :langs [:norn :lisp]})
; => {:type "norn.core/Map" :value "{:name \"Ada\", :langs [:norn :lisp]}" :count 2
;     :items [[":name" "\"Ada\""] [":langs" "[:norn :lisp]"]]}
```

`trace` makes the named functions print each call and its result, and
`untrace` stops it. With no names, `untrace` stops tracing everything:

```clojure
(defn countdown [n] (if (zero? n) :done (countdown (dec n))))

(trace countdown)
(countdown 2)
; prints: → (user/countdown 2)
; prints: ← user/countdown :done
(untrace)
```

Only the outer call was traced. As the section on redefinition explained,
`countdown` calls itself directly, not through its var, and tracing works by
replacing the var's function.

`time` evaluates an expression, prints how long it took, and returns its
value:

```text
user=> (time (reduce + (range 100000)))
Elapsed time: 8.193708 msecs
4999950000
```

`macroexpand` and `macroexpand-1` show what a macro call becomes;
[Chapter 14](14-macros.md) covers them, along with `norn expand`.
[Chapter 26](26-tools-and-performance.md) covers `norn.bench` for
measurements more careful than `time`.

## Editors and norn --server

Typing at a terminal is one way in. An editor is better for real work: you
evaluate the form under the cursor, see results inline, and get a debugger
panel with clickable frames. `norn --server` starts the REPL and also
listens on the loopback interface for editor connections, on port 7888
unless you give another:

```text
$ norn --server
Editor protocol listening on 127.0.0.1:7888
Norn 0.0.1 (fibers on 10 carriers)
Ctrl-C interrupts a running evaluation; Ctrl-D exits.
user=>
```

The editor protocol is plain Norn data over TCP, one map per line. A request
has an `:op`, an `:id` the response repeats, and the op's own keys. Results
come back as printed text, in `:value`, so the editor never has to read
arbitrary Norn values. Here is a session, with `→` for what the editor sent
and `←` for what came back:

```text
→ {:op :eval :id 1 :code "(+ 1 2)"}
← {:ns "user", :id 1, :value "3"}
→ {:op :eval :id 2 :code "(defn average [xs] (/ (reduce + xs) (count xs)))"}
← {:ns "user", :id 2, :value "#'user/average"}
→ {:op :eval :id 3 :code "(println \"averaging\") (average [])"}
← {:event :out :id 3 :text "averaging\n"}
← {:event :debug, :fiber 5, :condition "<editor>:1: Divide by zero", :restarts [{:report "Return a value from the failed operation instead", :name "use-value"} {:report "Try the failed operation again (e.g. after defining or fixing something)", :name "retry"} {:report "Abort this evaluation", :name "abort"}], :class "norn.core/ArithmeticError"}
→ {:op :eval-in-frame :id 4 :fiber 5 :frame 6 :code "xs"}
← {:id 4, :value "[]"}
→ {:op :eval :id 5 :code "(defn average [xs] (if (empty? xs) 0 (/ (reduce + xs) (count xs))))"}
← {:ns "user", :id 5, :value "#'user/average"}
→ {:op :restart-frame :id 6 :fiber 5 :frame 6}
← {:id 6, :value ":ok"}
← {:event :resumed, :fiber 5}
← {:ns "user", :id 3, :value "0"}
```

Some things to notice:

- What request 3 printed streamed back as an `:out` event, tagged with the
  request's id, before the request finished.
- The failure came as a pushed `:debug` event, not a response. Request 3
  stayed open, and the editor went on sending requests on the same
  connection. Each request runs in its own fiber, so a request waiting in
  the debugger doesn't block the others.
- Editor evaluations get one more restart, `abort`, which ends the
  evaluation and answers it with `{:error "Aborted"}`.
- After `:restart-frame`, a `:resumed` event said the fiber was running
  again, and then request 3 got its answer.

The ops cover the rest of what an editor needs. Among them are `:eval` (with
`:ns`, and `:file` and `:line` so errors and definitions point at the
source), `:load-file`, `:interrupt`, `:complete`, `:doc`, `:definition`,
`:macroexpand`, `:inspect`, `:ns-vars`, `:format`, `:lint` and `:test`, and
for the debugger `:debugging`, `:backtrace`, `:eval-in-frame`, `:restart`
(with `:restart`, a number, and `:args`, a string of arguments),
`:restart-frame` and `:abort`. Since it's one map per line, you can try the
protocol by hand with a tool such as `nc localhost 7888`.

The editor server and the terminal REPL are two clients of the same running
system. Definitions made through one are visible to the other.

## Saving a session as an image

After an hour at the REPL, your process holds definitions, loaded libraries
and data you've built up. `save-image` writes the whole heap to a file, and
`norn --image` starts a new process from it:

```text
user=> (defn fact [n] (if (< n 2) 1 (* n (fact (dec n)))))
#'user/fact
user=> (def notes (atom ["remember the milk"]))
#'user/notes
user=> (save-image "dev.nimg")
{:reused 0, :objects 22771, :patches 0, :bytes 1269728}
```

Later, `norn --image dev.nimg` prints the banner and the image's path, then
gives you a REPL in which everything is as you left it:

```text
user=> (fact 10)
3628800
user=> @notes
["remember the milk"]
```

Fibers waiting in the debugger are saved too, with their stacks, and are
still waiting after the restore. Open files, sockets and other native
handles can't survive the trip and have to be reopened.
[Chapter 27](27-images-and-deployment.md) covers images in full, including
`:entry` functions, layered images, and building images for deployment.

An image is a convenience, not a substitute for source files. Keep your
definitions in files under version control, and treat a dev image as a
snapshot of a session you can return to.

## Summary

- The REPL evaluates each input in a fiber of its own. Redefining a function
  affects every later call by name, including calls from fibers that are
  already running.
- Put code you expect to change in top-level functions called by name. A
  running `loop` body, and a function's calls to itself, don't pick up
  redefinitions.
- `norn dev` runs a project's `:main` and reloads each module you save;
  `defonce` keeps state across reloads.
- At the REPL, an unhandled error parks its fiber with the stack intact. The
  banner lists the restarts and frames, and `debug[N]>` takes `:r`, `:f`,
  `:bt` and `:a`.
- `eval-in-frame` and `backtrace` inspect a parked fiber. Fix the code, then
  restart a frame with `:f N` or choose a restart with `:r N`.
- Background fibers park too. Use `debugging`, `debug-restart`,
  `debug-restart-frame` and `debug-abort` with their fiber numbers.
- `doc`, `source`, `apropos`, `find-doc`, `definition`, `inspect`, `trace`
  and `time` answer questions about the running system.
- `norn --server` speaks the editor protocol, one Norn map per line, with
  pushed debugger events and a fiber per request.
- `save-image` saves a session's heap, and `norn --image` resumes it.

---

[Contents](README.md) · Previous: [11. Conditions and restarts](11-conditions-and-restarts.md) · Next: [13. Contracts](13-contracts.md) <!-- nav -->
