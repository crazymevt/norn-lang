//! Process-wide runtime state: interned symbols and keywords, namespaces,
//! shared singletons.
//!
//! M0 keeps these tables in Rust. In M5 they move into the heap so images can
//! save them (runtime.md §7).

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
    RT.get().expect("norn runtime not initialised")
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
        t.entry(name)
            .or_insert_with(|| Box::leak(Box::new(Namespace { name, mappings: RwLock::new(HashMap::new()) })))
    }

    /// Visit every value the runtime tables keep alive (GC roots).
    pub fn visit_roots(&self, f: &mut dyn FnMut(Value)) {
        self.symbols.lock().unwrap().values().for_each(|&v| f(v));
        self.keywords.lock().unwrap().values().for_each(|&v| f(v));
        for ns in self.namespaces.lock().unwrap().values() {
            f(ns.name);
            for (k, v) in ns.mappings() {
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
    /// Qualified symbols look in the named namespace. Unqualified symbols look
    /// in `ns`, then fall back to `core` (every namespace refers norn.core).
    pub fn resolve(&self, m: &mut Mutator, ns: &Namespace, core: &Namespace, sym: Value) -> Option<Value> {
        match sym.sym_ns() {
            Some(q) => {
                let target = self.find_ns(self.symbol(m, None, q))?;
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
}
