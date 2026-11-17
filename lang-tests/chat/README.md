# chat

A chat server over TCP or TLS, and a client for it: names, rooms with a
little history, private messages, a cap on connections, a rate limit per
client, and a slow client let go rather than holding up the room.

```sh
skein run serve -p 6667                                          # serve
skein run serve -p 6697 --cert server.pem --key server.key       # over TLS
skein run talk 127.0.0.1 6667                                    # talk: /nick ada, /join #lab, …
skein test                                                       # its tests (200 clients at once among them)
```

Commands: `/nick NAME`, `/join #ROOM`, `/part`, `/rooms`, `/who`,
`/msg NAME text`, `/quit`; anything else is said to the room.

Names, rooms and history are refs (`skein.stm`), so taking a name or leaving
happens all at once; a semaphore caps connections and a token bucket rates
each client (`skein.sync`). Each connection has a reader fiber and a writer
draining a bounded outbox; a client whose outbox fills is disconnected.

On one machine, client and server in one process, 200 clients each saying
50 things deliver 2,000,000 lines in about 14 s (both tiers).
