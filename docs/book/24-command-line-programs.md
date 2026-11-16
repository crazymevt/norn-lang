# 24. Command-line programs

[Contents](README.md) · Previous: [23. Databases](23-databases.md) · Next: [25. Testing](25-testing.md) <!-- nav -->

A command-line program has a small, strict interface. It reads arguments,
writes results to standard output and complaints to standard error, and
ends with an exit status that tells the shell how it went. Good ones also
explain themselves with `--help`, catch typos, and use color only when
someone is looking.

This chapter builds one such program, `notes`, which keeps short notes in
a folder. Along the way it covers `norn.cli`, which turns a description of
the command line into a parser, help text, and dispatch to subcommands, and
`norn.terminal`, which styles output and reads keys one at a time for
interactive programs. [Chapter 21](21-the-operating-system.md) covered the
raw materials, `norn.sys/program-args` and `norn.sys/exit`. Here you put
them to work.

## Arguments, output, and exit status

Run a script as `norn notes.norn add buy milk`, and
`norn.sys/program-args` returns `["add" "buy" "milk"]`, the arguments after
the script's path. When the program is done, `norn.sys/exit` ends it with a
status. By convention:

- 0 means success.
- 1 means the program ran but failed, or found nothing. `grep` returns 1
  when no line matches.
- 2 means the command line itself was wrong.

A script that reaches its end exits with 0, and one that ends with an
unhandled error exits with 1. Shell scripts and other programs depend on
these numbers, so choose them on purpose.

Output has two channels. Standard output is for results, the text that
another program might read through a pipe. Standard error is for messages
to the person running the program: errors, warnings, and progress. Keeping
them apart means `notes list | wc -l` counts notes, not error messages.

`println` writes to standard output. `norn.io` has `eprintln` and `eprint`,
which print to standard error the way `println` and `print` print to
standard output:

```clojure
(require '[norn.io :as io :refer [eprintln]] '[norn.string :as str])

(eprintln "notes: something went wrong") ; prints to standard error
```

`norn.log` ([Chapter 21](21-the-operating-system.md)) also writes to
standard error, and suits messages that need levels and timestamps.

## Describing the command line

Parsing arguments by hand starts simple and goes wrong fast: options in any
order, `--name=value` and `--name value`, `-abc` for three flags, values
that should be numbers, a `--help` that drifts out of date. `norn.cli`
handles all of that from a description of the command line, called a
spec. A spec is a map, so it's ordinary data you can build, inspect, and
test.

Here is a first spec for `notes`. For now it takes words to add as a note,
an option for the folder, and a flag for more output:

```clojure
(require '[norn.cli :as cli])

(def first-spec
  {:name "notes"
   :summary "Keep short notes in a folder."
   :options [{:name :dir :short \d :value "DIR" :default "notes" :doc "Where the notes live"}
             {:name :verbose :short \v :doc "Say more"}]
   :args [{:name :words :value "TEXT" :variadic true :doc "The note"}]})
```

`cli/parse` takes a spec and an argument vector and returns what it found.
Pass the vector yourself, as here, to try a spec at the REPL or in a test.
In a program, leave it out and `parse` reads `program-args`.

```clojure
(dissoc (cli/parse first-spec ["-v" "buy" "milk"]) :spec)
; => {:options {:dir "notes" :verbose true} :args {:words ["buy" "milk"]}
;     :command []}
```

The result has the `:options` by name, the positional `:args` by name, the
`:command` path, which is empty until there are subcommands, and the
`:spec` that matched, left out here to keep the output short.

Each option is a map. `:name` is the keyword it appears under in
`:options`, and also its long form, so `:dir` gives `--dir`. `:short` adds a
one-character form, `-d`. `:value` names the placeholder that help text
shows, and `:doc` describes the option.

Options appear in the result even when they weren't given. `:dir`
wasn't given, so it has its `:default`. `--verbose` is a flag, an option
that takes no value: it is `true` when given and `false` when not.

The parser accepts the usual spellings, in any order around the
arguments:

```clojure
(:options (cli/parse first-spec ["--dir=work" "x"])) ; => {:dir "work" :verbose false}
(:options (cli/parse first-spec ["x" "--dir" "work"])) ; => {:dir "work" :verbose false}
(:options (cli/parse first-spec ["-vdwork" "x"])) ; => {:dir "work" :verbose true}
(:args (cli/parse first-spec ["--" "-v" "is" "text"])) ; => {:words ["-v" "is" "text"]}
```

In `-vdwork`, `-v` is a flag and `-d` takes the rest of the word as its
value. After `--`, everything is an argument, even if it starts with a
dash. A negative number such as `-5` also counts as an argument.

### Flags and values

What makes an option a flag is what it lacks. An option with no `:value`,
`:type`, `:choices`, or `:multiple` is a flag. That's convenient, and it's
also the most common mistake with `norn.cli`. Leave out `:value` on an
option that should take one, and its value becomes a stray argument:

```clojure
(def oops {:name "notes" :options [{:name :tag :short \t}]})

(cli/parse oops ["-t" "work"]) ; !! norn.cli/UsageError

(condition-message (handler-case (cli/parse oops ["-t" "work"])
                     (cli/UsageError [e] e)))
; => "unexpected argument \"work\""
```

`-t` was taken as a flag, and `"work"` had nowhere to go. A bad command
line signals a `norn.cli/UsageError` whose message is written for the
person at the keyboard.

### Types, choices, and the rest

`:type` converts an option's text: `:string` (the default), `:int`,
`:float`, `:keyword`, `:boolean`, or a function of the text. `:choices`
lists the values allowed. Both produce usage errors with clear messages
when the text doesn't fit. Several more keys cover the shapes options come
in:

- `:multiple true` collects every occurrence in a vector, `[]` if none.
- `:count true` counts occurrences, as in `-vvv`, 0 if none.
- `:negatable true` gives a flag a `--no-` form, for a flag that defaults
  to on.
- `:required true` makes the option mandatory.
- `:env "NAME"` reads an environment variable when the option isn't given,
  before falling back to `:default`.

Here they are together in a throwaway spec:

```clojure
(def demo
  {:name "demo"
   :options [{:name :limit :short \n :type :int :default 10}
             {:name :format :short \f :choices ["text" "json"] :default "text"}
             {:name :tag :short \t :multiple true}
             {:name :verbose :short \v :count true}
             {:name :color :negatable true :default true}
             {:name :ratio :type :float}]})

(:options (cli/parse demo ["-n" "3" "-fjson" "-t" "a" "-tb" "-vv" "--no-color"]))
; => {:limit 3 :format "json" :tag ["a" "b"] :verbose 2 :color false}

(:options (cli/parse demo []))
; => {:limit 10 :format "text" :tag [] :verbose 0 :color true}
```

`:ratio` has no default and wasn't given, so it is absent from the map.
That's the one case where an option is missing from `:options`, and
`(:ratio options)` gives nil, as you'd want.
The errors read well:

```clojure
(defn usage-error [spec args]
  (handler-case (do (cli/parse spec args) nil)
    (cli/UsageError [e] (condition-message e))))

(usage-error demo ["-n" "lots"]) ; => "-n: expected an integer, got \"lots\""
(usage-error demo ["--format" "xml"]) ; => "--format: \"xml\" isn't one of text, json"
(usage-error demo ["--limt" "3"]) ; => "unknown option \"--limt\" (did you mean \"--limit\"?)"
```

The last one guesses what you meant. A mistake in the spec itself, such as
an option without a keyword name, is a plain error rather than a usage
error, because it's the programmer's mistake and not the user's:

```clojure
(cli/parse {:options [{:name "x"}]} []) ; !! Error
```

### Positional arguments

`:args` lists the positional arguments in order. Each one is required
unless it's `:optional`, and an optional one can have a `:default`. The
last one can be `:variadic`, collecting the rest into a vector. Arguments
take `:type` and `:choices` too. When an optional argument comes first,
the parser fills the required ones before it:

```clojure
(def copy-spec {:name "copy" :args [{:name :src :optional true :default "."} {:name :dest}]})

(:args (cli/parse copy-spec ["out"])) ; => {:src "." :dest "out"}
(:args (cli/parse copy-spec ["in" "out"])) ; => {:src "in" :dest "out"}
(usage-error copy-spec []) ; => "missing DEST"
(usage-error copy-spec ["a" "b" "c"]) ; => "unexpected argument \"c\""
```

## Subcommands

`notes` needs several actions: add a note, list them, and mark one done.
Programs like `git` handle that with subcommands, each with its own
options and arguments. In a spec, `:commands` holds a spec for each
subcommand, named by a string:

```clojure
(def spec-v2
  {:name "notes" :summary "Keep short notes in a folder." :version "1.0"
   :options [{:name :dir :short \d :value "DIR" :default "notes" :env "NOTES_DIR"
              :doc "Where the notes live"}
             {:name :verbose :short \v :count true :doc "Say more"}]
   :commands [{:name "add" :summary "Add a note"
               :options [{:name :tag :short \t :multiple true :doc "Tag the note"}]
               :args [{:name :words :value "TEXT" :variadic true :doc "The note"}]}
              {:name "list" :summary "List notes"
               :options [{:name :tag :short \t :value "TAG" :doc "Only notes with this tag"}
                         {:name :limit :short \n :type :int :default 10 :doc "At most this many"}
                         {:name :sort :choices ["newest" "oldest"] :default "newest" :doc "Order"}]}
              {:name "done" :summary "Remove a note"
               :args [{:name :id :type :int}]}]})
```

The options at the top are global: every subcommand accepts them, before
or after its name. A subcommand's own options apply only to it, so `--tag`
can mean "tag the new note" for `add` and "filter by tag" for `list`.
Now `:command` in the result says which subcommand ran:

```clojure
(dissoc (cli/parse spec-v2 ["add" "-t" "home" "buy" "milk" "-v"]) :spec)
; => {:command ["add"] :options {:dir "notes" :verbose 1 :tag ["home"]}
;     :args {:words ["buy" "milk"]}}

(:options (cli/parse spec-v2 ["--dir" "work" "list" "-n" "3"]))
; => {:dir "work" :verbose 0 :limit 3 :sort "newest"}

(usage-error spec-v2 ["lsit"]) ; => "unknown command \"lsit\" (did you mean \"list\"?)"
(usage-error spec-v2 []) ; => "missing command (one of add, list, done)"
```

Subcommands can have subcommands of their own, as in `notes remote add`,
and `:command` is then the whole path, `["remote" "add"]`. A command with
subcommands can't also take positional arguments.

The `:env` on `--dir` means that when `NOTES_DIR` is set and `--dir` isn't
given, the program uses the variable's value. That lets a user set the
folder once in their shell profile.

## Help that writes itself

Every spec gets `--help` and `-h`, and a spec with a `:version` gets
`--version`. Parsing either returns a map that says so, instead of
checking the rest of the command line:

```clojure
(dissoc (cli/parse spec-v2 ["list" "--help" "--bogus"]) :spec)
; => {:help true :command ["list"]}
```

`cli/usage` returns the help text for a spec, or for a subcommand when you
give its path. The text comes from the same spec that the parser uses, so
it can't fall out of date:

```clojure
(print (cli/usage spec-v2))
; prints: Usage: notes [options] <command> [args]
; prints:
; prints: Keep short notes in a folder.
; prints:
; prints: Commands:
; prints:   add   Add a note
; prints:   list  List notes
; prints:   done  Remove a note
; prints:
; prints: Options:
; prints:   -d, --dir DIR  Where the notes live (default notes; env NOTES_DIR)
; prints:   -v, --verbose  Say more (repeatable)
; prints:   -h, --help     Show this help and exit
; prints:       --version  Show the version and exit
; prints:
; prints: Run 'notes <command> --help' for a command's options.
```

A subcommand's help lists its own options, then the global ones it
inherits:

```clojure
(print (cli/usage spec-v2 ["list"]))
; prints: Usage: notes list [options]
; prints:
; prints: List notes
; prints:
; prints: Options:
; prints:   -t, --tag TAG      Only notes with this tag
; prints:   -n, --limit LIMIT  At most this many (default 10)
; prints:       --sort SORT    Order (one of newest, oldest; default newest)
; prints:   -h, --help         Show this help and exit
; prints:
; prints: Global options:
; prints:   -d, --dir DIR  Where the notes live (default notes; env NOTES_DIR)
; prints:   -v, --verbose  Say more (repeatable)
; prints:       --version  Show the version and exit
```

The notes in parentheses come from the option's keys: defaults, choices,
environment variables, and whether it repeats. A `:doc` on an argument
adds an "Arguments" section.

## Running commands

`parse` only parses. `cli/run` does the rest of a program's front door:

- For `--help` it prints the help, and for `--version` it prints the name
  and version. Both return 0.
- For a bad command line, it writes the error and a hint to standard error
  and returns 2.
- Otherwise it calls the `:run` function of the command that was chosen
  with the parsed map, and returns the exit status. That's `:run`'s value
  when it's an integer, and 0 otherwise.

A program's last line hands that status to `exit`:

<!-- skip -->
```clojure
(norn.sys/exit (cli/run spec))
```

That line ends the process, so this chapter calls `cli/run` with explicit
argument vectors instead and looks at the status it returns.

### The commands

Each subcommand needs a `:run` function. They share a few helpers for
storage. The notes live in one EDN file in the folder, read and written
with `norn.edn` ([Chapter 20](20-data-formats.md)). `write-atomic!`
replaces the file all at once, so a crash never leaves half a file
([Chapter 19](19-files-and-streams.md)).

```clojure
(require '[norn.edn :as edn] '[norn.terminal :as term])

(defn store-path [dir] (io/path dir "notes.edn"))

(defn load-notes [dir]
  (let [f (store-path dir)]
    (if (io/file? f) (edn/read-string (io/read-text f)) [])))

(defn save-notes! [dir notes]
  (io/mkdirs! dir)
  (io/write-atomic! (store-path dir) (edn/write-string notes)))
```

A note is a map with an `:id`, its `:text`, and its `:tags`. `show-note`
formats one for the screen. It uses `term/style`, which the next section
explains; for now, read it as "yellow" and "cyan" when color is on, and
plain text when it isn't.

```clojure
(defn show-note [{:keys [id text tags]}]
  (str (term/style (str/pad-left (str id) 3) :yellow) "  " text
       (when (seq tags)
         (str "  " (term/style (str/join " " (map #(str "#" %) tags)) :cyan)))))
```

The `:run` functions receive the parsed map, so they destructure
`:options` and `:args` and get on with the work:

```clojure
(defn add-cmd [{:keys [options args]}]
  (let [dir (:dir options)
        notes (load-notes dir)
        id (inc (reduce max 0 (map :id notes)))]
    (save-notes! dir (conj notes {:id id :text (str/join " " (:words args))
                                  :tags (:tag options)}))
    (println "added note" id)
    (when (pos? (:verbose options))
      (println "in" (store-path dir)))))

(defn list-cmd [{:keys [options]}]
  (let [notes (cond->> (load-notes (:dir options))
                (:tag options) (filter #(some #{(:tag options)} (:tags %)))
                (= "newest" (:sort options)) reverse
                true (take (:limit options)))]
    (if (empty? notes)
      (do (eprintln "notes: no notes") 1)
      (doseq [n notes] (println (show-note n))))))

(defn done-cmd [{:keys [options args]}]
  (let [dir (:dir options)
        notes (load-notes dir)
        id (:id args)]
    (if (some #(= id (:id %)) notes)
      (do (save-notes! dir (vec (remove #(= id (:id %)) notes)))
          (println "removed note" id))
      (do (eprintln "notes: no note" id) 1))))
```

Look at what each returns. `add-cmd` returns whatever `when` gives, which
isn't an integer, so `run` reports 0. `list-cmd` returns 1 when there is
nothing to list, and `done-cmd` returns 1 when the note doesn't exist,
after explaining on standard error. A missing note is a failure, not a bad
command line, so it gets 1 rather than 2.

Now attach each function to its command with `:run`, and the spec is
complete:

```clojure
(def runs {"add" add-cmd "list" list-cmd "done" done-cmd})

(def spec
  (update spec-v2 :commands
          (fn [cmds] (mapv #(assoc % :run (runs (:name %))) cmds))))
```

Writing `:run add-cmd` directly in each command map works just as well,
and the full listing at the end of the chapter does that.

### Trying it out

Calling `cli/run` with an argument vector runs the program as a user
would, minus the process. To keep the output plain whatever the
environment says about color, bind `term/*color*` to `:none`. A small
helper does both:

```clojure
(defn notes [& args]
  (binding [term/*color* :none]
    (cli/run spec (vec args))))

(notes "add" "buy" "milk" "-t" "home")
; prints: added note 1

(notes "add" "-v" "-t" "work" "call" "Ada") ; => 0

(notes "list")
; prints:   2  call Ada  #work
; prints:   1  buy milk  #home

(notes "list" "--sort" "oldest" "--tag" "work")
; prints:   2  call Ada  #work
```

The examples run in a temporary folder, so the notes go to `notes/` inside
it. The exit statuses tell the rest of the story:

```clojure
(notes "done" "9") ; => 1
(notes "done" "1") ; => 0
(notes "list" "--sort" "sideways") ; => 2
(notes "--version") ; => 0
(notes "--dir" "empty" "list") ; => 1
```

`done 9` and the empty folder print their complaints on standard error.
`--sort sideways` makes `run` print this there:

```text
notes: --sort: "sideways" isn't one of newest, oldest
Run 'notes list --help' for usage.
```

Because the spec, the commands, and the storage are all ordinary
functions and data, you can test the whole program this way, from
`norn.test` ([Chapter 25](25-testing.md)), without starting a process.

## Styling output

Color helps people scan output: ids in one color, tags in another, errors
in red. It also gets in the way when output goes to a file or another
program, which then has escape codes in its input. `norn.terminal` takes
care of the difference.

`term/style` wraps text in ANSI escape codes for attributes such as
`:bold`, `:dim`, `:italic`, `:underline`, and `:reverse`, and for colors.
Colors can be keywords such as `:red` and `:bright-blue`, a number from 0
to 255, an RGB vector, or a `"#rrggbb"` string. A map sets the foreground
and background separately:

```clojure
(binding [term/*color* :basic]
  (term/style "Error" :bold :red))
; => "\u001b[1;31mError\u001b[0m"

(binding [term/*color* :truecolor]
  (term/style "warm" {:fg [255 128 0] :bg :blue}))
; => "\u001b[38;2;255;128;0;44mwarm\u001b[0m"
```

### When to use color

`style` decides on its own whether to use color, by calling
`color-support`. That function returns `:none`, `:basic` (16 colors),
`:256`, or `:truecolor`, following conventions that users rely on:

- If `NO_COLOR` is set and not empty, there's no color.
- Otherwise, `FORCE_COLOR` set to 1, 2, or 3 forces that level, and 0 turns
  color off.
- Otherwise, there's color only when standard output is a terminal and
  `TERM` isn't `dumb`. `COLORTERM` and `TERM` then say how many colors the
  terminal has.

So `notes list` in a terminal is colored, and `notes list > out.txt` isn't,
with no code in `notes` to arrange it. The examples in this chapter don't
run on a terminal, so here `color-support` says `:none` (unless
`FORCE_COLOR` is set):

```clojure
(term/tty? :out) ; => false
(term/style "plain" :bold :red) ; => "plain"
```

When color is off, `style` returns the text unchanged. A color the
terminal can't show is brought down to the nearest one it can:

```clojure
(binding [term/*color* :basic]
  (term/style "warm" "#ff8000"))
; => "\u001b[33mwarm\u001b[0m"
```

Binding `term/*color*` overrides the decision. Bind it to `:none` in tests,
as the `notes` helper did, so that expected output doesn't depend on the
environment. Bind it to a level to write colored text to a file on
purpose, or to honor a `--color` option of your own.

Styles nest. An inner style ends by returning to the outer one, so you can
color a word inside a colored line.

### Measuring styled text

Escape codes take characters but no space on the screen, and some
characters, such as most Chinese and Japanese ones, take two columns.
`term/width` counts the columns text takes, ignoring escape codes, and
`term/strip` removes them:

```clojure
(def label (binding [term/*color* :basic] (term/style "日本" :red)))

(count label) ; => 11
(term/width label) ; => 4
(term/strip label) ; => "日本"
```

This matters when you line things up. `str/width`, `str/pad-left`, and
`str/pad-right` measure the same way, ignoring escape codes, so you can
pad styled text or style padded text; `show-note` does the second with the
id. To fit output to the terminal, `term/size`
returns `{:rows :cols}`, or nil when nothing says.

`norn.terminal` also has functions that return control sequences for you
to print: `cursor-to`, `cursor-up` and its relatives, `clear`,
`hide-cursor` and `show-cursor`, `alternate-screen` and `main-screen`, and
`title`:

```clojure
(term/cursor-to 3 7) ; => "\u001b[3;7H"
(term/clear :line) ; => "\u001b[2K"
```

## Reading keys

Normally a terminal is in line mode. It collects what you type, lets you
edit it, and hands the program a whole line when you press Enter. Ctrl-C
interrupts the program. An interactive program, such as a menu you move
through with arrow keys, needs each key as it's pressed. That's raw mode.

`term/with-raw-mode` turns raw mode on for its body and restores the
terminal afterward, even if the body fails. Inside, `term/read-key` returns
the next key. Keys typed don't echo, and Ctrl-C and Ctrl-Z arrive as keys
instead of signals, so your program decides what they mean. Output still
works as usual, including newlines.

A key is a map. A typed character is `{:key \a}`. A special key is a
keyword, with modifiers as flags: `{:key :up}`, `{:key :right :ctrl true}`,
`{:key :enter}`, `{:key :f5}`. Ctrl-C is `{:key \c :ctrl true}`.
`key-name` turns a key into text such as `"ctrl+c"`.

Raw mode needs a real terminal, which these examples don't have. But the
keys are decoded by ordinary Norn code over the bytes a terminal sends,
and `decode-keys` runs that decoder on bytes you give it. An arrow key, for
instance, arrives as three bytes: Escape, `[`, and a letter.

```clojure
(def esc (char 27))

(:keys (term/decode-keys (bytes (str "q" esc "[A" esc "[1;5C" (char 3) "\r"))))
; => [{:key \q} {:key :up} {:key :right :ctrl true} {:key \c :ctrl true} {:key :enter}]

(mapv term/key-name (:keys (term/decode-keys (bytes (str esc "[A" esc "[1;5C" (char 3))))))
; => ["up" "ctrl+right" "ctrl+c"]
```

`decode-keys` also returns the bytes at the end that don't make a whole
key yet, so that a key split across two reads still decodes. `read-key`
handles that for you. It also tells a lone Escape key from the start of a
sequence by waiting 50 ms for more bytes.

### A picker

The `notes pick` command will show the notes and let the user choose one
with the arrow keys. The way to build it is to keep the logic in plain
functions and leave only the reading and printing to raw mode. The state
is the highlighted row, and a function computes the next state from a key:

```clojure
(defn pick-step [{:keys [index size] :as state} k]
  (match k
    {:key :up} (assoc state :index (max 0 (dec index)))
    {:key :down} (assoc state :index (min (dec size) (inc index)))
    {:key :enter} (assoc state :done :chosen)
    (:or {:key \q} {:key :escape} {:key \c :ctrl true}) (assoc state :done :cancelled)
    _ state))
```

A map pattern in `match` matches any map with those keys and values, so
`{:key :up}` also matches an up arrow with modifiers held down. Keys that
mean nothing here leave the state alone. Because keys are data, you can
test the picker with keys made by `decode-keys`, no terminal needed:

```clojure
(defn keys-of [s] (:keys (term/decode-keys (bytes s))))

(reduce pick-step {:index 0 :size 3} (keys-of (str esc "[B" esc "[B" esc "[B" "x" esc "[A")))
; => {:index 1 :size 3}

(reduce pick-step {:index 0 :size 3} (keys-of (str esc "[B" "\r")))
; => {:index 1 :size 3 :done :chosen}
```

Down three times stops at the last row, `x` does nothing, and up moves
back to the middle. Drawing is a function of the state too, and it returns
a string:

```clojure
(defn render [notes {:keys [index]}]
  (str/join "\n"
            (cons (term/style "Up and down choose, Enter picks, q quits." :dim)
                  (map-indexed (fn [i n]
                                 (if (= i index)
                                   (term/style (str "> " (:text n)) :reverse)
                                   (str "  " (:text n))))
                               notes))))

(binding [term/*color* :none]
  (println (render [{:text "buy milk"} {:text "call Ada"}] {:index 1})))
; prints: Up and down choose, Enter picks, q quits.
; prints:   buy milk
; prints: > call Ada
```

What's left is a small loop that puts these together in raw mode: draw,
read a key, step, and repeat until the state is done. This is the one part
that needs a terminal, so it doesn't run here:

<!-- skip -->
```clojure
(defn pick-cmd [{:keys [options]}]
  (let [notes (vec (load-notes (:dir options)))]
    (cond
      (empty? notes) (do (eprintln "notes: no notes") 1)
      (not (term/tty? :in)) (do (eprintln "notes: pick needs a terminal") 1)
      :else
      (let [final (term/with-raw-mode {:hide-cursor true}
                    (loop [state {:index 0 :size (count notes)}]
                      (print (str (term/cursor-to 1 1) (term/clear) (render notes state)))
                      (let [state (pick-step state (term/read-key))]
                        (if (:done state) state (recur state)))))]
        (println)
        (if (= :chosen (:done final))
          (println (show-note (nth notes (:index final))))
          1)))))
```

`pick-cmd` checks `tty?` first, because raw mode is an error when standard
input isn't a terminal, and a clear message is kinder than a stack trace.
The `{:hide-cursor true}` option hides the cursor for the body's duration
and shows it again afterward. `with-raw-mode` can also switch to the
alternate screen, the way full-screen programs leave your scrollback
untouched, and turn on mouse reports and bracketed paste. See
[the reference](../reference/libraries.md#the-terminal) for those, for the
kitty keyboard protocol, and for `suspend!`, which handles Ctrl-Z.

Two more things come up in full-screen programs. To redraw when the window
changes size, listen for `:winch` with `norn.signal`
([Chapter 21](21-the-operating-system.md)) and call `term/size` again. And
to test a raw-mode program end to end, run it on a pseudo-terminal with
`norn.process/start-pty`, write key bytes to its input, and read what it
draws.

## The whole program

Here is `notes.norn` in full. Each piece ran in this chapter. The `ns`
form gathers the requires, the spec names each `:run` directly, and the
last line hands the exit status to the operating system.

<!-- skip -->
```clojure
(ns notes
  (:require [norn.cli :as cli] [norn.edn :as edn] [norn.io :as io :refer [eprintln]]
            [norn.string :as str] [norn.sys :as sys] [norn.terminal :as term]))

(defn store-path [dir] (io/path dir "notes.edn"))

(defn load-notes [dir]
  (let [f (store-path dir)]
    (if (io/file? f) (edn/read-string (io/read-text f)) [])))

(defn save-notes! [dir notes]
  (io/mkdirs! dir)
  (io/write-atomic! (store-path dir) (edn/write-string notes)))

(defn show-note [{:keys [id text tags]}]
  (str (term/style (str/pad-left (str id) 3) :yellow) "  " text
       (when (seq tags)
         (str "  " (term/style (str/join " " (map #(str "#" %) tags)) :cyan)))))

(defn add-cmd [{:keys [options args]}]
  (let [dir (:dir options)
        notes (load-notes dir)
        id (inc (reduce max 0 (map :id notes)))]
    (save-notes! dir (conj notes {:id id :text (str/join " " (:words args))
                                  :tags (:tag options)}))
    (println "added note" id)
    (when (pos? (:verbose options))
      (println "in" (store-path dir)))))

(defn list-cmd [{:keys [options]}]
  (let [notes (cond->> (load-notes (:dir options))
                (:tag options) (filter #(some #{(:tag options)} (:tags %)))
                (= "newest" (:sort options)) reverse
                true (take (:limit options)))]
    (if (empty? notes)
      (do (eprintln "notes: no notes") 1)
      (doseq [n notes] (println (show-note n))))))

(defn done-cmd [{:keys [options args]}]
  (let [dir (:dir options)
        notes (load-notes dir)
        id (:id args)]
    (if (some #(= id (:id %)) notes)
      (do (save-notes! dir (vec (remove #(= id (:id %)) notes)))
          (println "removed note" id))
      (do (eprintln "notes: no note" id) 1))))

(defn pick-step [{:keys [index size] :as state} k]
  (match k
    {:key :up} (assoc state :index (max 0 (dec index)))
    {:key :down} (assoc state :index (min (dec size) (inc index)))
    {:key :enter} (assoc state :done :chosen)
    (:or {:key \q} {:key :escape} {:key \c :ctrl true}) (assoc state :done :cancelled)
    _ state))

(defn render [notes {:keys [index]}]
  (str/join "\n"
            (cons (term/style "Up and down choose, Enter picks, q quits." :dim)
                  (map-indexed (fn [i n]
                                 (if (= i index)
                                   (term/style (str "> " (:text n)) :reverse)
                                   (str "  " (:text n))))
                               notes))))

(defn pick-cmd [{:keys [options]}]
  (let [notes (vec (load-notes (:dir options)))]
    (cond
      (empty? notes) (do (eprintln "notes: no notes") 1)
      (not (term/tty? :in)) (do (eprintln "notes: pick needs a terminal") 1)
      :else
      (let [final (term/with-raw-mode {:hide-cursor true}
                    (loop [state {:index 0 :size (count notes)}]
                      (print (str (term/cursor-to 1 1) (term/clear) (render notes state)))
                      (let [state (pick-step state (term/read-key))]
                        (if (:done state) state (recur state)))))]
        (println)
        (if (= :chosen (:done final))
          (println (show-note (nth notes (:index final))))
          1)))))

(def spec
  {:name "notes" :summary "Keep short notes in a folder." :version "1.0"
   :options [{:name :dir :short \d :value "DIR" :default "notes" :env "NOTES_DIR"
              :doc "Where the notes live"}
             {:name :verbose :short \v :count true :doc "Say more"}]
   :commands [{:name "add" :summary "Add a note"
               :options [{:name :tag :short \t :multiple true :doc "Tag the note"}]
               :args [{:name :words :value "TEXT" :variadic true :doc "The note"}]
               :run add-cmd}
              {:name "list" :summary "List notes"
               :options [{:name :tag :short \t :value "TAG" :doc "Only notes with this tag"}
                         {:name :limit :short \n :type :int :default 10 :doc "At most this many"}
                         {:name :sort :choices ["newest" "oldest"] :default "newest" :doc "Order"}]
               :run list-cmd}
              {:name "done" :summary "Remove a note"
               :args [{:name :id :type :int}]
               :run done-cmd}
              {:name "pick" :summary "Choose a note with the arrow keys"
               :run pick-cmd}]})

(sys/exit (cli/run spec))
```

Run it from a shell:

```sh
norn notes.norn add buy milk -t home
norn notes.norn add -t work call Ada
norn notes.norn list
norn notes.norn lsit; echo "status $?"
NOTES_DIR=elsewhere norn notes.norn list; echo "status $?"
```

```text
added note 1
added note 2
  2  call Ada  #work
  1  buy milk  #home
notes: unknown command "lsit" (did you mean "list"?)
Run 'notes --help' for usage.
status 2
notes: no notes
status 1
```

In a terminal, the ids are yellow and the tags cyan. `norn notes.norn pick`
opens the picker, and `norn notes.norn list | cat` is plain again.

To ship `notes` as a program of its own, build it into an image with an
entry function ([Chapter 27](27-images-and-deployment.md)). The entry
function receives the arguments, and its result becomes the exit status,
so it replaces the last line:

<!-- skip -->
```clojure
(defn main [& args] (cli/run spec args))
```

## Summary

- Use exit status 0 for success, 1 for a failure, and 2 for a bad command
  line. Results go to standard output and messages to standard error.
- A `norn.cli` spec is data: `:options`, `:args`, and `:commands`, each a
  map. `cli/parse` turns arguments into `{:options :args :command}` or
  signals a `UsageError` with a message meant for users.
- An option without `:value`, `:type`, `:choices`, or `:multiple` is a
  flag. `:default`, `:env`, `:required`, `:multiple`, `:count`, and
  `:negatable` cover the other shapes.
- `--help`, `--version`, and the help text come from the spec, so they
  stay accurate. `cli/usage` returns the text.
- `cli/run` prints help, reports usage errors, calls the chosen command's
  `:run`, and returns the status for `norn.sys/exit`.
- `term/style` colors text only when the output can show it, following
  `NO_COLOR`, `FORCE_COLOR`, and the terminal. Bind `term/*color*` to
  decide yourself, and measure styled text with `term/width`.
- Raw mode delivers keys as maps. Keep key handling in pure functions and
  test them with `decode-keys`, so only a thin loop needs a terminal.

---

[Contents](README.md) · Previous: [23. Databases](23-databases.md) · Next: [25. Testing](25-testing.md) <!-- nav -->
