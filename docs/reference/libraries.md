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
| [`norn.ndjson`](#json-lines) | Read and write JSON Lines, with record-by-record reduction. |
| [`norn.xml`](#xml) | Read and write XML 1.0 documents with namespace-aware names. |
| [`norn.yaml`](#yaml) | Read and write YAML configuration documents. |
| [`norn.cbor`](#cbor) | Encode and decode CBOR for binary interchange. |
| `norn.edn` | Read and write Norn data as text without evaluating it. |
| `norn.csv` | Read and write comma-separated rows and maps. |
| `norn.toml` | Read and write TOML configuration, with optional typed local dates and times. |
| `norn.bin` | Encode and decode supported Norn data in a binary format. |
| [`norn.bytes`](#bytes-and-packed-buffers) | Work with byte strings, text encodings, hexadecimal, and Base64. |
| [`norn.buffer`](#bytes-and-packed-buffers) | Store and read packed integer and floating-point values. |
| `norn.compress` | Compress or decompress deflate, zlib, and gzip data. |
| `norn.zip` | Read and write ZIP archives. |
| `norn.data` | Compare nested values and report what differs. |
| `norn.set` | Set operations and relational operations on sets of maps. |
| `norn.walk` | Traverse and transform nested collections. |
| `norn.math` | Mathematical functions and constants. |
| [`norn.decimal`](#decimal-values) | Work with exact base-10 values, scale, and explicit rounding. |
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

### JSON Lines

`norn.ndjson/read-string` and `read-file` return vectors. Pass `:keywords false`
to keep JSON object keys as strings. Each line needs one complete JSON value.
`write-string` and `write-file` emit a line feed after each value. For streams,
`reduce-records` takes a `norn.io/line-reader` and `write-record` takes a handle.
An early `reduced` result leaves the reader positioned at the next record.

```clojure
(require '[norn.ndjson :as ndjson])

(ndjson/read-string "{\"id\":7}\nnull\n") ; => [{:id 7} nil]
```

### XML

`(norn.xml/read-string text)` reads decoded XML text. `(norn.xml/read-file path)`
reads UTF-8 files and UTF-16 files with a byte-order mark. Both parse the
whole document and return its root element as a map.

| Key | Value |
| --- | --- |
| `:tag` | The element's expanded name, `[namespace-uri local-name]`. |
| `:attrs` | A map from expanded attribute names to strings. |
| `:namespaces` | The in-scope prefix bindings, including the built-in `xml` prefix. The `nil` key holds the default namespace when one is in scope. |
| `:content` | Child elements and text strings in document order. |

In expanded names, `nil` is the namespace for unqualified names. An active
default namespace applies to unprefixed elements. Unprefixed attributes
always have a `nil` namespace. Namespace declarations contribute to
`:namespaces` and URI strings are compared literally after XML attribute
normalization.

```clojure
(require '[norn.xml :as xml])

(let [catalog (xml/read-string
                (str "<catalog xmlns='urn:catalog' xmlns:m='urn:meta'>"
                     "<item id='7' m:kind='book'>Ada</item></catalog>"))
      item (first (:content catalog))]
  [(:tag item)
   (get-in item [:attrs [nil "id"]])
   (get-in item [:attrs ["urn:meta" "kind"]])
   (:content item)])
; => [["urn:catalog" "item"] "7" "book" ["Ada"]]
```

The parser keeps whitespace in element text and combines adjacent text,
including CDATA. Comments and processing instructions are discarded.
The parser expands predefined entities and numeric character references.
It rejects `DOCTYPE` declarations. XML syntax errors raise `ParseError`
with `:source`, `:line`, and `:column`. An invalid file encoding raises
`ParseError` with the file path as `:source`.

`(norn.xml/write-string root)` writes an element tree, and `(norn.xml/write-file
path root)` writes it as UTF-8. The writer escapes text and attributes,
reuses in-scope prefixes where possible, and generates prefixes for expanded
names without a binding. An element without `:namespaces` inherits its
parent's bindings. An element with `:namespaces` takes that map as its whole
scope, as `read-string` returns it, so leaving out the `nil` key means no
default namespace. Inherited prefixes stay in scope because XML 1.0 can't
undeclare them. The writer preserves the parsed tree's data. Original
prefix choices, comments, and CDATA boundaries can change when a tree is
written back out.

### YAML

`norn.yaml/read-string` and `read-file` read a single configuration document
with YAML 1.2 Core scalar resolution. They accept block and flow maps and
sequences, quoted strings, literal and folded block strings, and aliases to
completed anchors. String keys stay strings by default. Use `:keys keyword`
to convert them. `write-string` and `write-file` emit flow collections.
Keyword values and keys are written as strings.

```clojure
(require '[norn.yaml :as yaml])

(yaml/read-string "server:\n  ports: [80, 443]\n  enabled: true\n" :keys keyword)
; => {:server {:ports [80 443] :enabled true}}
```

The configuration reader has a depth limit of 128 and accepts scalar map keys.
Custom tags, directives, cyclic aliases, multiline plain or quoted scalars,
multi-document streams, and comment-preserving edits are outside its API.
The merge key `<<` is an ordinary key.

### CBOR

`norn.cbor/encode` and `decode` convert one item to and from `Bytes`. File
helpers are `read-file` and `write-file`. The codec handles integers, floats,
bytes, text, vectors, maps, null, and booleans. Bignum tags become integers,
decimal fraction tags become `norn.decimal` values, and other tags retain
their tag number and payload in `Tagged` instances. The reader accepts
indefinite-length arrays, maps, byte strings, and text strings. The writer
uses definite lengths and binary64 floats and makes no canonical ordering
guarantee. Both enforce a nesting limit of 256.

### Decimal values

`norn.decimal/parse` reads decimal text without going through a float.
`decimal` constructs a value from an integer coefficient and an optional
scale. Arithmetic is available through `add`, `subtract`, `multiply`, and
`divide`. Division and `quantize` take a result scale and default to
half-even rounding. Use `compare` to compare numeric values across scales.
Structural equality includes scale.

```clojure
(require '[norn.decimal :as d])

(d/text (d/add (d/parse "0.10") (d/parse "0.2"))) ; => "0.30"
```

### Bytes and packed buffers

`norn.bytes/decoder` starts a UTF-8 or UTF-16 decoder in a specified byte
order. Feed byte chunks to `decode-chunk` and pass its returned state to the
next call. `finish-decoder` detects a truncated character at the end.
`decode-text` handles a complete byte string. The decoder preserves BOM
characters so a format reader can interpret them.

`norn.buffer/pack` writes numeric elements into an immutable packed buffer.
`from-bytes` views existing `Bytes` without copying. `nth` reads an element,
`count` returns the element count, and `to-bytes` returns the backing value.
Supported types are `:u8`, `:i8`, `:u16`, `:i16`, `:u32`, `:i32`, `:u64`,
`:i64`, `:f32`, and `:f64`. Byte order defaults to `:le`. Use `:order :be`
for big-endian data.

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
| `norn.ffi` | Call C functions and describe native memory and data types. Pass `:main-thread true` for APIs that must run on the process's first thread, such as windowing and graphics. |
| `norn.log` | Emit leveled, structured logs. |
| `norn.time` | Work with instants, durations, and calendar dates. |
| `norn.schedule` | Run code on cron-like schedules. |

`norn.time` also has `LocalDate`, `LocalTime`, and `LocalDateTime` values for
calendar dates and clock times without a zone. Construct them with `local-date`,
`local-time`, and `local-date-time`, or use their `parse-local-*` helpers.
`format-local` writes their ISO calendar text. Pass `:local-times true` to
`norn.toml/read-string` to read TOML local dates and times as these values.
The default keeps returning strings. TOML writing accepts the typed values.

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
