//! Classes, instances and generic functions: the runtime half of CLOS
//! (design.md §4, runtime.md §6).
//!
//! The policy (C3 linearisation, slot inheritance, applicable methods,
//! method combination) is written in Skein (lib/core/core.skn). The runtime
//! provides the shapes, `class-of`, slot access, and the *dispatch hot path*:
//! computing a dispatch key and probing a generic function's cache, without
//! locks or allocation.
//!
//! ```text
//! Class:    [name][supers][cpl][slots][layout][builtin][policy][meta][subclasses][metaclass][hooks]
//! policy:   nil, or [flags, keys left out of = and hash, settable keys] (a vector)
//! layout:   Array [k0 … kn-1, d0 … dn-1]: slot keywords, then the values that
//!           instances made under an older layout get for slots they lack.
//!           One array, so a redefinition swaps keys and defaults atomically.
//! Instance: [class][layout][n][slot0 … slotn-1]      immutable
//!           [class][layout][n | MUTABLE][slots…][owner]   mutable (design §4.2):
//!           owner = a fiber, nil (free), or [lock-channel holder-atom] (shared)
//! Generic:  [name][methods][state][epoch][nreq][rest][dispatch][combination][lock][meta][class]
//! state:    Array [mask, eqls, cache, class-epoch]   immutable, swapped atomically
//! cache:    Array [nkeys, cap, count, (k0 … kn-1, em) × cap]   open addressing
//! ```

use crate::array::{array_from, array_new};
use crate::equal::{equal, hash};
use crate::heap::Mutator;
use crate::object::{Kind, alloc};
use crate::value::Value;
use std::sync::atomic::{AtomicU64, Ordering};

// ---- classes ------------------------------------------------------------------

pub mod class {
    pub const NAME: usize = 0;
    pub const SUPERS: usize = 1; // vector of direct superclasses
    pub const CPL: usize = 2; // vector: class precedence list, self first
    pub const SLOTS: usize = 3; // vector of effective slot definitions (maps)
    pub const LAYOUT: usize = 4; // Array [keys…, defaults…] (current class version)
    pub const BUILTIN: usize = 5; // raw: builtin id, or NOT_BUILTIN
    pub const POLICY: usize = 6; // nil, or vector [flags, excluded keys, mutable keys]
    pub const META: usize = 7; // map: :doc …
    pub const SUBCLASSES: usize = 8; // vector of direct subclasses
    pub const METACLASS: usize = 9; // the class's class (M9 part 9.5), or nil: StandardClass
    pub const HOOKS: usize = 10; // raw: which MOP hooks its methods or metaclass specialise (HOOK_*)
    pub const WORDS: usize = 11;
    pub const VALUE_FIELDS: &[usize] = &[NAME, SUPERS, CPL, SLOTS, LAYOUT, POLICY, META, SUBCLASSES, METACLASS];
    /// Hooks: print-object has a method for it; make goes through
    /// make-instance and initialize-instance; slot-value through
    /// slot-value-using-class.
    pub const HOOK_PRINT: u64 = 1;
    pub const HOOK_MAKE: u64 = 2;
    pub const HOOK_SLOT: u64 = 4;
    /// Flags, read on the class itself: some slot has a :type, so making an
    /// instance and setting a slot check it; and an instance of the class
    /// has been made (so a redefinition that adds a slot evaluates its
    /// initform for the instances that lack it).
    pub const FLAG_TYPED: u64 = 8;
    pub const FLAG_MADE: u64 = 16;
    pub const NOT_BUILTIN: u64 = u64::MAX;
    /// Policy flags: instances compare by identity (`(:equality :identity)`,
    /// and every mutable class).
    pub const IDENTITY: i64 = 1;
    /// Policy flags: instances are mutable and owned (`(:mutable)`).
    pub const MUTABLE: i64 = 2;
}

pub fn class_new(m: &mut Mutator, name: Value) -> Value {
    use class::*;
    let empty = crate::runtime::rt().empty_vector;
    let c = alloc(m, Kind::Class, WORDS);
    c.set_field(NAME, name);
    for f in [SUPERS, CPL, SLOTS, SUBCLASSES] {
        c.set_field(f, empty);
    }
    let arr = crate::runtime::rt().empty_array;
    c.set_field(LAYOUT, arr);
    c.set_field(POLICY, Value::NIL);
    c.set_word(BUILTIN, NOT_BUILTIN);
    c.set_field(META, crate::runtime::rt().empty_map);
    c.set_field(METACLASS, Value::NIL);
    c.set_word(HOOKS, 0);
    c
}

/// Is hook bit `bit` set on class c, or any class it inherits from?
pub fn class_hooked(c: Value, bit: u64) -> bool {
    if !c.is(Kind::Class) {
        return false;
    }
    let cpl = c.class_field(class::CPL);
    let mut on = false;
    if cpl.is(Kind::Vector) {
        cpl.vec_for_each(|k| {
            on = k.is(Kind::Class) && k.atomic_word(class::HOOKS).load(Ordering::Acquire) & bit != 0;
            !on
        });
    }
    on
}

/// Set hook bit `bit` on class c (for its subclasses too).
pub fn class_hook(c: Value, bit: u64) {
    c.atomic_word(class::HOOKS).fetch_or(bit, Ordering::AcqRel);
    if bit & class::HOOK_PRINT != 0 {
        ANY_PRINT_HOOK.store(true, Ordering::Release);
    }
}

/// Clear hook or flag bit `bit` on class c.
pub fn class_unhook(c: Value, bit: u64) {
    c.atomic_word(class::HOOKS).fetch_and(!bit, Ordering::AcqRel);
}

/// Is flag `bit` set on class c itself?
#[inline]
pub fn class_flag(c: Value, bit: u64) -> bool {
    c.is(Kind::Class) && c.atomic_word(class::HOOKS).load(Ordering::Relaxed) & bit != 0
}

/// Has any class a print-object method? (If not, printing needn't look.)
pub static ANY_PRINT_HOOK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Classes are redefined in place (their identity is stable), so readers on
/// other threads see each field through an atomic load.
impl Value {
    #[inline]
    pub fn class_field(self, f: usize) -> Value {
        Value(self.atomic_word(f).load(Ordering::Acquire))
    }
    pub fn class_set_field(self, f: usize, v: Value) {
        self.atomic_word(f).store(v.0, Ordering::Release)
    }
}

/// Bumped whenever any class is (re)defined: every dispatch cache built under
/// an older epoch is ignored (class precedence may have changed).
pub static CLASS_EPOCH: AtomicU64 = AtomicU64::new(1);

pub fn class_epoch() -> u64 {
    CLASS_EPOCH.load(Ordering::Acquire)
}

pub fn bump_class_epoch() {
    CLASS_EPOCH.fetch_add(1, Ordering::AcqRel);
}

/// Is `a` a subclass of (or the same as) `b`?
pub fn is_subclass(a: Value, b: Value) -> bool {
    if a == b {
        return true;
    }
    let cpl = a.class_field(class::CPL);
    let mut found = false;
    cpl.vec_for_each(|c| {
        found = c == b;
        !found
    });
    found
}

// ---- builtin classes -----------------------------------------------------------

macro_rules! builtins {
    ($($b:ident => $kw:literal),* $(,)?) => {
        #[repr(usize)]
        #[derive(Copy, Clone, PartialEq, Eq, Debug)]
        pub enum Builtin { $($b),* }
        pub const BUILTIN_NAMES: &[&str] = &[$($kw),*];
    };
}

builtins! {
    Object => "object", Null => "null", Boolean => "boolean", Character => "character",
    Integer => "integer", Float => "float", String => "string", Symbol => "symbol",
    Keyword => "keyword", List => "list", Vector => "vector", Map => "map", Set => "set",
    Seq => "seq", Function => "function", Var => "var", Fiber => "fiber", Channel => "channel",
    Atom => "atom", Scope => "scope", Class => "class", Generic => "generic-function",
    Bytes => "bytes", Handle => "handle", Ratio => "ratio", Regex => "regex", Queue => "queue", Other => "other",
}

const NBUILTINS: usize = Builtin::Other as usize + 1;

static BUILTINS: [AtomicU64; NBUILTINS] = [const { AtomicU64::new(Value::NIL.0) }; NBUILTINS];

/// Register the class object for a builtin id (named as in `BUILTIN_NAMES`).
pub fn register_builtin(name: &str, class: Value) -> bool {
    match BUILTIN_NAMES.iter().position(|&n| n == name) {
        Some(i) => {
            class.set_word(class::BUILTIN, i as u64);
            BUILTINS[i].store(class.0, Ordering::Release);
            true
        }
        None => false,
    }
}

/// Every builtin class slot, in `BUILTIN_NAMES` order (nil if unset).
pub fn builtins() -> Vec<Value> {
    BUILTINS.iter().map(|b| Value(b.load(Ordering::Acquire))).collect()
}

/// Restore the builtin class slots from an image.
pub fn set_builtins(classes: &[Value]) {
    for (b, &c) in BUILTINS.iter().zip(classes) {
        b.store(c.0, Ordering::Release);
    }
}

pub fn builtin(b: Builtin) -> Value {
    Value(BUILTINS[b as usize].load(Ordering::Acquire))
}

/// GC roots: the builtin class objects.
pub fn for_each_root(f: &mut dyn FnMut(Value)) {
    for b in &BUILTINS {
        let v = Value(b.load(Ordering::Acquire));
        if !v.is_nil() {
            f(v);
        }
    }
}

fn builtin_id(v: Value) -> Builtin {
    if v.is_fixnum() {
        return Builtin::Integer;
    }
    if v.is_cons() {
        return Builtin::List;
    }
    if v.is_imm() {
        return if v.is_nil() {
            Builtin::Null
        } else if v.is_bool() {
            Builtin::Boolean
        } else if v.is_char() {
            Builtin::Character
        } else {
            Builtin::Other
        };
    }
    match v.obj_kind() {
        Kind::Float => Builtin::Float,
        Kind::String => Builtin::String,
        Kind::Bytes => Builtin::Bytes,
        Kind::BigInt => Builtin::Integer,
        Kind::Ratio => Builtin::Ratio,
        Kind::Handle => Builtin::Handle,
        Kind::Symbol => Builtin::Symbol,
        Kind::Keyword => Builtin::Keyword,
        Kind::EmptyList => Builtin::List,
        Kind::Vector => Builtin::Vector,
        Kind::Map => Builtin::Map,
        Kind::Set => Builtin::Set,
        Kind::VecSeq | Kind::Range | Kind::Stream => Builtin::Seq,
        Kind::Queue => Builtin::Queue,
        Kind::Sorted if v.sorted_is_set() => Builtin::Set,
        Kind::Sorted => Builtin::Map,
        Kind::Regex => Builtin::Regex,
        Kind::Closure | Kind::Primitive => Builtin::Function,
        Kind::Var => Builtin::Var,
        Kind::Fiber => Builtin::Fiber,
        Kind::Chan => Builtin::Channel,
        Kind::Atom => Builtin::Atom,
        Kind::Scope => Builtin::Scope,
        Kind::Class => Builtin::Class,
        Kind::Generic => Builtin::Generic,
        _ => Builtin::Other,
    }
}

/// `(class-of v)`. Nil only before loom.core has defined the builtin classes.
#[inline]
pub fn class_of(v: Value) -> Value {
    if v.is(Kind::Instance) {
        return v.field(0);
    }
    // (a class or a generic function may have a class of its own: a
    // metaclass, a generic-function class)
    if v.is(Kind::Class) {
        let m = v.class_field(class::METACLASS);
        if !m.is_nil() {
            return m;
        }
    } else if v.is(Kind::Generic) {
        let c = Value(v.atomic_word(generic::CLASS).load(Ordering::Acquire));
        if !c.is_nil() {
            return c;
        }
    }
    builtin(builtin_id(v))
}

// ---- instances ---------------------------------------------------------------------

/// Number of slots described by a layout array.
#[inline]
pub fn layout_len(layout: Value) -> usize {
    layout.arr_len() / 2
}

/// The slot keywords of a layout.
pub fn layout_keys<'a>(layout: Value) -> &'a [Value] {
    &layout.arr_slice()[..layout_len(layout)]
}

pub fn layout_index(layout: Value, key: Value) -> Option<usize> {
    layout_keys(layout).iter().position(|&k| k == key)
}

/// The defaults of a layout (what older instances get for slots they lack).
pub fn layout_defaults<'a>(layout: Value) -> &'a [Value] {
    &layout.arr_slice()[layout_len(layout)..]
}

/// Build a layout from slot keywords and their defaults.
pub fn layout_new(m: &mut Mutator, keys: &[Value], defaults: &[Value]) -> Value {
    let mut v = keys.to_vec();
    v.extend_from_slice(defaults);
    array_from(m, &v)
}

/// The slot-count word's mark for a mutable instance (it has an owner word
/// after its slots).
pub const MUTABLE_INSTANCE: u64 = 1 << 62;

/// A class's policy flags (`class::IDENTITY`, `class::MUTABLE`).
pub fn policy_flags(c: Value) -> i64 {
    let p = c.class_field(class::POLICY);
    if p.is(Kind::Vector) { p.vec_nth(0).as_fixnum() } else { 0 }
}

/// Is `key` one of the policy's keys at `index` (1: excluded, 2: mutable)?
fn policy_has(c: Value, index: usize, key: Value) -> bool {
    let p = c.class_field(class::POLICY);
    p.is(Kind::Vector) && {
        let ks = p.vec_nth(index);
        ks.is(Kind::Vector) && (0..ks.vec_count()).any(|i| ks.vec_nth(i) == key)
    }
}

/// Is slot `key` left out of `=` and `hash` (`:compare false`)?
pub fn slot_excluded(c: Value, key: Value) -> bool {
    policy_has(c, 1, key)
}

/// May slot `key` be set (`:mutable true`)?
pub fn slot_mutable(c: Value, key: Value) -> bool {
    policy_has(c, 2, key)
}

/// A new instance of `class` with `vals` in the order of its current layout.
/// A mutable class's instance gets `owner` (the creating fiber).
pub fn instance_new(m: &mut Mutator, class: Value, vals: &[Value], owner: Value) -> Value {
    let layout = class.class_field(class::LAYOUT);
    debug_assert_eq!(layout_len(layout), vals.len());
    let mutable = policy_flags(class) & class::MUTABLE != 0;
    let hooks = class.atomic_word(class::HOOKS);
    if hooks.load(Ordering::Relaxed) & class::FLAG_MADE == 0 {
        hooks.fetch_or(class::FLAG_MADE, Ordering::Relaxed);
    }
    let o = alloc(m, Kind::Instance, 3 + vals.len() + usize::from(mutable));
    o.set_field(0, class);
    o.set_field(1, layout);
    o.set_word(2, vals.len() as u64 | if mutable { MUTABLE_INSTANCE } else { 0 });
    for (i, &v) in vals.iter().enumerate() {
        o.set_field(3 + i, v);
    }
    if mutable {
        o.set_field(3 + vals.len(), owner);
    }
    o
}

impl Value {
    pub fn instance_class(self) -> Value {
        self.field(0)
    }
    pub fn instance_layout(self) -> Value {
        self.field(1)
    }
    pub fn instance_len(self) -> usize {
        (self.word(2) & !MUTABLE_INSTANCE) as usize
    }
    pub fn instance_mutable(self) -> bool {
        self.word(2) & MUTABLE_INSTANCE != 0
    }
    /// The owner word of a mutable instance (atomic: other fibers claim it).
    pub fn instance_owner(self) -> &'static AtomicU64 {
        self.atomic_word(3 + self.instance_len())
    }
    /// Set slot `i` of the instance's own layout (mutable instances only).
    pub fn instance_set_raw(self, i: usize, v: Value) {
        self.atomic_word(3 + i).store(v.0, Ordering::Release)
    }
    pub fn instance_raw(self, i: usize) -> Value {
        Value(self.atomic_word(3 + i).load(Ordering::Acquire))
    }
}

/// Read slot `key` (a keyword) as the class is *currently* defined. An
/// instance made before a redefinition is read through its own layout:
/// slots it lacks come from the class's DEFAULTS; slots the class no longer
/// has don't exist.
pub fn slot_get(inst: Value, key: Value) -> Option<Value> {
    let class = inst.instance_class();
    let current = class.class_field(class::LAYOUT);
    let own = inst.instance_layout();
    if own == current {
        return layout_index(own, key).map(|i| inst.instance_raw(i));
    }
    let ci = layout_index(current, key)?;
    match layout_index(own, key) {
        Some(i) => Some(inst.instance_raw(i)),
        None => Some(current.arr_get(layout_len(current) + ci)),
    }
}

/// The slot values in the current layout's order.
pub fn instance_values(inst: Value) -> Vec<Value> {
    let current = inst.instance_class().class_field(class::LAYOUT);
    layout_keys(current).iter().map(|&k| slot_get(inst, k).unwrap_or(Value::NIL)).collect()
}

/// `(with inst :k v …)`: a copy (in the current layout) with slots replaced.
/// A copy of a mutable instance belongs to `owner`.
pub fn instance_with(m: &mut Mutator, inst: Value, kvs: &[Value], owner: Value) -> Result<Value, String> {
    let class = inst.instance_class();
    let layout = class.class_field(class::LAYOUT);
    let mut vals = instance_values(inst);
    for kv in kvs.chunks(2) {
        match layout_index(layout, kv[0]) {
            Some(i) => vals[i] = kv[1],
            None => {
                return Err(format!(
                    "{} has no slot {}",
                    crate::print::pr_str(class.class_field(class::NAME), true),
                    crate::print::pr_str(kv[0], true)
                ));
            }
        }
    }
    Ok(instance_new(m, class, &vals, owner))
}

// ---- generic functions -------------------------------------------------------------

pub mod generic {
    pub const NAME: usize = 0;
    pub const METHODS: usize = 1; // vector of method maps (managed in Skein)
    pub const STATE: usize = 2; // dispatch state Array (atomic)
    pub const EPOCH: usize = 3; // raw: bumped when methods change
    pub const NREQ: usize = 4; // raw: required parameters
    pub const REST: usize = 5; // raw: 0/1, has & rest
    pub const DISPATCH: usize = 6; // custom dispatch fn, or nil
    pub const COMBINATION: usize = 7; // method combination name
    pub const LOCK: usize = 8; // raw spinlock (method updates)
    pub const META: usize = 9; // map
    pub const CLASS: usize = 10; // its class (M9 part 9.5), or nil: StandardGenericFunction
    pub const WORDS: usize = 11;
    pub const VALUE_FIELDS: &[usize] = &[NAME, METHODS, STATE, DISPATCH, COMBINATION, META, CLASS];

    // state Array indices
    pub const S_MASK: usize = 0;
    pub const S_EQLS: usize = 1;
    pub const S_CACHE: usize = 2;
    pub const S_CLASS_EPOCH: usize = 3;
}

pub fn generic_new(m: &mut Mutator, name: Value, nreq: usize, rest: bool, combination: Value, dispatch: Value) -> Value {
    use generic::*;
    let state = array_from(m, &[Value::fix(0), Value::NIL, Value::NIL, Value::fix(0)]);
    let g = alloc(m, Kind::Generic, WORDS);
    g.set_field(NAME, name);
    g.set_field(METHODS, crate::runtime::rt().empty_vector);
    g.set_field(STATE, state);
    g.set_word(EPOCH, 0);
    g.set_word(NREQ, nreq as u64);
    g.set_word(REST, rest as u64);
    g.set_field(DISPATCH, dispatch);
    g.set_field(COMBINATION, combination);
    g.set_word(LOCK, 0);
    g.set_field(META, crate::runtime::rt().empty_map);
    g.set_field(CLASS, Value::NIL);
    g
}

impl Value {
    #[inline]
    pub fn gf_state(self) -> Value {
        Value(self.atomic_word(generic::STATE).load(Ordering::Acquire))
    }
    #[inline]
    pub fn gf_epoch(self) -> u64 {
        self.atomic_word(generic::EPOCH).load(Ordering::Acquire)
    }
    pub fn gf_nreq(self) -> usize {
        self.word(generic::NREQ) as usize
    }
    pub fn gf_rest(self) -> bool {
        self.word(generic::REST) != 0
    }
}

/// Replace a generic function's methods (if they are still `old`) and reset
/// its dispatch state. Returns false if another thread changed them first.
pub fn gf_set_methods(m: &mut Mutator, gf: Value, old: Value, new: Value, mask: u64, eqls: Value) -> bool {
    let state = array_from(m, &[Value::fix(mask as i64), eqls, Value::NIL, Value::fix(0)]);
    let _g = crate::conc::spin_lock(gf, generic::LOCK);
    if gf.field(generic::METHODS) != old {
        return false;
    }
    gf.atomic_word(generic::METHODS).store(new.0, Ordering::Release);
    gf.atomic_word(generic::STATE).store(state.0, Ordering::Release);
    gf.atomic_word(generic::EPOCH).fetch_add(1, Ordering::AcqRel);
    true
}

/// Compute the dispatch key for `args` under `state`: for each specialised
/// position, the argument itself if it is one of that position's `eql`
/// values, otherwise its class. Returns false if a class is missing.
#[inline]
pub fn dispatch_key(state: Value, args: &[Value], key: &mut Vec<Value>) {
    key.clear();
    let mut mask = state.arr_get(generic::S_MASK).as_fixnum() as u64;
    let eqls = state.arr_get(generic::S_EQLS);
    while mask != 0 {
        let i = mask.trailing_zeros() as usize;
        mask &= mask - 1;
        let a = args[i];
        let k = if !eqls.is_nil() && i < eqls.vec_count() {
            let s = eqls.vec_nth(i);
            if !s.is_nil() && s.set_contains(a) { a } else { class_of(a) }
        } else {
            class_of(a)
        };
        key.push(k);
    }
}

fn key_hash(key: &[Value]) -> u64 {
    // (a class by its identity hash directly: hash would set up a frame for
    // it; M10 part 6.4. Building and probing both hash this way.)
    key.iter().fold(0x9e37_79b9_u64, |h, &k| {
        let kh = if k.is(Kind::Class) { k.identity_hash() } else { hash(k) };
        crate::equal::mix64(h ^ kh)
    })
}

const C_NKEYS: usize = 0;
const C_CAP: usize = 1;
const C_COUNT: usize = 2;
const C_HDR: usize = 3;

/// Probe a dispatch cache. Lock-free: caches are immutable once published.
pub fn cache_lookup(cache: Value, key: &[Value]) -> Option<Value> {
    if cache.is_nil() {
        return None;
    }
    let nk = cache.arr_get(C_NKEYS).as_fixnum() as usize;
    if nk != key.len() {
        return None;
    }
    let cap = cache.arr_get(C_CAP).as_fixnum() as usize;
    let stride = nk + 1;
    let mut i = (key_hash(key) as usize) & (cap - 1);
    loop {
        let base = C_HDR + i * stride;
        let k0 = cache.arr_get(base);
        if k0 == Value::UNBOUND {
            return None;
        }
        // (keys are classes, compared by identity, or eql values: identity
        // first, so the usual hit allocates nothing; M10 part 6.4)
        if (0..nk).all(|j| {
            let c = cache.arr_get(base + j);
            c == key[j] || equal(c, key[j])
        }) {
            return Some(cache.arr_get(base + nk));
        }
        i = (i + 1) & (cap - 1);
    }
}

fn cache_build(m: &mut Mutator, nk: usize, cap: usize, entries: &[(Vec<Value>, Value)]) -> Value {
    let stride = nk + 1;
    let c = array_new(m, C_HDR + cap * stride, Value::UNBOUND);
    c.arr_set(C_NKEYS, Value::fix(nk as i64));
    c.arr_set(C_CAP, Value::fix(cap as i64));
    c.arr_set(C_COUNT, Value::fix(entries.len() as i64));
    for (k, em) in entries {
        let mut i = (key_hash(k) as usize) & (cap - 1);
        while c.arr_get(C_HDR + i * stride) != Value::UNBOUND {
            i = (i + 1) & (cap - 1);
        }
        for (j, &kj) in k.iter().enumerate() {
            c.arr_set(C_HDR + i * stride + j, kj);
        }
        c.arr_set(C_HDR + i * stride + nk, *em);
    }
    c
}

fn cache_entries(cache: Value) -> Vec<(Vec<Value>, Value)> {
    let mut out = Vec::new();
    if cache.is_nil() {
        return out;
    }
    let nk = cache.arr_get(C_NKEYS).as_fixnum() as usize;
    let cap = cache.arr_get(C_CAP).as_fixnum() as usize;
    for i in 0..cap {
        let base = C_HDR + i * (nk + 1);
        if cache.arr_get(base) != Value::UNBOUND {
            out.push(((0..nk).map(|j| cache.arr_get(base + j)).collect(), cache.arr_get(base + nk)));
        }
    }
    out
}

/// Install `key → em`, but only if neither the methods (`epoch`) nor the
/// class graph (`class_epoch`) changed since the miss began, so a stale
/// effective method can never be cached. Returns whether it was installed.
pub fn cache_put(m: &mut Mutator, gf: Value, epoch: u64, class_epoch: u64, key: &[Value], em: Value) -> bool {
    loop {
        let state = gf.gf_state();
        if gf.gf_epoch() != epoch || CLASS_EPOCH.load(Ordering::Acquire) != class_epoch {
            return false;
        }
        let old_cache = state.arr_get(generic::S_CACHE);
        let same_epoch = state.arr_get(generic::S_CLASS_EPOCH).as_fixnum() as u64 == class_epoch;
        let mut entries = if same_epoch { cache_entries(old_cache) } else { Vec::new() };
        entries.retain(|(k, _)| k.len() != key.len() || !k.iter().zip(key).all(|(&a, &b)| equal(a, b)));
        entries.push((key.to_vec(), em));
        let cap = (entries.len() * 2).next_power_of_two().max(4);
        let cache = cache_build(m, key.len(), cap, &entries);
        let new_state =
            array_from(m, &[state.arr_get(generic::S_MASK), state.arr_get(generic::S_EQLS), cache, Value::fix(class_epoch as i64)]);
        if gf.atomic_word(generic::STATE).compare_exchange(state.0, new_state.0, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            return true;
        }
    }
}

/// What tier 1 can link a call of generic function gf to (M10 part 6.4):
/// when its dispatch cache, current under this class epoch, holds one
/// entry, on one argument position, with no eql specializers and no custom
/// dispatch: (that position, the class there, the effective method, gf's
/// method epoch, the class epoch). A call whose argument there is an
/// instance of that class, with both epochs unchanged, is that method's.
pub fn single_dispatch(gf: Value) -> Option<(usize, Value, Value, u64, u64)> {
    if !gf.field(generic::DISPATCH).is_nil() {
        return None;
    }
    let epoch = gf.gf_epoch();
    let class_epoch = CLASS_EPOCH.load(Ordering::Acquire);
    let state = gf.gf_state();
    if state.arr_get(generic::S_CLASS_EPOCH).as_fixnum() as u64 != class_epoch || !state.arr_get(generic::S_EQLS).is_nil() {
        return None;
    }
    let mask = state.arr_get(generic::S_MASK).as_fixnum() as u64;
    if mask.count_ones() != 1 {
        return None;
    }
    let cache = state.arr_get(generic::S_CACHE);
    if cache.is_nil() {
        return None;
    }
    let entries = cache_entries(cache);
    match entries.as_slice() {
        [(key, em)] if key.len() == 1 => Some((mask.trailing_zeros() as usize, key[0], *em, epoch, class_epoch)),
        _ => None,
    }
}

/// The hot path: the cached effective method for these arguments, if any.
#[inline]
pub fn dispatch_cached(gf: Value, args: &[Value], key: &mut Vec<Value>) -> Option<Value> {
    let state = gf.gf_state();
    if state.arr_get(generic::S_CLASS_EPOCH).as_fixnum() as u64 != CLASS_EPOCH.load(Ordering::Acquire) {
        return None;
    }
    dispatch_key(state, args, key);
    cache_lookup(state.arr_get(generic::S_CACHE), key)
}
