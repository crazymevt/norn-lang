# 27. Images and deployment

[Contents](README.md) · Previous: [26. Tools and performance](26-tools-and-performance.md) · Next: [28. Foreign functions](28-foreign-functions.md) <!-- nav -->

A Norn program doesn't have to start from source each time. An image is a
snapshot of a running Norn heap, written to a file: your code, your data,
the namespaces you loaded, and even fibers that were waiting for
something. Starting from an image skips loading and compiling, and picks up
where the snapshot left off. This chapter shows how to save images from a
running program and build them from source, what survives a restore and
what has to be reconnected, how to inspect and compare images, and how
`norn package` turns a project into something you can hand to someone who
doesn't have Norn.

[Chapter 12](12-live-programming.md) uses images for development: you save
a REPL session and come back to it later. Here the focus is on images as the
way you ship a program.

## Saving an image

`save-image` writes the running image to a file. Give it an `:entry`, a var
or function to call when the image starts, and the image becomes a program.
Here's one that greets the people named on its command line:

```clojure
(require '[norn.process :as proc] '[norn.sys :as sys])

(def greetings (atom 0))

(defn greet [& names]
  (doseq [n names]
    (swap! greetings inc)
    (println (str "Hello, " n "!")))
  (count names))

(def saved (save-image "greet.nimg" :entry #'greet))

(set (keys saved)) ; => #{:objects :patches :reused :bytes}
(pos? (:bytes saved)) ; => true
```

The result says how many objects went into the file and how large it is.
Pass the entry as a var, `#'greet`, rather than the function itself. The
image then calls whatever the var holds, which is the same thing here, and
`norn image info` can name it.

Start the image with `norn --image`. Every argument after the image path
goes to the entry function as a string. To run it from the examples, this
helper calls the same `norn` executable that's running them:

```clojure
(defn run-image [path & args]
  (select-keys (apply proc/sh (:executable (sys/info)) "--image" path args)
               [:exit :out]))

(run-image "greet.nimg" "Ada" "Grace")
; => {:exit 2 :out "Hello, Ada!\nHello, Grace!\n"}
```

From a shell, that's:

```sh
norn --image greet.nimg Ada Grace
```

The entry's return value decides the exit status. An integer is the
status, clamped to 0–255, which is why greeting two people exited with 2.
Any other value means success, status 0. A condition that nobody handles
prints its message and exits with 1.

The child process started from a copy of this one's heap, so it had the
same `greetings` atom, but it's a separate process. Its changes don't come
back:

```clojure
@greetings ; => 0
```

Every argument goes to the entry, even `-e`: `(run-image "greet.nimg" "-e"
"(greet \"Lin\")")` greets `-e` and `(greet "Lin")` and exits with 2. To
evaluate an expression in the image instead of calling the entry, which is
handy for poking at a program's state, put `-e` and the expression before
`--image`:

```clojure
(select-keys (proc/sh (:executable (sys/info)) "-e" "(greet \"Lin\")" "--image" "greet.nimg")
             [:exit :out])
; => {:exit 0 :out "Hello, Lin!\n1\n"}
```

From a shell, that's `norn -e '(greet "Lin")' --image greet.nimg`. An image
without an entry also takes `norn --image dev.nimg -e EXPR`.

An image saved without `:entry` starts a REPL, which is what a development
image does. `(image-info)` returns the file the running image came from, or
`nil` when the process started from source:

```clojure
(image-info) ; => nil
```

## What survives

An image holds the heap, so everything that lives in the heap survives:
vars and their values, atoms, classes and their methods, closures,
namespaces, and the code of every function. Norn compiles each function
again for tier 1 as it runs, since images don't store machine code (see
[Chapter 26](26-tools-and-performance.md#two-tiers)).

Fibers survive too. By default, `save-image` saves every unfinished fiber
with its stack, and after a restore the fibers carry on. Here a fiber waits
on a channel when the image is saved, and the restored program sends it a
message:

```clojure
(def inbox (chan))

(def listener (spawn-detached (str "the listener got " (take! inbox))))

(defn deliver-and-wait [& _]
  (put! inbox :hello)
  (println (await listener))
  0)

(save-image "parked.nimg" :entry #'deliver-and-wait)

(run-image "parked.nimg") ; => {:exit 0 :out "the listener got :hello\n"}
```

The fiber that calls `save-image` is the exception. It goes on in the
process that saved, but not in the image. The image's own story starts
at its entry function.

`:fibers :none` saves no fibers. They end when the image is restored, and
anything that awaits one gets an error:

```clojure
(defn check-listener [& _]
  (println (handler-case (await listener)
             (Error [e] (str "await failed: " (condition-message e)))))
  0)

(save-image "no-fibers.nimg" :entry #'check-listener :fibers :none)

(run-image "no-fibers.nimg")
; => {:exit 0 :out "await failed: Fiber was not saved in the image\n"}
```

A program image usually wants `:fibers :none`, so every run starts the same
way. `norn build`, below, always saves this way.

Facts about the machine aren't in the heap. `norn.sys` reads the
environment, the command-line arguments, the host name, the core count and
the like live, so a restored image sees the machine it runs on, not the one
that saved it.

## Resources after a restore

Some objects stand for something outside the heap: an open file, a socket,
a database connection, a handle from a C library. The heap can save the
object, but not the thing it stands for. After a restore, such a resource
is **dead**. Using a dead file handle signals `ResourceLost`, and
`resource-live?` tells you which ones are dead:

```clojure
(require '[norn.io :as io])

(def notes (io/open "notes.txt" :write))
(resource-live? notes) ; => true

(defn use-notes [& _]
  (println "live?" (resource-live? notes))
  (handler-case (do (io/write notes "hi") 0)
    (ResourceLost [e] (println "lost:" (class-name (class-of e))) 1)))

(save-image "notes.nimg" :entry #'use-notes)

(:out (run-image "notes.nimg"))
; => "live? false\nlost: norn.core/ResourceLost\n"
```

File handles, sockets and pipes are tracked for you. For a resource of your
own, such as a connection object that holds a socket, call
`register-resource!` when you make it. The registry holds it weakly, so
registering doesn't keep it alive. Then choose how it comes back.

### Reconnecting eagerly

The generic function `on-image-restore` is called on every registered
resource after the image loads, before the entry function or REPL starts.
A method that reconnects the resource returns true, which marks it live:

```clojure
(defclass Conn ()
  ((addr :initarg :addr)
   (opened :initform 0 :mutable true))
  (:mutable))

(defn connect [addr]
  ;; A real one would open a socket here.
  (let [c (register-resource! (Conn :addr addr))]
    (update-slot! c :opened inc)
    c))

(defmethod on-image-restore ((c Conn))
  (println "reconnecting to" (:addr c))
  (update-slot! c :opened inc)
  true)

(def db (connect "db.local:5432"))

(defn report [& _]
  (println "live?" (resource-live? db) "opened" (:opened db) "times")
  0)

(save-image "conn.nimg" :entry #'report)

(:out (run-image "conn.nimg"))
; => "reconnecting to db.local:5432\nlive? true opened 2 times\n"
```

Use this for a resource the program needs at once, such as its database.
If the method signals an error, the restore prints a message and leaves the
resource dead, and the program starts anyway.

### Reconnecting lazily

Some resources are better reopened when they're next used, or not at all
if nothing uses them. For those, define a method on `reconnect` that
returns the live resource, and call `ensure-live` before each use.
`ensure-live` returns a live resource as it is. For a dead one it signals
`ResourceLost` with two restarts: `reconnect`, which calls your method,
and `abort`, which ends the fiber (restarts are covered in
[Chapter 11](11-conditions-and-restarts.md)). Use the value `ensure-live`
returns, since an OS handle can't be reopened in place and `reconnect` may
return a new object:

```clojure
(defclass LogFile () ((path :initarg :path) (handle :initarg :handle)))

(defn open-log [path]
  (register-resource! (LogFile :path path :handle (io/open path :append))))

(defmethod reconnect ((l LogFile))
  (println "reopening" (:path l))
  (open-log (:path l)))

(def the-log (atom (open-log "app.log")))

(defn log! [msg]
  (let [l (ensure-live @the-log)]
    (reset! the-log l)
    (io/write (:handle l) (str msg "\n"))))

(log! "before the save")

(defn log-after-restore [& _]
  (handler-bind [ResourceLost (fn [c] (invoke-restart 'reconnect))]
    (log! "after the restore"))
  (io/close (:handle @the-log))
  (print (io/read-text "app.log"))
  0)

(save-image "log.nimg" :entry #'log-after-restore)

(:out (run-image "log.nimg"))
; => "reconnecting to db.local:5432\nreopening app.log\nbefore the save\nafter the restore\n"
```

The first line comes from the `Conn` defined earlier, which is in this
image too and reconnects eagerly as before. The log file waits until
something writes to it.

The handler here chooses `reconnect` for every lost resource, which suits a
program image. At the REPL, with no handler, the same `ResourceLost` stops
in the debugger and offers both restarts, so you can choose.

## Building images from source

Saving from a running program captures whatever that program did,
including state you didn't mean to keep. For a release, build the image
from source instead, so the same sources always give the same image.
`norn build` starts a fresh Norn, loads your code, seals the modules it
loaded (their interfaces are fixed from then on, as the standard library's
are), and saves it with `:fibers :none`.

In a project, it needs only the output path. `norn.edn`'s `:main` names the
entry function, and `norn build` loads its namespace (see
[Chapter 8](08-modules-and-projects.md) for projects). Here's a small word
counter:

```edn
{:name wc
 :version "0.1.0"
 :paths ["src"]
 :main wc.main/main}
```

```clojure
(ns wc.main
  (:require [norn.io :as io]
            [norn.string :as str]))

(defn counts [text]
  {:lines (count (filter #{\newline} text))
   :words (count (remove str/blank? (str/split text #"\s+")))
   :chars (count text)})

(defn main [& paths]
  (if (empty? paths)
    (do (println "usage: wc FILE…") 2)
    (do (doseq [p paths]
          (let [{:keys [lines words chars]} (counts (io/read-text p))]
            (println (format "%6d %6d %6d %s" lines words chars p))))
        0)))

(counts "one two three\nfour five\n") ; => {:lines 2 :words 5 :chars 24}
```

```sh
norn build wc.nimg
norn --image wc.nimg sample.txt
```

```text
wc.nimg: 34255 objects, 1920976 bytes
     2      5     24 sample.txt
```

On the machine this chapter was written on, `norn --image wc.nimg` ran in
about 0.06 seconds, against about 0.25 seconds to load the same module from
source and call `main`. The difference grows with the program.

Outside a project, or to choose another entry, name the files to load and
the entry function:

```sh
norn build hello.nimg hello.norn --entry hello/main
```

Without `--entry`, and outside a project with a `:main`, the image has no
entry and starts a REPL.

Builds are reproducible. Building twice from the same sources with the same
`norn` gives byte-for-byte identical files. An image also records a
fingerprint of the runtime that wrote it, so a different build of `norn`
refuses it with a message instead of misreading it. Ship an image with the
`norn` that built it, which is what `norn package` does.

### Layered images

Several programs that share a large base, such as the same libraries, can
share one base image. Build the base from a file that loads the shared
libraries, here `(require 'norn.io 'norn.string 'norn.json)` in
`base.norn`. Then build each program on it with `--image BASE --layered`:

```sh
norn build base.nimg base.norn
norn build app.nimg --image base.nimg --layered
```

```text
base.nimg: 28366 objects, 1579808 bytes
app.nimg: 146 objects, 67896 bytes
```

The layered image holds only what changed since the base: new objects,
and patches for base objects that changed. It names its base, relative to
its own folder when the two share one, and checks the base's checksum when
it loads. Keep them together, since a missing or changed base stops the
program from starting. `save-image` takes `:layered true` for the same
thing from a running process that started from an image.

### Shaking an image

`wc.nimg` holds the whole of every module `wc` loaded, the compiler and
expander, and the index of documentation, though `main` uses a few dozen
functions. `--shake` leaves out what the entry function can't reach:

```sh
norn build wc.nimg --shake
```

```text
wc.nimg: 7379 objects, 385.6 KB, shaken from 1.8 MB (79% smaller): 1331 definitions left out, 13 modules whole, and 2 methods
```

It starts from `main` and follows what each function's compiled code
refers to (`norn.xref` reads it, as [Chapter 12](12-live-programming.md)
shows). Anything reached stays. That includes what the runtime itself calls,
such as `print-object`. The sealed modules' other names go, and with them
the code and data only those names kept. A generic function also loses the
methods for classes nothing kept can make. Your own modules are sealed by
`norn build`, so they're shaken too. Code the program runs isn't changed,
so a shaken `wc` gives the same output as the whole one.

Here the compiler went, because nothing in `wc` evaluates code. Without it,
the shaken image can't load a library that isn't in it, or run `-e`.
Shaking works out three cases from what's reachable:

- **The program evaluates code** (`eval`, `load-string`, `load-file`). The
  language is kept whole, compiler included, and so is each module that
  anything kept uses, since code evaluated later may refer to any of its
  names. Modules nothing uses still go.
- **The program reads text** (`read-string`, `norn.edn`). Tagged literals
  such as `#inst` need their readers' libraries, so those are loaded and
  kept.
- **The program looks things up by a name it computes** (`resolve`,
  `ns-resolve`, `ns-publics`, `require`). Shaking can't tell what the name
  will be, so it warns, naming the function that does it:

```text
warning: app.plugins/load calls norn.core/resolve: what it looks up may be gone; name it in :keep
```

`--keep` keeps a var, or every var of a namespace, besides what's reached.
Use it for what such code looks up:

```sh
norn build app.nimg --shake --keep app.plugins/csv --keep app.formats
```

`(save-image path :entry #'main :shake true :keep [...])` does the same
from a running process. Shaking without an entry keeps a REPL's image
usable: modules are kept whole or left out whole, and requiring one that
was left out loads it again. `:shake` can't be combined with `:layered`,
because a layer's base keeps everything anyway.

## Inspecting images

`norn image info` describes an image file: its layers, its size, its entry,
and how many public vars each namespace holds:

```sh
norn image info wc.nimg
```

```text
image:      /Users/you/wc/wc.nimg
size:       1920976 bytes
entry:      #'wc.main/main
namespaces: 16
vars:       1203
  norn.bytes                      16 vars
  norn.compiler                   25 vars
  norn.core                      943 vars
  norn.deps                       10 vars
  norn.edn                         5 vars
  norn.expander                   29 vars
  norn.io                         55 vars
  norn.io$1                        0 vars
  norn.pprint                      3 vars
  norn.process                     7 vars
  norn.string                     29 vars
  norn.syntax                     55 vars
  norn.sys                        23 vars
  norn.validate                    1 vars
  user                             0 vars
  wc.main                          2 vars
```

Only the libraries the program uses are in it. A namespace with `$1` in its
name is the compile-time instance of a module, which macros use (see
[Chapter 14](14-macros.md)). The shaken `wc.nimg` lists 5 namespaces and
278 vars: `norn.core`'s 273 that `main` reaches, and 1 of `norn.io`'s.
`norn image info` works on a shaken image too, since it asks the image for
its contents without compiling anything in it.

`norn image diff` compares two images, var by var. Suppose version 0.2 adds
a longest-line count to `counts`:

```sh
norn image diff wc-0.1.nimg wc.nimg
```

```text
~ wc.main/counts
+ wc.main/longest-line
```

`+` marks a var added, `-` one removed, and `~` one whose value or code
changed. An atom counts only by being there, not by what it holds, since
that is state, which changes as a program runs. That keeps the runtime's
own bookkeeping out of the diff. The compile-time instances of modules,
the `$1` namespaces, are left out too. Use a diff to check what a release really
changes, or to confirm that a rebuilt image is the same, in which case it
prints `no differences`.

## Packaging

An image needs a `norn` to run it. `norn package` puts the two together,
with your resources, into something you can give away. It builds the
project's image with `:main` as the entry, using the same `norn` you run it
with, then copies that `norn` in under your program's name. Run under
another name, `norn` looks for an image of that name beside it and runs it.

Give the project a `:package` map in `norn.edn`:

```edn
{:name wc
 :version "0.1.0"
 :paths ["src"]
 :main wc.main/main
 :package {:id "org.example.wc"
           :display-name "Word Count"
           :resources ["stopwords"]
           :targets [:cli :app]}}
```

- `:resources` lists files and folders to include, relative to the project.
- `:targets` says what to make: `:cli`, a command-line archive, and `:app`,
  a macOS app. The default is `[:cli]`. The flags `--cli` and `--app`
  override it, and `--out DIR` changes the output folder from `dist/`.
- `:id` and `:display-name` are the app's identifier and its name in Finder
  and the Dock. `:icon` names a PNG, which becomes an `.icns`, or an
  `.icns` file.
- `:shake true` builds the image with `--shake`, and `:keep` lists names
  for its `--keep`: `:keep [app.plugins/csv app.formats]`. `norn package`
  prints the shaking report and its warnings.

```sh
norn package
```

```text
made /Users/you/wc/dist/wc-0.1.0-macos-aarch64.tar.gz
made /Users/you/wc/dist/Word Count.app
      /Users/you/wc/dist/Word Count-0.1.0-macos-aarch64.zip
```

The command-line archive unpacks to a program and a folder for it:

```text
wc-0.1.0-macos-aarch64/
  bin/wc
  lib/wc/wc.nimg
  lib/wc/stopwords/en.txt
```

`bin/wc` is the launcher, a copy of `norn`. Run it, or link it into a
folder on your `PATH`, since it finds its image through a symbolic link
too. Every argument goes to `:main`, including a leading `-e`, and the
program runs in the caller's folder. To the person running it, it's an
ordinary program.

### Finding resources

A packaged program finds its resources through `norn.sys/app-resources`,
which returns the folder that holds the image and the resources, or `nil`
when the program isn't packaged. Falling back to the project folder lets
the same code run from source:

```clojure
(require '[norn.sys :as sys])

(defn stopwords-file []
  (io/path (or (sys/app-resources) ".") "stopwords" "en.txt"))

(stopwords-file) ; => "./stopwords/en.txt"
```

Don't use the folder the program runs in for resources, since that's the
caller's folder, and don't build paths from the image's location by hand.

### The macOS app

The app is `Display Name.app`, with an `Info.plist` built from
`norn.edn`, the launcher in `Contents/MacOS`, and the image, icon and
resources in `Contents/Resources`. It also comes zipped for sharing. The app
runs the same entry function as the command-line program, with no
arguments when it's opened from Finder.

`norn package` signs the app ad hoc, which needs no Apple Developer
account. It runs on the Mac that built it. Another Mac blocks a downloaded
copy the first time it opens. To open it there, try to open the app, then
go to System Settings > Privacy & Security and choose **Open Anyway**, or
run `xattr -dr com.apple.quarantine "Word Count.app"` before opening it.
A public release needs a Developer ID signature and notarization, which
`norn package` doesn't do.

### Native helpers

A library that runs a native program of its own declares it as a helper
in its `norn.edn`, with candidate paths in its folder. `{os}` and `{arch}`
are filled in, and `norn package` bundles the first candidate that exists.
Norn UI declares its renderer this way:

```edn
:helpers {"norn-ui-host" ["target/release/norn-ui-host" "bin/{os}-{arch}/norn-ui-host"]}
```

Helpers go beside the image in the archive and beside the launcher in the
app, and they're signed with it. If no candidate exists, `norn package`
stops and tells you which helper to build. For apps that use Norn UI, the
launcher points `NORN_UI_HOST` at the bundled renderer.

## Summary

- `(save-image path :entry #'main)` writes the running heap to a file.
  `norn --image path args…` starts it and calls the entry with the
  arguments, and an integer result is the exit status.
- Values, code, classes and parked fibers survive a restore. `:fibers
  :none` saves no fibers. `norn.sys` always describes the current machine.
- Files, sockets and other outside resources are dead after a restore.
  Register your own with `register-resource!`, then reconnect them in
  `on-image-restore` or lazily through `reconnect` and `ensure-live`.
- `norn build` makes a reproducible image from source, using `:main` in a
  project or `--entry` with files. `--image BASE --layered` makes a small
  image on a shared base.
- `--shake` (`:shake true` for `save-image` and `norn package`) leaves out
  what the entry can't reach, generic functions' methods for classes
  nothing makes, and the compiler when nothing evaluates. It warns about
  lookups by computed names, and `--keep` keeps what they need.
- `norn image info` describes an image, and `norn image diff` lists the vars
  two images differ in.
- `norn package` makes a command-line archive and, on macOS, an ad hoc
  signed app, with the image, a copy of `norn`, resources, and native
  helpers. Packaged code finds its resources with `norn.sys/app-resources`.

---

[Contents](README.md) · Previous: [26. Tools and performance](26-tools-and-performance.md) · Next: [28. Foreign functions](28-foreign-functions.md) <!-- nav -->
