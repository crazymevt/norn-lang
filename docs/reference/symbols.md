# Norn symbol index

[Reference contents](README.md) · [Standard library](libraries.md)

This index lists the public names in `norn.core` and common special or
contextual forms such as `if`, `catch`, and `call-next-method`. Entries
with links lead to the relevant reference sections. For other core
names, try `norn doc NAME` or `(source NAME)` at the REPL. Names
beginning with `%` are runtime internals.

[Punctuation](#punctuation) · [A](#a) · [B](#b) · [C](#c) · [D](#d) · [E](#e) · [F](#f) · [G](#g) · [H](#h) · [I](#i) · [J](#j) · [K](#k) · [L](#l) · [M](#m) · [N](#n) · [O](#o) · [P](#p) · [Q](#q) · [R](#r) · [S](#s) · [T](#t) · [U](#u) · [V](#v) · [W](#w) · [Y](#y) · [Z](#z)

## Punctuation

- [`*`](collections.md#numbers-and-equality) · `*check-contracts*` · `*debugger*` · `*handlers*` · `*inspecting*` · `*load-shift*`
- `*loading*` · `*out*` · `*restarts*` · `*trace-depth*` · [`+`](collections.md#numbers-and-equality) · [`-`](collections.md#numbers-and-equality)
- [`->`](syntax.md#branching-and-sequencing) · [`->>`](syntax.md#branching-and-sequencing) · `->contract` · [`/`](collections.md#numbers-and-equality) · [`<`](collections.md#numbers-and-equality) · [`<!`](concurrency.md#channels)
- [`<=`](collections.md#numbers-and-equality) · [`=`](collections.md#numbers-and-equality) · [`==`](collections.md#numbers-and-equality) · [`>`](collections.md#numbers-and-equality) · [`>!`](concurrency.md#channels) · [`>=`](collections.md#numbers-and-equality)

## A

- `abs` · `add-after-eval-hook!` · `add-cleanup!` · [`add-lib-path!`](syntax.md#namespaces-and-modules) · `add-watch` · `all-ns`
- [`all-of`](syntax.md#function-contracts) · `alter-var-root` · [`alts!`](concurrency.md#channels) · [`and`](syntax.md#branching-and-sequencing) · [`any-of`](syntax.md#function-contracts) · `any?`
- `apply` · [`apropos`](../getting-started.md#look-up-a-name) · [`ArithmeticError`](conditions.md#signaling-conditions) · [`ArityError`](conditions.md#signaling-conditions) · `as->` · `assert`
- [`assoc`](collections.md#lookup-and-update) · [`assoc!`](collections.md#transients) · [`assoc-in`](collections.md#lookup-and-update) · `Atom` · [`atom`](concurrency.md#atoms-and-dynamic-vars) · [`await`](concurrency.md#fibers-and-scopes)

## B

- [`backtrace`](conditions.md#debugging-a-parked-fiber) · [`begin-for-syntax`](syntax.md#macros-and-expansion) · [`binding`](concurrency.md#atoms-and-dynamic-vars) · `bit-and` · `bit-not` · `bit-or`
- `bit-shift-left` · `bit-shift-right` · `bit-xor` · `Boolean` · `boolean` · `boolean?`
- `butlast` · `Bytes` · `bytes` · `bytes->string` · `bytes-concat` · `bytes?`

## C

- [`call-next-method`](objects.md#method-combination) · [`cancel!`](concurrency.md#fibers-and-scopes) · `cancel-cleanup!` · `carrier-count` · [`case`](syntax.md#branching-and-sequencing) · `cat`
- [`catch`](conditions.md#handling-and-cleanup) · `catch-tag` · [`chan`](concurrency.md#channels) · `Channel` · `char` · `char?`
- `Character` · `Class` · `class-direct-subclasses` · `class-direct-superclasses` · `class-name` · [`class-of`](objects.md#construction-and-access)
- [`class-precedence-list`](objects.md#construction-and-access) · `class-slots` · `class?` · [`close!`](concurrency.md#channels) · [`closed?`](concurrency.md#channels) · `coll?`
- `Collection` · `comment` · [`comp`](collections.md#reduction-and-transducers) · `compare` · [`compare-and-set!`](concurrency.md#atoms-and-dynamic-vars) · `complement`
- [`completing`](collections.md#reduction-and-transducers) · [`compute-applicable-methods`](objects.md#protocols-and-object-customization) · [`compute-effective-method`](objects.md#protocols-and-object-customization) · [`compute-restarts`](conditions.md#restarts-and-recovery) · `concat` · [`cond`](syntax.md#branching-and-sequencing)
- `cond->` · `cond->>` · [`Condition`](conditions.md#signaling-conditions) · [`condition-message`](conditions.md#signaling-conditions) · [`condition-text`](conditions.md#signaling-conditions) · `condp`
- [`conj`](collections.md#persistent-collections) · [`conj!`](collections.md#transients) · `ConnectionError` · `cons` · `constantly` · [`contains?`](collections.md#lookup-and-update)
- `Contract` · `contract-name` · `contract?` · [`ContractError`](syntax.md#function-contracts) · [`count`](collections.md#sequence-traversal) · `current-fiber`
- `current-ns` · `cycle`

## D

- [`debug-abort`](conditions.md#debugging-a-parked-fiber) · [`debug-restart`](conditions.md#debugging-a-parked-fiber) · [`debug-restart-frame`](conditions.md#debugging-a-parked-fiber) · [`debugging`](conditions.md#debugging-a-parked-fiber) · `dec` · `declare`
- `dedupe` · [`def`](syntax.md#binding-and-functions) · [`defclass`](objects.md#class-definition-and-slots) · [`defcontract`](syntax.md#function-contracts) · [`defgeneric`](objects.md#generic-functions-and-methods) · [`define-method-combination`](objects.md#method-combination)
- `definition` · [`defmacro`](syntax.md#macros-and-expansion) · [`defmethod`](objects.md#generic-functions-and-methods) · [`defn`](syntax.md#binding-and-functions) · [`defn-`](syntax.md#binding-and-functions) · [`defprotocol`](objects.md#protocols-and-object-customization)
- `Delay` · `delay` · `delay?` · `deliver` · `denominator` · [`deref`](concurrency.md#atoms-and-dynamic-vars)
- `deref-object` · `destructure` · `disassemble` · [`disj`](collections.md#lookup-and-update) · [`disj!`](collections.md#transients) · [`dissoc`](collections.md#lookup-and-update)
- [`dissoc!`](collections.md#transients) · `distinct` · `distinct?` · [`do`](syntax.md#branching-and-sequencing) · `doall` · [`doc`](../getting-started.md#look-up-a-name)
- `done?` · `dorun` · [`doseq`](syntax.md#iteration-and-tail-calls) · [`dotimes`](syntax.md#iteration-and-tail-calls) · `doto` · `double`
- `double?` · [`drop`](collections.md#eager-transforms) · `drop-last` · `drop-while`

## E

- `eduction` · `empty` · `empty?` · `ensure-live` · `ensure-reduced` · [`Error`](conditions.md#signaling-conditions)
- [`error`](conditions.md#signaling-conditions) · `eval` · [`eval-in-frame`](conditions.md#debugging-a-parked-fiber) · `even?` · `every-pred` · `every?`
- [`ex-cause`](conditions.md#signaling-conditions) · [`ex-data`](conditions.md#signaling-conditions) · [`ex-info`](conditions.md#signaling-conditions) · [`ex-message`](conditions.md#signaling-conditions) · [`ExInfo`](conditions.md#signaling-conditions) · `exit`
- [`extend-protocol`](objects.md#protocols-and-object-customization) · [`extend-type`](objects.md#protocols-and-object-customization)

## F

- `false?` · `ffirst` · `Fiber` · `fiber-dump` · `fiber-error` · `fiber-id`
- `fiber-stats` · `fiber?` · `FileExists` · `FileNotFound` · [`filter`](collections.md#eager-transforms) · [`filterv`](collections.md#eager-transforms)
- [`finally`](conditions.md#handling-and-cleanup) · `find` · `find-doc` · [`find-restart`](conditions.md#restarts-and-recovery) · [`first`](collections.md#sequence-traversal) · `flatten`
- `Float` · `float` · `float?` · [`fn`](syntax.md#binding-and-functions) · [`fn-of`](syntax.md#function-contracts) · `fn?`
- `fnext` · `fnil` · [`for`](syntax.md#iteration-and-tail-calls) · [`for-syntax`](syntax.md#macros-and-expansion) · `force` · `format`
- `frequencies` · `Function` · `Future` · `future` · `future-cancel` · `future-cancelled?`
- `future-done?` · `future?`

## G

- `gc` · `gc-count` · `generic-methods` · `generic?` · `GenericFunction` · `gensym`
- [`get`](collections.md#lookup-and-update) · [`get-in`](collections.md#lookup-and-update) · `group-by`

## H

- `Handle` · `handle-info` · `handle-kind` · `handle?` · [`handler-bind`](conditions.md#handling-and-cleanup) · [`handler-case`](conditions.md#handling-and-cleanup)
- [`hash`](collections.md#numbers-and-equality) · [`hash-map`](collections.md#persistent-collections) · [`hash-set`](collections.md#persistent-collections) · `heap-bytes`

## I

- `ident?` · [`identical?`](collections.md#numbers-and-equality) · `identity` · [`if`](syntax.md#branching-and-sequencing) · [`if-let`](syntax.md#branching-and-sequencing) · `if-not`
- [`if-some`](syntax.md#branching-and-sequencing) · `ifn?` · `ignore-errors` · `image-info` · `in-ns` · `inc`
- `IndexError` · `infinite?` · [`initialize-instance`](objects.md#protocols-and-object-customization) · `inspect` · `instance->map` · [`instance?`](objects.md#construction-and-access)
- `int` · `int?` · `Integer` · `integer?` · `interleave` · `interpose`
- [`into`](collections.md#reduction-and-transducers) · `invoke-debugger` · [`invoke-restart`](conditions.md#restarts-and-recovery) · [`IOError`](conditions.md#signaling-conditions) · [`iterate`](collections.md#streams-and-ranges)

## J

- `jit-stats` · `juxt`

## K

- [`keep`](collections.md#eager-transforms) · `keep-indexed` · `key` · [`keys`](collections.md#lookup-and-update) · `Keyword` · `keyword`
- `keyword?`

## L

- `last` · [`lazy`](collections.md#streams-and-ranges) · [`lazy-seq`](collections.md#streams-and-ranges) · [`let`](syntax.md#binding-and-functions) · [`letfn`](syntax.md#binding-and-functions) · `List`
- [`list`](collections.md#persistent-collections) · `list*` · `list?` · `load-file` · `load-string` · [`locking`](objects.md#construction-and-access)
- `long` · [`loop`](syntax.md#iteration-and-tail-calls)

## M

- `macro-steps` · `macro?` · [`macroexpand`](syntax.md#macros-and-expansion) · [`macroexpand-1`](syntax.md#macros-and-expansion) · [`make`](objects.md#construction-and-access) · [`make-instance`](objects.md#protocols-and-object-customization)
- `Map` · [`map`](collections.md#eager-transforms) · `map-indexed` · [`map-of`](syntax.md#function-contracts) · `map?` · `mapcat`
- [`mapv`](collections.md#eager-transforms) · [`match`](syntax.md#pattern-matching) · [`MatchError`](syntax.md#pattern-matching) · `max` · `max-key` · `memoize`
- `merge` · `merge-with` · `min` · `min-key` · `mod` · `mutable?`

## N

- `name` · `namespace` · `NaN?` · `nano-time` · `nat-int?` · `neg-int?`
- `neg?` · [`next`](collections.md#sequence-traversal) · [`next-method?`](objects.md#method-combination) · `nfirst` · `nil?` · [`nilable`](syntax.md#function-contracts)
- `nnext` · `not` · `not-any?` · `not-empty` · `not-every?` · `not=`
- [`ns`](syntax.md#namespaces-and-modules) · `ns-aliases` · `ns-interns` · `ns-publics` · [`nth`](collections.md#lookup-and-update) · `nthnext`
- `nthrest` · `Null` · `num` · `Number` · `number?` · `numerator`

## O

- `Object` · `odd?` · [`offer!`](concurrency.md#channels) · `on-image-restore` · [`or`](syntax.md#branching-and-sequencing) · `owner`
- [`OwnershipError`](objects.md#construction-and-access)

## P

- `parse-boolean` · `parse-double` · `parse-long` · [`ParseError`](conditions.md#signaling-conditions) · `partial` · `partition`
- `partition-all` · `partition-by` · [`peek`](collections.md#persistent-collections) · `PermissionDenied` · [`persistent!`](collections.md#transients) · [`pmap`](concurrency.md#fibers-and-scopes)
- [`poll!`](concurrency.md#channels) · [`pop`](collections.md#persistent-collections) · [`pop!`](collections.md#transients) · `pos-int?` · `pos?` · `pr`
- `pr-str` · [`preduce`](concurrency.md#fibers-and-scopes) · `print` · [`print-object`](objects.md#protocols-and-object-customization) · `print-str` · `printf`
- `println` · `private?` · `prn` · `prn-str` · `Promise` · `promise`
- [`put!`](concurrency.md#channels)

## Q

- [`queue`](collections.md#persistent-collections) · `queue?` · `quot` · [`quote`](syntax.md#evaluation-and-quotation)

## R

- `rand` · `rand-int` · `rand-nth` · [`range`](collections.md#streams-and-ranges) · `Ratio` · `ratio?`
- `rational?` · `re-find` · `re-matches` · `re-pattern` · `re-quote` · `re-seq`
- `read-string` · `realize` · `realized?` · `realized?-object` · `reconnect` · [`recur`](syntax.md#iteration-and-tail-calls)
- [`reduce`](collections.md#reduction-and-transducers) · `reduce-kv` · [`reduced`](collections.md#reduction-and-transducers) · `reduced?` · `reductions` · `Regex`
- `regex?` · `register-resource!` · [`release!`](objects.md#construction-and-access) · `rem` · [`remove`](collections.md#eager-transforms) · `remove-after-eval-hook!`
- `remove-watch` · [`repeat`](collections.md#streams-and-ranges) · `repeatedly` · [`require`](syntax.md#namespaces-and-modules) · [`require-for-syntax`](syntax.md#macros-and-expansion) · [`reset!`](concurrency.md#atoms-and-dynamic-vars)
- `reset-vals!` · `resolve` · `resource-live?` · `ResourceLost` · [`rest`](collections.md#sequence-traversal) · [`restart-case`](conditions.md#restarts-and-recovery)
- `reverse` · `reverse*` · `rseq` · `rsubseq` · `run!` · [`RuntimeError`](conditions.md#signaling-conditions)

## S

- [`satisfies?`](objects.md#protocols-and-object-customization) · [`save-image`](../tooling.md#images) · `Scope` · `second` · `select-keys` · `Seq`
- [`seq`](collections.md#sequence-traversal) · `seq?` · `seqable?` · [`sequence`](collections.md#reduction-and-transducers) · `Sequential` · `sequential?`
- [`SeriousCondition`](conditions.md#signaling-conditions) · `Set` · `set` · [`set-slot!`](objects.md#construction-and-access) · `set?` · [`settle`](concurrency.md#fibers-and-scopes)
- [`share`](objects.md#construction-and-access) · `shuffle` · [`signal`](conditions.md#signaling-conditions) · [`SimpleError`](conditions.md#signaling-conditions) · [`sleep`](concurrency.md#fibers-and-scopes) · [`slot-value`](objects.md#construction-and-access)
- [`slot-value-using-class`](objects.md#protocols-and-object-customization) · `slurp` · `some` · `some->` · `some->>` · `some-fn`
- `some?` · `sort` · `sort-by` · [`sorted-map`](collections.md#persistent-collections) · [`sorted-set`](collections.md#persistent-collections) · `sorted?`
- [`source`](../getting-started.md#look-up-a-name) · `source-text` · [`spawn`](concurrency.md#fibers-and-scopes) · [`spawn-detached`](concurrency.md#fibers-and-scopes) · `spit` · `split-at`
- `split-with` · `StandardClass` · `StandardGenericFunction` · [`StandardObject`](objects.md#class-definition-and-slots) · `str` · `stream?`
- `String` · `string?` · `subbytes` · [`subclass?`](objects.md#construction-and-access) · `subs` · `subseq`
- [`subvec`](collections.md#lookup-and-update) · [`swap!`](concurrency.md#atoms-and-dynamic-vars) · `swap-vals!` · `Symbol` · `symbol` · `symbol?`

## T

- [`take`](collections.md#eager-transforms) · [`take!`](concurrency.md#channels) · `take-last` · `take-while` · [`throw`](conditions.md#signaling-conditions) · `throw-tag`
- `time` · [`timeout`](concurrency.md#channels) · [`TimeoutError`](conditions.md#signaling-conditions) · `TlsError` · `trace` · `trace-var!`
- `trampoline` · [`transduce`](collections.md#reduction-and-transducers) · [`transient`](collections.md#transients) · `transient?` · `tree-seq` · `true?`
- [`try`](conditions.md#handling-and-cleanup) · `type` · [`TypeError`](conditions.md#signaling-conditions)

## U

- `UnboundVarError` · `unreduced` · `untrace` · `untrace-all!` · `untrace-var!` · [`unwind-protect`](conditions.md#handling-and-cleanup)
- [`update`](collections.md#lookup-and-update) · [`update-in`](collections.md#lookup-and-update) · `update-keys` · [`update-slot!`](objects.md#construction-and-access) · `update-vals`

## V

- `val` · [`vals`](collections.md#lookup-and-update) · `Var` · [`var`](syntax.md#evaluation-and-quotation) · `var-get` · `var?`
- [`vec`](collections.md#streams-and-ranges) · [`vec-of`](syntax.md#function-contracts) · `Vector` · [`vector`](collections.md#persistent-collections) · `vector?` · `volatile!`
- `vreset!` · `vswap!`

## W

- [`warn`](conditions.md#signaling-conditions) · [`Warning`](conditions.md#signaling-conditions) · `weak-get` · `weak-ref` · [`when`](syntax.md#branching-and-sequencing) · `when-first`
- `when-let` · [`when-not`](syntax.md#branching-and-sequencing) · `when-some` · `while` · [`with`](objects.md#construction-and-access) · `with-out-str`
- [`with-scope`](concurrency.md#fibers-and-scopes) · `with-simple-restart`

## Y

- [`yield`](concurrency.md#fibers-and-scopes)

## Z

- `zero?` · `zipmap`
