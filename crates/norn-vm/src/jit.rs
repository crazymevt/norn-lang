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
//! - linked calls and inline returns (M10 part 3). A CALL or TAILCALL whose
//!   callee register was last loaded by GETVAR, of a var then holding a
//!   closure with an arity for its arguments, or by SELF, is compiled
//!   against that closure (link_for). A guard compares the register with it
//!   (one compare); when it holds and the frame fits in the segment, native
//!   code writes the frame itself, exactly as `push_frame` would, and goes
//!   into the callee's code (or stops for the interpreter, if the callee
//!   has none yet). Linked closures are kept alive (LINKED), so their
//!   addresses can't be reused. A failed guard (the var was redefined)
//!   makes the call the general way at once, and asks for the caller to be
//!   compiled again (relink). A return pops its frame inline when the
//!   caller's is below it in the segment. The interpreter's state (`St`)
//!   then lags behind: every helper first syncs it from the registers it's
//!   given, and run_inner from where native code stopped.
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
use norn_rt::func::{F_FN, F_PREV, F_PROTO, F_RESUME, HDR, VAR_DYNBOUND};
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
    /// The current stack segment's frames: from seg_base up to seg_end (a
    /// linked call pushes its frame inline when it fits; a return pops
    /// inline within it). The Vm keeps them (Vm::run_inner, jit_next).
    pub seg_base: u64,
    pub seg_end: u64,
    /// A linked call's guard failed (relink).
    pub relink_fn: u64,
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
            seg_base: 0,
            seg_end: 0,
            relink_fn: relink as *const () as u64,
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
const CTX_SEG_BASE: i32 = (14 + NPRIMS as i32) * W;
// Offsets into heap objects from their tagged pointers (tag 1, two header
// words): a proto's code, constants and native word; an array's first
// element; a code object's first instruction.
const PROTO_CONSTS: i32 = (2 + 4) * W - 1;
const PROTO_CODE: i32 = (2 + 5) * W - 1;
const PROTO_NATIVE: i32 = (2 + 7) * W - 1;
const ARR_DATA: i32 = (2 + 1) * W - 1;
const CODE_INS: i32 = (2 + 1) * W - 1;
// A vector's count and tail (norn-rt's vector.rs: [count][shift][root][tail]).
const VEC_CNT: i32 = 2 * W - 1;
const VEC_SHIFT: i32 = (2 + 1) * W - 1;
const VEC_ROOT: i32 = (2 + 2) * W - 1;
const VEC_TAIL: i32 = (2 + 3) * W - 1;
const CTX_SEG_END: i32 = (15 + NPRIMS as i32) * W;
const CTX_RELINK: i32 = (16 + NPRIMS as i32) * W;

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
    assert!(std::mem::offset_of!(JitCtx, seg_base) as i32 == CTX_SEG_BASE);
    assert!(std::mem::offset_of!(JitCtx, seg_end) as i32 == CTX_SEG_END);
    assert!(std::mem::offset_of!(JitCtx, relink_fn) as i32 == CTX_RELINK);
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

pub const NPRIMS: usize = 28;
/// The primitives with native fast paths, in `JitCtx::prims` order.
pub const FAST_PRIMS: [&str; NPRIMS] = [
    "+", "-", "<", ">", "<=", ">=", "==", "=", "inc", "dec", "zero?", // (fixnums)
    "count", "nth", "first", "identical?", "nil?", "some?", "not", // (M10 part 4)
    "string?", "keyword?", "symbol?", "float?", "vector?", "regex?", "bytes?", "char?", "boolean?", "integer?", // (contracts' checks)
];

/// The trampoline: (native entry, regs, consts, ctx, pc) → pc, in the
/// platform's convention, calling the entry (Cranelift's `tail`).
pub type Trampoline = unsafe extern "C" fn(usize, *mut Value, *const Value, *mut JitCtx, u64) -> u64;

pub static COMPILED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
pub static REJECTED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
pub static CODE_BYTES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// Call sites compiled linked, and protos compiled again after a linked
/// call's guard failed (jit-stats).
pub static LINKED_SITES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
pub static RELINKED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

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
    let relink_ref = b.import_signature(helper(2));

    // Linked calls (link_for), their closures kept alive; and a cell for
    // this code's entry, which a failed guard passes to relink.
    let my_nregs = proto.proto_nregs() as i32;
    let links: Vec<Option<Link>> = if relinks(proto) < RELINK_LIMIT {
        let cs = proto.proto_consts();
        ins.iter()
            .enumerate()
            .map(|(i, &w)| if matches!(op(w), CALL | TAILCALL) { link_for(proto, &ins, cs, i, a(w), b_(w)) } else { None })
            .collect()
    } else {
        (0..n).map(|_| None).collect()
    };
    if links.iter().any(Option::is_some) {
        let mut g = LINKED.lock().unwrap();
        let set = g.get_or_insert_with(Default::default);
        for f in links.iter().flatten().filter_map(|lk| lk.f) {
            set.insert(f.0);
        }
    }
    let cell: &'static std::sync::atomic::AtomicU64 = Box::leak(Box::new(std::sync::atomic::AtomicU64::new(0)));
    // The fast primitive a call names, when its callee was loaded from that
    // primitive's norn.core var: only it is tried (still compared at run
    // time, so a redefinition stays live), not every one in turn.
    let named_prim: Vec<Option<usize>> = {
        let cs = proto.proto_consts();
        ins.iter()
            .enumerate()
            .map(|(i, &w)| {
                if !matches!(op(w), CALL | TAILCALL) {
                    return None;
                }
                let k = (0..i).rev().find(|&k| a(ins[k]) == a(w))?;
                if op(ins[k]) != GETVAR {
                    return None;
                }
                let var = cs.arr_slice().get(bx(ins[k])).copied()?;
                let name = norn_rt::print::pr_str(var, true);
                let name = name.strip_prefix("#'norn.core/")?;
                FAST_PRIMS.iter().position(|p| *p == name)
            })
            .collect()
    };

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

    // A linked call's guard: the callee register holds the linked closure.
    // Returns the block that goes on when it does; the builder is left in
    // the block for when it doesn't (which first asks for a relink).
    let linked_guard = |b: &mut FunctionBuilder, lk: &Link, ra: i32| -> (Block, cranelift_codegen::ir::Value) {
        let f = b.ins().load(types::I64, mf, regs, ra * W);
        let want = match lk.f {
            Some(f) => b.ins().iconst(types::I64, f.0 as i64),
            None => b.ins().load(types::I64, mf, regs, (F_FN as i32 - HDR as i32) * W),
        };
        let hit = b.ins().icmp(IntCC::Equal, f, want);
        let (fast, miss) = (b.create_block(), b.create_block());
        b.ins().brif(hit, fast, &[], miss, &[]);
        b.switch_to_block(miss);
        if lk.f.is_some() {
            let rf = b.ins().load(types::I64, mf, ctx, CTX_RELINK);
            let pv = b.ins().iconst(types::I64, proto.0 as i64);
            let cv = b.ins().iconst(types::I64, cell as *const std::sync::atomic::AtomicU64 as i64);
            b.ins().call_indirect(relink_ref, rf, &[pv, cv]);
        }
        (fast, f)
    };
    // Go into a linked callee whose frame is at new_regs, at pc 0: its entry
    // safepoint (as `enter`), then its code, or the interpreter if it has
    // none yet (which counts the entry towards compiling it).
    let enter_linked = |b: &mut FunctionBuilder, new_regs: cranelift_codegen::ir::Value, lk: &Link| {
        let stop = |b: &mut FunctionBuilder, r: i64| {
            b.ins().store(mf, new_regs, ctx, CTX_CUR_REGS);
            let rv = b.ins().iconst(types::I64, r);
            b.ins().return_(&[rv]);
        };
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
        stop(b, POLL_FLAG as i64);
        b.switch_to_block(ok);
        let naddr = b.ins().iconst(types::I64, lk.proto.atomic_word(7) as *const std::sync::atomic::AtomicU64 as i64);
        let nat = b.ins().atomic_load(types::I64, mf, naddr);
        let compiled = b.ins().icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, nat, MIN_ENTRY as i64);
        let (native, interp) = (b.create_block(), b.create_block());
        b.ins().brif(compiled, native, &[], interp, &[]);
        b.switch_to_block(native);
        let cs = b.ins().iconst(types::I64, lk.proto.proto_consts().arr_ptr() as i64);
        let zero = b.ins().iconst(types::I64, 0);
        b.ins().return_call_indirect(native_ref, nat, &[new_regs, cs, ctx, zero]);
        b.switch_to_block(interp);
        stop(b, 0);
    };

    // Every return goes through one block (params: v, the register's byte
    // offset, the pc for the interpreter): see ret_value, and its body
    // after the instructions.
    let ret = b.create_block();
    for _ in 0..3 {
        b.append_block_param(ret, types::I64);
    }

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
        // Return v from this frame (the shared block `ret`). `ra`: the
        // register v goes back to if the interpreter must return it; at
        // `fail_pc` it does: this RETURN, or the `RETURN ra` after a
        // TAILCALL whose call has been made (re-running the TAILCALL would
        // call the primitive twice).
        let ret_value = |b: &mut FunctionBuilder, v: cranelift_codegen::ir::Value, ra: i32, fail_pc: usize| {
            let off = b.ins().iconst(types::I64, (ra * W) as i64);
            let fp = b.ins().iconst(types::I64, fail_pc as i64);
            b.ins().jump(ret, &[BlockArg::Value(v), BlockArg::Value(off), BlockArg::Value(fp)]);
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
            CALL => {
                // (a fast primitive's value; a linked callee's frame pushed
                // here, when the guard holds and it fits; else as any call)
                if rb == 1 || rb == 2 {
                    let slow = b.create_block();
                    let store_next = |b: &mut FunctionBuilder, v| {
                        b.ins().store(mf, v, regs, ra * W);
                        goto(b, next(i));
                    };
                    fast_prims(&mut b, mf, ctx, regs, ra, rb, slow, named_prim[i], &store_next);
                    b.switch_to_block(slow);
                }
                let linked = links[i].as_ref().map(|lk| linked_guard(&mut b, lk, ra));
                // (one general call, for a failed guard and a full segment)
                let general = b.create_block();
                b.ins().jump(general, &[]);
                b.switch_to_block(general);
                call_any(&mut b, ra, rb, i);
                if let (Some(lk), Some((fast, fv))) = (links[i].as_ref(), linked) {
                    b.switch_to_block(fast);
                    let new_regs = b.ins().iadd_imm_s(regs, ((my_nregs + HDR as i32) * W) as i64);
                    let end = b.ins().iadd_imm_s(new_regs, (lk.nregs as i32 * W) as i64);
                    let seg_end = b.ins().load(types::I64, mf, ctx, CTX_SEG_END);
                    let fits = b.ins().icmp(IntCC::UnsignedLessThanOrEqual, end, seg_end);
                    let push = b.create_block();
                    b.ins().brif(fits, push, &[], general, &[]);
                    b.switch_to_block(push);
                    // (as Vm::push_frame: this frame resumes after the call)
                    let resume = b.ins().iconst(types::I64, Value::fix(i as i64 + 1).0 as i64);
                    b.ins().store(mf, resume, regs, (F_RESUME as i32 - HDR as i32) * W);
                    let base = b.ins().load(types::I64, mf, ctx, CTX_SEG_BASE);
                    let fr = b.ins().iadd_imm_s(regs, -(HDR as i64) * W as i64);
                    let rel = b.ins().isub(fr, base);
                    let prev = b.ins().ushr_imm_u(rel, 2); // (fp tagged: bytes / 8 · 2)
                    let hdr = (my_nregs) * W; // (the new frame's header, from regs)
                    b.ins().store(mf, fv, regs, hdr + F_FN as i32 * W);
                    let pv = b.ins().iconst(types::I64, lk.proto.0 as i64);
                    b.ins().store(mf, pv, regs, hdr + F_PROTO as i32 * W);
                    let zero = b.ins().iconst(types::I64, Value::fix(0).0 as i64);
                    b.ins().store(mf, zero, regs, hdr + F_RESUME as i32 * W);
                    b.ins().store(mf, prev, regs, hdr + F_PREV as i32 * W);
                    for k in 0..rb {
                        let v = b.ins().load(types::I64, mf, regs, (ra + 1 + k) * W);
                        b.ins().store(mf, v, new_regs, k * W);
                    }
                    let nilv = b.ins().iconst(types::I64, nil);
                    for k in rb..lk.nregs as i32 {
                        b.ins().store(mf, nilv, new_regs, k * W);
                    }
                    enter_linked(&mut b, new_regs, lk);
                }
            }
            TAILCALL => {
                // (a fast primitive's value, returned; a closure's frame in
                // place of this one; or another primitive's value, returned)
                if rb == 1 || rb == 2 {
                    let slow = b.create_block();
                    fast_prims(&mut b, mf, ctx, regs, ra, rb, slow, named_prim[i], &|b: &mut FunctionBuilder, v| ret_value(b, v, ra, i + 1));
                    b.switch_to_block(slow);
                }
                let linked = links[i].as_ref().map(|lk| linked_guard(&mut b, lk, ra));
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
                ret_value(&mut b, v, ra, i + 1);
                b.switch_to_block(pushed);
                let new_regs = b.ins().load(types::I64, mf, ctx, CTX_NEXT_REGS);
                let zero = b.ins().iconst(types::I64, 0);
                enter(&mut b, new_regs, zero, true);
                if let (Some(lk), Some((fast, fv))) = (links[i].as_ref(), linked) {
                    // (as Vm::tail_frame: the callee's frame in place of
                    // this one; the arguments move down, never onto one
                    // not yet moved)
                    b.switch_to_block(fast);
                    let end = b.ins().iadd_imm_s(regs, (lk.nregs as i32 * W) as i64);
                    let seg_end = b.ins().load(types::I64, mf, ctx, CTX_SEG_END);
                    let fits = b.ins().icmp(IntCC::UnsignedLessThanOrEqual, end, seg_end);
                    let (push, full) = (b.create_block(), b.create_block());
                    b.ins().brif(fits, push, &[], full, &[]);
                    b.switch_to_block(full);
                    bail(&mut b);
                    b.switch_to_block(push);
                    for k in 0..rb {
                        let v = b.ins().load(types::I64, mf, regs, (ra + 1 + k) * W);
                        b.ins().store(mf, v, regs, k * W);
                    }
                    let nilv = b.ins().iconst(types::I64, nil);
                    for k in rb..lk.nregs as i32 {
                        b.ins().store(mf, nilv, regs, k * W);
                    }
                    b.ins().store(mf, fv, regs, (F_FN as i32 - HDR as i32) * W);
                    let pv = b.ins().iconst(types::I64, lk.proto.0 as i64);
                    b.ins().store(mf, pv, regs, (F_PROTO as i32 - HDR as i32) * W);
                    let zero = b.ins().iconst(types::I64, Value::fix(0).0 as i64);
                    b.ins().store(mf, zero, regs, (F_RESUME as i32 - HDR as i32) * W);
                    enter_linked(&mut b, regs, lk);
                }
            }
            RETURN => {
                let v = b.ins().load(types::I64, mf, regs, ra * W);
                ret_value(&mut b, v, ra, i);
            }
            GUARD if i + 2 < n => {
                let v = b.ins().load(types::I64, mf, regs, ra * W);
                let k = b.ins().load(types::I64, mf, consts, bx(w) as i32 * W);
                let same = b.ins().icmp(IntCC::Equal, v, k);
                b.ins().brif(same, blocks[i + 2], &[], blocks[i + 1], &[]);
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

    // The return (ret_value). Inline, as Vm::do_return, when the caller's
    // frame is below this one in the segment (not a run's bottom, not
    // across segments) and v isn't a retry: write v to the register the
    // caller's CALL names, and go on in the caller's code (or the
    // interpreter, if it has none yet). Else the helper (jit_return).
    {
        b.switch_to_block(ret);
        let ps = b.block_params(ret).to_vec();
        let (v, ra_off, fail_pc) = (ps[0], ps[1], ps[2]);
        let helper_blk = b.create_block();
        let try1 = b.create_block();
        let is_retry = b.ins().icmp_imm_s(IntCC::Equal, v, Value::RETRY.0 as i64);
        b.ins().brif(is_retry, helper_blk, &[], try1, &[]);
        b.switch_to_block(try1);
        let prev = b.ins().load(types::I64, mf, regs, (F_PREV as i32 - HDR as i32) * W);
        let below = b.ins().icmp_imm_s(IntCC::SignedLessThan, prev, 0);
        let inline = b.create_block();
        b.ins().brif(below, helper_blk, &[], inline, &[]);
        b.switch_to_block(inline);
        let base = b.ins().load(types::I64, mf, ctx, CTX_SEG_BASE);
        let off = b.ins().ishl_imm_u(prev, 2); // (prev = fp tagged: fp·2; fp·8 bytes)
        let cfr = b.ins().iadd(base, off);
        let cregs = b.ins().iadd_imm_s(cfr, HDR as i64 * W as i64);
        let rt = b.ins().load(types::I64, mf, cfr, F_RESUME as i32 * W);
        let cproto = b.ins().load(types::I64, mf, cfr, F_PROTO as i32 * W);
        let code = b.ins().load(types::I64, mf, cproto, PROTO_CODE);
        let cpc = b.ins().ushr_imm_u(rt, 1);
        let ioff = b.ins().ishl_imm_u(cpc, 2);
        let iaddr = b.ins().iadd(code, ioff);
        let call_ins = b.ins().uload32(mf, iaddr, CODE_INS - 4);
        let dst = b.ins().ushr_imm_u(call_ins, 8);
        let dst = b.ins().band_imm_s(dst, 0xff);
        let doff = b.ins().ishl_imm_u(dst, 3);
        let daddr = b.ins().iadd(cregs, doff);
        b.ins().store(mf, v, daddr, 0);
        let naddr = b.ins().iadd_imm_s(cproto, PROTO_NATIVE as i64);
        let cnat = b.ins().atomic_load(types::I64, mf, naddr);
        let compiled = b.ins().icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, cnat, MIN_ENTRY as i64);
        let (cn, ci) = (b.create_block(), b.create_block());
        b.ins().brif(compiled, cn, &[], ci, &[]);
        b.switch_to_block(cn);
        let carr = b.ins().load(types::I64, mf, cproto, PROTO_CONSTS);
        let cconsts = b.ins().iadd_imm_s(carr, ARR_DATA as i64);
        b.ins().return_call_indirect(native_ref, cnat, &[cregs, cconsts, ctx, cpc]);
        b.switch_to_block(ci);
        b.ins().store(mf, cregs, ctx, CTX_CUR_REGS);
        b.ins().return_(&[cpc]);
        b.switch_to_block(helper_blk);
        let f = b.ins().load(types::I64, mf, ctx, CTX_RET);
        let call = b.ins().call_indirect(ret_ref, f, &[ctx, regs, v]);
        let caller = b.inst_results(call)[0];
        let failed = b.ins().icmp_imm_s(IntCC::Equal, caller, 0);
        let (no, yes) = (b.create_block(), b.create_block());
        b.ins().brif(failed, no, &[], yes, &[]);
        b.switch_to_block(no);
        let raddr = b.ins().iadd(regs, ra_off);
        b.ins().store(mf, v, raddr, 0);
        b.ins().jump(exit, &[BlockArg::Value(fail_pc)]);
        b.switch_to_block(yes);
        let pcv = b.ins().load(types::I64, mf, ctx, CTX_NEXT_PC);
        enter(&mut b, caller, pcv, false);
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
    let entry = install_code(compiled.code_buffer())?;
    cell.store(entry as u64, std::sync::atomic::Ordering::Release);
    LINKED_SITES.fetch_add(links.iter().flatten().count(), std::sync::atomic::Ordering::Relaxed);
    Some(entry)
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
    named: Option<usize>,
    on_value: &dyn Fn(&mut FunctionBuilder, cranelift_codegen::ir::Value),
) {
    let (tru, fal, nil) = (Value::TRUE.0 as i64, Value::FALSE.0 as i64, Value::NIL.0 as i64);
    let f = b.ins().load(types::I64, mf, regs, ra * W);
    let x = b.ins().load(types::I64, mf, regs, (ra + 1) * W);
    let y = if rb == 2 { b.ins().load(types::I64, mf, regs, (ra + 2) * W) } else { x };
    let boolv = |b: &mut FunctionBuilder, c| {
        let t = b.ins().iconst(types::I64, tru);
        let ff = b.ins().iconst(types::I64, fal);
        b.ins().select(c, t, ff)
    };
    // Is v a heap object of kind k (tag 1, and its layout word k's)?
    let is_kind = |b: &mut FunctionBuilder, v: cranelift_codegen::ir::Value, k: norn_rt::Kind, no: Block| {
        let tag = b.ins().band_imm_s(v, norn_rt::value::TAG_MASK as i64);
        let obj = b.ins().icmp_imm_s(IntCC::Equal, tag, norn_rt::value::TAG_OBJ as i64);
        let ok = b.create_block();
        b.ins().brif(obj, ok, &[], no, &[]);
        b.switch_to_block(ok);
        let layout = b.ins().load(types::I64, mf, v, -(norn_rt::value::TAG_OBJ as i32));
        let want = b.ins().iconst(types::I64, &norn_rt::object::LAYOUTS[k as usize] as *const norn_rt::object::Layout as i64);
        let same = b.ins().icmp(IntCC::Equal, layout, want);
        let yes = b.create_block();
        b.ins().brif(same, yes, &[], no, &[]);
        b.switch_to_block(yes);
    };
    // Both fixnums (low bit 0)? Else slow.
    let fixnums = |b: &mut FunctionBuilder| {
        let tags = b.ins().bor(x, y);
        let tag = b.ins().band_imm_s(tags, 1);
        let ok = b.create_block();
        b.ins().brif(tag, slow, &[], ok, &[]);
        b.switch_to_block(ok);
    };
    let cands: Vec<usize> = (0..NPRIMS)
        .filter(|&k| named.is_none_or(|n| n == k))
        .filter(|&k| match FAST_PRIMS[k] {
            "inc" | "dec" | "zero?" | "count" | "first" | "nil?" | "some?" | "not" | "string?" | "keyword?" | "symbol?"
            | "float?" | "vector?" | "regex?" | "bytes?" | "char?" | "boolean?" | "integer?" => rb == 1,
            _ => rb == 2,
        })
        .collect();
    for k in cands {
        let pk = b.ins().load(types::I64, mf, ctx, (3 + k as i32) * W);
        let hit = b.ins().icmp(IntCC::Equal, f, pk);
        let (h, miss) = (b.create_block(), b.create_block());
        b.ins().brif(hit, h, &[], miss, &[]);
        b.switch_to_block(h);
        match FAST_PRIMS[k] {
            "+" | "-" | "inc" | "dec" => {
                fixnums(b);
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
                fixnums(b);
                let c0 = b.ins().icmp_imm_s(IntCC::Equal, x, 0);
                let v = boolv(b, c0);
                on_value(b, v);
            }
            // (a vector's count is its first word)
            "count" => {
                is_kind(b, x, norn_rt::Kind::Vector, slow);
                let n = b.ins().load(types::I64, mf, x, VEC_CNT);
                let v = b.ins().ishl_imm_u(n, 1);
                on_value(b, v);
            }
            // (a vector's element in its tail: index ≥ tailoff, below count)
            "nth" => {
                is_kind(b, x, norn_rt::Kind::Vector, slow);
                let fx = b.ins().band_imm_s(y, 1);
                let ok = b.create_block();
                b.ins().brif(fx, slow, &[], ok, &[]);
                b.switch_to_block(ok);
                let i = b.ins().sshr_imm_u(y, 1);
                let n = b.ins().load(types::I64, mf, x, VEC_CNT);
                let inside = b.ins().icmp(IntCC::UnsignedLessThan, i, n); // (a negative i is huge)
                let ok2 = b.create_block();
                b.ins().brif(inside, ok2, &[], slow, &[]);
                b.switch_to_block(ok2);
                // tailoff = 0 below 32 elements, else ((n - 1) >> 5) << 5
                let n1 = b.ins().iadd_imm_s(n, -1);
                let t = b.ins().ushr_imm_u(n1, 5);
                let t = b.ins().ishl_imm_u(t, 5);
                let small = b.ins().icmp_imm_s(IntCC::UnsignedLessThan, n, 32);
                let zero = b.ins().iconst(types::I64, 0);
                let tailoff = b.ins().select(small, zero, t);
                let in_tail = b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, i, tailoff);
                let (tail_blk, trie_blk, walk, leaf_blk) = (b.create_block(), b.create_block(), b.create_block(), b.create_block());
                b.append_block_param(walk, types::I64); // node
                b.append_block_param(walk, types::I64); // level
                b.append_block_param(leaf_blk, types::I64); // the leaf
                b.append_block_param(leaf_blk, types::I64); // the index in it
                b.ins().brif(in_tail, tail_blk, &[], trie_blk, &[]);
                b.switch_to_block(tail_blk);
                let tail = b.ins().load(types::I64, mf, x, VEC_TAIL);
                let j = b.ins().isub(i, tailoff);
                b.ins().jump(leaf_blk, &[BlockArg::Value(tail), BlockArg::Value(j)]);
                // (the trie: from the root, a child per 5 bits of i, down
                // to the leaf, as vec_leaf_for)
                b.switch_to_block(trie_blk);
                let root = b.ins().load(types::I64, mf, x, VEC_ROOT);
                let shift = b.ins().load(types::I64, mf, x, VEC_SHIFT);
                b.ins().jump(walk, &[BlockArg::Value(root), BlockArg::Value(shift)]);
                b.switch_to_block(walk);
                let (node, level) = (b.block_params(walk)[0], b.block_params(walk)[1]);
                let at_leaf = b.ins().icmp_imm_s(IntCC::SignedLessThanOrEqual, level, 0);
                let down = b.create_block();
                let m = b.ins().band_imm_s(i, 31);
                b.ins().brif(at_leaf, leaf_blk, &[BlockArg::Value(node), BlockArg::Value(m)], down, &[]);
                b.switch_to_block(down);
                let s = b.ins().ushr(i, level);
                let s = b.ins().band_imm_s(s, 31);
                let off = b.ins().ishl_imm_u(s, 3);
                let at = b.ins().iadd(node, off);
                let child = b.ins().load(types::I64, mf, at, ARR_DATA);
                let level2 = b.ins().iadd_imm_s(level, -5);
                b.ins().jump(walk, &[BlockArg::Value(child), BlockArg::Value(level2)]);
                b.switch_to_block(leaf_blk);
                let (leaf, j) = (b.block_params(leaf_blk)[0], b.block_params(leaf_blk)[1]);
                let off = b.ins().ishl_imm_u(j, 3);
                let at = b.ins().iadd(leaf, off);
                let v = b.ins().load(types::I64, mf, at, ARR_DATA);
                on_value(b, v);
            }
            // (a list's car; nil's first is nil)
            "first" => {
                let is_nil = b.ins().icmp_imm_s(IntCC::Equal, x, nil);
                let (yes, no) = (b.create_block(), b.create_block());
                b.ins().brif(is_nil, yes, &[], no, &[]);
                b.switch_to_block(yes);
                let nv = b.ins().iconst(types::I64, nil);
                on_value(b, nv);
                b.switch_to_block(no);
                let tag = b.ins().band_imm_s(x, norn_rt::value::TAG_MASK as i64);
                let cons = b.ins().icmp_imm_s(IntCC::Equal, tag, norn_rt::value::TAG_CONS as i64);
                let ok = b.create_block();
                b.ins().brif(cons, ok, &[], slow, &[]);
                b.switch_to_block(ok);
                let car = b.ins().load(types::I64, mf, x, W - norn_rt::value::TAG_CONS as i32);
                on_value(b, car);
            }
            "identical?" => {
                let c0 = b.ins().icmp(IntCC::Equal, x, y);
                let v = boolv(b, c0);
                on_value(b, v);
            }
            "nil?" | "some?" => {
                let cc = if FAST_PRIMS[k] == "nil?" { IntCC::Equal } else { IntCC::NotEqual };
                let c0 = b.ins().icmp_imm_s(cc, x, nil);
                let v = boolv(b, c0);
                on_value(b, v);
            }
            // (a heap object of the kind: its layout word)
            p @ ("string?" | "keyword?" | "symbol?" | "float?" | "vector?" | "regex?" | "bytes?") => {
                use norn_rt::Kind;
                let k = match p {
                    "string?" => Kind::String,
                    "keyword?" => Kind::Keyword,
                    "symbol?" => Kind::Symbol,
                    "float?" => Kind::Float,
                    "vector?" => Kind::Vector,
                    "regex?" => Kind::Regex,
                    _ => Kind::Bytes,
                };
                let (yes, no) = (b.create_block(), b.create_block());
                is_kind(b, x, k, no);
                b.ins().jump(yes, &[]);
                b.switch_to_block(yes);
                let t = b.ins().iconst(types::I64, tru);
                on_value(b, t);
                b.switch_to_block(no);
                let ff = b.ins().iconst(types::I64, fal);
                on_value(b, ff);
            }
            // (an immediate whose low byte says char)
            "char?" => {
                let low = b.ins().band_imm_s(x, 0xff);
                let c0 = b.ins().icmp_imm_s(IntCC::Equal, low, Value::char('a').0 as i64 & 0xff);
                let v = boolv(b, c0);
                on_value(b, v);
            }
            // (a fixnum is; anything else, a bignum or not, the primitive says)
            "integer?" => {
                let tag = b.ins().band_imm_s(x, 1);
                let fx = b.create_block();
                b.ins().brif(tag, slow, &[], fx, &[]);
                b.switch_to_block(fx);
                let t = b.ins().iconst(types::I64, tru);
                on_value(b, t);
            }
            "boolean?" => {
                let t0 = b.ins().icmp_imm_s(IntCC::Equal, x, tru);
                let f0 = b.ins().icmp_imm_s(IntCC::Equal, x, fal);
                let c0 = b.ins().bor(t0, f0);
                let v = boolv(b, c0);
                on_value(b, v);
            }
            "not" => {
                let n0 = b.ins().icmp_imm_s(IntCC::Equal, x, nil);
                let f0 = b.ins().icmp_imm_s(IntCC::Equal, x, fal);
                let c0 = b.ins().bor(n0, f0);
                let v = boolv(b, c0);
                on_value(b, v);
            }
            cmp => {
                fixnums(b);
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

// ---- linked calls (M10 part 3) ----

/// What a CALL or TAILCALL ra rb is linked to: the closure the var whose
/// root its register was last loaded from (GETVAR) held when the caller was
/// compiled, and that closure's arity for rb arguments. Native code checks
/// that the register holds this closure (a guard), and then pushes the
/// frame itself and goes straight into the callee.
/// `f` is None for a call of the running closure itself (SELF), whose
/// arity is this proto when rb is its fixed count: the guard compares with
/// the frame's own function.
struct Link {
    f: Option<Value>,
    proto: Value,
    nregs: usize,
}

/// Linked closures, kept alive so their addresses (in compiled code) are
/// never reused by another object: the guard compares addresses. Code is
/// never freed either.
static LINKED: Mutex<Option<std::collections::HashSet<u64>>> = Mutex::new(None);

/// How many times each proto (by address) has been recompiled because a
/// linked call's guard failed. Past RELINK_LIMIT, it compiles unlinked: its
/// var keeps changing.
static RELINKS: Mutex<Option<std::collections::HashMap<u64, u32>>> = Mutex::new(None);
const RELINK_LIMIT: u32 = 8;

/// Registers a linked callee may have: its nils are stored one by one.
const LINK_MAX_REGS: usize = 64;

/// Every linked closure (GC roots: sched's for_each_root).
pub fn for_each_linked(f: &mut dyn FnMut(Value)) {
    if let Some(s) = LINKED.lock().unwrap().as_ref() {
        s.iter().for_each(|&v| f(Value(v)));
    }
}

fn relinks(proto: Value) -> u32 {
    RELINKS.lock().unwrap().as_ref().and_then(|m| m.get(&proto.0).copied()).unwrap_or(0)
}

/// The link for CALL/TAILCALL i (callee register ra, rb arguments) of
/// a proto with instructions ins and constants consts, if any.
fn link_for(this: Value, ins: &[u32], consts: Value, i: usize, ra: usize, rb: usize) -> Option<Link> {
    use norn_rt::Kind;
    // (the last instruction before it that names ra: by the compiler's
    // layout, the GETVAR or SELF that loaded the callee; if not, the guard
    // fails at run time and the call goes the general way)
    let k = (0..i).rev().find(|&k| a(ins[k]) == ra)?;
    if op(ins[k]) == SELF {
        let nregs = this.proto_nregs();
        return (!this.proto_variadic() && this.proto_nfixed() == rb && nregs <= LINK_MAX_REGS)
            .then_some(Link { f: None, proto: this, nregs });
    }
    if op(ins[k]) != GETVAR {
        return None;
    }
    let var = consts.arr_slice().get(bx(ins[k])).copied()?;
    if !var.is(Kind::Var) {
        return None;
    }
    let f = var.var_root();
    if !f.is(Kind::Closure) {
        return None;
    }
    let ar = f.closure_template().template_arities();
    let proto = ar.arr_slice().iter().copied().find(|p| !p.proto_variadic() && p.proto_nfixed() == rb)?;
    let nregs = proto.proto_nregs();
    (nregs <= LINK_MAX_REGS).then_some(Link { f: Some(f), proto, nregs })
}

/// A linked call's guard failed in the code of proto whose entry is in
/// cell: the var holds another function now. Make proto compile again
/// (counting entries from 0), against what its vars hold then, unless it
/// has been compiled again already or too often.
extern "C" fn relink(proto: u64, cell: u64) -> u64 {
    let proto = Value(proto);
    // SAFETY: cell is the compiled code's own entry cell (leaked).
    let entry = unsafe { (*(cell as *const std::sync::atomic::AtomicU64)).load(std::sync::atomic::Ordering::Acquire) } as usize;
    if entry >= MIN_ENTRY && proto.proto_native_cas(entry, 0) {
        let mut g = RELINKS.lock().unwrap();
        *g.get_or_insert_with(Default::default).entry(proto.0).or_insert(0) += 1;
        RELINKED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    0
}
