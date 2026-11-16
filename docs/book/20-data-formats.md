# 20. Data formats

[Contents](README.md) · Previous: [19. Files and streams](19-files-and-streams.md) · Next: [21. The operating system](21-the-operating-system.md) <!-- nav -->

Programs trade data with files, services, and other programs, and each
partner has a format it prefers. Norn's standard library reads and writes
the common ones, and in each case the result is ordinary Norn data: maps,
vectors, strings, numbers. This chapter is organized by task rather than by
library: configuration files, data interchange, binary data, compression
and archives, money and other exact decimals, and finally identifiers and
hashing. Each section shows one library at work on a real example. For the
full list of a library's functions and options, use `norn doc` on its
namespace and see the [library reference](../reference/libraries.md).

Reading and writing files is [Chapter 19](19-files-and-streams.md)'s
subject. Most format libraries here work on strings or `Bytes`, so you
combine them with `norn.io`; a few also have `read-file` and `write-file`
helpers.

Load what the chapter uses:

```clojure
(require '[norn.io :as io]
         '[norn.string :as str]
         '[norn.toml :as toml]
         '[norn.yaml :as yaml]
         '[norn.edn :as edn]
         '[norn.json :as json]
         '[norn.ndjson :as ndjson]
         '[norn.csv :as csv]
         '[norn.xml :as xml])
```

## Configuration files

A configuration file is written by people and read by programs. It should
be pleasant to edit, and reading it should never run code. Norn reads three
formats for this: TOML, YAML, and EDN.

### TOML

TOML is a good default for a new program's settings. It's simple to write,
its types are unambiguous, and the format has one way to say most things.
`norn.toml/read-string` gives a map, with keys as keywords by default.
Tables (`[server]`) become nested maps, and arrays of tables (`[[users]]`)
become vectors of maps.

```clojure
(def shop-toml "
# The shop's settings
name = \"Corner Books\"
open = true

[server]
host = \"127.0.0.1\"
port = 8080
timeout = 2.5

[tax]
rate = \"0.0825\"
since = 2026-01-01

[[staff]]
name = \"Ada\"
roles = [\"admin\", \"till\"]

[[staff]]
name = \"Grace\"
roles = [\"till\"]
")

(def shop (toml/read-string shop-toml))

(:name shop) ; => "Corner Books"
(get-in shop [:server :port]) ; => 8080
(mapv :name (:staff shop)) ; => ["Ada" "Grace"]
```

Strings, integers, floats, booleans, arrays, and tables map to the Norn
types you'd expect. Dates and times need a decision. An offset date-time
such as `1979-05-27T07:32:00Z` names one moment, and it reads as a
`norn.time` instant ([Chapter 21](21-the-operating-system.md)). A *local*
date like `2026-01-01` has no time zone, and by default it reads as a
string. Pass `:local-times true` to get `norn.time` values instead:

```clojure
(get-in shop [:tax :since]) ; => "2026-01-01"

(let [typed (toml/read-string shop-toml :local-times true)]
  (norn.time/format-local (get-in typed [:tax :since])))
; => "2026-01-01"
```

The difference shows when you write the map back. `write-string` writes a
string as a TOML string, so a date read as a string comes back quoted. With
`:local-times true`, the date stays a date. Either way the round trip
gives you an equal map:

```clojure
(= shop (toml/read-string (toml/write-string shop))) ; => true
```

`write-string` writes the plain values first, then the tables, then the
arrays of tables. Map order isn't defined, so don't expect the keys in the
order you wrote them. TOML has no null, so a nil value is an error when
writing, and a key defined twice is an error when reading, as the
specification says. Reading errors are `ParseError`s that carry the line:

```clojure
(toml/read-string "port = 80\nport = 81\n") ; !! ParseError

(handler-case (toml/read-string "port = 80\nport = 81\n")
  (ParseError [e] (ex-data e)))
; => {:line 2 :source "TOML"}
```

`read-file` and `write-file` read and write a file directly:
`(toml/read-file "shop.toml")`.

### YAML

YAML is common in deployment and CI files, so you'll often read it even if
you'd never choose it. `norn.yaml` reads one configuration document: block
and flow collections, quoted and block strings, comments, and anchors with
aliases. Like `norn.toml`, it reads map keys as keywords. Pass
`:keys identity` to keep them as strings.

```clojure
(def deploy-yaml "
service: catalog
replicas: 3
ports: [80, 443]
env:
  - name: LOG_LEVEL
    value: info
  - name: REGION
    value: eu-west-1
debug: no
country: NO
banner: |
  Welcome
  to the shop
")

(def deploy (yaml/read-string deploy-yaml))

(:replicas deploy) ; => 3
(:ports deploy) ; => [80 443]
(mapv :name (:env deploy)) ; => ["LOG_LEVEL" "REGION"]
(:banner deploy) ; => "Welcome\nto the shop\n"
```

Look at `debug` and `country`. YAML 1.1 read `no` as false and turned the
country code for Norway into a boolean, a famous source of bugs. Norn
follows YAML 1.2's Core rules, where only `true` and `false` (also written
`True`, `TRUE`, `False`, `FALSE`) are booleans:

```clojure
[(:debug deploy) (:country deploy)] ; => ["no" "NO"]
```

The reader handles configuration, not every corner of YAML. Custom tags,
multiple documents in one stream, and multi-line plain scalars are out of
scope, and the merge key `<<` is an ordinary key. Anything it can't read
is a `ParseError` with the line.

`write-string` emits YAML's flow style, which is also valid JSON for most
data. It's useful for generating a file another tool reads, less so for
one a person will edit.

```clojure
(yaml/write-string [1 "two" true nil]) ; => "[1, \"two\", true, null]\n"
```

`read-file` and `write-file` read and write UTF-8 files directly.

### EDN

EDN is Norn's own data syntax as a format: the same text you write in
code, with lists, vectors, maps, sets, keywords, symbols, characters,
ratios, regexes, and `#bytes`. When a configuration file is only ever read
by Norn programs, EDN saves you a translation step, and it's the natural
format for saving and reloading program data.

`norn.edn/read-string` reads the first value in a string and never
evaluates anything. A list stays a list; it isn't called.

```clojure
(def saved (edn/read-string "{:theme :dark :recent #{\"a.txt\"} :ratio 3/4 :when (+ 1 2)}"))

(:ratio saved) ; => 3/4
(:when saved) ; => (+ 1 2)
(edn/read-all "1 :two \"three\"") ; => [1 :two "three"]
(edn/read-string "") ; => nil
(edn/read-string "" :eof :empty) ; => :empty
```

`read-all` reads every value in the string. `:eof` supplies the value for
text that holds none, so you can tell an empty file from a file containing
`nil`. `write-string` writes text that `read-string` reads back as an
equal value.

```clojure
(= saved (edn/read-string (edn/write-string saved))) ; => true
```

`write-string` keeps that promise by refusing what has no EDN form. Atoms,
functions, and instances of classes, such as decimals and times, signal a
`TypeError` naming the value. `pr-str` still prints them, but its text
doesn't read back. To save instances, use `norn.bin`, later in this
chapter.

```clojure
(edn/write-string {:count (atom 0)}) ; !! TypeError
```

## Interchange

Interchange formats carry data between programs written in different
languages. JSON is the most common; CSV is what spreadsheets speak; XML
remains in feeds, office documents, and older services.

### JSON

`norn.json/read-string` reads objects as maps with keyword keys, arrays as
vectors, `null` as nil, and numbers as integers (of any size) or floats.

```clojure
(def order (json/read-string
             "{\"id\": 1042, \"customer\": {\"name\": \"Ada\"},
               \"lines\": [{\"sku\": \"A-100\", \"qty\": 3}], \"note\": null}"))

(get-in order [:customer :name]) ; => "Ada"
(get-in order [:lines 0 :qty]) ; => 3
(:note order) ; => nil
```

Keyword keys are convenient when you know the keys, as with a service's
documented response. When keys are data, such as user names or IDs, or
contain spaces, pass `:keywords false` to keep them as strings:

```clojure
(json/read-string "{\"Ada Lovelace\": 36}" :keywords false) ; => {"Ada Lovelace" 36}
```

`write-string` goes the other way. Maps become objects, any sequence becomes
an array, and keywords, symbols, and characters become strings. A ratio
becomes a float. `:pretty true` indents by two spaces.

```clojure
(json/write-string {:sku :A-100}) ; => "{\"sku\":\"A-100\"}"
(json/write-string 3/4) ; => "0.75"
(json/write-string [#{1} '(2 3)]) ; => "[[1],[2,3]]"
(println (json/write-string (sorted-map :id 1042 :tags ["gift"]) :pretty true))
; prints: {
; prints:   "id": 1042,
; prints:   "tags": [
; prints:     "gift"
; prints:   ]
; prints: }
```

Map keys come out in the map's order, which for a hash map isn't defined;
the sorted map above gives a stable order. Values JSON can't represent,
such as NaN, infinities, or instances of your classes, are an error rather
than a silently wrong file. Convert them first.

```clojure
(json/write-string {:ratio ##NaN}) ; !! Error
(json/read-string "{\"id\": ") ; !! ParseError
```

A reading error carries `:line` and `:column` in its `ex-data`. The file
helpers `read-file` and `write-file` take the same options.

### JSON Lines

JSON Lines (also called NDJSON) puts one JSON value on each line. Logs,
event streams, and data exports use it because a file can grow forever and
still be read one record at a time. `norn.ndjson` has `read-string` and
`read-file`, which give a vector, and `write-string` and `write-file`, which
take a sequence:

```clojure
(ndjson/write-file "events.jsonl"
                   [{:type "view" :ms 40} {:type "buy" :ms 12} {:type "view" :ms 7}])
(io/read-text "events.jsonl")
; => "{\"type\":\"view\",\"ms\":40}\n{\"type\":\"buy\",\"ms\":12}\n{\"type\":\"view\",\"ms\":7}\n"
```

For a large file, don't build the vector. `reduce-records` reduces over the
records from a line reader, one at a time:

```clojure
(defn view-time [path]
  (io/with-open [r (io/line-reader (io/open path :read))]
    (ndjson/reduce-records (fn [total e] (if (= "view" (:type e)) (+ total (:ms e)) total))
                           0 r)))

(view-time "events.jsonl") ; => 47
```

A `reduced` value stops early, and the line reader stays at the next record.
To add to a file as events happen, open it in `:append` mode and call
`write-record` for each value. A bad line is a `ParseError` naming the line
number in the file:

```clojure
(io/with-open [h (io/open "events.jsonl" :append)]
  (ndjson/write-record h {:type "buy" :ms 3}))
(count (ndjson/read-file "events.jsonl")) ; => 4

(io/append-text "events.jsonl" "{oops\n")
(handler-case (ndjson/read-file "events.jsonl")
  (ParseError [e] (:line (ex-data e))))
; => 5
```

### CSV

CSV is what spreadsheets export and what analysts ask for. `norn.csv`
follows RFC 4180: fields may be quoted, and a quoted field can hold commas,
doubled quotes, and line breaks. `read-csv` gives a vector of rows, each a
vector of strings. `read-maps` uses the first row as a header and gives a
map per row.

```clojure
(def price-list
  (str "sku,name,price,qty\n"
       "A-100,Notebook,4.95,3\n"
       "B-220,\"Pen, blue\",1.20,10\n"
       "C-310,Desk lamp,24.00,1\n"))

(second (csv/read-csv price-list)) ; => ["A-100" "Notebook" "4.95" "3"]
(:name (second (csv/read-maps price-list))) ; => "Pen, blue"
```

Every field is a string. CSV has no types, so the program decides what a
column means. Convert as you read:

```clojure
(defn parse-item [{:keys [sku name price qty]}]
  {:sku sku :name name :price price :qty (parse-long qty)})

(mapv :qty (map parse-item (csv/read-maps price-list))) ; => [3 10 1]
```

The prices stay strings for now. Reading them as floats would lose cents;
the decimal section below reads them properly.

`read-maps` pairs header names with fields by position. A row with more or
fewer fields than the header is a `ParseError` naming its line, so ragged
input doesn't slip through. Blank lines are skipped. Pass `:strict false` to
take ragged rows anyway: extra fields are dropped, and missing keys are
left out of the map.

```clojure
(csv/read-maps "sku,qty\nA-100,3\nB-220\n") ; !! ParseError
(csv/read-maps "sku,qty\nA-100,3\nB-220\n" :strict false)
; => [{:sku "A-100" :qty "3"} {:sku "B-220"}]
```

`write-csv` writes rows, quoting a field only when it must. Values are
written with `str`, and nil is an empty field. Lines end in `\r\n`, as the
RFC says; pass `:newline "\n"` for the other. `write-maps` writes a header
from `:columns` (or the first map's keys) and then a row per map.

```clojure
(csv/write-csv [["sku" "note"] ["A-100" "says \"hi\""] ["B-220" nil]])
; => "sku,note\r\nA-100,\"says \"\"hi\"\"\"\r\nB-220,\r\n"

(csv/write-maps [{:sku "A-100" :qty 3} {:sku "B-220" :qty 10}]
                :columns [:sku :qty] :newline "\n")
; => "sku,qty\nA-100,3\nB-220,10\n"
```

Both readers take `:separator` and `:quote` for files that use semicolons or
tabs, and `read-maps` takes `:keys identity` to keep header names as strings.

```clojure
(csv/read-csv "a;b\n1;2\n" :separator \;) ; => [["a" "b"] ["1" "2"]]
```

### XML

`norn.xml` reads an XML 1.0 document into nested maps. Each element is
`{:tag … :attrs … :namespaces … :content […]}`. Names are *expanded*: a pair
of a namespace URI and a local name, with nil for no namespace. That looks
heavy for a simple document, but it means two names compare equal exactly
when XML says they're the same name, whatever prefixes the document used.

```clojure
(def feed (xml/read-string
            (str "<feed>"
                 "<entry id='1'><title>Spring sale</title></entry>"
                 "<entry id='2'><title>New &amp; noted</title></entry>"
                 "</feed>")))

(:tag feed) ; => [nil "feed"]
(:attrs (first (:content feed))) ; => {[nil "id"] "1"}
```

`:content` holds child elements and text strings in document order. A few
small functions are enough to query the tree:

```clojure
(defn children [el tag]
  (filter #(and (map? %) (= [nil tag] (:tag %))) (:content el)))

(defn text-of [el]
  (apply str (filter string? (:content el))))

(mapv (fn [entry] [(get-in entry [:attrs [nil "id"]])
                   (text-of (first (children entry "title")))])
      (children feed "entry"))
; => [["1" "Spring sale"] ["2" "New & noted"]]
```

The reader keeps whitespace between elements as text, so pretty-printed
XML has strings of spaces and newlines in `:content`. Filtering with `map?`,
as `children` does, skips them. Entities and character references are
decoded, comments and processing instructions are dropped, and a document
with a `DOCTYPE` is rejected, which closes off the entity-expansion attacks
that DTDs allow. Malformed input is a `ParseError` with `:line` and
`:column`.

`write-string` turns a tree back into text, escaping as needed. You can
build elements by hand; `:attrs`, `:namespaces`, and `:content` are
optional.

```clojure
(xml/write-string {:tag [nil "item"]
                   :attrs {[nil "sku"] "A-100"}
                   :content ["Notebook & pen" {:tag [nil "br"]}]})
; => "<item sku=\"A-100\">Notebook &amp; pen<br/></item>"
```

For documents with namespaces, `write-string` reuses the prefixes in
`:namespaces` and makes up prefixes for any it lacks. The
[library reference](../reference/libraries.md#xml) describes the scoping
rules in full. `read-file` reads UTF-8 files and UTF-16 files with a
byte-order mark, and `write-file` writes UTF-8.

## Binary data

Text formats are readable and slow to parse. Binary formats are compact,
exact for floats, and fast, and some data, such as images and network
protocols, is binary to begin with. Norn's binary libraries all work on
`Bytes`.

```clojure
(require '[norn.bytes :as b]
         '[norn.buffer :as buf]
         '[norn.cbor :as cbor]
         '[norn.bin :as bin])
```

### Bytes, hex, and Base64

`Bytes` is in core: `(bytes [1 2 3])`, `(bytes "text")`, `count`, `nth`,
`subbytes`, `bytes-concat`, and `bytes->string`. `norn.bytes` adds the
conversions you need at the edges of a program: hex and Base64 for putting
bytes into text, and fixed-width integers and floats for binary layouts.

```clojure
(b/hex (bytes "Norn")) ; => "4e6f726e"
(b/unhex "4e6f726e") ; => #bytes "4e6f726e"
(b/base64 (bytes "Norn")) ; => "Tm9ybg=="
(bytes->string (b/unbase64 "Tm9ybg==")) ; => "Norn"
```

`(base64 b :url)` uses the URL-safe alphabet without padding, the form used
in tokens and URLs. `unbase64` accepts either alphabet, padded or not.

Binary file formats often start with a fixed header: a magic string, a
version, a count. `uint-bytes` writes an unsigned integer in a given number
of bytes and byte order, `read-uint` reads one back from an offset, and
`slice` cuts out a range.

```clojure
(def header (b/concat-bytes (bytes "SHOP")
                            (b/uint-bytes 2 2 :be)
                            (b/uint-bytes 1500 4 :be)))

(count header) ; => 10
(b/hex header) ; => "53484f500002000005dc"

(defn read-header [bs]
  {:magic (bytes->string (b/slice bs 0 4))
   :version (b/read-uint bs 4 2 :be)
   :count (b/read-uint bs 6 4 :be)})

(read-header header) ; => {:magic "SHOP" :version 2 :count 1500}
```

`float-bytes` and `read-float` do the same for 4- and 8-byte IEEE 754
floats. For UTF-16 text, or text that arrives in chunks, `decode-text` and
the incremental `decoder` decode it without splitting characters.

### Packed numbers

When you have large arrays of numbers, such as samples, pixels, or
measurements, `norn.buffer` packs them into `Bytes`, each element taking
exactly the width of one element type: `:u8`, `:i8`,
`:u16`, `:i16`, `:u32`, `:i32`, `:u64`, `:i64`, `:f32`, or `:f64`.

```clojure
(def readings (buf/pack :i16 [120 -45 300]))

(buf/to-bytes readings) ; => #bytes "7800d3ff2c01"
(count readings) ; => 3
(nth readings 1) ; => -45
(reduce + readings) ; => 375
(buf/values readings) ; => [120 -45 300]
```

A buffer is a collection to core: `count` and `nth` take constant time, and
`reduce`, `map`, `into`, and the other sequence functions read its elements
in order. Byte order is little-endian unless you pass `:order :be`.
`from-bytes` goes the other way, viewing existing bytes as elements without
copying them, which is how you read a block of numbers from a file:

```clojure
(buf/values (buf/from-bytes :u16 (bytes [1 0 2 0]))) ; => [1 2]
(buf/values (buf/from-bytes :u16 (bytes [1 0 2 0]) :order :be)) ; => [256 512]
```

When you need to fill a buffer in place, `mutable` makes one of zeros,
`set!` and `fill!` write it, and `freeze!` turns it into an immutable buffer
without copying. A mutable buffer is owned by the fiber that made it, as
[Chapter 10](10-protocols-and-extension.md) explains for mutable classes.

```clojure
(def samples (buf/mutable :f32 4))
(buf/fill! samples 0.5)
(buf/set! samples 0 1.0)
(buf/values (buf/freeze! samples)) ; => [1.0 0.5 0.5 0.5]
```

### CBOR

CBOR is a binary cousin of JSON: maps, arrays, strings, numbers, booleans,
and null, plus byte strings and tags, in a compact encoding that many
languages and devices read. Use it when you'd use JSON but care about size,
want floats in their binary form, or need to embed raw bytes.

```clojure
(def packet (cbor/encode {"id" 7 "name" "Ada" "photo" (bytes [255 216])}))

(count packet) ; => 23
(get (cbor/decode packet) "photo") ; => #bytes "ffd8"
```

CBOR has no keywords, so `encode` only takes the types it can represent
faithfully. A keyword, a set, or a symbol is a `TypeError`, not a silent
conversion; use string keys, as above. Integers of any size work (big ones
travel as CBOR bignum tags), and CBOR's decimal fraction tag reads as a
`norn.decimal` value. Other tags come back as `Tagged` values holding the
tag number and its contents, so nothing is lost.

```clojure
(cbor/encode {:id 7}) ; !! TypeError
(cbor/decode (cbor/encode [1 nil 2.5 (* 100000000000 100000000000)]))
; => [1 nil 2.5 10000000000000000000000]
```

### norn.bin: Norn values as bytes

`norn.bin` is for Norn talking to Norn: caching results on disk, saving
program state, sending values between Norn processes. It writes every kind
of Norn data, including keywords, sets, ratios, characters, sorted
collections, queues, regexes, and instances of your classes, and reads it
back as an equal value.

```clojure
(def state {:user "ada" :cart #{:A-100 :B-220} :share 1/3 :at 1.5})

(= state (bin/decode (bin/encode state))) ; => true
(bin/write-file "state.bin" state)
(= state (bin/read-file "state.bin")) ; => true
```

It's not for things that aren't data: functions, atoms, channels, handles,
or mutable objects are an error to encode.

Decoding is safe for untrusted input. It builds data and runs no code, and
it makes instances only of the classes you list in `:classes`. An instance
of any other class is an error, so a file can't make your program
construct something you didn't expect.

```clojure
(defclass Price () ((sku) (cents)))

(def priced (bin/encode [(Price :sku "A-100" :cents 495)]))
(bin/decode priced) ; !! Error
(:cents (first (bin/decode priced :classes [Price]))) ; => 495
```

The format starts with a version number, and a reader rejects data written
by a newer version rather than misreading it.

## Compression and archives

```clojure
(require '[norn.compress :as z] '[norn.zip :as zip])
```

### Compressing data

`norn.compress` does deflate in its three wrappings: gzip (the default),
zlib, and raw deflate. `compress` and `decompress` take `Bytes` or a string,
and give `Bytes`. `gzip` and `gunzip` are shorthands.

```clojure
(def report (str/join "\n" (repeat 200 "A-100,Notebook,4.95,3")))

(def packed (z/gzip report))
(< (count packed) 100) ; => true
(= report (bytes->string (z/gunzip packed))) ; => true
```

`:level` trades speed for size, from 0 (store only) to 9 (smallest). A
`.gz` file is gzip data in a file, so reading one is a `read-bytes` and a
`gunzip`:

```clojure
(io/write-bytes "report.csv.gz" (z/compress report :level 9))
(count (str/split-lines (bytes->string (z/gunzip (io/read-bytes "report.csv.gz")))))
; => 200
```

Compressed data can claim to expand to far more than it occupies. A few
kilobytes can unpack to gigabytes, an attack known as a zip bomb. So
`decompress` stops at `:max-size` bytes of output, 256 MiB unless you say
otherwise, and signals an error instead of filling memory. Set it to what
your program expects:

```clojure
(z/decompress packed :max-size 1000) ; !! Error
```

`crc32` computes the checksum gzip and zip use, for formats that need it.

### ZIP archives

`norn.zip/zip` builds an archive from a map of names to contents, strings
or `Bytes`, and gives the archive as `Bytes`. A name ending in `/` is a
directory. Write the result with `io/write-bytes`.

```clojure
(def bundle (zip/zip {"README.txt" "Price list for March"
                      "data/prices.csv" price-list}))
(io/write-bytes "bundle.zip" bundle)
```

`entries` lists what an archive holds, `read-entry` reads one entry, and
`unzip` gives a map of every file's name to its bytes:

```clojure
(def archive (io/read-bytes "bundle.zip"))

(sort (map :name (zip/entries archive))) ; => ("README.txt" "data/prices.csv")
(bytes->string (zip/read-entry archive "README.txt")) ; => "Price list for March"
(zip/read-entry archive "missing.txt") ; => nil
(= price-list (bytes->string (get (zip/unzip archive) "data/prices.csv"))) ; => true
```

Each entry map also has `:size`, `:compressed-size`, `:method`, `:crc`, and
`:modified`. Every entry's checksum is checked when you read it.

`extract!` writes the files under a folder and returns their paths. Like
`decompress`, the zip reader is careful with archives from elsewhere: it
stops past `:max-size` bytes in all, and `extract!` refuses an entry whose
name would land outside the folder (an absolute path or one with `..`)
before writing anything.

```clojure
(zip/extract! archive "unpacked") ; => ["unpacked/README.txt" "unpacked/data/prices.csv"]
(io/read-text "unpacked/README.txt") ; => "Price list for March"
```

The library reads and writes stored and deflated entries, which is what
nearly every zip file uses. It doesn't yet handle ZIP64 (archives over
4 GiB or with more than 65,535 entries) or encrypted archives.

## Money and decimals

Floating-point numbers are binary fractions, and most decimal fractions
have no exact binary form:

```clojure
(+ 0.1 0.2) ; => 0.30000000000000004
```

For measurements, that error is noise. For money, it's a bug: totals drift
by a cent, and two amounts that should be equal aren't. Norn's ratios are
exact ([Chapter 3](03-values-and-expressions.md)), but they don't remember
how many decimal places a value had, and money needs that. `norn.decimal`
represents a decimal as an integer coefficient and a *scale*, the number of
digits after the point. `4.95` is 495 with scale 2.

```clojure
(require '[norn.decimal :as d])

(def price (d/parse "4.95"))
[(:coefficient price) (:scale price)] ; => [495 2]
(d/text price) ; => "4.95"
(d/text (d/add (d/parse "0.10") (d/parse "0.2"))) ; => "0.30"
```

`parse` reads decimal text exactly, without going through a float, and
`text` writes it back, keeping the scale. `add` and `subtract` give the
larger of the two scales. `multiply` adds the scales, as you'd do by hand.
`(decimal n)` makes a whole number, and `(decimal coefficient scale)` builds
one directly.

Decimals are values with their own functions, not numbers to core. `+`,
`<`, and `==` don't take them. This keeps one numeric tower in core, and it
makes every conversion visible: you parse at the edge of the program and
write text at the other edge.

```clojure
(+ price 1) ; !! TypeError
```

Now the price list from the CSV section can be totaled exactly:

```clojure
(defn line-total [{:keys [price qty]}]
  (d/multiply (d/parse price) (d/decimal (parse-long qty))))

(def subtotal
  (reduce d/add (d/decimal 0 2) (map line-total (csv/read-maps price-list))))

(d/text subtotal) ; => "50.85"
```

### Rounding

Multiplying by a tax rate gives more digits than a price has:

```clojure
(def tax-rate (d/parse (get-in shop [:tax :rate])))
(d/text (d/multiply subtotal tax-rate)) ; => "4.195125"
```

Rounding is a business decision, so `norn.decimal` never rounds on its own.
`quantize` changes a value to a given scale, rounding as you choose.
`divide`, whose exact result may have no end, always takes the scale you
want. The default rounding mode is half-even, also called banker's
rounding: a tie goes to the even neighbor, so rounding errors don't pile
up in one direction over many values.

```clojure
(d/text (d/quantize (d/multiply subtotal tax-rate) 2)) ; => "4.20"
(d/text (d/quantize (d/parse "2.5") 0)) ; => "2"
(d/text (d/quantize (d/parse "3.5") 0)) ; => "4"
(d/text (d/quantize (d/parse "2.5") 0 :half-up)) ; => "3"
```

The other modes are `:toward-zero`, `:floor`, `:ceiling`, and
`:unnecessary`, which signals an error if rounding would change the value.
Use it where you believe a value is already exact and want to know if it
isn't.

Splitting a bill three ways shows `divide` with a rounding mode. Round each
share down, and give the remainder to the last share so the parts add up to
the whole:

```clojure
(defn split-evenly [total n]
  (let [share (d/divide total (d/decimal n) (:scale total) :floor)
        rest (d/subtract total (d/multiply share (d/decimal (dec n))))]
    (conj (vec (repeat (dec n) share)) rest)))

(map d/text (split-evenly (d/parse "100.00") 3)) ; => ("33.33" "33.33" "33.34")
```

### Comparing decimals

Equality with `=` is structural, and the scale is part of the structure, so
`1.0` and `1.00` are not `=`. They are the same amount, which `d/compare`
reports. It works like `compare`, so you can pass it to `sort`.

```clojure
(= (d/parse "1.0") (d/parse "1.00")) ; => false
(d/compare (d/parse "1.0") (d/parse "1.00")) ; => 0
(map d/text (sort d/compare (map d/parse ["10" "2.50" "-1"]))) ; => ("-1" "2.50" "10")
```

To store decimals, write them as text with `d/text`: in JSON as a string,
in SQLite as `TEXT`, or as an integer count of cents when every amount has
the same scale. CBOR carries them directly, as its decimal fraction tag.
`norn.bin` does too, but a decimal is an instance of the class
`d/Decimal`, so list it in `:classes` when you decode.

## Identifiers and hashing

### UUIDs

A UUID is a 128-bit identifier you can create anywhere without asking a
central counter. `norn.uuid` represents UUIDs as their canonical text:
lowercase hex in the 8-4-4-4-12 pattern. They're strings, so they work as
map keys, in JSON, and in databases with no conversion.

```clojure
(require '[norn.uuid :as uuid])

(def id (uuid/v4))
(uuid/valid? id) ; => true
(uuid/version id) ; => 4
(count id) ; => 36
```

`v4` is random. `v7` puts the time in milliseconds first, then random bits,
so UUIDs made later sort later. That makes v7 a good primary key: new rows
go at the end of a database index instead of landing at random places in
it. `timestamp-ms` reads the time back out.

```clojure
(def a (uuid/v7))
(sleep 2)
(def b (uuid/v7))
(neg? (compare a b)) ; => true
(uuid/timestamp-ms (uuid/v7 1767225600000)) ; => 1767225600000
```

`parse` accepts the forms people paste, uppercase, braced, or without
dashes, and gives the canonical text, or signals a `ParseError` for text
that isn't a UUID. `->bytes` and `from-bytes` convert to and from the 16-byte
form that some binary protocols use.

```clojure
(uuid/parse "{6BA7B810-9DAD-11D1-80B4-00C04FD430C8}")
; => "6ba7b810-9dad-11d1-80b4-00c04fd430c8"
(uuid/parse "not-a-uuid") ; !! ParseError
```

### Digests

A digest, or cryptographic hash, is a short fingerprint of data: the same
input always gives the same digest, and finding two inputs with the same
digest is infeasible. Use one to check that a download arrived intact, to
name content by what it contains, or to detect changes. `norn.crypto`
returns digests as `Bytes`; write them as text with `crypto/hex` or
`b/base64`.

```clojure
(require '[norn.crypto :as crypto])

(crypto/hex (crypto/sha256 "abc"))
; => "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
(count (crypto/sha512 "abc")) ; => 64
```

`digest` takes the algorithm by name: `:sha256`, `:sha384`, `:sha512`, or
`:sha1`, which is there for old protocols that require it and shouldn't be
chosen for anything new. A checksum for the archive from the last section:

```clojure
(io/write-text "bundle.zip.sha256" (crypto/hex (crypto/sha256 (io/read-bytes "bundle.zip"))))

(defn verified? [path]
  (= (io/read-text (str path ".sha256")) (crypto/hex (crypto/sha256 (io/read-bytes path)))))

(verified? "bundle.zip") ; => true
```

### Signing messages with HMAC

A digest shows data hasn't changed by accident. To show it hasn't been
changed on purpose, by someone who could also recompute the digest, use an
HMAC: a digest keyed with a secret that only you and the receiver hold. A
webhook, a signed cookie, or a download link with an expiry all work this
way.

```clojure
(def signing-key (crypto/random-bytes 32))
(def message "order=1042&amount=50.85")
(def tag (crypto/hmac :sha256 signing-key message))

(crypto/hmac-valid? :sha256 signing-key message tag) ; => true
(crypto/hmac-valid? :sha256 signing-key "order=1042&amount=0.01" tag) ; => false
```

Check a tag with `hmac-valid?`, not by computing the HMAC and comparing
with `=`. `=` stops at the first byte that differs, and how long it takes
tells an attacker how much of a forged tag is right. `hmac-valid?` takes
the same time wherever the bytes differ, and so does `constant-time=`,
which compares any two secrets.

### Passwords and tokens

Never store a password, or a plain digest of one: digests are fast to
compute, so a stolen table of them can be tested against billions of
guesses. `pbkdf2` derives a key from a password and a random salt, and is
deliberately slow (600,000 rounds of SHA-256 by default). Store the salt and
the result. To check a login, derive again with the stored salt and compare
in constant time.

```clojure
(defn hash-password [password]
  (let [salt (crypto/random-bytes 16)]
    {:salt (b/hex salt) :hash (b/hex (crypto/pbkdf2 password salt))}))

(defn password-ok? [stored attempt]
  (crypto/constant-time= (b/hex (crypto/pbkdf2 attempt (b/unhex (:salt stored))))
                         (:hash stored)))

(def stored (hash-password "correct horse battery staple"))
(password-ok? stored "correct horse battery staple") ; => true
(password-ok? stored "Tr0ub4dor&3") ; => false
```

`random-bytes` gives bytes from the operating system's secure generator,
for keys and salts. `random-token` gives a URL-safe string of 32 random
bytes, or as many as you ask for, for session IDs, password-reset links,
and API keys. Use these rather than `rand` or `norn.random` for anything an
attacker might want to guess.

```clojure
(count (crypto/random-token)) ; => 43
(count (crypto/random-token 16)) ; => 22
```

## Summary

- Every format library reads into plain Norn data and writes from it.
  Reading errors are `ParseError`s with `:line` (and `:column` where it
  applies) in `ex-data`.
- For configuration, TOML reads keys as keywords and local dates as strings
  unless you pass `:local-times true`; YAML follows the 1.2 Core rules and
  reads keys as keywords too; EDN reads Norn data without evaluating it,
  and `write-string` refuses values that wouldn't read back.
- JSON reads keys as keywords by default (`:keywords false` keeps strings);
  JSON Lines streams records with `reduce-records`; CSV fields are always
  strings; XML elements are maps with expanded `[namespace local]` names.
- `norn.bytes` converts between bytes and hex, Base64, and fixed-width
  numbers; `norn.buffer` packs numeric arrays; CBOR is compact
  interchange; `norn.bin` saves any Norn data, instances only of classes
  you allow.
- `norn.compress` and `norn.zip` limit what they unpack with `:max-size`,
  and `extract!` never writes outside its folder.
- `norn.decimal` keeps money exact: parse and write text at the edges, round
  only with `quantize` or `divide`, and compare amounts with `d/compare`.
- `norn.uuid` gives UUIDs as strings (v7 for sortable keys); `norn.crypto`
  has digests, HMAC with constant-time checks, PBKDF2 for passwords, and
  secure random tokens.

---

[Contents](README.md) · Previous: [19. Files and streams](19-files-and-streams.md) · Next: [21. The operating system](21-the-operating-system.md) <!-- nav -->
