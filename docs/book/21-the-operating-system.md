# 21. The operating system

[Contents](README.md) · Previous: [20. Data formats](20-data-formats.md) · Next: [22. Networking and HTTP](22-networking-and-http.md) <!-- nav -->

A program lives inside an operating system. It has arguments and an
environment, it starts other programs and waits for them, it hears signals,
it reads the clock, and it reports what it's doing. This chapter covers the
libraries for that work: `norn.sys` for the process itself, `norn.process`
for child programs and pseudo-terminals, `norn.signal` for signals,
`norn.time` for instants, dates, and zones, `norn.schedule` for cron-like
schedules, and `norn.log` for leveled, structured logs. The chapter ends
with a small job runner that uses most of them together.

Files and streams have their own chapter, [Chapter 19](19-files-and-streams.md),
and this one leans on it: a child process's pipes are ordinary `norn.io`
streams. Parsing a command line belongs to
[Chapter 24](24-command-line-programs.md).

## The process you are in

`norn.sys` answers questions about the running program and the machine.
Every function reads its value when you call it, not when the library
loads. That matters for images ([Chapter 27](27-images-and-deployment.md)):
an image saved on one machine and restored on another sees the second
machine.

```clojure
(require '[norn.sys :as sys])

(sys/program-args) ; => []
```

`program-args` gives the program's own arguments. For
`norn report.norn --since 2026-01-01`, it is `["--since" "2026-01-01"]`. For
a built image, it is the arguments after the image. For `norn -e` and the
REPL, it is empty, which is what you see above. `args` gives the whole
command line, starting with the path of the `norn` binary, and you rarely
need it.

Environment variables come from `getenv`, which takes a name as a string,
keyword, or symbol and returns a string or nil, and `env`, which returns
all of them as a map of strings.

```clojure
(string? (sys/getenv "HOME")) ; => true
(sys/getenv "NORN_BOOK_SURELY_UNSET") ; => nil
(= (sys/getenv "HOME") (sys/getenv :HOME)) ; => true
(map? (sys/env)) ; => true
(get (sys/env) "HOME") ; the same string as getenv
```

The environment is read-only from inside Norn. To give a child process
different variables, pass them when you start it, as the next section shows.

`info` returns a map of facts about the machine and the process: `:os`,
`:family`, `:arch`, `:endian`, `:cores`, `:page-size`, `:memory`, `:pid`,
`:temp-dir`, `:home-dir`, `:user`, `:hostname`, `:cwd`, `:norn-version`,
`:build`, and `:executable`. Each key also has a function of the same name.

```clojure
(contains? #{:macos :linux :windows} (sys/os)) ; => true
(sys/family) ; :unix on macOS and Linux
(pos? (sys/cores)) ; => true
(= (sys/pid) (:pid (sys/info))) ; => true
(string? (:executable (sys/info))) ; => true
```

`:executable` is the path of the program that is running, which for a
script is the `norn` binary. You'll use it below to run Norn as a child
process. `runtime` reports on Norn's own machinery: how many carrier threads
run fibers, how many threads serve blocking calls, how many handles are
open, the heap size, and the image the process started from, if any.

```clojure
(set (keys (sys/runtime)))
; => #{:carriers :carrier-threads :blocking-threads :handles :heap-bytes :gc-count :image}
```

### Exit status

`sys/exit` ends the process with a status code, 0 by default. A script that
reaches its end exits with 0. A script that ends with an unhandled error
prints the error and exits with 1.

You can't show `exit` in this chapter's examples without ending the
example, so here is a script that exits with the number of arguments it
got. The example writes it to a file and then runs it as a child process,
which also previews `norn.process`.

```clojure
(require '[norn.io :as io] '[norn.process :as proc])

(def norn (:executable (sys/info)))

(io/write-text "count-args.norn"
  "(ns count-args (:require [norn.sys :as sys]))
(println \"got\" (sys/program-args))
(sys/exit (count (sys/program-args)))")

(proc/sh norn "count-args.norn" "a" "b" "c")
; => {:exit 3 :signal nil :out "got [a b c]\n" :err ""}
```

The result map has the exit status, the output the script printed, and its
error output. An unhandled error looks like this from the outside:

```clojure
(io/write-text "fail.norn" "(error \"boom\")")

(let [r (proc/sh norn "fail.norn")]
  [(:exit r) (first (norn.string/split-lines (:err r)))])
; => [1 "Error: fail.norn:1: boom"]
```

[Chapter 24](24-command-line-programs.md) builds on this for programs that
other people run: status 0 for success, 2 for a bad command line, and so on.

## Running other programs

`norn.process` starts child programs. The simplest entry point is `sh`. It
runs a program with arguments, waits for it to end, and returns its exit
status and everything it wrote.

```clojure
(proc/sh "echo" "hello" "world")
; => {:exit 0 :signal nil :out "hello world\n" :err ""}
```

Despite its name, `sh` doesn't use a shell. The first argument is the
program, found on `PATH`, and the rest are its arguments, passed as they
are. Spaces and quotes in an argument reach the program unchanged, so you
don't need to escape anything:

```clojure
(:out (proc/sh "printf" "%s|" "two words" "it's"))
; => "two words|it's|"
```

When you do want a shell, for a pipe or a redirect, run one and give it the
command line with `-c`:

```clojure
(proc/sh "sh" "-c" "echo out; echo err >&2; exit 3")
; => {:exit 3 :signal nil :out "out\n" :err "err\n"}
```

A nonzero exit status is not an error in Norn. The program ran and told you
something, and the result map carries it. Check `:exit` yourself. If the
program can't start at all, `sh` signals an `IOError`, a `FileNotFound` when
the program isn't there:

```clojure
(proc/sh "/no/such/program") ; !! FileNotFound
```

Options can follow the arguments. `:in` sends a string or bytes to the
program's standard input, `:dir` sets its working directory, and `:env` adds
or overrides environment variables. Everything else in the environment is
inherited, unless you pass `:clear-env true`.

```clojure
(:out (proc/sh "tr" "a-z" "A-Z" :in "quiet please\n")) ; => "QUIET PLEASE\n"
(:out (proc/sh "pwd" :dir "/")) ; => "/\n"
(:out (proc/sh "sh" "-c" "printf %s \"$GREETING\"" :env {"GREETING" "hi"}))
; => "hi"
```

Because the options start at the first keyword, an argument of the program
itself has to be a string. Write `"--verbose"`, not `:verbose`.

## Processes that keep running

`sh` collects all the output before it returns. For a program that runs for
a long time, produces a lot of output, or needs to talk back and forth, use
`start`. It returns as soon as the program is running.

```clojure
(def ticker (proc/start "sh" :args ["-c" "echo one; echo two; sleep 30"]))

(set (keys ticker)) ; => #{:process :pid :in :out :err}
```

`:in`, `:out`, and `:err` are streams, the same kind `norn.io` reads and
writes everywhere else. `:out` and `:err` are what the program writes, and
`:in` is what it reads. You can wrap `:out` in a line reader and read it as
it arrives:

```clojure
(def ticker-lines (io/line-reader (:out ticker)))

[(io/read-line ticker-lines) (io/read-line ticker-lines)] ; => ["one" "two"]
```

The program is now sleeping. `kill` sends it a signal, `:term` by default,
and `wait` waits for it to end and returns how it ended. A program that
exits has an `:exit` code. A program that a signal ends has a `:signal`
instead.

```clojure
(proc/kill ticker)
(proc/wait ticker) ; => {:exit nil :signal :term}
```

`kill` takes other signals by name, such as `:int`, `:hup`, or `:kill`, or
by number. `wait` parks only the fiber that calls it. The waiting happens on
Norn's blocking pool, so other fibers keep running while a child process
does its work.

`start` gives you up to four handles: the three streams and the process
itself. `sh` closes its handles for you. With `start`, close them yourself
when you're done. `proc/close` closes all of them; a stream you didn't pipe
is nil, and it skips that. Call it after `wait`, which needs the process
handle.

```clojure
(proc/close ticker)
(:handles (sys/runtime)) ; => 0
```

Closing a handle twice is harmless, so `proc/close` is safe to call after
you've closed one of the streams yourself.

### Choosing where the streams go

`:in`, `:out`, and `:err` each take `:pipe` (the default), `:inherit`, or
`:null`. `:inherit` connects the child to the same place as your program,
so its output appears in your terminal. `:null` discards output and gives
empty input. A stream that isn't piped comes back as nil.

```clojure
(let [p (proc/start "cat" :err :inherit)]
  (io/write (:in p) "one line\n")
  (io/close (:in p))
  (let [out (bytes->string (io/read-all (:out p)))
        ended (proc/wait p)]
    (proc/close p)
    [out (:err p) ended]))
; => ["one line\n" nil {:exit 0 :signal nil}]
```

Closing `:in` is how the child learns that its input has ended. `cat` exits
only after that. Forgetting to close `:in` is the most common reason a
child program seems to hang.

`start` also accepts `:args`, `:dir`, `:env`, and `:clear-env` as `sh`
does. `:clear-env true` starts the child with no environment except what
`:env` gives:

```clojure
(let [p (proc/start "sh" :args ["-c" "echo ${HOME:-no home} $X"]
                    :env {"X" "1"} :clear-env true)
      out (bytes->string (io/read-all (:out p)))]
  (proc/wait p)
  (proc/close p)
  out)
; => "no home 1\n"
```

### Connecting two programs

A shell pipeline such as `printf ... | sort` connects one program's output
to another's input. In Norn you do the connecting. Start both programs,
then copy from one to the other in a fiber of its own. `copy-stream!`
copies until the source ends.

```clojure
(let [produce (proc/start "printf" :args ["pear\napple\nfig\n"])
      sorter (proc/start "sort")]
  (spawn-detached
    (io/copy-stream! (:out produce) (:in sorter))
    (io/close (:in sorter)))
  (let [sorted (bytes->string (io/read-all (:out sorter)))]
    (proc/wait produce)
    (proc/wait sorter)
    (proc/close produce)
    (proc/close sorter)
    sorted))
; => "apple\nfig\npear\n"
```

The copying runs in its own fiber so that the main fiber can read the
sorter's output at the same time. With two fibers, neither program blocks
waiting for the other. Closing the sorter's input after the copy tells
`sort` that its input is complete, so it can write its result.
[Chapter 15](15-fibers-and-scopes.md) explains fibers, and `spawn-detached`
in particular.

### Waiting with a time limit

`wait` waits as long as it takes. To give up after a while, wait in another
fiber that reports to a channel, and race that channel against a `timeout`
with `alts!` ([Chapter 16](16-channels.md)). If the time runs out first,
kill the program and collect how it ended.

```clojure
(defn wait-at-most
  "Wait for p for up to ms milliseconds, then end it with :term."
  [p ms]
  (let [done (chan 1)]
    (spawn-detached (put! done (proc/wait p)))
    (let [[result port] (alts! [done (timeout ms)])]
      (if (identical? port done)
        result
        (do (proc/kill p)
            (take! done))))))

(let [p (proc/start "sleep" :args ["10"] :in :null :out :null :err :null)]
  (wait-at-most p 200))
; => {:exit nil :signal :term}

(let [p (proc/start "sleep" :args ["0"] :in :null :out :null :err :null)]
  (wait-at-most p 5000))
; => {:exit 0 :signal nil}
```

With every stream set to `:null`, only the process handle is left. These
two examples leave it open, which is harmless in a short script. A
long-running program should call `proc/close` here too.

## Programs that want a terminal

Some programs behave differently when they aren't talking to a terminal.
Shells drop their prompts, editors refuse to start, and many tools turn off
color. `start-pty` runs a program on a pseudo-terminal, a terminal that your
program plays the other side of. It accepts `:args`, `:dir`, `:env`, and `:clear-env` as `start` does,
plus `:rows` and `:cols` (24 by 80 unless you say otherwise).

```clojure
(let [p (proc/start-pty "sh" :args ["-c" "[ -t 1 ] && echo terminal; stty size"]
                        :rows 30 :cols 100)
      shown (bytes->string (io/read-all (:out p)))]
  [shown (proc/wait p)])
; => ["terminal\r\n30 100\r\n" {:exit 0 :signal nil}]
```

The program sees a real terminal of 30 rows and 100 columns. There is one
stream in each direction: `:in` is what's typed at the terminal and `:out`
is what the terminal shows, standard output and standard error together.
Terminal conventions apply, so lines end in `\r\n`, and what you type is
echoed back on `:out`:

```clojure
(let [p (proc/start-pty "cat")
      lines (io/line-reader (:out p))]
  (io/write (:in p) "hello\n")
  (let [shown [(io/read-line lines) (io/read-line lines)]]
    (io/write (:in p) (bytes [4]))   ; Ctrl-D: the end of cat's input
    (io/read-all (:out p))
    [shown (proc/wait p)]))
; => [["hello" "hello"] {:exit 0 :signal nil}]
```

The first `"hello"` is the terminal's echo of what you typed, and the
second is `cat` writing it back. Control characters work as they would at a
keyboard: byte 4 is Ctrl-D, which ends input, and byte 3 is Ctrl-C, which
interrupts the program.

Keep reading `:out` while the program runs, as a terminal emulator does. A
program whose output nobody reads waits until someone does, and on macOS
even its exit waits for its last output to be read. When the window you
are emulating changes size, call `(proc/resize p rows cols)`. The program
receives `SIGWINCH` and can ask for its new size.

## Signals

The operating system tells a process about events with signals. Ctrl-C at
a terminal sends `SIGINT`, `kill` sends `SIGTERM` by default, and a
terminal that changes size sends `SIGWINCH`. Most of these end a program
unless it arranges to handle them.

`norn.signal/listen` turns signals into channel messages. It takes signal
names and returns a map whose `:signals` channel receives each signal's
keyword as it arrives. While you listen for a signal, it no longer has its
default effect.

```clojure
(require '[norn.signal :as sig])

(def hup (sig/listen :hup :usr1))

(sig/raise :usr1)
(take! (:signals hup)) ; => :usr1

(proc/sh "kill" "-HUP" (str (sys/pid)))
(first (alts! [(:signals hup) (timeout 2000)])) ; => :hup

(sig/stop hup)
(take! (:signals hup)) ; => nil
```

`raise` sends a signal to your own process, which is handy for trying
things out. The second signal here comes from outside, through the `kill`
command. `stop` ends the listening and closes the channel, so a take from
it returns nil. Once nobody is listening for a signal, it has its default
effect again: after the last `stop`, `:term` ends the process as before.

You can listen for `:int`, `:term`, `:hup`, `:usr1`, `:usr2`, `:winch`,
`:quit`, `:pipe`, `:alrm`, `:cont`, `:tstp`, `:ttin`, `:ttou`, and `:abrt`.
Signals that can't be caught are an error to ask for:

```clojure
(sig/listen :kill) ; !! RuntimeError
```

On Windows, only `:int` is available.

The most common use is a clean shutdown. A server or worker listens for
`:int` and `:term`, and when one arrives it finishes what it's doing instead
of dying in the middle. Because signals arrive on a channel, a worker can
check for them with the same `alts!` it uses for everything else:

```clojure
(def stopping (sig/listen :int :term))

(def worker
  (spawn-detached
    (loop [jobs-done 0]
      (let [[s _] (alts! [(:signals stopping) (timeout 10)])]
        (if s
          {:stopped-by s :did-work (pos? jobs-done)}
          (recur (inc jobs-done)))))))

(sleep 50)
(sig/raise :term)
(await worker) ; => {:stopped-by :term :did-work true}
```

Each pass through the loop stands for one unit of work. When `:term`
arrives, the worker leaves the loop and returns a result instead of being
killed. A real program would flush its files or close its connections at
that point, then exit.

Ctrl-C works the same way in a program the `norn` command runs. Ordinarily
it interrupts the evaluation, as at the REPL. While the program listens for
`:int`, Ctrl-C is the program's to handle, and the `norn` command leaves the
evaluation alone.

## Time

`norn.time` has two families of values. Instants and durations are
points and spans on the time line, the same everywhere in the world.
Calendar values, such as dates, clock times, and zoned date-times, are how
people write those points down in a particular place. Most time bugs come
from mixing the two, so the library keeps them apart.

### Instants and durations

An `Instant` is a point in time, counted in nanoseconds from
1970-01-01T00:00:00Z. You usually get one from `now`, or from `parse`,
which reads RFC 3339 text. `format` writes it back.

```clojure
(require '[norn.time :as t])

(t/instant? (t/now)) ; => true

(def launch (t/parse "2026-11-13T07:05:00Z"))

(t/format launch) ; => "2026-11-13T07:05:00Z"
(t/epoch-ms launch) ; => 1794553500000
```

An instant has no zone. The text `2026-11-13T08:05:00+01:00` names the same
instant as `2026-11-13T07:05:00Z`, written from a place one hour ahead of
UTC, and the two parse to equal values. `format` writes UTC unless you give
it a zone, which can be a named IANA zone or an offset in seconds:

```clojure
(= launch (t/parse "2026-11-13T08:05:00+01:00")) ; => true
(t/format launch "Asia/Tokyo") ; => "2026-11-13T16:05:00+09:00"
(t/format launch -18000) ; => "2026-11-13T02:05:00-05:00"
```

Instants are values. They compare with `=`, and they print as tagged
literals, `#inst "2026-11-13T07:05:00Z"`, which read back as the same
instant ([Chapter 20](20-data-formats.md)); `str` gives their RFC 3339
text. To compare them in time order, use `before?`, `after?`,
and `compare-time`. `compare-time` works as a comparator for `sort`:

```clojure
(def new-year (t/parse "2026-01-01"))

(t/before? new-year launch) ; => true
(mapv t/format (sort t/compare-time [launch new-year]))
; => ["2026-01-01T00:00:00Z" "2026-11-13T07:05:00Z"]
```

A date alone, like `"2026-01-01"`, parses as midnight UTC.

A `Duration` is an amount of time. Make one with `duration` and named
amounts, and read it back in the unit you want. `plus` and `minus` move an
instant by a duration, and `between` measures the duration from one
instant to another.

```clojure
(def ninety-minutes (t/duration :hours 1 :minutes 30))

(t/seconds ninety-minutes) ; => 5400
(t/format (t/plus launch ninety-minutes)) ; => "2026-11-13T08:35:00Z"
(t/format (t/minus launch (t/duration :days 1))) ; => "2026-11-12T07:05:00Z"
(t/millis (t/between launch (t/parse "2026-11-13T07:05:02.5Z"))) ; => 2500
```

`seconds` and `millis` round toward zero, and `nanos` gives the exact
count.

The wall clock that `now` reads can jump, for instance when the machine
synchronizes its clock. To measure how long something takes, use
`monotonic-nanos` instead. It never jumps backward, but its numbers mean
nothing on their own. Only the difference between two readings means
something.

```clojure
(let [start (t/monotonic-nanos)]
  (sleep 20)
  (>= (- (t/monotonic-nanos) start) 20000000)) ; => true
```

### Calendar fields

People want years, months, and hours, and those depend on where you are.
`fields` breaks an instant into calendar fields in a zone, UTC by default:

```clojure
(t/fields launch)
; => {:year 2026 :month 11 :day 13 :hour 7 :minute 5 :second 0 :nanos 0
;     :weekday 5 :yearday 317 :offset 0}

(select-keys (t/fields launch "Asia/Kolkata") [:day :hour :minute :offset])
; => {:day 13 :hour 12 :minute 35 :offset 19800}
```

`:weekday` runs from 1 for Monday to 7 for Sunday, and `:offset` is in
seconds. `from-fields` goes the other way, from fields and an offset to an
instant:

```clojure
(t/format (t/from-fields {:year 2026 :month 11 :day 13 :hour 9 :offset 3600}))
; => "2026-11-13T08:00:00Z"
```

Missing time fields default to zero. A date that doesn't exist, such as
February 30, signals a `TypeError`.

### Dates and clock times without a zone

A birthday, a due date, or "the shop opens at 9:00" isn't an instant. It is
a calendar date or a clock time that means the same thing wherever you
read it. `LocalDate`, `LocalTime`, and `LocalDateTime` hold those values.

```clojure
(def due (t/local-date 2026 1 31))
(def opening (t/local-time 9 0))

(t/format-local due) ; => "2026-01-31"
(t/format-local opening) ; => "09:00:00"
(t/format-local (t/local-date-time due opening)) ; => "2026-01-31T09:00:00"
(= due (t/parse-local-date "2026-01-31")) ; => true
(:month due) ; => 1
```

They are class instances with slots, so `(:month due)` works as it does on
a map. `parse-local-time` reads `HH:MM` and `HH:MM:SS` with optional
fractional seconds, and `parse-local-date-time` reads a date and time
joined by `T` or a space. An impossible value is a `TypeError`, and text
that doesn't make a date or time is a `ParseError`:

```clojure
(t/local-date 2026 2 30) ; !! TypeError
(t/parse-local-date "2026-02-30") ; !! ParseError
```

They print as tagged literals, `#norn/date "2026-01-31"`, `#norn/time` and
`#norn/date-time`, which read back as the same values.

### Patterns

Text from elsewhere rarely says `2026-01-31`. A bank export says
`01/31/2026`, a letter `31 January 2026`. The format and parse functions
take a pattern, as C's `strftime` and `strptime` do: `%Y` is the year, `%m`
the month, `%d` the day, `%b` and `%B` the month's name, short and in full,
`%a` and `%A` the weekday's, `%H`, `%M` and `%S` the time, `%I` and `%p` a
12-hour clock, and `%z` the offset. `%F` is `%Y-%m-%d` and `%T` is
`%H:%M:%S`.

```clojure
(t/format-local due "%d %B %Y") ; => "31 January 2026"
(t/format-local due "%a %m/%d/%y") ; => "Sat 01/31/26"
(t/format-local opening "%I:%M %p") ; => "09:00 AM"
(t/format launch "Asia/Tokyo" "%F %H:%M %z") ; => "2026-11-13 16:05 +0900"

(= due (t/parse-local-date "1/31/2026" "%m/%d/%Y")) ; => true
(= due (t/parse-local-date "31 jan 2026" "%d %b %Y")) ; => true
(t/format-local (t/parse-local-time "2:15 pm" "%I:%M %p")) ; => "14:15:00"
```

Reading is forgiving where it can be: a number may have fewer digits than
`%m` or `%d` writes, case doesn't matter, a month's name may be short or
full, and whitespace matches any whitespace. A weekday must be the date's,
and a two-digit year is 1969 to 2068. Names are English.

```clojure
(t/parse-local-date "Mon, 31 Jan 2026" "%a, %d %b %Y") ; !! ParseError
```

The calendar helpers work on dates, and on date-times through their dates:

```clojure
(t/weekday (t/local-date 2026 11 13)) ; => 5
(t/days-between (t/local-date 2026 1 1) (t/local-date 2026 12 25)) ; => 358
(t/days-in-month 2028 2) ; => 29
(t/day-of-year (t/local-date 2026 12 31)) ; => 365
(t/before? (t/local-date 2026 1 1) (t/local-date 2026 1 2)) ; => true
```

`shift` moves a date along the calendar by `:years`, `:months`, `:weeks`,
and `:days`. Negative amounts move backward. Months are uneven, so when the
same day doesn't exist in the target month, `shift` gives that month's last
day:

```clojure
(t/format-local (t/shift due :months 1)) ; => "2026-02-28"
(t/format-local (t/shift due :days 1)) ; => "2026-02-01"
(t/months-between due (t/local-date 2026 2 28)) ; => 0
(t/months-between due (t/local-date 2026 3 1)) ; => 1
```

`months-between` counts whole months, so January 31 to February 28 is not
yet a month. On a `LocalDateTime`, `shift` also takes `:hours`,
`:minutes`, and smaller units, and they carry into the date:

```clojure
(t/format-local (t/shift (t/parse-local-date-time "2026-12-31T23:30") :minutes 45))
; => "2027-01-01T00:15:00"
```

An instant has no calendar, so `shift` on an instant accepts only `:hours`
and smaller units. Ask for days and you get an error that tells you to put
the instant in a zone first.

### Zones

A zone is the set of rules a place uses to turn instants into clock times,
including when daylight saving time starts and ends. `zone` returns one by
its IANA name. The rules come from the system's zone files, in `$TZDIR` or
`/usr/share/zoneinfo`.

```clojure
(t/zone "Europe/Paris") ; => #<Zone Europe/Paris>
(t/zone-info "Europe/Paris" launch) ; => {:offset 3600 :dst? false :abbreviation "CET"}
(t/zone-name "+05:30") ; => "+05:30"
(t/zone "Mars/Olympus_Mons") ; !! ParseError
```

`zone` also accepts a fixed offset such as `"+05:30"` or `"Z"`, a number of
seconds, `:utc`, and `:local`. `:local` is the machine's zone, which
`system-zone` reads from `$TZ` or `/etc/localtime`. A program whose results
depend on `:local` gives different answers on different machines, so name
the zone whenever the answer matters.

A `ZonedDateTime` is an instant together with its zone and the clock time
there. `in-zone` makes one from an instant, and `zoned` makes one from a
local date-time. `format-zoned` writes RFC 9557 text, which includes the
zone's name so that the value can be read back with `parse-zoned`:

```clojure
(def ny "America/New_York")

(t/format-zoned (t/in-zone launch ny))
; => "2026-11-13T02:05:00-05:00[America/New_York]"

(def standup (t/zoned (t/parse-local-date-time "2026-03-07T09:00") ny))
(t/format-zoned standup) ; => "2026-03-07T09:00:00-05:00[America/New_York]"
(= standup (t/parse-zoned (t/format-zoned standup))) ; => true
(t/format (t/to-instant standup)) ; => "2026-03-07T14:00:00Z"
```

### Daylight saving time

In New York, clocks jump from 2:00 to 3:00 on 2026-03-08 and fall back from
2:00 to 1:00 on 2026-11-01. So 2:30 on March 8 never happens there, and
1:30 on November 1 happens twice. `zoned` has to decide what such a time
means. Its `:resolve` option sets the policy. The policies follow
JavaScript's Temporal:

- `:compatible`, the default, moves a skipped time forward by the length of
  the gap and picks the earlier of a repeated time.
- `:earlier` and `:later` pick that side in both cases.
- `:reject` signals a `TypeError`.

```clojure
(t/format-zoned (t/zoned (t/parse-local-date-time "2026-03-08T02:30") ny))
; => "2026-03-08T03:30:00-04:00[America/New_York]"

(t/format-zoned (t/zoned (t/parse-local-date-time "2026-11-01T01:30") ny :resolve :later))
; => "2026-11-01T01:30:00-05:00[America/New_York]"

(t/zoned (t/parse-local-date-time "2026-11-01T01:30") ny :resolve :reject) ; !! TypeError
```

Shifting a zoned date-time raises the same question, and `shift` answers it
by unit. Days, months, and years keep the clock time, so "the same time
tomorrow" stays at 9:00 across a clock change. Hours and smaller move along
the time line, so an hour is always an hour:

```clojure
[(t/format-zoned (t/shift standup :days 1))
 (t/format-zoned (t/shift standup :hours 24))]
; => ["2026-03-08T09:00:00-04:00[America/New_York]"
;     "2026-03-08T10:00:00-04:00[America/New_York]"]
```

The day after the standup is only 23 hours long in New York, so 24 hours
later is 10:00. `start-of-day` gives the first instant of a date in a zone,
which is usually midnight:

```clojure
(t/format-zoned (t/start-of-day (t/local-date 2026 3 8) ny))
; => "2026-03-08T00:00:00-05:00[America/New_York]"
```

A rule of thumb: store and compare instants, do calendar arithmetic on
local or zoned values, and choose the zone on purpose.

## Schedules

`norn.schedule` runs code at times described by cron expressions. A cron
expression has five fields: minute, hour, day of the month, month, and day
of the week. Each field is `*`, a number, a range such as `9-17`, a list
such as `1,15`, and any of these with a step such as `*/15`. Months and
weekdays also take names such as `jan` and `mon-fri`. `@daily`, `@hourly`,
`@weekly`, `@monthly`, and `@yearly` stand for the usual expressions.

Before running anything, you can ask when an expression is due. `next-run`
gives the first time after an instant, and `next-runs` gives several:

```clojure
(require '[norn.schedule :as sch])

(t/format (sch/next-run "0 3 * * *" launch)) ; => "2026-11-14T03:00:00Z"

(mapv t/format (sch/next-runs "*/15 9-17 * * mon-fri" launch 3))
; => ["2026-11-13T09:00:00Z" "2026-11-13T09:15:00Z" "2026-11-13T09:30:00Z"]

(sch/next-run "0 0 30 2 *" launch) ; => nil
```

The last one asks for February 30, which never comes, so `next-run` returns
nil. It looks five years ahead before it gives up. With one argument,
`next-run` starts from now.

Times are in UTC unless you give `:zone`. "Every Monday at 9:00 in Paris"
is a different instant in winter and summer, and the zone takes care of
that:

```clojure
(mapv #(t/format % "Europe/Paris")
      (sch/next-runs "0 9 * * mon" launch 2 :zone "Europe/Paris"))
; => ["2026-11-16T09:00:00+01:00" "2026-11-23T09:00:00+01:00"]
```

A bad expression is a `ParseError` when you parse it, not when it's due.
`parse` returns the expression as sets of allowed values:

```clojure
(select-keys (sch/parse "*/20 9-11 * * *") [:minute :hour])
; => {:minute #{0 20 40} :hour #{9 10 11}}

(sch/parse "61 * * * *") ; !! ParseError
```

`schedule` runs a function each time an expression is due, in a fiber of
its own, and returns a schedule that `stop!` ends. `runs` counts how many
times it has run.

```clojure
(def heartbeat (sch/schedule "* * * * *" (fn [] (println "still here"))))

(sch/runs heartbeat) ; => 0
(sch/stop! heartbeat)
```

This one would print once a minute, but the example stops it at once.
A run that signals an error is reported on standard error, and the
schedule goes on. Runs never overlap: if a run is still going when the next
one is due, the next one waits for it.

Clock changes matter here too, and `norn.schedule` handles them as Vixie
cron does. A job set for 1:30 in New York runs once on the day 1:30
repeats, the first time. A job set for 2:30 runs at 3:00 on the day 2:30
is skipped, the moment the clock jumps:

```clojure
(mapv #(t/format % ny)
      (sch/next-runs "30 1 * * *" (t/parse "2026-10-31T12:00:00Z") 3 :zone ny))
; => ["2026-11-01T01:30:00-04:00" "2026-11-02T01:30:00-05:00" "2026-11-03T01:30:00-05:00"]

(mapv #(t/format % ny)
      (sch/next-runs "30 2 * * *" (t/parse "2026-03-07T12:00:00Z") 2 :zone ny))
; => ["2026-03-08T03:00:00-04:00" "2026-03-09T02:30:00-04:00"]
```

An expression whose minute or hour is `*` follows the clock instead.
`"0 * * * *"` runs every hour, so it runs at both 1:00s on the day 1:00
repeats, and an hour that's skipped has no run:

```clojure
(mapv #(t/format % ny)
      (sch/next-runs "0 * * * *" (t/parse "2026-11-01T04:30:00Z") 3 :zone ny))
; => ["2026-11-01T01:00:00-04:00" "2026-11-01T01:00:00-05:00" "2026-11-01T02:00:00-05:00"]
```

## Logging

`norn.log` writes leveled, structured log messages. Each message has a
level, a text, and key-value pairs:

```clojure
(require '[norn.log :as log])

(log/info "listening" :port 8080 :host "localhost")
```

By default a message goes to standard error as a line like this:

```text
2026-11-13T07:05:00.123Z INFO  user: listening port=8080 host="localhost"
```

The line has the time, the level, the namespace the message came from, the
text, and the pairs. The levels, lowest first, are `:trace`, `:debug`,
`:info`, `:warn`, and `:error`, and each has a macro of the same name. Only
messages at the current level or above are written. The level starts at
`:info`.

Because `log/debug` and the others are macros, a message below the current
level costs only the level check. Its arguments aren't evaluated, so you can
leave detailed debug messages in hot code.

### Where messages go

Each message becomes an event map, and a sink function receives it. The
default sink writes the line above. `set-sink!` replaces it, which is how
you send logs somewhere else or collect them in a test. Here is a sink that
keeps events in an atom:

```clojure
(def events (atom []))
(log/set-sink! (fn [event] (swap! events conj event)))

(log/info "listening" :port 8080)
(log/debug "connection details" :peer (swap! events conj :evaluated))

(count @events) ; => 1
(dissoc (first @events) :time)
; => {:level :info :ns "user" :msg "listening" :data [[:port 8080]]}
```

The debug message was below the level, so its `:peer` argument never ran.
An event has `:level`, `:ns`, `:msg`, `:data` (the pairs, in order), and
`:time`, an instant. To write JSON lines instead, make the sink build a map
and print it with `norn.json/write-string` (see
[Chapter 20](20-data-formats.md)). `(log/set-sink! nil)` restores the
default.

### Choosing what to see

`set-level!` with one argument sets the level for everything. With a
namespace as well, it sets the level for that namespace and those under it,
so you can turn up one part of a program without drowning in the rest:

```clojure
(log/set-level! :debug 'user)

(log/enabled? :debug "user") ; => true
(log/enabled? :debug "user.db") ; => true
(log/enabled? :debug "other") ; => false
```

`enabled?` asks whether a message at a level from a namespace would be
written. `*level*` is a dynamic variable, so you can also change the level
for one stretch of code, and the fibers it starts, with `binding`. It
applies where no namespace level does:

```clojure
(binding [log/*level* :error]
  (log/enabled? :warn "other")) ; => false
```

`spy` logs a form and its value at `:debug` and returns the value. It's
handy for a quick look in the middle of an expression:

```clojure
(reset! events [])
(+ 1 (log/spy (* 2 3))) ; => 7
(:msg (first @events)) ; => "(* 2 3)"
(:data (first @events)) ; => [[:value 6]]
```

## A small job runner

Here is a sketch that puts the pieces together: a function that runs a
shell command as a job, times it with the monotonic clock, and logs how it
went. Failure is data here. The command's exit status decides the log
level, and the function returns a map that a caller, or a schedule, can
act on.

```clojure
(defn run-job
  "Run shell command cmd as job `name`, log the outcome, and return it."
  [name cmd]
  (log/info "job starting" :job name)
  (let [start (t/monotonic-nanos)
        r (proc/sh "sh" "-c" cmd)
        ms (quot (- (t/monotonic-nanos) start) 1000000)
        outcome {:job name :exit (:exit r) :ms ms
                 :output (norn.string/trim (:out r))}]
    (if (= 0 (:exit r))
      (log/info "job finished" :job name :ms ms)
      (log/error "job failed" :job name :exit (:exit r)
                 :error (norn.string/trim (:err r))))
    outcome))

(reset! events [])

(dissoc (run-job "greet" "echo hello") :ms)
; => {:job "greet" :exit 0 :output "hello"}

(:exit (run-job "broken" "echo oops >&2; exit 4")) ; => 4

(mapv (juxt :level :msg) @events)
; => [[:info "job starting"] [:info "job finished"]
;     [:info "job starting"] [:error "job failed"]]

(:data (last @events))
; => [[:job "broken"] [:exit 4] [:error "oops"]]
```

To run it every night at 3:00 in a named zone, hand it to a schedule:

```clojure
(def nightly
  (sch/schedule "0 3 * * *" (fn [] (run-job "backup" "echo backing up"))
                :zone "Europe/Paris"))

(t/format (sch/next-run "0 3 * * *" launch :zone "Europe/Paris") "Europe/Paris")
; => "2026-11-14T03:00:00+01:00"

(sch/stop! nightly)
(log/set-sink! nil)
```

A real program would keep the schedule running, listen for `:term` with
`norn.signal`, and stop the schedule and exit cleanly when it arrives.

## Summary

- `norn.sys` reads arguments (`program-args`), the environment (`getenv`,
  `env`), and facts about the machine (`info`) when you ask. `exit` ends the
  process with a status code.
- `norn.process/sh` runs a program and returns `{:exit :signal :out :err}`.
  A nonzero exit is data, not an error. No shell is involved unless you run
  one.
- `start` gives you streams to a running program. Close `:in` to end its
  input, `wait` for it, `kill` it, and `close` its handles when you're done.
  Connect programs with `copy-stream!` in a fiber.
- `start-pty` runs a program on a pseudo-terminal, for programs that want a
  terminal.
- `norn.signal/listen` delivers signals on a channel, which makes clean
  shutdown an ordinary `alts!`.
- `norn.time` keeps instants and durations apart from calendar values.
  Measure elapsed time with `monotonic-nanos`, name zones explicitly, and
  pick a `:resolve` policy for times that clock changes skip or repeat.
- `norn.schedule` checks cron expressions up front, can tell you when they
  are due, and runs functions on them in fibers that never overlap.
- `norn.log` macros skip their arguments below the level. Levels can be set
  per namespace, and a sink function decides where events go.

---

[Contents](README.md) · Previous: [20. Data formats](20-data-formats.md) · Next: [22. Networking and HTTP](22-networking-and-http.md) <!-- nav -->
