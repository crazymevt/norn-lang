//! Tier-0 bytecode: register-based, 32-bit instructions (Lua 5 style).
//!
//! ```text
//!  ABC:  op:8 | a:8 | b:8 | c:8
//!  ABx:  op:8 | a:8 | bx:16          (constant index)
//!  AsBx: op:8 | a:8 | sbx:16 (+32768) (jump offset, small int)
//! ```

use norn_rt::Value;

pub const MOVE: u8 = 0; //      a b     R[a] = R[b]
pub const LOADK: u8 = 1; //     a bx    R[a] = K[bx]
pub const LOADI: u8 = 2; //     a sbx   R[a] = sbx
pub const LOADNIL: u8 = 3; //   a
pub const LOADTRUE: u8 = 4; //  a
pub const LOADFALSE: u8 = 5; // a
pub const GETVAR: u8 = 6; //    a bx    R[a] = root of var K[bx]
pub const DEFVAR: u8 = 7; //    a bx    root of var K[bx] = R[a]
pub const GETUP: u8 = 8; //     a b     R[a] = upvalue b of current closure
pub const SELF: u8 = 9; //      a       R[a] = current closure
pub const CLOSURE: u8 = 10; //  a bx    R[a] = new closure from template K[bx]
pub const SETUP: u8 = 11; //    a b c   upvalue b of closure R[a] = R[c] (letrec fix-up)
pub const JMP: u8 = 12; //      sbx     pc += sbx
pub const JMPF: u8 = 13; //     a sbx   if R[a] is falsey: pc += sbx
pub const LOOP: u8 = 14; //     sbx     safepoint, then pc += sbx (backward)
pub const CALL: u8 = 15; //     a b     R[a] = R[a](R[a+1] … R[a+b])
pub const TAILCALL: u8 = 16; // a b     return R[a](R[a+1] … R[a+b])
pub const RETURN: u8 = 17; //   a       return R[a]
pub const GUARD: u8 = 18; //    a bx    if R[a] is K[bx] (identical): skip the next instruction

pub const OP_NAMES: &[&str] = &[
    "MOVE", "LOADK", "LOADI", "LOADNIL", "LOADTRUE", "LOADFALSE", "GETVAR", "DEFVAR", "GETUP", "SELF", "CLOSURE",
    "SETUP", "JMP", "JMPF", "LOOP", "CALL", "TAILCALL", "RETURN", "GUARD",
];

pub const SBX_BIAS: i32 = 32768;

#[inline]
pub fn abc(op: u8, a: u8, b: u8, c: u8) -> u32 {
    op as u32 | (a as u32) << 8 | (b as u32) << 16 | (c as u32) << 24
}

#[inline]
pub fn abx(op: u8, a: u8, bx: u16) -> u32 {
    op as u32 | (a as u32) << 8 | (bx as u32) << 16
}

#[inline]
pub fn asbx(op: u8, a: u8, sbx: i32) -> u32 {
    abx(op, a, (sbx + SBX_BIAS) as u16)
}

#[inline]
pub fn op(i: u32) -> u8 {
    (i & 0xff) as u8
}
#[inline]
pub fn a(i: u32) -> usize {
    ((i >> 8) & 0xff) as usize
}
#[inline]
pub fn b(i: u32) -> usize {
    ((i >> 16) & 0xff) as usize
}
#[inline]
pub fn c(i: u32) -> usize {
    (i >> 24) as usize
}
#[inline]
pub fn bx(i: u32) -> usize {
    (i >> 16) as usize
}
#[inline]
pub fn sbx(i: u32) -> isize {
    (i >> 16) as isize - SBX_BIAS as isize
}

/// Human-readable listing of one proto's code.
pub fn disassemble_proto(proto: Value) -> String {
    use std::fmt::Write;
    let code = proto.proto_code();
    let consts = proto.proto_consts();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{}  ({} fixed{}, {} registers)",
        norn_rt::print::pr_str(proto.proto_name(), true),
        proto.proto_nfixed(),
        if proto.proto_variadic() { " + rest" } else { "" },
        proto.proto_nregs()
    );
    for pc in 0..code.code_len() {
        // SAFETY: pc < code_len.
        let i = unsafe { *code.code_ptr().add(pc) };
        let o = op(i);
        let name = OP_NAMES.get(o as usize).copied().unwrap_or("???");
        let operands = match o {
            LOADK | GETVAR | DEFVAR | CLOSURE | GUARD => {
                let k = consts.arr_get(bx(i));
                format!("r{} K{}  ; {}", a(i), bx(i), norn_rt::print::pr_str(k, true))
            }
            LOADI => format!("r{} {}", a(i), sbx(i)),
            JMP | LOOP => format!("-> {}", pc as isize + 1 + sbx(i)),
            JMPF => format!("r{} -> {}", a(i), pc as isize + 1 + sbx(i)),
            CALL | TAILCALL => format!("r{} ({} args)", a(i), b(i)),
            SETUP => format!("r{} up{} r{}", a(i), b(i), c(i)),
            GETUP => format!("r{} up{}", a(i), b(i)),
            MOVE => format!("r{} r{}", a(i), b(i)),
            _ => format!("r{}", a(i)),
        };
        let _ = writeln!(out, "  {pc:4}  {name:<9} {operands}");
    }
    out
}
