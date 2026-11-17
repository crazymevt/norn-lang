# lang-tests

Programs written to try Norn the way its users will, before its standard
library was sealed. What they found is in `docs/plans/todo.md`. Each is a project
of its own, with its own `norn.edn` and tests (`norn test` in its folder).
Each was a repository of its own until 2026-11-17, and its history came
with it (`git log lang-tests/quire`).

- **pulse**: an uptime monitor. SQLite, an HTTP server and client, a
  supervised process per target, `norn.cli`, `norn.log`, `norn.schedule`.
- **ledger**: bank exports to categories and reports. A Git dependency
  (norn-html), CSV and decimals, TOML, a full-screen terminal browser, EDN
  tags and date patterns.
- **quire**: a static site generator. Markdown and YAML, a `defsyntax`
  template macro, RSS and a sitemap, a zip, pipelines, restarts, a preview
  server that reloads browsers over server-sent events.
- **chat**: a chat server over TCP and TLS. Many connections, channels,
  STM, `norn.sync`.
- **loglens**: a log analyzer. Gzip'd JSON Lines as a stream, named regex
  groups, relations, diffs, percentiles, `norn.bin`, CBOR, `norn.bench`.

## A check on Norn itself

They use the language as a program does: libraries together, macros,
fibers, restarts. So they catch what the unit suites miss, and a change to
Norn should leave them working. `run.norn` checks that:

```sh
./target/release/norn lang-tests/run.norn                 # about a minute
./target/release/norn lang-tests/run.norn --only quire    # one project (repeatable)
./target/release/norn lang-tests/run.norn --record        # after a change on purpose
NORN_LANG_TESTS=1 cargo test --release -p norn --test lang_tests
```

- **Tests:** each project's `norn test` at tier 0, at tier 1 (the default),
  and with everything compiled from its first call
  (`NORN_JIT_THRESHOLD=1 NORN_JIT_SYNC=1`).
- **Outputs:** the programs on fixed inputs (`outputs.edn`): loglens's
  reports on a generated log, ledger's imports and reports, and quire's
  build of its sample site. What each prints, and the files it writes,
  must be the same in all three modes and as recorded in `expected/`. A
  miscompile shows up as a different output even where no test checks
  that value. A path of the temporary folder is written `$TMP`, so the
  recording doesn't depend on the machine. When outputs differ, the
  runner keeps each mode's folder and prints where they are, to compare
  with `diff -r`.
- **Coverage:** the lines of the standard library (`lib/norn`) the
  projects' tests reach, against `expected/coverage.edn`: which modules
  real programs leave untouched, and where the next lang-test could go.
  It's a report, not a check: a line or two moves with timing. Modules
  that `norn test` loads itself, and norn.core and the compiler (loaded
  at start-up), aren't counted.

ledger and quire depend on [norn-html](https://github.com/crazymevt/norn-html)
by Git, pinned in their `norn.lock`, so running them needs the network the
first time (norn caches what it fetches). A project whose `norn.lock`
names a local folder that isn't there is skipped, and the runner says so.

chat and pulse have no fixed outputs: they're servers, whose output
depends on the network and the clock. Their tests cover them.
