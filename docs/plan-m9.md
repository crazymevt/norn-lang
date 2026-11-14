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

**Done-check (M9a), proposed:**

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

