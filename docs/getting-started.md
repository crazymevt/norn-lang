# Getting started with Norn

[Documentation home](README.md)

## Build and start the REPL

From the repository root, use a stable Rust toolchain with Cargo.

```sh
cargo build --release -p norn --bin norn
./target/release/norn
```

The REPL starts in the `user` namespace. Enter a form and it prints the
result. The comments below show the expected results.

```clojure
(+ 2 3) ; => 5
(* (+ 2 3) 4) ; => 20
(if nil :yes :no) ; => :no
(if () :yes :no) ; => :yes
```

A nonempty list in code usually calls a function. Its first element supplies
the function, and the remaining elements supply arguments. Special forms and
macros control how their arguments are interpreted. Quote a form to use it as
data. Keywords such as `:yes` evaluate to themselves. Only `nil` and `false`
are falsey.

```clojure
'(+ 2 3) ; => (+ 2 3)
[(* 2 3) (inc 3)] ; => [6 4]
(:name {:name "Ada"}) ; => "Ada"
```

## Transform a collection

Use `defn` to name a function. A map binding such as `{:keys [name active]}`
extracts the values at `:name` and `:active` into locals. The `->>` macro
inserts the previous result as the final argument of each step.

```clojure
(defn active-names [people]
  (->> people
       (filter (fn [{:keys [active]}] active))
       (map (fn [{:keys [name]}] name))))

(active-names [{:name "Ada" :active true}
               {:name "Grace" :active false}
               {:name "Barbara" :active true}]) ; => ["Ada" "Barbara"]
```

On a vector, `filter` and `map` run immediately and return vectors. For an
unbounded computation, start with an explicit stream and bound what you consume.

```clojure
(vec (take 4 (iterate #(* 2 %) 1))) ; => [1 2 4 8]
```

## Run a module

A file that begins with `ns` is a module. It can require other modules and
refer to functions defined later in the file. Save this as `greet.norn`.

```clojure
(ns greet
  (:require [norn.string :as str]))

(defn greeting [name]
  (str "Hello, " (str/upper-case name) "!"))

(println (greeting "Ada"))
```

Run it from the directory where you saved it, using the path to the binary
you built. This command assumes you saved it at the repository root.

```sh
./target/release/norn greet.norn
```

It prints `Hello, ADA!`. A module can also be loaded by name. For example,
`greet.people` lives at `greet/people.norn` under a library search path.
Use `(require '[greet.people :as people])` at the REPL, or put
`(:require [greet.people :as people])` in another module's `ns` form.

## Look up a name

At the REPL, try `(doc map)`, `(source map)`, or `(apropos "range")`. From
the shell, use `./target/release/norn doc norn.string norn.string/upper-case`
to list a namespace's definitions and show help for a single function.

Continue with the [runnable examples](examples/README.md) or look up a topic
in the [language reference](reference/README.md).
