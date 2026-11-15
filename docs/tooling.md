# Command-line tools

[Documentation home](README.md)

The examples use `norn` on your `PATH`. From the repository root, you can
also substitute `./target/release/norn` after [building it](getting-started.md#build-and-start-the-repl).

| Command | Behavior |
| --- | --- |
| `norn` | Start the REPL in `user`. |
| `norn -e 'EXPR'` | Evaluate forms and print the last result. |
| `norn FILE.norn [ARGS…]` | Run a source file. Use `norn.sys/args` for the full process argument vector, starting with the program name. |
| `norn --server [PORT]` | Start the REPL and listen on loopback for [editor protocol](ui-protocol.md) clients. The default port is 7888. |
| `norn doc NAME…` | List a namespace, look up a qualified symbol, or look up an unqualified name in `norn.core`. |
| `norn expand --steps -e 'EXPR'` | Show macro expansion steps. This command also accepts a source filename in place of `-e 'EXPR'`. |
| `norn fmt [PATH…]` | Format source files. Add `--check` to report files that need changes. |
| `norn lint [PATH…]` | Report source diagnostics, including unresolved names and unused imports. |
| `norn test [PATH…]` | Discover and run tests. Options include `--only REGEX`, `--serial`, `--tap`, and `--junit FILE`. |
| `norn check [PATH…]` | Run formatting checks, lint, tests, and examples registered in the metadata of loaded definitions. |

## Write and run a test

Save this example as `greeting_test.norn`, then run
`norn test greeting_test.norn`. The runner recognizes files ending in
`_test.norn` when searching directories.

```clojure
(ns greeting-test
  (:require [norn.test :refer [deftest is testing]]))

(defn greeting [who]
  (str "Hello, " who "!"))

(deftest greets-the-caller
  (testing "includes the supplied name"
    (is (= "Hello, Ada!" (greeting "Ada")))))
```

A definition can include executable examples in its attribute map. Each
pair contains an expression and an expected expression. `norn check`
evaluates both in the definition's namespace and compares them with `=`.
Markdown code fences are separate from this metadata.

```clojure
(defn twice
  "Return twice the input."
  {:examples [[(twice 3) 6]]}
  [n]
  (+ n n))
```

## Projects and dependencies

Put `norn.edn` at the project root. `:paths` defaults to `["src"]`, and
`:main` names the entry function for `norn build` when no source files are
given. For example, `app.main` goes in `src/app/main.norn`.

```edn
{:name sample-app
 :version "0.1.0"
 :paths ["src"]
 :main app.main/main}
```

Add dependencies under `:deps`. A dependency can point to a folder with
`{:path "../shared"}` or to a Git repository with `:git` and either
`:tag` or `:sha`. Paths are relative to the project file that declares them.
Run `norn deps` to resolve dependencies and write `norn.lock`, which pins Git
dependencies to commits. `norn deps --update` resolves them again.

## Images

An image saves a Norn runtime's heap. You can save a development session with
`(save-image "dev.nimg")` and start it again with `norn --image dev.nimg`.
Use `norn image info dev.nimg` to inspect it and `norn image diff A.nimg B.nimg`
to compare two images. Native handles such as open files and sockets need to
be reopened or reconnected after restore.

Build a runnable image from a project with `norn build app.nimg`, or specify
a source file and entry function explicitly.

```sh
norn build app.nimg src/app/main.norn --entry app.main/main
norn --image app.nimg arg1 arg2
```

An entry function receives one string argument per command-line argument after
the image path. An integer return value supplies the process exit status,
clamped to 0–255. Other successful return values give exit status 0.
