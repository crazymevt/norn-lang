# 3. Values and expressions

[Contents](README.md) · Previous: [2. First steps](02-first-steps.md) · Next: [4. Functions and control flow](04-functions-and-control-flow.md) <!-- nav -->

Every Skein program is made of the same few kinds of values: numbers,
strings, characters, keywords, symbols, `nil`, booleans, and collections.
Code is written in those same values. This chapter covers how the reader
turns text into values, how the evaluator gives those values meaning, how
quoting stops evaluation, which values count as true, and the scalar
values one by one: `nil`, numbers, strings, characters, keywords, and
symbols. Collections get their own chapter, [Chapter 5](05-collections.md).

The examples are written the way the REPL shows them. A comment such as
`; => 5` is the value the form returns. Type the form at the `user=>`
prompt, or put it in a file and run it with `skein file.skn`.

## Text becomes data

Skein runs your program in two steps. First the *reader* turns source text
into data: lists, vectors, symbols, numbers, and so on. Then the
*evaluator* takes that data and computes a result. Most languages hide the
first step. In a Lisp it is visible, because the data the reader produces
is ordinary data you can build and inspect yourself.

You can call the reader directly with `read-string`. It returns the value
the text describes, without evaluating it.

```clojure
(read-string "(+ 1 2)") ; => (+ 1 2)
(read-string "{:name \"Ada\" :langs [:skein :ml]}") ; => {:name "Ada" :langs [:skein :ml]}
(count (read-string "(+ 1 2)")) ; => 3
```

The first result is a list of three items: the symbol `+` and the numbers
`1` and `2`. Nothing was added yet. That list only becomes a call when the
evaluator sees it.

Here is the notation the reader understands, in brief. The [syntax
reference](../reference/syntax.md#reader-notation) has the full table.

| Text | Reads as |
| --- | --- |
| `42`, `-7`, `0x1F`, `3/4`, `1.5`, `1.5e3` | Numbers: integers, a ratio, floats |
| `"text"` | A string |
| `\a`, `\space`, `\newline` | Characters |
| `:name`, `:app/name`, `::name`, `::alias/name` | Keywords |
| `name`, `app/name` | Symbols |
| `nil`, `true`, `false` | Nothing, and the two booleans |
| `(a b)`, `[a b]`, `{:k v}`, `#{a b}` | A list, vector, map, and set |
| `'x` | Shorthand for `(quote x)` |

Spaces, newlines, and commas all separate items. Commas are whitespace, so
you can use them where they help a reader, as in `{:x 1, :y 2}`, and leave
them out everywhere else.

There are four kinds of comment. `;` runs to the end of the line.
`#| … |#` is a block comment, and block comments nest. `#_` and `#;` each
skip exactly one following form, which is handy for switching off a whole
expression without finding its end.

```clojure
[1 #_ 2 3] ; => [1 3]
[1 #; (this whole list is skipped) 3] ; => [1 3]
[1 #| a block
      comment |# 4] ; => [1 4]
[1, 2, 3] ; => [1 2 3]
```

The `comment` macro is different: it is a form, and it evaluates to `nil`
without evaluating its body. People use it to keep example calls next to
the code they exercise.

```clojure
(comment
  (launch-rockets!)) ; => nil
```

## How evaluation works

The evaluator has a short list of rules.

1. Numbers, strings, characters, keywords, booleans, `nil`, and the empty
   list `()` evaluate to themselves.
2. A symbol evaluates to whatever it names: a local binding, or the value
   of a var defined with `def` or `defn`.
3. A vector, map, or set evaluates each of its items and builds a new
   collection from the results.
4. A nonempty list is an operation. Its first item says what to do with
   the rest.

The first three rules are short to show.

```clojure
42 ; => 42
:ready ; => :ready
"hello" ; => "hello"
() ; => ()
(def width 10)
width ; => 10
[width (* width 2)] ; => [10 20]
{:w width :h (+ width 5)} ; => {:w 10 :h 15}
```

`def` creates a *var* in the current namespace (`user`, at the REPL) and
gives it a value. After that, the symbol `width` evaluates to `10`
anywhere in the namespace. [Chapter 8](08-modules-and-projects.md) covers
namespaces.

The fourth rule is where most of the work happens. In an ordinary call,
Skein evaluates every item of the list, left to right, and then calls the
first value with the rest as arguments.

```clojure
(+ 1 2) ; => 3
(* (+ 2 3) 4) ; => 20
(str "width is " width) ; => "width is 10"
```

In `(* (+ 2 3) 4)`, the evaluator evaluates `*` to the multiplication
function, `(+ 2 3)` to `5`, and `4` to itself, then calls the function with
`5` and `4`. There is no operator precedence to remember, because every
operation is written with its own parentheses.

Skein is a *Lisp-1*: functions and other values share one namespace. The
symbol `+` names a function the same way `width` names a number, and you
can pass `+` to another function like any other value. (Common Lisp keeps
functions in a separate namespace. Skein does not.)

```clojure
(reduce + [1 2 3 4]) ; => 10
```

### Special forms and macros

An ordinary call evaluates all its arguments first, which is wrong for
some operations. `(if test then else)` must evaluate only one branch.
`(def name value)` must not evaluate `name`. Forms like these are *special
forms*, built into the compiler. Skein has only a handful: `quote`, `if`,
`do`, `def`, `var`, `recur`, and the primitive binding forms that `fn`,
`let`, and `loop` are built from.

Everything else that looks like syntax is a *macro*: a function that runs
at compile time, receives its arguments as unevaluated data, and returns
new code to compile in its place. `when`, `cond`, `let`, `defn`, and `->`
are all macros. You can see what a macro turns into with `macroexpand-1`:

```clojure
(macroexpand-1 '(when (> width 5) (println "wide") :wide)) ; => (if (> width 5) (do (println "wide") :wide))
```

`when` is an `if` with no else branch, and `do` runs several forms in
order and returns the value of the last.

```clojure
(do 1 2 3) ; => 3
(do) ; => nil
```

You rarely need to know whether a name is a special form or a macro. What
matters is that the first item of a list decides how the rest is treated.
[Chapter 14](14-macros.md) shows how to write your own macros.

## Quoting

Sometimes you want the data itself, not what it evaluates to. `quote`
returns its argument unevaluated, and the reader turns `'x` into
`(quote x)` for you.

```clojure
(quote (+ 1 2)) ; => (+ 1 2)
'(+ 1 2) ; => (+ 1 2)
'width ; => width
'[width (+ 1 2)] ; => [width (+ 1 2)]
```

A quoted list is a plain list of data. A quoted symbol is a symbol, not
the value it would name. Quoting reaches all the way down: inside
`'[width (+ 1 2)]` nothing is evaluated.

Because code is data, you can build code with ordinary functions and hand
it to `eval`, which runs the evaluator on it.

```clojure
(def form (list '* 6 7))
form ; => (* 6 7)
(eval form) ; => 42
(eval '{:area (* width width)}) ; => {:area 100}
```

You will rarely call `eval` in a program. It is here to show that the
evaluator works on data, which is the idea macros are built on. Macros
usually build their output with *syntax quote* (`` ` ``) rather than
`list`, which [Chapter 14](14-macros.md) explains.

The empty list needs no quote. `()` evaluates to itself, so `()` and `'()`
are the same value.

## Truth and falsehood

Skein has two booleans, `true` and `false`. Conditionals such as `if`,
`when`, `and`, and `or` accept any value as a test, though, and the rule
for what counts as true is short: **only `nil` and `false` are false.**
Everything else is true, including `0`, the empty string, and empty
collections.

```clojure
(if nil :yes :no) ; => :no
(if false :yes :no) ; => :no
(if 0 :yes :no) ; => :yes
(if "" :yes :no) ; => :yes
(if [] :yes :no) ; => :yes
(if () :yes :no) ; => :yes
```

The last line is the one Common Lisp programmers should notice. In Skein
the empty list `()` is a real, empty collection, and it is not `nil`.
Clojure programmers will find this familiar. To ask whether a collection
has items, use `empty?` or `seq`, which returns `nil` for an empty
collection.

```clojure
(= () nil) ; => false
(empty? ()) ; => true
(seq ()) ; => nil
(seq [1 2]) ; => (1 2)
```

The functions `boolean`, `not`, `true?`, `false?`, `nil?`, and `some?`
turn values into booleans or test for particular ones. `some?` means
"not `nil`", so it is true of `false`.

```clojure
[(boolean 0) (boolean nil) (not 0) (not nil)] ; => [true false false true]
[(true? 1) (true? true) (nil? false) (some? false)] ; => [false true false true]
```

## nil

`nil` means "nothing here". It is the value of an `if` with no else branch
when the test fails, of a map lookup that finds no key, and of a function
that returns nothing useful, such as `println`.

```clojure
(if false :never) ; => nil
(get {:a 1} :b) ; => nil
(:b {:a 1}) ; => nil
```

Most collection functions treat `nil` as an empty collection. That lets
you call them on a missing value without checking first.

```clojure
[(first nil) (count nil) (get nil :a) (seq nil)] ; => [nil 0 nil nil]
(conj nil 1) ; => (1)
(assoc nil :a 1) ; => {:a 1}
(into [] nil) ; => []
(str "a" nil "b") ; => "ab"
```

Arithmetic does not treat `nil` as zero. Adding `nil` to a number signals
a `TypeError`, which is usually what you want: a missing number is a bug
to find, not a value to guess.

```clojure
(+ 1 nil) ; !! TypeError
```

`rest` and `next` show the difference between "empty" and "nothing".
Both return everything after the first item. When there is nothing after
it, `rest` returns the empty list and `next` returns `nil`.

```clojure
(rest [1]) ; => ()
(next [1]) ; => nil
```

## Numbers

Skein has a *numeric tower*: integers of any size, exact ratios, and
floating-point numbers. Arithmetic picks the right kind for you and never
silently overflows.

### Integers

Integers are exact and unbounded. Small ones are stored compactly; when a
result outgrows a machine word, Skein moves to a big integer without you
asking. Both kinds are the same class, `Integer`, and compare equal
whenever their values are.

```clojure
(+ 4611686018427387903 1) ; => 4611686018427387904
(* 99999999999999999999 99999999999999999999) ; => 9999999999999999999800000000000000000001
(class-name (class-of 42)) ; => skein.core/Integer
(class-name (class-of (* 99999999999999999999 2))) ; => skein.core/Integer
```

Integer literals can be written in hexadecimal with `0x`. Leading zeros
mean nothing special: `07` is seven.

```clojure
0x1F ; => 31
07 ; => 7
```

The integer division functions are `quot` (truncates toward zero), `rem`
(remainder with the sign of the dividend), and `mod` (remainder with the
sign of the divisor). They accept integers only.

```clojure
[(quot 7 2) (quot -7 2)] ; => [3 -3]
[(rem -7 2) (mod -7 2)] ; => [-1 1]
(quot 7 2.0) ; !! TypeError
```

The bit operations `bit-and`, `bit-or`, `bit-xor`, `bit-not`,
`bit-shift-left`, and `bit-shift-right` work on integers of any size.

```clojure
(bit-shift-left 1 70) ; => 1180591620717411303424
```

### Ratios

Dividing two integers with `/` gives an exact answer. If the result is a
whole number, it is an integer. Otherwise it is a *ratio*, kept in lowest
terms.

```clojure
(/ 6 3) ; => 2
(/ 6 4) ; => 3/2
(+ 1/3 1/6) ; => 1/2
(* 3/4 4) ; => 3
[(numerator 6/4) (denominator 6/4)] ; => [3 2]
```

Ratios never lose precision, which makes them useful for money splits,
probabilities, and anything else where `0.1 + 0.2` should be exactly
`3/10`. Dividing an integer by zero signals an `ArithmeticError`.

```clojure
(+ 1/10 2/10) ; => 3/10
(/ 1 0) ; !! ArithmeticError
```

### Floats

A number with a decimal point or an exponent is a 64-bit IEEE floating
point number, called a `Float`. Floats are fast and approximate, with the
usual rounding.

```clojure
(+ 0.1 0.2) ; => 0.30000000000000004
1.5e3 ; => 1500.0
(/ 10 4.0) ; => 2.5
```

Any arithmetic that involves a float produces a float. Integers and ratios
combine into a ratio, or an integer when the result is whole.

```clojure
(+ 1 0.5) ; => 1.5
(+ 1/2 0.5) ; => 1.0
(+ 1 1/2) ; => 3/2
```

Floating-point division by zero follows IEEE rules and does not signal.
The reader writes the special values as `##Inf`, `##-Inf`, and `##NaN`.

```clojure
(/ 1.0 0) ; => ##Inf
(- (/ 1.0 0)) ; => ##-Inf
(NaN? (/ 0.0 0.0)) ; => true
```

To convert between kinds, use `double` (to a float), `long` or `int` (to
an integer, truncating toward zero), and the parsers `parse-long` and
`parse-double`, which return `nil` for text that is not a number.

```clojure
[(double 1/4) (long 7/2) (int -3.7)] ; => [0.25 3 -3]
[(parse-long "42") (parse-long "forty-two") (parse-double "2.5")] ; => [42 nil 2.5]
```

The usual predicates are there: `number?`, `integer?`, `ratio?`,
`rational?` (an integer or a ratio), `float?`, `zero?`, `pos?`, `neg?`,
`even?`, and `odd?`. So are `inc`, `dec`, `abs`, `min`, and `max`.

Powers, roots, rounding, trigonometry, and statistics live in the
`skein.math` library. `expt` stays exact when it can; `sqrt` returns a
float.

```clojure
(require '[skein.math :as math])
(math/expt 2 100) ; => 1267650600228229401496703205376
(math/expt 2/3 2) ; => 4/9
(math/sqrt 16) ; => 4.0
(math/round 2.5) ; => 3
(math/median [3 1 2]) ; => 2
(math/percentile [12 20 31 40 55] 95) ; => 52
```

Run `skein doc skein.math` for the full list.

### Equality: `=` and `==`

Skein has two ways to compare numbers, and the difference matters.

`=` is *value equality*. It works on every value, and for numbers it keeps
the three categories apart: an integer is never `=` to a float, and a ratio
is never `=` to a float, even when they denote the same quantity.

`==` is *numeric equality*. It works only on numbers and compares them by
value across categories.

```clojure
(= 1 1.0) ; => false
(== 1 1.0) ; => true
(= 1/2 0.5) ; => false
(== 1/2 0.5) ; => true
(= 2/4 1/2) ; => true
```

Why keep `1` and `1.0` apart under `=`? Because `=` is the equality that
maps and sets use, and it has to be a true equivalence relation that never
loses information. An exact `1` and an approximate `1.0` are different
values; a map can hold both as separate keys.

```clojure
(get {1 :exact} 1.0) ; => nil
(get {1 :exact} 1) ; => :exact
```

When you are asking a numeric question ("is this total zero?", "are these
two measurements the same?"), use `==` or the ordering functions `<`,
`<=`, `>`, `>=`, which also compare across categories. These are exact:
`==` compares a float with an integer or ratio by the float's exact binary
value, so `0.1` is not `==` to `1/10`, because `0.1` is not exactly one
tenth.

```clojure
(== 0.1 1/10) ; => false
(< 1/3 0.34) ; => true
(< 1 2 3) ; => true
```

The two also differ on the strange floats. Under `=`, `##NaN` equals
itself and `0.0` equals `-0.0`, so both can be found again as map keys.
Under `==`, NaN keeps its IEEE meaning and equals nothing, not even
itself.

```clojure
(= ##NaN ##NaN) ; => true
(== ##NaN ##NaN) ; => false
(= 0.0 -0.0) ; => true
```

`compare` and `sort` order numbers the way `<` does, across categories,
with NaN after every other number.

```clojure
(sort [3 1/2 0.25 2]) ; => [0.25 1/2 2 3]
(sort [##NaN 1 -1]) ; => [-1 1 ##NaN]
```

Clojure programmers will recognize `=` and `==`. Common Lisp programmers
can think of `=` as close to `equal` and `==` as close to CL's `=`. The
full rules are in the [collections
reference](../reference/collections.md#numbers-and-equality), and
[Chapter 5](05-collections.md) covers equality of collections.

## Strings

A string is an immutable sequence of Unicode text, written in double
quotes. The escapes are the familiar ones: `\n`, `\t`, `\"`, and `\\`.

```clojure
"tab\there" ; => "tab\there"
(println "line one\nline two")
; prints: line one
; prints: line two
```

`str` is the general way to build a string. It joins the printed forms of
its arguments, treats `nil` as the empty string, and with no arguments
returns `""`.

```clojure
(str "width: " width "px") ; => "width: 10px"
(str :done) ; => ":done"
(str) ; => ""
```

Strings are values: `=` compares their contents. `count` counts Unicode
code points, `subs` takes a slice by position, and `get` or `nth` returns
a single character.

```clojure
(= "skein" (str "no" "rn")) ; => true
(count "naïve") ; => 5
(subs "hello" 1 3) ; => "el"
(get "hello" 1) ; => \e
```

Two printing functions are worth knowing apart. `str` and `println` show a
string's contents. `pr-str` and `prn` show it the way the reader would
read it back, with quotes and escapes. The REPL prints results the second
way.

```clojure
(pr-str "say \"hi\"") ; => "\"say \\\"hi\\\"\""
(println "say \"hi\"")
; prints: say "hi"
(prn "say \"hi\"")
; prints: "say \"hi\""
```

`str` on a collection gives its contents as `print` would show them, so
strings inside it lose their quotes. Use `pr-str` when you need text that
reads back as the same value.

```clojure
(str [1 "x"]) ; => "[1 x]"
(pr-str [1 "x"]) ; => "[1 \"x\"]"
```

Everything else about text, including the `skein.string` library, regular
expressions, graphemes, and formatting, is in [Chapter 7](07-text.md).

## Characters

A character is a single Unicode code point, written with a backslash:
`\a`, `\Z`, `\7`. A few have names: `\space`, `\newline`, `\tab`, and
`\return`. A non-ASCII character can be written directly, as in `\é`.

```clojure
(first "abc") ; => \a
(int \a) ; => 97
(char 97) ; => \a
(str \n \o \r \n) ; => "skein"
(= \space (first " x")) ; => true
```

A string is not a vector of characters, and the two never compare equal,
but every sequence function accepts a string and walks its characters.

```clojure
(= "ab" [\a \b]) ; => false
(vec "ab") ; => [\a \b]
```

## Keywords

A keyword is a name that evaluates to itself. It starts with a colon:
`:name`, `:status`, `:http/get`. Keywords are the usual choice for map
keys, option names, and enumerated values such as `:pending` and `:done`,
because they are cheap to compare and read clearly.

```clojure
:pending ; => :pending
(= :pending :pending) ; => true
(identical? :pending :pending) ; => true
```

A keyword is also a function that looks itself up in a map. This is the
most common way to read a field.

```clojure
(def order {:id 7 :status :pending :total 42})
(:status order) ; => :pending
(:missing order) ; => nil
(:missing order :none) ; => :none
```

A keyword can have a namespace, written before a slash. Namespaced
keywords keep names from different parts of a program apart: `:billing/id`
and `:shipping/id` are different keys. A double colon qualifies a keyword
with the current namespace, so at the REPL `::id` means `:user/id`. Before
an alias it qualifies it with the namespace the alias stands for: after
`(require '[skein.string :as str])`, `::str/id` means `:skein.string/id`.

```clojure
::id ; => :user/id
(require '[skein.string :as str])
::str/id ; => :skein.string/id
(namespace :billing/id) ; => "billing"
(name :billing/id) ; => "id"
(keyword "status") ; => :status
(keyword "billing" "id") ; => :billing/id
```

## Symbols

A symbol is a name used in code: `width`, `map`, `skein.string/join`. When
the evaluator meets a symbol it looks up what the symbol names. Quoted, a
symbol is a value of its own, which is how code that manipulates code
refers to names.

```clojure
'width ; => width
(symbol? 'width) ; => true
(= 'width 'width) ; => true
(name 'skein.string/join) ; => "join"
(namespace 'skein.string/join) ; => "skein.string"
(symbol "width") ; => width
```

A symbol is not a keyword and not a string, and it never equals either.

```clojure
[(= 'a :a) (= 'a "a") (= :a "a")] ; => [false false false]
```

Unlike Clojure, symbols are not functions: `('a {'a 1})` signals an error.
Use `get` to look a symbol up in a map.

```clojure
(get '{a 1} 'a) ; => 1
```

A symbol that names a global definition names a *var*, the box that holds
the definition's current value. `(var name)`, written `#'name`, gives you
the var itself rather than its value. Vars are what make redefinition at
the REPL work: when you `def` a name again, every caller that goes
through the var sees the new value. [Chapter 12](12-live-programming.md)
builds on this.

```clojure
#'width ; => #'user/width
(var-get #'width) ; => 10
(def width 12)
width ; => 12
```

## Every value has a class

Every value in Skein, including numbers, strings, and `nil`, is an
instance of a class, and `class-of` tells you which. Classes are how
generic functions dispatch, so you can define a method on `Integer` or
`String` in the same way as on a class you wrote ([Chapter
9](09-classes-and-generic-functions.md)).

```clojure
(class-name (class-of "hi")) ; => skein.core/String
(class-name (class-of 3/4)) ; => skein.core/Ratio
(class-name (class-of nil)) ; => skein.core/Null
(class-name (class-of :k)) ; => skein.core/Keyword
(instance? Number 3/4) ; => true
```

For a quick answer at the REPL, `type` returns a keyword describing the
representation. It is finer-grained than the class: big and small
integers are both `Integer`, but `type` reports them separately.

```clojure
(type 1) ; => :integer
(type (* 99999999999999999999 2)) ; => :bigint
(type "hi") ; => :string
```

## Summary

- The reader turns text into data, and the evaluator turns data into
  results. `read-string` and `eval` expose each step.
- Most values evaluate to themselves, symbols evaluate to what they name,
  and a nonempty list is a call, a special form, or a macro, decided by its
  first item.
- `quote` (written `'`) returns a form unevaluated, which is how you write
  code as data.
- Only `nil` and `false` are false. `()`, `0`, and `""` are true, and `()`
  is a real empty list, not `nil`.
- Integers are unbounded, `/` on integers is exact and may give a ratio,
  and anything involving a float gives a float. `quot`, `rem`, and `mod`
  take integers only.
- `=` is value equality and keeps integers, ratios, and floats apart; `==` and `<` compare numbers across categories. Maps and sets use
  `=`.
- Strings are immutable Unicode text; `str` builds them, `pr-str` prints
  them readably. Keywords evaluate to themselves and look themselves up in
  maps. Symbols name things in code and are not callable.
- Every value has a class, which is what generic functions dispatch on.

---

[Contents](README.md) · Previous: [2. First steps](02-first-steps.md) · Next: [4. Functions and control flow](04-functions-and-control-flow.md) <!-- nav -->
