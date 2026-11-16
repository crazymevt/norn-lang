# Plan: M9 — finding the gaps before sealing

Drafted 2026-11-14; its proposals were accepted the same day (see
Decisions, at the end). It reorders
M9 (runtime.md §12) around sealing. The decisions behind M9's parts are in
design §3.2 (STM), §4.3 (the MOP), §6.5 (processes), §8.4 (types) and §9
(the FFI).

**Why before sealing.** Sealing freezes `norn.core` behind `api-diff` and
deprecation cycles. Writing real programs is how the gaps in it show:
M7.5's UI library and M7.6's editor both changed the stdlib. runtime.md put
M9 after sealing because "libraries nothing else in the stdlib builds on"
don't need to come first. That holds for their interfaces, but not for
what writing them uncovers. Some of M9 changes core itself (types,
contracts, the MOP, pattern matching), so it can only come first.

**So M9 splits:**

- **M9a, before sealing:** what changes `norn.core` or the runtime's
  contracts, and the libraries most likely to find gaps (as probes).
- **M9b, after sealing:** what needs a sealed module, and tooling that
  doesn't shape core.

Sealing then waits for M9a, as well as the M7.6 done-check.

## How things are now

- **Types:** none. The reader keeps metadata, so `^String x` reads, but
  the compiler ignores it.
- **Contracts:** none.
- **CLOS** (design §4, M2 and M7):
  - built: classes; generic functions with standard and simple method
    combination; dispatch caches; live redefinition; mutable owned
    instances;
  - not built: the MOP (`standard-class`, `compute-applicable-methods`,
    `slot-value-using-class`, metaclasses), `print-object`,
    `initialize-instance`, `define-method-combination`, and protocol
    sugar.
- **Concurrency:** fibers, scopes, channels, `alts!`, `norn.async` and
  `norn.sync`. No processes, mailboxes, links or supervisors.
- **Pattern matching:** only on syntax (`syntax-parse`, 8.3). Nothing
  matches data, which `receive` needs (design §6.5's example).
- **STM:** none.
- **The FFI:** `library`, `function` and `defcfun`, on the blocking pool,
  with byte buffers. No structs, callbacks or typed signatures, and
  nothing marks a module as unsafe.
- **Networking:** TCP, UDP and TLS (rustls), but no HTTP.
- **Processes:** `norn.process` runs programs with pipes, but has no
  pseudo-terminals.
- **Formats:** EDN and JSON. No binary format, CSV or TOML. No
  compression, archives or crypto.

## Parts

M9a, before sealing:

| Part | Work |
|---|---|
| 9.1 | **HTTP and URIs:** `norn.uri` (parse, build, encode), and `norn.http`, an HTTP/1.1 client and server: keep-alive, chunked bodies, TLS both ways on the existing rustls, bodies as `norn.io` streams. The server takes a handler from a request map to a response map. It's first because it exercises the most at once: sockets, streams, bytes, fibers, timeouts and errors under load. |
| 9.2 | **Pattern matching on data:** `match` in `norn.core`. Patterns are literals, bindings, `_`, vectors and lists with `& rest`, maps by key, `(:or …)`, and `:when` guards. A failed match says what didn't match. It's in core because `receive`, the supervisors and ordinary code all want it. |
| 9.3 | **Types: checked signatures and `declare`:** `^Type` on parameters and results (design §8.4). Types are classes, with a few forms (`(? T)` for nil-able, `(U A B)`, `(Vec T)`, `(Map K V)`, `(-> A B)`). `(declare (optimize …))` and `(declare (type …))` are accepted and recorded for M10. |
| 9.4 | **Contracts, with blame:** predicates and combinators (`(-> int? string?)`, `or/c`, `and/c`, `vec-of`, `map-of`), and `defcontract`. A public function's signature is its contract. It's checked on calls from other modules; the module's own calls go direct. A violation blames the side at fault: the caller (with its place) for bad arguments, the module for a bad result. |
| 9.5 | **The full MOP** (design §4.3): classes as instances of `standard-class`, and generic functions of `standard-generic-function`. Also `compute-applicable-methods`, `compute-effective-method`, `slot-value-using-class`, metaclasses, `initialize-instance` and `make-instance`'s protocol, `print-object` (the printer calls it for instances), `define-method-combination`, `defprotocol` as sugar for generic functions, and slot readers as generic functions (today they're plain functions, design §4), so a subclass can specialise one. The fast path stays whenever the hooks aren't specialised. |
| 9.6 | **Processes and supervision** (design §6.5): `defprocess`, mailboxes with selective `receive` (on 9.2's `match`, with timeouts), `send`, a registry of names, links and monitors with exit signals, and supervisors (one-for-one, one-for-all, rest-for-one, with restart limits). A crashed process stays in the debugger, as design §5.3 says, and its supervisor is told. |
| 9.7 | **`norn.stm`** (design §3.2): `ref`, `dosync`, `alter`, `commute` and `ensure`, with MVCC as Clojure's. A ref can't hold a mutable owned object (§4.2), and `io!` refuses side effects inside a transaction. |
| 9.8 | **The full FFI** (design §9): struct layouts (`defcstruct`: fields, alignment, nested structs and arrays), typed signatures, and callbacks from C. Callbacks run on the caller's thread during a call from Norn, as a nested run; a foreign thread's callbacks are posted to a channel. **Unsafe modules:** a module that uses the raw FFI says `(:unsafe)` in its `ns`, and the expander refuses raw FFI use elsewhere. |
| 9.9 | **SQLite,** on 9.8's FFI and the system's libsqlite3: connections, prepared statements, transactions, and rows as maps. It's the FFI's first real user. |
| 9.10 | **Pseudo-terminals** in `norn.process`: spawning on a pty, resizing, and reading and writing as streams. |
| 9.11 | **A binary format for Norn values** (`norn.bin`): every value type, versioned, and safe to read from untrusted input, as `norn.edn` is. Touching every type is the point: it finds gaps in types, the printer and equality. |
| 9.12 | **The smaller libraries,** each when something needs it: `norn.uuid`, `norn.csv`, `norn.toml`, compression (deflate, gzip, zlib), `norn.zip`, `norn.crypto` (digests, HMAC, secure random, on `ring`, which rustls already links), `norn.schedule` (cron-like), and `norn.data` (structural diff). |
| 9.13 | **Conditions for the runtime's errors:** a hierarchy of condition classes (`IOError`, with `FileNotFound`, `ConnectionError` and `TimeoutError` under it; `ParseError`; `OwnershipError`; `ArityError`; and so on), and a way for Rust to raise one with its data (the path, the address, the line). Today a Rust error carries only its message, so an `OwnershipError` is recognised by its text (M7), and every timeout, failed connection and parse error in the stdlib, `norn.http`'s included, is a plain `Error`. Handlers choose by class, so the hierarchy is part of what sealing freezes. |
| 9.14 | **Values that arrive later, and errors with data:** `delay` (computed once, when first forced), `future` (a fiber whose value `deref` waits for), promises (`promise` and `deliver`), and `ex-info` with `ex-data`. M7.6 left them for a design against fibers and conditions. Proposed: a future is a fiber and `@` is `await`, so cancelling and the debugger work as for any fiber; a promise is a one-shot channel; and `ex-info` makes a condition carrying a data map, which a handler reads with `ex-data` (a condition with a `:data` slot, under 9.13's hierarchy). |

Studio's phased panels come as their libraries do, and are where the
libraries get used for real, as M7.6 used the stdlib:

- the API workbench, after 9.1;
- the terminal, after 9.10;
- the Databases view, after 9.9;
- team chat, whenever.

M9b, after sealing (unchanged from runtime.md):

- **Needs sealing:**
  - tree shaking for images (`:shake` uses sealing to prove code unused);
  - `norn api-diff`;
  - the lint rule for redefining sealed vars.
- **Tooling, any time** (none of it shapes core): `norn lsp`, the sampling
  profiler, source-drift tracking (design §5.1), coverage, and `norn new`,
  which is small now that packages exist.

**Order.** 9.1 first: it's concrete, it exercises the most, and it unlocks
Studio's API workbench, while the type and contract decisions below get
made. Then 9.13 → 9.14 → 9.2 → 9.3 and 9.4 → 9.5 → 9.6 → 9.7 → 9.8 →
9.9. The runtime's condition classes come first: every library after
them raises them, and `norn.http` changes to. 9.14 builds on them
(`ex-info`) and on fibers, and 9.6's processes on 9.14's futures. Types
come early so the libraries after them are written with signatures, and
they test them. 9.10, 9.11 and 9.12 fit in when Studio or a library needs
them.

**Done-check (M9a)** (met on 2026-11-15; see the end of Status):

- Studio's API workbench, terminal and Databases view run on `norn.http`,
  the pty support and SQLite.
- Every public function of the new libraries has a checked signature.
- A contract violation across a module boundary blames the right side,
  with its place.
- A supervisor restarts a crashed process while the debugger still holds
  the crash.
- Tier 0 and tier 1 agree on every suite.

Then sealing.

## Decisions (the user, 2026-11-14)

The draft's proposals, accepted:

1. **M9 splits as above,** and sealing waits for M9a. runtime.md's row and
   its ordering note ("What depends on the full FFI or on `net` waits until
   after sealing") change to match.
2. **No static type checker in M9.** Signatures are checked dynamically,
   as contracts at module boundaries. That's the "gradual" part: sound
   where typed and untyped code meet, with nothing checked inside a module
   unless asked. M10's inference uses the signatures. A static checker
   (Typed Racket's) is a project of its own, and stays an open question.
3. **Type syntax is Clojure's metadata,** as design §8.4 shows (`^String
   x`, and `^Vec` after the parameter vector for the result), not a
   separate `(: f …)` declaration. Types are classes plus the few forms in
   9.3. Predicates (`int?`) are contracts, not types.
4. **Contracts are checked only across module boundaries.** The expander
   knows the calling module, so a call from another module goes through the
   contract and a call from inside doesn't; inside costs nothing. A debug
   setting (`*check-contracts* :all`) checks every call.
5. **`match` goes in `norn.core`** (9.2), since `receive` and ordinary code
   both need it. It's a macro over the existing data, with no new runtime
   type.
6. **Processes are fibers with mailboxes,** not a second kind of task.
   Exit signals are conditions, so a link or a supervisor sees what a
   handler would. A crash keeps design §5.3's behaviour: it waits in the
   debugger, and the supervisor's policy decides whether to restart it.
7. **The MOP keeps today's fast path** for classes and generic functions
   that don't specialise its hooks, so existing code costs the same.
   `print-object` changes how the printer shows instances: one place
   where core's behaviour changes.
8. **FFI callbacks are synchronous** on the thread that called out (a
   nested run, which can't park). A foreign thread's callbacks go to a
   channel. Calling into Norn from an arbitrary thread directly isn't
   offered.
9. **New native dependencies are fine before sealing:** `flate2` and
   `zip`, plus `ring`, which is already linked. Each new primitive
   invalidates saved images (runtime.md §12), which costs nothing before
   1.0. SQLite uses the system library through the FFI rather than being
   bundled.
10. **HTTP/1.1 only in M9.** HTTP/2 and WebSockets come when something
    needs them. The server's handler model is Ring's (a request map in, a
    response map out), so middleware is plain functions.

Added the same day, from what earlier milestones left open (the user,
2026-11-14):

11. **The runtime's errors get condition classes** (9.13), before more
    libraries are written on plain `Error`s.
12. **`delay`, `future`, promises and `ex-info` go in core** (9.14), on
    fibers and 9.13's conditions.
13. **Slot readers become generic functions,** as part of the MOP (9.5).
14. **Two-pass module expansion is dropped** (M8 decision 2, amended),
    unless sealing turns out to need it.

## Status


**9.1, HTTP and URIs (2026-11-14): done.**

- **`norn.uri`** (RFC 3986):
  - `parse` gives a map of a URI's parts, as written (IPv6 hosts without
    their brackets), and `format` puts it back together;
  - `encode` and `decode` handle UTF-8 percent-encoding, with `+` as a
    space in forms;
  - `query-params` (a name given twice gets a vector), `query-pairs` and
    `query-string`;
  - `resolve` makes a reference absolute against a base, passing every
    normal example in RFC 3986 §5.4.1;
  - `port-of` gives a URI's port, or its scheme's default.
- **`norn.http`, the client:**
  - `request`, and `get`, `post`, `put`, `delete` and `head`;
  - a status that isn't 2xx is a response like any other, while a failed
    connection or a timeout is an error;
  - connections are kept alive and pooled per scheme, host, port and
    trusted CA;
  - a stale pooled connection is replaced, and an idempotent request
    sent again;
  - redirects follow a relative `Location`, a 303 (or a POST's 301/302)
    becomes a GET, and there's a limit;
  - `:timeout` covers the whole exchange: it runs in a fiber that's
    cancelled when time runs out;
  - `:as :text` or `:bytes`, and `:ca-file` for a private CA;
  - request bodies can be strings, bytes, or sequences and streams (sent
    chunked).
- **`norn.http`, the server:** `serve` takes a handler from a request map
  to a response map (Ring's model), and runs each connection in its own
  fiber.
  - Connections are kept alive with an idle timeout, honour
    `Connection: close` and HTTP/1.0, and answer `Expect: 100-continue`.
  - It has limits: lines (431), headers, and bodies (`:max-body`, 413).
  - A malformed request gets a 400. A handler that fails gets a 500, and
    the error goes to `:on-error`.
  - A response body can be a string, bytes, a sequence or a `norn.io`
    stream (the last two chunked). HEAD responses carry the headers with
    no body.
  - It serves TLS with `:tls`, `:cert-file` and `:key-file`, and `stop!`
    closes the listener and the open connections.
  - Also `body-text` and `form-params`.
- **A bug the tests found:** pooled connections were shared across
  requests with different trust. A connection verified against a private
  CA served a later request that didn't trust it. The pool's key now
  includes the trusted CA.
- **A gap in the language, fixed (2026-11-14).** Defining `get` in a
  module captures every `get` in it, even above the definition, since a
  module's own names win, and nothing warned. Two halves:
  - **`(:refer-core :exclude [names…] :rename {name new-name})` in `ns`.**
    An excluded name is the module's own, defined or not. A renamed one is
    `norn.core`'s under its new name. `norn.http` says
    `(:refer-core :rename {get core-get})` and looks up maps with
    `core-get`. Both are ordinary namespace mappings, so images keep them.
  - **A lint rule** (`:shadow`): a module defining a `norn.core` name that
    its `ns` doesn't take for its own is told that every use of the name
    in it means the definition. It found 31 such definitions in the stdlib
    and 22 in norn-ui and Studio. Every one was meant (`norn.async`'s
    `map`, `norn.net`'s `resolve`, tests' `settle`, Studio's `close!`),
    and is now said so with `:exclude`.
  - **Tests:** `module_test` checks `:rename`, `:exclude`, the mistakes,
    and the rule.
- **Speed, a first measure:** about 7,100 requests/s from one client over
  a kept-alive connection, and 29,000/s from eight clients at once, with
  client and server in one process.
- **Tests:** `http_test.norn`, 77 checks at both tiers, covering:
  - URIs: parsing, formatting, encoding, queries, and RFC 3986's
    resolution examples;
  - HTTP: query parameters; text, byte, chunked and 1 MB bodies; stream
    bodies; methods and headers; HEAD; 404 and 500; redirects (relative,
    303, the limit); keep-alive reuse and `Connection: close`; a stale
    pooled connection; timeouts; malformed requests; the body limit;
    `100-continue`; HTTP/1.0; TLS, with an untrusted certificate
    refused; and `stop!`.

**9.13, conditions for the runtime's errors (2026-11-14): done.**

- **The hierarchy, in `norn.core`:**
  - `RuntimeError`, a mistake in the program, keeping its `use-value` and
    `retry` restarts: `TypeError` (`:expected`, `:value`), `ArityError`
    (`:function`, `:given`), `UnboundVarError` (`:var`), `IndexError`
    (`:index`), `ArithmeticError`, `OwnershipError`;
  - `IOError`, the world outside, with `:reason` (`:not-found`,
    `:refused`, `:reset`, `:timed-out`…) and `:path` or `:address`:
    `FileNotFound`, `PermissionDenied`, `FileExists` and `ConnectionError`
    (`TlsError` under it);
  - `TimeoutError` (`:after`), of its own, since what times out needn't be
    I/O;
  - `ParseError` (`:source`, `:line`, `:column`).

  I/O errors aren't `RuntimeError`s any more: a handler for program
  mistakes no longer catches a missing file. Every message is as it was.
- **Rust raises them with their data.**
  - `Error` gains a class: a `norn.core` class by name with slot values,
    or a Norn value (a condition to signal again).
  - `%runtime-error` makes that class. The old recognising of
    `OwnershipError` by its message's text is gone.
  - `io::ErrorKind` maps to the I/O classes. rustls's failures
    (`InvalidData`) are `TlsError`s.
  - A blocking-pool job fails with a `Failure` (a message and its class,
    safe to cross threads), which reaches the waiting fiber intact.
- **Where each class comes from:**
  - `TypeError` from the type checks;
  - `ArityError` from calls of closures and primitives;
  - `IndexError` and `UnboundVarError` where they happen;
  - `ArithmeticError` from division by zero;
  - the I/O classes from files, sockets, DNS (`ConnectionError`,
    `:reason :host-not-found`), accept, read, write and the TLS handshake;
  - `ParseError` from the reader, the comment-keeping reader, JSON (with
    the line and column), `norn.uri`, and HTTP messages.

  `norn.http` raises `TimeoutError` (`:after`), `ConnectionError` (a
  server that closes without answering) and `ParseError`.
- **A failed fiber keeps its condition** (a new fiber field, `CONDITION`),
  and `await` and scopes signal that same object again, class and data
  intact. Until now they made a new `RuntimeError` from its message. The
  VM's own `%abort-fiber` and die paths carry the condition through the
  unwinding. An image's dead fibers drop theirs.
- **Tests:** `errors_test.norn`, 28 checks at both tiers:
  - each class with its data;
  - the hierarchy, and restarts still offered;
  - files, a refused connection with its address, an untrusted
    certificate, an HTTP timeout;
  - JSON, source, URI and not-HTTP parse errors;
  - conditions through `await`, scopes, and a condition of one's own.

**9.14, values that arrive later, and errors with data (2026-11-14):
done.**

- **`deref` (and `@`) waits for a value:** an atom's or var's now, a
  fiber's when it ends, and a delay's, promise's or future's when it
  arrives. With `(deref x ms timeout-value)`, it waits at most that long
  for a promise or a future.
  - `realized?` covers all three, plus fibers.
  - `deref-object` and `realized?-object` are generic functions, so a
    class of one's own can be deref'd.
- **Promises:** `promise` and `deliver`. Any number of fibers can wait on
  one, and delivering a second time does nothing (it returns nil). A
  promise is an atom holding its state plus a channel closed on delivery.
- **Delays:** `delay`, `force` and `delay?`. The body is computed once,
  by the first fiber to ask, and the others wait for it. A failure is
  cached, and every deref signals that same condition. A delay that needs
  its own value to compute it is an error rather than a hang.
- **Futures:** `future`, `future?`, `future-done?`, `future-cancel` and
  `future-cancelled?`.
  - A future is a fiber in a scope of its own that nobody joins. So its
    failure belongs to its deref (not printed when it happens), while
    cancelling and the debugger work as for any fiber.
  - Its deref is `await`, so the condition it failed with comes back
    whole (9.13).
- **`ex-info`, `ex-data`, `ex-message` and `ex-cause`:**
  - `ex-info` makes an `ExInfo` (an `Error`) with a data map and a cause,
    for `throw` or `error`;
  - `ex-data` gives an `ExInfo`'s map, or a runtime error's slots
    (`{:path … :reason :not-found}`), or nil.
- **`deref` stays a primitive.** A Norn `deref` function made `@` on an
  atom four times slower at tier 1 (20 to 76 ms for 2 million). Instead,
  the primitive hands an object to `deref-object` through a new
  `ErrKind::Call`: the interpreter calls the function in the primitive's
  place, in the fiber, so it can park. A run from Rust calls it nested.
  `@` on an atom costs what it did (22 ms).
- **Two bugs found writing it:**
  - `compare-and-set!` compares by identity, so a promise's pending state
    must be an interned keyword, not a vector literal (deliver never
    matched, and deref waited for ever);
  - a future cancelled before its fiber starts never runs its body, so
    its `finally` never closes its done channel. Its deref now awaits the
    fiber itself, and `future-cancel` closes the channel.
  - `realized?` already existed, as a primitive for lazy streams, and the
    first version replaced it (`core_test` caught it). The new one hands
    everything that isn't a fiber or an object to that primitive.
- **Tests:** `later_test.norn`, 39 checks at both tiers:
  - twenty fibers waiting on one promise, and timeouts;
  - a delay forced by ten fibers at once, computed once, with its failure
    cached and the self-reference error;
  - futures' values, failures (the condition intact), timeouts and
    cancelling;
  - a class of one's own deref'd;
  - `ex-info` with data and a cause, and `ex-data` of a runtime error.

**9.2, `match` (2026-11-14): done.**

- **`match` in `norn.core`:** `(match x pattern [:when guard] body …)`.
  The first clause that matches gives the value. A guard sees the
  pattern's names. When nothing matches, it's a `MatchError` with the
  value (`:value`). The patterns:
  - `_`, and names, which bind (a name twice in one pattern means equal
    values);
  - `nil`, booleans, numbers, strings, characters and keywords, and
    quoted data;
  - `[p … & rest]`, matching any sequential value (a vector, a list, a
    seq), with `rest` nil when nothing's left;
  - `{:k p …}`, matching a map with those keys;
  - `(:or p …)`, where each pattern binds the same names (checked when
    expanding);
  - `(:as p name)`, `(:? pred p)`, and `:else`.
- **Compiled inline, with bodies in tail position,** so `recur` works in
  them, as `receive` (9.6) will need. Each clause's pattern becomes a test
  that yields its bindings, and its body is expanded once, so the code
  grows with the number of clauses rather than multiplying. Patterns are
  emitted left to right, so a repeated name compares against the first
  binding.
- **A runtime bug it found:** a lazy stream that had to be realized inside
  a run from Rust (`macroexpand-1`, here) was handed up to the fiber's
  interpreter. That interpreter forced the stream and ran its own
  instruction again, which re-ran the whole nested call with a new stream
  each time, for ever. A nested run now forces the stream itself and calls
  the primitive again. `core_test` checks it with a macro that maps over
  `(range)`.
- **Tests:** `match_test.norn`, 34 checks at both tiers: every kind of
  pattern, nesting, guards falling through, `recur` in a body, the
  `MatchError`, mistakes found when expanding, and how the code grows.

**9.3 and 9.4, types and contracts (2026-11-14): done, as decisions 2–4
describe.**

- **Contracts, in core:**
  - a predicate, a class, `:any`, or a combination: `fn-of` (arguments,
    a result, and `:rest`), `any-of`, `all-of`, `vec-of`, `map-of`,
    `nilable`;
  - `->contract`, `contract-name` and `contract?`;
  - a violation is a `ContractError` with `:function`, `:position` ("its
    2nd argument", "its result"), `:expected`, `:value` and `:blame`.

  **The names differ from the plan's.** `or/c` would read as the name
  `c` in a namespace called `or`, since `/` divides a namespace from a
  name, and `->` is already core's threading macro. So the combinators
  are `fn-of`, `any-of`, `all-of`, `vec-of`, `map-of` and `nilable`.
- **Signatures:**
  - `(defn f ^Integer [^Integer a ^(Vec Number) xs & ^Number more] …)`.
    A type is a class, `Any`, `(? T)`, `(U A B…)`, `(Vec T)`,
    `(Map K V)` or `(-> A… R)`. A name that isn't one (`^even?`) is an
    error when the module loads, pointing to `defcontract` for
    predicates.
  - `defcontract` gives a function's contract from a `fn-of`.
  - Either way, the contract lives in a private `f%contract` var beside
    the function, which is flagged.
- **Checked at module boundaries.** When another namespace (a module, or
  the REPL) names a flagged function, the expander puts in its place a
  private wrapper var in the *calling* namespace, made once per function
  and namespace. Its def runs before the form that first needs it, and
  the module cache records it with the rest.
  - The module's own calls go direct.
  - `*check-contracts*` (read when code is expanded) is `:boundaries` by
    default, or `:all`, or `:none`.
  - A function passed as a value from the other module is the wrapper,
    so it keeps its contract.
  - The wrapper reads the contract on each call, so redefining the
    function, with or without types, takes effect.
- **Blame, as Racket does it:**
  - a bad argument blames the calling module (with its file):
    `ct2.lib/add: its 2nd argument should be Integer, got "2", blaming
    ct2.user (…/ct2/user.norn)`;
  - a bad result blames the function's own module;
  - a `fn-of` argument's bad result blames the function it was given.
- **`(declare (optimize …) (type …))`** is recorded in `%declarations`
  with where it's written, for M10. `(declare name…)` is as it was.
- **Three things the work turned up:**
  - **A reader bug.** A collection's metadata was lost when its parts had
    their own (`^String [^Integer x]`): one slot held pending metadata.
    It's a stack now.
  - **Where to read a signature.** `fn` rebuilds its parameter vector, and
    `defn` usually goes through the fast path (plain data in and out),
    both of which drop metadata. So signatures are read from the `defn`
    and `fn` calls' own syntax, in `macro-step`, before either runs.
  - **Cost.** Uncached expansion is about 5% slower (180 to 189 ms for
    four libraries), from looking up each name a module writes to see if
    it has a contract. A first version cost 17%: it put the macro vars
    behind delays (each deref a generic function call now), and checked
    namespaces before the cheap flag. Cached loads don't expand, so they
    don't pay it.
- **Tests:** `contracts_test.norn`, 31 checks at both tiers, with modules
  in a temporary folder:
  - arguments and results, and their blame;
  - `(Vec T)`, the rest of the arguments, `(U …)`, `(? T)`, `(-> …)`;
  - a name that isn't a type;
  - the module's own calls, a function passed as a value, the REPL, an
    unchecked function;
  - `defcontract`, redefinitions, `:none`;
  - contracts used directly, and `declare`.

**9.5, the full MOP (2026-11-14): done, with the fast paths kept.**

- **Classes have classes.**
  - A class is an instance of `StandardClass` (the built-in `Class`), or
    of a metaclass, a subclass of it, named with `(:metaclass M)` in
    `defclass`.
  - A generic function is an instance of `StandardGenericFunction`, or of
    a subclass named with `(:generic-function-class C)` in `defgeneric`.
  - `class-of` says which, from new fields on classes and generic
    functions.
- **The protocol is generic functions with standard methods,** each
  specialisable:
  - `make-instance` (makes an instance, then calls `initialize-instance`);
  - `initialize-instance` (gives back the instance; instances are
    values, so a method returns it, changed with `with`, after
    `call-next-method`);
  - `slot-value-using-class`;
  - `print-object` (returns the text: printing an instance anywhere, with
    print, prn, str or pr-str, uses it);
  - `compute-applicable-methods` and `compute-effective-method`.
- **The fast paths stay** for whatever specialises none of it:
  - A class has hook bits, seen by its subclasses through the class
    precedence list. A method on `print-object` or `initialize-instance`
    for the class sets one, and so does a metaclass.
  - `make` goes through `make-instance` only when hooked, and so does
    `slot-value` through `slot-value-using-class`. `slot-value` checks the
    bit in Rust and, when it's set, hands the call over with
    `ErrKind::Call`, as `deref` does.
  - The print primitives ask `print-object` first, for any hooked
    instance in what they print, and the printer uses its text. Until some
    class has a method, they don't even look.
  - A dispatch miss goes through `compute-applicable-methods` and
    `compute-effective-method` only for a generic function with a class
    of its own. Its cache works as before.
- **`define-method-combination`** (its short form, `:operator op`) adds
  to the combinations `+`, `list` and the rest are.
- **Protocols:** `defprotocol` (a generic function per method,
  dispatching on its first argument), `extend-type`, `extend-protocol`
  (built-in classes too) and `satisfies?`.
- **Slot readers are generic functions** (decision 13). A subclass can
  specialise one, and two classes can have readers of the same name.
- **Speed.** This was measured at each step:
  - A first version cost readers and `slot-value` 2–3.5× (a Norn
    `slot-value`, readers as two calls deep).
  - Now a method whose body never calls the next method is *direct*: it
    takes no next-method argument, and alone it is the effective method,
    one call instead of two. That makes ordinary generic functions 6–11%
    faster than before this part.
  - Against before, 1 million calls:
    - readers 74 to 93 ms (tier 0) and 28 to 44 ms (tier 1);
    - `slot-value` 59 to 69 and 14 to 17;
    - `make` 344 to 369 and 289 to 299;
    - `(:k obj)` unchanged.
- **A bug it found:** `defclass`, `defgeneric` and `defmethod` looked up
  what they redefine by its unqualified name in the namespace current when
  the form *ran*. Inside a function (a test body, say), that wasn't the
  namespace the form was written in, and a `defmethod` there replaced its
  generic function, losing its other methods. They use the full name now.
- **Not done:** the long form of `define-method-combination`, and slots on
  metaclasses (a metaclass is for specialising the protocol; data about a
  class goes in its `:meta`).
- **Tests:** `mop_test.norn`, 34 checks at both tiers: classes' classes,
  printing (in collections, inherited, and the plain default),
  `initialize-instance` through `(C …)`, `make` and `make-instance`,
  readers specialised and shared, a metaclass auditing slot reads and
  making, a generic-function class watching method choice (with the cache
  still working), a combination of one's own, protocols on a class and on
  `Integer`, and which classes keep the fast paths.


**9.6, processes and supervisors (2026-11-14): done.**

- **`norn.actor`** (`norn.process` is the operating system's processes).
  A process is a fiber with a mailbox:
  - `spawn-process`, `self`, `send` (to a process or a registered name;
    to one that has ended, dropped), `register!` and `whereis`;
  - `receive` is selective: it takes the first message one of its clauses
    matches (`match`'s patterns and `:when` guards), leaving the rest in
    order, and waits for one, or `:after ms`. The bodies are in tail
    position, so `defprocess` (a `defn` whose body loops) keeps its state
    in `recur`;
  - `ask` sends a message with a promise on the end and waits for the
    process to deliver it, from anywhere, in a process or not (a
    `TimeoutError` after 5 s).
- **Ends.** A process's reason is `:normal`, the condition it failed with
  (the very one), `:killed`, or what it was told with `exit!`:
  - `link!` and `spawn-link`: an abnormal end ends the linked process too,
    with the same reason; `trap-exits!` makes it `[:EXIT p reason]`
    instead (and `exit!` a message, except `:kill`);
  - `monitor`: `[:DOWN ref p reason]`;
  - `exit-reason`, `alive?` and `await-exit`.
- **A crash** is seen by the process's outermost handler, which tells its
  links and monitors and declines, so the condition goes on to the
  debugger as any fiber's does (design §5.3), while the supervisor
  restarts the process.
- **Supervisors:** `supervisor` with `child` specs:
  - children are started in order, and stopped the last first: unlinked,
    told `:shutdown`, given `:shutdown-ms` (5000), then killed;
  - `:one-for-one`, `:one-for-all` and `:rest-for-one`;
  - `:permanent`, `:transient` and `:temporary` children (a temporary one
    stopped for a sibling's restart stays stopped);
  - more than `:max-restarts` (3) in `:max-ms` (5000) and it stops its
    children and ends, `:too-many-restarts`;
  - `supervise` is a child function, for supervisors under supervisors: a
    supervisor that hears its own supervisor end stops its children and
    ends too;
  - `which-children` and `stop-supervisor!`.
- **Something to allow for:** a fiber inherits the `handler-bind`
  handlers of the fiber that spawned it (by design: its docstring says
  so), as it does dynamic bindings. So the supervisor's handler saw its
  children's crashes as its own. Each process's handler now checks that
  it's in the process's own fiber.
- **Core:** `%pattern-vars` is public, for `receive` (its test for a clause
  mentions the names a pattern binds, so lint doesn't call them unused).
- **Not done:** distribution (processes on other machines), and a
  process's crash report is the fiber's ("Fiber N failed: …").
- **Tests:** `actor_test.norn`, 44 checks at both tiers: messages,
  selective receive and timeouts, names, crash reasons, links (abnormal,
  normal, trapped, to a process already ended), `exit!` and `:kill`,
  monitors, a supervisor restarting a crashed child under its name, the
  three strategies, the three restart kinds, too many restarts, a
  supervisor under a supervisor, and `ask` timing out. Run ten times at
  both tiers without a failure.

**9.7, `norn.stm` (2026-11-14): done.**

- **Refs and transactions,** MVCC as Clojure's:
  - `ref` (options `:validator` and `:max-history`), `dosync`, `alter`,
    `ref-set`, `commute`, `ensure`, and `deref`/`@`;
  - a transaction reads every ref as of its read point, so it sees one
    moment throughout, and its writes stay its own until it commits;
  - a commit locks the refs it changes (in id order, so commits can't
    deadlock), checks that none changed since the read point, and writes
    them all at one new point; otherwise the transaction runs again;
  - a reader that meets a commit's lock waits for it, since the commit
    may be writing a value at the reader's point;
  - `alter` and `ref-set` on a ref already changed since the read point
    run the transaction again at once, not at commit.
- **`commute`** is applied again at commit to the newest value and never
  conflicts. Nor does it run the transaction again for a value older than
  the ref's history: its newest value will do.
- **`ensure`** guards a ref only read, against write skew.
- **History:** a ref keeps `:max-history` (10) old values for older
  transactions; one that needs a value older than that runs again.
- **Around the transaction:**
  - `io!` refuses side effects inside one, and `after-commit` runs
    them once it commits;
  - a `dosync` inside one is part of it; `in-transaction?`;
  - a fiber started inside one isn't in it (it inherits the binding,
    not the transaction);
  - watches (`add-watch`) fire after a commit, and a validator's
    refusal is an error with nothing changed;
  - `*max-retries*` (10000), and `stm-stats` counts commits and retries
    and why.
- **Values only** (design §4.2): a ref can't hold a mutable object
  (shallowly checked), unless it's shared.
- **Retries unwind with a catch tag,** not a condition, so a handler or a
  `catch` in the body never sees them.
- **Speed.** The first version took 12 µs for a transaction that only
  reads, 25 µs for one with an `alter`, and 2.5 µs to deref a ref outside
  one. Three gaps in the runtime were most of it:
  - A primitive handing a call over (`ErrKind::Call`: `deref` of a delay,
    promise, future or anything with a `deref-object` method, and
    `slot-value` through the MOP) built a backtrace it never used: 7 µs a
    call, now 1.
  - `handler-case` built its handler list with a lazy `concat` and
    `map-indexed`, which `signal` then walked: twice as fast now, with
    lists and a cheap catch tag.
  - `subvec` was `(vec (take … (drop …)))`: 5 µs for ten items. It's a
    primitive for vectors now, 20 times faster.
  Now (tier 0, tier 1): 3.0/2.5 µs for a transaction that reads, 10/9 µs
  with an `alter`, 0.3/0.2 µs to deref a ref outside one. 16 fibers each
  making 500 transfers among 10 accounts: 257 ms before, 110 ms now.
- **Not done:** Clojure's barging (an older transaction winning a conflict
  against a younger one); here a long transaction under steady writes to
  its refs can lose again and again, up to `*max-retries*`. History is
  fixed, not grown on a fault.
- **Tests:** `stm_test.norn`, 38 checks at both tiers: transactions and
  nesting, one moment throughout, a conflict running a transaction
  again, write skew without and with `ensure`, `commute` not conflicting,
  12 fibers transferring at once, history, validators and watches, values
  only, `io!`, `after-commit` and fibers started inside. Run ten times at
  both tiers without a failure.

**9.8, the full FFI (2026-11-14): done.**

- **Memory** (`norn.ffi`):
  - addresses are integers;
  - `alloc` (bytes, or room for a type) and `free!`, and `with-alloc`;
  - `mem-get` and `mem-set!` for each C type at an address and offset;
  - `string-at`, `bytes-at`, `write-bytes!` and `alloc-string`.
- **Structs:** `(defcstruct point [x :i32 y :i32])` lays the fields out
  as C does, with nested structs and `[:array t n]`:
  - `sizeof`, `alignof` and `offset-of` (a key, or a path through nested
    structs);
  - `struct-get` and `struct-set!` (as maps), `field` and `set-field!`,
    and `new-struct`;
  - a parameter `[:* layout]` takes a map, copied to memory for the call
    (or an address), and `[:out layout]` is a struct C fills, returned as
    a map after the result.

  `struct tm`, filled by the C library's `gmtime_r`, reads back right,
  `:string` field included.
- **Callbacks:** `(callback [:pointer :pointer] :int f)`,
  `callback-address`, `free-callback!`, and a `[:callback params ret]`
  parameter, which takes a callback or a plain function (made a callback
  once and kept):
  - during a call from Norn, on its thread, f runs there and then, as a
    nested run;
  - from another thread (the blocking pool, or one of C's own), the call
    goes to a fiber serving the callback, while that thread waits;
  - an error in f can't unwind through C: C gets 0, and the error is
    signalled when the call from Norn returns (from another thread, it's
    reported).

  `qsort` sorts with a Norn comparator both ways, fast and `:blocking`.
- **`function-at`:** a C function known by its address (a function
  pointer C returned).
- **Checked signatures:** `(defcfun strlen libc "strlen" [s :string]
  :usize)` names the parameters. That gives a fixed arity (calls are 20%
  faster than through `& args`), and a contract checked on calls from
  other modules before C sees the arguments. The old form, types alone,
  still works, unchecked.
- **Unsafe modules:** a module that uses the raw FFI says `(:unsafe)` in
  its `ns`.
  - The expander refuses `norn.ffi`'s raw functions and the
    `%ffi-`/`%mem-` primitives under them anywhere else.
  - The REPL's `user` may use them.
  - A module that doesn't say it may still call one that does, and
    layouts (`defcstruct`, `sizeof`) are for anyone.
  - `lib_test.norn` says `(:unsafe)` now.
- **Not done in 9.8, then done in 9.8b:** structs by value, variadic
  functions, unions and bit fields, and reusing a freed callback's code.
- **Tests:** `ffi_test.norn`, 43 checks at both tiers:
  - memory in every C type;
  - layouts against C's rules, nested structs and arrays read and
    written, and `struct tm` from `gmtime_r`;
  - structs by value refused;
  - callbacks: `qsort` with a function and with a callback, an error
    signalled after C returns, floats and narrow integers through a
    callback's own address, and from the blocking pool;
  - checked signatures across modules;
  - the `(:unsafe)` rule, for the library and the primitives.

  Run ten times at both tiers without a failure.

**9.8b, the rest of the FFI (2026-11-14): done, on arm64.**

- **Structs by value,** in calls and in callbacks, both ways: a layout as
  a parameter or result type, passed and returned as a map (an address
  for a parameter is copied). arm64's rules (AAPCS64, and Apple's):
  - one to four floats or doubles of one type go in float registers;
  - anything else up to 16 bytes in one or two integer registers;
  - anything larger by reference, and a result through x8;
  - a struct that doesn't fit the registers left goes on the stack, whole.
- **Variadic functions:** `:...` last in the types, or `& more` in
  `defcfun`'s parameters. The extra arguments are numbers (64-bit
  integers, doubles), strings, Bytes and nil, with a stub for each call's
  types (cached). On Apple arm64 they all go on the stack, as Apple's ABI
  says. `snprintf` works.
- **Unions:** `defcunion`.
- **Bit fields:** `[:bits type width]` in a struct, placed as C does (in a
  unit of the type, from the lowest bit; one that doesn't fit starts the
  next unit; width 0 ends the unit). Signed ones read sign-extended, and
  writes are range-checked and leave the neighbours alone.
- **A freed callback's trampoline** is reused by the next callback with
  its signature, so executable memory doesn't grow with callbacks made
  and freed.
- **Against C itself:** `tests/norn/ffi_fixture.c`, which `ffi_test.norn`
  builds with `cc` into a shared library. It covers:
  - layouts as C computes them (padding, bit fields, unions);
  - each kind of struct by value, as arguments and results, and with
    the registers used up;
  - variadic `long`s, `double`s and a mix;
  - callbacks taking and returning each kind;
  - a callback from a thread of C's own, during a blocking call and
    during a fast one.

  Every case was right at the first run against C.
- **Not done:** x86-64 (System V's classification of each eight bytes,
  and `%al` for a variadic call, which Cranelift can't set) and Windows:
  they come with M12's ports, and are an error until then.
- **A race it showed up** (from 8.3): the expander loads `norn.validate`
  the first time a macro expands, and another fiber expanding meanwhile
  skipped the checks. Now it waits for the load. (That wasn't all of the
  flaky `pattern_test`, though: see 9.9.)
- **Tests:** `ffi_test.norn`, now 78 checks at both tiers. Run ten times
  at both tiers without a failure.

**9.9, SQLite (2026-11-14): done.**

- **`norn.sqlite`,** written in Norn on `norn.ffi` and the system's
  libsqlite3 (`NORN_SQLITE` names another). It's the FFI's first real
  user, and needed nothing new from it.
  - `open` (a path or `":memory:"`; `:readonly`, `:create`,
    `:busy-timeout`, `:blocking`), `close!` and `with-connection`.
  - `query` (rows as maps of column keywords, or `{:as :vectors}`),
    `query-one`, `execute!` (the rows changed), `exec!` (a script),
    `last-insert-id`, `tables` and `version`.
- **SQL** is a string, or a vector of it and its parameters: `?` and
  `?N` in order, or a map for `:name`, `@name` and `$name`.
  - Values: nil, integers, doubles, strings, Bytes, booleans and
    keywords.
  - Columns come back as integers, doubles, strings (NUL inside kept),
    Bytes and nil.
- **Prepared statements** are cached per connection by their text (64 at
  most) and reused.
- **Transactions:** `with-transaction` (`:mode :deferred`, `:immediate`
  or `:exclusive`) commits, or rolls back on any error, which goes on.
  One inside another is a savepoint, rolled back alone.
- **Fibers share a connection** safely: a lock per connection (on
  `norn.sync`'s semaphore) lets one fiber at a time use it, and a
  transaction keeps it for its fiber until the transaction ends.
- **Errors** are `DatabaseError`s, with `ConstraintError` and `BusyError`
  under them, carrying SQLite's `:code`, `:extended-code`, message and
  `:sql`. A connection is an error to use once it's closed, or after an
  image restore.
- **Calls** run on the carrier, which is quick for most databases.
  `:blocking true` runs `sqlite3_step` and `sqlite3_exec` on the blocking
  pool instead.
- **Speed** (an in-memory database, tier 0): 10,000 inserts in a
  transaction in 61 ms, and 10,000 rows of five columns read in 51 ms.
- **The flaky `pattern_test`, found:** `compiler_test` checks the
  compiler's self-hosting fixpoint by installing the plain compiler as
  `eval` while it compiles itself twice. `cargo test` runs every suite in
  one runtime, so for that long the other suites' `eval` skipped the
  expander. Errors then came from the seed compiler's checks, not
  `norn.validate`'s. It showed after every rebuild, when an empty module
  cache made those stages slow: with a fresh cache it failed 4 runs out
  of 4. `compiler_test` now runs each stage through that stage's own
  `eval`, installing nothing, and 5 fresh-cache runs out of 5 pass.
- **Not done:** Studio's Databases panel (it can come now that the
  library has landed), streaming a large result rather than a vector, and
  user-defined SQL functions (callbacks would do it).
- **Tests:** `sqlite_test.norn`, 39 checks at both tiers:
  - values round-tripping, and parameters of each kind, with their
    mistakes;
  - changes and ids, and errors with their classes and codes;
  - transactions: commit, rollback, savepoints and `:immediate`;
  - a file with two connections, including busy and read-only;
  - 8 fibers making transactions on one connection;
  - a `:blocking` connection, and the statement cache.

  Run ten times at both tiers without a failure.

**9.10, pseudo-terminals (2026-11-14): done.**

- **`norn.process/start-pty`** starts a program on a pseudo-terminal
  (`openpty`), rows × cols (24 × 80), with options as for `start`.
  - The child gets a new session, with the terminal as its controlling
    terminal (`setsid`, `TIOCSCTTY`), so `^C` at the terminal is a
    SIGINT to it.
  - Signals the runtime handles or ignores are set back to their
    defaults before `exec`. An ignored one stays ignored across `exec`,
    and `^C` would do nothing.
  - `TERM` is `xterm-256color` unless `:env` says otherwise.
- **Streams:** `:in` (what's typed) and `:out` (what the terminal shows:
  standard output and error together, and the echo) are the master's.
  They're the reactor's pipe kinds, so `norn.io` reads and writes them.
  A read once the child has gone (`EIO`) is the end of input.
- **`resize`** sets the size (`TIOCSWINSZ`; the child gets SIGWINCH).
- **Something to know, found writing the tests:** on macOS a program's
  exit waits until its last terminal output is read. A `wait` with that
  output unread never returns. `start-pty`'s docstring says to read `:out`
  as the program runs, as a terminal emulator does.
- **Tests:** `pty_test.norn`, 13 checks at both tiers: size, a terminal
  for stdin and stdout, `TERM`, echo and `\r\n`, the end of output,
  resizing, `^C` as SIGINT, and `cat` ended by `^D`.

**9.11, `norn.bin` (2026-11-14): done.**

- **A binary format for Norn values:**
  - `encode` and `decode`, and `write-file` and `read-file`;
  - `NRNB`, a version byte, then tagged values, with LEB128 counts and
    zigzag integers;
  - every kind of data: nil, booleans, integers of any size, floats,
    ratios, characters, strings, keywords, symbols, lists (and any
    realized sequence), vectors, maps, sets, sorted maps and sets,
    queues, Bytes, regexes, and instances of immutable classes;
  - a keyword or symbol after its first use is a reference, so records
    come out about a third smaller than their text;
  - not written: functions, vars, atoms, channels, fibers, handles and
    mutable objects (values only).
- **Safe for untrusted input:**
  - every count is checked against the bytes left;
  - text must be UTF-8, a map or set may not repeat a key, and nothing
    may follow the value;
  - nesting is limited (`:max-depth`, 512; 10,000 at most);
  - an instance is made only of a class in `:classes`, straight from its
    slots (no `initialize-instance`);
  - bad input is always a `ParseError`: 20,000 mutated encodings gave a
    value or a `ParseError`, nothing else.
- **Random values** (1,500 in the suite, seeded) round-trip equal, with
  the same hash, printed the same, and through text with `norn.edn` too.
- **Gaps it found:**
  - **The printer writes a queue as `#queue [...]`, which the reader
    didn't read.** It does now (its items are data, not evaluated, as
    `#bytes`'s are).
  - **Deep nesting crashed the process.** The reader overflowed the
    native stack between 20,000 and 40,000 levels: 200 KB of `[` killed
    it, which matters for `norn.edn`'s promise about untrusted text.
    It's an error past 10,000 levels now, and so is `norn.bin` both
    ways.
  - **Still open:** printing and `=` overflow too, between 40,000 and
    70,000 levels. Since reading stops at 10,000, only a value built in
    code that deep reaches them. The fix is a stack that grows (the
    `stacker` crate) or rewrites without recursion, a choice for later.
- **Tests:** `bin_test.norn`, 128 checks at both tiers.

**9.12, the smaller libraries (2026-11-14): done.**

- **`norn.compress`:** deflate, zlib and gzip (flate2's pure-Rust
  backend), `crc32`, and a `:max-size` limit (256 MiB) on what
  decompressing makes.
- **`norn.crypto`,** on ring, which rustls already links:
  - SHA-256/384/512 (and SHA-1 for old protocols);
  - HMAC, with `hmac-valid?` in constant time;
  - PBKDF2;
  - `random-bytes` and `random-token` from the system's secure
    generator;
  - `constant-time=`.

  Checked against FIPS 180's, RFC 4231's and RFC 6070's examples.
- **`norn.uuid`:** version 4 (secure random) and version 7 (ordered by
  time) as canonical strings, `parse` (braces, no dashes, any case),
  `valid?`, `version`, `timestamp-ms` and bytes both ways.
- **`norn.csv`** (RFC 4180): `read-csv`, `read-maps`, `write-csv` and
  `write-maps`. Quoted fields hold separators, doubled quotes and line
  breaks; `:separator` and `:quote` are options.
- **`norn.toml`** (TOML 1.0): `read-string` and `write-string`.
  - It reads every value kind: four kinds of string with their escapes
    and line-ending backslashes, integers in four bases with
    underscores, floats with `inf`/`nan`, and dates and times (an offset
    date-time is an instant, a local one its text).
  - Also dotted keys, inline tables, and tables and arrays of tables,
    nested.
  - What the spec forbids (a key or table twice, leading zeros, bad
    escapes…) is a `ParseError` with its line.
  - It reads the example at toml.io.
- **`norn.zip`:**
  - `entries`, `read-entry`, `unzip`, `extract!` and `zip`;
  - stored and deflated entries, UTF-8 names, and CRCs checked;
  - for untrusted archives, a total `:max-size`, and `extract!` refuses
    a name that would land outside its folder before writing anything;
  - the system's `unzip -t` passes our archives, and we read its `zip`'s.
- **`norn.schedule`:**
  - cron expressions: five fields with ranges, lists, steps and names,
    either-day matching as cron does, and `@daily` and the rest;
  - `next-run` and `next-runs` in a zone;
  - `schedule` runs a function in a fiber each time it's due, and
    `stop!` stops it.
- **`norn.data`:** `diff`, as clojure.data's: `[only-in-a only-in-b
  in-both]`, by key, member and index.
- **Gaps they found:**
  - **Sorted maps and sets weren't functions of their keys**, as maps
    and sets are. Now they are.
  - **There's no metadata on values:** no `meta` or `with-meta`. The
    reader keeps `^…` only for the expander. `norn.toml` keeps its
    marks beside the data instead. Clojure code expects metadata, so
    it's a question for sealing.
- **Not done:** ZIP64, and encrypted or other-method zip entries.
- **Tests:** `small_libs_test.norn`, 105 checks at both tiers. They're
  checked against the system's `gzip`, `zip` and `unzip` as well as the
  RFCs' vectors.

**Found and fixed (after 9.12): the intermittent crash.**

- **What happened.** With every suite running in one runtime
  (`norn_suites`), the process died now and then (SIGSEGV or SIGBUS), in
  the GC's tracing or the interpreter's frames. That was about 1 run in 10,
  and 9.9's build did it too. It needed the SQLite suite alongside a
  broad mix of the others, and more than one carrier: with
  `NORN_THREADS=1` it never happened.
- **The cause.** A carrier keeps one free stack segment cached
  (`Vm::spare`, the "hot split" cache), and the cache isn't a GC root. It
  was dropped before a collection at a safepoint, and when a carrier
  idles, but not in `native_region`. A collection can run while a carrier
  is inside a fast FFI call, and then it freed the cached segment. Back
  from C, the next deep call took that segment for its frames, writing
  them over whatever had been put in its memory. SQLite's suite makes
  thousands of fast calls, some of them long, so it was the one that hit
  it.
- **Also in the FFI:** a callback's nested run could leave a segment in
  the cache before the carrier went back into native code. And an error a
  callback raised, kept until C returns, could hold a condition that
  wasn't rooted meanwhile. Both are fixed.
- **How it was found.** First bisecting suites and tests, then
  `NORN_THREADS=1` and `=2` (it needed two carriers, and two made it
  frequent). Then a heap-check build: `norn-rt`'s new `gc-check` feature
  (MMTk's valid-object bit) checks every pointer the GC follows. It found
  no bad one, so the damage was in a stack segment, and a crash in
  `backtrace` pointed there.
- **The test:** `crates/norn/tests/io.rs`
  `a_collection_during_a_native_call_spares_the_cached_segment`. One fiber
  comes back across a segment boundary and enters a native call while
  another collects. It crashed 6 runs out of 6 without the fix, and
  passes with it.

**The M9a done-check (2026-11-15).**

- **Studio's panels:** the API workbench, the terminal and the Databases
  view run on `norn.http`, the pty support and SQLite.
- **Checked signatures:** every public function of the twenty libraries
  M9a and the stdlib batch added has one:
  - `norn.actor`, `bin`, `cli`, `compress`, `crypto`, `csv`, `data`,
    `ffi`, `http`, `process`, `property`, `schedule`, `sqlite`, `stm`,
    `terminal`, `time`, `toml`, `uri`, `uuid` and `zip`.
  - The exceptions are listed in `contracts_test.norn`, which checks the
    rest:
    - ten functions that take nothing a type can say (no arguments,
      only a function, or a process or its name);
    - `ffi/call`, which every `defcfun` function calls after checking its
      own C signature.
  - Types are as wide as what each function accepts. Keyword options and
    function-valued parameters are left untyped. A result type is written
    only where every path gives it.
  - Four functions that ran their argument through `str` now want what
    their docs say: `uri/parse`, `csv/read-csv`, `process/start` and
    `bin/write-file`.
- **What it cost, and two fixes:**
  - At first, SQLite inserts were three times slower, because every C
    call went through `ffi/call`'s check on its slow path. `call` has no
    signature now.
  - `(U …)` and `(? …)` tried each member behind a catch, as costly as a
    `try`. When each member is a class or a predicate, they now use a
    plain test, as a class does.
  - 10,000 inserts take 110 ms (93 ms with no signatures). The rest is the
    check on each `sqlite/execute!` call.
- **Blame:** `contracts_test.norn` checks both sides.
- **A crash held in the debugger:** `actor_test.norn`
  `a-crash-stays-in-the-debugger-while-it-is-restarted`. Under
  `*debugger* :park`, the supervisor restarts the child and the new one
  answers, while the crash waits in the debugger.
- **Tiers:** every suite passes at tier 0 and tier 1.
- **Found on the way:** `norn fmt` indented a body under the parameter
  vector when a result type came first (`(^Map [a]` and the body on the
  next line). A leading `^hint` now belongs to what follows it.

Sealing can start once the M7.6 working stretch is done.
