# Plan: Windows

Drafted 2026-11-18, on a Mac, before anything of Skein has run on
Windows. **Nothing here is decided, and none of it has been tried.** The
Windows API names are from memory and each needs checking against its
documentation as it's used. It's the first part of runtime.md §12's M12
(platforms and distribution).

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

1. **How Skein depends on a patched MMTk**: a `[patch.crates-io]` on a
   path or a git branch, for Windows only or for every platform; and
   whether to offer the layer upstream (recommended: yes, reviving
   #1418 on MMTk's new layout).
2. **The signal names on Windows** (section 2).
3. **`:winch`**: poll the size, or read input records.
4. **What `proc/sh` means** where there's no `sh`.
5. **The oldest Windows supported.** ConPTY needs Windows 10 1809;
   Windows on ARM in practice means 11.
