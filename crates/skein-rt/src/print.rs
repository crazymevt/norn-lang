//! Printer. `readably = true` is `pr` (output can be read back);
//! `false` is `print` (strings and chars appear raw).

use crate::object::Kind;
use crate::seq::SeqIter;
use crate::value::Value;
use std::fmt::Write;

pub fn pr_str(v: Value, readably: bool) -> String {
    let mut s = String::new();
    write_value(&mut s, v, readably);
    s
}

pub fn fmt_float(f: f64) -> String {
    if f.is_nan() {
        "##NaN".into()
    } else if f.is_infinite() {
        if f > 0.0 { "##Inf".into() } else { "##-Inf".into() }
    } else {
        format!("{f:?}")
    }
}

fn char_name(c: char) -> Option<&'static str> {
    Some(match c {
        '\n' => "newline",
        ' ' => "space",
        '\t' => "tab",
        '\r' => "return",
        '\u{8}' => "backspace",
        '\u{c}' => "formfeed",
        '\0' => "nul",
        _ => return None,
    })
}

fn write_str_escaped(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// What is left to write: values, and the text between and after them.
/// The printer keeps it on a stack of its own instead of recursing, so a
/// value nested a million deep prints without exhausting the native stack.
enum Work {
    Value(Value, bool),
    Text(&'static str),
    Owned(String),
}

/// Queue items (written with spaces between them), then `close`; the
/// opening text is written already.
fn push_seq(stack: &mut Vec<Work>, items: Vec<Value>, close: &'static str, readably: bool) {
    stack.push(Work::Text(close));
    for (i, x) in items.into_iter().enumerate().rev() {
        stack.push(Work::Value(x, readably));
        if i > 0 {
            stack.push(Work::Text(" "));
        }
    }
}

/// Queue `k v, k v…` pairs, then `close`.
fn push_entries(stack: &mut Vec<Work>, entries: Vec<(Value, Value)>, close: &'static str, readably: bool) {
    stack.push(Work::Text(close));
    for (i, (k, x)) in entries.into_iter().enumerate().rev() {
        stack.push(Work::Value(x, readably));
        stack.push(Work::Text(" "));
        stack.push(Work::Value(k, readably));
        if i > 0 {
            stack.push(Work::Text(", "));
        }
    }
}

fn write_qualified(out: &mut String, v: Value) {
    if let Some(ns) = v.sym_ns() {
        out.push_str(ns);
        out.push('/');
    }
    out.push_str(v.sym_name());
}

fn write_name(out: &mut String, name: Value) {
    if name.is_symbol() {
        write_qualified(out, name);
    } else {
        out.push_str("fn");
    }
}

thread_local! {
    /// While a print primitive runs: text for instances whose class has a
    /// print-object method (M9 part 9.5), by address, worked out first in
    /// Skein (the printer can't call it).
    static OVERRIDES: std::cell::RefCell<Vec<(u64, String)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Run f with `texts` (instance address, text) printed in those
/// instances' places.
pub fn with_overrides<R>(texts: Vec<(u64, String)>, f: impl FnOnce() -> R) -> R {
    if texts.is_empty() {
        return f();
    }
    let saved = OVERRIDES.with(|o| std::mem::replace(&mut *o.borrow_mut(), texts));
    let r = f();
    OVERRIDES.with(|o| *o.borrow_mut() = saved);
    r
}

pub fn write_value(out: &mut String, v: Value, readably: bool) {
    let mut stack = vec![Work::Value(v, readably)];
    while let Some(w) = stack.pop() {
        match w {
            Work::Text(t) => out.push_str(t),
            Work::Owned(t) => out.push_str(&t),
            Work::Value(v, readably) => write_one(out, &mut stack, v, readably),
        }
    }
}

/// Write v itself, queueing what it contains.
fn write_one(out: &mut String, stack: &mut Vec<Work>, v: Value, readably: bool) {
    if v.is_fixnum() {
        let _ = write!(out, "{}", v.as_fixnum());
        return;
    }
    if v.is_cons() {
        out.push('(');
        push_seq(stack, SeqIter::List(v).collect(), ")", readably);
        return;
    }
    if v.is_imm() {
        match v {
            Value::NIL => out.push_str("nil"),
            Value::TRUE => out.push_str("true"),
            Value::FALSE => out.push_str("false"),
            Value::UNBOUND => out.push_str("#<unbound>"),
            Value::EOF => out.push_str("#<eof>"),
            _ if v.is_char() => {
                let c = v.as_char();
                if readably {
                    out.push('\\');
                    match char_name(c) {
                        Some(n) => out.push_str(n),
                        None => out.push(c),
                    }
                } else {
                    out.push(c);
                }
            }
            _ => {
                let _ = write!(out, "#<imm {:#x}>", v.0);
            }
        }
        return;
    }
    if !v.is_obj() {
        let _ = write!(out, "#<bad value {:#x}>", v.0);
        return;
    }
    match v.obj_kind() {
        Kind::String => {
            if readably {
                write_str_escaped(out, v.as_str())
            } else {
                out.push_str(v.as_str())
            }
        }
        Kind::Symbol => write_qualified(out, v),
        Kind::Keyword => {
            out.push(':');
            write_qualified(out, v);
        }
        Kind::Float => out.push_str(&fmt_float(v.as_f64())),
        Kind::EmptyList => out.push_str("()"),
        Kind::Vector => {
            let mut items = Vec::with_capacity(v.vec_count());
            v.vec_for_each(|x| {
                items.push(x);
                true
            });
            out.push('[');
            push_seq(stack, items, "]", readably);
        }
        Kind::VecSeq | Kind::Range => {
            out.push('(');
            push_seq(stack, SeqIter::new(v).unwrap().collect(), ")", readably);
        }
        Kind::Map => {
            let mut entries = Vec::new();
            v.map_for_each(|k, x| {
                entries.push((k, x));
                true
            });
            out.push('{');
            push_entries(stack, entries, "}", readably);
        }
        Kind::Set => {
            let mut items = Vec::new();
            v.set_for_each(|x| {
                items.push(x);
                true
            });
            out.push_str("#{");
            push_seq(stack, items, "}", readably);
        }
        Kind::Sorted => {
            let entries = crate::colls::sorted_entries(v);
            if v.sorted_is_set() {
                out.push_str("#{");
                push_seq(stack, entries.into_iter().map(|e| e.0).collect(), "}", readably);
            } else {
                out.push('{');
                push_entries(stack, entries, "}", readably);
            }
        }
        Kind::Queue => {
            out.push_str("#queue [");
            push_seq(stack, crate::colls::queue_items(v), "]", readably);
        }
        Kind::Transient => out.push_str("#<transient>"),
        Kind::Regex => {
            let pat = v.field(0).as_str();
            if readably {
                // #"…": the pattern as written, with bare quotes escaped.
                out.push_str("#\"");
                let mut esc = false;
                for c in pat.chars() {
                    if esc {
                        esc = false;
                    } else if c == '\\' {
                        esc = true;
                    } else if c == '"' {
                        out.push('\\');
                    }
                    out.push(c);
                }
                out.push('"');
            } else {
                out.push_str(pat);
            }
        }
        Kind::Var => {
            out.push_str("#'");
            out.push_str(v.var_ns().sym_name());
            out.push('/');
            out.push_str(v.var_name().sym_name());
        }
        Kind::Closure => {
            out.push_str("#<fn ");
            write_name(out, v.closure_template().template_name());
            out.push('>');
        }
        Kind::Primitive => {
            out.push_str("#<primitive ");
            write_qualified(out, v.prim_name());
            out.push('>');
        }
        Kind::Proto => {
            out.push_str("#<proto ");
            write_name(out, v.proto_name());
            out.push('>');
        }
        Kind::Fiber => {
            use crate::conc::fiber;
            let state = match v.fiber_state().load(std::sync::atomic::Ordering::Acquire) {
                fiber::QUEUED => "queued",
                fiber::RUNNING | fiber::NOTIFIED => "running",
                fiber::PARKED => "parked",
                _ => "done",
            };
            let _ = write!(out, "#<fiber {} {state}>", v.fiber_id());
        }
        Kind::Atom => {
            out.push_str("#<atom ");
            stack.push(Work::Text(">"));
            stack.push(Work::Value(v.atom_get(), readably));
        }
        Kind::Chan => out.push_str("#<channel>"),
        Kind::Class => {
            out.push_str("#<class ");
            stack.push(Work::Text(">"));
            stack.push(Work::Value(v.class_field(crate::clos::class::NAME), readably));
        }
        Kind::Generic => {
            out.push_str("#<generic ");
            stack.push(Work::Text(">"));
            stack.push(Work::Value(v.field(crate::clos::generic::NAME), readably));
        }
        Kind::Instance
            if OVERRIDES.with(|o| {
                let o = o.borrow();
                match o.iter().find(|(a, _)| *a == v.0) {
                    Some((_, t)) => {
                        out.push_str(t);
                        true
                    }
                    None => false,
                }
            }) => {}
        Kind::Instance => {
            // #user/circle{:radius 3}, like a Clojure record.
            let class = v.instance_class();
            out.push('#');
            let layout = class.class_field(crate::clos::class::LAYOUT);
            let entries: Vec<(Value, Value)> =
                crate::clos::layout_keys(layout).iter().copied().zip(crate::clos::instance_values(v).iter().copied()).collect();
            push_entries(stack, entries, "}", readably);
            stack.push(Work::Text("{"));
            stack.push(Work::Value(class.class_field(crate::clos::class::NAME), readably));
        }
        Kind::Scope => out.push_str("#<scope>"),
        Kind::Weak => out.push_str("#<weak-ref>"),
        Kind::ByteArray => {
            let _ = write!(out, "#<byte-array {}>", v.as_bytes().len());
        }
        // Printing never forces: the print primitives force a stream first.
        Kind::Stream if crate::seq::first_unrealized(v).is_some() => out.push_str("#<stream>"),
        Kind::Stream => stack.push(Work::Value(crate::seq::stream_items(v), readably)),
        Kind::Reduced => {
            out.push_str("#<reduced ");
            stack.push(Work::Text(">"));
            stack.push(Work::Value(v.field(0), readably));
        }
        Kind::BigInt => out.push_str(&crate::numbers::big_string(v)),
        Kind::Ratio => {
            stack.push(Work::Value(v.field(1), readably));
            stack.push(Work::Text("/"));
            stack.push(Work::Value(v.field(0), readably));
        }
        Kind::Handle => {
            out.push_str("#<");
            stack.push(Work::Owned(format!(" {}>", v.word(crate::conc::handle::ID))));
            stack.push(Work::Value(v.field(crate::conc::handle::KIND), false));
        }
        Kind::Bytes => {
            // Readable as a literal: #bytes "68656c6c6f"
            out.push_str("#bytes \"");
            for b in v.as_bytes() {
                let _ = write!(out, "{b:02x}");
            }
            out.push('"');
        }
        k => {
            let _ = write!(out, "#<{k:?}>");
        }
    }
}
