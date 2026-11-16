//! Images: the heap written to a file and read back (runtime.md §7).
//!
//! An image holds every object reachable from a list of roots that the VM
//! chooses (runtime tables, builtin classes, run queues, timers…). The
//! format is relocatable: references are object numbers, not addresses, so
//! loading allocates each object in the MMTk heap and patches references
//! as it copies. No address has to be free and nothing is mapped at a fixed
//! base.
//!
//! ```text
//! header   HEADER_WORDS little-endian u64s (see H_*)
//! base     UTF-8 path of the image this one is a delta of, padded to 8 bytes
//! area     new objects, then patches: [object number][object words] for base
//!          objects that changed since the base was loaded
//! relocs   bitmap over the area: bit set = the word is a reference
//! roots    one encoded value per root
//! ```
//!
//! A reference is `(object number << 3) | tag`. Word 0 of an object (its
//! layout pointer in memory) is stored as its kind number. Objects are
//! numbered breadth-first from the roots, so the same heap and roots always
//! give the same bytes: build images are reproducible.
//!
//! **Layers.** A delta image names its base. Object numbers below the base's
//! count refer to base objects; loading a delta loads its base chain first.
//! Image objects are immortal (as in a boot-image space): they are GC roots
//! for the life of the process, so their addresses are never reused and a
//! later delta save can recognise them.

use crate::conc::{fiber, waiter};
use crate::func::SEG_HEADER;
use crate::heap::{Mutator, kind_at, scan, size_words};
use crate::object::{Kind, LAYOUTS, Layout};
use crate::runtime::Namespace;
use crate::value::{TAG_CONS, TAG_MASK, TAG_OBJ, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const MAGIC: u64 = u64::from_le_bytes(*b"NORNIMG\0");
pub const FORMAT: u64 = 1;

const H_MAGIC: usize = 0;
const H_FORMAT: usize = 1;
/// Fingerprint of the runtime build that wrote the image.
const H_BUILD: usize = 2;
/// Objects in the base chain (0: not a delta).
const H_BASE_COUNT: usize = 3;
/// Checksum of the base image (its H_SUM).
const H_BASE_SUM: usize = 4;
const H_NEW: usize = 5;
const H_PATCHES: usize = 6;
const H_AREA: usize = 7;
const H_ROOTS: usize = 8;
const H_PATH_BYTES: usize = 9;
/// Checksum of everything after the header.
const H_SUM: usize = 10;
const HEADER_WORDS: usize = 11;

/// Longest base chain we follow (a cycle guard).
const MAX_LAYERS: usize = 32;

fn checksum(words: &[u64]) -> u64 {
    let mut h: u64 = 0x243f_6a88_85a3_08d3;
    for &w in words {
        h = (h ^ w).wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_left(29);
    }
    h ^ words.len() as u64
}

fn is_ref(w: u64) -> bool {
    let t = w & TAG_MASK;
    t == TAG_OBJ || t == TAG_CONS
}

// ---- the loaded chain ---------------------------------------------------------------

struct Layer {
    path: PathBuf,
    sum: u64,
    words: Vec<u64>,
}

/// The images this process was loaded from, and every object they made.
#[derive(Default)]
struct Chain {
    layers: Vec<Layer>,
    /// Object number → the object.
    objs: Vec<Value>,
    /// Object number → where its current encoding lives: (layer << 48) | word.
    locs: Vec<u64>,
}

static CHAIN: Mutex<Option<Chain>> = Mutex::new(None);

/// GC roots: every object loaded from an image (they are immortal).
pub fn for_each_root(f: &mut dyn FnMut(Value)) {
    if let Some(c) = CHAIN.lock().unwrap().as_ref() {
        c.objs.iter().for_each(|&v| f(v));
    }
}

/// The image this process was started from (the top of its chain), if any.
pub fn loaded_path() -> Option<PathBuf> {
    CHAIN.lock().unwrap().as_ref().and_then(|c| c.layers.last().map(|l| l.path.clone()))
}

/// Paths of the loaded chain, base first.
pub fn loaded_chain() -> Vec<PathBuf> {
    CHAIN.lock().unwrap().as_ref().map(|c| c.layers.iter().map(|l| l.path.clone()).collect()).unwrap_or_default()
}

// ---- saving ----------------------------------------------------------------------------------

pub struct Save<'a> {
    /// Values to save as roots, in order (`Loaded::roots` gives them back).
    pub roots: &'a [Value],
    /// Fingerprint of this runtime build.
    pub build: u64,
    /// Write only what changed since the loaded image (a delta layer).
    pub layered: bool,
    /// Fibers to save dead: without their stacks, to be ended with an error
    /// after the restore. (Finished fibers are never dead.)
    pub dead: &'a dyn Fn(Value) -> bool,
}

#[derive(Debug, Default)]
pub struct Saved {
    /// Objects written (new in this layer).
    pub objects: usize,
    /// Base objects written again because they changed.
    pub patches: usize,
    /// Base objects reached but unchanged (not written).
    pub reused: usize,
    pub bytes: usize,
}

struct Writer<'a> {
    spec: &'a Save<'a>,
    /// Address → object number.
    ids: HashMap<u64, u64>,
    /// Every object reached, in discovery order.
    reached: Vec<Value>,
    /// Address → object number, for objects of the loaded chain.
    base: HashMap<u64, u64>,
    base_count: u64,
    next_new: u64,
    /// Claims to save as taken: their waiters belong to dead fibers.
    taken_claims: HashSet<u64>,
    dead_fibers: HashSet<u64>,
    area: Vec<u64>,
    relocs: Vec<u64>,
    /// Still finding objects (weak referents aren't followed, and only count
    /// if something else reaches them).
    discovering: bool,
}

fn addr(v: Value) -> u64 {
    v.ptr() as u64
}

impl<'a> Writer<'a> {
    fn is_dead(&self, f: Value) -> bool {
        self.dead_fibers.contains(&addr(f))
    }

    /// Call `f(offset, value)` for every word of `v` that the image stores
    /// as a value, with the value it stores there. Mostly the GC's scan;
    /// fibers and segments differ (see below).
    fn refs(&self, v: Value, f: &mut dyn FnMut(usize, Value)) {
        let p = v.ptr();
        // SAFETY: offsets come from the object's own layout.
        let word = |i: usize| Value(unsafe { *p.add(i) });
        match kind_at(p) {
            Kind::Fiber => {
                let dead = self.is_dead(v);
                for &i in fiber::VALUE_FIELDS {
                    let o = 2 + i;
                    match i {
                        // A dead fiber keeps no stack and waits on nothing.
                        fiber::SEG | fiber::PARK | fiber::CONDITION if dead => f(o, Value::NIL),
                        fiber::ERROR if dead => f(o, Value::UNBOUND),
                        _ => f(o, word(o)),
                    }
                }
                // NS is a raw &'static Namespace in memory; the image stores
                // the namespace's name.
                let ns = word(2 + fiber::NS).0 as *const Namespace;
                // SAFETY: NS always holds a leaked namespace.
                f(2 + fiber::NS, unsafe { (*ns).name });
            }
            Kind::Weak => {
                // A cleanup releases a per-process resource: never saved.
                f(2 + crate::weak::layout::CLEANUP, Value::NIL);
                if !self.discovering {
                    let r = word(2 + crate::weak::layout::REFERENT);
                    let kept = !is_ref(r.0) || self.ids.contains_key(&addr(r));
                    f(2 + crate::weak::layout::REFERENT, if kept { r } else { Value::NIL });
                }
            }
            Kind::Segment => {
                // The GC scans frames through the owning fiber; the image
                // stores them with the segment, live words only.
                f(3, word(3));
                for i in 0..v.seg_live_words() {
                    let o = 2 + SEG_HEADER + i;
                    f(o, word(o));
                }
            }
            _ => scan(p, &mut |slot| {
                let o = (slot.0.as_usize() - p as usize) / 8;
                f(o, word(o))
            }),
        }
    }

    fn reach(&mut self, v: Value) {
        if !is_ref(v.0) || self.ids.contains_key(&addr(v)) {
            return;
        }
        let id = match self.base.get(&addr(v)) {
            Some(&id) => id,
            None => {
                self.next_new += 1;
                self.next_new - 1
            }
        };
        self.ids.insert(addr(v), id);
        self.reached.push(v);
        if v.is(Kind::Fiber) {
            let st = v.fiber_state().load(std::sync::atomic::Ordering::Acquire);
            if st != fiber::DONE && (st != fiber::QUEUED && st != fiber::PARKED || (self.spec.dead)(v)) {
                self.dead_fibers.insert(addr(v));
            }
        }
    }

    /// Breadth-first from the roots.
    fn discover(&mut self) {
        for &r in self.spec.roots {
            self.reach(r);
        }
        let mut i = 0;
        while i < self.reached.len() {
            let v = self.reached[i];
            let mut found = Vec::new();
            self.refs(v, &mut |_, t| found.push(t));
            for t in found {
                self.reach(t);
            }
            i += 1;
        }
        // Waiters of dead fibers must never be woken: save their claims taken.
        for &v in &self.reached {
            if v.is(Kind::Waiter) {
                let f = v.field(waiter::FIBER);
                if f.is(Kind::Fiber) && self.dead_fibers.contains(&addr(f)) {
                    self.taken_claims.insert(addr(v.field(waiter::CLAIM)));
                }
            }
        }
    }

    fn encode_ref(&self, v: Value) -> u64 {
        (self.ids[&addr(v)] << 3) | (v.0 & TAG_MASK)
    }

    /// The canonical encoding of `v`: words, and which of them are references.
    fn encode(&self, v: Value) -> (Vec<u64>, Vec<usize>) {
        let p = v.ptr();
        let kind = kind_at(p);
        let n = crate::heap::object_words(p);
        // SAFETY: the object has n words.
        let mut w: Vec<u64> = unsafe { std::slice::from_raw_parts(p, n) }.to_vec();
        w[0] = kind as u64;
        match kind {
            Kind::Proto => w[2 + 7] = 0, // native code is per process
            Kind::Fiber => {
                w[2 + fiber::NOTIFY] = 0; // completion tickets are per process
                w[2 + fiber::LOCK] = 0;
                if self.is_dead(v) {
                    w[2 + fiber::STATE] = fiber::DONE;
                }
            }
            Kind::Segment => {
                let live = 2 + SEG_HEADER + v.seg_live_words();
                w[live..].iter_mut().for_each(|x| *x = 0);
            }
            Kind::Claim if self.taken_claims.contains(&addr(v)) => w[2] = 1,
            _ => {}
        }
        let mut refs = Vec::new();
        self.refs(v, &mut |o, val| {
            if is_ref(val.0) {
                w[o] = self.encode_ref(val);
                refs.push(o);
            } else {
                w[o] = val.0;
            }
        });
        (w, refs)
    }

    fn emit(&mut self, words: &[u64], refs: &[usize]) {
        let start = self.area.len();
        self.area.extend_from_slice(words);
        self.relocs.resize(self.area.len().div_ceil(64), 0);
        for &o in refs {
            let bit = start + o;
            self.relocs[bit / 64] |= 1 << (bit % 64);
        }
    }
}

/// The addresses of every object a save of `spec` would write (base layers
/// aside: as a whole image). The caller has stopped every fiber and holds
/// off the GC. (A shaken save asks, to keep only the symbols and keywords
/// something saved refers to.)
pub fn reached(spec: &Save) -> HashSet<u64> {
    let mut w = Writer {
        spec,
        ids: HashMap::new(),
        reached: Vec::new(),
        base: HashMap::new(),
        base_count: 0,
        next_new: 0,
        taken_claims: HashSet::new(),
        dead_fibers: HashSet::new(),
        area: Vec::new(),
        relocs: Vec::new(),
        discovering: true,
    };
    w.discover();
    w.ids.into_keys().collect()
}

/// Is `v` (a reference) among `reached`'s addresses?
pub fn reached_contains(reached: &HashSet<u64>, v: Value) -> bool {
    !is_ref(v.0) || reached.contains(&addr(v))
}

/// Write an image of everything reachable from `spec.roots`. The caller has
/// stopped every fiber and holds off the GC.
pub fn save(path: &Path, spec: &Save) -> Result<Saved, String> {
    let chain = CHAIN.lock().unwrap();
    let base = match (spec.layered, chain.as_ref()) {
        (true, Some(c)) => Some(c),
        (true, None) => return Err("a layered image needs a base: start from an image first".into()),
        (false, _) => None,
    };
    let base_count = base.map_or(0, |c| c.objs.len() as u64);
    let mut w = Writer {
        spec,
        ids: HashMap::new(),
        reached: Vec::new(),
        base: base.map(|c| c.objs.iter().enumerate().map(|(i, &v)| (addr(v), i as u64)).collect()).unwrap_or_default(),
        base_count,
        next_new: base_count,
        taken_claims: HashSet::new(),
        dead_fibers: HashSet::new(),
        area: Vec::new(),
        relocs: Vec::new(),
        discovering: true,
    };
    w.discover();
    w.discovering = false;

    let mut out = Saved::default();
    let reached = std::mem::take(&mut w.reached);
    // New objects, in number order (= discovery order).
    for &v in &reached {
        if w.ids[&addr(v)] >= w.base_count {
            let (words, refs) = w.encode(v);
            w.emit(&words, &refs);
            out.objects += 1;
        }
    }
    // Base objects that changed since they were loaded.
    if let Some(c) = base {
        for &v in &reached {
            let id = w.ids[&addr(v)];
            if id >= w.base_count {
                continue;
            }
            let (words, refs) = w.encode(v);
            let loc = c.locs[id as usize];
            let layer = &c.layers[(loc >> 48) as usize].words;
            let at = (loc & ((1 << 48) - 1)) as usize;
            if layer.get(at..at + words.len()) == Some(&words[..]) {
                out.reused += 1;
                continue;
            }
            let mut rec = vec![id];
            rec.extend_from_slice(&words);
            let refs: Vec<usize> = refs.iter().map(|o| o + 1).collect();
            w.emit(&rec, &refs);
            out.patches += 1;
        }
    }
    let roots: Vec<u64> = spec.roots.iter().map(|&r| if is_ref(r.0) { w.encode_ref(r) } else { r.0 }).collect();

    // The base is named relative to this image when they share a directory.
    let (base_path, base_sum) = match base {
        Some(c) => {
            let top = c.layers.last().unwrap();
            let same_dir = top.path.parent().map(Path::to_path_buf)
                == std::path::absolute(path).ok().and_then(|p| p.parent().map(Path::to_path_buf));
            let name = if same_dir { PathBuf::from(top.path.file_name().unwrap()) } else { top.path.clone() };
            (name.to_string_lossy().into_owned(), top.sum)
        }
        None => (String::new(), 0),
    };
    let mut tail: Vec<u64> = Vec::with_capacity(w.area.len() + w.relocs.len() + roots.len() + 8);
    let pb = base_path.as_bytes();
    for chunk in pb.chunks(8) {
        let mut b = [0u8; 8];
        b[..chunk.len()].copy_from_slice(chunk);
        tail.push(u64::from_le_bytes(b));
    }
    tail.extend_from_slice(&w.area);
    w.relocs.resize(w.area.len().div_ceil(64), 0);
    tail.extend_from_slice(&w.relocs);
    tail.extend_from_slice(&roots);

    let mut header = [0u64; HEADER_WORDS];
    header[H_MAGIC] = MAGIC;
    header[H_FORMAT] = FORMAT;
    header[H_BUILD] = spec.build;
    header[H_BASE_COUNT] = base_count;
    header[H_BASE_SUM] = base_sum;
    header[H_NEW] = out.objects as u64;
    header[H_PATCHES] = out.patches as u64;
    header[H_AREA] = w.area.len() as u64;
    header[H_ROOTS] = roots.len() as u64;
    header[H_PATH_BYTES] = pb.len() as u64;
    header[H_SUM] = checksum(&tail);

    let mut bytes = Vec::with_capacity((HEADER_WORDS + tail.len()) * 8);
    for x in header.iter().chain(tail.iter()) {
        bytes.extend_from_slice(&x.to_le_bytes());
    }
    out.bytes = bytes.len();
    let tmp = path.with_extension("nimg.tmp");
    std::fs::write(&tmp, &bytes).map_err(|e| format!("can't write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("can't write {}: {e}", path.display()))?;
    Ok(out)
}

// ---- loading -------------------------------------------------------------------------------------

pub struct Loaded {
    /// The roots, as saved.
    pub roots: Vec<Value>,
    /// Every fiber in the chain. Their NS word holds the namespace's name
    /// (a symbol) until the VM turns it back into a namespace pointer.
    pub fibers: Vec<Value>,
    /// Every weak reference in the chain (re-registered with the GC).
    pub weaks: Vec<Value>,
    /// Objects created.
    pub objects: usize,
}

fn read_words(path: &Path) -> Result<Vec<u64>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    if bytes.len() % 8 != 0 || bytes.len() < HEADER_WORDS * 8 {
        return Err(format!("{} is not a Norn image (bad length)", path.display()));
    }
    Ok(bytes.as_chunks::<8>().0.iter().map(|c| u64::from_le_bytes(*c)).collect())
}

/// Load an image (and the chain of images it is a delta of) into the heap.
/// Once per process, before anything else has touched the runtime.
pub fn load(m: &mut Mutator, path: &Path, build: u64) -> Result<Loaded, String> {
    let mut chain = CHAIN.lock().unwrap();
    if chain.is_some() {
        return Err("an image is already loaded".into());
    }
    let mut c = Chain::default();
    let mut found = Vec::new();
    let path = std::path::absolute(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let roots = load_layer(m, &path, build, &mut c, &mut found)?;
    // An object patched by a delta was seen twice.
    let mut seen = HashSet::new();
    found.retain(|f| seen.insert(f.0));
    let (fibers, weaks) = found.into_iter().partition(|v| v.is(Kind::Fiber));
    let objects = c.objs.len();
    *chain = Some(c);
    Ok(Loaded { roots, fibers, weaks, objects })
}

fn load_layer(m: &mut Mutator, path: &Path, build: u64, c: &mut Chain, fibers: &mut Vec<Value>) -> Result<Vec<Value>, String> {
    let bad = |what: &str| format!("{} is not a valid Norn image: {what}", path.display());
    if c.layers.len() >= MAX_LAYERS {
        return Err(bad("its base chain is too long (or loops)"));
    }
    let words = read_words(path)?;
    let h = &words[..HEADER_WORDS];
    if h[H_MAGIC] != MAGIC {
        return Err(format!("{} is not a Norn image", path.display()));
    }
    if h[H_FORMAT] != FORMAT {
        return Err(bad(&format!("format {} (this runtime reads format {FORMAT})", h[H_FORMAT])));
    }
    if h[H_BUILD] != build {
        return Err(format!("{} was saved by a different build of Norn; rebuild the image from source", path.display()));
    }
    if checksum(&words[HEADER_WORDS..]) != h[H_SUM] {
        return Err(bad("checksum mismatch (the file is damaged)"));
    }
    let base_count = h[H_BASE_COUNT] as usize;
    let path_bytes = h[H_PATH_BYTES] as usize;
    let (new, patches, area_len, nroots) = (h[H_NEW] as usize, h[H_PATCHES] as usize, h[H_AREA] as usize, h[H_ROOTS] as usize);
    let path_words = path_bytes.div_ceil(8);
    let area_at = HEADER_WORDS + path_words;
    let bits_at = area_at.checked_add(area_len).ok_or_else(|| bad("sizes"))?;
    let roots_at = bits_at + area_len.div_ceil(64);
    if roots_at.checked_add(nroots) != Some(words.len()) {
        return Err(bad("section sizes don't match the file length"));
    }

    // The base chain first.
    if base_count > 0 {
        let raw: Vec<u8> = words[HEADER_WORDS..area_at].iter().flat_map(|w| w.to_le_bytes()).take(path_bytes).collect();
        let name = PathBuf::from(String::from_utf8(raw).map_err(|_| bad("base path"))?);
        let base_path = if name.is_relative() { path.parent().unwrap_or(Path::new(".")).join(&name) } else { name };
        load_layer(m, &base_path, build, c, fibers)
            .map_err(|e| format!("loading {}, the base of {}: {e}", base_path.display(), path.display()))?;
        if c.objs.len() != base_count || c.layers.last().map(|l| l.sum) != Some(h[H_BASE_SUM]) {
            return Err(format!("{} was saved on a different version of {}", path.display(), base_path.display()));
        }
    } else if path_bytes != 0 || patches != 0 {
        return Err(bad("header"));
    }

    let layer_no = c.layers.len() as u64;
    let area = &words[area_at..bits_at];
    let bits = &words[bits_at..roots_at];
    let is_ref_word = |i: usize| bits[i / 64] >> (i % 64) & 1 == 1;
    let size_at = |off: usize| -> Result<(Kind, usize), String> {
        let k = *area.get(off).ok_or_else(|| bad("truncated object"))? as usize;
        if k >= LAYOUTS.len() {
            return Err(bad("unknown object kind"));
        }
        let kind = LAYOUTS[k].kind;
        // Mask raw size fields (keeping the mutable-instance mark, bit 62)
        // so a damaged one can't overflow the size arithmetic.
        let mask = ((1u64 << 48) - 1) | crate::clos::MUTABLE_INSTANCE;
        let n = size_words(kind, |i| area.get(off + 2 + i).map_or(0, |&x| (x & mask) as usize));
        if off + n > area.len() || n < 2 {
            return Err(bad("object overruns the image"));
        }
        Ok((kind, n))
    };

    // 1. Allocate the new objects.
    let first = c.objs.len();
    let mut off = 0;
    for _ in 0..new {
        let (kind, n) = size_at(off)?;
        let p = m.alloc_words(n);
        let tag = if kind == Kind::Cons { TAG_CONS } else { TAG_OBJ };
        c.objs.push(Value(p as u64 | tag));
        c.locs.push(layer_no << 48 | (area_at + off) as u64);
        off += n;
    }

    // 2. Copy them in, relocating references.
    let objs = &c.objs;
    let reloc = |w: u64| -> Result<u64, String> {
        let t = w & TAG_MASK;
        match objs.get((w >> 3) as usize) {
            Some(target) if (t == TAG_OBJ || t == TAG_CONS) && target.0 & TAG_MASK == t => Ok(target.0),
            _ => Err(bad("dangling reference")),
        }
    };
    let fill = |at: usize, n: usize, kind: Kind, dst: *mut u64| -> Result<(), String> {
        for j in 0..n {
            let x = area[at + j];
            let x = if j == 0 {
                &LAYOUTS[kind as usize] as *const Layout as u64
            } else if is_ref_word(at + j) {
                reloc(x)?
            } else {
                x
            };
            // SAFETY: dst was allocated with n words.
            unsafe { dst.add(j).write(x) };
        }
        Ok(())
    };
    off = 0;
    for &obj in &c.objs[first..first + new] {
        let (kind, n) = size_at(off)?;
        fill(off, n, kind, obj.ptr())?;
        if matches!(kind, Kind::Fiber | Kind::Weak) {
            fibers.push(obj);
        }
        off += n;
    }

    // 3. Patches overwrite base objects in place (same kind, same size).
    let mut patched = Vec::with_capacity(patches);
    for _ in 0..patches {
        let id = *area.get(off).ok_or_else(|| bad("truncated patch"))? as usize;
        off += 1;
        if id >= base_count {
            return Err(bad("patch of a non-base object"));
        }
        let (kind, n) = size_at(off)?;
        let target = c.objs[id];
        let tp = target.ptr();
        if kind_at(tp) != kind || crate::heap::object_words(tp) != n {
            return Err(bad("patch changes an object's shape"));
        }
        fill(off, n, kind, tp)?;
        if matches!(kind, Kind::Fiber | Kind::Weak) {
            fibers.push(target);
        }
        patched.push((id, area_at + off));
        off += n;
    }
    if off != area.len() {
        return Err(bad("trailing data in the object area"));
    }
    for (id, at) in patched {
        c.locs[id] = layer_no << 48 | at as u64;
    }

    let roots = words[roots_at..].iter().map(|&w| if is_ref(w) { reloc(w).map(Value) } else { Ok(Value(w)) }).collect::<Result<Vec<_>, _>>()?;
    c.layers.push(Layer { path: path.to_path_buf(), sum: h[H_SUM], words });
    Ok(roots)
}
