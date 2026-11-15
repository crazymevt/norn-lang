# Standard library

[Reference contents](README.md) · [Symbol index](symbols.md)

`norn.core` is available by default. To use another namespace, declare it
in `ns` or load it at the REPL with `require`. List a namespace's public
definitions with `norn doc NAMESPACE`. Use `norn doc NAMESPACE/SYMBOL` for the
signature, docstring, and any examples recorded for that symbol.

```sh
norn doc norn.string
norn doc norn.json/read-string norn.io/with-open norn.sqlite/query
```

## Text and data

| Namespace | Use |
| --- | --- |
| `norn.string` | Split, join, trim, and transform strings. |
| `norn.regex` | Compile patterns and find, replace, or split on matches. |
| `norn.rope` | Edit large texts with persistent ropes. |
| `norn.format` | Format numbers and quantities for display. |
| `norn.pprint` | Pretty-print nested data within a width. |
| `norn.markdown` | Read and render Markdown. |
| `norn.json` | Read and write JSON, including file helpers. |
| `norn.edn` | Read and write Norn data as text without evaluating it. |
| `norn.csv` | Read and write comma-separated rows and maps. |
| `norn.toml` | Read and write TOML configuration. |
| `norn.bin` | Encode and decode supported Norn data in a binary format. |
| `norn.bytes` | Work with byte strings, UTF-8, hexadecimal, and Base64. |
| `norn.compress` | Compress or decompress deflate, zlib, and gzip data. |
| `norn.zip` | Read and write ZIP archives. |
| `norn.data` | Compare nested values and report what differs. |
| `norn.set` | Set operations and relational operations on sets of maps. |
| `norn.walk` | Traverse and transform nested collections. |
| `norn.math` | Mathematical functions and constants. |
| `norn.random` | Random values and seeded generators. |
| `norn.uuid` | Work with UUIDs as strings. |
| `norn.crypto` | Digests, HMAC, key derivation, and secure random bytes. |

`(norn.json/read-string s)` reads JSON objects as maps with keyword keys.
Pass `:keywords false` to retain string keys. Arrays become vectors and
JSON `null` becomes `nil`.

```clojure
(require '[norn.json :as json])

[(json/read-string "{\"id\":7}")
 (json/read-string "{\"id\":7}" :keywords false)] ; => [{:id 7} {"id" 7}]
```

## I/O and the system

| Namespace | Use |
| --- | --- |
| `norn.io` | Read and write whole files or streams, and work with paths. |
| `norn.fs` | Walk directories, apply glob patterns, and watch for changes. |
| `norn.sys` | Read process arguments, environment variables, and system information. |
| `norn.process` | Run operating-system processes and work with pipes or pseudo-terminals. |
| `norn.signal` | Receive operating-system signals through channels. |
| `norn.net` | Work with TCP, UDP, and TLS. |
| `norn.http` | Send HTTP/1.1 requests and serve request maps with functions. |
| `norn.uri` | Parse, build, and percent-encode URIs. |
| `norn.sqlite` | Open SQLite databases, bind query parameters, and run transactions. |
| `norn.ffi` | Call C functions and describe native memory and data types. |
| `norn.log` | Emit leveled, structured logs. |
| `norn.time` | Work with instants, durations, and calendar dates. |
| `norn.schedule` | Run code on cron-like schedules. |

For a file, `norn.io/read-text` reads UTF-8 text and
`norn.io/read-bytes` reads bytes. For stream access,
`(norn.io/with-open [h (norn.io/open path :read)] body…)` closes the
handle when the body exits, including on failure.

SQLite uses the platform's SQLite library. `with-connection` closes the
connection after its body. In a statement vector, the first item supplies
SQL and the remaining items supply bound parameters.

```clojure
(require '[norn.sqlite :as db])

(db/with-connection [conn ":memory:"]
  (db/execute! conn "CREATE TABLE books (title TEXT)")
  (db/execute! conn ["INSERT INTO books VALUES (?)" "The Left Hand of Darkness"])
  (db/query-one conn ["SELECT title FROM books WHERE title = ?" "The Left Hand of Darkness"]))
; => {:title "The Left Hand of Darkness"}
```

## Concurrency

| Namespace | Use |
| --- | --- |
| `norn.async` | Connect channels with pipes, merges, fan-out, and publish/subscribe. |
| `norn.sync` | Coordinate fibers with semaphores, latches, barriers, and related types. |
| `norn.actor` | Run fibers with mailboxes, selective receive, links, monitors, and supervisors. |
| `norn.stm` | Coordinate changes to several refs in a transaction. |
| `norn.cache` | Use bounded caches and memoization. |

See [concurrency and state](concurrency.md) for channels, scopes, and a
transaction example.

## Language development

| Namespace | Use |
| --- | --- |
| `norn.core` | Language definitions, built-in classes, sequence functions, conditions, and concurrency primitives. |
| `norn.syntax` | Inspect and construct syntax objects, and write structured syntax patterns. |
| `norn.compiler`, `norn.expander` | The compiler and hygienic expander. |
| `norn.validate` | Validate the shapes of common forms during expansion. |
| `norn.test` | Write tests, fixtures, and assertions and run suites. |
| `norn.bench` | Measure code execution. |
| `norn.fmt`, `norn.lint`, `norn.check` | Format, lint, and check source. |
| `norn.doc`, `norn.expand` | Inspect documentation or trace macro expansion. |
| `norn.deps` | Read project manifests and resolve dependencies. |
| `norn.image` | Inspect and compare saved images. |

See [command-line tools](../tooling.md) for the corresponding shell commands.
