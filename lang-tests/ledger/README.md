# ledger

Imports bank exports (CSV), gives the transactions categories by rule or by
hand, and shows where the money went, as a table or an HTML page.

```sh
norn run import checking ~/Downloads/checking.csv   # add an account's export (again: nothing doubles)
norn run list --month 2026-11 --uncategorized       # the transactions
norn run rules                                      # give the rules' categories to those without one
norn run browse                                     # browse and categorize in the terminal
norn run report                                     # spending by month and category
norn run report --html report.html                  # the same as a page
norn test                                           # its tests
norn package                                        # a command-line archive in dist/
```

The config is `ledger.toml` (`--config FILE`, or `LEDGER_CONFIG`): where the
ledger file is (beside the config), how each account's export reads, and the
rules. `tests/fixtures/ledger.toml` is an example. Amounts are exact decimals.

In `browse`: arrows, Page Up/Down, Home and End move; `/` filters by
description; `c` sets the category (Tab completes); `u` shows only those
without one; `s` saves; `q` quits.

The HTML report is built with [norn-html](https://github.com/crazymevt/norn-html), a Git
dependency pinned in `norn.lock`.
