//! The bytecode verifier (M8 part 8.11).
//!
//! The interpreter and the tier-1 compiler trust code completely: register,
//! constant, upvalue and jump operands are used without checks, a
//! template's captures are read from the creating frame, and a parked
//! fiber resumes wherever its frames say. That's sound for code this
//! process compiled. Code from anywhere else is checked here before it can
//! run:
//!
//! - **images**, which carry compiled code and parked fibers and may have
//!   been built elsewhere (or crafted): every template, closure, proto and
//!   fiber in one is checked as it loads;
//! - **`%make-template`**, the primitive the Skein compiler assembles
//!   functions with, which any Skein code can call: a template is checked
//!   when it's made, which is when its protos become runnable. (The Rust
//!   seed compiler, which compiles core at boot, is trusted.)
//!
//! The module cache needs nothing: it holds expanded source, compiled
//! again by this process's compiler.
//!
//! What a proto must satisfy, for the template that owns it:
//! - its fields are what they should be (constants an array, code a code
//!   object, debug locals an array of name/register pairs);
//! - parameters fit its registers, and there are at most 255 of them;
//! - every opcode is known, and each operand is in range: registers below
//!   the register count, constants inside the constant array (a var for
//!   GETVAR and DEFVAR, a template for CLOSURE), upvalues below the
//!   template's count, jump targets inside the code, a call's arguments
//!   inside the registers;
//! - a closure's captures name registers and upvalues of the creating
//!   frame;
//! - no instruction falls off the end of the code.
//!
//! SETUP's closure is only known at run time, so the VM checks it there.

use crate::bytecode::*;
use skein_rt::Value;
use skein_rt::conc::fiber;
use skein_rt::func::{Capture, F_FN, F_PREV, F_PROTO, F_RESUME, HDR, PREV_BOUNDARY, PREV_SEG_START};
use skein_rt::object::Kind;
use skein_rt::print::pr_str;

/// Most registers a proto may have (operands are 8 bits).
pub const MAX_REGS: usize = 255;
/// Frames walked in one fiber before giving up (a cycle guard).
const MAX_FRAMES: usize = 1 << 22;

type R = Result<(), String>;

fn name_of(v: Value) -> String {
    if v.is_nil() { "fn".to_owned() } else { pr_str(v, true) }
}

/// Is v an encoded Capture?
fn capture_ok(v: Value) -> bool {
    v.is_fixnum() && v.as_fixnum() >= 0 && v.as_fixnum() < 1 << 10
}

/// Check template t's own shape: its arities are protos, its upvalue count
/// is its captures', and its captures are encoded. (Not its code.)
fn template_shape(t: Value) -> R {
    if !t.is(Kind::FnTemplate) {
        return Err("not a function template".into());
    }
    let ar = t.template_arities();
    if !ar.is(Kind::Array) || ar.arr_len() == 0 {
        return Err(format!("{}: no arities", name_of(t.template_name())));
    }
    if let Some(p) = ar.arr_slice().iter().find(|p| !p.is(Kind::Proto)) {
        return Err(format!("{}: an arity isn't a proto ({})", name_of(t.template_name()), pr_str(*p, true)));
    }
    let caps = t.template_captures();
    if !caps.is(Kind::Array) || caps.arr_len() != t.template_nups() {
        return Err(format!("{}: its captures don't match its upvalue count", name_of(t.template_name())));
    }
    if !caps.arr_slice().iter().all(|&c| capture_ok(c)) {
        return Err(format!("{}: a capture isn't one", name_of(t.template_name())));
    }
    Ok(())
}

/// Check template t: its shape, and each arity's code against it.
/// `from_image`: also require no tier-1 entry (native code is per process).
pub fn template(t: Value, from_image: bool) -> R {
    template_shape(t)?;
    for &p in t.template_arities().arr_slice() {
        proto(p, t.template_nups(), from_image).map_err(|e| format!("in {}: {e}", name_of(t.template_name())))?;
    }
    Ok(())
}

/// Check proto p's code, for a template with `nups` upvalues.
pub fn proto(p: Value, nups: usize, from_image: bool) -> R {
    let nregs = p.proto_nregs();
    let nfixed = p.proto_nfixed();
    if nregs > MAX_REGS {
        return Err(format!("{nregs} registers (at most {MAX_REGS})"));
    }
    if nfixed + p.proto_variadic() as usize > nregs {
        return Err(format!("{nfixed} parameters{} in {nregs} registers", if p.proto_variadic() { " and a rest" } else { "" }));
    }
    if from_image && p.proto_native() != 0 {
        return Err("a tier-1 entry from another process".into());
    }
    let consts = p.proto_consts();
    if !consts.is(Kind::Array) {
        return Err("its constants aren't an array".into());
    }
    let code = p.proto_code();
    if !code.is(Kind::Code) {
        return Err("its code isn't code".into());
    }
    let locals = p.proto_locals();
    if !(locals.is_nil() || locals.is(Kind::Array) && locals.arr_len() % 2 == 0 && locals.arr_slice().chunks(2).all(|l| l[1].is_fixnum())) {
        return Err("its debug locals aren't name/register pairs".into());
    }
    let n = code.code_len();
    if n == 0 {
        return Err("no code".into());
    }
    let k = consts.arr_slice();
    // SAFETY: a code object holds code_len instructions.
    let ins = unsafe { std::slice::from_raw_parts(code.code_ptr(), n) };
    for (pc, &i) in ins.iter().enumerate() {
        let at = |what: String| format!("at {pc} ({}): {what}", OP_NAMES.get(op(i) as usize).copied().unwrap_or("?"));
        let reg = |r: usize| if r < nregs { Ok(()) } else { Err(at(format!("register {r} of {nregs}"))) };
        let konst = |kind: Option<Kind>| -> Result<Value, String> {
            let x = *k.get(bx(i)).ok_or_else(|| at(format!("constant {} of {}", bx(i), k.len())))?;
            match kind {
                Some(kd) if !x.is(kd) => Err(at(format!("constant {} isn't a {kd:?}", bx(i)))),
                _ => Ok(x),
            }
        };
        let target = |pc: usize| {
            let t = pc as isize + 1 + sbx(i);
            if (0..n as isize).contains(&t) { Ok(()) } else { Err(at(format!("a jump to {t}, outside 0..{n}"))) }
        };
        match op(i) {
            MOVE => {
                reg(a(i))?;
                reg(b(i))?;
            }
            LOADK => {
                reg(a(i))?;
                konst(None)?;
            }
            LOADI | LOADNIL | LOADTRUE | LOADFALSE | SELF | RETURN => reg(a(i))?,
            GETVAR | DEFVAR => {
                reg(a(i))?;
                konst(Some(Kind::Var))?;
            }
            GETUP => {
                reg(a(i))?;
                if b(i) >= nups {
                    return Err(at(format!("upvalue {} of {nups}", b(i))));
                }
            }
            CLOSURE => {
                reg(a(i))?;
                let t = konst(Some(Kind::FnTemplate))?;
                template_shape(t).map_err(at)?;
                // Its captures come from this frame.
                for &c in t.template_captures().arr_slice() {
                    match Capture::decode(c) {
                        Capture::Local(r) => reg(r as usize)?,
                        Capture::Up(j) if j as usize >= nups => return Err(at(format!("a capture of upvalue {j} of {nups}"))),
                        _ => {}
                    }
                }
            }
            SETUP => {
                reg(a(i))?;
                reg(c(i))?;
            }
            JMP | LOOP => target(pc)?,
            JMPF => {
                reg(a(i))?;
                target(pc)?;
            }
            GUARD => {
                reg(a(i))?;
                konst(None)?;
                // (it may skip the next instruction, which must be there)
                if pc + 2 >= n {
                    return Err(at("a guard with nothing after the instruction it skips".into()));
                }
            }
            CALL | TAILCALL => {
                if a(i) + b(i) >= nregs {
                    return Err(at(format!("a call of r{} with {} arguments in {nregs} registers", a(i), b(i))));
                }
            }
            o => return Err(at(format!("unknown opcode {o}"))),
        }
        // Only a jump or a return doesn't go on to the next instruction.
        if !matches!(op(i), JMP | LOOP | RETURN) && pc + 1 == n {
            return Err(at("the code ends without returning".into()));
        }
    }
    Ok(())
}

/// Check closure c: its template, and room for its upvalues.
pub fn closure(c: Value) -> R {
    let t = c.closure_template();
    template_shape(t)?;
    if c.closure_nups() != t.template_nups() {
        return Err(format!("{}: {} upvalues for a template with {}", name_of(t.template_name()), c.closure_nups(), t.template_nups()));
    }
    Ok(())
}

/// Check parked fiber f's frames: each fits its segment, runs a proto of
/// its function, and resumes inside its code; the chain ends.
pub fn fiber_frames(f: Value) -> R {
    let mode = f.word(fiber::MODE);
    if mode > fiber::RETURN_PAIR {
        return Err(format!("resume mode {mode}"));
    }
    let mut seg = f.field(fiber::SEG);
    if mode == fiber::START || seg.is_nil() {
        return Ok(());
    }
    let fp = f.field(fiber::FP);
    if !fp.is_fixnum() || fp.as_fixnum() < 0 {
        return Err("a bad frame pointer".into());
    }
    let mut fp = fp.as_fixnum() as usize;
    let mut top = true;
    for _ in 0..MAX_FRAMES {
        if !seg.is(Kind::Segment) {
            return Err("a frame outside a stack segment".into());
        }
        let cap = seg.seg_capacity();
        if fp.checked_add(HDR).is_none_or(|e| e > cap) {
            return Err(format!("a frame at {fp} past its segment's {cap} words"));
        }
        // SAFETY: fp + HDR <= cap: the header is inside the segment.
        let slot = |i: usize| unsafe { *seg.seg_data().add(fp + i) };
        let (func, p, resume, prev) = (slot(F_FN), slot(F_PROTO), slot(F_RESUME), slot(F_PREV));
        closure(func).map_err(|e| format!("a frame's function: {e}"))?;
        if !func.closure_template().template_arities().arr_slice().contains(&p) {
            return Err(format!("a frame of {} runs a proto that isn't one of its arities", name_of(func.closure_template().template_name())));
        }
        proto(p, func.closure_template().template_nups(), true)?;
        if fp + HDR + p.proto_nregs() > cap {
            return Err("a frame's registers run past its segment".into());
        }
        let len = p.proto_code().code_len() as i64;
        let delivers = top && matches!(mode, fiber::DELIVER | fiber::DELIVER_PAIR);
        if !resume.is_fixnum() || resume.as_fixnum() < delivers as i64 || resume.as_fixnum() >= len {
            return Err(format!("a frame of {} resumes at {} of {len}", name_of(p.proto_name()), pr_str(resume, true)));
        }
        if !prev.is_fixnum() {
            return Err("a frame's link isn't one".into());
        }
        match prev.as_fixnum() {
            PREV_BOUNDARY => return Ok(()),
            PREV_SEG_START => {
                fp = seg.seg_prev_fp();
                seg = seg.seg_prev();
            }
            q if q >= 0 && (q as usize) < fp => fp = q as usize,
            q => return Err(format!("a frame links to {q}, not below it")),
        }
        top = false;
    }
    Err("its frames don't end".into())
}

/// Check object v from an image: templates, closures, protos (no tier-1
/// entry) and parked fibers; anything else needs nothing here.
pub fn image_object(v: Value) -> R {
    if v.is(Kind::FnTemplate) {
        template(v, true)
    } else if v.is(Kind::Closure) {
        closure(v)
    } else if v.is(Kind::Proto) {
        if v.proto_native() != 0 { Err("a proto with a tier-1 entry from another process".into()) } else { Ok(()) }
    } else if v.is(Kind::Fiber) {
        fiber_frames(v)
    } else {
        Ok(())
    }
}
