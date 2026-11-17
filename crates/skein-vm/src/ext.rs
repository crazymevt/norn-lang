//! Native extensions (M7.5, runtime.md §5.7): Rust crates that add
//! primitives, own native objects, and feed channels from threads of their
//! own. The first user is `skein-ui`, whose engine runs on the process's main
//! thread and sends events to Skein.
//!
//! - **Registering:** `register` takes the extensions before the runtime
//!   starts. Each installs its primitives in a namespace it owns (for
//!   example `skein.ui.native`). Extension primitives are numbered after the
//!   built-in ones, and their names are part of the build fingerprint, so an
//!   image only loads in a binary with the same extensions.
//! - **Libraries:** an extension can embed Skein libraries (name, source),
//!   which `require` finds like the built-in ones (`skein.ui` in `skein-ui`).
//!   Their sources are part of the build fingerprint too.
//! - **Primitives** follow the built-in rules (runtime.md §2.3): Rust leaf
//!   functions that never call Skein code.
//! - **Native objects** are `Handle`s (`handle` and `with_native`). Closing
//!   a handle drops the object; a handle saved in an image comes back as
//!   lost, like M6's sockets.
//! - **Threads outside the carriers** post to a `Port`, which keeps its
//!   channel rooted and builds each value inside `delivering`, with the
//!   shared feeder mutator (runtime.md §5.5). Carriers never post: in a
//!   primitive, put on the channel directly.

use crate::prims::{PRIMS, PrimDef};
use crate::vm::{Error, Result, Vm};
use skein_rt::heap::Mutator;
use skein_rt::runtime::rt;
use skein_rt::{Kind, Value};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

pub use crate::prims::PrimFn;

/// A native extension: primitives installed in namespace `ns` at start-up.
pub struct Extension {
    pub name: &'static str,
    pub ns: &'static str,
    pub prims: Vec<PrimDef>,
    /// Embedded Skein libraries: (name, source).
    pub libs: Vec<(&'static str, &'static str)>,
}

/// Every extension's embedded libraries.
pub(crate) fn libs() -> &'static [(&'static str, &'static str)] {
    static LIBS: OnceLock<Vec<(&'static str, &'static str)>> = OnceLock::new();
    LIBS.get_or_init(|| EXTENSIONS.get().map_or_else(Vec::new, |exts| exts.iter().flat_map(|e| e.libs.iter().copied()).collect()))
}

static EXTENSIONS: OnceLock<Vec<Extension>> = OnceLock::new();
static FLAT: OnceLock<Vec<(&'static str, &'static PrimDef)>> = OnceLock::new();
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Register the process's extensions. Once, before the runtime starts.
pub fn register(exts: Vec<Extension>) -> std::result::Result<(), String> {
    if INSTALLED.load(Ordering::Acquire) {
        return Err("Extensions must be registered before the runtime starts".into());
    }
    EXTENSIONS.set(exts).map_err(|_| "Extensions are already registered".to_string())
}

/// Every extension primitive with its namespace, in index order.
fn flat() -> &'static [(&'static str, &'static PrimDef)] {
    FLAT.get_or_init(|| {
        EXTENSIONS.get().map_or_else(Vec::new, |exts| exts.iter().flat_map(|e| e.prims.iter().map(move |p| (e.ns, p))).collect())
    })
}

/// The primitive with index `i`: built-in, then extension.
#[inline]
pub(crate) fn prim(i: usize) -> &'static PrimDef {
    if i < PRIMS.len() { &PRIMS[i] } else { flat()[i - PRIMS.len()].1 }
}

/// Bind the extension primitives (after the built-in ones).
pub(crate) fn install(vm: &mut Vm) {
    INSTALLED.store(true, Ordering::Release);
    for (k, (ns, p)) in flat().iter().enumerate() {
        let ns_sym = rt().symbol(&mut vm.m, None, ns);
        let namespace = rt().find_or_create_ns(ns_sym);
        let sym = rt().symbol(&mut vm.m, None, p.name);
        let qualified = rt().symbol(&mut vm.m, Some(ns), p.name);
        let prim = skein_rt::func::primitive_new(&mut vm.m, qualified, PRIMS.len() + k, p.min, p.max);
        let var = namespace.intern(&mut vm.m, sym);
        var.var_set_root(prim);
    }
}

/// Feed the extensions' names into the build fingerprint.
pub(crate) fn fingerprint(eat: &mut dyn FnMut(&[u8])) {
    for (ns, p) in flat() {
        eat(ns.as_bytes());
        eat(p.name.as_bytes());
        eat(&[p.min as u8, p.max.map_or(255, |m| m as u8)]);
    }
    for (name, src) in libs() {
        eat(name.as_bytes());
        eat(src.as_bytes());
    }
}

// ---- native objects -------------------------------------------------------------------------

/// A handle owning native object `obj`, of kind `kind` (a keyword name such
/// as "window"); `info` is shown by `handle-info`.
pub fn handle(m: &mut Mutator, kind: &str, obj: Box<dyn std::any::Any + Send>, info: Value) -> Result<Value> {
    crate::reactor::register(m, crate::reactor::Io::Native(obj), kind, info).map_err(Error::new)
}

/// Run `f` on the native object of handle `h` (of kind `kind`, holding a `T`).
pub fn with_native<T: 'static, R>(h: Value, kind: &str, f: impl FnOnce(&mut T) -> R) -> Result<R> {
    let e = crate::reactor::entry_of(h, kind)?;
    let mut io = e.io.lock().unwrap();
    match io.as_mut() {
        Some(crate::reactor::Io::Native(obj)) => match obj.downcast_mut::<T>() {
            Some(t) => Ok(f(t)),
            None => Err(Error::new(format!("This {kind} handle holds a different native type"))),
        },
        Some(_) => Err(Error::new(format!("Not a native {kind} handle"))),
        None => Err(Error::new("The handle is closed")),
    }
}

// ---- posting from other threads -------------------------------------------------------------

/// A channel that threads outside the carriers can post to. While a Port
/// exists, its channel stays alive.
pub struct Port {
    chan: Value,
    root: u64,
}

// SAFETY: the channel is rooted while the Port lives and the heap doesn't
// move objects; it is only used through `delivering` (post) or the
// allocation-free close.
unsafe impl Send for Port {}
unsafe impl Sync for Port {}

impl Port {
    /// A port for channel `chan`; call from a primitive (on a carrier).
    pub fn new(chan: Value) -> Result<Port> {
        if !chan.is(Kind::Chan) {
            return Err(Error::new(format!("A port needs a channel, got {}", chan.type_name())));
        }
        Ok(Port { chan, root: crate::sched::root(chan) })
    }

    /// From a thread outside the carriers: build a value with `make` and put
    /// it on the channel if there is room now. False if the channel is full
    /// or closed (the caller decides whether to coalesce, retry or drop).
    pub fn post(&self, make: impl FnOnce(&mut Mutator) -> Value) -> bool {
        crate::sys::on_feed_thread(|m| {
            let v = make(m);
            crate::conc::offer_from(m, self.chan, v)
        })
    }

    /// Close the channel: takers get nil once it is drained.
    pub fn close(&self) {
        crate::conc::close(self.chan);
    }
}

impl Drop for Port {
    fn drop(&mut self) {
        crate::sched::unroot(self.root);
    }
}

// ---- building values ------------------------------------------------------------------------

/// Small constructors for extension code building Skein values.
/// Any Skein number (fixnum, float, bignum or ratio) as an `f64`; None for
/// anything else.
pub fn to_f64(v: Value) -> Option<f64> {
    crate::numeric::num(v).ok().map(|n| n.as_f64())
}

pub mod build {
    use skein_rt::heap::Mutator;
    use skein_rt::runtime::rt;
    use skein_rt::{Value, map, string, vector};

    pub fn keyword(m: &mut Mutator, k: &str) -> Value {
        rt().keyword(m, None, k)
    }
    pub fn string(m: &mut Mutator, s: &str) -> Value {
        string::str_new(m, s)
    }
    pub fn float(m: &mut Mutator, f: f64) -> Value {
        string::float_new(m, f)
    }
    pub fn int(i: i64) -> Value {
        Value::fix(i)
    }
    pub fn vector(m: &mut Mutator, xs: &[Value]) -> Value {
        vector::vec_from_slice(m, xs)
    }
    /// A map with keyword keys.
    pub fn map(m: &mut Mutator, kvs: &[(&str, Value)]) -> Value {
        let mut out = map::map_empty();
        for &(k, v) in kvs {
            let key = keyword(m, k);
            out = map::map_assoc(m, out, key, v);
        }
        out
    }
}
