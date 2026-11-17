# 23. Databases

[Contents](README.md) · Previous: [22. Networking and HTTP](22-networking-and-http.md) · Next: [24. Command-line programs](24-command-line-programs.md) <!-- nav -->

When a program's data outgrows a file you rewrite whole, reach for a
database. SQLite is the natural first choice: it's a library rather than a
server, a database is one file, and it handles far more data and traffic
than most programs ever see. This chapter covers `skein.sqlite`: opening
connections, running statements and queries, passing parameters, handling
errors, transactions, reading large results a row at a time, looking at the
schema, and sharing a connection between fibers. It ends with a small
inventory application for a bookshop, built up step by step.

The chapter assumes you know some SQL. If you don't, the examples are
simple enough to follow, and SQLite's own documentation is the place to
learn more.

## Connections

`skein.sqlite` uses the SQLite library that comes with the operating system:
`/usr/lib/libsqlite3.dylib` on macOS and `libsqlite3.so.0` on Linux. To use
a different build, set the `SKEIN_SQLITE` environment variable to its path
before the library loads.

```clojure
(require '[skein.sqlite :as db] '[skein.io :as io])

(string? (db/version)) ; => true
```

`version` gives the SQLite library's version, such as `"3.45.1"`. `open`
opens a database file, creating it if it doesn't exist, and returns a
connection. The special name `":memory:"` opens a private database that
lives in memory and disappears when the connection closes, which is ideal
for tests and for the examples in this chapter.

```clojure
(def conn (db/open ":memory:"))
conn ; => #<sqlite ":memory:">
(db/open? conn) ; => true
```

A connection holds an open file and memory inside the C library, so close
it with `close!` when you're done. `with-connection` opens one, runs a
body, and closes it however the body ends, like `with-open` in
[Chapter 19](19-files-and-streams.md):

```clojure
(db/with-connection [c "scratch.db"]
  (db/open? c)) ; => true
(io/exists? "scratch.db") ; => true
```

`open` takes options after the path:

- `:readonly true` opens the database for reading only. Writing is an
  error.
- `:create false` refuses to create a file that isn't there.
- `:busy-timeout ms` says how long to wait when another connection holds a
  lock (5000 milliseconds by default). More on this later.
- `:blocking true` runs SQLite's work on the pool of threads for blocking
  calls instead of on a carrier. The section on fibers explains when that
  helps.

## Statements and queries

There are three ways to run SQL. `execute!` runs one statement that
doesn't return rows, such as `CREATE`, `INSERT`, `UPDATE`, or `DELETE`, and
returns the number of rows it changed, which is 0 for a statement that
changes none, such as `CREATE TABLE`. `exec!` runs a script of several
statements separated by semicolons and returns nil. `query` runs a
statement that returns rows and gives them as a vector.

```clojure
(db/exec! conn "
  CREATE TABLE authors (id INTEGER PRIMARY KEY, name TEXT NOT NULL, born INTEGER);
  INSERT INTO authors (name, born) VALUES ('Ursula K. Le Guin', 1929);
  INSERT INTO authors (name, born) VALUES ('Octavia E. Butler', 1947);
  INSERT INTO authors (name, born) VALUES ('Iain M. Banks', 1954);")

(db/execute! conn "UPDATE authors SET born = born WHERE born < 1950") ; => 2

(db/query conn "SELECT name, born FROM authors ORDER BY born")
; => [{:name "Ursula K. Le Guin" :born 1929}
;     {:name "Octavia E. Butler" :born 1947}
;     {:name "Iain M. Banks" :born 1954}]
```

Each row is a map from column names, as keywords, to values. Because rows
are ordinary maps, everything from [Chapter 5](05-collections.md) and
[Chapter 6](06-sequences-and-transducers.md) works on them directly:

```clojure
(->> (db/query conn "SELECT name, born FROM authors")
     (filter #(> (:born %) 1940))
     (map :name)
     sort)
; => ("Iain M. Banks" "Octavia E. Butler")
```

`query-one` gives the first row, or nil when there are none. It's the
natural call for a lookup by key or an aggregate.

```clojure
(db/query-one conn "SELECT count(*) AS n FROM authors") ; => {:n 3}
(db/query-one conn "SELECT name FROM authors WHERE born > 2000") ; => nil
```

The `AS n` matters. A column's key is the name SQLite gives it, and for an
expression that's the expression's text: without `AS`, the key would be the
keyword `:count(*)`, which you can't write as a keyword literal. Name every
computed column.

To get rows as vectors in column order instead of maps, pass
`{:as :vectors}`. That's handy for tabular output, or when you'll
destructure each row by position.

```clojure
(db/query conn "SELECT name, born FROM authors ORDER BY born LIMIT 2" {:as :vectors})
; => [["Ursula K. Le Guin" 1929] ["Octavia E. Butler" 1947]]
```

`execute!`, `query`, and `query-one` run exactly one statement. SQL with
two statements in it is an error, which closes a door on a class of
injection attacks. Use `exec!` for scripts.

```clojure
(db/query conn "SELECT 1; SELECT 2") ; !! db/DatabaseError
```

### Values

SQLite stores five kinds of value, and each comes back as a Skein type:

| SQLite | Skein |
| --- | --- |
| `INTEGER` | integer |
| `REAL` | float |
| `TEXT` | string |
| `BLOB` | `Bytes` |
| `NULL` | nil |

Going in, the same five types work, plus two conveniences. Booleans are
stored as 1 and 0, as SQLite has no boolean type, so they come back as
integers. Keywords are stored as their names and come back as strings.

```clojure
(db/query-one conn ["SELECT ? AS flag, ? AS status, ? AS data, ? AS missing"
                    true :pending (bytes [1 2]) nil])
; => {:flag 1 :status "pending" :data #bytes "0102" :missing nil}
```

A vector or a map is an error. Store structured
values as text you encode yourself, JSON for instance
([Chapter 20](20-data-formats.md)), or split them into columns and tables.
SQLite integers are 64 bits, so a larger Skein integer can't be stored as
one: it's a `DatabaseError`.
Store money as an integer count of cents, or as decimal text written with
`skein.decimal`, never as a float.

## Parameters

The examples so far put values straight into the SQL text. Don't do that
with values that come from outside the program. A name like `O'Brien`
breaks the statement, and a crafted one can change what it does. Instead,
pass a vector: the SQL first, then its parameters. Each `?` in the SQL takes
the next parameter, and SQLite keeps the values separate from the code.

```clojure
(db/execute! conn ["INSERT INTO authors (name, born) VALUES (?, ?)" "Flann O'Brien" 1911])
; => 1
(db/query-one conn ["SELECT born FROM authors WHERE name = ?" "Flann O'Brien"])
; => {:born 1911}
```

`?1`, `?2`, and so on refer to parameters by position, so one value can be
used twice:

```clojure
(db/query conn ["SELECT name FROM authors WHERE born BETWEEN ?1 AND ?1 + 20 ORDER BY born" 1930])
; => [{:name "Octavia E. Butler"}]
```

With many parameters, names read better than positions. Put a map after
the SQL, and `:name`, `@name`, or `$name` in the SQL takes the value at the
keyword `:name`:

```clojure
(db/query conn ["SELECT name FROM authors WHERE born >= :from AND born < :to ORDER BY born"
                {:from 1900 :to 1950}])
; => [{:name "Flann O'Brien"} {:name "Ursula K. Le Guin"} {:name "Octavia E. Butler"}]
```

A missing parameter or a wrong count is an error before the statement runs,
so a typo can't quietly bind nil:

```clojure
(db/query conn ["SELECT * FROM authors WHERE born = :year" {:yaer 1929}]) ; !! db/DatabaseError
(db/query conn ["SELECT * FROM authors WHERE born = ?" 1929 1947]) ; !! db/DatabaseError
```

Parameters stand for values, never for table or column names. When a name
has to come from data, check it against the names you expect, and quote it
with `quote-name`, which handles quotes inside the name:

```clojure
(db/quote-name "order") ; => "\"order\""
(db/quote-name "my \"odd\" table") ; => "\"my \"\"odd\"\" table\""
```

Statements are prepared once per connection and SQL text, and reused. With
parameters, the text stays the same however the values change, so a query
you run a thousand times is parsed once. That's another reason to prefer
parameters over building SQL strings.

## When a statement fails

Errors from SQLite are `DatabaseError` conditions. Two subclasses mark the
cases a program most often handles: `ConstraintError`, when a statement
would break a `UNIQUE`, `NOT NULL`, `CHECK`, or foreign key constraint, and
`BusyError`, when another connection holds a lock for longer than the busy
timeout. The classes live in `skein.sqlite`, so write them with the alias:
`db/ConstraintError`.

Each error carries SQLite's result `:code`, its more specific
`:extended-code`, and the `:sql` that failed.

```clojure
(db/exec! conn "CREATE UNIQUE INDEX authors_name ON authors (name)")

(handler-case (db/execute! conn ["INSERT INTO authors (name) VALUES (?)" "Iain M. Banks"])
  (db/ConstraintError [e]
    [(:code e) (:extended-code e) (:sql e)]))
; => [19 2067 "INSERT INTO authors (name) VALUES (?)"]
```

Code 19 is SQLite's constraint failure, and 2067 its extended code for a
`UNIQUE` violation. The message says which constraint failed:

```clojure
(handler-case (db/execute! conn ["INSERT INTO authors (name) VALUES (?)" "Iain M. Banks"])
  (db/ConstraintError [e] (condition-message e)))
; => "UNIQUE constraint failed: authors.name (in: INSERT INTO authors (name) VALUES (?))"
```

A common use is "insert, or report a duplicate", which is both simpler and
safer than checking first and inserting second, since another connection
could insert between your check and your insert:

```clojure
(defn add-author! [conn name born]
  (handler-case (do (db/execute! conn ["INSERT INTO authors (name, born) VALUES (?, ?)" name born])
                    :added)
    (db/ConstraintError [_] :exists)))

(add-author! conn "Ted Chiang" 1967) ; => :added
(add-author! conn "Ted Chiang" 1967) ; => :exists
```

Errors in the SQL itself, such as a misspelled column, are plain
`DatabaseError`s. Let those propagate: they're bugs to fix, not conditions
to handle.

```clojure
(db/query conn "SELECT nmae FROM authors") ; !! db/DatabaseError
```

## Transactions

A transaction groups statements so they take effect together or not at
all. Moving stock from one shelf to another is two updates, and a crash
between them must not lose the books. `with-transaction` runs its body in a
transaction. If the body returns, the transaction commits and the form
returns the body's value. If it doesn't, the transaction rolls back. That
covers an error, which then carries on to your handler, and also a warning
or a restart that unwinds past the body, or the fiber being cancelled.

```clojure
(db/exec! conn "
  CREATE TABLE shelves (name TEXT PRIMARY KEY, books INTEGER NOT NULL CHECK (books >= 0));
  INSERT INTO shelves VALUES ('front', 10), ('back', 2);")

(defn move-books! [conn from to n]
  (db/with-transaction [conn]
    (db/execute! conn ["UPDATE shelves SET books = books - ? WHERE name = ?" n from])
    (db/execute! conn ["UPDATE shelves SET books = books + ? WHERE name = ?" n to])
    n))

(defn shelf-counts [conn]
  (db/query conn "SELECT name, books FROM shelves ORDER BY name" {:as :vectors}))

(move-books! conn "front" "back" 4) ; => 4
(shelf-counts conn) ; => [["back" 6] ["front" 6]]
```

Moving more books than a shelf holds breaks the `CHECK` constraint on the
first update. The transaction rolls back, nothing changes, and the
`ConstraintError` reaches the caller:

```clojure
(move-books! conn "back" "front" 50) ; !! db/ConstraintError
(shelf-counts conn) ; => [["back" 6] ["front" 6]]
```

Any error rolls back the transaction, not only a database error, and so
does anything else that leaves the body early. That makes
transactions a good place to enforce rules the schema can't express:
signal an error, and every change made so far in the transaction is undone.

```clojure
(db/with-transaction [conn]
  (db/execute! conn "UPDATE shelves SET books = 0")
  (error "changed my mind"))
; !! Error

(shelf-counts conn) ; => [["back" 6] ["front" 6]]
(db/in-transaction? conn) ; => false
```

`transact` is the function underneath the macro: `(db/transact conn f)`
calls `f` with no arguments in a transaction. Use it when the work is
already a function.

### Nesting and savepoints

A function that uses a transaction may be called from one that already has
one, as `move-books!` might be called while restocking. In Skein, a
transaction inside another becomes a *savepoint*: if the inner one fails,
only its changes are undone, and the outer transaction carries on.

```clojure
(db/with-transaction [conn]
  (move-books! conn "front" "back" 1)
  (handler-case (move-books! conn "front" "back" 99)
    (db/ConstraintError [_] :skipped))
  (db/in-transaction? conn))
; => true

(shelf-counts conn) ; => [["back" 7] ["front" 5]]
```

The first move committed with the outer transaction; the second rolled back
on its own.

### Transaction modes

By default a transaction is *deferred*: it takes SQLite's write lock at its
first write, not when it starts. `:mode :immediate` takes the write lock at
the start, so a transaction that reads and then writes can't be overtaken by
another writer in between. `:mode :exclusive` goes further and, unless the
database uses the write-ahead log described below, keeps other connections
from reading too. The mode is written in the binding, after the connection:

```clojure
(db/with-transaction [conn :mode :immediate]
  (let [{:keys [books]} (db/query-one conn "SELECT books FROM shelves WHERE name = 'back'")]
    (db/execute! conn ["UPDATE shelves SET books = ? WHERE name = 'back'" (* 2 books)])))
; => 1
```

With one connection, as in these examples, the mode makes no difference.
It matters when several connections, in this process or others, write the
same file.

## Large results

`query` builds a vector of every row, which is right for most queries and
wrong for exporting a table of ten million rows. `each-row` calls a
function for each row as SQLite produces it and holds none of them. Rows
arrive as vectors in column order, and `each-row` returns how many there
were.

```clojure
(def seen (atom []))
(db/each-row conn "SELECT name FROM authors ORDER BY name LIMIT 3"
             (fn [[name]] (swap! seen conj name)))
; => 3
@seen ; => ["Flann O'Brien" "Iain M. Banks" "Octavia E. Butler"]
```

The function usually writes each row somewhere: a file, a socket, a
channel. The worked example at the end of the chapter exports a table to
CSV this way.

For showing a result to a person, in a REPL tool or an admin page,
`query-table` gives the column names and rows separately, and stops after
`:limit` rows, saying whether there were more. The columns are named even
when no rows match, which a vector of maps can't tell you.

```clojure
(db/query-table conn "SELECT name, born FROM authors ORDER BY born" :limit 2)
; => {:columns ["name" "born"]
;     :rows [["Flann O'Brien" 1911] ["Ursula K. Le Guin" 1929]]
;     :more? true}
```

After an insert into a table with an `INTEGER PRIMARY KEY`, `last-insert-id`
gives the new row's key:

```clojure
(db/execute! conn ["INSERT INTO authors (name, born) VALUES (?, ?)" "N. K. Jemisin" 1972])
(:name (db/query-one conn ["SELECT name FROM authors WHERE id = ?" (db/last-insert-id conn)]))
; => "N. K. Jemisin"
```

## The schema

A program sometimes needs to look at a database's structure: to show it, to
check it, or to decide what to migrate. `tables` gives the table names,
`schema` gives tables and views, and `columns` describes a table's columns.
SQLite's own internal tables are left out.

```clojure
(db/tables conn) ; => ["authors" "shelves"]
(db/columns conn "shelves")
; => [{:name "name" :type "TEXT" :not-null? false :primary-key? true}
;     {:name "books" :type "INTEGER" :not-null? true :primary-key? false}]
```

### Migrations

A database outlives the version of the program that created it, so the
schema changes over time. A simple and reliable way to manage that uses
SQLite's `user_version`, an integer stored in the database file that SQLite
itself ignores. Keep a vector of migration scripts. The database's
`user_version` says how many have run, and at startup you run the rest,
each in a transaction with the version update, so a failed migration leaves
the database as it was.

```clojure
(defn migrate! [conn migrations]
  (let [{:keys [user_version]} (db/query-one conn "PRAGMA user_version")]
    (doseq [[i script] (map-indexed vector migrations)
            :when (>= i user_version)]
      (db/with-transaction [conn]
        (db/exec! conn script)
        (db/exec! conn (str "PRAGMA user_version = " (inc i)))))
    (:user_version (db/query-one conn "PRAGMA user_version"))))
```

You'll see `migrate!` at work in the next section. Migrations are only
ever added to the end of the vector. Once one has run on a real database,
changing it does nothing for that database, so fix a mistake with a new
migration.

## Fibers and connections

A connection can be shared by many fibers ([Chapter 15](15-fibers-and-scopes.md)).
One fiber at a time uses it; the others wait their turn. A transaction
keeps the connection for its fiber until it ends, so statements from other
fibers never land in the middle of yours.

```clojure
(db/exec! conn "CREATE TABLE visits (page TEXT, n INTEGER)")

(with-scope [s]
  (dotimes [i 20]
    (spawn s (db/with-transaction [conn]
               (db/execute! conn ["INSERT INTO visits VALUES (?, ?)" "home" i])
               (db/execute! conn ["INSERT INTO visits VALUES (?, ?)" "home" (- i)])))))

(db/query-one conn "SELECT count(*) AS rows, sum(n) AS total FROM visits")
; => {:rows 40 :total 0}
```

Each transaction's two rows went in together, so the total is zero however
the fibers interleaved.

SQLite calls run on the fiber's carrier thread, because for most databases
they're quick. A query that reads a lot from disk holds that carrier while
it runs. If your program does heavy database work alongside other fibers
that need to stay responsive, open the connection with `:blocking true`, and
its statements run on the blocking pool instead.

### More than one connection

Several connections, in one process or in several, can open the same
database file. SQLite lets many read at once but only one write at a time.
A connection that wants to write while another holds the write lock waits
up to its busy timeout, then signals `BusyError`.

```clojure
(db/with-connection [a "shared.db"]
  (db/exec! a "CREATE TABLE t (x)")
  (db/with-connection [b "shared.db" :busy-timeout 50]
    (db/with-transaction [a :mode :immediate]
      (db/execute! a "INSERT INTO t VALUES (1)")
      (handler-case (db/execute! b "INSERT INTO t VALUES (2)")
        (db/BusyError [e] :busy)))))
; => :busy
```

Here `b` gave up after 50 milliseconds because `a` was holding its
transaction open. In practice: keep write transactions short, use the
default timeout or a longer one, and treat a `BusyError` as "try again
later". For a file that several processes read while one writes, SQLite's
write-ahead log helps a great deal; turn it on once with
`(db/query-one conn "PRAGMA journal_mode = WAL")`, and the setting stays in
the file.

## A worked example: a bookshop inventory

To finish, here is a small inventory for a bookshop. It keeps books and
sales, refuses to sell stock it doesn't have, reports the best sellers, and
exports the catalog to CSV. It uses most of the chapter, and it's shaped
the way a real one would be: a schema made by migrations, functions that
take the connection as their first argument, and transactions around
anything that changes more than one row.

### The schema

Two tables to start: books, with their price in cents and their stock, and
sales. The second migration, added "later", gives books an author column.

```clojure
(def migrations
  ["CREATE TABLE books (
      id          INTEGER PRIMARY KEY,
      isbn        TEXT NOT NULL UNIQUE,
      title       TEXT NOT NULL,
      price_cents INTEGER NOT NULL CHECK (price_cents >= 0),
      stock       INTEGER NOT NULL DEFAULT 0 CHECK (stock >= 0));
    CREATE TABLE sales (
      id      INTEGER PRIMARY KEY,
      book_id INTEGER NOT NULL REFERENCES books (id),
      qty     INTEGER NOT NULL CHECK (qty > 0),
      sold_at TEXT NOT NULL DEFAULT (datetime('now')));"
   "ALTER TABLE books ADD COLUMN author TEXT;"])

(defn open-shop [path]
  (let [conn (db/open path)]
    (db/exec! conn "PRAGMA foreign_keys = ON")
    (migrate! conn migrations)
    conn))

(def shop (open-shop "shop.db"))
(:user_version (db/query-one shop "PRAGMA user_version")) ; => 2
(mapv :name (db/columns shop "books"))
; => ["id" "isbn" "title" "price_cents" "stock" "author"]
```

SQLite doesn't enforce foreign keys unless asked, once per connection, so
`open-shop` turns them on before anything else. Opening the shop again
runs no migrations, because the database already has both:

```clojure
(db/close! shop)
(def shop (open-shop "shop.db"))
(migrate! shop migrations) ; => 2
```

### Adding and restocking books

Adding a book returns its new id, or signals the `ConstraintError` if the
ISBN is already there. The map of parameters names the columns, which reads
better than a row of question marks.

```clojure
(defn add-book! [conn {:keys [isbn title author price-cents stock] :or {stock 0}}]
  (db/execute! conn ["INSERT INTO books (isbn, title, author, price_cents, stock)
                      VALUES (:isbn, :title, :author, :price, :stock)"
                     {:isbn isbn :title title :author author :price price-cents :stock stock}])
  (db/last-insert-id conn))

(defn restock! [conn isbn n]
  (db/execute! conn ["UPDATE books SET stock = stock + ? WHERE isbn = ?" n isbn]))

(add-book! shop {:isbn "978-0441478125" :title "The Left Hand of Darkness"
                 :author "Ursula K. Le Guin" :price-cents 1799 :stock 4}) ; => 1
(add-book! shop {:isbn "978-0446675505" :title "Parable of the Sower"
                 :author "Octavia E. Butler" :price-cents 1650 :stock 2}) ; => 2
(add-book! shop {:isbn "978-1857231380" :title "Consider Phlebas"
                 :author "Iain M. Banks" :price-cents 1299}) ; => 3

(restock! shop "978-1857231380" 5) ; => 1
(restock! shop "000-0000000000" 5) ; => 0
```

`restock!` returns what `execute!` returns, the number of rows changed: 1
for a known ISBN, 0 for an unknown one. The caller can tell "done" from "no
such book" without a second query.

### Selling

A sale lowers the stock and records the sale, and both must happen or
neither. The stock check uses the same idea as `restock!`: the `UPDATE`
only matches a book with enough stock, so if it changes no rows, the sale
can't go ahead. Signalling an error inside the transaction rolls it back,
and `ex-info` carries the details to whoever handles it
([Chapter 11](11-conditions-and-restarts.md)).

```clojure
(defn sell! [conn isbn qty]
  (db/with-transaction [conn :mode :immediate]
    (let [book (db/query-one conn ["SELECT id, stock FROM books WHERE isbn = ?" isbn])]
      (when-not book
        (throw (ex-info "No such book" {:isbn isbn})))
      (when (zero? (db/execute! conn ["UPDATE books SET stock = stock - :qty
                                       WHERE id = :id AND stock >= :qty"
                                      {:id (:id book) :qty qty}]))
        (throw (ex-info "Not enough stock" {:isbn isbn :wanted qty :have (:stock book)})))
      (db/execute! conn ["INSERT INTO sales (book_id, qty) VALUES (?, ?)" (:id book) qty])
      (db/last-insert-id conn))))

(sell! shop "978-0441478125" 1) ; => 1
(sell! shop "978-0441478125" 2) ; => 2
(sell! shop "978-1857231380" 3) ; => 3
(sell! shop "978-0446675505" 2) ; => 4
```

Selling more than the shelf holds fails, and nothing changes: no sale is
recorded, and the stock stays where it was.

```clojure
(handler-case (sell! shop "978-0446675505" 1)
  (ExInfo [e] [(ex-message e) (ex-data e)]))
; => ["Not enough stock" {:isbn "978-0446675505" :wanted 1 :have 0}]

(:n (db/query-one shop "SELECT count(*) AS n FROM sales")) ; => 4
```

### Reporting

The best sellers are one query. Money stays in integer cents inside the
database and the program, and becomes text only for display:

```clojure
(defn money [cents]
  (str "$" (quot cents 100) "." (if (< (mod cents 100) 10) "0" "") (mod cents 100)))

(money 1799) ; => "$17.99"
(money 5005) ; => "$50.05"

(defn best-sellers [conn n]
  (db/query conn ["SELECT b.title, sum(s.qty) AS sold, sum(s.qty) * b.price_cents AS revenue
                   FROM sales s JOIN books b ON b.id = s.book_id
                   GROUP BY b.id
                   ORDER BY sold DESC, b.title
                   LIMIT ?" n]))

(mapv (juxt :title :sold (comp money :revenue)) (best-sellers shop 2))
; => [["Consider Phlebas" 3 "$38.97"] ["The Left Hand of Darkness" 3 "$53.97"]]
```

Both books sold three copies, so the second sort key, the title, decides
their order. The foreign key keeps the data honest: a sale for a book that
doesn't exist is refused.

```clojure
(db/execute! shop ["INSERT INTO sales (book_id, qty) VALUES (?, ?)" 99 1])
; !! db/ConstraintError
```

### Exporting to CSV

Finally, the catalog as a CSV file for the shop's spreadsheet. `each-row`
streams the rows, and each one is written to the file as it arrives, so the
export uses the same memory for three books or three million.
`skein.csv/write-csv` ([Chapter 20](20-data-formats.md)) quotes any field
that needs it, such as a title with a comma.

```clojure
(require '[skein.csv :as csv])

(defn export-catalog! [conn path]
  (io/with-atomic-file [out path]
    (io/write out (csv/write-csv [["isbn" "title" "author" "price" "stock"]]))
    (db/each-row conn "SELECT isbn, title, author, price_cents, stock FROM books ORDER BY title"
                 (fn [[isbn title author cents stock]]
                   (io/write out (csv/write-csv [[isbn title author (money cents) stock]]))))))

(export-catalog! shop "catalog.csv") ; => 3

(mapv :stock (csv/read-maps (io/read-text "catalog.csv"))) ; => ["2" "0" "1"]
(:title (first (csv/read-maps (io/read-text "catalog.csv")))) ; => "Consider Phlebas"
```

The export goes through `with-atomic-file` ([Chapter 19](19-files-and-streams.md)),
so the spreadsheet never opens a half-written file, and `export-catalog!`
returns the count from `each-row`.

```clojure
(db/close! shop)
(db/close! conn)
```

## Summary

- `open` gives a connection to a database file or `":memory:"`;
  `with-connection` closes it for you. `skein.sqlite` uses the system's
  SQLite, or the one `SKEIN_SQLITE` names.
- `execute!` runs one statement and returns the rows changed; `exec!` runs
  a script; `query` gives rows as maps with keyword keys (or vectors with
  `{:as :vectors}`), and `query-one` the first row or nil. Name computed
  columns with `AS`.
- Pass values as parameters, never in the SQL text: `["… ? …" v]` by
  position, or `["… :name …" {:name v}]` by name. Quote identifiers with
  `quote-name`.
- Failures are `DatabaseError`s with `:code`, `:extended-code`, and `:sql`;
  `ConstraintError` and `BusyError` mark the cases you usually handle.
- `with-transaction` commits when its body returns and rolls back however
  else it ends: an error, a warning or restart unwinding past it, or a
  cancelled fiber. A nested transaction is a savepoint that rolls back alone.
  `:mode :immediate` takes the write lock up front.
- `each-row` streams large results; `query-table` limits and labels a
  result for display; `tables`, `schema`, and `columns` describe the
  database, and `PRAGMA user_version` drives simple migrations.
- Fibers can share a connection, one statement or transaction at a time.
  Connections to the same file wait up to their busy timeout for each
  other's locks.

---

[Contents](README.md) · Previous: [22. Networking and HTTP](22-networking-and-http.md) · Next: [24. Command-line programs](24-command-line-programs.md) <!-- nav -->
