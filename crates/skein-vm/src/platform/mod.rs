//! What Skein asks of the operating system directly, in one place.
//!
//! Most of the runtime reaches the system through Rust's standard library
//! and a few crates that already know each platform (`mio`, `libloading`,
//! `rustls`, `notify`). The rest is here: each platform is one file that
//! provides the same functions, and nothing outside this module calls
//! the system through `libc` (`ffi.rs` uses its `memcpy`, `calloc` and
//! `free`, which are C's and everywhere). A port adds a file beside
//! `unix.rs`.
//!
//! What a platform provides:
//!
//! - **processes**: `Child` (what the reactor holds: `id`, `wait`, and
//!   `From<std::process::Child>`), `exit_signal`, `kill`, `raise`, and the signals Skein
//!   names (`SIGNALS`, `SIGINT`, `SIGQUIT`, `MAX_SIGNAL`, `catchable`);
//! - **listening for signals**: `on_signals` (a thread that's told of each)
//!   and its `SignalHandle`, `default_disposition`, `default_when`;
//! - **pipes** the reactor can poll: the types `PipeIn` and `PipeOut`
//!   (each a `mio::event::Source` that reads or writes without blocking),
//!   `pipe`, and a child's ends, `child_input`, `child_output`,
//!   `child_errors`;
//! - **pseudo-terminals**: `spawn_on_pty` (and `PtyError`),
//!   `set_window_size`, `is_pty_closed`;
//! - **the host terminal**: `is_terminal`, `terminal_size`,
//!   `terminal_raw`, `terminal_restore`, `terminal_read`;
//! - **the machine**: `hostname`, `total_memory`, `page_size`,
//!   `local_offset`, `raise_file_limit`, `sync_dir`, `connect_settled`;
//! - **executable memory** for tier 1: `code_alloc`, `code_write`.
//!
//! What's left outside it that a port must still look at: `sys.rs` takes
//! a terminal to be descriptor 0, 1 or 2, and signals are numbers
//! everywhere (`os.rs`, `prims.rs`, and `skein.signal` in Skein).

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(crate) use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::*;
