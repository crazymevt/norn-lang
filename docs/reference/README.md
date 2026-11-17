# Skein language reference

[Documentation home](../README.md) · [Symbol index](symbols.md) · [Standard library](libraries.md)

| Chapter | Topics |
| --- | --- |
| [Syntax and evaluation](syntax.md) | Reader notation, evaluation, binding, functions, control flow, modules, and macros. |
| [Values, collections, and sequences](collections.md) | Equality, numbers, persistent data, eager operations, streams, and transducers. |
| [Classes and generic functions](objects.md) | Slots, construction, multiple dispatch, method combination, and protocols. |
| [Conditions and restarts](conditions.md) | Signaling, handling, recovery, cleanup, and debugger entry points. |
| [Concurrency and state](concurrency.md) | Fibers, channels, atoms, dynamic bindings, and transactions. |
| [Standard library](libraries.md) | Bundled namespaces, API discovery, and representative examples. |

Signatures use `…` for repeated forms. In code examples, `; =>` shows the
expected result. `skein doc NAME` reads the current runtime's documentation
metadata, and `(source NAME)` at the REPL shows a definition when source is
available.
