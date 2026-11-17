# loglens

Reports on request logs: JSON Lines, gzip'd or not, read a line at a time.
By route (count, p50 and p95 latency, errors) and by the users' plan; a
random sample; how today compares with a saved report.

```sh
skein run gen day1.ndjson.gz --lines 100000 --seed 1        # a made-up log
skein run report day1.ndjson.gz --users users.ndjson --save day1.bin --sample 5
skein run report day2.ndjson.gz --users users.ndjson --against day1.bin --export day2.cbor
skein run bench day1.ndjson.gz                               # how long reading takes
skein test
```

Each line is `{"ts", "level", "user", "msg"}`, a request's msg being
`METHOD PATH STATUS 12ms 512b`. Users are JSON Lines `{"id", "name", "plan"}`,
joined to the requests on user = id. A saved report is `skein.bin` (Skein to
Skein, everything kept); an export is CBOR with string keys, for other
programs.

Written to try `skein.compress` (as a stream), `skein.ndjson`, `skein.regex`'s
named groups, `skein.set`'s relations, `skein.data/diff`, `skein.format`,
`skein.random`, `skein.math`'s percentiles, `skein.bin`, `skein.cbor`, `skein.walk`
and `skein.bench`.
