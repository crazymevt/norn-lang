//! Process-wide runtime state: interned symbols and keywords, namespaces,
//! shared singletons.
//!
//! The tables live in Rust; an image saves their contents as roots and
//! `init_from_image` rebuilds them (runtime.md §7).

use crate::array::array_new;
use crate::equal::{hash_str, mix64};
use crate::func::var_new;
use crate::heap::Mutator;
use crate::map::{map_make, set_make};
use crate::object::{Kind, alloc};
use crate::string::{SYM_HASH, SYM_NAME, SYM_NS, str_new};
use crate::value::Value;
use crate::vector::vec_make_empty;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, RwLock};

type NameKey = (Option<String>, String);

pub struct Namespace {
    pub name: Value,
    mappings: RwLock<HashMap<Value, Value>>,
    /// alias symbol → the name of the namespace it stands for (`str` →
    /// `skein.string`), from `(require '[skein.string :as str])`.
    aliases: RwLock<HashMap<Value, Value>>,
}

impl Namespace {
    /// The var mapped to unqualified symbol `sym` in this namespace.
    pub fn lookup(&self, sym: Value) -> Option<Value> {
        self.mappings.read().unwrap().get(&sym).copied()
    }

    /// Find or create the var `this-ns/sym`.
    pub fn intern(&self, m: &mut Mutator, sym: Value) -> Value {
        let mut map = self.mappings.write().unwrap();
        if let Some(&v) = map.get(&sym)
            && v.var_ns() == self.name
        {
            return v;
        }
        let v = var_new(m, self.name, sym);
        map.insert(sym, v);
        v
    }

    /// All (symbol, var) pairs, unordered.
    pub fn mappings(&self) -> Vec<(Value, Value)> {
        self.mappings.read().unwrap().iter().map(|(&k, &v)| (k, v)).collect()
    }

    /// Remove `sym`'s mapping if it is one of this namespace's own vars.
    /// Code already compiled against the var keeps it; new code no longer
    /// resolves the name. True if it was removed.
    pub fn unmap_own(&self, sym: Value) -> bool {
        let mut map = self.mappings.write().unwrap();
        if map.get(&sym).is_some_and(|v| v.var_ns() == self.name) {
            map.remove(&sym);
            true
        } else {
            false
        }
    }

    /// Map `sym` in this namespace to `var` of another namespace (`:refer`).
    pub fn refer(&self, sym: Value, var: Value) {
        self.mappings.write().unwrap().insert(sym, var);
    }

    pub fn add_alias(&self, alias: Value, target: Value) {
        self.aliases.write().unwrap().insert(alias, target);
    }

    /// The namespace name `alias` stands for here, if it is an alias.
    pub fn alias_target(&self, alias: Value) -> Option<Value> {
        self.aliases.read().unwrap().get(&alias).copied()
    }

    /// All (alias, namespace name) pairs, unordered.
    pub fn aliases(&self) -> Vec<(Value, Value)> {
        self.aliases.read().unwrap().iter().map(|(&k, &v)| (k, v)).collect()
    }
}

pub struct Runtime {
    symbols: Mutex<HashMap<NameKey, Value>>,
    keywords: Mutex<HashMap<NameKey, Value>>,
    namespaces: Mutex<HashMap<Value, &'static Namespace>>,
    pub empty_list: Value,
    pub empty_vector: Value,
    pub empty_map: Value,
    pub empty_set: Value,
    pub empty_array: Value,
}

static RT: OnceLock<Runtime> = OnceLock::new();

/// The global runtime. Panics if `init` hasn't run.
#[inline]
pub fn rt() -> &'static Runtime {
    RT.get().expect("skein runtime not initialised")
}

/// Initialise the runtime (idempotent).
pub fn init(m: &mut Mutator) {
    RT.get_or_init(|| {
        let empty_array = array_new(m, 0, Value::NIL);
        let empty_map = map_make(m, 0, Value::NIL);
        Runtime {
            symbols: Mutex::new(HashMap::new()),
            keywords: Mutex::new(HashMap::new()),
            namespaces: Mutex::new(HashMap::new()),
            empty_list: alloc(m, Kind::EmptyList, 0),
            empty_vector: vec_make_empty(m, empty_array),
            empty_set: set_make(m, empty_map),
            empty_map,
            empty_array,
        }
    });
}

/// The runtime's tables as an image saves them (runtime.md §7.1), in a
/// deterministic order: symbols and keywords sorted by name, namespaces by
/// name, and each namespace's mappings by symbol.
pub struct Tables {
    pub symbols: Vec<Value>,
    pub keywords: Vec<Value>,
    /// (namespace name, [(symbol, var)…], [(alias, namespace name)…])
    pub namespaces: Vec<NsTable>,
    /// empty list, vector, map, set, array
    pub empties: [Value; 5],
}

pub type NsTable = (Value, Vec<(Value, Value)>, Vec<(Value, Value)>);

fn name_key(v: Value) -> NameKey {
    let ns = v.field(SYM_NS);
    (if ns.is_nil() { None } else { Some(ns.as_str().to_owned()) }, v.field(SYM_NAME).as_str().to_owned())
}

fn sorted_by_name(mut v: Vec<Value>) -> Vec<Value> {
    v.sort_by_cached_key(|&s| name_key(s));
    v
}

impl Runtime {
    pub fn tables(&self) -> Tables {
        let symbols = sorted_by_name(self.symbols.lock().unwrap().values().copied().collect());
        let keywords = sorted_by_name(self.keywords.lock().unwrap().values().copied().collect());
        let mut namespaces: Vec<NsTable> = self
            .all_namespaces()
            .into_iter()
            .map(|ns| {
                let mut m = ns.mappings();
                m.sort_by_cached_key(|&(k, _)| name_key(k));
                let mut a = ns.aliases();
                a.sort_by_cached_key(|&(k, _)| name_key(k));
                (ns.name, m, a)
            })
            .collect();
        namespaces.sort_by_cached_key(|&(n, _, _)| name_key(n));
        Tables {
            symbols,
            keywords,
            namespaces,
            empties: [self.empty_list, self.empty_vector, self.empty_map, self.empty_set, self.empty_array],
        }
    }
}

/// Install tables loaded from an image (instead of `init`). Fails if the
/// runtime was already initialised.
pub fn init_from_image(t: Tables) -> Result<(), String> {
    let index = |vs: &[Value]| -> HashMap<NameKey, Value> { vs.iter().map(|&v| (name_key(v), v)).collect() };
    let namespaces = t
        .namespaces
        .iter()
        .map(|(name, maps, aliases)| {
            let ns: &'static Namespace = Box::leak(Box::new(Namespace {
                name: *name,
                mappings: RwLock::new(maps.iter().copied().collect()),
                aliases: RwLock::new(aliases.iter().copied().collect()),
            }));
            (*name, ns)
        })
        .collect();
    let [empty_list, empty_vector, empty_map, empty_set, empty_array] = t.empties;
    let r = Runtime {
        symbols: Mutex::new(index(&t.symbols)),
        keywords: Mutex::new(index(&t.keywords)),
        namespaces: Mutex::new(namespaces),
        empty_list,
        empty_vector,
        empty_map,
        empty_set,
        empty_array,
    };
    RT.set(r).map_err(|_| "the runtime is already initialised".to_string())
}

/// A fresh mutator with the runtime initialised, for unit tests.
pub fn init_for_tests() -> Mutator {
    let mut m = Mutator::new();
    init(&mut m);
    m
}

fn intern_in(table: &Mutex<HashMap<NameKey, Value>>, m: &mut Mutator, kind: Kind, ns: Option<&str>, name: &str) -> Value {
    let key = (ns.map(str::to_owned), name.to_owned());
    let mut t = table.lock().unwrap();
    if let Some(&v) = t.get(&key) {
        return v;
    }
    let ns_v = match ns {
        Some(s) => str_new(m, s),
        None => Value::NIL,
    };
    let name_v = str_new(m, name);
    let s = alloc(m, kind, 3);
    s.set_field(SYM_NS, ns_v);
    s.set_field(SYM_NAME, name_v);
    let h = mix64(hash_str(name) ^ ns.map(hash_str).unwrap_or(0).rotate_left(17) ^ kind as u64);
    s.set_word(SYM_HASH, h);
    t.insert(key, s);
    s
}

/// Split `"ns/name"` → (Some(ns), name). `"/"` and `"a/"` are names.
pub fn split_qualified(s: &str) -> (Option<&str>, &str) {
    match s.find('/') {
        Some(i) if i > 0 && i + 1 < s.len() => (Some(&s[..i]), &s[i + 1..]),
        _ => (None, s),
    }
}

impl Runtime {
    pub fn symbol(&self, m: &mut Mutator, ns: Option<&str>, name: &str) -> Value {
        intern_in(&self.symbols, m, Kind::Symbol, ns, name)
    }

    /// A symbol with this name that isn't interned: it prints as the name
    /// but is identical only to itself (the expander's local variables).
    pub fn symbol_uninterned(&self, m: &mut Mutator, name: &str) -> Value {
        let name_v = str_new(m, name);
        let s = alloc(m, Kind::Symbol, 3);
        s.set_field(SYM_NS, Value::NIL);
        s.set_field(SYM_NAME, name_v);
        s.set_word(SYM_HASH, mix64(hash_str(name) ^ Kind::Symbol as u64));
        s
    }

    /// Intern from text, splitting on the first `/`.
    pub fn symbol_from(&self, m: &mut Mutator, s: &str) -> Value {
        let (ns, name) = split_qualified(s);
        self.symbol(m, ns, name)
    }

    pub fn keyword(&self, m: &mut Mutator, ns: Option<&str>, name: &str) -> Value {
        intern_in(&self.keywords, m, Kind::Keyword, ns, name)
    }

    pub fn keyword_from(&self, m: &mut Mutator, s: &str) -> Value {
        let (ns, name) = split_qualified(s);
        self.keyword(m, ns, name)
    }

    pub fn find_ns(&self, name: Value) -> Option<&'static Namespace> {
        self.namespaces.lock().unwrap().get(&name).copied()
    }

    pub fn find_or_create_ns(&self, name: Value) -> &'static Namespace {
        let mut t = self.namespaces.lock().unwrap();
        t.entry(name).or_insert_with(|| {
            Box::leak(Box::new(Namespace {
                name,
                mappings: RwLock::new(HashMap::new()),
                aliases: RwLock::new(HashMap::new()),
            }))
        })
    }

    /// Visit every value the runtime tables keep alive (GC roots).
    pub fn visit_roots(&self, f: &mut dyn FnMut(Value)) {
        self.symbols.lock().unwrap().values().for_each(|&v| f(v));
        self.keywords.lock().unwrap().values().for_each(|&v| f(v));
        for ns in self.namespaces.lock().unwrap().values() {
            f(ns.name);
            for (k, v) in ns.mappings().into_iter().chain(ns.aliases()) {
                f(k);
                f(v);
            }
        }
        for v in [self.empty_list, self.empty_vector, self.empty_map, self.empty_set, self.empty_array] {
            f(v);
        }
    }

    pub fn all_namespaces(&self) -> Vec<&'static Namespace> {
        self.namespaces.lock().unwrap().values().copied().collect()
    }

    /// Resolve `sym` to a var as seen from namespace `ns`.
    ///
    /// Qualified symbols look in the named namespace, where the qualifier may
    /// be an alias of `ns`. Unqualified symbols look in `ns`, then fall back
    /// to `core` (every namespace refers skein.core).
    pub fn resolve(&self, m: &mut Mutator, ns: &Namespace, core: &Namespace, sym: Value) -> Option<Value> {
        match sym.sym_ns() {
            Some(q) => {
                let q = self.symbol(m, None, q);
                let target = self.find_ns(ns.alias_target(q).unwrap_or(q))?;
                let local = self.symbol(m, None, sym.sym_name());
                target.lookup(local)
            }
            None => ns.lookup(sym).or_else(|| core.lookup(sym)),
        }
    }
}

/// GC roots held by the runtime tables (nothing before `init`).
pub fn for_each_root(f: &mut dyn FnMut(Value)) {
    if let Some(r) = RT.get() {
        r.visit_roots(f);
    }
    crate::clos::for_each_root(f);
    crate::weak::for_each_root(f);
}
