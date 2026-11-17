# ledger

Imports bank exports (CSV), gives the transactions categories by rule or by
hand, and shows where the money went, as a table or an HTML page.

```sh
skein run import checking ~/Downloads/checking.csv   # add an account's export (again: nothing doubles)
skein run list --month 2026-11 --uncategorized       # the transactions
skein run rules                                      # give the rules' categories to those without one
skein run browse                                     # browse and categorize in the terminal
skein run report                                     # spending by month and category
skein run report --html report.html                  # the same as a page
skein test                                           # its tests
skein package                                        # a command-line archive in dist/
```

The config is `ledger.toml` (`--config FILE`, or `LEDGER_CONFIG`): where the
ledger file is (beside the config), how each account's export reads, and the
rules. `tests/fixtures/ledger.toml` is an example. Amounts are exact decimals.

In `browse`: arrows, Page Up/Down, Home and End move; `/` filters by
description; `c` sets the category (Tab completes); `u` shows only those
without one; `s` saves; `q` quits.

The HTML report is built with [skein-html](https://github.com/crazymevt/skein-html), a Git
dependency pinned in `skein.lock`.
