//! Tier 1: native code with Cranelift (runtime.md §8.2).
//!
//! Each proto's bytecode becomes one native function
//!
//! ```text
//! tail fn(regs: *mut Value, consts: *const Value, ctx: *const JitCtx, pc: u64) -> u64
//! ```
//!
//! that starts at instruction `pc` (an entry for every instruction, through a
//! jump table) and runs until it reaches an instruction it doesn't handle.
//! It returns that instruction's index, and the interpreter executes it.
//! The interpreter enters it through a trampoline (`enter`), since it uses
//! Cranelift's `tail` calling convention.
//!
//! Registers stay in the heap frame, so at every exit the frame is exactly
//! what the interpreter expects:
//! - mixed tier-0/tier-1 stacks, the GC, the debugger and backtraces need
//!   nothing new;
//! - "deoptimising" is just interpreting from the current pc.
//!
//! Handled natively:
//! - moves, constants, var reads (fast path), upvalues, branches;
//! - loop back-edges, with the safepoint check;
//! - calls to `+ - < > <= >= == = inc dec zero?` when the callee *is* that
//!   primitive (checked at run time, so redefinitions stay live) and the
//!   arguments are fixnums;
//! - making closures (`CLOSURE`, through a helper that allocates);
//! - calls, tail calls and returns (M10 part 2). A call is a helper
//!   (crate::vm::jit_call) that works out what the callee is, as
//!   Vm::invoke does (through vars, generic functions' dispatch, classes),
//!   and either pushes a closure's frame (in the heap, as the interpreter
//!   does) and then `return_call`s into the callee's code, or calls a
//!   primitive or data function and goes on with its value. A return is a
//!   helper that pops the frame and a `return_call` back into the caller's
//!   code at its resume point. The native stack doesn't grow, and tail
//!   calls stay proper. The callee's entry safepoint is checked natively;
//!   when it's due, native code stops with POLL_FLAG and the interpreter
//!   polls. A primitive that fails or parks stops native code with
//!   PENDING_FLAG at its instruction, and the interpreter handles it as if
//!   it had made the call (Vm::jit_pending), without calling it again.
//!
//! Everything else (`apply`, `eval`, `throw`, `def`, a stack overflow,
//! failed guards) goes back to the interpreter. When native code stops,
//! `JitCtx::cur_regs` says which frame it stopped in.
//!
//! A proto is compiled once it has been entered THRESHOLD times (calls and
//! loop turns, counted in its native word: entry_for).

use crate::bytecode::*;
use cranelift_codegen::control::ControlPlane;
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{AbiParam, Block, BlockArg, Function, InstBuilder, JumpTableData, MemFlagsData, Signature, UserFuncName, types};
use cranelift_codegen::isa::OwnedTargetIsa;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use norn_rt::Value;
use norn_rt::func::{HDR, VAR_DYNBOUND};
use std::sync::{Mutex, OnceLock};

/// What native code needs from its carrier. `repr(C)`: offsets are baked
/// into the generated code (the CTX_ constants).
#[repr(C)]
pub struct JitCtx {
    pub budget: *mut i64,
    pub attention: *const u64,
    pub seen: *const u64,
    pub prims: [u64; NPRIMS],
    /// Out: the registers of the frame native code stopped in.
    pub cur_regs: u64,
    /// Out, from a helper: the next frame's native entry (1: none), its
    /// constants, and (after a return) the pc to resume at.
    pub next_native: u64,
    pub next_consts: u64,
    pub next_pc: u64,
    /// The helpers' addresses (no relocations in the code).
    pub call_fn: u64,
    pub tail_fn: u64,
    pub ret_fn: u64,
    /// Out, from a helper: a call's value, or the frame it pushed.
    pub value: u64,
    pub next_regs: u64,
    /// The carrier's Vm (for calling primitives).
    pub vm: u64,
    /// CLOSURE's helper (crate::vm::jit_closure).
    pub closure_fn: u64,
}

impl JitCtx {
    pub fn new() -> JitCtx {
        JitCtx {
            budget: std::ptr::null_mut(),
            attention: norn_rt::safepoint::attention_ptr(),
            seen: std::ptr::null(),
            prims: [0; NPRIMS],
            cur_regs: 0,
            next_native: 0,
            next_consts: 0,
            next_pc: 0,
            call_fn: crate::vm::jit_call as *const () as u64,
            tail_fn: crate::vm::jit_tail as *const () as u64,
            ret_fn: crate::vm::jit_return as *const () as u64,
            value: 0,
            next_regs: 0,
            vm: 0,
            closure_fn: crate::vm::jit_closure as *const () as u64,
        }
    }
}

impl Default for JitCtx {
    fn default() -> Self {
        Self::new()
    }
}

const CTX_CUR_REGS: i32 = (3 + NPRIMS as i32) * W;
const CTX_NEXT_NATIVE: i32 = (4 + NPRIMS as i32) * W;
const CTX_NEXT_CONSTS: i32 = (5 + NPRIMS as i32) * W;
const CTX_NEXT_PC: i32 = (6 + NPRIMS as i32) * W;
const CTX_CALL: i32 = (7 + NPRIMS as i32) * W;
const CTX_TAIL: i32 = (8 + NPRIMS as i32) * W;
const CTX_RET: i32 = (9 + NPRIMS as i32) * W;
const CTX_VALUE: i32 = (10 + NPRIMS as i32) * W;
const CTX_NEXT_REGS: i32 = (11 + NPRIMS as i32) * W;
const CTX_CLOSURE: i32 = (13 + NPRIMS as i32) * W;

// (the offsets above are the fields' places in JitCtx)
const _: () = {
    assert!(std::mem::offset_of!(JitCtx, cur_regs) as i32 == CTX_CUR_REGS);
    assert!(std::mem::offset_of!(JitCtx, next_native) as i32 == CTX_NEXT_NATIVE);
    assert!(std::mem::offset_of!(JitCtx, next_consts) as i32 == CTX_NEXT_CONSTS);
    assert!(std::mem::offset_of!(JitCtx, next_pc) as i32 == CTX_NEXT_PC);
    assert!(std::mem::offset_of!(JitCtx, call_fn) as i32 == CTX_CALL);
    assert!(std::mem::offset_of!(JitCtx, tail_fn) as i32 == CTX_TAIL);
    assert!(std::mem::offset_of!(JitCtx, ret_fn) as i32 == CTX_RET);
    assert!(std::mem::offset_of!(JitCtx, value) as i32 == CTX_VALUE);
    assert!(std::mem::offset_of!(JitCtx, next_regs) as i32 == CTX_NEXT_REGS);
    assert!(std::mem::offset_of!(JitCtx, closure_fn) as i32 == CTX_CLOSURE);
};

/// What the call helpers say happened (crate::vm::jit_call, jit_tail).
pub const NOT_MINE: u64 = 0;
pub const PENDING: u64 = 1;
pub const VALUE: u64 = 2;
pub const PUSHED: u64 = 3;

/// Set in what native code returns when a primitive it called failed or
/// asked to park: the Vm holds which (Vm::jit_pending), for the
/// interpreter to handle at the instruction (the pc) as if it had made the
/// call, which isn't made again.
pub const PENDING_FLAG: u64 = 1 << 61;

/// Set in what native code returns when a callee's entry safepoint is due:
/// the interpreter polls, then carries on at the pc in the other bits.
pub const POLL_FLAG: u64 = 1 << 62;

pub const NPRIMS: usize = 11;
/// The primitives with native fast paths, in `JitCtx::prims` order.
pub const FAST_PRIMS: [&str; NPRIMS] = ["+", "-", "<", ">", "<=", ">=", "==", "=", "inc", "dec", "zero?"];

/// The trampoline: (native entry, regs, consts, ctx, pc) → pc, in the
/// platform's convention, calling the entry (Cranelift's `tail`).
pub type Trampoline = unsafe extern "C" fn(usize, *mut Value, *const Value, *mut JitCtx, u64) -> u64;

pub static COMPILED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
pub static REJECTED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
pub static CODE_BYTES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Is tier 1 on? (`NORN_TIER=1`; tier 0 is the default for now.)
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("NORN_TIER").as_deref() == Ok("1"))
}

pub(crate) fn isa() -> Option<&'static OwnedTargetIsa> {
    static ISA: OnceLock<Option<OwnedTargetIsa>> = OnceLock::new();
    ISA.get_or_init(|| {
        let mut flags = settings::builder();
        flags.set("opt_level", "speed").ok()?;
        flags.set("is_pic", "true").ok()?;
        cranelift_native::builder().ok()?.finish(settings::Flags::new(flags)).ok()
    })
    .as_ref()
}

// ---- executable memory -------------------------------------------------------------

struct CodeArena {
    cur: usize,
    end: usize,
}

static ARENA: Mutex<CodeArena> = Mutex::new(CodeArena { cur: 0, end: 0 });
const ARENA_CHUNK: usize = 16 << 20;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn sys_icache_invalidate(start: *mut libc::c_void, len: libc::size_t);
}

// (arm64 keeps instruction and data caches apart: code just written must
// be flushed from one to the other before it runs, or stale bytes run)
#[cfg(all(target_arch = "aarch64", not(target_os = "macos")))]
unsafe extern "C" {
    fn __clear_cache(start: *mut libc::c_char, end: *mut libc::c_char);
}

/// Copy machine code into executable memory (MAP_JIT on macOS: runtime.md
/// §7.4). Never freed: protos are rarely collected, and code is small.
pub(crate) fn install_code(bytes: &[u8]) -> Option<usize> {
    let len = bytes.len().div_ceil(16) * 16;
    let mut a = ARENA.lock().unwrap();
    if a.end - a.cur < len {
        let size = ARENA_CHUNK.max(len);
        // SAFETY: a fresh anonymous mapping.
        let p = unsafe {
            #[cfg(target_os = "macos")]
            let flags = libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_JIT;
            #[cfg(not(target_os = "macos"))]
            let flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS;
            libc::mmap(std::ptr::null_mut(), size, libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC, flags, -1, 0)
        };
        if p == libc::MAP_FAILED {
            return None;
        }
        a.cur = p as usize;
        a.end = p as usize + size;
    }
    let at = a.cur;
    a.cur += len;
    // SAFETY: [at, at+len) is ours; on macOS this thread toggles W^X around the copy.
    unsafe {
        #[cfg(target_os = "macos")]
        libc::pthread_jit_write_protect_np(0);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), at as *mut u8, bytes.len());
        #[cfg(target_os = "macos")]
        {
            libc::pthread_jit_write_protect_np(1);
            sys_icache_invalidate(at as *mut libc::c_void, len);
        }
        #[cfg(all(target_arch = "aarch64", not(target_os = "macos")))]
        __clear_cache(at as *mut libc::c_char, (at + len) as *mut libc::c_char);
    }
    Some(at)
}

// ---- compilation --------------------------------------------------------------------------

const W: i32 = 8;

/// Compile a proto's bytecode to native code. `None` if it can't be (the
/// interpreter keeps running it).
pub fn compile(proto: Value) -> Option<usize> {
    use std::sync::atomic::Ordering::Relaxed;
    let r = compile_inner(proto);
    match r {
        Some(_) => COMPILED.fetch_add(1, Relaxed),
        None => REJECTED.fetch_add(1, Relaxed),
    };
    r
}

fn compile_inner(proto: Value) -> Option<usize> {
    let isa = isa()?;
    let code = proto.proto_code();
    let n = code.code_len();
    // SAFETY: n instructions follow the length word.
    let ins: Vec<u32> = (0..n).map(|i| unsafe { *code.code_ptr().add(i) }).collect();

    let sig = native_sig();
    let mut func = Function::with_name_signature(UserFuncName::user(0, 0), sig.clone());
    let mut fbc = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut func, &mut fbc);
    let mf = MemFlagsData::trusted();
    // (native functions call each other with return_call; the helpers are
    // ordinary functions)
    let native_ref = b.import_signature(sig);
    let helper = |n: usize| {
        let mut s = Signature::new(isa.default_call_conv());
        for _ in 0..n {
            s.params.push(AbiParam::new(types::I64));
        }
        s.returns.push(AbiParam::new(types::I64));
        s
    };
    let call_ref = b.import_signature(helper(5));
    let tail_ref = b.import_signature(helper(5));
    let ret_ref = b.import_signature(helper(3));
    let closure_ref = b.import_signature(helper(4));

    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    let blocks: Vec<Block> = (0..n).map(|_| b.create_block()).collect();
    let exit = b.create_block(); // param: pc to hand back
    b.append_block_param(exit, types::I64);

    b.switch_to_block(entry);
    let p = b.block_params(entry).to_vec();
    let (regs, consts, ctx, pc) = (p[0], p[1], p[2], p[3]);
    let pc32 = b.ins().ireduce(types::I32, pc);
    let exit_pc = [BlockArg::Value(pc)];
    let default = b.func.dfg.block_call(exit, &exit_pc);
    let table: Vec<_> = blocks.iter().map(|&blk| b.func.dfg.block_call(blk, &[])).collect();
    let jt = b.create_jump_table(JumpTableData::new(default, &table));
    b.ins().br_table(pc32, jt);

    let nil = Value::NIL.0 as i64;
    let tru = Value::TRUE.0 as i64;
    let fal = Value::FALSE.0 as i64;
    let unbound = Value::UNBOUND.0 as i64;
    let next = |i: usize| if i + 1 < n { Some(blocks[i + 1]) } else { None };

    // Go into the frame whose registers are new_regs, at pc (a value): its
    // native code if it has some (a tail call), else back to the
    // interpreter there. With `poll`, its entry safepoint first, as
    // Vm::push_frame's: when due, stop there with POLL_FLAG.
    let enter = |b: &mut FunctionBuilder, new_regs: cranelift_codegen::ir::Value, pcv: cranelift_codegen::ir::Value, poll: bool| {
        let stop = |b: &mut FunctionBuilder, r: cranelift_codegen::ir::Value| {
            b.ins().store(mf, new_regs, ctx, CTX_CUR_REGS);
            b.ins().return_(&[r]);
        };
        if poll {
            let bp = b.ins().load(types::I64, mf, ctx, 0);
            let budget = b.ins().load(types::I64, mf, bp, 0);
            let budget = b.ins().iadd_imm_s(budget, -1);
            b.ins().store(mf, budget, bp, 0);
            let ap = b.ins().load(types::I64, mf, ctx, W);
            let att = b.ins().atomic_load(types::I64, mf, ap);
            let sp = b.ins().load(types::I64, mf, ctx, 2 * W);
            let seen = b.ins().load(types::I64, mf, sp, 0);
            let fresh = b.ins().icmp(IntCC::Equal, att, seen);
            let left = b.ins().icmp_imm_s(IntCC::SignedGreaterThan, budget, 0);
            let go = b.ins().band(fresh, left);
            let (ok, due) = (b.create_block(), b.create_block());
            b.ins().brif(go, ok, &[], due, &[]);
            b.switch_to_block(due);
            let flagged = b.ins().bor_imm_s(pcv, POLL_FLAG as i64);
            stop(b, flagged);
            b.switch_to_block(ok);
        }
        let nat = b.ins().load(types::I64, mf, ctx, CTX_NEXT_NATIVE);
        let has = b.ins().icmp_imm_s(IntCC::UnsignedGreaterThan, nat, 1);
        let (native, interp) = (b.create_block(), b.create_block());
        b.ins().brif(has, native, &[], interp, &[]);
        b.switch_to_block(native);
        let consts2 = b.ins().load(types::I64, mf, ctx, CTX_NEXT_CONSTS);
        b.ins().return_call_indirect(native_ref, nat, &[new_regs, consts2, ctx, pcv]);
        b.switch_to_block(interp);
        stop(b, pcv);
    };

    for (i, &w) in ins.iter().enumerate() {
        b.switch_to_block(blocks[i]);
        let (o, ra, rb) = (op(w), a(w) as i32, b_(w) as i32);
        let bail = |b: &mut FunctionBuilder| {
            let v = b.ins().iconst(types::I64, i as i64);
            b.ins().jump(exit, &[BlockArg::Value(v)]);
        };
        // Stop at this instruction with PENDING_FLAG: a primitive called
        // natively failed or asked to park (the Vm says which).
        let pending = |b: &mut FunctionBuilder| {
            let v = b.ins().iconst(types::I64, (i as u64 | PENDING_FLAG) as i64);
            b.ins().jump(exit, &[BlockArg::Value(v)]);
        };
        // Return v from this frame: the helper pops it and delivers v to the
        // caller, which goes on natively (or the interpreter returns).
        // `tail`: v is the value of the TAILCALL here, whose call has been
        // made, so when the interpreter must return it, it does so from
        // the `RETURN ra` that follows (re-running the TAILCALL would call
        // the primitive twice).
        let ret_value = |b: &mut FunctionBuilder, v: cranelift_codegen::ir::Value, tail: Option<i32>| {
            let f = b.ins().load(types::I64, mf, ctx, CTX_RET);
            let call = b.ins().call_indirect(ret_ref, f, &[ctx, regs, v]);
            let caller = b.inst_results(call)[0];
            let failed = b.ins().icmp_imm_s(IntCC::Equal, caller, 0);
            let (no, yes) = (b.create_block(), b.create_block());
            b.ins().brif(failed, no, &[], yes, &[]);
            b.switch_to_block(no);
            match tail {
                Some(ra) => {
                    b.ins().store(mf, v, regs, ra * W);
                    let next_pc = b.ins().iconst(types::I64, (i + 1) as i64);
                    b.ins().jump(exit, &[BlockArg::Value(next_pc)]);
                }
                None => bail(b),
            }
            b.switch_to_block(yes);
            let pcv = b.ins().load(types::I64, mf, ctx, CTX_NEXT_PC);
            enter(b, caller, pcv, false);
        };
        // A call of what's in register ra with rb arguments: the helper
        // pushes a closure's frame (this one resumes after the call when
        // it returns) or calls a primitive; or the interpreter does it.
        let call_any = |b: &mut FunctionBuilder, ra: i32, rb: i32, i: usize| {
            let f = b.ins().load(types::I64, mf, ctx, CTX_CALL);
            let ra_v = b.ins().iconst(types::I64, ra as i64);
            let rb_v = b.ins().iconst(types::I64, rb as i64);
            let resume = b.ins().iconst(types::I64, (i + 1) as i64);
            let call = b.ins().call_indirect(call_ref, f, &[ctx, regs, ra_v, rb_v, resume]);
            let status = b.inst_results(call)[0];
            let (pushed, value, pend, other) = (b.create_block(), b.create_block(), b.create_block(), b.create_block());
            let is_pushed = b.ins().icmp_imm_s(IntCC::Equal, status, PUSHED as i64);
            let not_pushed = b.create_block();
            b.ins().brif(is_pushed, pushed, &[], not_pushed, &[]);
            b.switch_to_block(not_pushed);
            let is_value = b.ins().icmp_imm_s(IntCC::Equal, status, VALUE as i64);
            let not_value = b.create_block();
            b.ins().brif(is_value, value, &[], not_value, &[]);
            b.switch_to_block(not_value);
            let is_pending = b.ins().icmp_imm_s(IntCC::Equal, status, PENDING as i64);
            b.ins().brif(is_pending, pend, &[], other, &[]);
            b.switch_to_block(other);
            bail(b);
            b.switch_to_block(pend);
            pending(b);
            b.switch_to_block(value);
            let v = b.ins().load(types::I64, mf, ctx, CTX_VALUE);
            b.ins().store(mf, v, regs, ra * W);
            match next(i) {
                Some(blk) => {
                    b.ins().jump(blk, &[]);
                }
                None => {
                    let v = b.ins().iconst(types::I64, (i + 1) as i64);
                    b.ins().jump(exit, &[BlockArg::Value(v)]);
                }
            }
            b.switch_to_block(pushed);
            let new_regs = b.ins().load(types::I64, mf, ctx, CTX_NEXT_REGS);
            let zero = b.ins().iconst(types::I64, 0);
            enter(b, new_regs, zero, true);
        };
        let goto = |b: &mut FunctionBuilder, to: Option<Block>| match to {
            Some(blk) => {
                b.ins().jump(blk, &[]);
            }
            None => {
                let v = b.ins().iconst(types::I64, (i + 1) as i64);
                b.ins().jump(exit, &[BlockArg::Value(v)]);
            }
        };
        let target = |off: isize| -> Option<Block> {
            let t = i as isize + 1 + off;
            if t >= 0 && (t as usize) < n { Some(blocks[t as usize]) } else { None }
        };
        match o {
            MOVE => {
                let v = b.ins().load(types::I64, mf, regs, rb * W);
                b.ins().store(mf, v, regs, ra * W);
                goto(&mut b, next(i));
            }
            LOADK => {
                let v = b.ins().load(types::I64, mf, consts, bx(w) as i32 * W);
                b.ins().store(mf, v, regs, ra * W);
                goto(&mut b, next(i));
            }
            LOADI | LOADNIL | LOADTRUE | LOADFALSE => {
                let bits = match o {
                    LOADI => Value::fix(sbx(w) as i64).0 as i64,
                    LOADNIL => nil,
                    LOADTRUE => tru,
                    _ => fal,
                };
                let v = b.ins().iconst(types::I64, bits);
                b.ins().store(mf, v, regs, ra * W);
                goto(&mut b, next(i));
            }
            GETVAR => {
                // var = consts[bx] (tagged object pointer): root at payload
                // word 2, flags at word 3; tag 1 is folded into the offsets.
                let var = b.ins().load(types::I64, mf, consts, bx(w) as i32 * W);
                let flags = b.ins().load(types::I64, mf, var, (2 + 3) * W - 1);
                let dyn_bits = b.ins().band_imm_s(flags, VAR_DYNBOUND as i64);
                let dyn_bit = b.ins().icmp_imm_s(IntCC::NotEqual, dyn_bits, 0);
                let root_addr = b.ins().iadd_imm_s(var, (2 + 2) * W as i64 - 1);
                let root = b.ins().atomic_load(types::I64, mf, root_addr);
                let is_unbound = b.ins().icmp_imm_s(IntCC::Equal, root, unbound);
                let slow = b.ins().bor(dyn_bit, is_unbound);
                let ok = b.create_block();
                let s = b.create_block();
                b.ins().brif(slow, s, &[], ok, &[]);
                b.switch_to_block(s);
                bail(&mut b);
                b.switch_to_block(ok);
                b.ins().store(mf, root, regs, ra * W);
                goto(&mut b, next(i));
            }
            GETUP | SELF => {
                // The frame's closure is at regs - HDR (+F_FN = 0).
                let clo = b.ins().load(types::I64, mf, regs, -(HDR as i32) * W);
                let v = if o == SELF { clo } else { b.ins().load(types::I64, mf, clo, (2 + 2 + rb) * W - 1) };
                b.ins().store(mf, v, regs, ra * W);
                goto(&mut b, next(i));
            }
            // (the interpreter runs it: its closure is only known at run
            // time, and it checks the upvalue is there; letrec only)
            SETUP => bail(&mut b),
            JMP => goto(&mut b, target(sbx(w))),
            JMPF => {
                let v = b.ins().load(types::I64, mf, regs, ra * W);
                let is_nil = b.ins().icmp_imm_s(IntCC::Equal, v, nil);
                let is_false = b.ins().icmp_imm_s(IntCC::Equal, v, fal);
                let falsey = b.ins().bor(is_nil, is_false);
                let (t, f) = (b.create_block(), b.create_block());
                b.ins().brif(falsey, t, &[], f, &[]);
                b.switch_to_block(t);
                goto(&mut b, target(sbx(w)));
                b.switch_to_block(f);
                goto(&mut b, next(i));
            }
            LOOP => {
                // Safepoint: budget and attention, as in Vm::poll.
                let bp = b.ins().load(types::I64, mf, ctx, 0);
                let budget = b.ins().load(types::I64, mf, bp, 0);
                let budget = b.ins().iadd_imm_s(budget, -1);
                b.ins().store(mf, budget, bp, 0);
                let ap = b.ins().load(types::I64, mf, ctx, W);
                let att = b.ins().atomic_load(types::I64, mf, ap);
                let sp = b.ins().load(types::I64, mf, ctx, 2 * W);
                let seen = b.ins().load(types::I64, mf, sp, 0);
                let fresh = b.ins().icmp(IntCC::Equal, att, seen);
                let left = b.ins().icmp_imm_s(IntCC::SignedGreaterThan, budget, 0);
                let go = b.ins().band(fresh, left);
                let (t, s) = (b.create_block(), b.create_block());
                b.ins().brif(go, t, &[], s, &[]);
                b.switch_to_block(s);
                bail(&mut b);
                b.switch_to_block(t);
                goto(&mut b, target(sbx(w)));
            }
            CALL if rb == 1 || rb == 2 => {
                let slow = b.create_block();
                let store_next = |b: &mut FunctionBuilder, v| {
                    b.ins().store(mf, v, regs, ra * W);
                    goto(b, next(i));
                };
                fast_prims(&mut b, mf, ctx, regs, ra, rb, slow, &store_next);
                b.switch_to_block(slow);
                call_any(&mut b, ra, rb, i);
            }
            CALL => call_any(&mut b, ra, rb, i),
            TAILCALL => {
                // (a fixnum primitive's value, returned; a closure's frame in
                // place of this one; or another primitive's value, returned)
                if rb == 1 || rb == 2 {
                    let slow = b.create_block();
                    fast_prims(&mut b, mf, ctx, regs, ra, rb, slow, &|b: &mut FunctionBuilder, v| ret_value(b, v, Some(ra)));
                    b.switch_to_block(slow);
                }
                let f = b.ins().load(types::I64, mf, ctx, CTX_TAIL);
                let ra_v = b.ins().iconst(types::I64, ra as i64);
                let rb_v = b.ins().iconst(types::I64, rb as i64);
                let resume = b.ins().iconst(types::I64, (i + 1) as i64);
                let call = b.ins().call_indirect(tail_ref, f, &[ctx, regs, ra_v, rb_v, resume]);
                let status = b.inst_results(call)[0];
                let (pushed, value, pend, other) = (b.create_block(), b.create_block(), b.create_block(), b.create_block());
                let is_pushed = b.ins().icmp_imm_s(IntCC::Equal, status, PUSHED as i64);
                let not_pushed = b.create_block();
                b.ins().brif(is_pushed, pushed, &[], not_pushed, &[]);
                b.switch_to_block(not_pushed);
                let is_value = b.ins().icmp_imm_s(IntCC::Equal, status, VALUE as i64);
                let not_value = b.create_block();
                b.ins().brif(is_value, value, &[], not_value, &[]);
                b.switch_to_block(not_value);
                let is_pending = b.ins().icmp_imm_s(IntCC::Equal, status, PENDING as i64);
                b.ins().brif(is_pending, pend, &[], other, &[]);
                b.switch_to_block(other);
                bail(&mut b);
                b.switch_to_block(pend);
                pending(&mut b);
                b.switch_to_block(value);
                let v = b.ins().load(types::I64, mf, ctx, CTX_VALUE);
                ret_value(&mut b, v, Some(ra));
                b.switch_to_block(pushed);
                let new_regs = b.ins().load(types::I64, mf, ctx, CTX_NEXT_REGS);
                let zero = b.ins().iconst(types::I64, 0);
                enter(&mut b, new_regs, zero, true);
            }
            RETURN => {
                let v = b.ins().load(types::I64, mf, regs, ra * W);
                ret_value(&mut b, v, None);
            }
            CLOSURE => {
                // (the helper makes it, as the interpreter does: it
                // allocates, which never collects)
                let f = b.ins().load(types::I64, mf, ctx, CTX_CLOSURE);
                let ra_v = b.ins().iconst(types::I64, ra as i64);
                let bx_v = b.ins().iconst(types::I64, bx(w) as i64);
                b.ins().call_indirect(closure_ref, f, &[ctx, regs, ra_v, bx_v]);
                goto(&mut b, next(i));
            }
            _ => bail(&mut b),
        }
    }

    b.switch_to_block(exit);
    let rv = b.block_params(exit)[0];
    b.ins().store(mf, regs, ctx, CTX_CUR_REGS);
    b.ins().return_(&[rv]);
    b.seal_all_blocks();
    b.finalize(isa.frontend_config());

    let mut cctx = cranelift_codegen::Context::for_function(func);
    let compiled = cctx.compile(&**isa, &mut ControlPlane::default()).ok()?;
    if !compiled.buffer.relocs().is_empty() {
        return None;
    }
    CODE_BYTES.fetch_add(compiled.code_buffer().len(), std::sync::atomic::Ordering::Relaxed);
    install_code(compiled.code_buffer())
}

#[inline]
fn b_(w: u32) -> usize {
    b(w)
}

/// The fixnum fast paths of a call (CALL or TAILCALL ra with rb = 1 or 2
/// arguments) of one of FAST_PRIMS: when the callee is that primitive and
/// the arguments are fixnums, the value goes to on_value; anything else
/// (another callee, other arguments, an overflow) jumps to slow.
#[allow(clippy::too_many_arguments)]
fn fast_prims(
    b: &mut FunctionBuilder,
    mf: MemFlagsData,
    ctx: cranelift_codegen::ir::Value,
    regs: cranelift_codegen::ir::Value,
    ra: i32,
    rb: i32,
    slow: Block,
    on_value: &dyn Fn(&mut FunctionBuilder, cranelift_codegen::ir::Value),
) {
    let (tru, fal) = (Value::TRUE.0 as i64, Value::FALSE.0 as i64);
    let f = b.ins().load(types::I64, mf, regs, ra * W);
    let x = b.ins().load(types::I64, mf, regs, (ra + 1) * W);
    let y = if rb == 2 { b.ins().load(types::I64, mf, regs, (ra + 2) * W) } else { x };
    // Both fixnums (low bit 0)?
    let tags = b.ins().bor(x, y);
    let tag = b.ins().band_imm_s(tags, 1);
    let checked = b.create_block();
    b.ins().brif(tag, slow, &[], checked, &[]);
    b.switch_to_block(checked);
    let cands: Vec<usize> = if rb == 2 { (0..8).collect() } else { vec![8, 9, 10] };
    let boolv = |b: &mut FunctionBuilder, c| {
        let t = b.ins().iconst(types::I64, tru);
        let ff = b.ins().iconst(types::I64, fal);
        b.ins().select(c, t, ff)
    };
    for k in cands {
        let pk = b.ins().load(types::I64, mf, ctx, (3 + k as i32) * W);
        let hit = b.ins().icmp(IntCC::Equal, f, pk);
        let (h, miss) = (b.create_block(), b.create_block());
        b.ins().brif(hit, h, &[], miss, &[]);
        b.switch_to_block(h);
        match FAST_PRIMS[k] {
            "+" | "-" | "inc" | "dec" => {
                let (lhs, rhs) = match FAST_PRIMS[k] {
                    "inc" | "dec" => (x, b.ins().iconst(types::I64, 2)),
                    _ => (x, y),
                };
                let add = matches!(FAST_PRIMS[k], "+" | "inc");
                let r = if add { b.ins().iadd(lhs, rhs) } else { b.ins().isub(lhs, rhs) };
                // Signed overflow of the raw (2n) values = fixnum overflow.
                let ovf = if add {
                    let a1 = b.ins().bxor(lhs, r);
                    let a2 = b.ins().bxor(rhs, r);
                    b.ins().band(a1, a2)
                } else {
                    let a1 = b.ins().bxor(lhs, rhs);
                    let a2 = b.ins().bxor(lhs, r);
                    b.ins().band(a1, a2)
                };
                let neg = b.ins().icmp_imm_s(IntCC::SignedLessThan, ovf, 0);
                let ok = b.create_block();
                b.ins().brif(neg, slow, &[], ok, &[]);
                b.switch_to_block(ok);
                on_value(b, r);
            }
            "zero?" => {
                let c0 = b.ins().icmp_imm_s(IntCC::Equal, x, 0);
                let v = boolv(b, c0);
                on_value(b, v);
            }
            cmp => {
                let cc = match cmp {
                    "<" => IntCC::SignedLessThan,
                    ">" => IntCC::SignedGreaterThan,
                    "<=" => IntCC::SignedLessThanOrEqual,
                    ">=" => IntCC::SignedGreaterThanOrEqual,
                    _ => IntCC::Equal, // == and = on fixnums
                };
                let c0 = b.ins().icmp(cc, x, y);
                let v = boolv(b, c0);
                on_value(b, v);
            }
        }
        b.switch_to_block(miss);
    }
    b.ins().jump(slow, &[]);
}

/// Native functions' signature: (regs, consts, ctx, pc) → pc, in
/// Cranelift's `tail` convention (calls between them are tail calls).
fn native_sig() -> Signature {
    let mut sig = Signature::new(CallConv::Tail);
    for _ in 0..4 {
        sig.params.push(AbiParam::new(types::I64));
    }
    sig.returns.push(AbiParam::new(types::I64));
    sig
}

/// Run native code from the interpreter: entry's function on (regs,
/// consts, ctx) from pc, through the trampoline (made once).
///
/// # Safety
/// `entry` is a native function compiled by this module for the frame
/// whose registers are `regs`, and `ctx` is the carrier's.
pub unsafe fn enter(entry: usize, regs: *mut Value, consts: *const Value, ctx: *mut JitCtx, pc: u64) -> u64 {
    static T: OnceLock<Option<usize>> = OnceLock::new();
    let t = T.get_or_init(make_trampoline).expect("tier 1's trampoline");
    // SAFETY: the trampoline has this signature (made below).
    unsafe {
        let f: Trampoline = std::mem::transmute(t);
        f(entry, regs, consts, ctx, pc)
    }
}

fn make_trampoline() -> Option<usize> {
    let isa = isa()?;
    let mut sig = Signature::new(isa.default_call_conv());
    for _ in 0..5 {
        sig.params.push(AbiParam::new(types::I64));
    }
    sig.returns.push(AbiParam::new(types::I64));
    let mut func = Function::with_name_signature(UserFuncName::user(0, 1), sig);
    let mut fbc = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut func, &mut fbc);
    let native_ref = b.import_signature(native_sig());
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    let p = b.block_params(entry).to_vec();
    let call = b.ins().call_indirect(native_ref, p[0], &[p[1], p[2], p[3], p[4]]);
    let r = b.inst_results(call)[0];
    b.ins().return_(&[r]);
    b.seal_all_blocks();
    b.finalize(isa.frontend_config());
    let mut cctx = cranelift_codegen::Context::for_function(func);
    let compiled = cctx.compile(&**isa, &mut ControlPlane::default()).ok()?;
    install_code(compiled.code_buffer())
}

// ---- promotion and the native calling convention's helpers ------------------------------------

/// The lowest real code address: a proto's native word below it is tier
/// 1's count of how often the proto has been entered (stored +1; 0: never),
/// 1 means it can't be compiled, at or above it is its entry.
pub const MIN_ENTRY: usize = 4096;

/// How many entries (calls, returns into it, loop turns) before a proto is
/// compiled: NORN_JIT_THRESHOLD, 1 (the first) by default.
/// How many entries (calls, and loop turns) promote a proto to tier 1 by
/// default (NORN_JIT_THRESHOLD sets it, 1 to 4000). Code that runs once
/// (start-up, loading a module) isn't worth compiling: at 1, `norn -e` took
/// 1.07 s at tier 1 to tier 0's 0.11 s, and loading http.norn 75 ms to 45;
/// at 100, 0.54 s and 37 ms, and the benchmarks gain the same.
const THRESHOLD: usize = 100;

fn threshold() -> usize {
    static T: OnceLock<usize> = OnceLock::new();
    *T.get_or_init(|| std::env::var("NORN_JIT_THRESHOLD").ok().and_then(|s| s.parse().ok()).unwrap_or(THRESHOLD).clamp(1, 4000))
}

/// The native entry for proto, counting this entry, and compiling it once
/// it has been entered `threshold` times. 1: run it in the interpreter (not
/// yet, or it can't be compiled). Allocates nothing on the Norn heap.
pub fn entry_for(proto: Value) -> usize {
    loop {
        let n = proto.proto_native();
        if n >= MIN_ENTRY || n == 1 {
            return n;
        }
        let seen = if n == 0 { 1 } else { n };
        if seen >= threshold() {
            let entry = compile(proto).unwrap_or(1);
            // (installed unless another carrier has installed its own
            // meanwhile; the count may have moved on under us)
            loop {
                let m = proto.proto_native();
                if m >= MIN_ENTRY || m == 1 {
                    return m;
                }
                if proto.proto_native_cas(m, entry) {
                    return entry;
                }
            }
        }
        if proto.proto_native_cas(n, seen + 1) {
            return 1;
        }
    }
}
