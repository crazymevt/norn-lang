//! Child processes, time, signals and fiber dumps: the primitives behind
//! skein.process, skein.time and skein.signal, and `(fiber-dump)`.

use crate::platform;
use crate::reactor::{self, Io};
use crate::sched;
use crate::vm::{Error, Result, Vm};
use skein_rt::conc::{claim_new, fiber, try_claim, waiter, waiter_new};
use skein_rt::map::{map_assoc, map_empty};
use skein_rt::runtime::rt;
use skein_rt::string::str_new;
use skein_rt::vector::vec_from_slice;
use skein_rt::{Kind, Mutator, Value};

/// A program that can't be started, as the condition its I/O error makes
/// (FileNotFound for a missing program or working directory,
/// PermissionDenied…), with :path the program, or the directory when it's
/// the directory that's missing.
fn spawn_err(s: &Spawn, e: &std::io::Error) -> Error {
    // (a missing directory is NotFound here whatever the system calls it:
    // Windows says the name isn't a directory)
    let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
    let (path, kind) = match &s.dir {
        Some(d) if !std::path::Path::new(d).is_dir() => (d.clone(), &missing),
        _ => (s.cmd.clone(), e),
    };
    Error::io(format!("Can't start {}: {e}", s.cmd), kind).with("path", crate::vm::ErrData::Str(path))
}

fn kw(m: &mut Mutator, k: &str) -> Value {
    rt().keyword(m, None, k)
}

fn map_of(m: &mut Mutator, kvs: &[(&str, Value)]) -> Value {
    let mut map = map_empty();
    for &(k, v) in kvs {
        let k = kw(m, k);
        map = map_assoc(m, map, k, v);
    }
    map
}

// ---- child processes -------------------------------------------------------------------------

/// How a child's standard stream is connected.
#[derive(Clone, Copy)]
pub(crate) enum Stdio {
    Pipe,
    Inherit,
    Null,
}

pub(crate) struct Spawn {
    pub cmd: String,
    pub args: Vec<String>,
    pub dir: Option<String>,
    pub env: Vec<(String, String)>,
    pub clear_env: bool,
    pub stdio: [Stdio; 3],
}

/// Start a child process: `{:process h :pid n :in h :out h :err h}` (a
/// stream is nil unless piped). Pipes are non-blocking reactor handles.
pub(crate) fn start(vm: &mut Vm, s: Spawn) -> Result<Value> {
    use std::process::Stdio as S;
    let to = |x: Stdio| match x {
        Stdio::Pipe => S::piped(),
        Stdio::Inherit => S::inherit(),
        Stdio::Null => S::null(),
    };
    let mut c = std::process::Command::new(&s.cmd);
    c.args(&s.args).stdin(to(s.stdio[0])).stdout(to(s.stdio[1])).stderr(to(s.stdio[2]));
    if let Some(d) = &s.dir {
        c.current_dir(d);
    }
    if s.clear_env {
        c.env_clear();
    }
    c.envs(s.env.iter().map(|(k, v)| (k, v)));
    let mut child = c.spawn().map_err(|e| spawn_err(&s, &e))?;
    let pid = child.id();
    let m = &mut vm.m;
    let stdin = match child.stdin.take() {
        Some(x) => {
            let tx = platform::child_input(x).map_err(|e| Error::new(e.to_string()))?;
            reactor::register(m, Io::PipeOut(tx), "pipe-out", Value::NIL).map_err(Error::new)?
        }
        None => Value::NIL,
    };
    let mut reader = |x: Option<std::process::ChildStdout>| -> Result<Value> {
        Ok(match x {
            Some(x) => {
                let rx = platform::child_output(x).map_err(|e| Error::new(e.to_string()))?;
                reactor::register(m, Io::PipeIn(rx), "pipe-in", Value::NIL).map_err(Error::new)?
            }
            None => Value::NIL,
        })
    };
    let stdout = reader(child.stdout.take())?;
    let stderr = match child.stderr.take() {
        Some(x) => {
            let rx = platform::child_errors(x).map_err(|e| Error::new(e.to_string()))?;
            reactor::register(m, Io::PipeIn(rx), "pipe-in", Value::NIL).map_err(Error::new)?
        }
        None => Value::NIL,
    };
    let cmd = str_new(m, &s.cmd);
    let pidv = Value::fix(pid as i64);
    let info = map_of(m, &[("cmd", cmd), ("pid", pidv)]);
    // (on Windows the platform's Child is a type of its own)
    #[allow(clippy::useless_conversion)]
    let child: platform::Child = child.into();
    let ph = reactor::register(m, Io::Child(child), "process", info).map_err(Error::new)?;
    Ok(map_of(m, &[("process", ph), ("pid", pidv), ("in", stdin), ("out", stdout), ("err", stderr)]))
}

/// Start a child on a pseudo-terminal (M9 part 9.10): its stdin, stdout and
/// stderr are the terminal, which it has as its controlling terminal (a new
/// session). `{:process h :pid n :in h :out h :err nil}`: :in writes to the
/// terminal (the keyboard), :out reads it (the screen); both the master's.
pub(crate) fn pty_start(vm: &mut Vm, s: Spawn, rows: u16, cols: u16) -> Result<Value> {
    let mut c = std::process::Command::new(&s.cmd);
    c.args(&s.args);
    if let Some(d) = &s.dir {
        c.current_dir(d);
    }
    if s.clear_env {
        c.env_clear();
    }
    c.envs(s.env.iter().map(|(k, v)| (k, v)));
    let (child, rx, tx) = platform::spawn_on_pty(&mut c, s.clear_env, rows, cols).map_err(|e| match e {
        platform::PtyError::Open(e) => Error::new(format!("Can't open a pseudo-terminal: {e}")),
        platform::PtyError::Spawn(e) => spawn_err(&s, &e),
    })?;
    drop(c);
    let pid = child.id();
    let m = &mut vm.m;
    let out = reactor::register(m, Io::PipeIn(rx), "pipe-in", Value::NIL).map_err(Error::new)?;
    let input = reactor::register(m, Io::PipeOut(tx), "pipe-out", Value::NIL).map_err(Error::new)?;
    let cmd = str_new(m, &s.cmd);
    let pidv = Value::fix(pid as i64);
    let info = map_of(m, &[("cmd", cmd), ("pid", pidv)]);
    let ph = reactor::register(m, Io::Child(child), "process", info).map_err(Error::new)?;
    Ok(map_of(m, &[("process", ph), ("pid", pidv), ("in", input), ("out", out), ("err", Value::NIL)]))
}

/// `(%pty-resize h rows cols)`: tell the terminal (h, its :in) its new size
/// (the child gets SIGWINCH).
pub(crate) fn pty_resize(h: Value, rows: u16, cols: u16) -> Result<Value> {
    reactor::with_io(h, |io| match io {
        Io::PipeOut(w) => Some(platform::set_window_size(w, rows, cols)),
        _ => None,
    })?
    .ok_or_else(|| Error::of("TypeError", "resize expects a terminal's :in"))?
    .map_err(|e| Error::new(format!("Can't resize the terminal: {e}")))?;
    Ok(Value::NIL)
}

/// Wait (on the blocking pool) for a child to end: `{:exit code :signal n}`
/// (one of them nil).
pub(crate) fn wait(vm: &mut Vm, h: Value) -> Result<Value> {
    let e = reactor::entry_of(h, "process")?;
    crate::blocking::run(vm, move || {
        let r = {
            let mut io = e.io.lock().unwrap();
            match io.as_mut() {
                Some(Io::Child(c)) => c.wait().map_err(|e| format!("wait failed: {e}")),
                _ => Err("The process handle is closed".to_string()),
            }
        };
        Box::new(move |m: &mut Mutator| {
            let st = r?;
            let code = st.code().map_or(Value::NIL, |c| Value::fix(c as i64));
            // (by name: the numbers differ between systems; a number only for
            // one without a name)
            let sig = platform::exit_signal(&st).map_or(Value::NIL, |s| match signal_name(s) {
                Some(n) => kw(m, n),
                None => Value::fix(s as i64),
            });
            Ok(map_of(m, &[("exit", code), ("signal", sig)]))
        }) as crate::blocking::Finish
    })
}

/// Send a signal (by number) to a child.
pub(crate) fn kill(vm: &mut Vm, h: Value, sig: i32) -> Result<Value> {
    reactor::entry_of(h, "process")?;
    let k = kw(&mut vm.m, "pid");
    let pid = crate::prims::get(h.field(skein_rt::conc::handle::INFO), k).map_or(0, |v| v.as_fixnum());
    platform::kill(pid, sig).map(|()| Value::NIL).map_err(|e| Error::new(format!("kill failed: {e}")))
}

pub(crate) fn signal_number(name: &str) -> Option<i32> {
    platform::SIGNALS.iter().find(|(k, _)| *k == name).map(|(_, n)| *n)
}

fn signal_name(n: i32) -> Option<&'static str> {
    platform::SIGNALS.iter().find(|(_, v)| *v == n).map(|(k, _)| *k)
}

// ---- signals --------------------------------------------------------------------------------------

/// Stops a signal subscription (and unroots its channel) when dropped.
struct Listening {
    handle: platform::SignalHandle,
    root: u64,
    nums: Vec<i32>,
}

impl Drop for Listening {
    fn drop(&mut self) {
        self.handle.close();
        for &n in &self.nums {
            LISTENERS[n as usize].fetch_sub(1, Ordering::SeqCst);
        }
        sched::unroot(self.root);
    }
}

use std::sync::atomic::{AtomicUsize, Ordering};

use platform::MAX_SIGNAL;

/// How many skein.signal listeners each signal has.
static LISTENERS: [AtomicUsize; MAX_SIGNAL] = [const { AtomicUsize::new(0) }; MAX_SIGNAL];

/// How many skein.signal listeners signal `n` has.
pub(crate) fn listeners(n: i32) -> usize {
    if n > 0 && (n as usize) < MAX_SIGNAL { LISTENERS[n as usize].load(Ordering::SeqCst) } else { 0 }
}

/// Is a skein.signal listener listening for SIGINT? The skein command then
/// leaves ^C to the program, as its own (a clean shutdown, say), rather than
/// interrupting the evaluation too.
pub fn listening_for_interrupt() -> bool {
    LISTENERS[platform::SIGINT as usize].load(Ordering::SeqCst) > 0
}

/// Run `f` at each Ctrl-C no listener hears. (On Windows a signal this
/// process raises itself has to reach it too, so the runtime keeps it.)
#[cfg(windows)]
pub fn on_interrupt(f: impl FnMut() + Send + 'static) {
    platform::on_interrupt(f);
}

/// The signals whose disposition has been looked at (see `keep_default`).
static PREPARED: std::sync::Mutex<[bool; MAX_SIGNAL]> = std::sync::Mutex::new([false; MAX_SIGNAL]);

/// signal_hook installs its handler for good: once the last listener for
/// a signal stops, the signal would be caught and dropped (SIGTERM would
/// no longer end the process). So the first time Skein listens for a
/// signal whose disposition is the default, it also registers an action
/// that does what the default would whenever no listener is left. (A
/// signal something else already handles, as the skein command does
/// SIGINT, or ignores, as Rust does SIGPIPE, keeps that: signal_hook
/// chains to the previous handler.)
fn keep_default(sig: i32) -> Result<()> {
    let mut prepared = PREPARED.lock().unwrap_or_else(|e| e.into_inner());
    if prepared[sig as usize] {
        return Ok(());
    }
    if platform::default_disposition(sig).map_err(|e| Error::new(format!("Can't listen for signals: {e}")))? {
        platform::default_when(sig, move || LISTENERS[sig as usize].load(Ordering::SeqCst) == 0)
            .map_err(|e| Error::new(format!("Can't listen for signals: {e}")))?;
    }
    prepared[sig as usize] = true;
    Ok(())
}

/// `(%signal-listen [:usr1 …])`: `[handle channel]`; the channel receives
/// each signal's name as it arrives.
pub(crate) fn listen(vm: &mut Vm, names: &[String]) -> Result<Value> {
    let mut nums = Vec::new();
    for n in names {
        match signal_number(n) {
            // (these stop or end the process, or mean it went wrong: they
            // can't be caught, or the runtime keeps them)
            Some(x) if !platform::catchable(x) => {
                return Err(Error::new(format!(":{n} can't be listened for")));
            }
            Some(x) => nums.push(x),
            None => return Err(Error::new(format!("Unknown signal :{n}"))),
        }
    }
    for &n in &nums {
        if n as usize >= MAX_SIGNAL {
            return Err(Error::new(format!("Can't listen for signal {n}")));
        }
        keep_default(n)?;
    }
    let c = skein_rt::conc::chan_new(&mut vm.m, 64);
    let root = sched::root(c);
    let handle =
        match platform::on_signals(&nums, "skein-signals", move |s| crate::sys::feed(c, |m| kw(m, signal_name(s).unwrap_or("other")))) {
            Ok(h) => h,
            Err(e) => {
                sched::unroot(root);
                return Err(Error::new(format!("Can't listen for signals: {e}")));
            }
        };
    for &n in &nums {
        LISTENERS[n as usize].fetch_add(1, Ordering::SeqCst);
    }
    let listening = Listening { handle, root, nums };
    let h = reactor::register(&mut vm.m, Io::Watch(Box::new(listening)), "signals", Value::NIL).map_err(Error::new)?;
    Ok(vec_from_slice(&mut vm.m, &[h, c]))
}

// ---- time --------------------------------------------------------------------------------------------

pub(crate) fn now_nanos() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as i64)
}

/// This machine's UTC offset in seconds at `epoch_secs` (time zone rules,
/// daylight saving included).
pub(crate) fn local_offset(epoch_secs: i64) -> i64 {
    platform::local_offset(epoch_secs)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (H. Hinnant).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

/// `[year month day hour minute second nanos weekday yearday]` of an
/// instant seen at UTC offset `offset` (seconds). Weekday 1 = Monday.
pub(crate) fn civil(nanos: i64, offset: i64) -> [i64; 9] {
    let local = nanos + offset * 1_000_000_000;
    let secs = local.div_euclid(1_000_000_000);
    let ns = local.rem_euclid(1_000_000_000);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let weekday = (days + 3).rem_euclid(7) + 1; // 1970-01-01 was a Thursday
    let yearday = days - days_from_civil(y, 1, 1) + 1;
    [y, m, d, sod / 3600, sod % 3600 / 60, sod % 60, ns, weekday, yearday]
}

/// The instant (epoch nanoseconds) of a civil date-time at UTC offset `offset`.
pub(crate) fn from_civil(f: [i64; 7], offset: i64) -> std::result::Result<i64, String> {
    let [y, m, d, h, mi, s, ns] = f;
    if !(1..=12).contains(&m)
        || d < 1
        || d > days_in_month(y, m)
        || !(0..24).contains(&h)
        || !(0..60).contains(&mi)
        || !(0..=60).contains(&s)
        || !(0..1_000_000_000).contains(&ns)
    {
        return Err(format!("Not a valid date-time: {y}-{m}-{d} {h}:{mi}:{s}.{ns:09}"));
    }
    let secs = days_from_civil(y, m, d) * 86_400 + h * 3600 + mi * 60 + s - offset;
    secs.checked_mul(1_000_000_000).and_then(|x| x.checked_add(ns)).ok_or_else(|| "Date-time out of range".to_string())
}

/// RFC 3339 text: `2026-11-13T07:05:00.123Z`, or with `+01:00` for a
/// non-zero offset. Fractions are shown only when non-zero (as millis,
/// micros or nanos, whichever is exact).
pub(crate) fn format_iso(nanos: i64, offset: i64) -> String {
    let [y, m, d, h, mi, s, ns, _, _] = civil(nanos, offset);
    let frac = if ns == 0 {
        String::new()
    } else if ns % 1_000_000 == 0 {
        format!(".{:03}", ns / 1_000_000)
    } else if ns % 1000 == 0 {
        format!(".{:06}", ns / 1000)
    } else {
        format!(".{ns:09}")
    };
    let zone = if offset == 0 {
        "Z".to_string()
    } else {
        let a = offset.abs();
        format!("{}{:02}:{:02}", if offset < 0 { '-' } else { '+' }, a / 3600, a % 3600 / 60)
    };
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}{frac}{zone}")
}

/// Parse RFC 3339 (`2026-11-13T07:05:00Z`, `…T07:05:00.5+01:00`; a space
/// may replace the T; a bare date means midnight UTC): `(nanos, offset)`.
pub(crate) fn parse_iso(s: &str) -> std::result::Result<(i64, i64), String> {
    let bad = || format!("Not an RFC 3339 date-time: {s:?}");
    let b = s.as_bytes();
    let num = |a: usize, n: usize| -> std::result::Result<i64, String> {
        let t = s.get(a..a + n).ok_or_else(bad)?;
        if !t.bytes().all(|c| c.is_ascii_digit()) {
            return Err(bad());
        }
        t.parse().map_err(|_| bad())
    };
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return Err(bad());
    }
    let (y, mo, d) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    if b.len() == 10 {
        return Ok((from_civil([y, mo, d, 0, 0, 0, 0], 0)?, 0));
    }
    if !matches!(b[10], b'T' | b't' | b' ') || b.len() < 19 || b[13] != b':' || b[16] != b':' {
        return Err(bad());
    }
    let (h, mi, sec) = (num(11, 2)?, num(14, 2)?, num(17, 2)?);
    let mut i = 19;
    let mut ns = 0;
    if b.get(i) == Some(&b'.') {
        let start = i + 1;
        let mut j = start;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j == start || j - start > 9 {
            return Err(bad());
        }
        ns = num(start, j - start)? * 10i64.pow((9 - (j - start)) as u32);
        i = j;
    }
    let offset = match b.get(i) {
        Some(b'Z' | b'z') if i + 1 == b.len() => 0,
        Some(&c @ (b'+' | b'-')) if i + 6 == b.len() && b[i + 3] == b':' => {
            let o = num(i + 1, 2)? * 3600 + num(i + 4, 2)? * 60;
            if c == b'-' { -o } else { o }
        }
        _ => return Err(bad()),
    };
    Ok((from_civil([y, mo, d, h, mi, sec, ns], offset)?, offset))
}

// ---- fiber dumps ---------------------------------------------------------------------------------------

/// Why a parked fiber is waiting.
fn wait_reason(f: Value) -> &'static str {
    let w = sched::aget(f, fiber::PARK);
    if !sched::aget(f, fiber::JOINING).is_nil() {
        return "joining a scope";
    }
    if !w.is(Kind::Waiter) {
        return "parked";
    }
    if w.field(waiter::CHAN).is(Kind::Chan) {
        "on a channel"
    } else if reactor::holds_waiter(w) {
        "on I/O"
    } else if crate::blocking::holds_waiter(w) {
        "in a blocking call"
    } else if crate::main_lane::holds_waiter(w) {
        "in a main-thread call"
    } else if sched::timer_holds(w) {
        "sleeping"
    } else {
        "awaiting a fiber"
    }
}

/// A text dump of every fiber reachable from the GC roots: state, why it
/// waits, its namespace, and its frames (innermost first). Pauses every
/// fiber meanwhile; call it from a thread that isn't a carrier.
pub(crate) fn dump_text() -> String {
    if let Err(e) = sched::pause_all(std::time::Duration::from_secs(10)) {
        return format!("fiber dump failed: {e}\n");
    }
    // The walk lists the scheduler's roots, which takes the timer lock, so
    // only the reactor and the pool are held off. (A timer firing meanwhile
    // can only move a fiber to a run queue: no fiber runs while paused.)
    let text = skein_rt::safepoint::while_running(|| {
        sched::with_deliveries_held(|| {
            let mut fibers = Vec::new();
            skein_rt::walk::reachable(&skein_rt::walk::gc_roots(), |v| {
                if v.is(Kind::Fiber) {
                    fibers.push(v);
                }
            });
            fibers.sort_by_key(|f| f.fiber_id());
            let mut out = String::new();
            let mut shown = 0;
            for f in fibers {
                let st = f.fiber_state().load(std::sync::atomic::Ordering::Acquire);
                if st == fiber::DONE {
                    continue;
                }
                shown += 1;
                let state = match st {
                    fiber::QUEUED => "runnable".to_string(),
                    fiber::PARKED => format!("parked {}", wait_reason(f)),
                    _ => "running".to_string(),
                };
                // SAFETY: NS holds a leaked namespace.
                let ns = unsafe { (*(f.word(fiber::NS) as *const skein_rt::runtime::Namespace)).name.sym_name() };
                out.push_str(&format!("fiber {} {state} (in {ns})\n", f.fiber_id()));
                let seg = f.field(fiber::SEG);
                if seg.is_nil() {
                    out.push_str("    (not started)\n");
                    continue;
                }
                for (s, fp) in crate::vm::walk_frames(seg, f.field(fiber::FP).as_fixnum() as usize) {
                    // SAFETY: (s, fp) is a live frame.
                    let proto = unsafe { *s.seg_data().add(fp + skein_rt::func::F_PROTO) };
                    let name = proto.proto_name();
                    let name = if name.is_nil() { "fn".to_string() } else { skein_rt::print::pr_str(name, false) };
                    out.push_str(&format!("    {name}\n"));
                }
            }
            format!("Fiber dump: {shown} fiber(s) not finished\n{out}")
        })
    });
    sched::resume_all();
    text
}

/// `(fiber-dump)`: park this fiber while a helper thread makes the dump.
pub(crate) fn fiber_dump(vm: &mut Vm) -> Result<Value> {
    if !vm.can_park() {
        return Err(Vm::park_error());
    }
    let me = vm.current;
    let claim = claim_new(&mut vm.m);
    let w = waiter_new(&mut vm.m, me, Value::NIL, claim, Value::NIL, waiter::NO_CANCEL_WAKE);
    sched::park_on(vm, w, false);
    let key = sched::root(me);
    std::thread::Builder::new()
        .name("skein-dump".into())
        .spawn(move || {
            let text = dump_text();
            crate::sys::on_feed_thread(|m| {
                let s = str_new(m, &text);
                if try_claim(claim) {
                    sched::deliver(w, s, Value::NIL);
                }
            });
            sched::unroot(key);
        })
        .map_err(|e| Error::new(e.to_string()))?;
    Ok(Value::NIL)
}

/// Print a fiber dump to standard error on SIGQUIT (Ctrl-\), as the JVM does.
pub(crate) fn dump_on_sigquit() {
    let _ = platform::on_signals(&[platform::SIGQUIT], "skein-sigquit", |_| eprint!("{}", dump_text()));
}
