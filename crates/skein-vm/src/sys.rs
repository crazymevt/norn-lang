//! Files, paths, file watching and system information: the primitives
//! behind skein.io, skein.fs and skein.sys (lib/skein/*.skn).
//!
//! Anything that can block in the OS runs on the blocking pool, so a slow
//! disk or network file system never stalls a carrier. Paths are strings.

use crate::blocking::{self, Finish};
use crate::reactor::{self, Io};
use crate::vm::{Error, FailData, Failure, Result, Vm};
use skein_rt::map::{map_assoc, map_empty};
use skein_rt::runtime::rt;
use skein_rt::string::{bytes_new, str_new};
use skein_rt::vector::vec_from_slice;
use skein_rt::{Mutator, Value};
use std::path::{Path, PathBuf};

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

fn strings(m: &mut Mutator, xs: &[String]) -> Value {
    let vs: Vec<Value> = xs.iter().map(|s| str_new(m, s)).collect();
    vec_from_slice(m, &vs)
}

/// A file operation's failure, as the condition its kind makes
/// (FileNotFound, PermissionDenied…), with :path and :reason.
fn err(op: &str, p: &str, e: std::io::Error) -> Failure {
    Failure::io_path(op, p, &e)
}

/// Run blocking `f` on the pool; `done` builds the Skein value.
fn pooled<T: Send + 'static>(
    vm: &mut Vm,
    f: impl FnOnce() -> std::result::Result<T, Failure> + Send + 'static,
    done: impl FnOnce(&mut Mutator, T) -> Value + Send + 'static,
) -> Result<Value> {
    blocking::run(vm, move || {
        let r = f();
        Box::new(move |m: &mut Mutator| r.map(|t| done(m, t))) as Finish
    })
}

// ---- whole files and directories ------------------------------------------------------------

pub(crate) fn read(vm: &mut Vm, path: String, text: bool) -> Result<Value> {
    pooled(
        vm,
        move || {
            let b = std::fs::read(&path).map_err(|e| err("Can't read", &path, e))?;
            if text {
                String::from_utf8(b).map(Ok).map_err(|_| {
                    Failure::of("ParseError", format!("Can't read {path} as text: it isn't valid UTF-8"))
                        .with("source", FailData::Str(path.clone()))
                })
            } else {
                Ok(Err(b))
            }
        },
        |m, r: std::result::Result<String, Vec<u8>>| match r {
            Ok(s) => str_new(m, &s),
            Err(b) => bytes_new(m, &b),
        },
    )
}

pub(crate) fn write(vm: &mut Vm, path: String, data: Vec<u8>, append: bool) -> Result<Value> {
    pooled(
        vm,
        move || {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .append(append)
                .truncate(!append)
                .open(&path)
                .map_err(|e| err("Can't write", &path, e))?;
            f.write_all(&data).map_err(|e| err("Can't write", &path, e))
        },
        |_, ()| Value::NIL,
    )
}

/// `{:type :file|:dir|:symlink|:other :size n :modified epoch-ms :readonly bool}`, or nil.
pub(crate) fn info(vm: &mut Vm, path: String) -> Result<Value> {
    pooled(
        vm,
        move || Ok(std::fs::symlink_metadata(&path).ok()),
        |m, md| {
            let Some(md) = md else { return Value::NIL };
            let t = md.file_type();
            let ty = kw(m, if t.is_symlink() { "symlink" } else if t.is_dir() { "dir" } else if t.is_file() { "file" } else { "other" });
            let modified = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(Value::NIL, |d| Value::fix(d.as_millis() as i64));
            map_of(
                m,
                &[
                    ("type", ty),
                    ("size", Value::fix(md.len() as i64)),
                    ("modified", modified),
                    ("readonly", Value::bool(md.permissions().readonly())),
                ],
            )
        },
    )
}

/// The names in a directory, sorted.
pub(crate) fn list(vm: &mut Vm, path: String) -> Result<Value> {
    pooled(
        vm,
        move || {
            let mut names: Vec<String> = std::fs::read_dir(&path)
                .map_err(|e| err("Can't list", &path, e))?
                .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
                .collect();
            names.sort();
            Ok(names)
        },
        |m, names| strings(m, &names),
    )
}

/// Every path under `root` (not following symbolic links), depth first, sorted.
pub(crate) fn walk(vm: &mut Vm, root: String) -> Result<Value> {
    fn go(dir: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
        let mut es: Vec<PathBuf> = std::fs::read_dir(dir)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
        es.sort();
        for p in es {
            out.push(p.to_string_lossy().into_owned());
            if std::fs::symlink_metadata(&p).map(|m| m.is_dir()).unwrap_or(false) {
                go(&p, out)?;
            }
        }
        Ok(())
    }
    pooled(
        vm,
        move || {
            let mut out = Vec::new();
            go(Path::new(&root), &mut out).map_err(|e| err("Can't walk", &root, e))?;
            Ok(out)
        },
        |m, paths| strings(m, &paths),
    )
}

pub(crate) fn mkdirs(vm: &mut Vm, path: String) -> Result<Value> {
    pooled(vm, move || std::fs::create_dir_all(&path).map_err(|e| err("Can't create", &path, e)), |_, ()| Value::NIL)
}

pub(crate) fn delete(vm: &mut Vm, path: String, recursive: bool) -> Result<Value> {
    pooled(
        vm,
        move || {
            let md = std::fs::symlink_metadata(&path).map_err(|e| err("Can't delete", &path, e))?;
            let r = if md.is_dir() {
                if recursive { std::fs::remove_dir_all(&path) } else { std::fs::remove_dir(&path) }
            } else {
                std::fs::remove_file(&path)
            };
            r.map_err(|e| err("Can't delete", &path, e))
        },
        |_, ()| Value::NIL,
    )
}

pub(crate) fn rename(vm: &mut Vm, from: String, to: String) -> Result<Value> {
    pooled(vm, move || std::fs::rename(&from, &to).map_err(|e| err("Can't rename", &from, e)), |_, ()| Value::NIL)
}

/// Create one directory, failing with FileExists if the path exists.
pub(crate) fn mkdir(vm: &mut Vm, path: String) -> Result<Value> {
    pooled(vm, move || std::fs::create_dir(&path).map_err(|e| err("Can't create the directory", &path, e)), |_, ()| Value::NIL)
}

/// Flush a directory's entries to storage, so a rename or a new file in
/// it survives a crash.
pub(crate) fn sync_dir(vm: &mut Vm, path: String) -> Result<Value> {
    pooled(
        vm,
        move || std::fs::File::open(&path).and_then(|d| d.sync_all()).map_err(|e| err("Can't sync the directory", &path, e)),
        |_, ()| Value::NIL,
    )
}

pub(crate) fn copy(vm: &mut Vm, from: String, to: String) -> Result<Value> {
    pooled(vm, move || std::fs::copy(&from, &to).map_err(|e| err("Can't copy", &from, e)), |_, n| Value::fix(n as i64))
}

// ---- file handles -----------------------------------------------------------------------------

/// `(%fs-open path mode)`: mode is :read, :write (create or truncate),
/// :append (create, add to the end), :read-write (create, keep contents) or
/// :create-new (create, failing with FileExists if the path exists).
pub(crate) fn open(vm: &mut Vm, path: String, mode: &str) -> Result<Value> {
    let mut o = std::fs::OpenOptions::new();
    match mode {
        "read" => o.read(true),
        "write" => o.write(true).create(true).truncate(true),
        "append" => o.append(true).create(true),
        "read-write" => o.read(true).write(true).create(true).truncate(false),
        "create-new" => o.write(true).create_new(true),
        _ => return Err(Error::new(format!("Unknown file mode :{mode} (use :read, :write, :append, :read-write or :create-new)"))),
    };
    let mode = mode.to_owned();
    pooled(
        vm,
        move || o.open(&path).map_err(|e| err("Can't open", &path, e)).map(|f| (f, path, mode)),
        |m, (f, path, mode)| {
            let p = str_new(m, &path);
            let md = kw(m, &mode);
            let info = map_of(m, &[("path", p), ("mode", md)]);
            reactor::register(m, Io::File(f), "file", info).unwrap_or(Value::NIL)
        },
    )
}

/// Up to `n` bytes from a file handle, or nil at the end.
pub(crate) fn file_read(vm: &mut Vm, h: Value, n: usize) -> Result<Value> {
    let e = reactor::entry_of(h, "file")?;
    pooled(
        vm,
        move || {
            use std::io::Read;
            let mut io = e.io.lock().unwrap();
            let Some(Io::File(f)) = io.as_mut() else { return Err(Failure::from("The handle is closed")) };
            let mut buf = vec![0u8; n.clamp(1, 1 << 24)];
            let k = f.read(&mut buf).map_err(|e| { let (c, r) = crate::vm::io_kind(&e); Failure::of(c, format!("read failed: {e}")).with("reason", FailData::Kw(r)) })?;
            buf.truncate(k);
            Ok(buf)
        },
        |m, b| if b.is_empty() { Value::NIL } else { bytes_new(m, &b) },
    )
}

/// Flush a file handle's data and metadata to storage.
pub(crate) fn file_sync(vm: &mut Vm, h: Value) -> Result<Value> {
    let e = reactor::entry_of(h, "file")?;
    pooled(
        vm,
        move || {
            let io = e.io.lock().unwrap();
            let Some(Io::File(f)) = io.as_ref() else { return Err(Failure::from("The handle is closed")) };
            f.sync_all().map_err(|e| {
                let (c, r) = crate::vm::io_kind(&e);
                Failure::of(c, format!("sync failed: {e}")).with("reason", FailData::Kw(r))
            })
        },
        |_, ()| Value::NIL,
    )
}

/// Write all of `data` to a file handle.
pub(crate) fn file_write(vm: &mut Vm, h: Value, data: Vec<u8>) -> Result<Value> {
    let e = reactor::entry_of(h, "file")?;
    pooled(
        vm,
        move || {
            use std::io::Write;
            let mut io = e.io.lock().unwrap();
            let Some(Io::File(f)) = io.as_mut() else { return Err(Failure::from("The handle is closed")) };
            f.write_all(&data).map_err(|e| { let (c, r) = crate::vm::io_kind(&e); Failure::of(c, format!("write failed: {e}")).with("reason", FailData::Kw(r)) })?;
            Ok(data.len())
        },
        |_, n| Value::fix(n as i64),
    )
}

/// One line from standard input (without its newline), or nil at the end.
pub(crate) fn stdin_line(vm: &mut Vm) -> Result<Value> {
    pooled(
        vm,
        || {
            let mut s = String::new();
            let n = std::io::stdin().read_line(&mut s).map_err(|e| format!("Can't read standard input: {e}"))?;
            Ok((n > 0).then(|| s.trim_end_matches(['\n', '\r']).to_owned()))
        },
        |m, s| s.map_or(Value::NIL, |s| str_new(m, &s)),
    )
}

/// `(%stdin-read n)`: exactly n bytes of standard input (from the same
/// buffer read-line uses), or nil when it ends first. For a protocol that
/// frames messages by length, as the Language Server Protocol does.
pub(crate) fn stdin_read(vm: &mut Vm, n: usize) -> Result<Value> {
    pooled(
        vm,
        move || {
            use std::io::Read;
            let mut buf = vec![0u8; n];
            match std::io::stdin().lock().read_exact(&mut buf) {
                Ok(()) => Ok(Some(buf)),
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
                Err(e) => Err(format!("Can't read standard input: {e}").into()),
            }
        },
        |m, b| b.map_or(Value::NIL, |b| skein_rt::string::bytes_new(m, &b)),
    )
}

/// Text to standard error at once, flushed.
pub(crate) fn write_stderr(s: &str) {
    use std::io::Write;
    let mut e = std::io::stderr().lock();
    let _ = e.write_all(s.as_bytes());
    let _ = e.flush();
}

/// `(%write-stdout s)`: s to standard output at once, flushed, whatever
/// *out* is (a protocol's messages, while printing goes elsewhere).
pub(crate) fn write_stdout(s: &str) {
    use std::io::Write;
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(s.as_bytes());
    let _ = o.flush();
}

// ---- watching -----------------------------------------------------------------------------------

/// The one mutator that threads outside the carriers (file watchers, signal
/// listeners, dump helpers) allocate with, one at a time. It is never
/// dropped: a thread-local mutator would be destroyed by the thread's exit
/// destructor, outside any safepoint, possibly in the middle of a
/// collection.
struct Feeder(Option<Mutator>);

// SAFETY: the mutator is used by one thread at a time (under the lock), as
// a running participant; MMTk identifies it by its cell, not its thread.
unsafe impl Send for Feeder {}

static FEEDER: std::sync::Mutex<Feeder> = std::sync::Mutex::new(Feeder(None));

/// From a non-carrier thread: run `f` as a running participant with the
/// shared feeder mutator.
pub(crate) fn on_feed_thread<R>(f: impl FnOnce(&mut Mutator) -> R) -> R {
    crate::sched::delivering(|| {
        let mut g = FEEDER.lock().unwrap();
        f(g.0.get_or_insert_with(Mutator::new))
    })
}

/// From a non-carrier thread: build a value and offer it on channel `c`
/// (dropped if the channel is full or closed).
pub(crate) fn feed(c: Value, make: impl FnOnce(&mut Mutator) -> Value) {
    on_feed_thread(|m| {
        let v = make(m);
        crate::conc::offer_from(m, c, v);
    });
}

/// Keeps a watcher's channel rooted for as long as the watcher lives.
struct Watching {
    _w: notify::RecommendedWatcher,
    root: u64,
}

impl Drop for Watching {
    fn drop(&mut self) {
        crate::sched::unroot(self.root);
    }
}

/// `(%fs-watch path recursive?)`: `[handle channel]`. The channel receives
/// `{:kind :create|:modify|:remove|:other :paths [...]}`; events are dropped
/// while it is full (it buffers 1024).
pub(crate) fn watch(vm: &mut Vm, path: String, recursive: bool) -> Result<Value> {
    use notify::{EventKind, RecursiveMode, Watcher};
    let c = skein_rt::conc::chan_new(&mut vm.m, 1024);
    let root = crate::sched::root(c);
    let mut w = notify::recommended_watcher(move |ev: notify::Result<notify::Event>| {
        let Ok(ev) = ev else { return };
        let kind = match ev.kind {
            EventKind::Create(_) => "create",
            EventKind::Modify(_) => "modify",
            EventKind::Remove(_) => "remove",
            _ => "other",
        };
        let paths: Vec<String> = ev.paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        feed(c, |m| {
            let k = kw(m, kind);
            let ps = strings(m, &paths);
            map_of(m, &[("kind", k), ("paths", ps)])
        });
    })
    .map_err(|e| {
        crate::sched::unroot(root);
        Error::new(format!("Can't watch {path}: {e}"))
    })?;
    let mode = if recursive { RecursiveMode::Recursive } else { RecursiveMode::NonRecursive };
    if let Err(e) = w.watch(Path::new(&path), mode) {
        crate::sched::unroot(root);
        return Err(Error::new(format!("Can't watch {path}: {e}")));
    }
    let p = str_new(&mut vm.m, &path);
    let info = map_of(&mut vm.m, &[("path", p)]);
    let h = reactor::register(&mut vm.m, Io::Watch(Box::new(Watching { _w: w, root })), "watch", info).map_err(Error::new)?;
    Ok(vec_from_slice(&mut vm.m, &[h, c]))
}

// ---- paths (pure; no I/O) --------------------------------------------------------------------------

pub(crate) fn path_join(parts: &[String]) -> String {
    let mut p = PathBuf::new();
    for s in parts {
        p.push(s);
    }
    p.to_string_lossy().into_owned()
}

/// :parent, :name, :ext, :stem, :absolute or :canonical of a path (nil where
/// it has none; :canonical is nil if the path doesn't exist).
pub(crate) fn path_part(m: &mut Mutator, p: &str, part: &str) -> Result<Value> {
    let path = Path::new(p);
    let s = match part {
        "parent" => path.parent().map(|x| x.to_string_lossy().into_owned()),
        "name" => path.file_name().map(|x| x.to_string_lossy().into_owned()),
        "ext" => path.extension().map(|x| x.to_string_lossy().into_owned()),
        "stem" => path.file_stem().map(|x| x.to_string_lossy().into_owned()),
        "absolute" => Some(std::path::absolute(path).map_err(|e| Error::new(format!("{p}: {e}")))?.to_string_lossy().into_owned()),
        "canonical" => std::fs::canonicalize(path).ok().map(|x| x.to_string_lossy().into_owned()),
        _ => return Err(Error::new(format!("Unknown path part :{part}"))),
    };
    Ok(s.map_or(Value::NIL, |s| str_new(m, &s)))
}

// ---- the system (read live, so a restored image sees the machine it runs on) ---------------

pub(crate) fn getenv(m: &mut Mutator, name: &str) -> Value {
    std::env::var(name).map_or(Value::NIL, |v| str_new(m, &v))
}

pub(crate) fn env(m: &mut Mutator) -> Value {
    let mut map = map_empty();
    for (k, v) in std::env::vars() {
        let (k, v) = (str_new(m, &k), str_new(m, &v));
        map = map_assoc(m, map, k, v);
    }
    map
}

pub(crate) fn args(m: &mut Mutator) -> Value {
    let a: Vec<String> = std::env::args().collect();
    strings(m, &a)
}

static PROGRAM_ARGS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

/// The running program's own arguments: those after a script's path, an
/// image's, or a tool's name. The command line sets them once, before
/// any Skein code runs; an embedder that never does gets none.
pub fn set_program_args(args: Vec<String>) {
    let _ = PROGRAM_ARGS.set(args);
}

pub(crate) fn program_args(m: &mut Mutator) -> Value {
    strings(m, PROGRAM_ARGS.get().map(Vec::as_slice).unwrap_or(&[]))
}

fn hostname() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: buf is writable for its length.
    let r = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if r != 0 {
        return None;
    }
    let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..n]).into_owned())
}

fn total_memory() -> Option<u64> {
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

/// Everything skein.sys reports about the machine and the process.
pub(crate) fn system(m: &mut Mutator) -> Value {
    let os = kw(m, match std::env::consts::OS { "macos" => "macos", "linux" => "linux", "windows" => "windows", o => o });
    let family = kw(m, std::env::consts::FAMILY);
    let arch = kw(m, std::env::consts::ARCH);
    let endian = kw(m, if cfg!(target_endian = "little") { "little" } else { "big" });
    let cores = Value::fix(std::thread::available_parallelism().map_or(1, |n| n.get()) as i64);
    // SAFETY: sysconf has no preconditions.
    let page = Value::fix(unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as i64);
    let mem = total_memory().map_or(Value::NIL, |n| Value::fix(n as i64));
    let pid = Value::fix(std::process::id() as i64);
    let tmp = str_new(m, &std::env::temp_dir().to_string_lossy());
    let home = getenv(m, "HOME");
    let user = std::env::var("USER").or_else(|_| std::env::var("LOGNAME")).map_or(Value::NIL, |u| str_new(m, &u));
    let host = hostname().map_or(Value::NIL, |h| str_new(m, &h));
    let cwd = std::env::current_dir().map_or(Value::NIL, |d| str_new(m, &d.to_string_lossy()));
    let version = str_new(m, env!("CARGO_PKG_VERSION"));
    let build = str_new(m, &format!("{:016x}", crate::image::build_id()));
    let exe = std::env::current_exe().map_or(Value::NIL, |p| str_new(m, &p.to_string_lossy()));
    map_of(
        m,
        &[
            ("os", os),
            ("family", family),
            ("arch", arch),
            ("endian", endian),
            ("cores", cores),
            ("page-size", page),
            ("memory", mem),
            ("pid", pid),
            ("temp-dir", tmp),
            ("home-dir", home),
            ("user", user),
            ("hostname", host),
            ("cwd", cwd),
            ("skein-version", version),
            ("build", build),
            ("executable", exe),
        ],
    )
}

// ---- glob patterns --------------------------------------------------------------------------------

/// Does `path` match `pattern`? `*` matches within one path segment, `**`
/// matches any number of segments, `?` one character.
pub(crate) fn glob_match(pattern: &str, path: &str) -> bool {
    fn seg(p: &[char], s: &[char]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some('*'), _) => seg(&p[1..], s) || (!s.is_empty() && seg(p, &s[1..])),
            (Some('?'), Some(_)) => seg(&p[1..], &s[1..]),
            (Some(a), Some(b)) if a == b => seg(&p[1..], &s[1..]),
            _ => false,
        }
    }
    fn segs(p: &[&str], s: &[&str]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some(&"**"), _) => segs(&p[1..], s) || (!s.is_empty() && segs(p, &s[1..])),
            (Some(a), Some(b)) => {
                let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
                seg(&a, &b) && segs(&p[1..], &s[1..])
            }
            _ => false,
        }
    }
    let p: Vec<&str> = pattern.split('/').collect();
    let s: Vec<&str> = path.split('/').collect();
    segs(&p, &s)
}

/// The directory a glob pattern searches: its segments before the first
/// one with a wildcard ("." if none).
pub(crate) fn glob_base(pattern: &str) -> String {
    let fixed: Vec<&str> = pattern.split('/').take_while(|s| !s.contains(['*', '?'])).collect();
    let n = fixed.len().min(pattern.split('/').count().saturating_sub(1));
    let base = fixed[..n].join("/");
    if base.is_empty() && pattern.starts_with('/') { "/".into() } else if base.is_empty() { ".".into() } else { base }
}

// ---- the host terminal ------------------------------------------------------------------------

/// Terminal settings saved by `tty_raw`, restored by `tty_restore` or, for
/// any still raw, when the process exits.
static SAVED_TERMIOS: std::sync::Mutex<Vec<(libc::c_int, libc::termios)>> = std::sync::Mutex::new(Vec::new());

fn tty_fd(v: Value) -> Result<libc::c_int> {
    match v.is_fixnum().then(|| v.as_fixnum()) {
        Some(fd @ 0..=2) => Ok(fd as libc::c_int),
        _ => Err(Error::new("A terminal is standard input (0), output (1) or error (2)")),
    }
}

/// Is descriptor `fd` (0–2) a terminal?
pub(crate) fn is_tty(fd: Value) -> Result<Value> {
    let fd = tty_fd(fd)?;
    // SAFETY: isatty only inspects the descriptor.
    Ok(Value::bool(unsafe { libc::isatty(fd) } == 1))
}

/// `[rows cols]` of the terminal on `fd`, or nil when it isn't one (or
/// doesn't say).
pub(crate) fn tty_size(m: &mut Mutator, fd: Value) -> Result<Value> {
    let fd = tty_fd(fd)?;
    let mut ws = libc::winsize { ws_row: 0, ws_col: 0, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: TIOCGWINSZ writes a winsize.
    let r = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ as _, &mut ws) };
    Ok(if r == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
        vec_from_slice(m, &[Value::fix(ws.ws_row as i64), Value::fix(ws.ws_col as i64)])
    } else {
        Value::NIL
    })
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
pub(crate) fn tty_raw(fd: Value) -> Result<Value> {
    static AT_EXIT: std::sync::Once = std::sync::Once::new();
    let fd = tty_fd(fd)?;
    let mut saved = SAVED_TERMIOS.lock().unwrap();
    if saved.iter().any(|(f, _)| *f == fd) {
        return Ok(Value::FALSE);
    }
    // SAFETY: termios is plain data; tcgetattr fills it.
    let mut orig: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut orig) } != 0 {
        return Err(Error::new(format!("Can't use raw mode: {}", std::io::Error::last_os_error())));
    }
    let mut raw = orig;
    // SAFETY: cfmakeraw edits the struct it's given.
    unsafe { libc::cfmakeraw(&mut raw) };
    raw.c_oflag |= libc::OPOST | libc::ONLCR;
    raw.c_cc[libc::VMIN] = 1;
    raw.c_cc[libc::VTIME] = 0;
    // SAFETY: tcsetattr with a filled-in termios.
    if unsafe { libc::tcsetattr(fd, libc::TCSADRAIN, &raw) } != 0 {
        return Err(Error::new(format!("Can't use raw mode: {}", std::io::Error::last_os_error())));
    }
    saved.push((fd, orig));
    // SAFETY: registers a function with no captured state.
    AT_EXIT.call_once(|| unsafe {
        libc::atexit(restore_terminals);
    });
    Ok(Value::TRUE)
}

/// Undo `tty_raw` on `fd`: true if it was raw.
pub(crate) fn tty_restore(fd: Value) -> Result<Value> {
    let fd = tty_fd(fd)?;
    let mut saved = SAVED_TERMIOS.lock().unwrap();
    let Some(i) = saved.iter().position(|(f, _)| *f == fd) else { return Ok(Value::FALSE) };
    let (_, t) = saved.remove(i);
    // SAFETY: tcsetattr with settings tcgetattr gave for this descriptor.
    if unsafe { libc::tcsetattr(fd, libc::TCSADRAIN, &t) } != 0 {
        return Err(Error::new(format!("Can't restore the terminal: {}", std::io::Error::last_os_error())));
    }
    Ok(Value::TRUE)
}

/// Up to `max` bytes from `fd` as they arrive, waiting at most `timeout_ms`
/// (negative: no limit) for the first: Bytes (empty if none came in time),
/// or nil at the end of input. Reads the descriptor itself, not the
/// buffered standard input that read-line uses.
pub(crate) fn tty_read(vm: &mut Vm, fd: Value, max: i64, timeout_ms: i64) -> Result<Value> {
    let fd = tty_fd(fd)?;
    let max = max.clamp(1, 65536) as usize;
    let timeout = timeout_ms.clamp(-1, i32::MAX as i64) as libc::c_int;
    pooled(
        vm,
        move || loop {
            let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            // SAFETY: poll on one pollfd.
            let r = unsafe { libc::poll(&mut p, 1, timeout) };
            if r < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(format!("Can't read the terminal: {e}").into());
            }
            if r == 0 {
                return Ok(Some(Vec::new()));
            }
            let mut buf = vec![0u8; max];
            // SAFETY: read into a buffer of max bytes.
            let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), max) };
            if n < 0 {
                let e = std::io::Error::last_os_error();
                if matches!(e.kind(), std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock) {
                    continue;
                }
                return Err(format!("Can't read the terminal: {e}").into());
            }
            buf.truncate(n as usize);
            return Ok((n > 0).then_some(buf));
        },
        |m, b: Option<Vec<u8>>| b.map_or(Value::NIL, |b| bytes_new(m, &b)),
    )
}
