//! Line coverage (M9b part 3; `norn test --coverage`).
//!
//! When coverage is on, the expander puts `(norn.core/%cover! id)` before
//! the code it expands from each source line (the first form starting on
//! it, within the form around it), id naming that file and line here. Each
//! run of it counts. A line no code was compiled from (a comment, a blank
//! line, the rest of a multi-line literal) has no counter, so it's neither
//! hit nor missed. Counting is a primitive call, at either tier.

use norn_rt::string::str_new;
use norn_rt::vector::vec_from_slice;
use norn_rt::{Mutator, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// The most lines counted in one process.
const CAPACITY: usize = 1 << 20;

static ON: AtomicBool = AtomicBool::new(false);
static COUNTS: OnceLock<Box<[AtomicU64]>> = OnceLock::new();
static LINES: Mutex<Option<Lines>> = Mutex::new(None);

#[derive(Default)]
struct Lines {
    ids: HashMap<(String, u32), u32>,
    keys: Vec<(String, u32)>,
}

/// `(%cover-on!)`: count from now on (the expander instruments what it
/// expands after this).
pub(crate) fn turn_on() {
    COUNTS.get_or_init(|| (0..CAPACITY).map(|_| AtomicU64::new(0)).collect());
    LINES.lock().unwrap().get_or_insert_with(Lines::default);
    ON.store(true, Ordering::Release);
}

/// `(%cover-on?)`
pub(crate) fn on() -> bool {
    ON.load(Ordering::Acquire)
}

/// `(%cover-line! file line)`: the counter for that line (made the first
/// time), or nil when coverage is off or full.
pub(crate) fn line_id(file: &str, line: u32) -> Value {
    if !on() {
        return Value::NIL;
    }
    let mut guard = LINES.lock().unwrap();
    let Some(l) = guard.as_mut() else { return Value::NIL };
    let key = (file.to_owned(), line);
    if let Some(&id) = l.ids.get(&key) {
        return Value::fix(id as i64);
    }
    if l.keys.len() >= CAPACITY {
        return Value::NIL;
    }
    let id = l.keys.len() as u32;
    l.keys.push(key.clone());
    l.ids.insert(key, id);
    Value::fix(id as i64)
}

/// `(%cover! id)`: count one run of line id.
#[inline]
pub(crate) fn hit(id: i64) {
    if let Some(c) = COUNTS.get()
        && let Some(n) = c.get(id as usize)
    {
        n.fetch_add(1, Ordering::Relaxed);
    }
}

/// `(%cover-counts)`: every counted line, `[[file line count]…]`, in the
/// order they were first compiled.
pub(crate) fn counts(m: &mut Mutator) -> Value {
    let guard = LINES.lock().unwrap();
    let Some(l) = guard.as_ref() else { return vec_from_slice(m, &[]) };
    let c = COUNTS.get();
    let mut rows = Vec::with_capacity(l.keys.len());
    for (i, (file, line)) in l.keys.iter().enumerate() {
        let n = c.map_or(0, |c| c[i].load(Ordering::Relaxed));
        let f = str_new(m, file);
        rows.push(vec_from_slice(m, &[f, Value::fix(*line as i64), Value::fix(n as i64)]));
    }
    vec_from_slice(m, &rows)
}
