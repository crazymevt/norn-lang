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

### EDN, CSV and TOML

`norn.edn/write-string` writes text that `read-string` reads back as an
equal value. Decimals and times are written as tagged literals
(`#norn/decimal "12.30"`, `#norn/date "2026-11-03"`, `#inst "…"`), as `pr-str`
prints them; `read-string` reads those, `#uuid`, and other tags through
`:readers` (a map of tag to function) and `:default` (a function of tag and
form). A value with no such text, such as an atom, a function, or an
instance whose class has no `tagged-form` method, signals `TypeError` naming it.
`:pretty true` lays the text out over lines. Use `pr-str` to print
anything.

`norn.csv/read-maps` reads a header row and makes a map of each row after it.
A row with more or fewer fields than the header signals `ParseError` with
the row's `:line`. Pass `:strict false` to drop extra fields and leave
missing ones out. Malformed CSV text signals `ParseError` too.

`norn.toml` has `read-file` and `write-file`, like the other formats.

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
completed anchors. String keys become keywords by default, as in
`norn.json`, `norn.toml`, and `norn.csv`. Use `:keys identity` to keep them
as strings. `write-string` and `write-file` emit flow collections.
Keyword values and keys are written as strings.

```clojure
(require '[norn.yaml :as yaml])

(yaml/read-string "server:\n  ports: [80, 443]\n  enabled: true\n")
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

Decimals are values with their own functions, not numbers to core
arithmetic: `+`, `<`, `==` and `number?` don't take them. This keeps one
numeric tower in core, with exact comparisons between its integers, ratios
and floats. Convert at the edges with `parse` and `text`, or with
`decimal` from an integer coefficient.

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
`from-bytes` views existing `Bytes` without copying. A buffer is a collection
to core: `count` and `nth` are constant time, and `seq`, `reduce`, `into`,
`vec` and the other sequence functions read its elements in order.
`norn.buffer/values` gives them as a vector, and `to-bytes` returns the
backing value.
Supported types are `:u8`, `:i8`, `:u16`, `:i16`, `:u32`, `:i32`, `:u64`,
`:i64`, `:f32`, and `:f64`. Byte order defaults to `:le`. Use `:order :be`
for big-endian data.

`norn.buffer/mutable` makes a buffer to update in place, such as pixels or
audio samples: `(mutable :u8 (* 4 width height))` holds zeros, and
`mutable-copy` starts from another buffer's elements. `set!`, `fill!` and
`copy!` (from a buffer of the same type and byte order, as one block, or from
values) write it, and `count`, `nth` and the sequence functions read it. Like
any `(:mutable)` object, it belongs to the fiber that made it. Other fibers
get an `OwnershipError` until it is handed on with `release!` or a channel
send. `freeze!` turns it into an immutable buffer without copying, after
which the mutable buffer can't be used. `snapshot` copies its elements
instead, and writing carries on.

```clojure
(require '[norn.buffer :as buf])

(let [pixels (buf/mutable :u8 (* 4 2 2))]
  (buf/fill! pixels 255)
  (buf/set! pixels 0 0)
  (buf/to-bytes (buf/freeze! pixels))) ; Bytes, ready for a canvas :pixels command
```

## I/O and the system

| Namespace | Use |
| --- | --- |
| `norn.io` | Read and write whole files or streams, and work with paths. |
| `norn.fs` | Walk directories, apply glob patterns, and watch for changes. |
| `norn.sys` | Read process arguments, environment variables, and system information. |
| `norn.cli` | Parse command lines: typed options, arguments, subcommands, and generated help. |
| `norn.terminal` | Detect the terminal, read keys in raw mode, and style output with ANSI colors. |
| `norn.process` | Run operating-system processes and work with pipes or pseudo-terminals. |
| `norn.signal` | Receive operating-system signals through channels. |
| `norn.net` | Work with TCP, UDP, and TLS. |
| `norn.http` | Send HTTP/1.1 requests, stream response bodies, send and parse multipart forms, and serve request maps with functions. |
| `norn.uri` | Parse, build, and percent-encode URIs. |
| `norn.sqlite` | Open SQLite databases, bind query parameters, and run transactions. |
| `norn.ffi` | Call C functions and describe native memory and data types. Pass `:main-thread true` for APIs that must run on the process's first thread, such as windowing and graphics. |
| `norn.log` | Emit leveled, structured logs. |
| `norn.time` | Work with instants, durations, calendar dates, named time zones, and calendar arithmetic. |
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
handle when the body exits, including on failure. It takes several
bindings, `[in (open a :read) out (open b :write)]`, and closes them in
reverse order. `close` does nothing given nil. `eprint` and `eprintln`
print to standard error, as `print` and `println` print to standard output.

`norn.process/start` returns the process handle and its piped streams.
`norn.process/close` closes them all; call `wait` first. `sh` runs a program
to completion and accepts `:in`, `:dir`, `:env`, and `:clear-env`. A program
that doesn't exist signals `FileNotFound`.

On streams, `read-exactly` reads a fixed number of bytes: it gives nil at the
end, and signals `IOError` with `:reason :truncated` when the stream stops
partway. `copy-stream!` copies one stream to another, up to `:limit` bytes
if given. `text-reader` and `read-chunk` decode UTF-8 or UTF-16 incrementally,
keeping characters whole across reads.

For files, `(open path :read-write)` creates a missing file and keeps an
existing file's contents. `(open path :create-new)` creates a file only if none exists,
signalling `FileExists` otherwise, and `sync!` flushes a handle to storage.
`temp-file` and `temp-directory` make new uniquely named paths, and
`with-temp-file` and `with-temp-directory` delete them when the body ends.
`write-atomic!` and `with-atomic-file` replace a file all at once. The new
contents go to a file beside it, which is flushed and then renamed over the
original, so readers see the old or the new and a crash leaves one of them.
The replacement doesn't keep the old file's permissions.

```clojure
(require '[norn.io :as io])

(io/write-atomic! "settings.edn" (pr-str settings))
(io/with-temp-directory [dir]
  (io/write-text (io/path dir "scratch.txt") "…")) ; dir is gone afterwards
```

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

`execute!` returns the number of rows the statement changed, which is 0 for
a statement that changes none, such as `CREATE TABLE`. `with-transaction`
and `transact` commit when the body returns and roll back when it doesn't:
an error, a warning or restart that unwinds past it, or the fiber being
cancelled. Inside another transaction they use a savepoint, which rolls back
alone. Errors are `DatabaseError`s, with `ConstraintError` and `BusyError`
among them; a value SQLite can't store, such as an integer beyond 64 bits,
is one too.

### HTTP bodies and forms

By default, `norn.http/request` reads the whole response body. With
`:as :stream`, the body is a `norn.io` stream that you read as the data
arrives. Every stream function accepts it, including `read`, `read-line`,
`copy-stream!`, and `text-reader`. A streamed body can also be the `:body`
of another request.

If you read the body to the end, the client returns its connection to the
pool for reuse. If you close the body first, the client closes the
connection. `with-response` closes the body when its form ends, however it
ends. `:timeout` covers the exchange only until the response headers arrive.

```clojure
(require '[norn.http :as http] '[norn.io :as io])

(http/with-response [r {:url "https://example.com/big.log"}]
  (let [lines (io/line-reader (:body r))]
    (loop [n 0]
      (if (io/read-line lines) (recur (inc n)) n))))
```

The `:multipart` option sends a `multipart/form-data` form. Each part has a
`:name` and either a `:value` or a `:file` path. A `:value` can be a string,
bytes, or a stream. Add `:filename` to make a part a file upload, and give
`:content-type` or `:headers` as needed. When every part is a string or
bytes, the client sends the body with a length. A file or stream part makes
the client stream the whole body. On a server, `multipart-params` returns a
request's parts in order as maps with `:name`, `:filename`, `:content-type`,
`:headers`, `:body`, and, for a part that isn't a file, `:value`.

```clojure
(http/post "https://example.com/upload" nil
           :multipart [{:name "title" :value "Holiday"}
                       {:name "photo" :file "beach.jpg" :content-type "image/jpeg"}])

(defn handler [req]
  (let [parts (http/multipart-params req)]
    {:status 200 :body (str (count parts) " parts")}))
```

Classes can act as streams too. Define methods on `norn.io/stream-read`,
`stream-write`, and `stream-close` for a class, and the `norn.io` functions
accept its instances.

### Time zones and calendar arithmetic

`norn.time/zone` returns a named IANA zone, such as `"Europe/Paris"`. Zone
data comes from the system's TZif files in `$TZDIR` or `/usr/share/zoneinfo`.
For times after a file's last listed transition, the file's POSIX TZ rule
applies. Each zone file is read once and kept. `zone` also accepts:

- `"+05:30"` or `"Z"`
- an offset in seconds
- `:utc`
- `:local`, which is the same zone as `system-zone`

`system-zone` reads `$TZ` (a zone name or a POSIX TZ string). Otherwise it
uses the zone that `/etc/localtime` names. A zone also works anywhere
`fields` and `format` accept a zone.

A `ZonedDateTime` holds an instant, its zone, its offset, and the wall-clock
`LocalDateTime` there. `in-zone` converts an instant. `zoned` converts a
local date-time, and the `:resolve` option controls how it handles a
wall-clock time that a clock change skips or repeats. The policies follow
JavaScript's Temporal:

- `:compatible` (the default) picks the later time for a skipped time and
  the earlier one for a repeated time.
- `:earlier` and `:later` pick that side in both cases.
- `:reject` signals a `TypeError`.

```clojure
(require '[norn.time :as t])

(def ny "America/New_York")
(t/format-zoned (t/zoned (t/parse-local-date-time "2026-03-08T02:30") ny))
; => "2026-03-08T03:30:00-04:00[America/New_York]"  (2:30 is skipped)
(t/format-zoned (t/zoned (t/parse-local-date-time "2026-11-01T01:30") ny :resolve :later))
; => "2026-11-01T01:30:00-05:00[America/New_York]"  (the second 1:30)

(let [z (t/zoned (t/parse-local-date-time "2026-03-07T09:00") ny)]
  [(t/format-zoned (t/shift z :days 1))     ; the same wall-clock time
   (t/format-zoned (t/shift z :hours 24))]) ; 24 hours later
; => ["2026-03-08T09:00:00-04:00[America/New_York]"
;     "2026-03-08T10:00:00-04:00[America/New_York]"]
```

`shift` moves a `LocalDate`, `LocalDateTime`, `ZonedDateTime`, or `Instant`
by `:years`, `:months`, `:weeks`, `:days`, `:hours`, `:minutes`, `:seconds`,
`:millis`, or `:nanos`. Negative amounts move backward. Years and months
follow the calendar. A day past the end of the target month becomes that
month's last day, so 2026-01-31 plus one month is 2026-02-28. A zoned time
keeps its wall-clock time across date units, then moves along the time line
for time units. Other calendar helpers are `days-between`, `months-between`,
`weekday`, `day-of-year`, `days-in-month`, `leap-year?`, `epoch-day`,
`from-epoch-day`, and `start-of-day`.

`format-zoned` writes RFC 9557 text such as
`2026-03-08T03:30:00-04:00[America/New_York]`, and `parse-zoned` reads it.

`today` and `local-now` give the date, and the date and time, on this
machine's clock (or in a zone given).

`format-local`, `format-zoned` and `format` take a strftime pattern, and
`parse-local-date`, `parse-local-time`, `parse-local-date-time` and
`parse-zoned` read one: `%Y %y %m %d %e %j %b %B %a %A %H %I %p %M %S %f %z
%Z %F %T %%`, with English names. Reading accepts fewer digits, either
case, short or full month names and any whitespace; a weekday must match
the date. `parse-zoned` with a pattern needs `%z`, and `%Z` is written
only. `(t/format-local (t/local-date 2026 11 3) "%a %d %b %Y")` is
`"Tue 03 Nov 2026"`, and `(t/parse-local-date "11/3/2026" "%m/%d/%Y")`
reads it back from a US-style date.
`before?`, `after?`, and `compare-time` compare dates, times, and zoned
date-times.

Text that isn't a date, time, or zone signals `ParseError`: `parse` and the
other parsing functions, and `zone` given a name it can't find. Values that
don't make a date or time signal `TypeError`, such as `(local-date 2026 2
30)` or a wall-clock time that `:reject` refuses.

`norn.schedule` reads five-field cron expressions and runs their times in
a zone (`:zone`, UTC by default). Across a clock change it follows Vixie
cron. A time that the change skips runs once, at the change. A time that it
repeats runs once, the first time. An expression whose minute or hour is
`*` follows the clock instead: it runs in both occurrences of a repeated
hour and not at all in a skipped one. Bad cron text signals `ParseError`.

### The terminal

`norn.terminal` works with the terminal the program runs in. Its main
functions are:

- `tty?` checks whether `:in`, `:out`, or `:err` is a terminal.
- `size` returns `{:rows :cols}`.
- `color-support` returns `:none`, `:basic`, `:256`, or `:truecolor`. It
  follows `NO_COLOR`, `FORCE_COLOR`, `COLORTERM`, and `TERM`.

`style` adds ANSI attributes and colors to text, and nested styles work. It
converts a color to what the terminal can show and returns plain text when
color is off. Bind `*color*` to override that decision. `strip` removes
escape sequences, and `width` counts the columns that text takes on screen.
`norn.string/width`, `pad-left`, `pad-right`, and `center` also ignore
escape sequences, so styled text lines up.
`cursor-to`, `clear`, `hide-cursor`, and `alternate-screen` produce control
sequences for you to print.

In raw mode, keys arrive as they are typed, with no echo, and Ctrl-C and
Ctrl-Z are keys instead of signals. `with-raw-mode` turns raw mode on and
restores the terminal afterward, even when its body fails. Its options can
also turn on the alternate screen, a hidden cursor, mouse reports, bracketed
paste, and the kitty keyboard protocol for the body's duration.

The `:keyboard` option turns on the kitty keyboard protocol, which kitty,
WezTerm, foot, Ghostty, and recent iTerm2 support. Other terminals ignore
it. With the protocol, Escape arrives without the 50 ms wait, and
combinations that look the same in a plain terminal arrive as different
keys. For example, Ctrl-I is `{:key \i :ctrl true}` instead of Tab. Print
`keyboard-query` to ask whether the terminal supports the protocol. A
terminal that does answers with the key `{:key :keyboard-flags :flags n}`.

To suspend the program when the user presses Ctrl-Z, call `suspend!`. It
turns off the options and raw mode so that the shell works normally. When
the shell resumes the program with `fg`, `suspend!` turns them back on and
returns, and the program should then redraw the screen. If no shell with
job control can resume the program, `suspend!` doesn't stop it.

`read-key` returns keys as maps:

- `{:key \a}` for a typed character
- `{:key :up :ctrl true}` for a special key with modifiers
- `{:key :f5}` for a function key
- `{:key :paste :text "…"}` for a bracketed paste
- `{:key :mouse :button :left :action :press :row 3 :col 10}` for a mouse
  report

`read-key` decodes xterm sequences, including kitty's `CSI u` keys. An
Escape with nothing after it within 50 ms is the Escape key. `key-name`
returns names such as `"ctrl+shift+left"`. `decode-keys` decodes bytes
without a terminal.

```clojure
(require '[norn.terminal :as t])

(t/with-raw-mode {:hide-cursor true}
  (println (t/style "Press keys; q quits." :bold))
  (loop []
    (let [k (t/read-key)]
      (println (t/style (t/key-name k) :cyan))
      (when-not (= k {:key \q}) (recur)))))
```

To handle resizing, listen for `:winch` with `norn.signal/listen` and call
`size` again. To run another program on a terminal of its own, use
`norn.process/start-pty`.

### Command-line programs

`norn.cli` parses a command line against a spec, which is a map that
describes the program's options, positional arguments, and subcommands.
`run` parses `norn.sys/program-args`, the arguments after the script's path
or the image. Then it does one of three things:

- For `--help`, it prints the generated help and returns 0.
- For a bad command line, it writes the error to stderr and returns 2.
- Otherwise, it calls the matching command's `:run` with the parsed map and
  returns the exit status.

```clojure
(ns greet (:require [norn.cli :as cli] [norn.string :as str] [norn.sys :as sys]))

(def spec
  {:name "greet" :summary "Say hello." :version "1.0"
   :options [{:name :count :short \c :type :int :default 1 :doc "How many times"}
             {:name :shout :short \s :doc "Upper case"}]
   :args [{:name :who :optional true :default "world"}]
   :run (fn [{:keys [options args]}]
          (dotimes [_ (:count options)]
            (println (cond-> (str "Hello, " (:who args) "!") (:shout options) str/upper-case))))})

(sys/exit (cli/run spec))
```

`norn greet.norn -sc 2 Ada` prints `HELLO, ADA!` twice. `norn greet.norn
--cuont 2` reports `greet: unknown option "--cuont" (did you mean
"--count"?)` and exits with status 2. For a program built with `norn build`,
the entry function receives the arguments. Write it as
`(defn main [& args] (cli/run spec args))` and its result becomes the exit
status.

These keys describe an option:

- `:name` is the option's keyword in `:options`.
- `:long` is the long form. It defaults to the name, and `false` removes it.
- `:short` is a single-character form.
- `:value` is the placeholder for an option that takes a value.
- `:type` converts the value. Use `:string`, `:int`, `:float`, `:keyword`,
  `:boolean`, or a function. A function can signal a condition to reject the
  value.
- `:choices` lists the allowed values.
- `:default` supplies the value when the option is not given.
- `:env` names an environment variable to read when the option is not given.
- `:required` makes the option mandatory.
- `:multiple` collects every occurrence in a vector.
- `:count` counts occurrences, as in `-vvv`.
- `:negatable` adds a `--no-name` form for a flag.
- `:doc` is the help text.

An option without `:value`, `:type`, `:choices`, or `:multiple` is a flag.
When a flag is not given, its value is `false`. A `:count` option is 0 and a
`:multiple` option is `[]`.

An argument in `:args` has a `:name`. It can also be `:optional` with a
`:default`, or the last argument can be `:variadic` and collect the rest.
Arguments accept `:type` and `:choices`. `:commands` holds subcommand specs,
which can be nested. A subcommand accepts its parents' options too.

The parser follows these rules:

- Options can come after arguments unless the spec sets `:options-first`.
- `--` ends the options.
- `-abc` combines flags, and `-ofile` attaches a value to a short option.
- A negative number such as `-5` is an argument.

`cli/parse` returns the parsed map without acting on it and signals a
`norn.cli/UsageError` for a bad command line. `cli/usage` returns the help
text.

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
| `norn.property` | Property-based tests: generators, shrinking, and seeds that reproduce failures. |
| `norn.bench` | Measure code execution. |
| `norn.fmt`, `norn.lint`, `norn.check` | Format, lint, and check source. |
| `norn.doc`, `norn.expand` | Inspect documentation or trace macro expansion. |
| `norn.deps` | Read project manifests and resolve dependencies. |
| `norn.image` | Inspect and compare saved images. |

See [command-line tools](../tooling.md) for the corresponding shell commands.
