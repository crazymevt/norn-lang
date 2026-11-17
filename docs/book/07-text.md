# 7. Text

[Contents](README.md) · Previous: [6. Sequences and transducers](06-sequences-and-transducers.md) · Next: [8. Modules and projects](08-modules-and-projects.md) <!-- nav -->

Most programs read text, take it apart, and put new text together for a
person to read. This chapter covers how Skein represents strings and
characters, the `skein.string` toolkit, the difference between a code
point and what a reader sees as one character, regular expressions,
`format` and the human-friendly formatters in `skein.format`, and three
libraries for larger jobs: `skein.pprint` for printing nested data,
`skein.rope` for text you edit in place, and `skein.markdown`.

A running example ties the middle of the chapter together: a few lines of
a web server's access log that we parse, summarize, and print as a table.

## Strings are values

A Skein string is an immutable sequence of Unicode characters, stored as
UTF-8. Like every other Skein value, a string never changes after you make
it. Functions that "modify" a string return a new one.

`count`, `nth`, and `subs` all work in characters, never bytes, whatever
the text contains.

```clojure
(count "hello") ; => 5
(count "naïve") ; => 5
(nth "naïve" 2) ; => \ï
(subs "hello world" 6) ; => "world"
(subs "hello world" 0 5) ; => "hello"
```

`subs` takes a start index and an optional end index (exclusive). An index
past the end of the string signals an error rather than clamping.

```clojure
(subs "hello" 2 9) ; !! IndexError
(nth "hello" 9) ; !! IndexError
```

Because a string is a sequence, the sequence functions from
[Chapter 6](06-sequences-and-transducers.md) work on it, one character at
a time. Skein's sequence functions are eager and type-preserving where they
can be, but a string can only hold characters, so `map` and `filter` over
a string return a vector of characters. Turn them back into a string with
`apply str`.

```clojure
(vec "abc") ; => [\a \b \c]
(filter #(not= % \l) "hello") ; => [\h \e \o]
(apply str (filter #(not= % \l) "hello")) ; => "heo"
(frequencies "hello") ; => {\h 1 \e 1 \l 2 \o 1}
```

Strings compare with `=` by content and sort in code-point order, which
puts every uppercase ASCII letter before every lowercase one. Sort by a
key when you want case not to matter.

```clojure
(= "abc" (str "ab" "c")) ; => true
(sort ["banana" "Cherry" "apple"]) ; => ["Cherry" "apple" "banana"]
(compare "a" "b") ; => -1
```

Indexing into a string is quick for ASCII text. For other text, Skein keeps
a small per-thread cache of recent positions, so walking through a string
from start to end costs about what the text does. Random access deep into
a long non-ASCII string is the slow case. If you need it often, turn the
string into a vector of characters once, or use a rope (see
[Ropes](#ropes-for-text-you-edit) below).

## Characters

A character literal starts with a backslash: `\a`, `\ï`, `\日`. A few have
names: `\space`, `\newline`, `\tab`, `\return`, `\backspace`,
`\formfeed`, and `\nul`. `int` gives a character's code point, and `char`
goes the other way. A character can also be written by its code point in
hex: `\u0308` is the combining diaeresis that `(char 0x308)` makes.

```clojure
[\a \space \newline] ; => [\a \space \newline]
(int \a) ; => 97
(char 97) ; => \a
(char (inc (int \a))) ; => \b
(first "日本") ; => \日
```

Characters are not numbers. `<` and `+` don't accept them, but `compare`
does, and so do `sort` and `=`.

```clojure
(< \a \b) ; !! Error
(compare \a \b) ; => -1
(sort [\c \a \b]) ; => [\a \b \c]
```

`str` turns a character into a one-character string, and the
`skein.string` case functions accept a character too.

```clojure
(str \x) ; => "x"
```

## Building strings

`str` concatenates the printed text of its arguments. Strings and
characters go in as they are, `nil` becomes the empty string, and other
values appear as `print` would show them.

```clojure
(str "a" \b 1 :c nil [2 3]) ; => "ab1:c[2 3]"
```

There are three ways to turn a value into text, and they differ in who the
text is for. `str` and `print` produce text for people: a string appears
without quotes. `pr-str` and `prn` produce text for the reader: the result
reads back as the same value.

```clojure
(str "say \"hi\"") ; => "say \"hi\""
(pr-str "say \"hi\"") ; => "\"say \\\"hi\\\"\""
(read-string (pr-str [1 "two" \3])) ; => [1 "two" \3]
```

`print-str` and `prn-str` return what `print` and `prn` would print.
`with-out-str` captures everything its body prints, which is handy when a
function you call writes to standard output.

```clojure
(with-out-str
  (print "total:")
  (println " " 42)) ; => "total:  42\n"
```

To build a string from many pieces, collect the pieces in a vector and
join them at the end instead of calling `str` again and again on a growing
string. `skein.string/join` takes an optional separator and the items.

```clojure
(require '[skein.string :as str])

(str/join ", " ["red" "green" "blue"]) ; => "red, green, blue"
(str/join [1 2 3]) ; => "123"
(str/join "\n" (map #(str "- " %) ["one" "two"])) ; => "- one\n- two"
```

The `(require '[skein.string :as str])` form loads the library and gives it
the short alias `str`. That alias doesn't hide the core function `str`:
`str/join` names a var in `skein.string`, while plain `str` still names the
core function. [Chapter 8](08-modules-and-projects.md) covers `require` in
full.

## The skein.string toolkit

`skein.string` collects the everyday string functions. Run `skein doc
skein.string` for the full list. This section works through them on a real
task.

Here are three lines from a web server's access log. Each line holds a
date, a time, an HTTP method, a path, a status code, and a response size
in bytes.

```clojure
(def log-text
  "2026-11-15 09:12:01 GET /index.html 200 5321
2026-11-15 09:12:04 GET /about 200 1200

2026-11-15 09:13:10 POST /login 302 0
")
```

`split-lines` (also called `lines`) splits text at `\n` or `\r\n`. The
blank line in the middle comes through as an empty string, so drop the
blank lines with `blank?`, which is true of `nil`, the empty string, and
strings of only whitespace.

```clojure
(str/split-lines "a\nb\r\nc") ; => ["a" "b" "c"]

(def log-lines (remove str/blank? (str/split-lines log-text)))

(count log-lines) ; => 3
```

`split` breaks a string at each match of a pattern. The pattern can be a
string, a character, or a regular expression, and an optional limit caps
the number of parts, with the last part holding the rest.

```clojure
(str/split (first log-lines) " ")
; => ["2026-11-15" "09:12:01" "GET" "/index.html" "200" "5321"]
(str/split "key=value=more" "=" 2) ; => ["key" "value=more"]
(str/split "a,b,,c" \,) ; => ["a" "b" "" "c"]
```

Splitting on a single space breaks as soon as the log has two spaces in a
row. Splitting on a regex that matches any run of whitespace is sturdier,
after `trim` removes whitespace at both ends.

```clojure
(str/split "  GET   /about  " #"\s+") ; => ["" "GET" "/about" ""]
(str/split (str/trim "  GET   /about  ") #"\s+") ; => ["GET" "/about"]
```

`trim`, `triml`, and `trimr` remove whitespace at both ends, the start, or
the end. `trim-newline` removes only trailing `\n` and `\r`.

Now we can turn a line into a map. `parse-long` from `skein.core` reads an
integer from a string, or returns `nil` when the string isn't one.

```clojure
(defn parse-entry [line]
  (let [[date time method path status bytes] (str/split (str/trim line) #"\s+")]
    {:date date :time time :method method :path path
     :status (parse-long status) :bytes (parse-long bytes)}))

(parse-entry (first log-lines))
; => {:date "2026-11-15" :time "09:12:01" :method "GET"
;     :path "/index.html" :status 200 :bytes 5321}
(parse-long "12x") ; => nil
```

### Searching

`includes?`, `starts-with?`, and `ends-with?` answer yes-or-no questions.
`index-of` and `last-index-of` return a character index, or `nil` when the
text isn't there. Each accepts a string or a character.

```clojure
(def entries (mapv parse-entry log-lines))

(filterv #(str/ends-with? (:path %) ".html") entries)
; => [{:date "2026-11-15" :time "09:12:01" :method "GET"
;      :path "/index.html" :status 200 :bytes 5321}]
(str/includes? "/index.html" \.) ; => true
(str/index-of "/index.html" ".") ; => 6
(str/last-index-of "a/b/c" "/") ; => 3
(str/index-of "hello" "z") ; => nil
```

`index-of` also takes a starting index, so you can step through every
occurrence.

```clojure
(str/index-of "a/b/c" "/" 2) ; => 3
```

### Changing case and replacing

`upper-case`, `lower-case`, and `capitalize` follow Unicode's rules, so a
character can become more than one.

```clojure
(str/upper-case "straße") ; => "STRASSE"
(str/capitalize "hELLO") ; => "Hello"
(str/upper-case \a) ; => "A"
```

`replace` swaps every match of a pattern for a replacement, and
`replace-first` swaps only the first. With a plain string pattern, the
match is literal.

```clojure
(str/replace "/a/b/c" "/" " > ") ; => " > a > b > c"
(str/replace-first "/a/b/c" "/" "") ; => "a/b/c"
```

`escape` maps individual characters, which is the right tool for escaping
text for HTML or a shell. The map can be a map or a function. A character
it maps to `nil` stays as it is.

```clojure
(str/escape "<a href=\"x\">" {\< "&lt;" \> "&gt;" \" "&quot;"})
; => "&lt;a href=&quot;x&quot;&gt;"
```

Regex patterns and replacement functions come up again in
[Regular expressions](#regular-expressions).

## Graphemes and display width

A Unicode code point is not always what a person thinks of as one
character. An accented letter can be written as one code point (`é`) or
as a plain letter followed by a combining accent. A flag is two code
points, and some emoji are a whole sequence joined together. Unicode calls
a user-perceived character a *grapheme cluster*.

`count` counts code points. `grapheme-count` and `graphemes` count and
split by what a reader sees. Here, `(char 0x308)` is the combining
diaeresis, the two dots over the e.

```clojure
(def noel (str "noe" (char 0x308) "l"))

noel ; => "noël"
(count noel) ; => 5
(str/grapheme-count noel) ; => 4
(str/graphemes noel) ; => ["n" "o" "ë" "l"]
(str/graphemes "🇳🇴 ok") ; => ["🇳🇴" " " "o" "k"]
```

The two spellings of an accented letter look the same but are different
strings. Here `(char 0x301)` is the combining acute accent.
`skein.string` doesn't normalize text, so compare strings from different
sources with care.

```clojure
(= "é" (str "e" (char 0x301))) ; => false
```

Reversing by code point would move the accent onto the wrong letter.
`str/reverse` keeps each grapheme intact. (Core `reverse` works on any
sequence and returns a list of characters.)

```clojure
(str/reverse noel) ; => "lëon"
(str/reverse "abc") ; => "cba"
```

A second question is how wide text is on screen. In a terminal or a
monospace editor, most characters take one column, East Asian wide
characters take two, and combining marks take none. `width` measures
columns, and the layout functions use it: `pad-left`, `pad-right`,
`center`, and `truncate`.

```clojure
(str/width "Oslo") ; => 4
(str/width "東京") ; => 4
(str/width noel) ; => 4
(str/pad-left "42" 6) ; => "    42"
(str/pad-left 42 6 "0") ; => "000042"
(str/pad-right "東京" 6 ".") ; => "東京.."
(str/center "hi" 6 "*") ; => "**hi**"
(str/truncate "日本語テキスト" 7) ; => "日本語"
(str/truncate "a long title" 8 "…") ; => "a long …"
```

`truncate` returns the longest start of the string that fits in the given
columns, without splitting a wide character. With an ellipsis, a string
that has to be shortened ends with it, and the whole result still fits.

Padding by width rather than by `count` is what keeps a table lined up
when its cells contain wide characters.

```clojure
(doseq [[city people] [["Oslo" 709000] ["東京" 14000000] ["Zürich" 421000]]]
  (println (str "|" (str/pad-right city 8) "|" (str/pad-left people 9) "|")))
; prints: |Oslo    |   709000|
; prints: |東京    | 14000000|
; prints: |Zürich  |   421000|
```

## Regular expressions

A regex literal is written `#"pattern"`. Inside it, backslashes need no
doubling: `#"\d+"` matches one or more digits. A regex is a value. It
prints as written, compares by its pattern, and survives being saved in an
image. `re-pattern` builds one from a string at run time.

```clojure
#"\d+" ; => #"\d+"
(= #"\d+" (re-pattern "\\d+")) ; => true
```

Skein's regexes use the syntax of Rust's `regex` crate. Matching always
takes time linear in the input, which makes them safe to run on untrusted
text. The cost of that guarantee is that backreferences and lookaround
aren't available. Flags go inline at the start of the pattern: `(?i)` for
case-insensitive, `(?m)` for multi-line `^` and `$`, `(?s)` to let `.`
match a newline, and `(?x)` to allow whitespace and comments.

### The core functions

Three functions in `skein.core` cover the common cases. `re-find` returns
the first match, `re-seq` returns every match, and `re-matches` succeeds
only when the whole string matches. Each returns the matched text, or
`nil` when there is no match.

```clojure
(re-find #"\d+" "order 66, aisle 7") ; => "66"
(re-seq #"\d+" "order 66, aisle 7") ; => ["66" "7"]
(re-matches #"\d+" "66") ; => "66"
(re-matches #"\d+" "66a") ; => nil
(re-find #"(?i)error" "Disk ERROR on sda") ; => "ERROR"
```

When the pattern has groups in parentheses, each result is a vector: the
whole match followed by each group. A group that took no part in the
match is `nil`.

```clojure
(re-find #"(\w+)@([\w.]+)" "write to ada@example.org today")
; => ["ada@example.org" "ada" "example.org"]
(re-find #"(cat)|(dog)" "hotdog") ; => ["dog" nil "dog"]
```

Destructuring pairs naturally with groups.

```clojure
(let [[_ user host] (re-find #"(\w+)@([\w.]+)" "ada@example.org")]
  [user host]) ; => ["ada" "example.org"]
```

### Match details with skein.regex

`skein.regex` adds positions, named groups, replacement, and splitting.
Name a group with `(?<name>…)`. `re/match` returns a map with the matched
text, its start and end indexes (in characters), the groups as a vector,
and the named groups by keyword.

Back to the access log. A regex with named groups describes a line more
precisely than splitting on spaces does, and rejects a line that doesn't
fit.

```clojure
(require '[skein.regex :as re])

(def line-re
  #"(?<date>\S+) (?<time>\S+) (?<method>[A-Z]+) (?<path>\S+) (?<status>\d{3}) (?<bytes>\d+)")

(:named (re/match line-re (first log-lines)))
; => {:date "2026-11-15" :time "09:12:01" :method "GET"
;     :path "/index.html" :status "200" :bytes "5321"}
(re/match line-re "not a log line") ; => nil
(select-keys (re/match #"\d+" "abc 123") [:match :start :end])
; => {:match "123" :start 4 :end 7}
```

A new version of `parse-entry` uses it, returning `nil` for a line it
can't read.

```clojure
(defn parse-entry [line]
  (when-let [m (re/match line-re (str/trim line))]
    (-> (:named m)
        (update :status parse-long)
        (update :bytes parse-long))))

(def entries (keep parse-entry (conj (vec log-lines) "garbage")))

(mapv :path entries) ; => ["/index.html" "/about" "/login"]
```

The rest of `skein.regex`: `find`, `find-all`, and `matches` mirror the
core functions (and `find` and `match` take a starting index);
`match-all` gives the details of every match; `found?` and `matches?` are
predicates; `literal` makes a regex that matches a string exactly, quoting
any special characters.

```clojure
(re/find-all #"\d+" "1 22 333") ; => ["1" "22" "333"]
(re/find #"\d+" "a 12 b 34" 4) ; => "34"
(re/found? #"\d" "abc1") ; => true
(re/matches? #"\d+" "abc1") ; => false
(re/literal "1+1=2") ; => #"1\+1=2"
(re/found? (re/literal "1+1") "is 1+1 two?") ; => true
```

### Replacing and splitting with a regex

`str/replace` and `str/replace-first` accept a regex too. In the
replacement string, `$1` refers to the first group, `${name}` to a named
group, and `$$` to a literal dollar sign.

```clojure
(str/replace "2026-11-15" #"(\d+)-(\d+)-(\d+)" "$3/$2/$1") ; => "15/11/2026"
(str/replace "2026-11-15" #"(?<y>\d+)-(?<m>\d+)-(?<d>\d+)" "${d}.${m}.${y}")
; => "15.11.2026"
(str/replace "cost: 5" #"\d+" "$$$0") ; => "cost: $5"
```

The replacement can also be a function. It receives what `re-find` would
return for each match (the text, or a vector when there are groups) and
returns the replacement text.

```clojure
(str/replace "3 apples and 12 pears" #"\d+" (fn [n] (str (* 2 (parse-long n)))))
; => "6 apples and 24 pears"
(str/replace "hello world" #"(\w)(\w*)" (fn [[_ first-letter more]] (str more first-letter "ay")))
; => "ellohay orldway"
```

`re/replace` and `re/split` take the regex first, which reads better in a
`->>` pipeline. `re/split` also takes a limit.

```clojure
(re/split #"\s*,\s*" "a , b,c ,d") ; => ["a" "b" "c" "d"]
(re/split #"\s*,\s*" "a , b,c ,d" 2) ; => ["a" "b,c ,d"]
(re/replace #"\s+" "too   many    spaces" " ") ; => "too many spaces"
```

A pattern that isn't a valid regex is an error, signaled when the regex is
built: at read time for a literal, or when `re-pattern` runs.

```clojure
(re-pattern "(unclosed") ; !! RuntimeError
```

## Formatting with format and printf

`format` builds text from a template with printf-style directives, and
`printf` prints the result. Both are in `skein.core`. The directives are
`%s` (the value as `str` shows it), `%S` (as `pr-str` shows it), `%d`,
`%f`, `%e`, `%g`, `%x`, `%X`, `%o`, `%b`, `%c`, `%%` for a percent sign,
and `%n` for a newline.

```clojure
(format "%s scored %d points" "Ada" 42) ; => "Ada scored 42 points"
(format "%s vs %S" "text" "text") ; => "text vs \"text\""
(format "%.2f" 3.14159) ; => "3.14"
(format "%x %X %o %b" 255 255 8 5) ; => "ff FF 10 101"
(format "%c%c" \o \k) ; => "ok"
(format "100%%") ; => "100%"
```

A width pads a value to that many columns, on the left by default. The
flags are `-` (pad on the right), `0` (pad with zeros), `+` (always show a
sign), a space (a space where a plus sign would go), and `,` (group
thousands). A precision after a dot sets the digits after the point for
numbers, or the maximum length for `%s`.

```clojure
(format "[%6d][%-6d][%06d]" 42 42 42) ; => "[    42][42    ][000042]"
(format "%+d %+d" 5 -5) ; => "+5 -5"
(format "%,d" 1234567) ; => "1,234,567"
(format "%,.2f" 1234.5) ; => "1,234.50"
(format "%.3s" "abcdef") ; => "abc"
(format "[%-6s]" "東京") ; => "[東京  ]"
```

As with the padding functions, widths for `%s` count display columns, so
`format` lines up wide characters correctly.

`format` checks its arguments. A `%d` given a float, or a template with
more directives than arguments, signals an error instead of printing
something wrong.

```clojure
(format "%d" 3.7) ; !! RuntimeError
(format "%s and %s" "one") ; !! RuntimeError
```

Note that `%s` shows `nil` as `nil`, while `str` turns it into the empty
string.

```clojure
[(format "%s" nil) (str nil)] ; => ["nil" ""]
```

### Numbers for people: skein.format

`skein.format` has a handful of functions for presenting quantities to a
person.

```clojure
(require '[skein.format :as fmt])

(fmt/number 1234567) ; => "1,234,567"
(fmt/number 1234.5 :decimals 1) ; => "1,234.5"
(fmt/percent 0.256) ; => "26%"
(fmt/percent 0.2561 :decimals 1) ; => "25.6%"
(fmt/bytes-size 1536) ; => "1.5 KB"
(fmt/bytes-size 512) ; => "512 B"
(fmt/duration 1500) ; => "1.5s"
(fmt/duration 3725000) ; => "1h 2m"
(fmt/ordinal 22) ; => "22nd"
(fmt/plural 1 "file") ; => "1 file"
(fmt/plural 3 "file") ; => "3 files"
(fmt/plural 2 "child" "children") ; => "2 children"
```

`bytes-size` uses units of 1024. `duration` takes milliseconds and shows
the two largest units.

### Finishing the report

With parsing, padding, and formatting in hand, the log report comes
together. It groups the entries by path, adds up the bytes, and prints one
row per path.

```clojure
(defn report [entries]
  (let [by-path (group-by :path entries)]
    (println (str (str/pad-right "path" 12) (str/pad-left "hits" 5) (str/pad-left "sent" 10)))
    (doseq [path (sort (keys by-path))]
      (let [es (get by-path path)]
        (printf "%-12s%5d%10s%n" path (count es) (fmt/bytes-size (reduce + (map :bytes es))))))))

(report entries)
; prints: path         hits      sent
; prints: /about          1    1.2 KB
; prints: /index.html     1    5.2 KB
; prints: /login          1       0 B
```

## Pretty printing nested data

`prn` prints a value on one line, which is unreadable once the value is
large. `skein.pprint/pprint` prints it to fit a width (80 columns by
default, or the value of `skein.pprint/*width*`). Whatever fits on the rest
of a line prints as `pr-str` would show it. A collection that doesn't fit
puts its items on separate lines, indented under the first.

```clojure
(require '[skein.pprint :as pp])

(def project
  [{:name "web" :version "0.3.0" :deps ["skein.string" "skein.regex" "skein.format"]}
   {:name "cli" :version "1.0.0"}])

(pp/pprint project 40)
; prints: [{:name "web",
; prints:   :version "0.3.0",
; prints:   :deps ["skein.string" "skein.regex"
; prints:          "skein.format"]}
; prints:  {:name "cli", :version "1.0.0"}]
```

Small items such as numbers fill each line as far as the width allows.
`pprint-str` returns the text instead of printing it.

```clojure
(pp/pprint (vec (range 30)) 30)
; prints: [0 1 2 3 4 5 6 7 8 9 10 11 12
; prints:  13 14 15 16 17 18 19 20 21
; prints:  22 23 24 25 26 27 28 29]
(pp/pprint-str [1 2 3]) ; => "[1 2 3]"
```

A list that starts with a symbol looks like code, and `pprint` lays it out
the way code is usually written: the symbol and its first argument stay
together and the rest are indented by two.

```clojure
(pp/pprint '(defn greet [name] (str "Hello, " name "!")) 30)
; prints: (defn greet
; prints:   [name]
; prints:   (str "Hello, " name "!"))
```

`skein expand`, covered in [Chapter 14](14-macros.md), uses the same
printer to lay out expanded code.

## Ropes for text you edit

A string is the right representation for text you read, transform, and
write out. It is a poor one for text that changes a little at a time, such
as an editor buffer: every insertion into a string copies the whole thing.

`skein.rope` provides ropes: persistent trees of string chunks. Inserting,
deleting, slicing, and finding a line take a number of steps logarithmic in
the text's length. Like every Skein collection, a rope is a value. An edit
returns a new rope that shares most of its structure with the old one, so
keeping every previous version for undo costs little.

```clojure
(require '[skein.rope :as rope])

(def doc (rope/rope "one\ntwo\nthree"))

(rope/length doc) ; => 13
(rope/line-count doc) ; => 3
(rope/line doc 1) ; => "two"

(def edited (rope/insert doc 4 "TWO and "))

(rope/text edited) ; => "one\nTWO and two\nthree"
(rope/text doc) ; => "one\ntwo\nthree"
```

The original `doc` is unchanged. Other edits are `delete`, `replace`
(both take a start and an exclusive end), and `join`. `slice` and
`char-at` read without editing.

```clojure
(rope/text (rope/delete edited 4 12)) ; => "one\ntwo\nthree"
(rope/text (rope/replace doc 0 3 "1")) ; => "1\ntwo\nthree"
(rope/slice edited 4 12) ; => "TWO and "
(rope/char-at doc 4) ; => \t
```

The line functions are what an editor needs to map between a cursor
position and a line. Lines count from 0, and indexes count characters.

```clojure
(rope/line-of doc 5) ; => 1
(rope/line-start doc 2) ; => 8
(rope/line-end doc 0) ; => 3
```

Ropes stay fast as they grow. This builds a thousand-line document one
line at a time, then reads a line from the middle.

```clojure
(def big
  (reduce (fn [r i] (rope/insert r (rope/length r) (str "line " i "\n")))
          (rope/rope)
          (range 1000)))

(rope/line-count big) ; => 1001
(rope/line big 500) ; => "line 500"
```

Two ropes with the same text can have different internal shapes, so
compare ropes by their text: `(= (rope/text a) (rope/text b))`.

## Markdown

`skein.markdown` reads Markdown (CommonMark, with GitHub's tables,
strikethrough, and task lists) into plain Skein data, and renders that data
as HTML or plain text. Because the parsed form is ordinary vectors and
maps, you can inspect it and transform it with the collection functions
you already know.

```clojure
(require '[skein.markdown :as md])

(md/parse "# Notes\n\nSome *text* and `code`.")
; => [:doc [:heading {:level 1} "Notes"]
;     [:paragraph "Some " [:em "text"] " and " [:code "code"] "."]]
```

Each block is a vector that starts with a keyword tag, then an attribute
map when the block has attributes, then its contents. The namespace
documentation (`skein doc skein.markdown`) lists every tag. A list's items
hold their text in `:plain` blocks when the list is tight, and a task
list item records whether it's checked.

```clojure
(md/parse "- [x] write\n- [ ] test")
; => [:doc [:list {:ordered false :tight true}
;           [:item {:checked true} [:plain "write"]]
;           [:item {:checked false} [:plain "test"]]]]
```

`html` renders Markdown text, or parsed data, as HTML. It escapes the text
it writes. `text` strips the markup.

```clojure
(md/html "# Hi\n\nSome *text* & [a link](https://skein.dev).")
; => "<h1>Hi</h1>\n<p>Some <em>text</em> &amp; <a href=\"https://skein.dev\">a link</a>.</p>\n"
(md/text "# Hi\n\nSome *text*.") ; => "Hi\nSome text.\n"
```

Since the parsed document is data, transforming it before rendering is an
ordinary data transformation. This collects the text of every top-level
heading, which is the start of a table of contents.

```clojure
(def notes (md/parse "# Intro\n\nHello.\n\n# Usage\n\nRun it."))

(->> (rest notes)
     (filter #(= :heading (first %)))
     (mapv #(md/text [:doc %]))) ; => ["Intro\n" "Usage\n"]
```

## Summary

- Strings are immutable UTF-8 values. `count`, `nth`, `subs`, and every
  index in `skein.string` and `skein.regex` count characters, not bytes.
- Sequence functions over a string return vectors of characters. Rebuild a
  string with `apply str` or `str/join`.
- `str` and `print` make text for people; `pr-str` and `prn` make text that
  reads back as the same value.
- A grapheme is what a reader sees as one character. Use `graphemes`,
  `grapheme-count`, and `str/reverse` for it, and `width`, the padding
  functions, and `truncate` to lay text out in columns.
- Regexes are values written `#"…"`, with linear-time matching and no
  backreferences. `re-find`, `re-seq`, and `re-matches` cover most needs;
  `skein.regex` adds positions, named groups, replacement, and splitting.
- `format` and `printf` take printf-style directives with widths in display
  columns; `skein.format` presents numbers, sizes, and durations for people.
- `skein.pprint` prints nested data to fit a width, `skein.rope` holds text
  you edit in place, and `skein.markdown` turns Markdown into data and HTML.

---

[Contents](README.md) · Previous: [6. Sequences and transducers](06-sequences-and-transducers.md) · Next: [8. Modules and projects](08-modules-and-projects.md) <!-- nav -->
