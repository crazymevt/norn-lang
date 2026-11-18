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
//! - **processes**: `exit_signal`, `kill`, `raise`, and the signals Skein
//!   names (`SIGNALS`, `SIGINT`, `SIGQUIT`, `MAX_SIGNAL`, `catchable`,
//!   `default_disposition`);
//! - **pseudo-terminals**: `open_pty`, `enter_terminal_session`,
//!   `set_window_size`, `is_pty_closed`;
//! - **the host terminal**: `is_terminal`, `terminal_size`,
//!   `terminal_raw`, `terminal_restore`, `terminal_read`;
//! - **the machine**: `hostname`, `total_memory`, `page_size`,
//!   `local_offset`, `raise_file_limit`;
//! - **executable memory** for tier 1: `code_alloc`, `code_write`.
//!
//! What isn't behind this yet, and is still Unix's (a port has these to
//! design, not only to fill in):
//!
//! - the reactor's pipes (`reactor.rs` `Io::PipeIn`/`PipeOut` are
//!   `mio::unix::pipe`; `os.rs` makes a child's pipes and a
//!   pseudo-terminal's master into them, and `prims.rs` `%pipe` makes a
//!   pair);
//! - listening for signals (`os.rs` `listen` and `dump_on_sigquit`, on
//!   `signal_hook::iterator`);
//! - file descriptors 0 to 2 as what a terminal is (`sys.rs`).

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(crate) use unix::*;
