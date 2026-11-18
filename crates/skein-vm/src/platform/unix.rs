//! Unix (macOS and Linux): see `platform/mod.rs` for what this provides.

use std::io::{Error, Result};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::{Command, ExitStatus};

// ---- processes and signals ----------------------------------------------------------------------

/// The signal that ended a process, if one did.
pub(crate) fn exit_signal(st: &ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    st.signal()
}

/// Send signal `sig` to process `pid`.
pub(crate) fn kill(pid: i64, sig: i32) -> Result<()> {
    // SAFETY: kill has no memory preconditions.
    let r = unsafe { libc::kill(pid as libc::pid_t, sig) };
    if r == 0 { Ok(()) } else { Err(Error::last_os_error()) }
}

/// Send signal `sig` to this process.
pub(crate) fn raise(sig: i32) {
    // SAFETY: raise has no memory preconditions.
    unsafe { libc::raise(sig) };
}

/// The signals Skein names, both ways (a keyword's name, its number).
pub(crate) const SIGNALS: &[(&str, i32)] = &[
    ("int", libc::SIGINT),
    ("term", libc::SIGTERM),
    ("hup", libc::SIGHUP),
    ("kill", libc::SIGKILL),
    ("quit", libc::SIGQUIT),
    ("usr1", libc::SIGUSR1),
    ("usr2", libc::SIGUSR2),
    ("winch", libc::SIGWINCH),
    ("pipe", libc::SIGPIPE),
    ("alrm", libc::SIGALRM),
    ("cont", libc::SIGCONT),
    ("stop", libc::SIGSTOP),
    ("tstp", libc::SIGTSTP),
    ("ttin", libc::SIGTTIN),
    ("ttou", libc::SIGTTOU),
    ("abrt", libc::SIGABRT),
    ("segv", libc::SIGSEGV),
    ("bus", libc::SIGBUS),
    ("fpe", libc::SIGFPE),
    ("ill", libc::SIGILL),
    ("trap", libc::SIGTRAP),
];

pub(crate) const SIGINT: i32 = libc::SIGINT;
pub(crate) const SIGQUIT: i32 = libc::SIGQUIT;

/// Signal numbers are below this on every Unix Skein runs on.
pub(crate) const MAX_SIGNAL: usize = 65;

/// Can a program listen for `sig`? Not for those that stop or end the
/// process, or mean it went wrong: they can't be caught, or the runtime
/// keeps them.
pub(crate) fn catchable(sig: i32) -> bool {
    !matches!(sig, libc::SIGKILL | libc::SIGSTOP | libc::SIGSEGV | libc::SIGBUS | libc::SIGILL | libc::SIGFPE | libc::SIGTRAP)
}

/// Is `sig` handled as the system would by default (nothing has installed
/// a handler for it, or ignored it)?
pub(crate) fn default_disposition(sig: i32) -> Result<bool> {
    // SAFETY: a null new action only reads the current one into `old`.
    let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
    if unsafe { libc::sigaction(sig, std::ptr::null(), &mut old) } != 0 {
        return Err(Error::last_os_error());
    }
    Ok(old.sa_sigaction == libc::SIG_DFL)
}

// ---- pseudo-terminals ---------------------------------------------------------------------------

/// A new pseudo-terminal of `rows` by `cols`: its master (closed in a
/// child) and its slave.
pub(crate) fn open_pty(rows: u16, cols: u16) -> Result<(OwnedFd, OwnedFd)> {
    let (mut master, mut slave): (libc::c_int, libc::c_int) = (-1, -1);
    let mut ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: openpty writes the two descriptors; the rest may be null.
    let r = unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &mut ws) };
    if r != 0 {
        return Err(Error::last_os_error());
    }
    // SAFETY: both are open descriptors we now own.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    // SAFETY: fcntl on an open descriptor.
    unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
    Ok((master, slave))
}

/// Have the child `c` start a session of its own with its standard input
/// (a pseudo-terminal's slave) as its controlling terminal, and the
/// signals the runtime handles or ignores back at their defaults (an
/// ignored one would stay ignored through exec, and ^C would do nothing).
pub(crate) fn enter_terminal_session(c: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: between fork and exec only async-signal-safe calls.
    unsafe {
        c.pre_exec(|| {
            for sig in [
                libc::SIGINT,
                libc::SIGQUIT,
                libc::SIGTSTP,
                libc::SIGTTIN,
                libc::SIGTTOU,
                libc::SIGHUP,
                libc::SIGTERM,
                libc::SIGPIPE,
                libc::SIGCHLD,
                libc::SIGWINCH,
                libc::SIGALRM,
                libc::SIGUSR1,
                libc::SIGUSR2,
            ] {
                libc::signal(sig, libc::SIG_DFL);
            }
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Tell the pseudo-terminal whose master is `master` its new size (its
/// child gets SIGWINCH).
pub(crate) fn set_window_size(master: &impl AsRawFd, rows: u16, cols: u16) -> Result<()> {
    let ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: TIOCSWINSZ on an open descriptor, with a winsize.
    if unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ as _, &ws) } != 0 {
        return Err(Error::last_os_error());
    }
    Ok(())
}

/// Is `e` what reading a pseudo-terminal gives once its child has gone
/// (the end of its output)?
pub(crate) fn is_pty_closed(e: &Error) -> bool {
    e.raw_os_error() == Some(libc::EIO)
}

// ---- the host terminal --------------------------------------------------------------------------

/// Terminal settings saved by `terminal_raw`, restored by
/// `terminal_restore` or, for any still raw, when the process exits.
static SAVED_TERMIOS: std::sync::Mutex<Vec<(libc::c_int, libc::termios)>> = std::sync::Mutex::new(Vec::new());

/// Is descriptor `fd` a terminal?
pub(crate) fn is_terminal(fd: i32) -> bool {
    // SAFETY: isatty only inspects the descriptor.
    unsafe { libc::isatty(fd) == 1 }
}

/// `(rows, cols)` of the terminal on `fd`, or None when it isn't one (or
/// doesn't say).
pub(crate) fn terminal_size(fd: i32) -> Option<(u16, u16)> {
    let mut ws = libc::winsize { ws_row: 0, ws_col: 0, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: TIOCGWINSZ writes a winsize.
    let r = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ as _, &mut ws) };
    (r == 0 && ws.ws_col > 0 && ws.ws_row > 0).then_some((ws.ws_row, ws.ws_col))
}

extern "C" fn restore_terminals() {
    if let Ok(saved) = SAVED_TERMIOS.lock() {
        for (fd, t) in saved.iter().rev() {
            // SAFETY: tcsetattr with settings tcgetattr gave for this descriptor.
            unsafe { libc::tcsetattr(*fd, libc::TCSANOW, t) };
        }
    }
}

/// Put the terminal on `fd` in raw mode: bytes as typed, no echo, no line
/// editing, no signals from Ctrl-C or Ctrl-Z. Output processing stays on,
/// so a newline still returns the cursor. True if this changed it, false
/// if it was already raw (from an earlier call).
pub(crate) fn terminal_raw(fd: i32) -> Result<bool> {
    static AT_EXIT: std::sync::Once = std::sync::Once::new();
    let mut saved = SAVED_TERMIOS.lock().unwrap();
    if saved.iter().any(|(f, _)| *f == fd) {
        return Ok(false);
    }
    // SAFETY: termios is plain data; tcgetattr fills it.
    let mut orig: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut orig) } != 0 {
        return Err(Error::last_os_error());
    }
    let mut raw = orig;
    // SAFETY: cfmakeraw edits the struct it's given.
    unsafe { libc::cfmakeraw(&mut raw) };
    raw.c_oflag |= libc::OPOST | libc::ONLCR;
    raw.c_cc[libc::VMIN] = 1;
    raw.c_cc[libc::VTIME] = 0;
    // SAFETY: tcsetattr with a filled-in termios.
    if unsafe { libc::tcsetattr(fd, libc::TCSADRAIN, &raw) } != 0 {
        return Err(Error::last_os_error());
    }
    saved.push((fd, orig));
    // SAFETY: registers a function with no captured state.
    AT_EXIT.call_once(|| unsafe {
        libc::atexit(restore_terminals);
    });
    Ok(true)
}

/// Undo `terminal_raw` on `fd`: true if it was raw.
pub(crate) fn terminal_restore(fd: i32) -> Result<bool> {
    let mut saved = SAVED_TERMIOS.lock().unwrap();
    let Some(i) = saved.iter().position(|(f, _)| *f == fd) else { return Ok(false) };
    let (_, t) = saved.remove(i);
    // SAFETY: tcsetattr with settings tcgetattr gave for this descriptor.
    if unsafe { libc::tcsetattr(fd, libc::TCSADRAIN, &t) } != 0 {
        return Err(Error::last_os_error());
    }
    Ok(true)
}

/// Up to `max` bytes from `fd` as they arrive, waiting at most
/// `timeout_ms` (negative: no limit) for the first: the bytes (none if
/// none came in time), or None at the end of input. Blocks.
pub(crate) fn terminal_read(fd: i32, max: usize, timeout_ms: i32) -> Result<Option<Vec<u8>>> {
    loop {
        let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        // SAFETY: poll on one pollfd.
        let r = unsafe { libc::poll(&mut p, 1, timeout_ms) };
        if r < 0 {
            let e = Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        if r == 0 {
            return Ok(Some(Vec::new()));
        }
        let mut buf = vec![0u8; max];
        // SAFETY: read into a buffer of max bytes.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), max) };
        if n < 0 {
            let e = Error::last_os_error();
            if matches!(e.kind(), std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock) {
                continue;
            }
            return Err(e);
        }
        buf.truncate(n as usize);
        return Ok((n > 0).then_some(buf));
    }
}

// ---- the machine ----------------------------------------------------------------------------------

pub(crate) fn hostname() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: buf is writable for its length.
    let r = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if r != 0 {
        return None;
    }
    let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..n]).into_owned())
}

/// The machine's memory in bytes.
pub(crate) fn total_memory() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        let mut v: u64 = 0;
        let mut len = std::mem::size_of::<u64>();
        // SAFETY: v and len describe a valid buffer for hw.memsize.
        let r = unsafe {
            libc::sysctlbyname(c"hw.memsize".as_ptr(), &mut v as *mut u64 as *mut libc::c_void, &mut len, std::ptr::null_mut(), 0)
        };
        (r == 0).then_some(v)
    }
    #[cfg(target_os = "linux")]
    {
        // SAFETY: sysinfo fills the struct.
        let mut si: libc::sysinfo = unsafe { std::mem::zeroed() };
        let r = unsafe { libc::sysinfo(&mut si) };
        (r == 0).then_some(si.totalram as u64 * si.mem_unit as u64)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// The size of a page of memory in bytes.
pub(crate) fn page_size() -> usize {
    // SAFETY: sysconf has no preconditions.
    unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize }
}

/// This machine's UTC offset in seconds at `epoch_secs` (time zone rules,
/// daylight saving included).
pub(crate) fn local_offset(epoch_secs: i64) -> i64 {
    let t = epoch_secs as libc::time_t;
    // SAFETY: localtime_r fills tm from t.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { !libc::localtime_r(&t, &mut tm).is_null() };
    if ok { tm.tm_gmtoff } else { 0 }
}

/// Let this process have as many open files as the system allows.
pub(crate) fn raise_file_limit() {
    // SAFETY: getrlimit/setrlimit only read and write the struct.
    unsafe {
        let mut r: libc::rlimit = std::mem::zeroed();
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut r) != 0 {
            return;
        }
        let want = r.rlim_max;
        // macOS refuses RLIM_INFINITY; its real cap is kern.maxfilesperproc.
        #[cfg(target_os = "macos")]
        let want = {
            let mut cap: libc::c_int = 0;
            let mut len = std::mem::size_of::<libc::c_int>();
            if libc::sysctlbyname(
                c"kern.maxfilesperproc".as_ptr(),
                &mut cap as *mut _ as *mut libc::c_void,
                &mut len,
                std::ptr::null_mut(),
                0,
            ) == 0
            {
                want.min(cap as libc::rlim_t)
            } else {
                want
            }
        };
        if want > r.rlim_cur {
            r.rlim_cur = want;
            libc::setrlimit(libc::RLIMIT_NOFILE, &r);
        }
    }
}

// ---- executable memory, for tier 1 ----------------------------------------------------------------

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn sys_icache_invalidate(start: *mut libc::c_void, len: libc::size_t);
}

// (arm64 keeps instruction and data caches apart: code just written must
// be flushed from one to the other before it runs, or stale bytes run)
#[cfg(all(target_arch = "aarch64", not(target_os = "macos")))]
unsafe extern "C" {
    fn __clear_cache(start: *mut libc::c_char, end: *mut libc::c_char);
}

/// `size` bytes of memory that code can be written to and run from
/// (MAP_JIT on macOS: runtime.md §7.4). Never freed.
pub(crate) fn code_alloc(size: usize) -> Option<usize> {
    // SAFETY: a fresh anonymous mapping.
    let p = unsafe {
        #[cfg(target_os = "macos")]
        let flags = libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_JIT;
        #[cfg(not(target_os = "macos"))]
        let flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS;
        libc::mmap(std::ptr::null_mut(), size, libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC, flags, -1, 0)
    };
    (p != libc::MAP_FAILED).then_some(p as usize)
}

/// Copy `bytes` to `at`, in memory from `code_alloc`, and make the `len`
/// bytes there ready to run.
///
/// # Safety
/// [at, at + len) is within memory `code_alloc` gave, and nothing runs it
/// meanwhile. On macOS this thread toggles W^X around the copy.
pub(crate) unsafe fn code_write(at: usize, bytes: &[u8], len: usize) {
    unsafe {
        #[cfg(target_os = "macos")]
        libc::pthread_jit_write_protect_np(0);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), at as *mut u8, bytes.len());
        #[cfg(target_os = "macos")]
        {
            libc::pthread_jit_write_protect_np(1);
            sys_icache_invalidate(at as *mut libc::c_void, len);
        }
        #[cfg(all(target_arch = "aarch64", not(target_os = "macos")))]
        __clear_cache(at as *mut libc::c_char, (at + len) as *mut libc::c_char);
        #[cfg(not(any(target_os = "macos", target_arch = "aarch64")))]
        let _ = len;
    }
}
