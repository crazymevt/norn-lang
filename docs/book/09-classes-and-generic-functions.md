# 9. Classes and generic functions

[Contents](README.md) · Previous: [8. Modules and projects](08-modules-and-projects.md) · Next: [10. Protocols and extension](10-protocols-and-extension.md) <!-- nav -->

Maps and vectors carry most data in Norn, and for a lot of programs they are
all you need. Sooner or later, though, you want a name for a kind of thing:
a `point`, an `invoice`, a `parse-error`. You want to say which fields it
has, give them defaults, and write operations that do different things for
different kinds of values. This chapter covers Norn's object system, which
comes from the Common Lisp Object System (CLOS). You define classes with
`defclass`, and operations with `defgeneric` and `defmethod`. Methods
belong to operations, not to classes, and they can be chosen by the classes
of all their arguments.

Norn's big change from CLOS is that instances are immutable values, like
maps. The chapter starts there, then moves on to generic functions, multiple
dispatch, dispatch on values, method combination, and the two hooks you
reach for most: `print-object` and `initialize-instance`.
[Chapter 10](10-protocols-and-extension.md) continues with protocols,
classes that act as collections, mutable classes, and redefining classes
while a program runs.

## Defining a class

A class has a name, a list of superclasses, and a list of slots. A slot is
a named field. Here is a point with two slots:

```clojure
(defclass point ()
  ((x :initform 0)
   (y :initform 0)))
```

The empty list `()` means the class has no superclasses of its own, so it
inherits from `StandardObject`, the root of all user classes. Each slot is
a list that starts with its name. `:initform 0` gives the slot a default
value for when you don't supply one.

A class is also its own constructor. Call it with keyword arguments, one per
slot:

```clojure
(point :x 1 :y 2) ; => #user/point{:x 1 :y 2}
(point :x 5)      ; => #user/point{:x 5 :y 0}
(point)           ; => #user/point{:x 0 :y 0}
```

`make` does the same thing when the class is held in a variable or chosen
at run time:

```clojure
(make point :x 3 :y 4) ; => #user/point{:x 3 :y 4}
```

An instance prints as its class name, with a namespace, followed by its
slots in braces. That printed form is for people. The reader doesn't read it
back, so if you need to save instances as text, convert them to maps first
(see below) or define your own printed form with `print-object`, covered
later in the chapter.

The keyword for each argument comes from the slot name, so the slot `x` is
set with `:x`. An argument the class doesn't know is an error, which catches
typos early:

```clojure
(point :z 1) ; !! Error
```

## Reading slots

There are three ways to read a slot. The shortest is to call the slot's
keyword on the instance, as you would with a map:

```clojure
(def p (point :x 1 :y 2))

(:x p)          ; => 1
(:z p :missing) ; => :missing
```

As with a map, a keyword lookup can take a default, and a slot the class
doesn't have gives `nil` or the default. `get` and `contains?` work the same
way. `slot-value` is stricter, and signals an error for a slot that doesn't
exist:

```clojure
(slot-value p :y) ; => 2
(slot-value p :z) ; !! RuntimeError
```

The third way is a reader. The `:reader` slot option defines a function
that reads the slot:

```clojure
(defclass circle ()
  ((radius :required true :reader radius)
   (label :initform "circle")))

(radius (circle :radius 3)) ; => 3
```

Readers are worth using in code that other people call. A reader is a
generic function, so a subclass can change what it returns, which a
keyword lookup can't do. You'll see this in the section on inheritance.

The `:required true` option above means the constructor has no default to
fall back on. Leaving the slot out is an error:

```clojure
(circle :label "plain") ; !! Error
```

## Slot options

A slot can take these options:

- `:initform expr` gives a default. The expression is evaluated again for
  each instance made without the slot, and never when the class is
  defined. A redefinition that adds a slot evaluates its initform once,
  for the instances that already exist (see
  [Chapter 10](10-protocols-and-extension.md)).
- `:required true` makes the slot mandatory when it has no `:initform`.
- `:reader name` defines a reader function.
- `:initarg :k` changes the keyword the constructor accepts. It defaults to
  the slot's name as a keyword.
- `:doc "…"` describes the slot.
- `:compare false` and `:mutable true` change equality and allow updates in
  place. [Chapter 10](10-protocols-and-extension.md) covers both.

Here is `:initarg` at work. The slot is called `title`, but the
constructor takes `:subject`:

```clojure
(defclass ticket ()
  ((title :initarg :subject :required true)
   (tags :initform #{})))

(ticket :subject "Printer jam") ; => #user/ticket{:title "Printer jam" :tags #{}}
(:title (ticket :subject "No coffee")) ; => "No coffee"
(ticket :title "Oops") ; !! Error
```

A slot can also carry a `:type`, as in `(n :type Integer)`. The type is
written as a signature's is ([Chapter 13](13-contracts.md)): a class, `Any`,
`(? T)`, `(U A B)`, `(Vec T)`, `(Map K V)` or `(-> A R)`. It's checked
whenever an instance is made or changed, by the constructor, `with`,
`assoc`, `set-slot!` or `update-slot!`. A value of the wrong type signals a
`ContractError` naming the class, the slot, the type and the value:

```clojure
(defclass counter () ((n :type Integer :initform 0)))

(counter :n 3) ; => #user/counter{:n 3}
(counter :n "3") ; !! ContractError
```

## Instances are values

In CLOS, an instance is a mutable object: you change its slots in place,
and everyone holding it sees the change. A Norn instance is closer to a map.
It never changes after it is made. To "change" one, you make a new instance
that differs in some slots. That's what `with` does:

```clojure
(def p2 (with p :x 10))

p2 ; => #user/point{:x 10 :y 2}
p  ; => #user/point{:x 1 :y 2}
```

`with` takes any number of slot and value pairs, and it signals an error for
a slot the class doesn't have. The original `p` is untouched, and since
instances never change, it's safe to share them between functions and
between fibers without copying or locking.

Because instances are values, they compare by value. Two instances are equal
when they have the same class and equal slots, and equal instances hash the
same, so they work as map keys and set members:

```clojure
(= (point :x 1 :y 2) (point :x 1 :y 2)) ; => true
(= (point :x 1 :y 2) {:x 1 :y 2})       ; => false
(contains? #{(point :x 1 :y 2)} (point :x 1 :y 2)) ; => true
```

An instance is never equal to a map with the same entries. Its class is part
of its value.

If you come from Clojure, a Norn class is something like a record: named
fields, value semantics, keyword access. There are differences. An instance
is not a map: `map?` returns false, and `seq` and `count` don't work on it.
It holds only the slots its class declares. When you want the slots as a
map, ask for one:

```clojure
(keys p)          ; => (:x :y)
(vals p)          ; => (1 2)
(instance->map p) ; => {:x 1 :y 2}
```

This is how you turn instances into data for JSON, EDN or a database row.

## Inheritance

A class lists its superclasses after its name. A subclass has every slot of
its superclasses, plus its own:

```clojure
(defclass account ()
  ((owner :required true :reader owner)
   (balance :initform 0 :reader balance)))

(defclass savings (account)
  ((rate :initform 1/50)))

(def acct (savings :owner "Ada" :balance 100))

acct ; => #user/savings{:owner "Ada" :balance 100 :rate 1/50}
(balance acct) ; => 100
```

The reader `balance` was defined for `account`, and it works on a `savings`
instance because a `savings` is an `account`. `instance?` asks whether a
value belongs to a class or any of its subclasses, and `subclass?` asks the
same of two classes:

```clojure
(instance? account acct)         ; => true
(instance? savings (account :owner "Bo")) ; => false
(subclass? savings account)      ; => true
(class-of acct)                  ; => #<class user/savings>
```

A class can have more than one superclass. Norn orders a class and its
ancestors into one list, the class precedence list, and uses it whenever it
needs to know which class is more specific. The order follows the C3
algorithm, which keeps each class's own order of superclasses and puts every
class before its superclasses:

```clojure
(defclass named () ((name)))
(defclass dated () ((date)))
(defclass document (named dated) ((body)))

(class-precedence-list document)
; => [#<class user/document> #<class user/named> #<class user/dated>
;     #<class norn.core/StandardObject> #<class norn.core/Object>]
```

Methods for `named` come before methods for `dated` when both apply to a
`document`, because `named` is listed first.

## Every value has a class

Classes aren't only for your own types. Every value in Norn has one, and the
built-in classes are capitalized names in `norn.core`:

```clojure
(class-of 42)     ; => #<class norn.core/Integer>
(class-of 1/3)    ; => #<class norn.core/Ratio>
(class-of "text") ; => #<class norn.core/String>
(class-of :k)     ; => #<class norn.core/Keyword>
(class-of nil)    ; => #<class norn.core/Null>
(class-of [1 2])  ; => #<class norn.core/Vector>
(class-of {})     ; => #<class norn.core/Map>
(class-of inc)    ; => #<class norn.core/Function>
(class-of point)  ; => #<class norn.core/Class>
```

The built-in classes have superclasses too, so you can talk about groups of
types. An integer is a `Number`, and a vector is a `Sequential` and a
`Collection`:

```clojure
(class-precedence-list Integer)
; => [#<class norn.core/Integer> #<class norn.core/Number> #<class norn.core/Object>]
(class-precedence-list Vector)
; => [#<class norn.core/Vector> #<class norn.core/Sequential>
;     #<class norn.core/Collection> #<class norn.core/Object>]
```

`Object` is at the top of every list. Other built-in classes include
`Float`, `Boolean`, `Character`, `Symbol`, `List`, `Seq`, `Set`, `Atom`,
`Fiber` and `Channel`. The names are capitalized so they never collide with
functions such as `vector` or `list`. Your own classes can use any name; the
reference docs and this book write them in lowercase.

The point of all this is the next section: methods can be written for any
of these classes, built-in or not.

## Generic functions and methods

A generic function is a function whose behavior depends on its arguments'
classes. You declare it with `defgeneric`, giving its name and parameters,
and add behavior for particular classes with `defmethod`:

```clojure
(defclass square () ((side :required true)))
(defclass rect () ((w :required true) (h :required true)))

(defgeneric area (shape))

(defmethod area ((s square)) (* (:side s) (:side s)))
(defmethod area ((r rect)) (* (:w r) (:h r)))

(area (square :side 3))   ; => 9
(area (rect :w 2 :h 5))   ; => 10
```

In a `defmethod`, a parameter written as `(s square)` is specialized: the
method applies only when that argument is an instance of `square` or a
subclass of it. When you call `area`, Norn finds the methods that apply to
the argument and runs the most specific one.

Calling a generic function with an argument no method accepts is an error:

```clojure
(area "circle") ; !! Error
```

The method doesn't live inside the class. It belongs to the generic
function, and you can add one from anywhere, including for a class you
didn't write. Here is a method on the built-in `Vector` class, so a pair
like `[2 3]` can stand for a rectangle:

```clojure
(defmethod area ((v Vector)) (* (first v) (second v)))

(area [2 3]) ; => 6
```

A method specialized on `Object` applies to everything, so it works as a
fallback:

```clojure
(defmethod area ((x Object)) 0)

(area "circle") ; => 0
```

A few details about generic functions:

- `defmethod` creates the generic function if it doesn't exist yet, so
  `defgeneric` is optional. Writing it anyway documents the parameters in one
  place.
- Defining a method for the same classes again replaces the old one. This is
  how you fix a method at the REPL.
- A generic function is a value like any other. You can pass `area` to
  `map`: `(map area shapes)`.
- An unspecialized parameter, such as `y` in `((x square) y)`, accepts any
  value. It's the same as specializing it on `Object`.

Readers are generic functions too, which is why a subclass can change one.
Here `bonus` overrides the `balance` reader for its instances and uses
`call-next-method`, covered below, to get the stored value:

```clojure
(defclass bonus (account) ((extra :initform 10)))

(defmethod balance ((b bonus)) (+ (call-next-method) (:extra b)))

(balance (bonus :owner "Cy" :balance 50)) ; => 60
(balance (account :owner "Di" :balance 50)) ; => 50
```

## Multiple dispatch

Methods in most object systems are chosen by one argument, the receiver.
A Norn generic function looks at the classes of all its required arguments.
This is called multiple dispatch, and it fits problems where the right
behavior depends on two things at once.

The classic example is a collision between two objects in a game:

```clojure
(defclass asteroid () ())
(defclass ship () ())

(defgeneric collide (a b))

(defmethod collide ((a Object) (b Object)) :pass-by)
(defmethod collide ((a asteroid) (b asteroid)) :bounce)
(defmethod collide ((a ship) (b asteroid)) :ship-damaged)
(defmethod collide ((a asteroid) (b ship)) (collide b a))

(collide (ship) (asteroid))     ; => :ship-damaged
(collide (asteroid) (ship))     ; => :ship-damaged
(collide (asteroid) (asteroid)) ; => :bounce
(collide (ship) (ship))         ; => :pass-by
```

When several methods apply, Norn sorts them by specificity, comparing the
arguments from left to right. For each argument, it uses that argument's
class precedence list: a method on `ship` is more specific than one on
`Object` for a `ship`. The first argument where two methods differ decides.

Multiple dispatch replaces the "visitor" patterns and `instanceof` chains
you'd write in a single-dispatch language. You write one method per
combination you care about, and a fallback for the rest.

## Methods for particular values

Sometimes a case depends on a value, not a class. An `eql` specializer makes
a method apply only when the argument equals a given value:

```clojure
(defgeneric describe-number (n))

(defmethod describe-number ((n Number)) "a number")
(defmethod describe-number ((n Integer)) "an integer")
(defmethod describe-number ((n (eql 0))) "zero")

(describe-number 0)   ; => "zero"
(describe-number 7)   ; => "an integer"
(describe-number 2.5) ; => "a number"
```

An `eql` method is more specific than any class method for the same
argument, so `0` gets `"zero"` even though it is also an `Integer`. The
comparison uses `=`, so `(eql :draft)` matches the keyword `:draft`, and
`(eql "x")` matches any string equal to `"x"`.

## Dispatching on a value you compute

Clojure programmers know multimethods: you supply a dispatch function, and
methods are chosen by its result. Norn has the same thing as an option to
`defgeneric`. With `(:dispatch f)`, the generic function applies `f` to the
arguments and picks the method for that result.

This is useful for plain data, which has no class of its own to dispatch
on. A map with a `:shape` key, for instance:

```clojure
(defgeneric shape-area (s) (:dispatch :shape))

(defmethod shape-area :square [s] (* (:side s) (:side s)))
(defmethod shape-area :rect [s] (* (:w s) (:h s)))
(defmethod shape-area :default [s] 0)

(shape-area {:shape :square :side 4})  ; => 16
(shape-area {:shape :rect :w 2 :h 3})  ; => 6
(shape-area {:shape :blob})            ; => 0
```

The methods look different here. After the name comes the dispatch value,
then a plain parameter vector. `:default` is the method for values no other
method names. Any function can dispatch, not only a keyword, and it receives
all the arguments:

```clojure
(defgeneric render (doc format) (:dispatch (fn [doc format] format)))

(defmethod render :html [doc format] (str "<p>" doc "</p>"))
(defmethod render :text [doc format] doc)

(render "Hello" :html) ; => "<p>Hello</p>"
```

A dispatch value can be a vector, which lets you dispatch on several things
at once. It can also be a class, and then it matches the class and its
subclasses:

```clojure
(defgeneric classify (x) (:dispatch class-of))

(defmethod classify Number [x] :number)
(defmethod classify Integer [x] :integer)
(defmethod classify :default [x] :other)

[(classify 5) (classify 1.5) (classify "five")] ; => [:integer :number :other]
```

Use class dispatch when your values are instances, and `:dispatch` when
they're plain data or when the deciding fact isn't a class.

## Method combination

So far one method has run per call: the most specific one. Often you want
more than that. A subclass wants to add a step to what its parent does, or
code wants to log, validate or time every call without each method doing it.
Norn's standard method combination gives methods roles through qualifiers.

- A primary method, with no qualifier, computes the result. Only the most
  specific runs, unless it calls the next one.
- A `:before` method runs before the primary methods. Every applicable one
  runs, most specific first.
- An `:after` method runs after them. Every applicable one runs, least
  specific first.
- An `:around` method wraps everything else. The most specific runs first,
  and decides whether and when to call the rest.

Here is withdrawal from the accounts defined earlier, with each kind of
method recording that it ran:

```clojure
(def trail (atom []))

(defgeneric withdraw (acct amount))

(defmethod withdraw ((a account) amount)
  (swap! trail conj :primary)
  (with a :balance (- (balance a) amount)))

(defmethod withdraw :before ((a account) amount)
  (swap! trail conj :check-funds)
  (when (> amount (balance a))
    (error "Insufficient funds")))

(defmethod withdraw :before ((a savings) amount)
  (swap! trail conj :check-savings-limit))

(defmethod withdraw :after ((a account) amount)
  (swap! trail conj :audit))

(defmethod withdraw :around ((a savings) amount)
  (swap! trail conj :around-start)
  (let [result (call-next-method)]
    (swap! trail conj :around-end)
    result))

(withdraw acct 30) ; => #user/savings{:owner "Ada" :balance 70 :rate 1/50}
@trail
; => [:around-start :check-savings-limit :check-funds :primary :audit :around-end]
```

The `:around` method ran first. Its `call-next-method` ran the rest: the
`:before` methods from most specific (`savings`) to least (`account`), then
the primary, then the `:after` method. The `:before` and `:after` methods'
return values are ignored. The call returns what the primary returned,
unless an `:around` method returns something else.

A `:before` method that signals an error stops the call before the primary
runs, which makes it a natural place for checks:

```clojure
(withdraw acct 500) ; !! Error
```

### Calling the next method

Inside a primary or `:around` method, `(call-next-method)` calls the next
most specific method with the same arguments. You saw it in the `bonus`
reader above. You can also pass different arguments, and
`(next-method?)` tells you whether there is a next method at all:

```clojure
(defgeneric scaled (x))

(defmethod scaled ((x Number)) [:number x (next-method?)])
(defmethod scaled ((x Integer)) (call-next-method (* 10 x)))

(scaled 4)   ; => [:number 40 false]
(scaled 1.5) ; => [:number 1.5 false]
```

Calling `call-next-method` when there is no next method is an error.

### Other combinations

The standard combination is one choice. A generic function can instead
combine the results of all its applicable primary methods with an operator.
Name it with `(:method-combination …)`. The built-in ones include `+`,
`list`, `and`, `or`, `min`, `max` and `vector`. Methods may carry the
combination's name as a qualifier, which makes their intent clear:

```clojure
(defclass item () ())
(defclass taxed (item) ())
(defclass shipped (taxed) ())

(defgeneric price (x) (:method-combination +))

(defmethod price + ((x item)) 10)
(defmethod price + ((x taxed)) 2)
(defmethod price + ((x shipped)) 5)

(price (shipped)) ; => 17
(price (taxed))   ; => 12
```

Each class adds its part, and `+` sums them. A `list` combination collects
the parts, most specific first, and `and` is true only if every method
agrees:

```clojure
(defgeneric labels (x) (:method-combination list))
(defmethod labels list ((x item)) :item)
(defmethod labels list ((x shipped)) :shipped)

(labels (shipped)) ; => (:shipped :item)
```

`:around` methods still work in these combinations, and wrap the combined
result:

```clojure
(defmethod price :around ((x shipped)) (* 2 (call-next-method)))

(price (shipped)) ; => 34
```

To combine with an operator of your own, define a combination with
`define-method-combination`. Its `:operator` is any function or macro name:

```clojure
(define-method-combination joined :operator str)

(defgeneric badge (x) (:method-combination joined))
(defmethod badge joined ((x item)) "item")
(defmethod badge joined ((x taxed)) "taxed/")

(badge (taxed)) ; => "taxed/item"
```

## Printing instances

By default an instance prints as a record of its slots. That's good for
debugging and poor for values with a natural written form, like money or
a date. To control how instances of a class print, define a method on
`print-object`. It returns the text:

```clojure
(defclass money ()
  ((cents :required true)
   (currency :initform :usd)))

(defmethod print-object ((m money))
  (let [c (:cents m)]
    (str (quot c 100) "." (if (< (mod c 100) 10) "0" "") (mod c 100)
         " " (name (:currency m)))))

(def price-tag (money :cents 1205))

(pr-str price-tag) ; => "12.05 usd"
(str "Total: " price-tag) ; => "Total: 12.05 usd"
(println [price-tag (money :cents 99 :currency :eur)])
; prints: [12.05 usd 0.99 eur]
```

The method is used everywhere instances print: `print`, `println`, `prn`,
`str`, `pr-str`, `format`, inside collections, and at the REPL. A subclass
inherits it.

Keep in mind that a custom printed form is still not readable by the reader.
It's a display format, not a serialization.

## Initializing instances

Sometimes the constructor should do more than fill slots from its
arguments: compute one slot from others, normalize a value, or reject bad
input. For that, define a method on `initialize-instance`. It receives the
new instance and a map of the arguments, and returns the instance to use.
Because instances are values, the method doesn't set slots. It calls
`call-next-method` to get the standard instance, then returns a changed copy
made with `with`:

```clojure
(defclass temperature ()
  ((celsius)
   (fahrenheit)))

(defmethod initialize-instance ((t temperature) args)
  (let [t (call-next-method)]
    (cond
      (:celsius t) (with t :fahrenheit (+ 32 (* 9/5 (:celsius t))))
      (:fahrenheit t) (with t :celsius (* 5/9 (- (:fahrenheit t) 32)))
      :else (error "A temperature needs :celsius or :fahrenheit"))))

(temperature :celsius 100) ; => #user/temperature{:celsius 100 :fahrenheit 212}
(temperature :fahrenheit 32) ; => #user/temperature{:celsius 0 :fahrenheit 32}
(temperature) ; !! Error
```

The same method runs for `(temperature …)`, `(make temperature …)` and
`make-instance`. It does not run for `with`: `with` copies an existing
instance and changes the slots you name, without calling the constructor.
So `(with (temperature :celsius 100) :celsius 0)` keeps the old
`:fahrenheit`. If your class keeps derived slots consistent, write a
function that rebuilds the instance through the constructor, and use it
instead of `with`.

Validation fits in a method that checks the arguments before it calls the
next method:

```clojure
(defmethod initialize-instance ((m money) args)
  (when (neg? (:cents args))
    (error "Money can't be negative:" (:cents args)))
  (call-next-method))

(money :cents 500) ; => 5.00 usd
(money :cents -1)  ; !! Error
```

The expected value above is the printed form, `5.00 usd`, from the
`print-object` method in the previous section.

## Summary

- `defclass` defines a class with slots. Build instances with `(name :slot
  value …)` or `make`; read slots with keywords, `slot-value` or `:reader`
  functions.
- Instances are immutable values. `with` makes an updated copy, equality is
  by class and slot values, and `instance->map` turns an instance into
  plain data.
- Every value has a class. Built-in classes such as `Integer`, `String` and
  `Vector` can have methods like your own.
- `defgeneric` and `defmethod` define generic functions. Methods are chosen
  by the classes of all required arguments, with `eql` specializers for
  particular values.
- `(:dispatch f)` gives Clojure-style multimethods: methods chosen by the
  value of `f` applied to the arguments.
- Standard method combination adds `:before`, `:after` and `:around`
  methods, and `call-next-method` reaches the next method. `+`, `list`,
  `and` and `define-method-combination` combine every method's result.
- `print-object` controls how instances print, and `initialize-instance`
  runs extra code when an instance is made.

---

[Contents](README.md) · Previous: [8. Modules and projects](08-modules-and-projects.md) · Next: [10. Protocols and extension](10-protocols-and-extension.md) <!-- nav -->
