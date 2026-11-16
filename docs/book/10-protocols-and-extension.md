# 10. Protocols and extension

[Contents](README.md) · Previous: [9. Classes and generic functions](09-classes-and-generic-functions.md) · Next: [11. Conditions and restarts](11-conditions-and-restarts.md) <!-- nav -->

[Chapter 9](09-classes-and-generic-functions.md) introduced classes and
generic functions. This chapter is about fitting your types into the rest of
the language, and the language into your types. It covers protocols, which
group generic functions under one name; adding behavior to built-in classes;
making a class work as a collection, so `map`, `reduce` and `into` accept it;
choosing how instances compare; mutable classes, with the ownership rules
that keep them safe across fibers; redefining classes in a running program;
and a first look at the metaobject protocol, the layer underneath it all.

## Protocols

A generic function is a single operation. Often several operations belong
together: anything that can be drawn has a size and a way to draw it, and
anything that can be stored can be saved and loaded. A protocol names such a
group.

```clojure
(defprotocol Shape
  "Things with an area and a perimeter."
  (area [s])
  (perimeter [s]))
```

`defprotocol` defines one generic function for each entry, here `area` and
`perimeter`, each dispatching on its first argument. The protocol itself,
`Shape`, is a small map that records which functions belong to it.

`extend-type` gives a class methods for a protocol's functions. You write
each method like a `defn` body, with the instance as the first parameter:

```clojure
(defclass circle () ((r :required true)))
(defclass rect () ((w :required true) (h :required true)))

(extend-type circle Shape
  (area [c] (* 3 (:r c) (:r c)))
  (perimeter [c] (* 6 (:r c))))

(extend-type rect Shape
  (area [r] (* (:w r) (:h r)))
  (perimeter [r] (* 2 (+ (:w r) (:h r)))))

(area (circle :r 2))          ; => 12
(perimeter (rect :w 2 :h 5))  ; => 14
```

(The circle uses 3 for pi to keep the numbers whole.)

`satisfies?` asks whether a value's class has a method for every function in
a protocol:

```clojure
(satisfies? Shape (rect :w 1 :h 1)) ; => true
(satisfies? Shape "rect")           ; => false
```

Protocols in Norn are a thin layer over what Chapter 9 already showed.
`extend-type` expands into `defmethod` forms, and `area` is an ordinary
generic function, so you can also write `(defmethod area ((s square)) …)`
directly, add `:before` methods to it, or call it with `map`. The protocol
adds a name for the group and the `satisfies?` question. If you come from
Clojure, where protocols are a separate mechanism from multimethods, this is
the main difference: in Norn there is one dispatch system, and protocols are
a way of organizing it.

Two limits to know. Each protocol entry takes one parameter vector, so a
protocol function has a single arity; a second vector is an error. Use `&
more` for optional arguments. And in this version the docstrings
inside `defprotocol` aren't shown by `doc`, so put documentation that users
need in the protocol's own docstring or in your module's docs.

## Extending built-in classes

Every value has a class, and you can add methods for any of them. That means
you can teach existing types your protocol without wrapping them.

`extend-protocol` is `extend-type` turned around: one protocol, several
classes. Here a vector of two numbers counts as a rectangle, and an integer
as a square of that side:

```clojure
(extend-protocol Shape
  Vector
  (area [v] (* (first v) (second v)))
  (perimeter [v] (* 2 (+ (first v) (second v))))

  Integer
  (area [n] (* n n))
  (perimeter [n] (* 4 n)))

(mapv area [(circle :r 1) (rect :w 2 :h 3) [4 5] 6]) ; => [3 6 20 36]
```

One function, `area`, handled four different kinds of value, two of them
built in.

You can extend abstract classes too. A method on `Collection` covers
vectors, lists, maps and sets at once, and a method on `Null` decides what
the function does with `nil`:

```clojure
(defprotocol Describe
  (describe [x]))

(extend-protocol Describe
  Null
  (describe [_] "nothing")

  String
  (describe [s] (str "the text " (pr-str s)))

  Collection
  (describe [c] (str (count c) " items")))

(describe nil)        ; => "nothing"
(describe "hi")       ; => "the text \"hi\""
(describe [1 2 3])    ; => "3 items"
(describe {:a 1})     ; => "1 items"
(describe 42)         ; !! Error
```

A method on `Object` would cover the integer too, and every other value.
`satisfies?` doesn't count `Object` methods, though, since they say nothing
specific about a type: it answers whether the class itself, or one of its
more specific ancestors, has methods.

```clojure
(satisfies? Describe '(1 2)) ; => true
(satisfies? Describe 42)     ; => false
```

Extending a built-in class affects the whole program, since there is only one
`String` class. Methods for your own protocols on built-in classes are fine.
Adding methods to someone else's generic function for someone else's class
is riskier: two libraries might each define one, and the last loaded wins.

## Classes that act as collections

Some classes are containers: a playlist holds songs, a deck holds cards, a
matrix holds rows. You'd like `count`, `first`, `map`, `filter`, `reduce` and
`for` to work on them directly. Norn lets a class become a collection by
defining a method on one generic function, `collection-seq`, which returns
the items:

```clojure
(defclass playlist ()
  ((title :required true)
   (songs :initform [])))

(defmethod collection-seq ((p playlist)) (:songs p))

(def road-trip (playlist :title "Road trip" :songs ["Holiday" "Drive" "Home"]))

(count road-trip)          ; => 3
(first road-trip)          ; => "Holiday"
(vec road-trip)            ; => ["Holiday" "Drive" "Home"]
(map count road-trip)      ; => [7 5 4]
(reduce str road-trip)     ; => "HolidayDriveHome"
(into #{} road-trip)       ; => #{"Holiday" "Drive" "Home"}
(for [s road-trip :when (> (count s) 4)] s) ; => ["Holiday" "Drive"]
```

`collection-seq` may return a seq, a built-in collection such as a vector,
or `nil` when there are no items. The sequence functions then work on what
it returned. That's why `(map count road-trip)` gave a vector: the songs are
a vector, and Norn's sequence functions keep the type of their input, as
[Chapter 6](06-sequences-and-transducers.md) explains.

```clojure
(seq (playlist :title "Empty")) ; => nil
(empty? (playlist :title "Empty")) ; => true
```

By default `count` and `nth` walk the items `collection-seq` returns. When
your class can answer faster, define the two optional generic functions,
`collection-count` and `collection-nth`. An `nth` method should signal
`IndexError` for an index that isn't there, which `nth` with a default value
turns into the default:

```clojure
(defmethod collection-count ((p playlist)) (count (:songs p)))
(defmethod collection-nth ((p playlist) i) (nth (:songs p) i))

(nth road-trip 1)         ; => "Drive"
(nth road-trip 10)        ; !! IndexError
(nth road-trip 10 :none)  ; => :none
```

These methods are ordinary calls in the calling fiber. If one signals an
error, the caller's handlers see it, and it can do I/O or wait on a channel.

Being a collection is about reading. `conj`, `assoc` and `into` with your
instance as the target don't know how to build a new playlist, so they
signal an error. Write your own `add-song` function that returns an updated
instance with `with`.

## Choosing how instances compare

Chapter 9 showed that instances compare by class and slot values. That's
right for most data. Two options change it.

### Leaving a slot out

Some slots aren't part of what a value means: a cache, a timestamp of when
it was loaded, a precomputed hash. Mark such a slot `:compare false`, and
`=` and `hash` ignore it:

```clojure
(defclass document ()
  ((path :required true)
   (text :initform "")
   (rendered :initform nil :compare false)))

(def a (document :path "a.md" :text "# Hi"))
(def b (with a :rendered "<h1>Hi</h1>"))

(= a b)               ; => true
(= (hash a) (hash b)) ; => true
(= a (with a :text "# Bye")) ; => false
```

The slot is still there and readable. It doesn't count toward
equality.

### Comparing by identity

Some objects stand for a particular thing in the world: a player in a game,
an open session, a node in a graph that might have cycles. Two of them with
the same slots are still two different things. Give such a class the option
`(:equality :identity)`, and each instance is equal only to itself:

```clojure
(defclass player ()
  ((name :required true))
  (:equality :identity))

(def ada (player :name "Ada"))

(= ada ada)                  ; => true
(= ada (player :name "Ada")) ; => false
(= ada (with ada :name "Ada")) ; => false
```

`with` still works, but it makes a new instance, and so a new identity.
Identity classes are still immutable; if you need a thing whose state
changes, read on.

## Mutable classes

Immutable instances are the default, and you should reach for them first.
Occasionally you need an object that changes in place: a connection pool, a
buffer you fill in a hot loop, a counter many fibers update. Norn supports
this as an opt-in. Mark the class `(:mutable)`, and mark each slot that can
change with `:mutable true`:

```clojure
(defclass hit-counter ()
  ((label :required true)
   (hits :initform 0 :mutable true))
  (:mutable))

(def home (hit-counter :label "home"))

(set-slot! home :hits 5)
(update-slot! home :hits inc)
(update-slot! home :hits + 10)

(:hits home) ; => 16
```

`set-slot!` stores a value, and `update-slot!` applies a function to the
current value with any extra arguments, like `swap!` does for an atom. Only
slots declared `:mutable true` can change:

```clojure
(set-slot! home :label "index") ; !! RuntimeError
```

A mutable class always compares by identity, whatever else you say. An
object that changes can't have a stable hash, and a map that held it would
lose track of it. Its subclasses are mutable too, and `with` on a mutable
instance makes a copy.

```clojure
(= home (hit-counter :label "home")) ; => false
(= home home)                        ; => true
```

### Ownership

Mutable objects bring back the problem that immutable values avoid: two
fibers changing the same object at once. Norn's answer is ownership. A
mutable instance belongs to the fiber that made it, and only that fiber may
read or change it. Another fiber that tries gets an `OwnershipError`, a
clear error at the point of the mistake, instead of a race that shows up
weeks later as a wrong number.

The examples here start a second fiber with `with-scope` and `spawn`, which
[Chapter 15](15-fibers-and-scopes.md) covers properly. For now, read
`(with-scope [s] (await (spawn s expr)))` as "evaluate `expr` in another
fiber and wait for its result". An error in the child is signaled again in
the parent.

```clojure
(owner home) ; => #<fiber 1 running>

(with-scope [s] (await (spawn s (:hits home)))) ; !! OwnershipError
```

There are three ways to let another fiber use the object.

**Hand it over.** `release!` gives up ownership, and the next fiber to touch
the object becomes its owner. Use it when work moves from one fiber to
another, for example a request object passed to a worker:

```clojure
(release! home)
(owner home) ; => nil

(with-scope [s] (await (spawn s (update-slot! home :hits inc) (:hits home)))) ; => 17
```

When the owning fiber finishes, its objects become free again, so the main
fiber can read `home` once the worker is done:

```clojure
(:hits home) ; => 17
```

Putting a mutable object directly on a channel releases it for you, and the
fiber that takes it claims it. Chapter 16 covers channels. If the object is
nested inside other data you send, release it yourself first.

**Share it.** `share` makes an object usable by every fiber. Each change
takes a lock on the object, parking a fiber that has to wait, so concurrent
`update-slot!` calls never lose an update:

```clojure
(def site (share (hit-counter :label "site")))

(owner site) ; => :shared

(with-scope [s]
  (dotimes [_ 100]
    (spawn s (update-slot! site :hits inc))))

(:hits site) ; => 100
```

**Lock it for a group of changes.** One `update-slot!` is atomic. When a
change touches several slots, or reads one slot to compute another, wrap it
in `locking` so no other fiber sees it half done:

```clojure
(defclass wallet ()
  ((checking :initform 100 :mutable true)
   (savings :initform 0 :mutable true))
  (:mutable))

(def w (share (wallet)))

(defn move-to-savings! [w amount]
  (locking w
    (update-slot! w :checking - amount)
    (update-slot! w :savings + amount)))

(with-scope [s]
  (dotimes [_ 10]
    (spawn s (move-to-savings! w 10))))

[(:checking w) (:savings w)] ; => [0 100]
```

Reads of a shared object's single slot don't take the lock. Use `locking`
when several reads have to agree with each other.

Ownership checks cost one load and compare per access, and only for
instances of mutable classes. The printer, the debugger and the inspector
read objects without the check, since they only look. For state that many
fibers share, consider whether an atom holding an immutable value, or a
transaction over refs, fits better; [Chapter 17](17-shared-state.md)
compares the options.

## Redefining classes while the program runs

Norn is meant to be changed while it runs: you redefine a function at the
REPL, and every caller uses the new version on its next call.
[Chapter 12](12-live-programming.md) is about that workflow. Classes take
part in it too. Evaluating `defclass` again for an existing class updates
the class in place. It keeps its identity, so every reference to it, every
method specialized on it, and every existing instance stays valid.

```clojure
(defclass user ()
  ((name :required true)
   (email)))

(def ada-user (user :name "Ada" :email "ada@example.com"))
(def the-class user)

(defclass user ()
  ((name :required true)
   (role :initform :member)))

(identical? the-class user) ; => true
(instance? user ada-user)   ; => true
ada-user                    ; => #user/user{:name "Ada" :role :member}
```

The existing instance was made under the old definition, and it is read
through the new one:

- A slot the class gained, here `role`, reads as its `:initform`, evaluated
  once when the class was redefined, because instances already existed.
- A slot the class lost, here `email`, is gone. A keyword lookup gives `nil`
  or your default, and the constructor no longer accepts it.
- Equality and `with` use the current definition. `with` gives an instance
  in the new layout.

```clojure
(:email ada-user :gone)        ; => :gone
(with ada-user :role :admin)   ; => #user/user{:name "Ada" :role :admin}
(= ada-user (user :name "Ada")) ; => true
(user :name "Bo" :email "bo@example.com") ; !! Error
```

Nothing was copied or migrated. Instances are immutable, so there is no
state to convert. Each instance keeps the layout it was made with, and the
class decides how that layout is read.

Methods change the same way. A `defmethod` takes effect on the next call
everywhere, including in code that's already running and holds the generic
function:

```clojure
(defgeneric greet (u))
(defmethod greet ((u user)) (str "Hi, " (:name u)))

(def greeter (fn [] (greet ada-user)))
(greeter) ; => "Hi, Ada"

(defmethod greet ((u user)) (str "Welcome back, " (:name u)))
(greeter) ; => "Welcome back, Ada"
```

Generic functions keep a dispatch cache, and adding or replacing a method
invalidates it, on every core at once. You don't need to restart anything.

## A taste of the metaobject protocol

Classes and generic functions are themselves objects with classes. The
metaobject protocol (MOP) is the set of generic functions Norn uses to run
the object system: making instances, reading slots, printing, and choosing
methods. Specializing them changes how the object system behaves for the
classes you choose. You've already met two of them, `print-object` and
`initialize-instance`, in Chapter 9.

Start by looking. A class is an instance of `StandardClass`, and a generic
function is an instance of `StandardGenericFunction`:

```clojure
(identical? StandardClass (class-of circle)) ; => true
(class-of area)                             ; => #<class norn.core/GenericFunction>
(instance? StandardGenericFunction area)    ; => true
(class-name circle)                         ; => user/circle
(mapv :name (class-slots rect))             ; => [:w :h]
(class-direct-superclasses circle)          ; => [#<class norn.core/StandardObject>]
(mapv :specs (generic-methods perimeter))
; => [[#<class user/circle>] [#<class user/rect>] [#<class norn.core/Vector>]
;     [#<class norn.core/Integer>]]
```

`class-direct-subclasses` goes the other way, and `class-precedence-list`,
from Chapter 9, gives the full order of ancestors.

### Metaclasses

A metaclass is a class whose instances are classes. Define one as a subclass
of `StandardClass`, and name it in a class's `(:metaclass …)` option. Then
methods specialized on the metaclass apply to every class that uses it.

Here is a metaclass that records every slot read made through `slot-value`
or a reader, using `slot-value-using-class`, the generic function behind
`slot-value`:

```clojure
(defclass audited (StandardClass) ())

(def reads (atom []))

(defmethod slot-value-using-class ((c audited) obj slot)
  (swap! reads conj slot)
  (call-next-method))

(defclass secret ()
  ((pin :reader pin)
   (holder))
  (:metaclass audited))

(def s1 (secret :pin 1234 :holder "Ada"))

(pin s1)             ; => 1234
(slot-value s1 :holder) ; => "Ada"
@reads               ; => [:pin :holder]
(class-of secret)    ; => #<class user/audited>
```

A keyword lookup such as `(:holder s1)` reads the slot directly and doesn't
go through `slot-value-using-class`. So if a metaclass has to see every
read, have users read through readers. `make-instance` can be specialized on
a metaclass the same way, to change how all its classes construct instances.

### Generic function classes

Method selection is also open. A generic function made with
`(:generic-function-class C)`, where `C` is a subclass of
`StandardGenericFunction`, calls `compute-applicable-methods` and
`compute-effective-method` methods you specialize on `C`. Norn caches the
result for each combination of argument classes, so these run on the first
call for a combination, not on every call:

```clojure
(defclass logged-generic (StandardGenericFunction) ())

(def lookups (atom []))

(defmethod compute-applicable-methods ((gf logged-generic) args)
  (swap! lookups conj (mapv class-of args))
  (call-next-method))

(defgeneric shout (x) (:generic-function-class logged-generic))
(defmethod shout ((x String)) (str x "!"))

[(shout "hey") (shout "you")] ; => ["hey!" "you!"]
@lookups ; => [[#<class norn.core/String>]]
```

Both calls had a `String` argument, so methods were computed once.

Classes and generic functions that specialize none of these keep Norn's fast
paths: making, reading and dispatching go straight to the runtime. You pay
for the MOP only where you use it. The
[object system design](../design.md#43-meta-object-protocol-decided) and
the [reference](../reference/objects.md) list the protocol's generic
functions.

## Summary

- `defprotocol` names a group of generic functions that dispatch on their
  first argument. `extend-type` and `extend-protocol` add methods, and
  `satisfies?` checks for them. Protocols are ordinary generic functions
  underneath.
- Any class can get methods, including built-in ones such as `String`,
  `Vector`, `Collection` and `Null`.
- A method on `collection-seq` makes a class work with `count`, `seq`,
  `map`, `reduce`, `into` and the rest. `collection-count` and
  `collection-nth` make counting and indexing fast.
- `:compare false` leaves a slot out of equality and hashing.
  `(:equality :identity)` makes each instance equal only to itself.
- `(:mutable)` classes with `:mutable true` slots change in place with
  `set-slot!` and `update-slot!`. Each instance has an owner fiber; use
  `release!` to hand it over, `share` to let all fibers use it, and
  `locking` for multi-slot changes.
- Re-evaluating `defclass` updates the class in place. Existing instances
  are read through the new definition, and new methods apply on the next
  call everywhere.
- The metaobject protocol, through metaclasses and generic function classes,
  lets you change how instances are made, how slots are read and how methods
  are chosen.

---

[Contents](README.md) · Previous: [9. Classes and generic functions](09-classes-and-generic-functions.md) · Next: [11. Conditions and restarts](11-conditions-and-restarts.md) <!-- nav -->
