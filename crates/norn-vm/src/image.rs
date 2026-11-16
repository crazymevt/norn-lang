//! Saving and restoring images (design §7, runtime.md §7): what the VM saves
//! as roots, stopping every fiber for a save, and bringing an image back.
//!
//! **Save.** `save-image` parks the calling fiber and hands the work to a
//! helper thread. The helper pauses the scheduler (every running fiber
//! yields at its next poll, so all fibers sit at resume points), holds off the
//! timer thread and the GC, and writes everything reachable from the roots
//! below. The saving fiber itself is saved dead: it does not continue in the
//! image. With `:fibers :none` every unfinished fiber is saved dead.
//!
//! **Restore.** `load` runs before any carrier starts: it loads the objects,
//! rebuilds the runtime tables and turns each fiber's saved namespace name
//! back into a namespace. `restore` runs on carrier 0 in place of boot: it
//! requeues the runnable fibers, re-arms timers with the time they had left,
//! and ends the dead fibers (their joiners and scopes see an error). Then
//! `(norn.core/%image-restored)` calls `on-image-restore` (see core.norn).

use crate::sched::{self, SavedTimer};
use crate::vm::{Error, FRAME_KINDS, FrameKinds, Result, Vm};
use norn_rt::conc::{claim_new, fiber, try_claim, waiter, waiter_new};
use norn_rt::image::{Save, Saved};
use norn_rt::runtime::{Tables, rt};
use norn_rt::{Kind, Mutator, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Version of the root layout below (part of the build fingerprint too).
const ROOTS_VERSION: i64 = 3;

/// How long a save waits for running fibers to reach a resume point.
const PAUSE_LIMIT: Duration = Duration::from_secs(10);

/// Fingerprint of this runtime build. An image records the build that wrote
/// it: primitive numbers, object layouts, bytecode and the core library's
/// expectations all have to match.
pub fn build_id() -> u64 {
    static ID: OnceLock<u64> = OnceLock::new();
    *ID.get_or_init(|| {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |b: &[u8]| {
            for &x in b {
                h = (h ^ x as u64).wrapping_mul(0x0000_0100_0000_01b3);
            }
            h = (h ^ 0xff).wrapping_mul(0x0000_0100_0000_01b3);
        };
        eat(env!("CARGO_PKG_VERSION").as_bytes());
        eat(&norn_rt::image::FORMAT.to_le_bytes());
        eat(&ROOTS_VERSION.to_le_bytes());
        eat(&crate::reader::READER_VERSION.to_le_bytes());
        for p in crate::prims::PRIMS {
            eat(p.name.as_bytes());
            eat(&[p.min as u8, p.max.map_or(255, |m| m as u8)]);
        }
        crate::ext::fingerprint(&mut eat);
        for l in norn_rt::object::LAYOUTS {
            eat(l.name.as_bytes());
        }
        for n in norn_rt::clos::BUILTIN_NAMES {
            eat(n.as_bytes());
        }
        eat(crate::eval::CORE_SRC.as_bytes());
        eat(crate::eval::COMPILER_SRC.as_bytes());
        eat(crate::eval::EXPANDER_SRC.as_bytes());
        for (name, src) in crate::eval::LIBS {
            eat(name.as_bytes());
            eat(src.as_bytes());
        }
        h
    })
}

// ---- roots ------------------------------------------------------------------------------
//
//   ROOTS_VERSION
//   identity-hash counter, gensym counter, class epoch, fiber-id counter
//   the five empty collections
//   n, builtin classes…
//   the five frame-kind closures (or nils)
//   entry
//   n, symbols…            n, keywords…
//   n, then per namespace: name, m, (symbol var)…, a, (alias namespace-name)…
//   n, runnable fibers…
//   n, then per timer: ms-left, wake?, target

fn fix(n: u64) -> Value {
    Value::fix(n as i64)
}

fn collect_roots(entry: Value, keep_fibers: bool, timers: &[SavedTimer]) -> Vec<Value> {
    let t = rt().tables();
    let mut r = vec![Value::fix(ROOTS_VERSION)];
    r.push(fix(norn_rt::object::identity_hash_counter()));
    r.push(fix(crate::reader::gensym_counter()));
    r.push(fix(norn_rt::clos::CLASS_EPOCH.load(std::sync::atomic::Ordering::Acquire)));
    r.push(fix(sched::fiber_id_counter()));
    r.extend(t.empties);
    let b = norn_rt::clos::builtins();
    r.push(fix(b.len() as u64));
    r.extend(b);
    match FRAME_KINDS.get() {
        Some(k) => r.extend([k.catch, k.unwind, k.bind, k.finish, k.runtime_error]),
        None => r.extend([Value::NIL; 5]),
    }
    r.push(entry);
    for list in [&t.symbols, &t.keywords] {
        r.push(fix(list.len() as u64));
        r.extend(list.iter().copied());
    }
    r.push(fix(t.namespaces.len() as u64));
    for (name, maps, aliases) in &t.namespaces {
        r.push(*name);
        r.push(fix(maps.len() as u64));
        for &(k, v) in maps {
            r.push(k);
            r.push(v);
        }
        r.push(fix(aliases.len() as u64));
        for &(k, v) in aliases {
            r.push(k);
            r.push(v);
        }
    }
    let queued = if keep_fibers { sched::queued_fibers() } else { Vec::new() };
    r.push(fix(queued.len() as u64));
    r.extend(queued);
    // Without fibers, sleeping fibers' timers go too; `timeout` channels stay.
    let timers: Vec<&SavedTimer> = timers.iter().filter(|t| keep_fibers || !t.wake).collect();
    r.push(fix(timers.len() as u64));
    for t in timers {
        r.push(fix(t.ms_left));
        r.push(Value::bool(t.wake));
        r.push(t.target);
    }
    r
}

/// Reads the root list back, checking each value's shape (an image must not
/// be able to crash the runtime by putting the wrong thing in a table).
struct RootReader<'a> {
    v: &'a [Value],
    i: usize,
}

impl RootReader<'_> {
    fn bad(&self) -> String {
        format!("the image's root table is damaged (at entry {})", self.i)
    }
    fn any(&mut self) -> std::result::Result<Value, String> {
        let x = *self.v.get(self.i).ok_or_else(|| self.bad())?;
        self.i += 1;
        Ok(x)
    }
    fn int(&mut self) -> std::result::Result<u64, String> {
        let x = self.any()?;
        if x.is_fixnum() && x.as_fixnum() >= 0 { Ok(x.as_fixnum() as u64) } else { Err(self.bad()) }
    }
    fn count(&mut self) -> std::result::Result<usize, String> {
        let n = self.int()? as usize;
        if n > self.v.len() { Err(self.bad()) } else { Ok(n) }
    }
    fn of(&mut self, k: Kind) -> std::result::Result<Value, String> {
        let x = self.any()?;
        if x.is(k) { Ok(x) } else { Err(self.bad()) }
    }
    fn opt(&mut self, k: Kind) -> std::result::Result<Value, String> {
        let x = self.any()?;
        if x.is_nil() || x.is(k) { Ok(x) } else { Err(self.bad()) }
    }
    fn many(&mut self, k: Kind) -> std::result::Result<Vec<Value>, String> {
        let n = self.count()?;
        (0..n).map(|_| self.of(k)).collect()
    }
}

// ---- restore -------------------------------------------------------------------------------

/// What carrier 0 finishes after the runtime tables are back.
pub(crate) struct Restore {
    pub next_fiber_id: u64,
    queued: Vec<Value>,
    timers: Vec<SavedTimer>,
    dead: Vec<Value>,
}

/// Load an image and rebuild the runtime tables. Runs before any carrier.
pub(crate) fn load(path: &Path) -> std::result::Result<Restore, String> {
    let mut m = Mutator::new();
    let loaded = norn_rt::image::load(&mut m, path, build_id())?;
    // Its code may have been built elsewhere: check it before any runs
    // (M8 part 8.11).
    let mut bad = None;
    norn_rt::image::for_each_root(&mut |v| {
        if bad.is_none()
            && let Err(e) = crate::verify::image_object(v)
        {
            bad = Some(e);
        }
    });
    if let Some(e) = bad {
        return Err(format!("{} is not a valid Norn image: its code: {e}", path.display()));
    }
    let mut r = RootReader { v: &loaded.roots, i: 0 };
    if r.any()? != Value::fix(ROOTS_VERSION) {
        return Err(r.bad());
    }
    let (ihash, gensym, epoch, next_fiber_id) = (r.int()?, r.int()?, r.int()?, r.int()?);
    let empties = [
        r.of(Kind::EmptyList)?,
        r.of(Kind::Vector)?,
        r.of(Kind::Map)?,
        r.of(Kind::Set)?,
        r.of(Kind::Array)?,
    ];
    let nb = r.count()?;
    let builtins = (0..nb).map(|_| r.opt(Kind::Class)).collect::<std::result::Result<Vec<_>, _>>()?;
    let kinds = (0..5).map(|_| r.opt(Kind::Closure)).collect::<std::result::Result<Vec<_>, _>>()?;
    let entry = r.any()?;
    let symbols = r.many(Kind::Symbol)?;
    let keywords = r.many(Kind::Keyword)?;
    let nns = r.count()?;
    let mut namespaces = Vec::with_capacity(nns);
    for _ in 0..nns {
        let name = r.of(Kind::Symbol)?;
        let n = r.count()?;
        let mut maps = Vec::with_capacity(n);
        for _ in 0..n {
            maps.push((r.of(Kind::Symbol)?, r.of(Kind::Var)?));
        }
        let na = r.count()?;
        let mut aliases = Vec::with_capacity(na);
        for _ in 0..na {
            aliases.push((r.of(Kind::Symbol)?, r.of(Kind::Symbol)?));
        }
        namespaces.push((name, maps, aliases));
    }
    let queued = r.many(Kind::Fiber)?;
    let nt = r.count()?;
    let mut timers = Vec::with_capacity(nt);
    for _ in 0..nt {
        let ms_left = r.int()?;
        let wake = r.any()?;
        let target = if wake == Value::TRUE { r.of(Kind::Waiter)? } else { r.of(Kind::Chan)? };
        timers.push(SavedTimer { ms_left, wake: wake == Value::TRUE, target });
    }
    if r.i != r.v.len() {
        return Err(r.bad());
    }

    norn_rt::runtime::init_from_image(Tables { symbols, keywords, namespaces, empties })?;
    norn_rt::clos::set_builtins(&builtins);
    if kinds.iter().all(|k| k.is(Kind::Closure)) {
        let _ = FRAME_KINDS.set(FrameKinds {
            catch: kinds[0],
            unwind: kinds[1],
            bind: kinds[2],
            finish: kinds[3],
            runtime_error: kinds[4],
        });
    }
    norn_rt::object::set_identity_hash_counter(ihash);
    crate::reader::set_gensym_counter(gensym);
    norn_rt::clos::CLASS_EPOCH.fetch_max(epoch, std::sync::atomic::Ordering::AcqRel);

    for &w in &loaded.weaks {
        norn_rt::weak::register(w);
    }

    // Fibers saved their namespace by name.
    let mut dead = Vec::new();
    for &f in &loaded.fibers {
        let name = Value(f.word(fiber::NS));
        if !name.is(Kind::Symbol) {
            return Err("a fiber in the image has no namespace".into());
        }
        let ns = rt().find_or_create_ns(name);
        f.set_word(fiber::NS, ns as *const norn_rt::runtime::Namespace as u64);
        if f.fiber_state().load(std::sync::atomic::Ordering::Acquire) == fiber::DONE
            && f.field(fiber::ERROR) == Value::UNBOUND
        {
            dead.push(f);
        }
    }

    let core_sym = rt().symbol(&mut m, None, "norn.core");
    if let Some(core) = rt().find_ns(core_sym) {
        let sym = rt().symbol(&mut m, None, "%image-entry");
        core.intern(&mut m, sym).var_set_root(entry);
    }
    Ok(Restore { next_fiber_id, queued, timers, dead })
}

/// Finish a restore on carrier 0 (in place of boot).
pub(crate) fn restore(vm: &mut Vm, r: Restore) {
    for &f in &r.queued {
        sched::schedule(f);
    }
    for t in &r.timers {
        sched::restore_timer(t);
    }
    for &f in &r.dead {
        sched::finish_unsaved(vm, f);
    }
}

// ---- save ---------------------------------------------------------------------------------------

/// Outcomes of finished saves, by the saving fiber's id.
static OUTCOMES: Mutex<Option<HashMap<u64, std::result::Result<Saved, String>>>> = Mutex::new(None);

/// `(%save-image path entry keep-fibers? layered?)`: park this fiber while a
/// helper thread saves; `(%save-image-outcome)` then reports.
pub(crate) fn save_image(vm: &mut Vm, path: Value, entry: Value, keep_fibers: bool, layered: bool) -> Result<Value> {
    if !vm.can_park() {
        return Err(Vm::park_error());
    }
    let path = PathBuf::from(path.as_str());
    let me = vm.current;
    let claim = claim_new(&mut vm.m);
    let w = waiter_new(&mut vm.m, me, Value::NIL, claim, Value::NIL, waiter::NO_CANCEL_WAKE);
    sched::park_on(vm, w, false);
    // Only this thread knows the parked fiber until it is woken.
    let key = sched::root(me);
    let id = me.fiber_id();
    std::thread::Builder::new()
        .name("norn-image".into())
        .spawn(move || {
            let outcome = save_paused(&path, me, entry, keep_fibers, layered);
            OUTCOMES.lock().unwrap().get_or_insert_with(HashMap::new).insert(id, outcome);
            norn_rt::safepoint::while_running(|| {
                if try_claim(claim) {
                    sched::deliver(w, Value::NIL, Value::NIL);
                }
            });
            sched::unroot(key);
        })
        .map_err(|e| Error::new(format!("can't start the image saver: {e}")))?;
    Ok(Value::NIL)
}

fn save_paused(path: &Path, saver: Value, entry: Value, keep: bool, layered: bool) -> std::result::Result<Saved, String> {
    sched::pause_all(PAUSE_LIMIT)?;
    // As a running participant the GC can't start until we are done.
    let r = norn_rt::safepoint::while_running(|| {
        sched::with_timers_held(|timers| {
            let roots = collect_roots(entry, keep, &timers);
            // Fibers waiting on I/O or the blocking pool wait on something
            // the restored process won't have.
            let io_waiting = |f: Value| {
                let w = crate::sched::aget(f, norn_rt::conc::fiber::PARK);
                w.is(Kind::Waiter) && (crate::reactor::holds_waiter(w) || crate::blocking::holds_waiter(w) || crate::main_lane::holds_waiter(w))
            };
            let dead = |f: Value| !keep || f == saver || io_waiting(f);
            norn_rt::image::save(path, &Save { roots: &roots, build: build_id(), layered, dead: &dead })
        })
    });
    sched::resume_all();
    r
}

/// `(%save-image-outcome)`: the result of this fiber's last save, as a map.
pub(crate) fn save_outcome(vm: &mut Vm) -> Result<Value> {
    let id = vm.current.fiber_id();
    let o = OUTCOMES.lock().unwrap().as_mut().and_then(|m| m.remove(&id));
    match o {
        None => Err(Error::new("no image save has finished in this fiber")),
        Some(Err(e)) => Err(Error::new(format!("save-image failed: {e}"))),
        Some(Ok(s)) => {
            let mut kv = Vec::new();
            for (k, n) in [("objects", s.objects), ("patches", s.patches), ("reused", s.reused), ("bytes", s.bytes)] {
                kv.push(rt().keyword(&mut vm.m, None, k));
                kv.push(fix(n as u64));
            }
            crate::prims::hash_map(vm, &kv)
        }
    }
}

/// `(image-info)`: `{:image path :chain [base… path]}`, or nil if this
/// process booted from source.
pub(crate) fn info(vm: &mut Vm) -> Result<Value> {
    let chain = norn_rt::image::loaded_chain();
    let Some(top) = chain.last() else { return Ok(Value::NIL) };
    let s = |vm: &mut Vm, p: &Path| norn_rt::string::str_new(&mut vm.m, &p.to_string_lossy());
    let top = s(vm, top);
    let paths: Vec<Value> = chain.iter().map(|p| s(vm, p)).collect();
    let paths = norn_rt::vector::vec_from_slice(&mut vm.m, &paths);
    let kv = [rt().keyword(&mut vm.m, None, "image"), top, rt().keyword(&mut vm.m, None, "chain"), paths];
    crate::prims::hash_map(vm, &kv)
}
