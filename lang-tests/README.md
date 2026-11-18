# lang-tests

Programs written to try Skein the way its users will, before its standard
library was sealed. What they found is in `docs/plans/todo.md`. Each is a project
of its own, with its own `skein.edn` and tests (`skein test` in its folder).
Each was a repository of its own until 2026-11-17, and its history came
with it (`git log lang-tests/quire`).

- **pulse**: an uptime monitor. SQLite, an HTTP server and client, a
  supervised process per target, `skein.cli`, `skein.log`, `skein.schedule`.
- **ledger**: bank exports to categories and reports. A Git dependency
  (skein-html), CSV and decimals, TOML, a full-screen terminal browser, EDN
  tags and date patterns.
- **quire**: a static site generator. Markdown and YAML, a `defsyntax`
  template macro, RSS and a sitemap, a zip, pipelines, restarts, a preview
  server that reloads browsers over server-sent events.
- **chat**: a chat server over TCP and TLS. Many connections, channels,
  STM, `skein.sync`.
- **loglens**: a log analyzer. Gzip'd JSON Lines as a stream, named regex
  groups, relations, diffs, percentiles, `skein.bin`, CBOR, `skein.bench`.
- **stash**: a key-value server speaking the Redis protocol. Sharded
  atoms, classes and generic functions for its kinds of value, sorted
  sets, expiry, publish and subscribe, pipelining, snapshots. Written
  after the others (2026-11-18), for what they left out: a heap that
  stays, and many carriers working on shared state.

## A check on Skein itself

They use the language as a program does: libraries together, macros,
fibers, restarts. So they catch what the unit suites miss, and a change to
Skein should leave them working. `run.skn` checks that:

```sh
./target/release/skein lang-tests/run.skn                 # about a minute
./target/release/skein lang-tests/run.skn --only quire    # one project (repeatable)
./target/release/skein lang-tests/run.skn --record        # after a change on purpose
SKEIN_LANG_TESTS=1 cargo test --release -p skein --test lang_tests
```

- **Tests:** each project's `skein test` at tier 0, at tier 1 (the default),
  and with everything compiled from its first call
  (`SKEIN_JIT_THRESHOLD=1 SKEIN_JIT_SYNC=1`).
- **Outputs:** the programs on fixed inputs (`outputs.edn`): loglens's
  reports on a generated log, ledger's imports and reports, and quire's
  build of its sample site, stash's replies to a script of commands and
  the snapshot it saves. What each prints, and the files it writes,
  must be the same in all three modes and as recorded in `expected/`. A
  miscompile shows up as a different output even where no test checks
  that value. A path of the temporary folder is written `$TMP`, so the
  recording doesn't depend on the machine. When outputs differ, the
  runner keeps each mode's folder and prints where they are, to compare
  with `diff -r`.
- **Coverage:** the lines of the standard library (`lib/skein`) the
  projects' tests reach, against `expected/coverage.edn`: which modules
  real programs leave untouched, and where the next lang-test could go.
  It's a report, not a check: a line or two moves with timing. Modules
  that `skein test` loads itself, and skein.core and the compiler (loaded
  at start-up), aren't counted.

ledger and quire depend on [skein-html](https://github.com/crazymevt/skein-html)
by Git, pinned in their `skein.lock`, so running them needs the network the
first time (skein caches what it fetches). A project whose `skein.lock`
names a local folder that isn't there is skipped, and the runner says so.

chat and pulse have no fixed outputs: they're servers, whose output
depends on the network and the clock. Their tests cover them. stash is a
server too, but can run a script against a store whose clock only the
script moves.
