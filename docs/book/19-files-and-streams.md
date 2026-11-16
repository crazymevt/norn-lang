# 19. Files and streams

[Contents](README.md) · Previous: [18. Actors and supervision](18-actors-and-supervision.md) · Next: [20. Data formats](20-data-formats.md) <!-- nav -->

Most programs read something from disk, write something back, or both. This
chapter covers `norn.io`, the library for files, paths, and streams, and
`norn.fs`, which walks directory trees, matches glob patterns, and watches
for changes. It starts with whole files, the case you meet most often, then
moves to paths and the file system, errors, handles and streams, temporary
files, and atomic replacement. It ends with a small program that summarizes
a folder of log files.

Every example here creates the files it reads, in the current directory.
Try them in a scratch folder.

## Whole files

Load the library first. The examples in this chapter call it `io`.

```clojure
(require '[norn.io :as io] '[norn.fs :as fs])
```

When a file fits comfortably in memory, read and write it in one call.
`write-text` replaces a file's contents with a string, creating the file if
it doesn't exist. `read-text` gives the contents back.

```clojure
(io/write-text "notes.txt" "milk\neggs\n") ; => nil
(io/read-text "notes.txt") ; => "milk\neggs\n"
```

Text is always UTF-8, on every operating system, and line endings stay as
they are in the file: Norn never translates `\r\n` to `\n` behind your back.
When you want the lines themselves, `lines` splits at either ending and
leaves out the terminators.

```clojure
(io/append-text "notes.txt" "bread\r\n") ; => nil
(io/lines "notes.txt") ; => ["milk" "eggs" "bread"]
```

`append-text` adds to the end of a file, creating it if needed. It's the
simplest way to keep a log.

Binary files work the same way with `read-bytes` and `write-bytes`. A
`Bytes` value is an immutable byte string. It prints as hex, `count` and
`nth` work on it, and `vec` turns it into a vector of integers from 0 to 255.

```clojure
(io/write-bytes "header.bin" (bytes [137 80 78 71])) ; => nil
(io/read-bytes "header.bin") ; => #bytes "89504e47"
(vec (io/read-bytes "header.bin")) ; => [137 80 78 71]
```

`(bytes "text")` gives a string's UTF-8 bytes, and `bytes->string` decodes
them again. [Chapter 20](20-data-formats.md) covers `norn.bytes`, which has
hex, Base64, and the integer and float encodings that binary formats need.

Core also has `slurp` and `spit`, familiar from Clojure. `slurp` reads a
file as text. `spit` writes `Bytes` as they are and anything else as its
`str`, so it suits quick experiments. For appending, and for code you mean
to keep, use the `norn.io` functions: their names say what they do.

### A file that isn't text

`read-text` checks that the file really is UTF-8. If it isn't, you get a
`ParseError` rather than a string with garbage in it.

```clojure
(io/write-bytes "latin1.txt" (bytes [99 97 102 233])) ; "café" in Latin-1
(io/read-text "latin1.txt") ; !! ParseError
```

When you'd rather have the text with replacement characters, read the bytes
and decode them leniently. `bytes->string` takes a second argument, `true`,
for that.

```clojure
(bytes->string (io/read-bytes "latin1.txt") true) ; => "caf�"
```

## Paths

A path in Norn is a string. There's no path class to convert to and from:
every function that takes a path takes a string, and the path functions
give strings back. `path` joins parts with the platform's separator.

```clojure
(io/path "reports" "2026" "march.csv") ; => "reports/2026/march.csv"
```

The other path functions take a path apart. None of them touch the disk.

```clojure
(io/file-name "reports/2026/march.csv") ; => "march.csv"
(io/parent "reports/2026/march.csv") ; => "reports/2026"
(io/extension "reports/2026/march.csv") ; => "csv"
(io/stem "reports/2026/march.csv") ; => "march"
(io/extension "archive.tar.gz") ; => "gz"
(io/extension "README") ; => nil
(io/parent "march.csv") ; => nil
```

Only the last dot counts for `extension` and `stem`, so `archive.tar.gz` has
the extension `gz` and the stem `archive.tar`.

`absolute` makes a path absolute against the current directory, without
checking that anything is there. `canonical` resolves `.`, `..`, and
symbolic links against the real file system, so it gives nil for a path
that doesn't exist.

```clojure
(= (io/absolute "notes.txt") (io/path (norn.sys/cwd) "notes.txt")) ; => true
(io/canonical "no/such/file") ; => nil
```

`canonical` matters when you compare paths. On macOS, for example, the
temporary directory lives under `/var`, which is a link to `/private/var`.
Two paths can name the same file and still be different strings. The
file-system events in the last part of this chapter report canonical paths.

## The file system

`info` describes what's at a path, or gives nil if nothing is there.

```clojure
(let [i (io/info "notes.txt")]
  [(:type i) (:size i) (:readonly i)]) ; => [:file 17 false]
(io/info "nothing-here") ; => nil
```

The map also has `:modified`, the last modification time in milliseconds
since the epoch. `:type` is `:file`, `:dir`, `:symlink`, or `:other`. `info`
reports a symbolic link as a link. The predicates `file?` and `directory?`
follow links to what they point at, which is usually what you want, and
`exists?` asks whether there's anything at the path at all.

```clojure
[(io/exists? "notes.txt") (io/file? "notes.txt") (io/directory? "notes.txt")]
; => [true true false]
```

To make directories, use `mkdirs!`, which creates any missing parents and
does nothing if the directory exists already. `list-dir` gives the names in
a directory, sorted. They're names, not paths: join them to the directory
with `path` when you need to open them.

```clojure
(io/mkdirs! "drafts/old") ; => nil
(io/write-text (io/path "drafts" "intro.md") "# Intro")
(io/write-text (io/path "drafts" "outline.md") "# Outline")
(io/list-dir "drafts") ; => ["intro.md" "old" "outline.md"]
```

`copy!` copies a file and returns the number of bytes it copied. `rename!`
moves a file or directory. `delete!` removes a file or an empty directory.
With `:recursive true`, it removes a directory and everything in it, so
take care with what you pass it.

```clojure
(io/copy! "drafts/intro.md" "drafts/old/intro-v1.md") ; => 7
(io/rename! "drafts/outline.md" "drafts/plan.md") ; => nil
(io/list-dir "drafts") ; => ["intro.md" "old" "plan.md"]
(io/delete! "drafts/old") ; !! IOError
(io/delete! "drafts" :recursive true) ; => nil
(io/exists? "drafts") ; => false
```

The first `delete!` fails because `drafts/old` isn't empty. Deleting a tree
is never the default.

## When things go wrong

A file operation can fail for reasons your program doesn't control: the
file isn't there, the disk is full, someone else holds the permissions.
Norn reports these as conditions, the subject of
[Chapter 11](11-conditions-and-restarts.md). They all descend from
`IOError`:

- `FileNotFound` when a path doesn't exist,
- `PermissionDenied` when the operating system says no,
- `FileExists` when you asked for a file that must be new,
- and `IOError` itself for everything else.

```clojure
(io/read-text "missing.txt") ; !! FileNotFound
(io/read-text "missing.txt") ; !! IOError
```

Both checks pass: a `FileNotFound` is an `IOError`, so a handler for
`IOError` catches all of them. The condition carries what went wrong as
data. `ex-data` gives its `:path` and `:reason`, so a handler can decide
what to do without parsing the message.

```clojure
(handler-case (io/read-text "missing.txt")
  (IOError [e] (ex-data e)))
; => {:path "missing.txt" :reason :not-found}

(handler-case (io/read-text "missing.txt")
  (IOError [e] (condition-message e)))
; => "Can't read missing.txt: No such file or directory (os error 2)"
```

The message includes the operating system's own words, which vary between
systems. Match on the class or on `:reason` (for files, `:not-found`,
`:permission-denied`, `:exists`, or `:other`), not on the text.

The usual pattern for an optional file is to catch `FileNotFound` and supply
a default. Catch the narrow class: a `PermissionDenied` on a settings file
is a real problem, and turning it into "no settings" hides it.

```clojure
(defn read-settings [path]
  (handler-case (read-string (io/read-text path))
    (FileNotFound [_] {})))

(read-settings "settings.edn") ; => {}
(io/write-text "settings.edn" "{:theme :dark}")
(read-settings "settings.edn") ; => {:theme :dark}
```

### Restarts on file errors

File errors also come with two restarts, `use-value` and `retry`, like the
runtime errors in Chapter 11. A handler that runs without unwinding,
installed with `handler-bind`, can pick one. `use-value` makes the failed
call return a value of your choice:

```clojure
(handler-bind [FileNotFound (fn [e] (invoke-restart 'use-value ""))]
  (count (io/read-text "missing.txt")))
; => 0
```

`retry` runs the operation again. A handler might create the missing
directory and then retry the write:

```clojure
(handler-bind [FileNotFound (fn [e]
                              (io/mkdirs! (io/parent (:path (ex-data e))))
                              (invoke-restart 'retry))]
  (io/write-text "out/today/summary.txt" "all good"))
(io/read-text "out/today/summary.txt") ; => "all good"
```

The difference from `handler-case` is where the decision happens. The
handler runs at the point of failure, inside `write-text`, and the call
carries on. At the REPL, the same restarts appear in the debugger when a
file error goes unhandled ([Chapter 12](12-live-programming.md)), so you can
fix the problem and continue instead of starting over.

## Handles and with-open

Whole-file functions are enough until a file is too big to hold, or you want
to write it a piece at a time, or the data comes from somewhere that isn't
a file at all. For those cases, open a *handle*: a stream you read from or
write to and then close.

`open` takes a path and a mode:

| Mode | Opens for | If the file exists | If it doesn't |
| --- | --- | --- | --- |
| `:read` | reading | reads it | `FileNotFound` |
| `:write` | writing | empties it | creates it |
| `:append` | writing at the end | keeps it | creates it |
| `:read-write` | both, from the start | keeps it | creates it |
| `:create-new` | writing | `FileExists` | creates it |

`write` takes a string (written as UTF-8) or `Bytes`. `close` closes the
handle. Closing one twice is harmless, and closing nil does nothing.

A handle holds an operating-system resource, so close it when you're done,
even when something fails partway. `with-open` does that for you: it binds
a name to the handle, runs the body, and closes the handle however the body
ends, by returning or by a condition.

```clojure
(io/with-open [h (io/open "journal.txt" :write)]
  (io/write h "Monday: started the book\n")
  (io/write h "Tuesday: read about files\n"))
(io/lines "journal.txt")
; => ["Monday: started the book" "Tuesday: read about files"]
```

`with-open` returns the body's value. Using a handle after it's closed is an
error, so it can't be read by accident later.

```clojure
(def kept (io/with-open [h (io/open "journal.txt" :read)] h))
(io/read kept 10) ; !! RuntimeError
```

`with-open` takes several bindings, as Clojure's does. Say you want to copy
from one handle to another. Each binding can use the ones before it, and
the handles close in reverse order, the last opened first. If opening one
fails, the ones already open are closed.

```clojure
(io/with-open [in (io/open "journal.txt" :read)
               out (io/open "journal-copy.txt" :write)]
  (io/copy-stream! in out))
; => 51
```

`copy-stream!` copies everything from one stream to another and returns the
number of bytes. With `:limit n`, it stops after `n` bytes.

If you forget to close a handle, Norn closes it eventually, once the handle
becomes unreachable and the garbage collector notices. Don't lean on that.
It happens at an unpredictable time, and a process that opens files in a
loop can run out of descriptors long before then.

Files are read and written on a pool of threads set aside for blocking
work, so a fiber waiting on the disk never holds up the other fibers
([Chapter 15](15-fibers-and-scopes.md)). You don't have to do anything to get
this: every `norn.io` call works this way.

## Reading streams

A handle gives you bytes. `read` takes the most you want and returns up to
that many, or nil at the end of the stream. A loop that reads until nil
sees the whole file, in pieces of whatever size the stream delivers.

```clojure
(io/write-text "alphabet.txt" "abcdefghijklmnopqrstuvwxyz")

(io/with-open [h (io/open "alphabet.txt" :read)]
  (loop [pieces []]
    (if-let [b (io/read h 10)]
      (recur (conj pieces (bytes->string b)))
      pieces)))
; => ["abcdefghij" "klmnopqrst" "uvwxyz"]
```

`read` may return fewer bytes than you asked for, even before the end. A
file usually gives you the full amount; a pipe or a socket gives you what
has arrived. When a format needs an exact number of bytes, a fixed-size
header for instance, use `read-exactly`. It keeps reading until it has them
all, gives nil if the stream is already at its end, and signals an
`IOError` with `:reason :truncated` if the stream ends partway.

```clojure
(io/with-open [h (io/open "alphabet.txt" :read)]
  [(bytes->string (io/read-exactly h 4))
   (bytes->string (io/read-exactly h 22))
   (io/read-exactly h 4)])
; => ["abcd" "efghijklmnopqrstuvwxyz" nil]

(io/with-open [h (io/open "alphabet.txt" :read)]
  (io/read-exactly h 100))
; !! IOError
```

`read-all` reads whatever is left in a stream into one `Bytes` value.

### Lines

Most text is processed a line at a time. A *line reader* wraps a stream and
gives you one line per `read-line`, without the line ending, and nil at the
end. It accepts `\n` and `\r\n`, and a last line without a newline still
counts.

```clojure
(io/write-text "todo.txt" "write tests\r\nfix the parser\nship it")

(io/with-open [r (io/line-reader (io/open "todo.txt" :read))]
  (loop [n 1 out []]
    (if-let [line (io/read-line r)]
      (recur (inc n) (conj out (str n ". " line)))
      out)))
; => ["1. write tests" "2. fix the parser" "3. ship it"]
```

A line reader is a stream itself, and closing it closes the stream inside,
so you can open the file right in the `with-open` binding as above. Memory
use stays flat however long the file is: the reader holds only the part it
has read past the last line.

With no argument, `read-line` reads a line from standard input. That's how
a script reads what the user types, or what another program pipes into it.

<!-- skip -->
```clojure
(print "Name? ")
(println (str "Hello, " (io/read-line)))
```

### Text in pieces

Sometimes you want text without line structure, for example to feed a
parser from a large file. Reading bytes and decoding each piece separately
goes wrong when a multi-byte character straddles two reads. A *text reader*
handles that: `read-chunk` returns the next piece of decoded text, holding
back a partial character until the rest of it arrives.

```clojure
(io/write-text "greeting.txt" "héllo, wörld")

(io/with-open [h (io/open "greeting.txt" :read)]
  (let [r (io/text-reader h)]
    (loop [out ""]
      (if-let [chunk (io/read-chunk r)]
        (recur (str out chunk))
        out))))
; => "héllo, wörld"
```

`text-reader` decodes UTF-8 by default. Pass `:encoding :utf-16le` or
`:encoding :utf-16be` for UTF-16 files. A byte-order mark at the start
is kept as a character, so the code that reads the format decides what it
means.

### Streams beyond files

The functions in this section work on any stream, not only on files. A
child process's standard output, a TCP socket, and an HTTP response body
are all streams, and `read`, `read-line`, `copy-stream!`, and the rest work
on them the same way. [Chapter 21](21-the-operating-system.md) uses this
with process pipes, and [Chapter 22](22-networking-and-http.md) with sockets.

You can make your own class a stream too. `norn.io` defines three generic
functions, `stream-read`, `stream-write`, and `stream-close`. Give your class
methods for them ([Chapter 9](09-classes-and-generic-functions.md)), and
every stream function takes its instances. Here is a stream that produces
the same text over and over, up to a total size, the kind of thing you
might write for a test:

```clojure
(defclass Repeating () ((text :required true) (left :required true)))

(defmethod io/stream-read ((s Repeating) n)
  (let [left @(:left s)]
    (when (pos? left)
      (let [b (bytes (:text s))
            k (min n left (count b))]
        (swap! (:left s) - k)
        (subbytes b 0 k)))))

(def source (Repeating :text "la" :left (atom 9)))

(io/with-open [r (io/line-reader source)]
  (io/read-line r))
; => "lalalalal"
```

`stream-read` returns up to `n` bytes, or nil at the end. The left count is
in an atom because instances are values ([Chapter 9](09-classes-and-generic-functions.md)),
and a stream has state that changes. `with-open` worked with no
`stream-close` method: the default does nothing.

For text or bytes you already have, `bytes-stream` makes a stream of them,
so code written for streams can take them too:

```clojure
(let [r (io/line-reader (io/bytes-stream "first\nsecond\n"))]
  [(io/read-line r) (io/read-line r)]) ; => ["first" "second"]
```

## Temporary files

A program often needs a scratch file: somewhere to download to before
checking the result, or a test fixture. `temp-file` creates a new, empty
file with a name nothing else has, in the system's temporary directory, and
returns its path. `temp-directory` does the same for a directory.

```clojure
(let [p (io/temp-file :prefix "export-" :suffix ".csv")]
  (try
    [(io/file? p) (io/extension p)]
    (finally (io/delete! p))))
; => [true "csv"]
```

You have to delete those yourself. The `with-` forms do it for you, when
the body ends, however it ends. `with-temp-directory` deletes the directory
and everything in it.

```clojure
(def seen (atom nil))

(io/with-temp-directory [dir]
  (reset! seen dir)
  (io/write-text (io/path dir "a.txt") "scratch")
  (io/list-dir dir))
; => ["a.txt"]

(io/exists? @seen) ; => false
```

Both take the same options: `:prefix`, `:suffix`, and `:dir` to create the
file or directory somewhere other than the system's temporary directory.

## Replacing a file safely

`write-text` replaces a file's contents, but not all at once. It empties
the file and then writes. If the program crashes in between, or another
process reads the file at the wrong moment, it sees a half-written file.
For a settings file or a database export, that can be worse than no file.

`write-atomic!` avoids it. It writes the new contents to a new file beside
the old one, flushes it to storage, and renames it over the original. A
rename within a directory is atomic, so a reader sees the old contents or
the new, never a mix, and a crash leaves one of them in place.

```clojure
(io/write-atomic! "settings.edn" (pr-str {:theme :light :font-size 14})) ; => nil
(read-settings "settings.edn") ; => {:theme :light :font-size 14}
```

For contents you write in pieces, `with-atomic-file` gives you a handle to
the new file and does the swap when the body returns. If the body fails,
the original file is left as it was and the partial file is removed.

```clojure
(io/with-atomic-file [h "settings.edn"]
  (io/write h "{:theme ")
  (error "the disk is full"))
; !! Error

(read-settings "settings.edn") ; => {:theme :light :font-size 14}
(filter #(= "tmp" (io/extension %)) (io/list-dir ".")) ; => []
```

The last line checks that no temporary file was left beside `settings.edn`.

The new file is a new file, so it doesn't keep the old one's permissions.
If those matter, set them again after the replacement.

Two related tools: opening with `:create-new` creates a file only if no
file is there, signalling `FileExists` otherwise. It's the building block for
a lock file or for "never overwrite" output. And `sync!` flushes everything
written to a handle to storage, for when you need a write to survive a
power cut before you carry on.

```clojure
(io/with-open [h (io/open "run.lock" :create-new)]
  (io/write h "pid 4242"))
(io/open "run.lock" :create-new) ; !! FileExists
```

## Walking, globbing, and watching

`norn.fs` works with directory trees. `walk` gives every path under a
directory, files and directories both, depth first and sorted. It doesn't
follow symbolic links, so a link loop can't send it in circles.

```clojure
(doseq [p ["site/index.md" "site/about.md"
           "site/posts/2026/spring.md" "site/posts/2026/cover.png"
           "site/posts/draft.txt"]]
  (io/mkdirs! (io/parent p))
  (io/write-text p (str "contents of " p)))

(fs/walk "site/posts")
; => ["site/posts/2026" "site/posts/2026/cover.png"
;     "site/posts/2026/spring.md" "site/posts/draft.txt"]
```

Since the result is a vector of strings, the rest is ordinary sequence work
([Chapter 6](06-sequences-and-transducers.md)). The total size of the files
in a tree:

```clojure
(->> (fs/walk "site")
     (filter io/file?)
     (map (comp :size io/info))
     (reduce +))
; => 156
```

`glob` matches paths against a pattern. `*` matches any run of characters
within one path segment, `?` matches one character, and `**` matches any
number of segments, including none. Segments are separated by `/` on every
operating system, so the same pattern works everywhere.

```clojure
(fs/glob "site/*.md") ; => ["site/about.md" "site/index.md"]
(fs/glob "site/**/*.md")
; => ["site/about.md" "site/index.md" "site/posts/2026/spring.md"]
(fs/glob "site/**/*.png") ; => ["site/posts/2026/cover.png"]
```

Because `**` can match zero segments, `site/**/*.md` finds the Markdown
files at the top of `site` too. The part of the pattern before the first
wildcard is the directory `glob` walks. If that directory doesn't exist,
nothing matches, and `glob` returns an empty vector.

```clojure
(fs/glob "nowhere/**/*.md") ; => []
```

### Watching for changes

`watch` asks the operating system to report changes to a file or
directory. With `:recursive true`, it covers everything under a directory.
It returns a map holding a handle and a channel, `:events`, that receives a
map for each change: `{:kind … :paths […]}`, where `:kind` is `:create`,
`:modify`, `:remove`, or `:other`.

Events arrive on a channel, so you read them the way you read any channel
([Chapter 16](16-channels.md)), here with a timeout so the example can't
hang:

```clojure
(io/mkdirs! "inbox")
(def w (fs/watch "inbox"))
(sleep 200)
(io/write-text "inbox/order-7.json" "{}")

(defn wait-for-file
  "The first event on watch w that mentions a file called name, or
  :timed-out after five seconds."
  [w name]
  (let [deadline (timeout 5000)]
    (loop []
      (let [[ev ch] (alts! [(:events w) deadline])]
        (cond (= ch deadline) :timed-out
              (nil? ev) :closed
              (some #(= name (io/file-name %)) (:paths ev)) ev
              :else (recur))))))

(def ev (wait-for-file w "order-7.json"))
(contains? #{:create :modify} (:kind ev)) ; => true
(io/file-name (first (:paths ev))) ; => "order-7.json"
(fs/unwatch w)
```

Three habits make watching reliable. First, filter: the operating system
may report events you didn't expect, such as one for the directory itself,
and different systems describe the same change differently (a new file may
arrive as `:create`, `:modify`, or both). Treat an event as "something
changed here" and look at the file to find out what. Second, compare paths
with `canonical`: events carry canonical paths, which may not be the
strings you passed in. Third, call `unwatch` when you're done. It releases
the operating-system watch and closes the channel.

The short `sleep` after `watch` gives the operating system a moment to
start watching. A real program starts its watch once, at startup, and the
pause doesn't matter.

## A worked example: summarizing logs

Here is a small program that uses most of this chapter. A service writes
one log file per day into a folder, with lines like
`2026-03-02T09:00:01 ERROR payment declined`. We want a count of each level
across all the files, written to a report file that a dashboard reads.

First, some logs to work on:

```clojure
(io/mkdirs! "logs/archive")
(io/write-text "logs/2026-03-01.log"
               (str "2026-03-01T08:00:00 INFO started\n"
                    "2026-03-01T08:05:12 WARN slow response\n"
                    "2026-03-01T09:00:01 ERROR payment declined\n"))
(io/write-text "logs/2026-03-02.log"
               (str "2026-03-02T08:00:00 INFO started\r\n"
                    "2026-03-02T08:30:00 INFO checkpoint\r\n"))
(io/write-text "logs/archive/2026-02-28.log"
               "2026-02-28T23:59:59 ERROR disk full\n")
(io/write-text "logs/README" "not a log file")
```

Reading each file a line at a time keeps memory flat however large the logs
get. The level is the second word of a line; anything that doesn't look
like a log line is skipped.

```clojure
(require '[norn.string :as str])

(defn line-level [line]
  (let [words (str/split line #" ")]
    (when (>= (count words) 3) (keyword (str/lower-case (nth words 1))))))

(line-level "2026-03-01T08:00:00 INFO started") ; => :info
(line-level "garbage") ; => nil

(defn count-levels
  "Add the levels of the lines in file path to counts, a map."
  [counts path]
  (io/with-open [r (io/line-reader (io/open path :read))]
    (loop [counts counts]
      (if-let [line (io/read-line r)]
        (recur (if-let [level (line-level line)]
                 (update counts level (fnil inc 0))
                 counts))
        counts))))

(count-levels {} "logs/2026-03-01.log") ; => {:info 1 :warn 1 :error 1}
```

`glob` finds the log files, including the archived one, and `reduce`
folds each file into the running counts. A file that disappears between
the glob and the read, because log rotation moved it, shouldn't stop the
report, so `FileNotFound` skips it. Other errors still stop it.

```clojure
(defn summarize [dir]
  (reduce (fn [counts path]
            (handler-case (count-levels counts path)
              (FileNotFound [_] counts)))
          {}
          (fs/glob (str dir "/**/*.log"))))

(summarize "logs") ; => {:info 3 :warn 1 :error 2}
```

Finally, the report. The dashboard reads the file at any moment, so it's
replaced atomically, and the report lists the levels in a fixed order.

```clojure
(defn write-report! [dir out]
  (let [counts (summarize dir)]
    (io/with-atomic-file [h out]
      (doseq [level [:error :warn :info]]
        (io/write h (str (name level) " " (get counts level 0) "\n"))))
    counts))

(write-report! "logs" "report.txt") ; => {:info 3 :warn 1 :error 2}
(io/lines "report.txt") ; => ["error 2" "warn 1" "info 3"]
```

To keep the report current, run `write-report!` whenever the folder
changes, from a fiber reading a recursive watch on `logs`. Many events can
arrive for one change, so a good loop waits until the events stop for a
moment before writing a new report.

## Summary

- `read-text`, `write-text`, `append-text`, `lines`, `read-bytes`, and
  `write-bytes` handle whole files. Text is UTF-8, line endings are kept,
  and invalid UTF-8 is a `ParseError`.
- Paths are strings. `path` joins them; `parent`, `file-name`, `extension`,
  and `stem` take them apart without touching the disk; `canonical`
  resolves links against the real file system.
- File errors are `IOError`s (`FileNotFound`, `PermissionDenied`,
  `FileExists`) with `:path` and `:reason` in `ex-data`, and offer
  `use-value` and `retry` restarts.
- `open` gives a handle; `with-open` closes it however the body ends. It
  takes several bindings and closes them in reverse order.
- `read`, `read-exactly`, `read-all`, `line-reader` with `read-line`, and
  `text-reader` with `read-chunk` work on any stream: files, pipes,
  sockets, and your own classes with `stream-read` methods.
- `with-temp-file` and `with-temp-directory` clean up after themselves.
  `write-atomic!` and `with-atomic-file` replace a file so readers never
  see half of it.
- `norn.fs` walks trees with `walk`, matches `glob` patterns with `*`, `?`,
  and `**`, and reports changes on a channel with `watch`.

---

[Contents](README.md) · Previous: [18. Actors and supervision](18-actors-and-supervision.md) · Next: [20. Data formats](20-data-formats.md) <!-- nav -->
