# 28. Foreign functions

[Contents](README.md) · Previous: [27. Images and deployment](27-images-and-deployment.md) · Next: [29. Norn for Lispers](29-norn-for-lispers.md) <!-- nav -->

Sooner or later a program needs something only a C library offers: a
system call the standard library doesn't wrap, a graphics API, a codec, a
vendor's SDK. `norn.ffi` calls C functions directly, with no glue code to
compile. This chapter shows how to declare and call C functions, pass
strings, buffers and structs, manage C memory, let C call back into Norn,
and choose which thread a call runs on. The examples call the C library
that comes with the operating system, so they run as they are.

Calling C gives up the guarantees the rest of Norn makes. A wrong signature
or a bad address can crash the whole process, not only signal a condition.
The FFI is built so that you know where that risk lives.

## An unsafe module

A module that calls C says so in its `ns` form with `(:unsafe)`:

```clojure
(ns clock.c
  (:require [norn.ffi :as ffi])
  (:unsafe))
```

The parts of `norn.ffi` that touch C or its memory, such as `library`,
`defcfun`, `alloc` and `callback`, are marked unsafe, and the expander
refuses them in any module that doesn't declare `(:unsafe)`. A module that
only requires `norn.ffi` can't call them by accident:

```clojure
(ns clock.ui
  (:require [norn.ffi :as ffi]))

(eval '(defn grab [] (ffi/alloc 8))) ; !! norn.expander/ExpandError
```

The message says what went wrong: "norn.ffi/alloc is unsafe (it can crash
the process): only a module that says (:unsafe) in its ns may use it." The
primitives underneath `norn.ffi` are refused the same way. At the REPL, the
`user` namespace may use them, so you can experiment.

The point is to keep the unsafe code small. Put the C declarations in one
module that says `(:unsafe)`, give it ordinary Norn functions as its
interface, and let the rest of the program use those. A module that uses an
unsafe module doesn't need to be unsafe itself. Struct layouts touch no
memory, so any module may define them.

## Calling a C function

`ffi/library` loads a dynamic library. `:c` names the C library and `:m`
the math library, wherever the operating system keeps them. Any other
library takes a path or a name the system's loader understands.
`defcfun` then defines a Norn function that calls a C function:

```clojure
(in-ns 'clock.c)

(def libc (ffi/library :c))

(ffi/defcfun strlen libc "strlen" [s :string] :usize)

(strlen "hello") ; => 5
(strlen "héllo") ; => 6
```

The parts are the Norn name, the library, the C name, the parameters as
name and type pairs, and the return type. `strlen` counts bytes, and `é`
takes two in UTF-8, so the second call returns 6. A `:string` parameter
passes a NUL-terminated UTF-8 copy of the string, made for the call and
freed after it. `nil` passes NULL, for the C functions that accept it, and
any other value is a `TypeError`.

The C types are keywords:

- Integers: `:i8`, `:u8`, `:i16`, `:u16`, `:i32` (or `:int`), `:u32`
  (`:uint`), `:i64`, `:u64` (`:usize`), and `:long` and `:ulong` for C's
  `long`, which is 64 bits except on Windows, where it's 32.
- Floats: `:f32` (`:float`) and `:f64` (`:double`).
- `:pointer`, an address as an integer, with `nil` for NULL.
- `:void`, for a function that returns nothing.

```clojure
(ffi/defcfun labs libc "labs" [n :long] :long)
(labs -5000000000) ; => 5000000000

(def libm (ffi/library :m))
(ffi/defcfun c-pow libm "pow" [x :double y :double] :double)
(c-pow 2.0 10.0) ; => 1024.0
```

A `:string` return copies the C string into a Norn string, or gives `nil`
for NULL:

```clojure
(ffi/defcfun getenv libc "getenv" [name :string] :string)
(ffi/defcfun strerror libc "strerror" [n :int] :string)

(getenv "NO_SUCH_VARIABLE_HERE") ; => nil
(strerror 2) ; => "No such file or directory"
```

Without `defcfun`, `ffi/function` makes a foreign function value, and
`ffi/call` calls it. That's useful when you choose the function at run
time:

```clojure
(def c-abs (ffi/function libc "abs" [:int] :int))
c-abs ; => #<foreign abs>
(ffi/call c-abs -7) ; => 7
```

### Checked signatures

`defcfun` gives the function a checked signature built from the C types
(contracts are covered in [Chapter 13](13-contracts.md)). A call from
another module that passes the wrong kind of value fails with a
`ContractError` before C sees it, and the error blames the caller:

```clojure
(in-ns 'clock.ui)

(clock.c/strlen "ok") ; => 2
(clock.c/strlen 42) ; !! ContractError
```

As with any checked signature, calls inside the defining module aren't
checked by default. The functions you export from an unsafe module are the
ones to keep honest, so validate arguments there, at the edge.

## Buffers, out parameters, and variadic functions

Many C functions write into a buffer you supply. An `:out` parameter takes
a size. The FFI allocates a fresh buffer of that size, passes its address,
and returns its contents as `Bytes` after the result. The function's
result is then a vector: the return value, then each `:out` buffer in
order.

`snprintf` is also variadic. End the parameter list with `& name`, and the
extra arguments can be integers, floats, strings, `Bytes` and `nil`:

```clojure
(in-ns 'clock.c)

(ffi/defcfun snprintf libc "snprintf" [buf :out n :usize fmt :string & xs] :int)

(let [[n buf] (snprintf 64 64 "%d items at %.2f" 3 9.5)]
  (subs (bytes->string buf true) 0 n))
; => "3 items at 9.50"
```

C's variadic functions read each argument as the format says, and nothing
checks that the two agree. Integers go as 64-bit integers and floats as
doubles, following C's promotions. Pass `1.0`, not `1`, where the format
expects a double:

```clojure
(let [[n buf] (snprintf 64 64 "%.1f" 2)]
  (subs (bytes->string buf true) 0 n))
; => "0.0"
```

A `:bytes` parameter passes a pointer to the data of a `Bytes` value you
already have, for C functions that read a buffer.

## C memory

Sometimes C wants memory that outlives one call, or a pointer to an array.
`ffi/alloc` takes a size in bytes, or a C type to make room for, and
returns the address of zeroed memory from C's allocator. `ffi/free!` gives
it back. `mem-get` and `mem-set!` read and write a C value of a given type
at an address and an optional byte offset:

```clojure
(defn ints->c
  "A C array of 32-bit ints holding xs: its address. free! it."
  [xs]
  (let [p (ffi/alloc (* 4 (count xs)))]
    (doseq [[i x] (map-indexed vector xs)]
      (ffi/mem-set! p :i32 (* 4 i) x))
    p))

(defn c->ints [p n]
  (mapv (fn [i] (ffi/mem-get p :i32 (* 4 i))) (range n)))

(let [p (ints->c [10 -20 30])]
  (try (c->ints p 3)
       (finally (ffi/free! p))))
; => [10 -20 30]
```

Memory from `alloc` isn't managed by the garbage collector. Free it
exactly once, and never use an address after freeing it. `with-alloc`
pairs the two for memory you need only while some code runs. It binds a
name to new memory, optionally fills it, runs the body, and frees the
memory however the body exits:

```clojure
(ffi/with-alloc [buf 16]
  (ffi/write-bytes! buf "hi there")
  (ffi/mem-set! buf :u8 8 0)
  [(ffi/string-at buf) (ffi/bytes-at buf 2)])
; => ["hi there" #bytes "6869"]
```

`write-bytes!` copies a string's UTF-8 or a `Bytes` value to an address,
`string-at` reads a NUL-terminated string, and `bytes-at` copies a number
of bytes out. `alloc-string` makes a NUL-terminated copy of a string in new
memory, for C that keeps the pointer. Reading through a NULL address is
caught and signalled rather than crashing:

```clojure
(ffi/mem-get 0 :i32) ; !! Error
```

Any other bad address isn't caught. It crashes the process, which is why
these functions are unsafe.

## Structs and unions

`defcstruct` describes a C struct's layout. Fields are laid out as C does:
each at the next offset its alignment allows, and the whole size rounded
up to the largest alignment. A field's type can be a C type keyword,
another struct, or an array, `[:array type n]`:

```clojure
(ffi/defcstruct point [x :i32 y :i32])

(ffi/defcstruct marker
  [tag :u8
   at point
   weight :double
   label [:array :u8 4]])

(ffi/sizeof marker) ; => 32
(ffi/offset-of marker :at) ; => 4
(ffi/offset-of marker :weight) ; => 16
(ffi/offset-of marker [:at :y]) ; => 8
```

The `:tag` byte is followed by three bytes of padding, because `point`
aligns to 4. `:weight` aligns to 8, so it starts at 16. A path of keys,
like `[:at :y]`, reaches into nested structs. A layout is a plain value, and
`ffi/sizeof` and `ffi/alignof` work on it as on the C type keywords.

Structs in C memory read and write as maps. `new-struct` allocates one and
fills it from a map, leaving other fields zero. `struct-get` reads the whole
struct, and `field` and `set-field!` read and write one field:

```clojure
(def m (ffi/new-struct marker {:tag 1 :at {:x 3 :y 4} :label [72 105 0 0]}))

(ffi/struct-get m marker)
; => {:tag 1 :at {:x 3 :y 4} :weight 0.0 :label [72 105 0 0]}

(ffi/set-field! m marker [:at :y] 40)
(ffi/field m marker :at) ; => {:x 3 :y 40}

(ffi/free! m)
```

`with-alloc` takes a layout and a map to fill it from:

```clojure
(ffi/with-alloc [p point {:x 1 :y 2}]
  (ffi/struct-get p point))
; => {:x 1 :y 2}
```

A bit field is `[:bits type width]`. Bits fill each unit of the type from
the lowest up, as C compilers on the supported platforms lay them out:

```clojure
(ffi/defcstruct status [ready [:bits :u8 1] level [:bits :u8 3] mode [:bits :u8 4]])

(ffi/with-alloc [p status {:ready 1 :level 5 :mode 9}]
  [(ffi/sizeof status) (ffi/mem-get p :u8)])
; => [1 155]
```

`defcunion` describes a union: every field starts at offset 0, and the size
is the largest field's. Reading a field reinterprets the same memory:

```clojure
(ffi/defcunion word [i :i64 d :f64])

(ffi/with-alloc [p word]
  (ffi/set-field! p word :d 1.0)
  (format "%x" (ffi/field p word :i)))
; => "3ff0000000000000"
```

### Passing structs to C

Most C APIs pass structs by pointer. A `[:* layout]` parameter takes a map,
copies it into temporary memory for the call, and passes the address. It
takes an address too, when the struct already lives in C memory. A
`[:out layout]` parameter is a struct for C to fill. You pass `nil` for it,
and the struct comes back as a map after the result.

The C library's own `gmtime_r` shows both. It reads a `time_t` and fills a
`struct tm`:

```clojure
(ffi/defcstruct tm
  [sec :int min :int hour :int mday :int mon :int year :int
   wday :int yday :int isdst :int gmtoff :long zone :string])

(ffi/defcstruct time-t [t :i64])

(ffi/defcfun gmtime-r libc "gmtime_r" [t [:* time-t] out [:out tm]] :pointer)

(let [[_ tm-value] (gmtime-r {:t 0} nil)]
  (select-keys tm-value [:year :mon :mday :hour :wday]))
; => {:year 70 :mon 0 :mday 1 :hour 0 :wday 4}
```

That's Thursday, January 1, 1970. C counts years from 1900 and months from
0. A `:string` field is a `char *`, so it reads and writes as an address,
here `:zone`. `ffi/string-at` reads the text, as in
`(ffi/string-at (:zone tm-value))`. To put your own text in such a field,
make a copy in C memory with `alloc-string` and `free!` it when C is done
with it. Because the field holds an address both ways, a struct read from
C can be passed straight back. Here `strftime` formats the time:

```clojure
(ffi/defcfun strftime libc "strftime" [buf :out max :usize fmt :string t [:* tm]] :usize)

(defn utc-text [seconds]
  (let [[_ tm-value] (gmtime-r {:t seconds} nil)
        [n buf] (strftime 64 64 "%Y-%m-%d %H:%M" tm-value)]
    (subs (bytes->string buf true) 0 n)))

(utc-text 1700000000) ; => "2023-11-14 22:13"
```

A layout used as a parameter or return type, without `:*`, passes the
struct by value. The C library's `ldiv` returns one:

```clojure
(ffi/defcstruct ldiv-t [quot :long rem :long])
(ffi/defcfun ldiv libc "ldiv" [num :long den :long] ldiv-t)

(ldiv 17 5) ; => {:quot 3 :rem 2}
(ldiv -17 5) ; => {:quot -3 :rem -2}
```

Structs by value and variadic functions work on 64-bit ARM (macOS and
Linux) and on x86-64 Linux in this release. Elsewhere they signal an error.

## Callbacks

Some C functions take a function pointer and call it. `qsort` is the
classic: it sorts an array and calls a comparison function for each pair
it compares. A `[:callback [param-types] return-type]` parameter accepts a
Norn function:

```clojure
(ffi/defcfun qsort libc "qsort"
  [base :pointer n :usize size :usize cmp [:callback [:pointer :pointer] :int]] :void)

(defn compare-ints [a b]
  (compare (ffi/mem-get a :i32) (ffi/mem-get b :i32)))

(let [p (ints->c [5 3 9 1 7])]
  (qsort p 5 4 compare-ints)
  (try (c->ints p 5) (finally (ffi/free! p))))
; => [1 3 5 7 9]
```

`qsort` hands the comparison two pointers into the array, and
`compare-ints` reads the ints they point at. The first time you pass a
function for a callback type, the FFI makes a C function pointer for it,
and keeps it for the next call with the same function.

`ffi/callback` makes the C function pointer yourself. Do this when C keeps
the pointer after the call returns, for example to call it on an event
later. `callback-address` gives its address, to store in a struct or pass
as a `:pointer`. Free it with `free-callback!` once C won't call it again:

```clojure
(def descending
  (ffi/callback [:pointer :pointer] :int (fn [a b] (compare-ints b a))))

descending ; => #<callback [:pointer :pointer] :int>

(let [p (ints->c [5 3 9 1 7])]
  (qsort p 5 4 descending)
  (try (c->ints p 5) (finally (ffi/free! p))))
; => [9 7 5 3 1]

(ffi/free-callback! descending)
```

`ffi/function-at` goes the other way. It makes a foreign function from an
address, for a function pointer that C hands you.

A condition can't unwind through C's stack frames. When a callback signals
an error, C gets 0 from it and carries on, and the error is signalled in
Norn when the C call returns:

```clojure
(let [p (ints->c [2 1])]
  (try (qsort p 2 4 (fn [_ _] (/ 1 0)))
       (finally (ffi/free! p))))
; !! ArithmeticError
```

When C calls back from a thread of its own, or from the blocking pool
described next, the call goes to a fiber that serves that callback, while
the C thread waits for the answer. An error there is reported on standard
error, since there's no Norn call to signal it in. Callbacks can take and
return structs by value too, as maps.

## Which thread runs the call

Norn runs fibers on a few carrier threads (see
[Chapter 15](15-fibers-and-scopes.md)). By default a foreign call runs
right on the carrier of the fiber that makes it. That is the fastest way,
and right for functions that return quickly, like everything so far. A
call that takes long holds the carrier, and the runtime starts a spare
carrier to keep other fibers running.

For a call that blocks, such as one that waits on a device, a lock or the
network, pass `:blocking true`. Its calls run on the blocking pool, and the
calling fiber parks until the call returns, so its carrier stays free:

```clojure
(ffi/defcfun usleep libc "usleep" [us :uint] :int :blocking true)

(with-scope [s]
  (mapv await (mapv (fn [_] (spawn s (usleep 100000))) (range 4))))
; => [0 0 0 0]
```

The four sleeps run side by side, and the other fibers in the program carry
on meanwhile.

Some APIs, such as window systems and graphics on macOS, must be called
from the process's first thread. `:main-thread true` sends a function's
calls there. The `norn` command keeps its first thread free for these
calls, and runs them one at a time. A program that embeds Norn serves them
only if it says so, and `ffi/main-thread-available?` tells you:

```clojure
(ffi/main-thread-available?) ; => true

(ffi/defcfun on-main-thread libc "pthread_main_np" [] :int :main-thread true)
(ffi/defcfun on-this-thread libc "pthread_main_np" [] :int)

[(on-main-thread) (on-this-thread)] ; => [1 0]
```

`pthread_main_np` is macOS's way to ask whether this is the first thread.
A callback that C makes during a main-thread call can make main-thread
calls of its own. A function can't be both `:blocking` and
`:main-thread`.

## Foreign functions and images

A library handle is a resource of the kind [Chapter 27](27-images-and-deployment.md)
describes, but you don't need to reconnect it. After an image restore,
libraries load again and foreign functions find their C functions again the
first time they're called, because a `Library` holds no state from the
session. A program that declares its C functions at the top level works the
same from an image:

```clojure
(require '[norn.process :as proc] '[norn.sys :as sys])

(defn main [& words]
  (println (mapv strlen words))
  0)

(save-image "lengths.nimg" :entry #'main)

(:out (proc/sh (:executable (sys/info)) "--image" "lengths.nimg" "a" "héllo"))
; => "[1 6]\n"
```

What doesn't survive is anything that lives in C's memory. Memory from
`alloc`, structs C made, callbacks, and functions from `function-at` are
gone after a restore. Treat them like open files: make them again after
the restore, with `on-image-restore` or when they're next needed.

## Summary

- A module that calls C declares `(:unsafe)`. Other modules may use it, but
  can't call `norn.ffi`'s unsafe functions themselves.
- `ffi/library` loads a library, with `:c` and `:m` for the system's own.
  `defcfun` defines a Norn function from a C name, typed parameters, and a
  return type.
- `defcfun` functions have checked signatures, so a wrong argument from
  another module is a `ContractError`, not a crash.
- `:string`, `:bytes` and `:out` handle text and buffers. `& rest` makes a
  variadic call, where floats must be passed as floats.
- `alloc`, `free!`, `mem-get`, `mem-set!` and `with-alloc` manage C memory,
  which the collector doesn't track.
- `defcstruct` and `defcunion` lay out structs and unions as C does. Structs
  read and write as maps, by pointer with `[:* layout]` and `[:out layout]`,
  or by value.
- C calls Norn back through `[:callback …]` parameters or `ffi/callback`.
  An error in a callback is signalled when the C call returns.
- Calls run on the carrier by default, `:blocking true` moves them to the
  blocking pool, and `:main-thread true` to the process's first thread.

---

[Contents](README.md) · Previous: [27. Images and deployment](27-images-and-deployment.md) · Next: [29. Norn for Lispers](29-norn-for-lispers.md) <!-- nav -->
