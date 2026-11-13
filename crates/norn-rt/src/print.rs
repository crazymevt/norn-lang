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

fn write_seq(out: &mut String, it: impl Iterator<Item = Value>, open: &str, close: &str, readably: bool) {
    out.push_str(open);
    for (i, x) in it.enumerate() {
        if i > 0 {
            out.push(' ');
        }
        write_value(out, x, readably);
    }
    out.push_str(close);
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

pub fn write_value(out: &mut String, v: Value, readably: bool) {
    if v.is_fixnum() {
        let _ = write!(out, "{}", v.as_fixnum());
        return;
    }
    if v.is_cons() {
        write_seq(out, SeqIter::List(v), "(", ")", readably);
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
            write_seq(out, items.into_iter(), "[", "]", readably);
        }
        Kind::VecSeq | Kind::Range => write_seq(out, SeqIter::new(v).unwrap(), "(", ")", readably),
        Kind::Map => {
            out.push('{');
            let mut first = true;
            v.map_for_each(|k, val| {
                if !first {
                    out.push_str(", ");
                }
                first = false;
                write_value(out, k, readably);
                out.push(' ');
                write_value(out, val, readably);
                true
            });
            out.push('}');
        }
        Kind::Set => {
            let mut items = Vec::new();
            v.set_for_each(|x| {
                items.push(x);
                true
            });
            write_seq(out, items.into_iter(), "#{", "}", readably);
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
            write_value(out, v.atom_get(), readably);
            out.push('>');
        }
        Kind::Chan => out.push_str("#<channel>"),
        Kind::Scope => out.push_str("#<scope>"),
        k => {
            let _ = write!(out, "#<{k:?}>");
        }
    }
}
