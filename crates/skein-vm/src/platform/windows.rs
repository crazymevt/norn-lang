//! Windows: see `platform/mod.rs` for what this provides, and
//! `docs/plans/plan-windows.md` for the parts still to come.
//!
//! Signals are console events: Ctrl-C is `:int`, Ctrl-Break `:break`, the
//! console closing or the session ending `:term`.

use mio::windows::NamedPipe;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString, c_void};
use std::io::{Error, ErrorKind, Result};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, AsRawSocket, FromRawHandle, IntoRawHandle, OwnedHandle, RawHandle};
use std::os::windows::process::ExitStatusExt;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    BOOL, CloseHandle, ERROR_BROKEN_PIPE, FILETIME, GetLastError, HANDLE, INVALID_HANDLE_VALUE, STATUS_CONTROL_C_EXIT, SYSTEMTIME,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Networking::WinSock::{FD_SET, SOCKET, SOCKET_ERROR, TIMEVAL, select};
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OVERLAPPED, FILE_TYPE_PIPE, GetFileType, ReadFile};
use windows_sys::Win32::System::Console::{
    CONSOLE_SCREEN_BUFFER_INFO, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, ENABLE_PROCESSED_OUTPUT,
    ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetConsoleScreenBufferInfo, GetStdHandle,
    INPUT_RECORD, KEY_EVENT, PeekConsoleInputW, ReadConsoleInputW, ReadConsoleW, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    SetConsoleMode,
};
use windows_sys::Win32::System::Console::{
    COORD, CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT, ClosePseudoConsole,
    CreatePseudoConsole, HPCON, ResizePseudoConsole, SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache;
use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE, VirtualAlloc};
use windows_sys::Win32::System::Pipes::PeekNamedPipe;
use windows_sys::Win32::System::SystemInformation::{
    ComputerNameDnsHostname, GetComputerNameExW, GetSystemInfo, GlobalMemoryStatusEx, MEMORYSTATUSEX, SYSTEM_INFO,
};
use windows_sys::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
    GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcess,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, PROCESS_TERMINATE, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
    UpdateProcThreadAttribute, WaitForSingleObject,
};
use windows_sys::Win32::System::Time::{
    DYNAMIC_TIME_ZONE_INFORMATION, FileTimeToSystemTime, GetDynamicTimeZoneInformation, SystemTimeToFileTime,
    SystemTimeToTzSpecificLocalTimeEx,
};

// ---- processes and signals ----------------------------------------------------------------------

/// The signal that ended a process: a Windows process has only an exit code.
pub(crate) fn exit_signal(_st: &ExitStatus) -> Option<i32> {
    None
}

/// Send signal `sig` to process `pid`: `:term` and `:kill` end it, with exit
/// code 1. Windows has no way to ask another process to end, or to interrupt
/// one that doesn't share a process group made for it.
pub(crate) fn kill(pid: i64, sig: i32) -> Result<()> {
    if !matches!(sig, SIGTERM | SIGKILL) {
        return Err(Error::new(ErrorKind::Unsupported, "only :term and :kill can be sent to a process on Windows"));
    }
    // SAFETY: the handle is closed here, and nothing else is passed.
    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE, 0, pid as u32);
        if h.is_null() {
            return Err(Error::last_os_error());
        }
        let ok = TerminateProcess(h, 1);
        let e = Error::last_os_error();
        CloseHandle(h);
        if ok != 0 { Ok(()) } else { Err(e) }
    }
}

/// Send signal `sig` to this process, on a thread of its own, as a signal
/// comes from outside what's running: heard, or else what it does by default.
pub(crate) fn raise(sig: i32) {
    if sig == SIGKILL {
        // SAFETY: ends this process.
        unsafe { TerminateProcess(GetCurrentProcess(), 1) };
    }
    let _ = std::thread::Builder::new().name("skein-raise".into()).spawn(move || {
        if !arrive(sig) {
            std::process::exit(if sig == SIGTERM { 1 } else { STATUS_CONTROL_C_EXIT as i32 });
        }
    });
}

// (the C runtime's numbers; :kill and :winch have none there, and take Unix's)
const SIGTERM: i32 = 15;
const SIGBREAK: i32 = 21;
const SIGKILL: i32 = 9;
const SIGWINCH: i32 = 28;

/// The signals Skein names, both ways (a keyword's name, its number): Ctrl-C,
/// the console closing or the session ending, Ctrl-Break, and the console's
/// size changing.
pub(crate) const SIGNALS: &[(&str, i32)] =
    &[("int", SIGINT), ("term", SIGTERM), ("break", SIGBREAK), ("kill", SIGKILL), ("winch", SIGWINCH)];

pub(crate) const SIGINT: i32 = 2;
/// Ctrl-Break, which asks for a fiber dump as Ctrl-\ does on Unix.
pub(crate) const SIGQUIT: i32 = SIGBREAK;

/// Signal numbers are below this.
pub(crate) const MAX_SIGNAL: usize = 32;

/// Can a program listen for `sig`?
pub(crate) fn catchable(sig: i32) -> bool {
    sig != SIGKILL
}

/// Is `sig` handled as the system would by default?
pub(crate) fn default_disposition(_sig: i32) -> Result<bool> {
    Ok(true)
}

// ---- listening for signals ------------------------------------------------------------------------

/// Who's listening for what.
struct Listener {
    id: u64,
    nums: Vec<i32>,
    tx: std::sync::mpsc::Sender<i32>,
}

static LISTENERS: Mutex<Vec<Listener>> = Mutex::new(Vec::new());

/// What Ctrl-C does when no listener hears it: the skein command interrupts
/// what it's evaluating.
type Interrupt = Box<dyn FnMut() + Send>;
static INTERRUPT: Mutex<Option<Interrupt>> = Mutex::new(None);

/// Run `f` at each Ctrl-C that no listener hears.
pub(crate) fn on_interrupt(f: impl FnMut() + Send + 'static) {
    watch_console();
    *INTERRUPT.lock().unwrap() = Some(Box::new(f));
}

/// Tell the listeners for `sig`: did any hear it?
fn tell_listeners(sig: i32) -> bool {
    let mut heard = false;
    for l in LISTENERS.lock().unwrap().iter().filter(|l| l.nums.contains(&sig)) {
        heard |= l.tx.send(sig).is_ok();
    }
    heard
}

/// `sig` has come: has anything here dealt with it? (If not, it does what it
/// does by default. The console's size changing does nothing.)
fn arrive(sig: i32) -> bool {
    if tell_listeners(sig) {
        return true;
    }
    if sig == SIGINT
        && let Some(f) = INTERRUPT.lock().unwrap().as_mut()
    {
        f();
        return true;
    }
    sig == SIGWINCH
}

/// The system calls this, on a thread of its own, for each console event.
unsafe extern "system" fn console_event(kind: u32) -> BOOL {
    let sig = match kind {
        CTRL_C_EVENT => SIGINT,
        CTRL_BREAK_EVENT => SIGBREAK,
        CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => SIGTERM,
        _ => return 0,
    };
    let dealt_with = arrive(sig);
    if dealt_with && sig == SIGTERM {
        // (the process ends as this returns, and may have 5 seconds in all:
        // the time it has to shut down)
        std::thread::sleep(Duration::from_secs(4));
    }
    dealt_with as BOOL
}

/// Have the system tell us of console events, from now on.
fn watch_console() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    // SAFETY: the handler lives as long as the process.
    ONCE.call_once(|| unsafe {
        SetConsoleCtrlHandler(Some(console_event), 1);
    });
}

/// Tell the `:winch` listeners when the console's size changes. A console
/// says so only among what's typed at it, which is the program's to read, so
/// the size is looked at four times a second while anyone's listening.
fn watch_size() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = std::thread::Builder::new().name("skein-winch".into()).spawn(|| {
            let mut last = None;
            loop {
                std::thread::sleep(Duration::from_millis(250));
                if !LISTENERS.lock().unwrap().iter().any(|l| l.nums.contains(&SIGWINCH)) {
                    last = None;
                    continue;
                }
                let now = [1, 2, 0].into_iter().find_map(terminal_size);
                if last.is_some() && now != last {
                    tell_listeners(SIGWINCH);
                }
                last = now;
            }
        });
    });
}

/// Stops `on_signals`' thread when closed.
pub(crate) struct SignalHandle(u64);

impl SignalHandle {
    pub(crate) fn close(&self) {
        // (its sender dropped, the thread ends)
        LISTENERS.lock().unwrap().retain(|l| l.id != self.0);
    }
}

/// Call `each` with every one of the signals `nums` as it arrives, on a
/// thread of its own called `thread`, until the handle is closed.
pub(crate) fn on_signals(nums: &[i32], thread: &str, mut each: impl FnMut(i32) + Send + 'static) -> Result<SignalHandle> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name(thread.into()).spawn(move || rx.into_iter().for_each(&mut each))?;
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    LISTENERS.lock().unwrap().push(Listener { id, nums: nums.to_vec(), tx });
    watch_console();
    if nums.contains(&SIGWINCH) {
        watch_size();
    }
    Ok(SignalHandle(id))
}

/// From now on, when `sig` arrives and `unheard()` is true, do what the
/// system would by default: as it does here, a console event no listener
/// heard going on to the system.
pub(crate) fn default_when(_sig: i32, _unheard: impl Fn() -> bool + Send + Sync + 'static) -> Result<()> {
    Ok(())
}

// ---- pipes ----------------------------------------------------------------------------------------

/// The two ends of a pipe, as the reactor holds them: named pipes opened for
/// overlapped I/O, the only kind a completion port can watch. mio buffers
/// them: a write is taken at once and goes out behind the caller, closing
/// the end included.
pub(crate) type PipeIn = NamedPipe;
pub(crate) type PipeOut = NamedPipe;

/// A name no other pipe has.
fn pipe_name() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(r"\\.\pipe\skein-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))
}

/// A new pipe: where to write, and where that's read.
pub(crate) fn pipe() -> Result<(PipeOut, PipeIn)> {
    let name = pipe_name();
    // (NamedPipe::new refuses a name that's taken: nobody can be there first)
    let rx = NamedPipe::new(&name)?;
    let tx = std::fs::OpenOptions::new().write(true).custom_flags(FILE_FLAG_OVERLAPPED).open(&name)?;
    // SAFETY: a handle we own, opened for overlapped I/O.
    Ok((unsafe { NamedPipe::from_raw_handle(tx.into_raw_handle()) }, rx))
}

// (The standard library makes a child's pipes as named pipes, with its own
// end opened for overlapped I/O: if that ever changed, registering one with
// the reactor would fail, and say so.)

/// A child's standard input, to write to.
pub(crate) fn child_input(x: std::process::ChildStdin) -> Result<PipeOut> {
    // SAFETY: a handle we own, opened for overlapped I/O.
    Ok(unsafe { NamedPipe::from_raw_handle(x.into_raw_handle()) })
}

/// A child's standard output, to read.
pub(crate) fn child_output(x: std::process::ChildStdout) -> Result<PipeIn> {
    // SAFETY: a handle we own, opened for overlapped I/O.
    Ok(unsafe { NamedPipe::from_raw_handle(x.into_raw_handle()) })
}

/// A child's standard error, to read.
pub(crate) fn child_errors(x: std::process::ChildStderr) -> Result<PipeIn> {
    // SAFETY: a handle we own, opened for overlapped I/O.
    Ok(unsafe { NamedPipe::from_raw_handle(x.into_raw_handle()) })
}

// ---- pseudo-terminals ---------------------------------------------------------------------------

/// Why a child couldn't be started on a pseudo-terminal.
pub(crate) enum PtyError {
    /// No pseudo-terminal could be made.
    Open(Error),
    /// The program couldn't be started.
    Spawn(Error),
}

/// A child process, as the reactor holds it: one the standard library
/// started, or one on a pseudo-console, which it can't start.
pub(crate) enum Child {
    Plain(std::process::Child),
    OnConsole { process: Arc<OwnedHandle>, pid: u32 },
}

impl From<std::process::Child> for Child {
    fn from(c: std::process::Child) -> Child {
        Child::Plain(c)
    }
}

impl Child {
    pub(crate) fn id(&self) -> u32 {
        match self {
            Child::Plain(c) => c.id(),
            Child::OnConsole { pid, .. } => *pid,
        }
    }

    /// Wait for it to end.
    pub(crate) fn wait(&mut self) -> Result<ExitStatus> {
        match self {
            Child::Plain(c) => c.wait(),
            Child::OnConsole { process, .. } => {
                let h = process.as_raw_handle() as HANDLE;
                let mut code = 0;
                // SAFETY: the handle is open while `process` is held; code is writable.
                unsafe {
                    if WaitForSingleObject(h, INFINITE) != WAIT_OBJECT_0 || GetExitCodeProcess(h, &mut code) == 0 {
                        return Err(Error::last_os_error());
                    }
                }
                Ok(ExitStatus::from_raw(code))
            }
        }
    }
}

/// A pseudo-console, until it's closed.
struct Console(Mutex<Option<HPCON>>);

impl Console {
    fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        let Some(hpc) = *self.0.lock().unwrap() else { return Ok(()) };
        // SAFETY: the console is open while the lock says so.
        let hr = unsafe { ResizePseudoConsole(hpc, COORD { X: cols as i16, Y: rows as i16 }) };
        if hr < 0 { Err(Error::from_raw_os_error(hr)) } else { Ok(()) }
    }

    /// Close it: what it has yet to show goes out, then its output ends.
    fn close(&self) {
        let hpc = self.0.lock().unwrap().take();
        if let Some(hpc) = hpc {
            // SAFETY: taken from the lock: it's closed once.
            unsafe { ClosePseudoConsole(hpc) };
        }
    }
}

/// The consoles of children still running, by the handle of the pipe that
/// types at them: what `set_window_size` is given.
static CONSOLES: Mutex<BTreeMap<usize, Arc<Console>>> = Mutex::new(BTreeMap::new());

/// `arg` added to a command line, quoted as programs' C runtimes read it.
fn push_arg(line: &mut Vec<u16>, arg: &OsStr) {
    const QUOTE: u16 = b'"' as u16;
    const SLASH: u16 = b'\\' as u16;
    let wide: Vec<u16> = arg.encode_wide().collect();
    if !line.is_empty() {
        line.push(b' ' as u16);
    }
    if !wide.is_empty() && !wide.iter().any(|&c| c == b' ' as u16 || c == b'\t' as u16 || c == QUOTE) {
        line.extend(wide);
        return;
    }
    line.push(QUOTE);
    let mut slashes = 0;
    for c in wide {
        if c == SLASH {
            slashes += 1;
        } else {
            if c == QUOTE {
                line.extend(std::iter::repeat_n(SLASH, slashes + 1));
            }
            slashes = 0;
        }
        line.push(c);
    }
    line.extend(std::iter::repeat_n(SLASH, slashes));
    line.push(QUOTE);
}

/// The environment `c` is to run in, as CreateProcessW takes one: sorted,
/// names compared without regard to case.
fn environment_block(c: &std::process::Command, clear_env: bool) -> Vec<u16> {
    let key = |k: &OsStr| k.to_string_lossy().to_uppercase();
    let mut vars: BTreeMap<String, (OsString, OsString)> = BTreeMap::new();
    if !clear_env {
        vars.extend(std::env::vars_os().map(|(k, v)| (key(&k), (k, v))));
    }
    for (k, v) in c.get_envs() {
        match v {
            Some(v) => vars.insert(key(k), (k.to_owned(), v.to_owned())),
            None => vars.remove(&key(k)),
        };
    }
    let mut block = Vec::new();
    for (k, v) in vars.values() {
        block.extend(k.encode_wide());
        block.push(b'=' as u16);
        block.extend(v.encode_wide());
        block.push(0);
    }
    // (an empty block is two NULs)
    block.resize(block.len() + if block.is_empty() { 2 } else { 1 }, 0);
    block
}

/// Start `c` on pseudo-console `hpc`.
fn start_on_console(c: &std::process::Command, clear_env: bool, hpc: HPCON) -> Result<PROCESS_INFORMATION> {
    let mut line = Vec::new();
    push_arg(&mut line, c.get_program());
    c.get_args().for_each(|a| push_arg(&mut line, a));
    line.push(0);
    let mut env = environment_block(c, clear_env);
    let dir: Option<Vec<u16>> = c.get_current_dir().map(|d| d.as_os_str().encode_wide().chain([0]).collect());
    // SAFETY: the list is sized as the system asks, in words for its alignment,
    // and outlives the call; the strings are NUL-terminated, the block ended as
    // CREATE_UNICODE_ENVIRONMENT says, and the structs are plain data.
    unsafe {
        let mut size = 0;
        InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
        let mut list = vec![0u64; size.div_ceil(8)];
        let attrs: LPPROC_THREAD_ATTRIBUTE_LIST = list.as_mut_ptr().cast();
        if InitializeProcThreadAttributeList(attrs, 1, 0, &mut size) == 0 {
            return Err(Error::last_os_error());
        }
        let done = (|| {
            let attr = PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize;
            if UpdateProcThreadAttribute(attrs, 0, attr, hpc as *const c_void, size_of::<HPCON>(), std::ptr::null_mut(), std::ptr::null())
                == 0
            {
                return Err(Error::last_os_error());
            }
            let mut si: STARTUPINFOEXW = std::mem::zeroed();
            si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            si.lpAttributeList = attrs;
            // (else a child can take this process's own redirected streams
            // in place of the console's)
            si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            si.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
            si.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
            si.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
            let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
            let ok = CreateProcessW(
                std::ptr::null(),
                line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                env.as_mut_ptr().cast(),
                dir.as_ref().map_or(std::ptr::null(), |d| d.as_ptr()),
                &si.StartupInfo,
                &mut pi,
            );
            if ok == 0 { Err(Error::last_os_error()) } else { Ok(pi) }
        })();
        DeleteProcThreadAttributeList(attrs);
        done
    }
}

/// Start `c` on a new pseudo-terminal of `rows` by `cols`. The child, and
/// the two ends its parent keeps: what the terminal shows, and what's typed
/// at it.
pub(crate) fn spawn_on_pty(
    c: &mut std::process::Command,
    clear_env: bool,
    rows: u16,
    cols: u16,
) -> std::result::Result<(Child, PipeIn, PipeOut), PtyError> {
    let open = || -> Result<(PipeOut, PipeIn, HPCON)> {
        // (the console's own ends are synchronous, as it requires)
        let typed = pipe_name();
        let tx = NamedPipe::new(&typed)?;
        let conin = std::fs::OpenOptions::new().read(true).open(&typed)?;
        let shown = pipe_name();
        let rx = NamedPipe::new(&shown)?;
        let conout = std::fs::OpenOptions::new().write(true).open(&shown)?;
        let mut hpc = 0;
        let size = COORD { X: cols as i16, Y: rows as i16 };
        // SAFETY: both handles are open; the console takes copies of its own.
        let hr = unsafe { CreatePseudoConsole(size, conin.as_raw_handle() as HANDLE, conout.as_raw_handle() as HANDLE, 0, &mut hpc) };
        if hr < 0 { Err(Error::from_raw_os_error(hr)) } else { Ok((tx, rx, hpc)) }
    };
    let (tx, rx, hpc) = open().map_err(PtyError::Open)?;
    let console = Arc::new(Console(Mutex::new(Some(hpc))));
    let pi = match start_on_console(c, clear_env, hpc) {
        Ok(pi) => pi,
        Err(e) => {
            console.close();
            return Err(PtyError::Spawn(e));
        }
    };
    // SAFETY: CreateProcessW gave us these two handles to own.
    let process = unsafe {
        CloseHandle(pi.hThread);
        Arc::new(OwnedHandle::from_raw_handle(pi.hProcess as RawHandle))
    };
    let key = tx.as_raw_handle() as usize;
    CONSOLES.lock().unwrap().insert(key, console.clone());
    // A console's output doesn't end with its program: it ends when the
    // console is closed, so that's done as the program ends.
    let watched = process.clone();
    std::thread::Builder::new()
        .name("skein-console".into())
        .spawn(move || {
            // SAFETY: the handle is open while `watched` is held.
            unsafe { WaitForSingleObject(watched.as_raw_handle() as HANDLE, INFINITE) };
            // (the pipe may be closed by now, and its handle another console's)
            let mut consoles = CONSOLES.lock().unwrap();
            if consoles.get(&key).is_some_and(|c| Arc::ptr_eq(c, &console)) {
                consoles.remove(&key);
            }
            drop(consoles);
            console.close();
        })
        .map_err(PtyError::Open)?;
    Ok((Child::OnConsole { process, pid: pi.dwProcessId }, rx, tx))
}

/// Tell the pseudo-terminal whose master is `master` its new size.
pub(crate) fn set_window_size(master: &PipeOut, rows: u16, cols: u16) -> Result<()> {
    let console = CONSOLES.lock().unwrap().get(&(master.as_raw_handle() as usize)).cloned();
    console.map_or(Ok(()), |c| c.resize(rows, cols))
}

/// Is `e` what reading a pseudo-terminal gives once its child has gone?
pub(crate) fn is_pty_closed(e: &Error) -> bool {
    e.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32)
}

// ---- the host terminal --------------------------------------------------------------------------

/// The standard stream `fd` (0, 1 or 2).
fn std_handle(fd: i32) -> HANDLE {
    let which = match fd {
        0 => STD_INPUT_HANDLE,
        1 => STD_OUTPUT_HANDLE,
        _ => STD_ERROR_HANDLE,
    };
    // SAFETY: GetStdHandle has no memory preconditions.
    unsafe { GetStdHandle(which) }
}

/// The console mode of `h`, or None when it isn't a console.
fn console_mode(h: HANDLE) -> Option<u32> {
    let mut mode = 0;
    // SAFETY: mode is writable; a handle that isn't a console only fails.
    (unsafe { GetConsoleMode(h, &mut mode) } != 0).then_some(mode)
}

fn set_console_mode(h: HANDLE, mode: u32) -> Result<()> {
    // SAFETY: SetConsoleMode has no memory preconditions.
    if unsafe { SetConsoleMode(h, mode) } != 0 { Ok(()) } else { Err(Error::last_os_error()) }
}

/// The console modes `terminal_raw` replaced on `fd`'s behalf. Windows keeps
/// the keyboard's and the screen's apart, where a Unix terminal has one set.
struct SavedModes {
    fd: i32,
    input: Option<u32>,
    output: Option<u32>,
}

impl SavedModes {
    fn restore(&self) -> Result<()> {
        if let Some(mode) = self.input {
            set_console_mode(std_handle(0), mode)?;
        }
        if let Some(mode) = self.output {
            set_console_mode(std_handle(output_of(self.fd)), mode)?;
        }
        Ok(())
    }
}

/// The stream that shows what's typed at `fd`.
fn output_of(fd: i32) -> i32 {
    if fd == 2 { 2 } else { 1 }
}

/// Modes saved by `terminal_raw`, restored by `terminal_restore` or, for any
/// still raw, when the process exits.
static SAVED_MODES: Mutex<Vec<SavedModes>> = Mutex::new(Vec::new());

/// Is descriptor `fd` a terminal?
pub(crate) fn is_terminal(fd: i32) -> bool {
    console_mode(std_handle(fd)).is_some()
}

/// `(rows, cols)` of the window of the console's screen buffer `h`.
fn window_size(h: HANDLE) -> Option<(u16, u16)> {
    // SAFETY: the struct is plain data, which the call fills.
    let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
    if unsafe { GetConsoleScreenBufferInfo(h, &mut info) } == 0 {
        return None;
    }
    let rows = info.srWindow.Bottom as i32 - info.srWindow.Top as i32 + 1;
    let cols = info.srWindow.Right as i32 - info.srWindow.Left as i32 + 1;
    (rows > 0 && cols > 0).then_some((rows as u16, cols as u16))
}

/// `(rows, cols)` of the terminal on `fd`, or None when it isn't one (or
/// doesn't say). (Standard input has no size of its own: the console's
/// screen is on one of the other two.)
pub(crate) fn terminal_size(fd: i32) -> Option<(u16, u16)> {
    if !is_terminal(fd) {
        return None;
    }
    window_size(std_handle(fd)).or_else(|| window_size(std_handle(1))).or_else(|| window_size(std_handle(2)))
}

extern "C" fn restore_terminals() {
    if let Ok(saved) = SAVED_MODES.lock() {
        for s in saved.iter().rev() {
            let _ = s.restore();
        }
    }
}

/// Put the terminal on `fd` in raw mode: bytes as typed, no echo, no line
/// editing, no signals from Ctrl-C. Output processing stays on, so a newline
/// still returns the cursor, and escape sequences work both ways. True if
/// this changed it, false if it was already raw (from an earlier call).
pub(crate) fn terminal_raw(fd: i32) -> Result<bool> {
    static AT_EXIT: std::sync::Once = std::sync::Once::new();
    let mut saved = SAVED_MODES.lock().unwrap();
    if saved.iter().any(|s| s.fd == fd) {
        return Ok(false);
    }
    if !is_terminal(fd) {
        return Err(Error::last_os_error());
    }
    let (keyboard, screen) = (std_handle(0), std_handle(output_of(fd)));
    let modes = SavedModes { fd, input: console_mode(keyboard), output: console_mode(screen) };
    if let Some(mode) = modes.input {
        let raw = (mode & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT)) | ENABLE_VIRTUAL_TERMINAL_INPUT;
        set_console_mode(keyboard, raw)?;
    }
    if let Some(mode) = modes.output
        && let Err(e) = set_console_mode(screen, mode | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING)
    {
        let _ = modes.restore();
        return Err(e);
    }
    saved.push(modes);
    // SAFETY: registers a function with no captured state.
    AT_EXIT.call_once(|| unsafe {
        libc::atexit(restore_terminals);
    });
    Ok(true)
}

/// Undo `terminal_raw` on `fd`: true if it was raw.
pub(crate) fn terminal_restore(fd: i32) -> Result<bool> {
    let mut saved = SAVED_MODES.lock().unwrap();
    let Some(i) = saved.iter().position(|s| s.fd == fd) else { return Ok(false) };
    saved.remove(i).restore()?;
    Ok(true)
}

/// Milliseconds to `deadline`, for a wait.
fn millis_until(deadline: Option<Instant>) -> u32 {
    deadline.map_or(INFINITE, |d| d.saturating_duration_since(Instant::now()).as_millis().min((INFINITE - 1) as u128) as u32)
}

/// Is a key's character next in the console's input? A console's handle is
/// signalled for anything (the mouse, focus, a key let go), and a read would
/// then block until a key comes: anything else is taken out of the way.
fn key_waiting(h: HANDLE) -> Result<bool> {
    loop {
        // SAFETY: the record is plain data, which the call fills.
        let mut rec: INPUT_RECORD = unsafe { std::mem::zeroed() };
        let mut n = 0;
        if unsafe { PeekConsoleInputW(h, &mut rec, 1, &mut n) } == 0 {
            return Err(Error::last_os_error());
        }
        if n == 0 {
            return Ok(false);
        }
        if rec.EventType == KEY_EVENT as u16 {
            // SAFETY: EventType says which member of the unions is in use.
            let (down, ch) = unsafe { (rec.Event.KeyEvent.bKeyDown, rec.Event.KeyEvent.uChar.UnicodeChar) };
            if down != 0 && ch != 0 {
                return Ok(true);
            }
        }
        if unsafe { ReadConsoleInputW(h, &mut rec, 1, &mut n) } == 0 {
            return Err(Error::last_os_error());
        }
    }
}

/// UTF-8 read from the console beyond what its caller had room for.
static CONSOLE_PENDING: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// `terminal_read` from a console. It's read as UTF-16, which every console
/// gives whatever its code page, and handed on as UTF-8.
fn read_console(h: HANDLE, max: usize, deadline: Option<Instant>) -> Result<Option<Vec<u8>>> {
    loop {
        {
            let mut pending = CONSOLE_PENDING.lock().unwrap();
            if !pending.is_empty() {
                let n = max.min(pending.len());
                return Ok(Some(pending.drain(..n).collect()));
            }
        }
        // SAFETY: WaitForSingleObject has no memory preconditions.
        match unsafe { WaitForSingleObject(h, millis_until(deadline)) } {
            WAIT_OBJECT_0 => {}
            WAIT_TIMEOUT => return Ok(Some(Vec::new())),
            _ => return Err(Error::last_os_error()),
        }
        if !key_waiting(h)? {
            continue;
        }
        let mut units = vec![0u16; (max / 3).max(1)];
        let mut n = 0;
        // SAFETY: units is writable for the length given.
        if unsafe { ReadConsoleW(h, units.as_mut_ptr().cast::<c_void>(), units.len() as u32, &mut n, std::ptr::null()) } == 0 {
            return Err(Error::last_os_error());
        }
        units.truncate(n as usize);
        // (half of a character: the other half is already there)
        if units.last().is_some_and(|u| (0xD800..0xDC00).contains(u)) {
            let mut low = 0u16;
            // SAFETY: low is writable for the one unit asked for.
            if unsafe { ReadConsoleW(h, (&raw mut low).cast::<c_void>(), 1, &mut n, std::ptr::null()) } != 0 && n == 1 {
                units.push(low);
            }
        }
        CONSOLE_PENDING.lock().unwrap().extend_from_slice(String::from_utf16_lossy(&units).as_bytes());
    }
}

/// Wait for a pipe to have something to read: false if time ran out first.
/// (A pipe's handle can't be waited on: it's always signalled.)
fn wait_for_pipe(h: HANDLE, deadline: Option<Instant>) -> Result<bool> {
    loop {
        let mut available = 0;
        // SAFETY: only the count is asked for; the rest may be null.
        let ok = unsafe { PeekNamedPipe(h, std::ptr::null_mut(), 0, std::ptr::null_mut(), &mut available, std::ptr::null_mut()) };
        if ok == 0 {
            // SAFETY: GetLastError has no preconditions.
            return if unsafe { GetLastError() } == ERROR_BROKEN_PIPE { Ok(true) } else { Err(Error::last_os_error()) };
        }
        if available > 0 {
            return Ok(true);
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Up to `max` bytes from `fd` as they arrive, waiting at most `timeout_ms`
/// (negative: no limit) for the first: the bytes (none if none came in
/// time), or None at the end of input. Blocks.
pub(crate) fn terminal_read(fd: i32, max: usize, timeout_ms: i32) -> Result<Option<Vec<u8>>> {
    let deadline = (timeout_ms >= 0).then(|| Instant::now() + Duration::from_millis(timeout_ms as u64));
    let h = std_handle(fd);
    if is_terminal(fd) {
        return read_console(h, max, deadline);
    }
    // SAFETY: GetFileType has no memory preconditions.
    if unsafe { GetFileType(h) } == FILE_TYPE_PIPE && !wait_for_pipe(h, deadline)? {
        return Ok(Some(Vec::new()));
    }
    let mut buf = vec![0u8; max];
    let mut n = 0;
    // SAFETY: buf is writable for max bytes, and the read isn't overlapped.
    if unsafe { ReadFile(h, buf.as_mut_ptr(), max as u32, &mut n, std::ptr::null_mut()) } == 0 {
        // SAFETY: GetLastError has no preconditions.
        return if unsafe { GetLastError() } == ERROR_BROKEN_PIPE { Ok(None) } else { Err(Error::last_os_error()) };
    }
    buf.truncate(n as usize);
    Ok((n > 0).then_some(buf))
}

// ---- the machine ----------------------------------------------------------------------------------

pub(crate) fn hostname() -> Option<String> {
    let mut buf = [0u16; 256];
    let mut n = buf.len() as u32;
    // SAFETY: buf is writable for the n units given.
    let ok = unsafe { GetComputerNameExW(ComputerNameDnsHostname, buf.as_mut_ptr(), &mut n) };
    (ok != 0).then(|| String::from_utf16_lossy(&buf[..n as usize]))
}

/// The machine's memory in bytes.
pub(crate) fn total_memory() -> Option<u64> {
    // SAFETY: the struct is plain data, which the call fills.
    let mut m: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    (unsafe { GlobalMemoryStatusEx(&mut m) } != 0).then_some(m.ullTotalPhys)
}

/// The size of a page of memory in bytes.
pub(crate) fn page_size() -> usize {
    // SAFETY: the struct is plain data, which the call fills.
    let mut info: SYSTEM_INFO = unsafe { std::mem::zeroed() };
    unsafe { GetSystemInfo(&mut info) };
    info.dwPageSize as usize
}

const TICKS_PER_SEC: i64 = 10_000_000;
/// Seconds from 1601, where Windows counts from, to 1970.
const SECS_1601_TO_1970: i64 = 11_644_473_600;

fn file_time(ticks: i64) -> FILETIME {
    FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 }
}

/// This machine's UTC offset in seconds at `epoch_secs` (time zone rules,
/// daylight saving included). 0 for a time Windows can't represent.
pub(crate) fn local_offset(epoch_secs: i64) -> i64 {
    let Some(utc) = epoch_secs.checked_add(SECS_1601_TO_1970).and_then(|s| s.checked_mul(TICKS_PER_SEC)).filter(|t| *t >= 0) else {
        return 0;
    };
    // SAFETY: these are plain data, which the calls fill from values they're given.
    unsafe {
        let mut zone: DYNAMIC_TIME_ZONE_INFORMATION = std::mem::zeroed();
        let (mut utc_parts, mut local_parts): (SYSTEMTIME, SYSTEMTIME) = (std::mem::zeroed(), std::mem::zeroed());
        let mut local: FILETIME = std::mem::zeroed();
        if GetDynamicTimeZoneInformation(&mut zone) == u32::MAX
            || FileTimeToSystemTime(&file_time(utc), &mut utc_parts) == 0
            || SystemTimeToTzSpecificLocalTimeEx(&zone, &utc_parts, &mut local_parts) == 0
            || SystemTimeToFileTime(&local_parts, &mut local) == 0
        {
            return 0;
        }
        let local = ((local.dwHighDateTime as i64) << 32) | local.dwLowDateTime as i64;
        (local - utc).div_euclid(TICKS_PER_SEC)
    }
}

/// Has the connect `s` started finished? True once it's up; the error if it
/// failed. (A connecting socket's peer address is no guide here: Windows
/// gives it before the connection is made.)
pub(crate) fn connect_settled(s: &mio::net::TcpStream) -> Result<bool> {
    let sock = s.as_raw_socket() as SOCKET;
    let one = || {
        let mut set = FD_SET { fd_count: 1, fd_array: [0; 64] };
        set.fd_array[0] = sock;
        set
    };
    let (mut connected, mut failed) = (one(), one());
    let now = TIMEVAL { tv_sec: 0, tv_usec: 0 };
    // SAFETY: the sets each hold one open socket, and select only writes to them.
    if unsafe { select(0, std::ptr::null_mut(), &mut connected, &mut failed, &now) } == SOCKET_ERROR {
        return Err(Error::last_os_error());
    }
    if failed.fd_count != 0 {
        return Err(s.take_error()?.unwrap_or_else(|| Error::from(ErrorKind::ConnectionRefused)));
    }
    Ok(connected.fd_count != 0)
}

/// Flush the directory `path`'s entries to storage: NTFS journals them
/// itself, and a directory can't be flushed as a file can.
pub(crate) fn sync_dir(path: &str) -> Result<()> {
    std::fs::metadata(path).map(|_| ())
}

/// Let this process have as many open files as the system allows: Windows
/// has no limit to raise.
pub(crate) fn raise_file_limit() {}

// ---- executable memory, for tier 1 ----------------------------------------------------------------

/// `size` bytes of memory that code can be written to and run from. Never
/// freed.
pub(crate) fn code_alloc(size: usize) -> Option<usize> {
    // SAFETY: a fresh allocation, at an address the system chooses.
    let p = unsafe { VirtualAlloc(std::ptr::null(), size, MEM_RESERVE | MEM_COMMIT, PAGE_EXECUTE_READWRITE) };
    (!p.is_null()).then_some(p as usize)
}

/// Copy `bytes` to `at`, in memory from `code_alloc`, and make the `len`
/// bytes there ready to run.
///
/// # Safety
/// [at, at + len) is within memory `code_alloc` gave, and nothing runs it
/// meanwhile.
pub(crate) unsafe fn code_write(at: usize, bytes: &[u8], len: usize) {
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), at as *mut u8, bytes.len());
        // (arm64 keeps instruction and data caches apart: code just written
        // must reach the one from the other before it runs)
        FlushInstructionCache(GetCurrentProcess(), at as *const c_void, len);
    }
}
