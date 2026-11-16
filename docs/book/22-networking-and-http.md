# 22. Networking and HTTP

[Contents](README.md) · Previous: [21. The operating system](21-the-operating-system.md) · Next: [23. Databases](23-databases.md) <!-- nav -->

This chapter covers talking to other machines. It starts with `norn.net`,
which opens TCP connections, UDP sockets, and TLS, and then moves up to
`norn.http`, an HTTP/1.1 client and server, and `norn.uri`, which parses
and builds the URLs that HTTP uses. Along the way you'll build a small JSON
service and its client.

Every example in this chapter runs on your own machine. Each one starts a
server on the loopback address, `127.0.0.1`, on a port the operating system
picks, talks to it, and stops it. Only the last section reaches the
internet.

Two ideas from earlier chapters carry most of the weight here. A socket is
a stream, so the `norn.io` functions from
[Chapter 19](19-files-and-streams.md) read and write it. And network
operations park the fiber that makes them, never the thread underneath, so
a server can give every connection a fiber of its own
([Chapter 15](15-fibers-and-scopes.md)) and serve thousands at once.

## TCP

TCP gives you a two-way stream of bytes between two programs. One side
listens on a port and accepts connections. The other side connects to it.

```clojure
(require '[norn.net :as net] '[norn.io :as io] '[norn.string :as str])

(def listener (net/listen "127.0.0.1" 0))

(def port (net/port (net/local-address listener)))
(pos? port) ; => true
```

Port 0 asks the operating system for any free port. `local-address` tells
you which one you got, as an `"ip:port"` string, and `port` takes the
number out of it. Listening on `"127.0.0.1"` means only programs on this
machine can connect. Use `"0.0.0.0"` to accept connections from other
machines.

A connection is a stream like any other. Here is a handler for a small
line-based protocol: it reads lines and answers each one in upper case
until the client stops sending.

```clojure
(defn shout-back [conn]
  (let [lines (io/line-reader conn)]
    (loop []
      (when-let [line (io/read-line lines)]
        (io/write conn (str (str/upper-case line) "\n"))
        (recur))))
  (io/close conn))
```

`accept` waits for the next connection and returns it. A server accepts in
a loop, and gives each connection its own fiber so that a slow client
doesn't hold up the others:

```clojure
(def acceptor
  (spawn-detached
    (loop []
      (when-let [conn (try (net/accept listener) (catch Error _ nil))]
        (spawn-detached (shout-back conn))
        (recur)))))
```

The `try` ends the loop when the listener closes: `accept` on a closed
listener signals a `ConnectionError` with `:reason :closed`. That's how
you'll stop this server later. Now connect as a client:

```clojure
(def conn (net/connect "127.0.0.1" port))
(def replies (io/line-reader conn))

(io/write conn "hello\nworld\n")
[(io/read-line replies) (io/read-line replies)] ; => ["HELLO" "WORLD"]
```

`connect` takes a host name or address and a port. A name such as
`"localhost"` is looked up first, and `connect` tries each address it finds
until one works.

When the client has nothing more to send, it can say so without closing
its side for reading. `shutdown-output` sends the end of the stream. The
server's `read-line` returns nil, its loop ends, and it closes the
connection, which the client sees as the end of its own input:

```clojure
(net/shutdown-output conn)
(io/read-line replies) ; => nil
(io/close conn)
```

Each connection has its own fiber, so many clients can talk at once.
Here three clients run in parallel, each in a fiber, and the main fiber
waits for their answers:

```clojure
(defn ask [text]
  (let [c (net/connect "localhost" port)]
    (io/write c (str text "\n"))
    (net/shutdown-output c)
    (let [answer (bytes->string (io/read-all c))]
      (io/close c)
      answer)))

(with-scope [s]
  (mapv await (mapv (fn [i] (spawn s (ask (str "client " i)))) [1 2 3])))
; => ["CLIENT 1\n" "CLIENT 2\n" "CLIENT 3\n"]
```

Closing the listener stops new connections. Connections already accepted
carry on until they finish.

```clojure
(io/close listener)
```

### When connecting fails

A failed connection signals a `ConnectionError`, which is a kind of
`IOError`. Its data says why. Here nothing listens on the port any more:

```clojure
(def closed-port
  (let [l (net/listen "127.0.0.1" 0)
        p (net/port (net/local-address l))]
    (io/close l)
    p))

(net/connect "127.0.0.1" closed-port) ; !! IOError

(:reason (ex-data (handler-case (net/connect "127.0.0.1" closed-port)
                    (IOError [e] e))))
; => :refused
```

A host name that doesn't resolve is a `ConnectionError` too. `resolve`
looks a name up without connecting, and returns its addresses:

```clojure
(every? string? (net/resolve "localhost" 80)) ; => true
(net/resolve "no-such-host.invalid") ; !! ConnectionError
```

Name lookups run on Norn's blocking pool, so a slow DNS server parks only
the fiber that asked.

## UDP

UDP sends single messages, called datagrams, with no connection and no
promise of delivery. `udp` opens a socket bound to an address, `send-to`
sends one datagram to a host and port, and `receive` waits for the next one
and says where it came from.

```clojure
(def a (net/udp "127.0.0.1" 0))
(def b (net/udp "127.0.0.1" 0))

(net/send-to a "ping" "127.0.0.1" (net/port (net/local-address b)))

(let [{:keys [data from]} (net/receive b)]
  [(bytes->string data) (= from (net/local-address a))])
; => ["ping" true]

(io/close a)
(io/close b)
```

The data arrives as bytes. `receive` reads up to 65536 bytes by default.
Pass a size as its second argument to read less.

## TLS

TLS encrypts a TCP connection and lets the client check who it's talking
to. In `norn.net`, TLS is an option on `connect` and `accept`. A client
trusts the operating system's root certificates, which is what you want
for public servers. For your own servers, you can name a certificate file
to trust with `:ca-file`.

A TLS server needs a certificate and its private key. For this example,
make a self-signed certificate for `localhost` with the `openssl` command,
run through `norn.process` ([Chapter 21](21-the-operating-system.md)):

```clojure
(require '[norn.process :as proc])

(:exit (proc/sh "openssl" "req" "-x509" "-newkey" "rsa:2048" "-nodes"
                "-keyout" "key.pem" "-out" "cert.pem" "-days" "1"
                "-subj" "/CN=localhost"
                "-addext" "subjectAltName=DNS:localhost"
                "-addext" "basicConstraints=critical,CA:FALSE"))
; => 0
```

That writes `cert.pem` and `key.pem` to the current directory. Now a
TLS server accepts with `:tls true` and the two files, and a client
connects with `:tls true` and trusts the certificate with `:ca-file`:

```clojure
(def tls-listener (net/listen "127.0.0.1" 0))
(def tls-port (net/port (net/local-address tls-listener)))

(spawn-detached
  (loop []
    (when-let [c (try (net/accept tls-listener :tls true
                                  :cert-file "cert.pem" :key-file "key.pem")
                      (catch Error _ nil))]
      (spawn-detached
        (try (io/write c (str "you said: " (io/read-line (io/line-reader c)) "\n"))
             (catch Error _ nil)
             (finally (io/close c))))
      (recur))))

(let [c (net/connect "localhost" tls-port :tls true :ca-file "cert.pem")]
  (io/write c "hello\n")
  (let [answer (io/read-line (io/line-reader c))]
    (io/close c)
    answer))
; => "you said: hello"
```

The client connects to `"localhost"` because that's the name in the
certificate. The client checks that name, so connecting to `"127.0.0.1"`
would fail. `:server-name` checks a different name than the one you
connect to.

After the handshake, the connection is a stream as before. The client does
the handshake inside `connect`, so a server it doesn't trust fails right
there. Without `:ca-file`, the client trusts only the system's roots, and
this self-signed certificate isn't one of them:

```clojure
(net/connect "localhost" tls-port :tls true) ; !! TlsError

(io/close tls-listener)
```

`TlsError` is a kind of `ConnectionError`, so code that handles connection
failures handles this one too.

## URIs

Before HTTP, a short look at the addresses it uses. `norn.uri` parses a URI
into a map of its parts, as they are written:

```clojure
(require '[norn.uri :as uri])

(uri/parse "https://ann@example.com:8443/a/b?x=1&y=2#top")
; => {:scheme "https" :user-info "ann" :host "example.com" :port 8443
;     :path "/a/b" :query "x=1&y=2" :fragment "top"}
```

A part the URI doesn't have is absent from the map, except `:path`, which
is always there and may be `""`. `format` puts a map back together, so you
can parse a URI, change a part with `assoc`, and format it again.
`port-of` gives the port, falling back to the scheme's default:

```clojure
(uri/format (assoc (uri/parse "http://example.com/a?x=1") :path "/b"))
; => "http://example.com/b?x=1"
(uri/port-of (uri/parse "https://example.com/")) ; => 443
```

The parts stay percent-encoded. Query strings have their own helpers.
`query-params` decodes one into a map, and a name given more than once
gets a vector of its values. `query-pairs` keeps every pair in order.
`query-string` goes the other way and does the encoding for you:

```clojure
(uri/query-params "q=norn+lisp&tag=a&tag=b&flag")
; => {"q" "norn lisp" "tag" ["a" "b"] "flag" ""}

(uri/query-string [["q" "a & b"] ["tag" ["x" "y"]]])
; => "q=a%20%26%20b&tag=x&tag=y"
```

`encode` and `decode` percent-encode and decode a single piece of text.
`encode` leaves only letters, digits, and `- . _ ~` alone, unless you name
more characters to keep, such as `/` in a path:

```clojure
(uri/encode "my file.txt") ; => "my%20file.txt"
(uri/encode "docs/my file.txt" "/") ; => "docs/my%20file.txt"
(uri/decode "caf%C3%A9") ; => "café"
(uri/decode "%zz") ; !! ParseError
```

`resolve` turns a relative reference into an absolute URI, the way a
browser follows a link:

```clojure
(uri/resolve "http://example.com/docs/guide/intro.html" "../api/index.html")
; => "http://example.com/docs/api/index.html"
(uri/resolve "http://example.com/docs/guide/intro.html" "/login?next=1")
; => "http://example.com/login?next=1"
```

## An HTTP server

HTTP is text over TCP. A client writes a request, and the server writes a
response. `norn.http` lets you work with both as maps. A server is a
function from a request map to a response map, which `serve` runs for every
request:

```clojure
(require '[norn.http :as http])

(defn hello [req]
  {:status 200
   :body (str "hello " (get (:query-params req) "name" "world"))})

(def server (http/serve hello))
(def base (str "http://127.0.0.1:" (:port server)))
```

`serve` listens on `127.0.0.1` and a free port unless you pass `:host` and
`:port`. It returns a map with the `:port` and `:address`, and it keeps
serving until you call `stop!`. Each connection runs in a fiber of its own.

To see that HTTP really is text, talk to the server over a plain TCP
connection:

```clojure
(let [c (net/connect "127.0.0.1" (:port server))]
  (io/write c "GET /?name=tcp HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
  (let [text (bytes->string (io/read-all c))]
    (io/close c)
    [(first (str/split-lines text)) (last (str/split-lines text))]))
; => ["HTTP/1.1 200 OK" "hello tcp"]
```

In between are the response headers that `serve` added: the content type
and the body's length.

### Requests

The handler's request map has the request's `:method` as a keyword, its
`:path`, the raw `:query` string and the decoded `:query-params`, the
`:headers`, the `:body`, and some details of the connection. Here is a
handler that keeps the last request it saw so you can look at one:

```clojure
(def last-request (atom nil))

(def peek-server
  (http/serve (fn [req] (reset! last-request req) {:status 204})))

(http/get (str "http://127.0.0.1:" (:port peek-server) "/greet")
          :query-params {"name" "Ada Lovelace"})

(select-keys @last-request [:method :path :query :query-params :body :scheme])
; => {:method :get :path "/greet" :query "name=Ada%20Lovelace"
;     :query-params {"name" "Ada Lovelace"} :body nil :scheme :http}

(get (:headers @last-request) "user-agent") ; => "norn"

(http/stop! peek-server)
```

Header names are always lower case, in requests and in responses, so you
never have to guess how a client spelled `Content-Type`. A header that
appears more than once has its values joined with `", "`. The `:body` is
bytes, or nil when the request has none. `body-text` decodes it as UTF-8
text, and gives `""` for no body. `:remote-addr` is the client's address.

The server reads the whole request body before it calls the handler, up to
16 MB unless you pass `:max-body`. A larger body gets a 413 response
without reaching your handler.

### Responses

A response map has a `:status`, `:headers`, and a `:body`. The status
defaults to 200. A string body is sent as UTF-8 text with a `text/plain`
content type unless you give one. A body can also be bytes, nil, a
sequence of strings or bytes, or a stream. The last two are sent as they
are produced, as the section on streaming shows.

A handler can also return a plain string, which becomes a 200 response
with that text.

When a handler signals an error, the client gets a 500 response, and the
server calls its `:on-error` function with the error and the request. The
default prints a line about it to standard error. Pass your own to log
errors properly:

```clojure
(def failures (atom []))

(def fragile
  (http/serve (fn [req] (error "kaboom"))
              :on-error (fn [e req] (swap! failures conj [(:path req) (condition-message e)]))))

(:status (http/get (str "http://127.0.0.1:" (:port fragile) "/boom"))) ; => 500
@failures ; => [["/boom" "kaboom"]]

(http/stop! fragile)
```

The failure stays with that one request. The server keeps running, and the
connection's other requests are unaffected.

## The HTTP client

You've already used the client. `http/get` sends a GET request and returns
the response as a map:

```clojure
(def r (http/get (str base "/") :query-params {"name" "client"}))

(select-keys r [:status :reason :body]) ; => {:status 200 :reason "OK" :body "hello client"}
(get (:headers r) "content-type") ; => "text/plain; charset=utf-8"
(:url r) ; the URL that answered, with the query added
```

`:query-params` adds encoded parameters to the URL. The body comes back as
text unless you ask for `:as :bytes`. `head`, `post`, `put`, and `delete`
work the same way, and `post` and `put` take the body as their second
argument. All of them call `request`, which takes one map and accepts every
option:

```clojure
(:body (http/request {:method :get :url (str base "/")
                      :headers {"Accept" "text/plain"}
                      :query-params {"name" "map"}}))
; => "hello map"
```

A status that isn't 2xx is still a response, not an error. The server
answered, and you decide what a 404 means to your program:

```clojure
(def missing (http/serve (fn [req] {:status 404 :body "no such page"})))

((juxt :status :reason :body) (http/get (str "http://127.0.0.1:" (:port missing) "/x")))
; => [404 "Not Found" "no such page"]
```

What does signal an error is an exchange that never completes. A
connection that fails is a `ConnectionError`. A malformed response is a
`ParseError`. And a request that takes longer than its `:timeout` in
milliseconds is a `TimeoutError`:

```clojure
(def slow (http/serve (fn [req] (sleep 1000) "late")))

(http/get (str "http://127.0.0.1:" (:port slow) "/") :timeout 100) ; !! TimeoutError

(http/stop! slow)
```

The timeout covers the whole exchange, including any redirects. Because
these are separate condition classes, a program can retry a timeout, give
up on a refused connection, and report a parse error, each with its own
`handler-case` clause ([Chapter 11](11-conditions-and-restarts.md)).

### Redirects and connections

The client follows redirects, up to five by default. `:url` in the result
says where the answer finally came from. Pass `:follow-redirects false` to
see the redirect itself:

```clojure
(def moved
  (http/serve (fn [req]
                (case (:path req)
                  "/old" {:status 301 :headers {"Location" "/new"}}
                  "/new" "you found it"))))

(def moved-base (str "http://127.0.0.1:" (:port moved)))

(:body (http/get (str moved-base "/old"))) ; => "you found it"
(:status (http/get (str moved-base "/old") :follow-redirects false)) ; => 301
```

As browsers do, the client turns a POST into a GET when it follows a 303,
or a 301 or 302.

The client keeps connections open and reuses them for later requests to
the same scheme, host, and port. That saves a TCP handshake, and a TLS
handshake too, on every request. You can see it from the server's side,
because every request on one connection has the same `:remote-addr`:

```clojure
(def callers (atom #{}))
(def counter (http/serve (fn [req] (swap! callers conj (:remote-addr req)) "ok")))

(http/close-idle!)
(dotimes [_ 5] (http/get (str "http://127.0.0.1:" (:port counter) "/")))
(count @callers) ; => 1

(http/stop! counter)
(http/stop! moved)
(http/stop! missing)
```

`close-idle!` closes the connections the client is keeping, which the
example does first so that it starts fresh. Pass `:keep-alive false` to
close a request's connection when it's done.

## A JSON service

Now put the server and client together. The service keeps notes in an
atom and speaks JSON. It has three routes: list the notes, add one, and
fetch one by id.

Start with the data:

```clojure
(require '[norn.json :as json])

(def notes (atom {:next 1 :items {}}))

(defn add-note! [text]
  (let [id (:next @notes)]
    (swap! notes (fn [n] (-> n (update :next inc) (assoc-in [:items id] text))))
    id))

(defn note-list []
  (mapv (fn [[id text]] {:id id :text text}) (sort (:items @notes))))
```

(A real service would allocate the id inside the `swap!` so two requests
can't get the same one. [Chapter 17](17-shared-state.md) shows how.)

Routing is a question of method and path, and `match` answers it well.
Split the path into its segments and match on the method and the segments
together:

```clojure
(defn segments [path] (vec (remove empty? (str/split path #"/"))))

(segments "/notes/3") ; => ["notes" "3"]

(defn routes [req]
  (match [(:method req) (segments (:path req))]
    [:get ["notes"]] {:status 200 :json (note-list)}
    [:post ["notes"]] {:status 201 :json {:id (add-note! (:text (:json req)))}}
    [:get ["notes" id]]
    (if-let [text (get-in @notes [:items (parse-long id)])]
      {:status 200 :json {:id (parse-long id) :text text}}
      {:status 404 :json {:error "no such note"}})
    _ {:status 404 :json {:error "not found"}}))
```

The routes read `:json` from the request and put `:json` in the response,
which HTTP knows nothing about. Middleware closes the gap. In Norn, as in
Clojure's Ring, middleware is a function that takes a handler and returns a
new handler that does something before and after it:

```clojure
(defn wrap-json [handler]
  (fn [req]
    (let [req (if (:body req)
                (assoc req :json (json/read-string (http/body-text req)))
                req)
          resp (handler req)]
      (if (contains? resp :json)
        (-> resp
            (dissoc :json)
            (assoc :body (json/write-string (:json resp)))
            (assoc-in [:headers "content-type"] "application/json"))
        resp))))
```

Because handlers and middleware are plain functions, you can call them
without a server at all, which makes them easy to test:

```clojure
((wrap-json routes) {:method :get :path "/nowhere"})
; => {:status 404 :body "{\"error\":\"not found\"}"
;     :headers {"content-type" "application/json"}}
```

Middleware stacks with `->`. Here is a second one that logs each request,
so the full stack reads from the inside out:

```clojure
(def request-log (atom []))

(defn wrap-log [handler]
  (fn [req]
    (let [resp (handler req)]
      (swap! request-log conj [(:method req) (:path req) (:status resp)])
      resp)))

(def notes-server (http/serve (-> routes wrap-json wrap-log)))
(def notes-url (str "http://127.0.0.1:" (:port notes-server) "/notes"))
```

The client side is a small function that sends JSON and decodes the
answer:

```clojure
(defn call [method url & [body]]
  (let [resp (http/request (cond-> {:method method :url url}
                             body (assoc :body (json/write-string body)
                                         :headers {"content-type" "application/json"})))]
    [(:status resp) (json/read-string (:body resp))]))

(call :post notes-url {:text "buy milk"}) ; => [201 {:id 1}]
(call :post notes-url {:text "call Ada"}) ; => [201 {:id 2}]
(call :get (str notes-url "/2")) ; => [200 {:id 2 :text "call Ada"}]
(call :get (str notes-url "/9")) ; => [404 {:error "no such note"}]
(call :get notes-url)
; => [200 [{:id 1 :text "buy milk"} {:id 2 :text "call Ada"}]]

@request-log
; => [[:post "/notes" 201] [:post "/notes" 201] [:get "/notes/2" 200]
;     [:get "/notes/9" 404] [:get "/notes" 200]]

(http/stop! notes-server)
```

`json/read-string` turns object keys into keywords by default, which is
why the client sees `{:id 1}`. [Chapter 20](20-data-formats.md) covers its
options.

## Streaming

So far every body has been read or written whole. That's fine for small
messages. For a large file, a long log, or results that arrive over time,
you want to send and read the body in pieces.

### Streaming a response

A response body that is a sequence is sent piece by piece as the sequence
produces it, using HTTP's chunked encoding. To produce the pieces over
time, make the sequence lazy. Norn's sequence functions are eager by
default ([Chapter 6](06-sequences-and-transducers.md)), so `(lazy ...)`
marks where laziness starts:

```clojure
(defn ticks [req]
  {:status 200
   :body (map (fn [i] (sleep 200) (str "tick " i "\n")) (lazy (range 5)))})

(def tick-server (http/serve ticks))
(def tick-url (str "http://127.0.0.1:" (:port tick-server) "/"))
```

Each tick takes 200 ms, so the whole body takes a second. A body that is a
`norn.io` stream, such as an open file, is sent the same way.

### Reading a response as it arrives

On the client side, `:as :stream` makes the response body a stream that
you read as the data arrives. `with-response` sends the request this way
and closes the body when its form ends, however it ends:

```clojure
(require '[norn.time :as t])

(def started (t/monotonic-nanos))

(http/with-response [resp {:url tick-url}]
  (let [lines (io/line-reader (:body resp))
        first-line (io/read-line lines)
        ms (quot (- (t/monotonic-nanos) started) 1000000)]
    [first-line (< ms 700)]))
; => ["tick 0" true]
```

The first line arrives well before the server has produced the last one.
Any `norn.io` stream function works on the body: `read`, `read-line`,
`copy-stream!` to save it to a file, or `text-reader` to decode it.

If you read the body to its end, the client returns the connection to its
pool. If you close it early, as this example does, the client closes the
connection, because the rest of the body is still on its way. The
`:timeout` option covers a streamed request only until the response
headers arrive, so a long download isn't cut off partway.

```clojure
(http/stop! tick-server)
```

### Streaming a request

A request body can be a stream or a sequence as well, and it is sent
chunked. That lets you upload a file without reading it into memory first:

```clojure
(def size-server
  (http/serve (fn [req] (str (count (:body req)) " bytes"))))

(io/write-text "big.txt" (apply str (repeat 10000 "0123456789\n")))

(io/with-open [f (io/open "big.txt" :read)]
  (:body (http/post (str "http://127.0.0.1:" (:port size-server) "/") f)))
; => "110000 bytes"

(http/stop! size-server)
```

On the server, the request body still arrives whole, so the handler sees
bytes. A streamed response body can even be the `:body` of another
request, which turns a Norn program into a proxy that never holds the
whole body.

## Forms and file uploads

HTML forms send their fields in one of two encodings. A plain form sends
`name=value` pairs, the same encoding as a query string, and `form-params`
decodes it on the server:

```clojure
(def form-server (http/serve (fn [req] (pr-str (http/form-params req)))))
(def form-url (str "http://127.0.0.1:" (:port form-server) "/"))

(read-string
  (:body (http/post form-url "title=Holiday&place=the+sea"
                    :headers {"content-type" "application/x-www-form-urlencoded"})))
; => {"title" "Holiday" "place" "the sea"}

(http/stop! form-server)
```

A form with a file upload uses `multipart/form-data` instead. The client's
`:multipart` option sends one. Each part has a `:name` and either a
`:value`, which can be a string, bytes, or a stream, or a `:file` path.
`:filename` makes a part a file upload, and `:content-type` sets its type.

On the server, `multipart-params` returns the parts in order. Each part has
`:name`, `:content-type`, `:headers`, and its `:body` as bytes. A file part
also has its `:filename`, and a part that isn't a file has its text as
`:value`.

```clojure
(def upload-server
  (http/serve
    (fn [req]
      (pr-str (mapv (fn [part]
                      (cond-> (select-keys part [:name :filename :value])
                        (:filename part) (assoc :size (count (:body part)))))
                    (http/multipart-params req))))))

(io/write-text "notes.txt" "some notes\n")

(read-string
  (:body (http/post (str "http://127.0.0.1:" (:port upload-server) "/") nil
                    :multipart [{:name "title" :value "Holiday"}
                                {:name "notes" :file "notes.txt" :content-type "text/plain"}])))
; => [{:name "title" :value "Holiday"} {:name "notes" :filename "notes.txt" :size 11}]

(http/stop! upload-server)
```

A `:file` part takes its file name from the path. When every part is a
string or bytes, the client sends the body with a length. A file or stream
part makes it stream the whole body instead, so a large upload is never
held in memory on the sending side.

## Over the internet

Everything above works the same against real servers. An `https://` URL
uses TLS with the operating system's trusted certificates, and the
connection pool reuses TLS connections as well. These examples need a
network connection, so they don't run as part of this chapter:

<!-- skip -->
```clojure
(def page (http/get "https://example.com/" :timeout 10000))
(:status page) ; => 200
(str/includes? (:body page) "Example Domain") ; => true

(http/with-response [r {:url "https://example.com/" :timeout 10000}]
  (io/with-open [out (io/open "example.html" :write)]
    (io/copy-stream! (:body r) out)))
```

Two cautions for programs that talk to the outside world. Always pass a
`:timeout`, because a server that stops answering otherwise holds the
calling fiber forever. And treat a status outside 2xx as data to check,
since the client doesn't signal an error for it.

`norn.http` speaks HTTP/1.1. HTTP/2 and WebSockets aren't built.

## Summary

- Sockets are `norn.io` streams. `norn.net/listen` and `accept` serve TCP,
  `connect` opens it, and port 0 picks a free port. Give each connection a
  fiber.
- UDP sockets send and receive single datagrams with `send-to` and
  `receive`.
- TLS is `:tls true` on `connect` and `accept`. A client trusts the
  system's roots or a `:ca-file`, and an untrusted server fails at
  `connect` with a `TlsError`.
- `norn.uri` parses URIs into maps of parts, builds and decodes query
  strings, percent-encodes, and resolves relative references.
- An HTTP server is a function from a request map to a response map, run by
  `serve`. Middleware is a function from handler to handler.
- The client returns non-2xx responses as data. Failed connections,
  timeouts, and malformed messages are distinct condition classes.
- Sequence and stream bodies are sent chunked. `with-response` reads a
  response as it arrives, and a stream can be a request's body.
- `:multipart` sends forms with files, and `multipart-params` and
  `form-params` read them on the server.

---

[Contents](README.md) · Previous: [21. The operating system](21-the-operating-system.md) · Next: [23. Databases](23-databases.md) <!-- nav -->
