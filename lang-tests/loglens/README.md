# loglens

Reports on request logs: JSON Lines, gzip'd or not, read a line at a time.
By route (count, p50 and p95 latency, errors) and by the users' plan; a
random sample; how today compares with a saved report.

```sh
norn run gen day1.ndjson.gz --lines 100000 --seed 1        # a made-up log
norn run report day1.ndjson.gz --users users.ndjson --save day1.bin --sample 5
norn run report day2.ndjson.gz --users users.ndjson --against day1.bin --export day2.cbor
norn run bench day1.ndjson.gz                               # how long reading takes
norn test
```

Each line is `{"ts", "level", "user", "msg"}`, a request's msg being
`METHOD PATH STATUS 12ms 512b`. Users are JSON Lines `{"id", "name", "plan"}`,
joined to the requests on user = id. A saved report is `norn.bin` (Norn to
Norn, everything kept); an export is CBOR with string keys, for other
programs.

Written to try `norn.compress` (as a stream), `norn.ndjson`, `norn.regex`'s
named groups, `norn.set`'s relations, `norn.data/diff`, `norn.format`,
`norn.random`, `norn.math`'s percentiles, `norn.bin`, `norn.cbor`, `norn.walk`
and `norn.bench`.
