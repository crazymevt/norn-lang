# stash

A key-value server that speaks the Redis protocol (RESP), so `redis-cli`
and Redis client libraries can talk to it: strings and counters, hashes,
lists, sorted sets, keys that expire, publish and subscribe, and snapshots
to a file.

```sh
skein run serve -p 6379 --snapshot stash.bin     # serve; SAVE and stopping write the file
skein run send 127.0.0.1 6379 SET greeting hello  # one command to a server
skein run run tests/fixtures/tour.stash           # a script of commands, each reply printed
skein test                                        # its tests (200 clients at once among them)
```

Commands: `GET SET SETNX GETDEL APPEND STRLEN INCR DECR INCRBY DECRBY MGET
MSET`, `DEL EXISTS TYPE KEYS DBSIZE FLUSHALL EXPIRE PEXPIRE PERSIST TTL
PTTL`, `HSET HGET HDEL HGETALL HKEYS HVALS HLEN HEXISTS HINCRBY`, `LPUSH
RPUSH LPOP RPOP LLEN LRANGE`, `ZADD ZINCRBY ZREM ZSCORE ZCARD ZRANK ZRANGE
ZRANGEBYSCORE`, `SUBSCRIBE UNSUBSCRIBE PUBLISH`, `PING ECHO INFO SAVE
COMMAND QUIT`. `SET` takes `EX`, `PX`, `NX` and `XX`. Values are UTF-8
text.

How it's made:

- **The store** (`stash.store`) spreads its keys over sixteen shards, each
  an atom. A command on one key is one `swap!` of its shard, so it happens
  all at once and commands on other shards don't wait. Each kind of value
  is a class, and `kind` and the reply encoder are generic functions. A
  command is a `defcommand`, which records how many arguments it takes.
- **Expiry**: an expired key is gone to whoever asks from that moment; a
  sorted set of `[expires key]` in each shard lets a sweeper clear them
  away without looking at every key. The clock is the store's own, so
  tests and scripts move time themselves.
- **Connections** (`stash.server`): a reader fiber and a writer draining
  a bounded outbox, so replies keep their order under pipelining and one
  write carries what has gathered. A subscriber too slow to take what's
  published is let go.
- **Snapshots** are `skein.bin` data written with `write-atomic!`, sorted
  by key: two stores holding the same are the same file.
- **What goes wrong** is a condition (`CommandError`, `WrongType`,
  `ProtocolError`), turned into the reply where the connection is served.

What a script prints is fixed (its clock moves only at `@advance`), so
`lang-tests/run.skn` records it, and the snapshot it saves, for every tier.
