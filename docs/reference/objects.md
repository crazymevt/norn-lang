# Classes and generic functions

[Reference contents](README.md) · [Symbol index](symbols.md)

## Class definition and slots

**Macro** · `(defclass name (super…) (slot…) option…)` defines or updates
a class. An empty superclass list uses `StandardObject`. Multiple
inheritance uses C3 linearization to determine the class precedence list.
Redefining the class keeps its identity, and existing instances see the
current slot layout.

A slot specification is a symbol or a list such as
`(width :initform 1 :reader width)`. The slot options control initialization, access, and equality.

| Option | Meaning |
| --- | --- |
| `:initarg :k` | Keyword accepted at construction. It defaults to a keyword made from the slot name. |
| `:initform expr` | Default expression for a slot omitted at construction. |
| `:required true` | Require an argument if the slot has no default. |
| `:reader name` | Define a generic function that reads the slot. |
| `:compare false` | Exclude the slot from structural equality and hashing. |
| `:mutable true` | Allow in-place slot updates on instances of a class declared with `(:mutable)`. |

Instances compare by class and slot values by default.
`(:equality :identity)` compares instances by identity. An instance
of a `(:mutable)` class also uses identity equality.

## Construction and access

| Function | Behavior |
| --- | --- |
| `(make class :k v …)`, `(class :k v …)` | Construct an instance. Unknown initargs and missing required slots signal errors. |
| `(slot-value obj :slot)`, `(:slot obj)` | Read a slot. Keyword lookup accepts a fallback, as with a map. |
| `(with obj :slot value …)` | Return an instance with updated slot values. |
| `(class-of x)`, `(instance? class x)` | Get a value's class or test its membership in a class, including subclasses. |
| `(subclass? class superclass)`, `(class-precedence-list class)` | Test inheritance or inspect the ordered chain used for method selection. |

```clojure
(defclass rectangle ()
  ((width :required true :reader width)
   (height :initform 1 :reader height)))

(let [r (make rectangle :width 3)]
  [(width r)
   (height r)
   (:height (with r :height 4))
   (instance? rectangle r)]) ; => [3 1 4 true]
```

Mutable instances are owned by the fiber that creates them. Use
`set-slot!` or `update-slot!` on a declared mutable slot. Access by a
different fiber can signal `OwnershipError`. Use `release!` to relinquish
ownership, or `share` to allow concurrent reads and protect writes with
`(locking obj body…)`.

## Generic functions and methods

**Macro** · `(defgeneric name (params…) option…)` creates a generic
function. With class dispatch, every required argument participates in
method selection.

**Macro** · `(defmethod name qualifier? ((x Class) y …) body…)` installs
or replaces a method. An unspecialized parameter such as `y` accepts
`Object`. A specializer `(eql value)` matches a specific value. When
comparing applicable methods, Norn checks required arguments from left
to right and uses each argument's class precedence list. If no primary
method applies, the call signals an error.

```clojure
(defgeneric combine (x y))
(defmethod combine ((x Number) (y Number)) (+ x y))
(defmethod combine ((x String) (y String)) (str x y))
(defmethod combine ((x (eql :label)) y) (str "Label " y))

[(combine 2 3) (combine "no" "rn") (combine :label 4)] ; => [5 "norn" "Label 4"]
```

For value dispatch, give `defgeneric` the option `(:dispatch f)`. It uses
the result of applying `f` to the arguments. In this form, methods use
`(defmethod name dispatch-value [params…] body…)`. Use the dispatch value
`:default` for a fallback.

### Method combination

Standard combination runs the applicable `:around` methods from most
to least specific. An around method can call the next method or return
its own result. Inside the around chain, `:before` methods run from most
to least specific, the most specific primary method runs, and `:after`
methods run from least to most specific. The primary result is returned
unless an around method changes it.

In primary and around methods, `(next-method?)` tests whether there is
a next method. `(call-next-method)` calls it with the original arguments.
`call-next-method` can also take replacement arguments. Calling it with
no next method signals an error.

A generic function can choose a combination with
`(:method-combination name)`. Built-ins include `+`, `and`, and `list`.
Use `(define-method-combination name :operator op)` to combine
primary results with a named operator.

## Protocols and object customization

| Form | Behavior |
| --- | --- |
| `(defprotocol P (method [this args…]) …)` | Define a group of generic functions for first-argument dispatch. |
| `(extend-type Class P (method [this args…] body…) …)` | Install implementations for a class. |
| `(extend-protocol P Class (method [this args…] body…) …)` | Install implementations for several classes. |
| `(satisfies? P x)` | Test whether `x` has a specialized method for every method in the protocol. |

### Collections a class defines

A class becomes a collection to core functions with methods on three generic
functions in `norn.core`. `seq`, `count`, `nth`, `first`, `rest`, `next`,
`vec` and `set` call them for its instances, so `reduce`, `into`,
transducers, `map`, `sort` and the rest work too. Built-in collections never
reach them and keep their fast paths.

| Generic function | Behavior |
| --- | --- |
| `(collection-seq coll)` | Required. The items as a seq, a built-in collection, or `nil` when empty. Without a method, `seq` signals `TypeError`. |
| `(collection-count coll)` | Optional. How many items there are. The default counts `collection-seq`'s items. |
| `(collection-nth coll index)` | Optional. The item at `index`, or an `IndexError` when there is none. The default walks `collection-seq`. `(nth coll i default)` checks `i` against `collection-count` first. |

```clojure
(defclass countdown () ((from :initform 3)))
(defmethod collection-seq ((c countdown)) (range (:from c) 0 -1))

(vec (countdown :from 4))           ; => [4 3 2 1]
(reduce + (countdown :from 4))      ; => 10
```

The methods run as ordinary calls in the calling fiber: their errors reach
the caller's handlers, and they can park. `norn.buffer/Buffer` defines all
three, so `count` and `nth` are constant time for packed buffers.

The metaobject protocol exposes generic functions for object construction,
slot access, printing, and dispatch. Start with `make-instance`,
`initialize-instance`, `slot-value-using-class`, `print-object`,
`compute-applicable-methods`, and `compute-effective-method`. See the
[object system design](../design.md#43-meta-object-protocol-decided) for
the customization model.
