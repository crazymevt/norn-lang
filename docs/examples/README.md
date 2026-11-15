# Norn examples

[Documentation home](../README.md)

Build the binary from the repository root with
`cargo build --release -p norn --bin norn`. Run the following commands
from that same directory.

| Program | Run | Prints |
| --- | --- | --- |
| [Totals](totals.norn) | `./target/release/norn docs/examples/totals.norn` | `[[:books 35] [:games 30]]` |
| [Generic functions](dispatch.norn) | `./target/release/norn docs/examples/dispatch.norn` | `[:book-envelope :express-parcel :standard]` |
| [Conditions and restarts](restarts.norn) | `./target/release/norn docs/examples/restarts.norn` | `[12 0 7]` |
| [Producer and consumer](channels.norn) | `./target/release/norn docs/examples/channels.norn` | `[1 4 9 16 25]` |

- **Totals** reads JSON with keyword keys and uses a transducer to select paid
  orders before reducing their totals. Change a `"paid"` field
  to `false` in the JSON to exclude that order. See
  [collections and sequences](../reference/collections.md).
- **Generic functions** dispatches on both a package class and a delivery
  class. Try passing an express delivery with a book and observe which method
  wins. See [classes and generic functions](../reference/objects.md).
- **Conditions and restarts** lets the caller choose a replacement for malformed
  input. Use `(invoke-restart 'skip-reading)` in the handler to omit the value. See
  [conditions and restarts](../reference/conditions.md).
- **Producer and consumer** sends values through an unbuffered channel and
  closes it when the producer finishes. Try `(chan 2)` to let the producer
  buffer up to two values. See [concurrency](../reference/concurrency.md).
