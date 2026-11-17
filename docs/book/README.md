# The Skein Book

[Documentation home](../README.md)

A guide to programming in Skein, from the first expression at the REPL to a
packaged application. It teaches the language and its standard libraries
in order, with examples you can run. For complete lists of functions and
options, use the [language reference](../reference/README.md) and
`skein doc`.

## Part I · The language

1. [Introduction](01-introduction.md)
2. [First steps](02-first-steps.md)
3. [Values and expressions](03-values-and-expressions.md)
4. [Functions and control flow](04-functions-and-control-flow.md)
5. [Collections](05-collections.md)
6. [Sequences and transducers](06-sequences-and-transducers.md)
7. [Text](07-text.md)
8. [Modules and projects](08-modules-and-projects.md)

## Part II · Objects, errors, and the live system

9. [Classes and generic functions](09-classes-and-generic-functions.md)
10. [Protocols and extension](10-protocols-and-extension.md)
11. [Conditions and restarts](11-conditions-and-restarts.md)
12. [Live programming](12-live-programming.md)
13. [Contracts](13-contracts.md)
14. [Macros](14-macros.md)

## Part III · Concurrency

15. [Fibers and scopes](15-fibers-and-scopes.md)
16. [Channels](16-channels.md)
17. [Shared state](17-shared-state.md)
18. [Actors and supervision](18-actors-and-supervision.md)

## Part IV · Working with the world

19. [Files and streams](19-files-and-streams.md)
20. [Data formats](20-data-formats.md)
21. [The operating system](21-the-operating-system.md)
22. [Networking and HTTP](22-networking-and-http.md)
23. [Databases](23-databases.md)
24. [Command-line programs](24-command-line-programs.md)

## Part V · Shipping

25. [Testing](25-testing.md)
26. [Tools and performance](26-tools-and-performance.md)
27. [Images and deployment](27-images-and-deployment.md)
28. [Foreign functions](28-foreign-functions.md)

## Appendix

29. [Skein for Lispers](29-skein-for-lispers.md)

## Running the examples

Every `clojure` block in the book runs under `check.py`, which compares
each result with the `; =>`, `; prints:` or `; !!` comment after it. The
blocks of a chapter run in order in one fresh `skein` process, in a
temporary folder.

```sh
python3 docs/book/check.py                       # every chapter
python3 docs/book/check.py docs/book/16-channels.md
python3 docs/book/check.py --skein path/to/skein   # a different build
```

A block that can't run unattended, because it needs the network or a
terminal, has `<!-- skip -->` on the line before its fence. See the
docstring at the top of [check.py](check.py) for the details.
