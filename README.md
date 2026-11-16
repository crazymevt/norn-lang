# pulse

Watches web sites and keeps a record of whether they're up: a checker per
site, a status page, a JSON API, and a command line.

```sh
norn run add example https://example.com --every 30   # watch a site
norn run list                                          # the sites
norn run serve --port 8080                             # check them; page at http://127.0.0.1:8080/
norn run report --hours 24                             # uptime (status 1 if one is down)
norn test                                              # its tests
norn package                                           # a command-line archive in dist/
```

The database is `pulse.db` (`--db FILE`, or `PULSE_DB`). `serve -v` logs
each request. Checks older than 30 days are pruned at 03:00 UTC.

The API:

```
GET    /api/targets                 every target, with its last day
POST   /api/targets                 {"name", "url", "every_s"}: watch one
GET    /api/targets/NAME/checks     its checks, newest first (?limit=)
POST   /api/targets/NAME/check      check it now
DELETE /api/targets/NAME            stop watching it
```
