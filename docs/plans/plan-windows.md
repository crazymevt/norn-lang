# Plan: Windows

Drafted 2026-11-18, on a Mac, before anything of Skein had run on
Windows; it's the first part of runtime.md §12's M12 (platforms and
distribution). **Parts 0 to 6 were then done the same day on a Windows
11 ARM64 VM**: see Status, next. The sections after it are the draft as
it was written, kept for the reasoning; where the VM's work chose
otherwise, Status says so.

## Status (2026-11-18, from the VM; `../mmtk-core-windows/HANDOFF.md` has all of it)

On Windows 11 ARM64, a release build, with `clang` and `git` on the
`PATH`:

| Check | Result |
|---|---|
| MMTk's layer: its own tests, MMTk's unit tests, the mock tests | 11 of 11; 544 of 544; 39 pass, 2 fail for reasons not the layer's |
| The 51 suites of `tests/skein`, tier 0, tier 1, and tier 0 with collections forced | 51 pass in each |
| The lang-tests, six programs in three modes | All pass, outputs as recorded on the Mac |
| On macOS and Linux, the same changes | See the pull request that brought them in |

What it took, beyond filling in `platform/windows.rs`:

- **Two bugs in the MMTk layer**: the heap's address (2 TB is among a
  Windows process's own allocations: 4 launches of 300 died; it's 8 TB
  there now) and `munmap` across more than one allocation.
- **The seam changed shape once**, as section 3 foresaw:
  `platform::Child`. And `sync_dir` and `connect_settled` joined it
  (Windows can't flush a directory; a connecting socket there reports a
  peer address before it's connected).
- **Paths**: `SKEIN_PATH` split as `PATH` is, `\\?\` taken off
  canonical paths, `fs/glob` with either separator, and several places
  in the libraries and the lang-tests that split on `/`.
- **The FFI**: Windows arm64's variadic calls, and Microsoft's bit-field
  layout.

**Decided on the VM, and confirmed by the user on 2026-11-19** (the
plan had left them open):

- **Signals**: `:int` Ctrl-C, `:break` Ctrl-Break (new), `:term` the
  console closing or the session ending, `:winch` the console's size,
  `:kill` sent only. Any other name is an error.
- **`:winch` polls** the size four times a second while anyone listens.
- **`proc/kill` sends only `:term` and `:kill`** on Windows (the draft
  proposed `:int` as a Ctrl-Break to the child's process group).
- **Pipes are `mio::windows::NamedPipe`** (A in section 1). A child's
  pipes are the standard library's, which happen to be overlapped named
  pipes on our side: its private detail.
- **`proc/sh` needed no decision**: it runs a program directly.

**Open when the VM's work was done, and settled since where it says:**

1. **How Skein depends on a patched MMTk: decided 2026-11-19 (the
   user), as it is.** The patch stays in this repository
   (`tools/windows/mmtk-0.33.0.patch`) and is made again as needed: at
   each MMTk upgrade, or when the layer needs a fix. Not a branch of
   mmtk-core, and not sent upstream. `tools/windows/mmtk.sh` makes
   `target/mmtk-windows` from the MMTk on crates.io and it, and the
   build is told to use that (`cargo --config
   "patch.crates-io.mmtk.path='target/mmtk-windows'" build`). Skein's
   manifests and `Cargo.lock` are as they were, so that build can't be
   `--locked`.
2. **CI on Windows arm64** (part 7) is a job that can't fail the run
   yet (`.github/workflows/ci.yml`, `windows`): the suites, the suites
   with collections forced, the lang-tests, and `cargo test`, which had
   never run on Windows (images, the 10,000 connections, `skein
   --server`, `skein lsp`, `skein new`, `skein package`). Its first run
   is the pull request that added it. A clone on Windows needs `git
   config core.autocrlf false` first: the job sets it, and nothing in
   the repository does.
3. **x64** (part 8): **done 2026-11-19**, written on the Mac and run
   only by CI (`windows-x64`, on `windows-latest`, another job that
   can't fail the run). MMTk's Windows layer ran there unchanged.
   - **The FFI** has Win64's rules: a struct of 1, 2, 4 or 8 bytes goes
     and comes back as an integer of that size (no HFAs: two floats go in
     an integer register), any other size by reference to a copy and
     comes back through a hidden pointer passed first; a float among a
     variadic function's variadic arguments goes as its bits in the
     integer register. `ffi_fixture.c` gained structs of 3 and 4 bytes
     and one after four arguments, the cases Win64 sets apart.
   - **Tier 1 probes the stack** on Windows, both machines: Cranelift
     emits no stack probes unless asked, and a frame larger than a page
     that skips Windows' guard page is an access violation. The probes
     are inline (an outline one is a call to `__chkstk`, a relocation the
     installed code doesn't resolve). Small frames get none.
     `jit::tests` runs a 256 KB frame on a fresh thread.
   - **CI's results**, its first run (44524743638): every step green.
     The suites, 342 tests, at tier 1 and at tier 0 with collections
     forced; the lang-tests, six programs in three modes; and `cargo
     test` (the workspace but jit-spike). The job puts clang on the
     PATH for `ffi_test`'s fixture; ring builds as cc chooses for MSVC.
4. **Not tried**: the REPL by hand at a console; a real Ctrl-Break, the
   console closing, the session ending.
5. **A source file that begins with a byte-order mark didn't load**
   (found there; Windows tools write one). **Fixed 2026-11-19:** the
   reader starts after one, and counts line 1's columns from there.
6. **The oldest Windows supported: decided 2026-11-19 (the user),
   Windows 10 version 1809**, ConPTY's own minimum (on ARM, Windows 11
   in practice).
7. **From skein-ui's handoff** (2026-11-19), **fixed the same day:**
   - **TLS**: Windows installs only some root certificates and fetches
     the rest when its own verifier meets one, so a server with an
     issuer not yet fetched (`https://example.com`'s) failed with
     `UnknownIssuer`. On Windows, a connection with no `:ca` is verified
     by Windows (`rustls-platform-verifier`), as schannel's users are;
     elsewhere it's the installed roots, as before.
   - **`skein package`'s helpers**: on Windows a helper's candidate is
     found as NAME.exe too, goes into the archive as NAME.exe, and the
     launcher finds `skein-ui-host.exe` beside its image.
   - Not changed: the command-line archive is a `.tar.gz` everywhere
     (Windows 10 1809 and later have `tar`). **A Windows layout came
     2026-11-20**: `skein package --windows`, below.
8. **`skein package --windows`** (2026-11-20, made on the Mac, and run
   on Windows with Linnet's package): see [Packaging for
   Windows](#packaging-for-windows).

## Packaging for Windows

`skein package --windows` (`lib/skein/package.skn`, documented in
[tooling.md](../tooling.md#windows-apps)) makes
`Display Name-VERSION-windows-ARCH.zip` on any OS. The zip holds
`Display Name/bin/NAME.exe` and `Display Name/lib/NAME/`, laid out as the
command-line archive is, because that's where the launcher looks
(`find_app`, `../lib/NAME/NAME.skimg`). `NAME.exe` is a Windows `skein.exe`
edited by **`skein.pe`** (`lib/skein/pe.skn`), a PE reader and editor
written in Skein:

- **The subsystem.** A windowed app's exe has the optional header's
  Subsystem changed from 3 (console) to 2 (Windows GUI), so Explorer starts
  it without a console window. The default is windowed when skein-ui's
  renderer is bundled (`:package :windows :gui` decides otherwise).
  `skein-ui-host.exe` is a console program too (no `windows_subsystem` in
  its crate), and a console child of a process with no console gets a new
  console window. So a windowed app's console helpers are marked windowed
  as well. Pipes still work: Rust's `Command` always passes the child's
  handles in `STARTUPINFO` (`STARTF_USESTDHANDLES`), and a GUI child gets
  them from `GetStdHandle` like any other. skein-ui starts the renderer
  with `:err :inherit`, so the renderer's standard error is the app's,
  which in a windowed app is nothing. Rust's standard library treats a
  missing standard handle as a sink.
- **The resources.** The icon (`:windows :icon`, an `.ico`, or a PNG of
  256 pixels or less) becomes `RT_ICON`s and an `RT_GROUP_ICON`. A
  VERSIONINFO built from `:version` and `:display-name` becomes the
  `RT_VERSION`. Resources the exe has already (a manifest) are kept.
  `skein.pe` writes them as a new `.rsrc` section after the last one
  (`.reloc`, in MSVC's output), in the free room the linker leaves after
  the section table. If the old `.rsrc` is the last section, it's replaced
  in place. Nothing else moves, so code, data, relocations and the
  exception tables stay where the linker put them. The new section's
  header is written, then NumberOfSections, SizeOfImage,
  SizeOfInitializedData and the resource data directory are updated. A
  signature (Authenticode) is dropped because it would no longer match. A
  checksum is computed again if the exe had one (MSVC's linker writes 0
  without `/RELEASE`, and Windows checks it only for drivers).
- **Its tests** (`tests/skein/pe_test.skn`, `crates/skein/tests/package.rs`)
  use three real PEs of a few kilobytes, linked by `rust-lld` with no
  Windows SDK (`tests/skein/pe`, `build.sh` rebuilds them): ARM64 and x64
  with `.reloc` last, and ARM64 with a manifest in a `.rsrc` before
  `.reloc`. Among the checks: the resources read back as written; the old
  sections' bytes are unchanged; a second edit replaces the `.rsrc`; and
  removing the resources gives back the original byte for byte.

**Checked on the Mac (2026-11-20):** an independent PE reader (Python
stdlib, outside the repository) found no errors in the output: alignment,
SizeOfImage, overlapping sections, the resource tree's order and offsets,
data in its section, and each VERSIONINFO node's length. The checksum
matches the one stored in a signed, MSVC-linked x64 program (VMware's
`VMwareToolsUpgrader.exe`). Editing that program dropped its signature,
appended a second `.rsrc` after its `.reloc`, kept its manifest and version
resources, and gave a checksum that reader agrees with. The VERSIONINFO
`skein.pe` writes for that program's strings is byte for byte the one
`rc.exe` wrote. Linnet's sample (the ARM64 test program standing in for
`skein.exe` and for `skein-ui-host.exe`, since there's no Windows build of
either on the Mac) has subsystem 2, its nine icon images identical to
`assets/linnet.ico`'s, the version strings, a windowed helper, and a zip
that `unzip -t` passes.

**Not checked: anything on Windows.** On the VM, check that:

- a `skein package --windows` of Linnet starts with no console window,
  either for itself or for `skein-ui-host.exe`;
- Explorer shows the icon and Properties > Details shows the version;
- the edited `hello-arm64.exe` still runs and prints its line;
- an app built on the Mac loads its image in a `skein.exe` built on
  Windows from the same commit. The build fingerprint (`image::build_id`)
  hashes only sources and tables, nothing per OS, so it should, but that
  hasn't been tried.

The things to know:

- **A windowed app's errors go nowhere.** If its image doesn't load (a
  launcher from another build, say), `skein.exe` prints why to a standard
  error that isn't there, and the app just doesn't open. To find out why,
  run the same `NAME.exe` from a console. It prints nothing there either,
  because a GUI program doesn't attach to its parent's console. Instead,
  package with `:gui false`, or run `skein.exe` on the image. A later
  improvement: the launcher shows a message box when it fails with no
  console.
- **Launcher and packaging `skein` must be the same build.** Nothing
  checks this before Windows does.
- **SmartScreen** asks before opening an unsigned program downloaded from
  the internet (tooling.md says how to get past it). Code signing needs a
  certificate and `signtool` on Windows. Signing after `skein.pe`'s edit
  is fine, because the edit comes first.
- **What `skein.pe` refuses**: a program with data after its sections
  other than a signature (an installer's payload, a MinGW symbol table),
  or with no room for another section header. Neither applies to
  `skein.exe` from MSVC.
- `skein.zip` writes no Unix permissions, so `unzip` on macOS or Linux
  makes files with mode 000. Windows doesn't read those bits.

The alternatives, not taken: a prebuilt launcher stub with an icon slot
(one more binary to build and keep in step with `skein.exe`), and `rcedit`
(Windows only, an external tool).

## The draft

The target, in order: **Windows on ARM first** (the user has a VM), then
x64. ARM first because tier 1's arm64 back end and the arm64 FFI carry
over; x64 needs the Win64 calling convention in both. Windows isn't
supported until x64 works: nearly every Windows machine is one.

## Where things stand

- **The collector.** MMTk 0.33.0, the latest release, has no Windows
  support; upstream's pull request for it (mmtk-core #1418) has been
  still since February 2026. A Windows layer for MMTk's `util/os` was
  written on 2026-11-18 and compiles for both Windows targets; it has
  never run. It's outside this repository, in `../mmtk-core-windows`
  (with `HANDOFF.md`, and `README-WINDOWS.md` for the steps). Nothing of
  Skein runs on Windows until this does: it's part 0.
- **Skein's own system calls** are behind one module since 2026-11-18,
  `crates/skein-vm/src/platform/`: `mod.rs` lists what a platform
  provides and `unix.rs` provides it. A port is a `windows.rs` beside
  it. Nothing else in the runtime calls the system through `libc`, or
  names `mio::unix`, `std::os::unix` or `signal_hook`.
- **What's already portable**, through crates that know Windows: sockets
  and the reactor (`mio`), TLS (`rustls`, `ring`), loading libraries
  (`libloading`), file watching (`notify`), the REPL's line editor
  (`rustyline`), Ctrl-C in the `skein` command (`ctrlc`), tier 1's code
  generator (Cranelift has Windows arm64 and x64).
- **Skein can't be compile-checked for Windows from a Mac**: `ring`
  builds C. On Windows, `cargo check -p skein-vm` with MMTk patched to
  the local copy gives the real list of what's missing, and is the first
  thing to run there.

## What `platform/windows.rs` has to provide

Easy, and enough for `skein -e` and the suites that touch no processes:

| Unix | Windows |
|---|---|
| `hostname` | `GetComputerNameExW` |
| `total_memory` | `GlobalMemoryStatusEx` |
| `page_size` | `GetSystemInfo` (4 KB; note the 64 KB allocation granularity) |
| `local_offset` | `GetTimeZoneInformationForYear` and the bias for that moment, or `SystemTimeToTzSpecificLocalTime` and the difference |
| `raise_file_limit` | nothing to do |
| `code_alloc` | `VirtualAlloc(MEM_RESERVE \| MEM_COMMIT, PAGE_EXECUTE_READWRITE)` |
| `code_write` | copy, then `FlushInstructionCache` (needed on arm64) |
| `is_terminal` | `GetConsoleMode` succeeds on `GetStdHandle(n)` |
| `terminal_size` | `GetConsoleScreenBufferInfo`'s window |
| `terminal_raw` / `terminal_restore` | `SetConsoleMode`: off `ENABLE_LINE_INPUT`, `ENABLE_ECHO_INPUT`, `ENABLE_PROCESSED_INPUT`; on `ENABLE_VIRTUAL_TERMINAL_INPUT`, and `ENABLE_VIRTUAL_TERMINAL_PROCESSING` on output |
| `terminal_read` | `WaitForSingleObject` on the input handle with the timeout, then `ReadFile` |

`skein.terminal` writes and reads escape sequences; with the two
virtual-terminal modes on, a Windows 10 console speaks them, so the
library above the primitives should carry over.

The three that are a different shape on Windows, not only other names:

### 1. Pipes

The reactor needs each end of a pipe to be a `mio::event::Source` that
reads or writes without blocking. On Unix that's `mio::unix::pipe`. A
Windows anonymous pipe (what `std::process` gives a child) can't be
used with overlapped I/O, so it can't be polled.

- **A. Named pipes** (recommended). `mio::windows::NamedPipe` is a
  pollable source over a named pipe opened for overlapped I/O. For a
  child's stream: make a uniquely named pipe, keep the overlapped server
  end as `PipeIn`/`PipeOut`, open the client end as an ordinary
  inheritable handle and give it to the child (`Stdio::from` an
  `OwnedHandle`). `pipe()` is the same with both ends kept. To check:
  that `NamedPipe`'s readiness fits the reactor's edge-triggered
  counting (it buffers internally and has one operation outstanding at a
  time), and what a closed other end looks like (`ERROR_BROKEN_PIPE`,
  which `prims.rs` must take as end of input, as it takes 0 bytes now).
- **B. A thread for each pipe**, blocking in `ReadFile` and feeding the
  reactor through a `mio::Waker`. Simple and certain, and a thread for
  every stream of every child. The fallback if A's readiness doesn't
  fit.

### 2. Signals

Windows has no signals between processes. It has console control
events, delivered to a handler on a thread of its own
(`SetConsoleCtrlHandler`): Ctrl-C, Ctrl-Break, and the console closing,
logging off, shutting down.

- **Listening** (`on_signals`, `default_disposition`, `default_when`):
  one control handler, installed once, that feeds the listeners.
  Returning `FALSE` from it passes the event to the next handler, and
  the last is the system's (the process ends): that's "do the default
  when nobody is listening", with nothing to emulate.
- **The names.** Proposed: `:int` is Ctrl-C, `:break` is new and is
  Ctrl-Break, `:term` is the close, logoff and shutdown events, `:kill`
  can be sent and not caught. `:hup`, `:usr1`, `:usr2`, `:winch`,
  `:pipe` and the rest don't exist: listening for one is an error that
  says so, as an unknown name is now. **[to decide]**
- **Sending** (`kill`): `:kill` is `TerminateProcess`. `:int` to a child
  is `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT)` to its process group,
  which needs the child started with `CREATE_NEW_PROCESS_GROUP`, and
  arrives as a break, not a Ctrl-C. Anything else: an error.
- **`exit_signal`** is always None: a Windows process has an exit code
  and nothing else. `{:exit n :signal nil}` always.
- **`:winch`**: the console's size changing isn't a control event. It
  comes as an input record (`WINDOW_BUFFER_SIZE_EVENT`), or by asking
  the size again. `skein.terminal` listens for `:winch` today: on
  Windows it would poll the size. **[to decide: poll, or read input
  records]**
- Signal numbers are in Skein code too (`skein.signal`, and
  `%process-kill` takes one): the table of names is the platform's, so
  those carry over; anything that assumes Unix's numbers doesn't.

### 3. Pseudo-terminals

Unix: `openpty` gives two descriptors, and the child is an ordinary
process with the slave as its standard streams. Windows (10, 1809 and
later): `CreatePseudoConsole(size, input, output)` over two pipes gives
a handle, and the child must be started with that handle in its
attribute list (`PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`), which
`std::process::Command` can't do.

- `spawn_on_pty` on Windows calls `CreateProcessW` itself. So what it
  returns can't be a `std::process::Child`: **`platform` needs a `Child`
  type of its own** (std's on Unix, a process handle on Windows) with
  `id`, `wait` and `kill`, and `reactor.rs`'s `Io::Child` holds that.
  This is the one change to the seam's shape that's already known.
- `set_window_size` is `ResizePseudoConsole`, on the console's handle,
  not on a pipe: `PipeOut` on Windows carries it, or the resize goes
  through the process handle. `is_pty_closed` is `ERROR_BROKEN_PIPE`.
- What the child sees is a console, and what comes out of the pipe is
  escape sequences: `skein.terminal`'s tests against a pseudo-terminal
  should carry over; `pty_test.skn`'s use of `sh`, `stty` and `zsh`
  doesn't.

## Besides the platform module

- **The FFI** (`ffi.rs`). arm64: Windows follows the standard arm64
  rules that Linux does, except for variadic functions (every variadic
  argument goes as if it had no floating-point registers). x64: Win64 is
  its own convention (four register arguments, shadow space, structs by
  reference above 8 bytes): new code, for part 8. `long` is 32 bits on
  Windows on both: `:long` and `:ulong` in `skein.ffi` must follow.
- **Tier 1** (`jit.rs`). Helpers are called with
  `isa.default_call_conv()`, which Cranelift picks for the target, and
  native functions call each other by tail call: to check on Windows
  arm64. Windows wants unwind tables registered for generated code
  (`RtlAddFunctionTable`) if anything ever unwinds through it; Skein
  returns errors as values and doesn't, so this is left until a debugger
  or a crash handler needs it.
- **The standard library and its tests.** Paths (`\`, drive letters,
  `skein.io/path`), the home directory (`USERPROFILE`), line endings in
  files the tests compare, `proc/sh` (there's no `sh`: `cmd /c` or
  nothing), executable names (`.exe`), read-only as the one permission,
  SQLite and other libraries by name (`.dll`). The suites that start
  `sh`, `zsh`, `stty` or send signals need Windows versions or a skip
  that says why.
- **CI.** GitHub hosts Windows arm64 runners (`windows-11-arm`); to
  confirm they're free for public repositories before relying on it.

## Parts

| # | Part | Done when |
|---|---|---|
| 0 | **MMTk on Windows.** Run `../mmtk-core-windows` on the VM: its layer's own tests, MMTk's unit tests, the mock tests | They pass but for the expected ones; what the layer got wrong is fixed |
| 1 | **It compiles.** `skein-rt`, then `skein-vm` with a `windows.rs` of the easy table above and `todo!()` for the rest | `cargo build` on Windows arm64 |
| 2 | **It runs.** `skein -e`, the REPL, the suites with no processes, pipes or signals | Those suites pass at tier 0, then tier 1; a collection under `SKEIN_GC_STRESS` |
| 3 | **Files and paths** in the standard library | `io`, `fs` and `sys` suites pass |
| 4 | **Pipes and processes** (A above) | `skein.process` without pseudo-terminals; the suites that start a child `skein` |
| 5 | **Signals** as decided above | `skein.signal`'s tests, in their Windows form |
| 6 | **Pseudo-terminals** (ConPTY, and `platform::Child`) | `pty_test` and `terminal_test` in their Windows form |
| 7 | **CI** on Windows arm64, and the lang-tests | Every suite and the lang-tests green there |
| 8 | **x64**: Win64 in the FFI and tier 1's helpers | The same, on `x86_64-pc-windows-msvc` |

Parts 1 to 3 need nothing but the easy table. Each of 4, 5 and 6 is one
of the three designs above, and they don't depend on each other.

## To decide

(As drafted. Status, above, says how each was settled: by the VM's
work, and by the user on 2026-11-19.)

1. **How Skein depends on a patched MMTk**: a `[patch.crates-io]` on a
   path or a git branch, for Windows only or for every platform; and
   whether to offer the layer upstream (recommended: yes, reviving
   #1418 on MMTk's new layout).
2. **The signal names on Windows** (section 2).
3. **`:winch`**: poll the size, or read input records.
4. **What `proc/sh` means** where there's no `sh`.
5. **The oldest Windows supported.** ConPTY needs Windows 10 1809;
   Windows on ARM in practice means 11.
