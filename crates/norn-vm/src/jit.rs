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
//! Registers live in the heap frame, so at every exit the frame is exactly
//! what the interpreter expects:
//! - mixed tier-0/tier-1 stacks, the GC, the debugger and backtraces need
//!   nothing new;
//! - "deoptimising" is just interpreting from the current pc.
//!
//! Between entries (M10 part 5.1: `entries`) native code keeps registers in
//! SSA values, writing through to the frame, except floats it keeps
//! unboxed (part 5.2: FloFlow), which it boxes when it stops.
//!
//! Handled natively:
//! - moves, constants, var reads (fast path), upvalues, branches;
//! - loop back-edges, with the safepoint check;
//! - calls to `+ - * / < > <= >= == = inc dec zero?` when the callee *is*
//!   that primitive (checked at run time, so redefinitions stay live) and
//!   the arguments are fixnums or floats (part 5.2), and `quot rem mod
//!   even? odd?` on fixnums (part 7.2);
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
//! loop turns, counted in its native word: entry_for), on the compile
//! thread (background compilation): the interpreter runs it meanwhile, and
//! picks up its code at the next call or loop turn once it's installed.
//! `NORN_JIT_SYNC=1` compiles on the carrier that reached the threshold,
//! which waits for it, as before (tests that need code compiled at a known
//! moment).

use crate::bytecode::*;
use cranelift_codegen::control::ControlPlane;
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{AbiParam, Block, BlockArg, Function, InstBuilder, JumpTableData, MemFlagsData, Signature, UserFuncName, types};
use cranelift_codegen::isa::OwnedTargetIsa;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
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
    /// Box a float (crate::vm::jit_box_float).
    pub box_float_fn: u64,
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
            box_float_fn: crate::vm::jit_box_float as *const () as u64,
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
const CTX_BOX_FLOAT: i32 = (17 + NPRIMS as i32) * W;
// A float's value (its first payload word).
const FLOAT_BITS: i32 = 2 * W - 1;
/// A string's length word (its top bit: all ASCII), and its bytes, from
/// its tagged pointer.
const STR_LEN: i32 = 2 * W - 1;
const STR_BYTES: i32 = 3 * W - 1;
// An instance's class (its first field).
const INST_CLASS: i32 = 2 * W - 1;

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
    assert!(std::mem::offset_of!(JitCtx, box_float_fn) as i32 == CTX_BOX_FLOAT);
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

pub const NPRIMS: usize = 40;
/// The primitives with native fast paths, in `JitCtx::prims` order.
pub const FAST_PRIMS: [&str; NPRIMS] = [
    "+", "-", "<", ">", "<=", ">=", "==", "=", "inc", "dec", "zero?", // (fixnums; floats, M10 part 5.2)
    "*", "/", // (M10 part 5.2)
    "quot", "rem", "mod", "even?", "odd?", // (fixnums, M10 part 7.2)
    "count", "nth", "first", "identical?", "nil?", "some?", "not", // (M10 part 4)
    "string?", "keyword?", "symbol?", "float?", "vector?", "regex?", "bytes?", "char?", "boolean?", "integer?", // (contracts' checks)
    "reduced?", "unreduced", // (reduce's loop, every item)
    "seq", "next", "deref", // (a list's walk; an atom's value)
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
/// Protos queued for the compile thread or being compiled (jit-stats'
/// :pending: tests wait for it to be 0 before checking compiled code).
pub static COMPILES_PENDING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Is tier 1 on? It's the default since M10 (decision 2: it won on the
/// baseline, agreed with tier 0 on every suite and sibling project, and,
/// compiling in the background, starts as fast); `NORN_TIER=0` runs the
/// interpreter alone.
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("NORN_TIER").as_deref() != Ok("0"))
}

pub(crate) fn isa() -> Option<&'static OwnedTargetIsa> {
    static ISA: OnceLock<Option<OwnedTargetIsa>> = OnceLock::new();
    ISA.get_or_init(|| {
        let mut flags = settings::builder();
        flags.set("opt_level", if std::env::var("NORN_JIT_OPT").as_deref() == Ok("none") { "none" } else { "speed" }).ok()?;
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
    let box_ref = b.import_signature(helper(2));

    // Linked calls (link_for), their closures kept alive; and a cell for
    // this code's entry, which a failed guard passes to relink.
    let my_nregs = proto.proto_nregs() as i32;
    let nregs = my_nregs as usize;
    if !regs_in_range(&ins, nregs) {
        return None;
    }
    let ssa = ssa_enabled();
    let entry_pcs = entries(&ins, ssa);
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
        // (a generic call's function and class too: the guard compares them)
        for g in links.iter().flatten().filter_map(|lk| lk.gf.as_ref()) {
            set.insert(g.gf.0);
            set.insert(g.class.0);
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
    // A call whose callee was loaded from a keyword constant, with one
    // argument: a slot or map read (kw_get), the keyword still compared at
    // run time.
    let kw_callee: Vec<Option<Value>> = {
        let cs = proto.proto_consts();
        ins.iter()
            .enumerate()
            .map(|(i, &w)| {
                if !matches!(op(w), CALL | TAILCALL) || b_(w) != 1 {
                    return None;
                }
                let k = (0..i).rev().find(|&k| a(ins[k]) == a(w))?;
                if op(ins[k]) != LOADK {
                    return None;
                }
                cs.arr_slice().get(bx(ins[k])).copied().filter(|v| v.is_keyword())
            })
            .collect()
    };
    // A call whose callee came from a local or an upvalue (a function passed
    // in, as higher-order code calls them; not a var, whose callee is linked
    // when it can be): those get call_closure's native path (part 7.3).
    let local_callee: Vec<bool> = ins
        .iter()
        .enumerate()
        .map(|(i, &w)| {
            matches!(op(w), CALL | TAILCALL)
                && (0..i).rev().find(|&k| a(ins[k]) == a(w)).is_some_and(|k| matches!(op(ins[k]), MOVE | GETUP))
        })
        .collect();
    let ff = flo_flow(&ins, proto.proto_consts(), &named_prim, &entry_pcs, nregs, ssa && despecs(proto) < DESPEC_LIMIT);
    let kw_ref = b.import_signature(helper(2));
    let live = live_in(&ins, nregs);
    if std::env::var("NORN_JIT_TRACE_FLO").is_ok() {
        eprintln!("flo {}", norn_rt::print::pr_str(proto, true));
        for i in 0..n {
            eprintln!("  {i:3} entry={} spec={} fin={:?} live={:?}", entry_pcs[i], ff.spec[i], ff.fin[i].iter().collect::<Vec<_>>(), live[i].iter().collect::<Vec<_>>());
        }
    }
    let despec_ref = b.import_signature(helper(2));

    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    let blocks: Vec<Block> = (0..n).map(|_| b.create_block()).collect();
    let exit = b.create_block(); // param: pc to hand back
    b.append_block_param(exit, types::I64);

    // Cranelift variables per register (see `entries`): its value, which an
    // entry loads from the frame (those nothing reads are dropped); and
    // while it surely holds a float (FloFlow), the f64, and whether `vars`
    // and the frame hold it boxed (1) or not yet (0).
    let vars: Vec<Variable> = (0..nregs).map(|_| b.declare_var(types::I64)).collect();
    let fvars: Vec<Variable> = (0..nregs).map(|_| b.declare_var(types::F64)).collect();
    let bvars: Vec<Variable> = (0..nregs).map(|_| b.declare_var(types::I8)).collect();

    b.switch_to_block(entry);
    let p = b.block_params(entry).to_vec();
    let (regs, consts, ctx, pc) = (p[0], p[1], p[2], p[3]);
    let pc32 = b.ins().ireduce(types::I32, pc);
    let exit_pc = [BlockArg::Value(pc)];
    let default = b.func.dfg.block_call(exit, &exit_pc);
    // (a pc that isn't an entry stops at once: the interpreter runs on)
    let stubs: Vec<Option<Block>> = (0..n).map(|k| entry_pcs[k].then(|| b.create_block())).collect();
    let table: Vec<_> = stubs
        .iter()
        .map(|s| match s {
            Some(blk) => b.func.dfg.block_call(*blk, &[]),
            None => b.func.dfg.block_call(exit, &exit_pc),
        })
        .collect();
    let jt = b.create_jump_table(JumpTableData::new(default, &table));
    b.ins().br_table(pc32, jt);
    let float_layout = &norn_rt::object::LAYOUTS[norn_rt::Kind::Float as usize] as *const norn_rt::object::Layout as i64;
    // (v is a float: tag 1 and the float layout; else to `no`)
    let check_float = |b: &mut FunctionBuilder, v: cranelift_codegen::ir::Value, no: Block| {
        let tag = b.ins().band_imm_s(v, norn_rt::value::TAG_MASK as i64);
        let obj = b.ins().icmp_imm_s(IntCC::Equal, tag, norn_rt::value::TAG_OBJ as i64);
        let ok = b.create_block();
        b.ins().brif(obj, ok, &[], no, &[]);
        b.switch_to_block(ok);
        let layout = b.ins().load(types::I64, mf, v, -(norn_rt::value::TAG_OBJ as i32));
        let same = b.ins().icmp_imm_s(IntCC::Equal, layout, float_layout);
        let yes = b.create_block();
        b.ins().brif(same, yes, &[], no, &[]);
        b.switch_to_block(yes);
    };
    for (k, s) in stubs.iter().enumerate() {
        if let Some(blk) = *s {
            b.switch_to_block(blk);
            for (r, &var) in vars.iter().enumerate() {
                let v = b.ins().load(types::I64, mf, regs, r as i32 * W);
                b.def_var(var, v);
            }
            // (the registers assumed floats here: checked, unboxed; when one
            // isn't, refuse, and have the code compiled again with fewer
            // assumptions)
            if !ff.fin[k].is_empty() {
                let refuse = b.create_block();
                for r in ff.fin[k].iter() {
                    let v = b.use_var(vars[r]);
                    check_float(&mut b, v, refuse);
                    let f = b.ins().load(types::F64, mf, v, FLOAT_BITS);
                    b.def_var(fvars[r], f);
                    let one = b.ins().iconst(types::I8, 1);
                    b.def_var(bvars[r], one);
                }
                b.ins().jump(blocks[k], &[]);
                b.switch_to_block(refuse);
                let fp = b.ins().iconst(types::I64, despecialise as *const () as i64);
                let pv = b.ins().iconst(types::I64, proto.0 as i64);
                let cv = b.ins().iconst(types::I64, cell as *const std::sync::atomic::AtomicU64 as i64);
                b.ins().call_indirect(despec_ref, fp, &[pv, cv]);
                let kv = b.ins().iconst(types::I64, k as i64);
                b.ins().jump(exit, &[BlockArg::Value(kv)]);
            } else {
                b.ins().jump(blocks[k], &[]);
            }
        }
    }
    // A float register's value boxed: the box it has, or a new one (the
    // helper allocates, which never collects), written to the frame; its
    // copies (the registers of `floats` with its label) get the same box.
    let materialize = |b: &mut FunctionBuilder, r: usize, labels: &[u32], floats: &RegSet| -> cranelift_codegen::ir::Value {
        let flag = b.use_var(bvars[r]);
        let (has, make, join) = (b.create_block(), b.create_block(), b.create_block());
        b.append_block_param(join, types::I64);
        b.ins().brif(flag, has, &[], make, &[]);
        b.switch_to_block(has);
        let t = b.use_var(vars[r]);
        b.ins().jump(join, &[BlockArg::Value(t)]);
        b.switch_to_block(make);
        let f = b.use_var(fvars[r]);
        let bits = b.ins().bitcast(types::I64, MemFlagsData::new(), f);
        let fp = b.ins().load(types::I64, mf, ctx, CTX_BOX_FLOAT);
        let call = b.ins().call_indirect(box_ref, fp, &[ctx, bits]);
        let t = b.inst_results(call)[0];
        b.ins().store(mf, t, regs, r as i32 * W);
        b.ins().jump(join, &[BlockArg::Value(t)]);
        b.switch_to_block(join);
        let t = b.block_params(join)[0];
        let one = b.ins().iconst(types::I8, 1);
        for q in floats.iter().filter(|&q| q == r || labels[q] == labels[r]) {
            b.def_var(vars[q], t);
            b.def_var(bvars[q], one);
            if q != r {
                b.ins().store(mf, t, regs, q as i32 * W);
            }
        }
        t
    };
    // Set a register to a value (written through to the frame: storing is
    // cheap, reloading what was just stored isn't, and the frame stays
    // exact); to a float, unboxed (not yet in the frame); write back a set
    // of float registers (boxed); a float register's f64.
    //
    // (but not a temporary's that nothing reads: written at instruction k,
    // not live after it, and no local's register, its own function's or an
    // inlined one's, which the debugger shows. The interpreter reads only
    // live registers, and what the slot still holds is an older value of
    // the frame's, as good to the GC. Copying a loop's counter, its test and
    // its step into argument registers and back cost a store each.)
    let named = named_registers(proto);
    let set_at = |b: &mut FunctionBuilder, k: usize, r: i32, v: cranelift_codegen::ir::Value| {
        b.def_var(vars[r as usize], v);
        let r = r as usize;
        if r >= 256 || named.contains(r) || (k + 1 < n && live[k + 1].contains(r)) {
            b.ins().store(mf, v, regs, r as i32 * W);
        }
    };
    let setf = |b: &mut FunctionBuilder, r: i32, f: cranelift_codegen::ir::Value| {
        b.def_var(fvars[r as usize], f);
        let zero = b.ins().iconst(types::I8, 0);
        b.def_var(bvars[r as usize], zero);
    };
    let spill = |b: &mut FunctionBuilder, s: &RegSet, labels: &[u32], floats: &RegSet| {
        for r in s.iter() {
            materialize(b, r, labels, floats);
        }
    };
    let getf = |b: &mut FunctionBuilder, r: i32| b.use_var(fvars[r as usize]);
    // After a call, the float registers `floats` (all boxed for it) but
    // `except`: their f64s from their boxes.
    let reload_floats = |b: &mut FunctionBuilder, floats: &RegSet, except: usize| {
        for r in floats.iter().filter(|&r| r != except) {
            let t = b.use_var(vars[r]);
            let f = b.ins().load(types::F64, mf, t, FLOAT_BITS);
            b.def_var(fvars[r], f);
        }
    };

    let nil = Value::NIL.0 as i64;
    let tru = Value::TRUE.0 as i64;
    let fal = Value::FALSE.0 as i64;
    let unbound = Value::UNBOUND.0 as i64;

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
            let att = b.ins().load(types::I64, mf, ap, 0);
            let sp = b.ins().load(types::I64, mf, ctx, 2 * W);
            let seen = b.ins().load(types::I64, mf, sp, 0);
            let fresh = b.ins().icmp(IntCC::Equal, att, seen);
            let left = b.ins().icmp_imm_s(IntCC::SignedGreaterThan, budget, 0);
            // (a branch for each test, not their and: fewer instructions)
            let (ok, due, in_budget) = (b.create_block(), b.create_block(), b.create_block());
            b.ins().brif(fresh, in_budget, &[], due, &[]);
            b.switch_to_block(in_budget);
            b.ins().brif(left, ok, &[], due, &[]);
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
        let (fast, miss) = (b.create_block(), b.create_block());
        let f = if let Some(g) = &lk.gf {
            // (the generic function; its argument an instance of the class;
            // its methods and the classes as when linked)
            let gfv = b.ins().iconst(types::I64, g.gf.0 as i64);
            let hit = b.ins().icmp(IntCC::Equal, f, gfv);
            let step = |b: &mut FunctionBuilder, c| {
                let next = b.create_block();
                b.ins().brif(c, next, &[], miss, &[]);
                b.switch_to_block(next);
            };
            step(b, hit);
            let x = b.ins().load(types::I64, mf, regs, (ra + 1 + g.pos as i32) * W);
            let tag = b.ins().band_imm_s(x, norn_rt::value::TAG_MASK as i64);
            let obj = b.ins().icmp_imm_s(IntCC::Equal, tag, norn_rt::value::TAG_OBJ as i64);
            step(b, obj);
            let layout = b.ins().load(types::I64, mf, x, -(norn_rt::value::TAG_OBJ as i32));
            let inst = b.ins().icmp_imm_s(IntCC::Equal, layout, &norn_rt::object::LAYOUTS[norn_rt::Kind::Instance as usize] as *const norn_rt::object::Layout as i64);
            step(b, inst);
            let class = b.ins().load(types::I64, mf, x, INST_CLASS);
            let same = b.ins().icmp_imm_s(IntCC::Equal, class, g.class.0 as i64);
            step(b, same);
            let ep = b.ins().iconst(types::I64, g.gf.atomic_word(norn_rt::clos::generic::EPOCH) as *const std::sync::atomic::AtomicU64 as i64);
            let e = b.ins().atomic_load(types::I64, mf, ep);
            let same_e = b.ins().icmp_imm_s(IntCC::Equal, e, g.epoch as i64);
            step(b, same_e);
            let cp = b.ins().iconst(types::I64, &norn_rt::clos::CLASS_EPOCH as *const std::sync::atomic::AtomicU64 as i64);
            let ce = b.ins().atomic_load(types::I64, mf, cp);
            let same_c = b.ins().icmp_imm_s(IntCC::Equal, ce, g.class_epoch as i64);
            // (the frame is the effective method's)
            let em = b.ins().iconst(types::I64, lk.f.map_or(0, |f| f.0) as i64);
            b.ins().brif(same_c, fast, &[], miss, &[]);
            em
        } else {
            let want = match lk.f {
                Some(f) => b.ins().iconst(types::I64, f.0 as i64),
                None => b.ins().load(types::I64, mf, regs, (F_FN as i32 - HDR as i32) * W),
            };
            let hit = b.ins().icmp(IntCC::Equal, f, want);
            b.ins().brif(hit, fast, &[], miss, &[]);
            f
        };
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
        let att = b.ins().load(types::I64, mf, ap, 0);
        let sp = b.ins().load(types::I64, mf, ctx, 2 * W);
        let seen = b.ins().load(types::I64, mf, sp, 0);
        let fresh = b.ins().icmp(IntCC::Equal, att, seen);
        let left = b.ins().icmp_imm_s(IntCC::SignedGreaterThan, budget, 0);
        // (a branch for each test, not their and: fewer instructions)
        let (ok, due, in_budget) = (b.create_block(), b.create_block(), b.create_block());
        b.ins().brif(fresh, in_budget, &[], due, &[]);
        b.switch_to_block(in_budget);
        b.ins().brif(left, ok, &[], due, &[]);
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

    // A call of what's in register ra with rb arguments, when it's a closure
    // with a fixed arity for rb (found at run time) and its frame fits in
    // the segment: the frame written here, as a linked call's (as
    // Vm::push_frame: this one resumes after the call), and its code
    // entered (its entry safepoint first; the interpreter, if it has no
    // code yet). Anything else jumps to `general`. (M10 part 7.3: calls of
    // closures held in locals, as higher-order code makes, went through
    // the call helper.)
    let call_closure = |b: &mut FunctionBuilder, ra: i32, rb: i32, i: usize, general: Block| {
        let step = |b: &mut FunctionBuilder, c: cranelift_codegen::ir::Value| {
            let next = b.create_block();
            b.ins().brif(c, next, &[], general, &[]);
            b.switch_to_block(next);
        };
        let f = b.ins().load(types::I64, mf, regs, ra * W);
        let tag = b.ins().band_imm_s(f, norn_rt::value::TAG_MASK as i64);
        let obj = b.ins().icmp_imm_s(IntCC::Equal, tag, norn_rt::value::TAG_OBJ as i64);
        step(b, obj);
        let layout = b.ins().load(types::I64, mf, f, -(norn_rt::value::TAG_OBJ as i32));
        let is_clo = b.ins().icmp_imm_s(IntCC::Equal, layout, &norn_rt::object::LAYOUTS[norn_rt::Kind::Closure as usize] as *const norn_rt::object::Layout as i64);
        step(b, is_clo);
        // (its template's arities: the first with rb fixed parameters)
        let tmpl = b.ins().load(types::I64, mf, f, 2 * W - 1);
        let ar = b.ins().load(types::I64, mf, tmpl, 3 * W - 1);
        let n = b.ins().load(types::I64, mf, ar, 2 * W - 1);
        let (look, found) = (b.create_block(), b.create_block());
        b.append_block_param(look, types::I64);
        b.append_block_param(found, types::I64);
        let zero = b.ins().iconst(types::I64, 0);
        b.ins().jump(look, &[BlockArg::Value(zero)]);
        b.switch_to_block(look);
        let k = b.block_params(look)[0];
        let more = b.ins().icmp(IntCC::SignedLessThan, k, n);
        step(b, more);
        let off = b.ins().ishl_imm_u(k, 3);
        let at = b.ins().iadd(ar, off);
        let p = b.ins().load(types::I64, mf, at, ARR_DATA);
        let nf = b.ins().load(types::I64, mf, p, 3 * W - 1);
        let var = b.ins().load(types::I64, mf, p, 4 * W - 1);
        let same_n = b.ins().icmp_imm_s(IntCC::Equal, nf, rb as i64);
        let fixed = b.ins().icmp_imm_s(IntCC::Equal, var, 0);
        let hit = b.ins().band(same_n, fixed);
        let k1 = b.ins().iadd_imm_s(k, 1);
        b.ins().brif(hit, found, &[BlockArg::Value(p)], look, &[BlockArg::Value(k1)]);
        b.switch_to_block(found);
        let p = b.block_params(found)[0];
        let nregs = b.ins().load(types::I64, mf, p, 5 * W - 1);
        let new_regs = b.ins().iadd_imm_s(regs, ((my_nregs + HDR as i32) * W) as i64);
        let size = b.ins().ishl_imm_u(nregs, 3);
        let end = b.ins().iadd(new_regs, size);
        let seg_end = b.ins().load(types::I64, mf, ctx, CTX_SEG_END);
        let fits = b.ins().icmp(IntCC::UnsignedLessThanOrEqual, end, seg_end);
        step(b, fits);
        let resume = b.ins().iconst(types::I64, Value::fix(i as i64 + 1).0 as i64);
        b.ins().store(mf, resume, regs, (F_RESUME as i32 - HDR as i32) * W);
        let base = b.ins().load(types::I64, mf, ctx, CTX_SEG_BASE);
        let fr = b.ins().iadd_imm_s(regs, -(HDR as i64) * W as i64);
        let rel = b.ins().isub(fr, base);
        let prev = b.ins().ushr_imm_u(rel, 2); // (fp tagged: bytes / 8 · 2)
        let hdr = my_nregs * W; // (the new frame's header, from regs)
        b.ins().store(mf, f, regs, hdr + F_FN as i32 * W);
        b.ins().store(mf, p, regs, hdr + F_PROTO as i32 * W);
        let zero_fix = b.ins().iconst(types::I64, Value::fix(0).0 as i64);
        b.ins().store(mf, zero_fix, regs, hdr + F_RESUME as i32 * W);
        b.ins().store(mf, prev, regs, hdr + F_PREV as i32 * W);
        for k in 0..rb {
            let v = b.ins().load(types::I64, mf, regs, (ra + 1 + k) * W);
            b.ins().store(mf, v, new_regs, k * W);
        }
        // (its other registers nil)
        let (fill, filled) = (b.create_block(), b.create_block());
        b.append_block_param(fill, types::I64);
        let start = b.ins().iconst(types::I64, rb as i64);
        b.ins().jump(fill, &[BlockArg::Value(start)]);
        b.switch_to_block(fill);
        let k = b.block_params(fill)[0];
        let left = b.ins().icmp(IntCC::SignedLessThan, k, nregs);
        let body = b.create_block();
        b.ins().brif(left, body, &[], filled, &[]);
        b.switch_to_block(body);
        let off = b.ins().ishl_imm_u(k, 3);
        let at = b.ins().iadd(new_regs, off);
        let nilv = b.ins().iconst(types::I64, Value::NIL.0 as i64);
        b.ins().store(mf, nilv, at, 0);
        let k1 = b.ins().iadd_imm_s(k, 1);
        b.ins().jump(fill, &[BlockArg::Value(k1)]);
        b.switch_to_block(filled);
        // (its entry safepoint, as enter_linked's; then its code, or the
        // interpreter there)
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
        let att = b.ins().load(types::I64, mf, ap, 0);
        let sp = b.ins().load(types::I64, mf, ctx, 2 * W);
        let seen = b.ins().load(types::I64, mf, sp, 0);
        let fresh = b.ins().icmp(IntCC::Equal, att, seen);
        let left = b.ins().icmp_imm_s(IntCC::SignedGreaterThan, budget, 0);
        // (a branch for each test, not their and: fewer instructions)
        let (ok, due, in_budget) = (b.create_block(), b.create_block(), b.create_block());
        b.ins().brif(fresh, in_budget, &[], due, &[]);
        b.switch_to_block(in_budget);
        b.ins().brif(left, ok, &[], due, &[]);
        b.switch_to_block(due);
        stop(b, POLL_FLAG as i64);
        b.switch_to_block(ok);
        let naddr = b.ins().iadd_imm_s(p, PROTO_NATIVE as i64);
        let nat = b.ins().atomic_load(types::I64, mf, naddr);
        let compiled = b.ins().icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, nat, MIN_ENTRY as i64);
        let (native, interp) = (b.create_block(), b.create_block());
        b.ins().brif(compiled, native, &[], interp, &[]);
        b.switch_to_block(native);
        let carr = b.ins().load(types::I64, mf, p, PROTO_CONSTS);
        let cs = b.ins().iadd_imm_s(carr, ARR_DATA as i64);
        let zero = b.ins().iconst(types::I64, 0);
        b.ins().return_call_indirect(native_ref, nat, &[new_regs, cs, ctx, zero]);
        b.switch_to_block(interp);
        stop(b, 0);
    };

    // Every return goes through two shared blocks: ret_inline (param: v),
    // when the caller's frame is below this one, and ret_helper (params: v,
    // the register's byte offset, the pc for the interpreter) otherwise:
    // see ret_value, and their bodies after the instructions.
    let ret_inline = b.create_block();
    b.append_block_param(ret_inline, types::I64);
    let ret_helper = b.create_block();
    for _ in 0..3 {
        b.append_block_param(ret_helper, types::I64);
    }

    // Before going from where the floats are fo (their copies ao) to
    // instruction s: those s doesn't know of, and will read, boxed (and
    // copies that stop being known as copies there).
    let spill_into = |b: &mut FunctionBuilder, fo: &RegSet, ao: &[u32], s: usize| {
        let keep = fo.intersect(&live[s]);
        let mut need = fo.minus(&ff.fin[s]).intersect(&live[s]);
        for r in keep.iter() {
            for q in keep.iter().filter(|&q| q > r && ao[q] == ao[r]) {
                if !(ff.fin[s].contains(r) && ff.fin[s].contains(q) && ff.ain[s][r] == ff.ain[s][q]) {
                    need.insert(r);
                }
            }
        }
        spill(b, &need, ao, fo);
    };

    for (i, &w) in ins.iter().enumerate() {
        b.switch_to_block(blocks[i]);
        let (o, ra, rb) = (op(w), a(w) as i32, b_(w) as i32);
        let set = |b: &mut FunctionBuilder, r: i32, v: cranelift_codegen::ir::Value| set_at(b, i, r, v);
        // (the registers surely floats as this instruction starts and ends:
        // FloFlow)
        let (fin, fout) = (&ff.fin[i], &ff.fout[i]);
        let (ain, aout) = (&ff.ain[i][..], &ff.aout[i][..]);
        // A register's value (a float's, boxed if it isn't yet).
        let get = |b: &mut FunctionBuilder, r: i32| if fin.contains(r as usize) { materialize(b, r as usize, ain, fin) } else { b.use_var(vars[r as usize]) };
        // Stop at this instruction, the frame already written back; or write
        // it back first (bail).
        let stop_at = |b: &mut FunctionBuilder, pc: usize| {
            let v = b.ins().iconst(types::I64, pc as i64);
            b.ins().jump(exit, &[BlockArg::Value(v)]);
        };
        let stop_here = |b: &mut FunctionBuilder| stop_at(b, i);
        let bail = |b: &mut FunctionBuilder| {
            spill(b, fin, ain, fin);
            stop_here(b);
        };
        // Stop at this instruction with PENDING_FLAG: a primitive called
        // natively failed or asked to park (the Vm says which).
        let pending = |b: &mut FunctionBuilder| {
            let v = b.ins().iconst(types::I64, (i as u64 | PENDING_FLAG) as i64);
            b.ins().jump(exit, &[BlockArg::Value(v)]);
        };
        // Go on at instruction `to` (the floats it doesn't know of, and
        // will read, boxed first); past the end, stop there.
        // Go from the end of instruction k (its floats fo, their copies ao)
        // to instruction `to`; past the end, stop there.
        let goto_from = |b: &mut FunctionBuilder, k: usize, fo: &RegSet, ao: &[u32], to: Option<usize>| match to {
            Some(s) => {
                spill_into(b, fo, ao, s);
                b.ins().jump(blocks[s], &[]);
            }
            None => {
                spill(b, fo, ao, fo);
                stop_at(b, k + 1);
            }
        };
        let goto = |b: &mut FunctionBuilder, to: Option<usize>| goto_from(b, i, fout, aout, to);
        let nextpc = (i + 1 < n).then_some(i + 1);
        // Return v from this frame: inline when the caller's frame is below
        // this one in the segment (not a run's bottom, not across segments)
        // and v isn't a retry, this frame then gone; else through the helper,
        // with float registers `dirty` written back first (it may fail, and
        // the interpreter return from here). `ra`: the register v goes back
        // to if the interpreter must return it; at `fail_pc` it does: this
        // RETURN, or the `RETURN ra` after a TAILCALL whose call has been
        // made (re-running the TAILCALL would call the primitive twice).
        let ret_value = |b: &mut FunctionBuilder, v: cranelift_codegen::ir::Value, ra: i32, fail_pc: usize, dirty: &RegSet| {
            let is_retry = b.ins().icmp_imm_s(IntCC::Equal, v, Value::RETRY.0 as i64);
            let prev = b.ins().load(types::I64, mf, regs, (F_PREV as i32 - HDR as i32) * W);
            let below = b.ins().icmp_imm_s(IntCC::SignedLessThan, prev, 0);
            let slow = b.ins().bor(is_retry, below);
            let site_slow = b.create_block();
            b.ins().brif(slow, site_slow, &[], ret_inline, &[BlockArg::Value(v)]);
            b.switch_to_block(site_slow);
            spill(b, dirty, ain, fin);
            let off = b.ins().iconst(types::I64, (ra * W) as i64);
            let fp = b.ins().iconst(types::I64, fail_pc as i64);
            b.ins().jump(ret_helper, &[BlockArg::Value(v), BlockArg::Value(off), BlockArg::Value(fp)]);
        };
        // A call of what's in register ra with rb arguments, the frame
        // written back: the helper pushes a closure's frame (this one
        // resumes after the call when it returns) or calls a primitive; or
        // the interpreter does it. With `spec`, the call is assumed to give
        // a float (FloFlow): when its value isn't one, stop after it.
        let call_any = |b: &mut FunctionBuilder, ra: i32, rb: i32, i: usize, spec: bool| {
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
            stop_here(b);
            b.switch_to_block(pend);
            pending(b);
            b.switch_to_block(value);
            let v = b.ins().load(types::I64, mf, ctx, CTX_VALUE);
            set(b, ra, v);
            // (the floats were boxed for the call: their f64s again from
            // their boxes, so that none is live across a call, which would
            // keep it in memory on the fast paths too: no float register is
            // kept across a call)
            reload_floats(b, fin, ra as usize);
            if spec {
                let not_float = b.create_block();
                check_float(b, v, not_float);
                let f = b.ins().load(types::F64, mf, v, FLOAT_BITS);
                b.def_var(fvars[ra as usize], f);
                let one = b.ins().iconst(types::I8, 1);
                b.def_var(bvars[ra as usize], one);
                goto(b, nextpc);
                b.switch_to_block(not_float);
                stop_at(b, i + 1);
            } else {
                goto(b, nextpc);
            }
            b.switch_to_block(pushed);
            let new_regs = b.ins().load(types::I64, mf, ctx, CTX_NEXT_REGS);
            let zero = b.ins().iconst(types::I64, 0);
            enter(b, new_regs, zero, true);
        };
        // A keyword's read of the argument (kw_callee): on_value with what
        // kw_get gives; the builder is left where it doesn't apply.
        let kw_read = |b: &mut FunctionBuilder, kw: Value, on_value: &dyn Fn(&mut FunctionBuilder, cranelift_codegen::ir::Value)| {
            let f = get(b, ra);
            let same = b.ins().icmp_imm_s(IntCC::Equal, f, kw.0 as i64);
            let (go, no) = (b.create_block(), b.create_block());
            b.ins().brif(same, go, &[], no, &[]);
            b.switch_to_block(go);
            let x = get(b, ra + 1);
            let kv = b.ins().iconst(types::I64, kw.0 as i64);
            let fp = b.ins().iconst(types::I64, kw_get as *const () as i64);
            let call = b.ins().call_indirect(kw_ref, fp, &[x, kv]);
            let v = b.inst_results(call)[0];
            let got = b.ins().icmp_imm_s(IntCC::NotEqual, v, Value::UNBOUND.0 as i64);
            let yes = b.create_block();
            b.ins().brif(got, yes, &[], no, &[]);
            b.switch_to_block(yes);
            on_value(b, v);
            b.switch_to_block(no);
        };
        // A fast primitive's arguments: a float register's f64 for the
        // numeric ones (no box needed), else the value.
        let numeric = named_prim[i].is_some_and(|k| matches!(FAST_PRIMS[k], "+" | "-" | "*" | "/" | "inc" | "dec" | "zero?" | "<" | ">" | "<=" | ">=" | "==" | "="));
        let arg = |b: &mut FunctionBuilder, r: i32| if numeric && fin.contains(r as usize) { Arg::F(getf(b, r)) } else { Arg::T(get(b, r)) };
        match o {
            MOVE => {
                if fin.contains(rb as usize) {
                    // (a float: its f64, its box if it has one, its flag)
                    let (f, t, fl) = (b.use_var(fvars[rb as usize]), b.use_var(vars[rb as usize]), b.use_var(bvars[rb as usize]));
                    b.def_var(fvars[ra as usize], f);
                    b.def_var(bvars[ra as usize], fl);
                    set(&mut b, ra, t);
                } else {
                    let v = get(&mut b, rb);
                    set(&mut b, ra, v);
                }
                goto(&mut b, nextpc);
            }
            LOADK => {
                let v = b.ins().load(types::I64, mf, consts, bx(w) as i32 * W);
                set(&mut b, ra, v);
                if fout.contains(ra as usize) {
                    let f = b.ins().load(types::F64, mf, v, FLOAT_BITS);
                    b.def_var(fvars[ra as usize], f);
                    let one = b.ins().iconst(types::I8, 1);
                    b.def_var(bvars[ra as usize], one);
                }
                goto(&mut b, nextpc);
            }
            LOADI | LOADNIL | LOADTRUE | LOADFALSE => {
                let bits = match o {
                    LOADI => Value::fix(sbx(w) as i64).0 as i64,
                    LOADNIL => nil,
                    LOADTRUE => tru,
                    _ => fal,
                };
                let v = b.ins().iconst(types::I64, bits);
                set(&mut b, ra, v);
                goto(&mut b, nextpc);
            }
            GETVAR => {
                // var = consts[bx] (tagged object pointer): root at payload
                // word 2, flags at word 3; tag 1 is folded into the offsets.
                let var = b.ins().load(types::I64, mf, consts, bx(w) as i32 * W);
                let flags = b.ins().load(types::I64, mf, var, (2 + 3) * W - 1);
                let dyn_bits = b.ins().band_imm_s(flags, VAR_DYNBOUND as i64);
                let root_addr = b.ins().iadd_imm_s(var, (2 + 2) * W as i64 - 1);
                // (a plain load: the interpreter's is an acquire, but what
                // the root points to is read through it, an address
                // dependency, which orders those reads on aarch64 as x86;
                // and an acquire keeps every later load waiting for it)
                let root = b.ins().load(types::I64, mf, root_addr, 0);
                let is_unbound = b.ins().icmp_imm_s(IntCC::Equal, root, unbound);
                // (a branch for each: or-ing the two tests made them
                // booleans first, five instructions more)
                let (bound, ok, s) = (b.create_block(), b.create_block(), b.create_block());
                b.ins().brif(dyn_bits, s, &[], bound, &[]);
                b.switch_to_block(bound);
                b.ins().brif(is_unbound, s, &[], ok, &[]);
                b.switch_to_block(s);
                bail(&mut b);
                b.switch_to_block(ok);
                set(&mut b, ra, root);
                goto(&mut b, nextpc);
            }
            GETUP | SELF => {
                // The frame's closure is at regs - HDR (+F_FN = 0).
                let clo = b.ins().load(types::I64, mf, regs, -(HDR as i32) * W);
                let v = if o == SELF { clo } else { b.ins().load(types::I64, mf, clo, (2 + 2 + rb) * W - 1) };
                set(&mut b, ra, v);
                goto(&mut b, nextpc);
            }
            // (the interpreter runs it: its closure is only known at run
            // time, and it checks the upvalue is there; letrec only)
            SETUP => bail(&mut b),
            JMP => goto(&mut b, jump_target(i, w, n)),
            // (a float is true)
            JMPF if fin.contains(ra as usize) => goto(&mut b, nextpc),
            JMPF => {
                let v = get(&mut b, ra);
                let is_nil = b.ins().icmp_imm_s(IntCC::Equal, v, nil);
                let is_false = b.ins().icmp_imm_s(IntCC::Equal, v, fal);
                let falsey = b.ins().bor(is_nil, is_false);
                let (t, f) = (b.create_block(), b.create_block());
                b.ins().brif(falsey, t, &[], f, &[]);
                b.switch_to_block(t);
                goto(&mut b, jump_target(i, w, n));
                b.switch_to_block(f);
                goto(&mut b, nextpc);
            }
            LOOP => {
                // Safepoint: budget and attention, as in Vm::poll.
                let bp = b.ins().load(types::I64, mf, ctx, 0);
                let budget = b.ins().load(types::I64, mf, bp, 0);
                let budget = b.ins().iadd_imm_s(budget, -1);
                b.ins().store(mf, budget, bp, 0);
                let ap = b.ins().load(types::I64, mf, ctx, W);
                let att = b.ins().load(types::I64, mf, ap, 0);
                let sp = b.ins().load(types::I64, mf, ctx, 2 * W);
                let seen = b.ins().load(types::I64, mf, sp, 0);
                let fresh = b.ins().icmp(IntCC::Equal, att, seen);
                let left = b.ins().icmp_imm_s(IntCC::SignedGreaterThan, budget, 0);
                // (a branch for each test, not their and: fewer instructions)
                let (t, s, in_budget) = (b.create_block(), b.create_block(), b.create_block());
                b.ins().brif(fresh, in_budget, &[], s, &[]);
                b.switch_to_block(in_budget);
                b.ins().brif(left, t, &[], s, &[]);
                b.switch_to_block(s);
                bail(&mut b);
                b.switch_to_block(t);
                goto(&mut b, jump_target(i, w, n));
            }
            CALL => {
                let spec = ff.spec[i];
                // (a fast primitive's value, a float unboxed when assumed
                // one; a linked callee's frame pushed here, when the guard
                // holds and it fits; else as any call)
                if let Some(kw) = kw_callee[i] {
                    kw_read(&mut b, kw, &|b: &mut FunctionBuilder, v| {
                        set(b, ra, v);
                        goto(b, nextpc);
                    });
                } else if rb == 1 || rb == 2 || (rb == 3 && numeric) {
                    let slow = b.create_block();
                    let store_next = |b: &mut FunctionBuilder, v| {
                        set(b, ra, v);
                        goto(b, nextpc);
                    };
                    let float_next = |b: &mut FunctionBuilder, f| {
                        setf(b, ra, f);
                        goto(b, nextpc);
                    };
                    let f = get(&mut b, ra);
                    let x = arg(&mut b, ra + 1);
                    let y = if rb >= 2 { arg(&mut b, ra + 2) } else { x };
                    let z = (rb == 3).then(|| arg(&mut b, ra + 3));
                    let on_float: Option<OnValue> = if spec { Some(&float_next) } else { None };
                    // (a comparison whose value a JMPF tests next, perhaps
                    // after a MOVE of it, as the compiler leaves an `if`'s
                    // test: on its fast paths it branches to where the JMPF
                    // would go, its value still stored, and moved)
                    let mv = (i + 2 < n && op(ins[i + 1]) == MOVE && b_(ins[i + 1]) as i32 == ra).then(|| a(ins[i + 1]) as i32);
                    let j = if mv.is_some() { i + 2 } else { i + 1 };
                    let tested = mv.unwrap_or(ra);
                    let fused = rb == 2
                        && named_prim[i].is_some_and(|k| matches!(FAST_PRIMS[k], "<" | ">" | "<=" | ">=" | "==" | "="))
                        && j < n
                        && op(ins[j]) == JMPF
                        && a(ins[j]) as i32 == tested
                        && !ff.fin[j].contains(tested as usize)
                        && !ff.fin[i + 1].contains(ra as usize);
                    let branch_next = |b: &mut FunctionBuilder, c: cranelift_codegen::ir::Value| {
                        let t = b.ins().iconst(types::I64, tru);
                        let f = b.ins().iconst(types::I64, fal);
                        let v = b.ins().select(c, t, f);
                        // (each stored only if read after: the MOVE and the
                        // JMPF read them here, not from the frame)
                        let set_tested = |b: &mut FunctionBuilder, r: i32| {
                            b.def_var(vars[r as usize], v);
                            let after = [(j + 1 < n).then_some(j + 1), jump_target(j, ins[j], n)];
                            let r = r as usize;
                            if r >= 256 || named.contains(r) || after.iter().flatten().any(|&s| live[s].contains(r)) {
                                b.ins().store(mf, v, regs, r as i32 * W);
                            }
                        };
                        if mv.is_some() {
                            set_at(b, i + 1, ra, v);
                        } else {
                            set_tested(b, ra);
                        }
                        spill_into(b, fout, aout, i + 1);
                        if let Some(rd) = mv {
                            set_tested(b, rd);
                            spill_into(b, &ff.fout[i + 1], &ff.aout[i + 1][..], j);
                        }
                        let (yes, no) = (b.create_block(), b.create_block());
                        b.ins().brif(c, yes, &[], no, &[]);
                        let (fo, ao) = (&ff.fout[j], &ff.aout[j][..]);
                        b.switch_to_block(yes);
                        goto_from(b, j, fo, ao, (j + 1 < n).then_some(j + 1));
                        b.switch_to_block(no);
                        goto_from(b, j, fo, ao, jump_target(j, ins[j], n));
                    };
                    let on_bool: Option<OnValue> = if fused { Some(&branch_next) } else { None };
                    fast_prims(&mut b, mf, ctx, f, x, y, z, rb, slow, named_prim[i], box_ref, &store_next, on_float, on_bool);
                    b.switch_to_block(slow);
                }
                // (from here the frame is written back: the helpers and a
                // linked callee's frame read it)
                spill(&mut b, fin, ain, fin);
                let linked = links[i].as_ref().map(|lk| linked_guard(&mut b, lk, ra));
                // (one general call, for a failed guard and a full segment)
                let general = b.create_block();
                if links[i].is_none() && kw_callee[i].is_none() && !spec && local_callee[i] {
                    // (a closure not linked, as one held in a local: its
                    // frame written here all the same, M10 part 7.3)
                    call_closure(&mut b, ra, rb, i, general);
                } else {
                    b.ins().jump(general, &[]);
                }
                b.switch_to_block(general);
                call_any(&mut b, ra, rb, i, spec);
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
                if let Some(kw) = kw_callee[i] {
                    kw_read(&mut b, kw, &|b: &mut FunctionBuilder, v| ret_value(b, v, ra, i + 1, fin));
                } else if rb == 1 || rb == 2 || (rb == 3 && numeric) {
                    let slow = b.create_block();
                    let f = get(&mut b, ra);
                    let x = arg(&mut b, ra + 1);
                    let y = if rb >= 2 { arg(&mut b, ra + 2) } else { x };
                    let z = (rb == 3).then(|| arg(&mut b, ra + 3));
                    fast_prims(&mut b, mf, ctx, f, x, y, z, rb, slow, named_prim[i], box_ref, &|b: &mut FunctionBuilder, v| ret_value(b, v, ra, i + 1, fin), None, None);
                    b.switch_to_block(slow);
                }
                spill(&mut b, fin, ain, fin);
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
                stop_here(&mut b);
                b.switch_to_block(pend);
                pending(&mut b);
                b.switch_to_block(value);
                let v = b.ins().load(types::I64, mf, ctx, CTX_VALUE);
                ret_value(&mut b, v, ra, i + 1, &RegSet::default());
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
                    stop_here(&mut b);
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
                // (attention since the last poll: the interpreter returns,
                // polling first, so a profile's sample taken now counts for
                // this function, which may have no other safepoint)
                let ap = b.ins().load(types::I64, mf, ctx, W);
                let att = b.ins().load(types::I64, mf, ap, 0);
                let sp = b.ins().load(types::I64, mf, ctx, 2 * W);
                let seen = b.ins().load(types::I64, mf, sp, 0);
                let (go, poll) = (b.create_block(), b.create_block());
                let fresh = b.ins().icmp(IntCC::Equal, att, seen);
                b.ins().brif(fresh, go, &[], poll, &[]);
                b.switch_to_block(poll);
                bail(&mut b);
                b.switch_to_block(go);
                let v = get(&mut b, ra);
                ret_value(&mut b, v, ra, i, fin);
            }
            GUARD if i + 2 < n => {
                let v = get(&mut b, ra);
                let k = b.ins().load(types::I64, mf, consts, bx(w) as i32 * W);
                let same = b.ins().icmp(IntCC::Equal, v, k);
                let (skip, on) = (b.create_block(), b.create_block());
                b.ins().brif(same, skip, &[], on, &[]);
                b.switch_to_block(skip);
                goto(&mut b, Some(i + 2));
                b.switch_to_block(on);
                goto(&mut b, Some(i + 1));
            }
            CLOSURE => {
                // (the helper makes it, as the interpreter does: it
                // allocates, which never collects)
                spill(&mut b, fin, ain, fin);
                let f = b.ins().load(types::I64, mf, ctx, CTX_CLOSURE);
                let ra_v = b.ins().iconst(types::I64, ra as i64);
                let bx_v = b.ins().iconst(types::I64, bx(w) as i64);
                b.ins().call_indirect(closure_ref, f, &[ctx, regs, ra_v, bx_v]);
                let v = b.ins().load(types::I64, mf, regs, ra * W);
                b.def_var(vars[ra as usize], v);
                reload_floats(&mut b, fin, ra as usize);
                goto(&mut b, nextpc);
            }
            _ => bail(&mut b),
        }
    }

    // The return (ret_value). Inline, as Vm::do_return (ret_inline): write
    // v to the register the caller's CALL names, and go on in the caller's
    // code (or the interpreter, if it has none yet). Else the helper
    // (jit_return, ret_helper).
    {
        b.switch_to_block(ret_inline);
        let v = b.block_params(ret_inline)[0];
        let prev = b.ins().load(types::I64, mf, regs, (F_PREV as i32 - HDR as i32) * W);
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
        b.switch_to_block(ret_helper);
        let ps = b.block_params(ret_helper).to_vec();
        let (v, ra_off, fail_pc) = (ps[0], ps[1], ps[2]);
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
    // (NORN_JIT_DUMP=text: print the machine code of what's compiled whose
    // name has text in it)
    let dump = std::env::var("NORN_JIT_DUMP").ok().filter(|t| norn_rt::print::pr_str(proto, true).contains(t.as_str()));
    if dump.is_some() {
        cctx.set_disasm(true);
    }
    let compiled = cctx.compile(&**isa, &mut ControlPlane::default()).ok()?;
    if dump.is_some() {
        eprintln!("{}\n{}", norn_rt::print::pr_str(proto, true), compiled.vcode.as_deref().unwrap_or(""));
    }
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

/// Where a fast primitive's value goes (fast_prims).
type OnValue<'a> = &'a dyn Fn(&mut FunctionBuilder, cranelift_codegen::ir::Value);

/// An argument of a fast primitive: a value, or a float register's f64
/// (M10 part 5.2).
#[derive(Clone, Copy)]
enum Arg {
    T(cranelift_codegen::ir::Value),
    F(cranelift_codegen::ir::Value),
}

impl Arg {
    /// The value, for the paths that take one (an f64 stands for itself:
    /// those paths aren't tried with one).
    fn value(self) -> cranelift_codegen::ir::Value {
        match self {
            Arg::T(v) | Arg::F(v) => v,
        }
    }
}

/// The fixnum and float fast paths of a call (CALL or TAILCALL of f with
/// rb = 1 or 2 arguments, x and y; y is x when rb is 1; a float register's
/// as its f64 for the numeric ones) of one of FAST_PRIMS: when the callee is that primitive and
/// the arguments are fixnums (or floats, for the numeric ones), the value
/// goes to on_value (a float result to on_float unboxed, when given);
/// anything else (another callee, other arguments, an overflow) jumps to
/// slow.
#[allow(clippy::too_many_arguments)]
fn fast_prims(
    b: &mut FunctionBuilder,
    mf: MemFlagsData,
    ctx: cranelift_codegen::ir::Value,
    f: cranelift_codegen::ir::Value,
    xa: Arg,
    ya: Arg,
    za: Option<Arg>,
    rb: i32,
    slow: Block,
    named: Option<usize>,
    box_ref: cranelift_codegen::ir::SigRef,
    on_value: &dyn Fn(&mut FunctionBuilder, cranelift_codegen::ir::Value),
    on_float: Option<OnValue>,
    on_bool: Option<OnValue>,
) {
    // (an unboxed argument is only given for the numeric primitives, named)
    let unboxed = matches!(xa, Arg::F(_)) || matches!(ya, Arg::F(_));
    // (three arguments: + - * only, when x or y is an unboxed float)
    if rb == 3 && !unboxed {
        b.ins().jump(slow, &[]);
        return;
    }
    let (x, y) = (xa.value(), ya.value());
    let (tru, fal, nil) = (Value::TRUE.0 as i64, Value::FALSE.0 as i64, Value::NIL.0 as i64);
    let boolv = |b: &mut FunctionBuilder, c| {
        let t = b.ins().iconst(types::I64, tru);
        let ff = b.ins().iconst(types::I64, fal);
        b.ins().select(c, t, ff)
    };
    // A comparison's outcome c (an i8): to on_bool when given, else as a
    // boolean to on_value.
    let bool_out = |b: &mut FunctionBuilder, c| match on_bool {
        Some(ob) => ob(b, c),
        None => {
            let v = boolv(b, c);
            on_value(b, v);
        }
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
    // An all-ASCII string's length (its length word, less the flag); to
    // `slow` for anything else.
    let ascii_len = |b: &mut FunctionBuilder, v: cranelift_codegen::ir::Value| {
        is_kind(b, v, norn_rt::Kind::String, slow);
        let w = b.ins().load(types::I64, mf, v, STR_LEN);
        let ascii = b.ins().icmp_imm_s(IntCC::SignedLessThan, w, 0); // (the flag is the top bit)
        let ok = b.create_block();
        b.ins().brif(ascii, ok, &[], slow, &[]);
        b.switch_to_block(ok);
        b.ins().band_imm_s(w, i64::MAX)
    };
    // A number as an f64 (M10 part 5.2): a fixnum converted (with `exact`,
    // only within 2^53, where the conversion is exact), a float's value;
    // anything else, slow.
    let to_f64 = |b: &mut FunctionBuilder, v: cranelift_codegen::ir::Value, exact: bool| {
        let (fx, ob, join) = (b.create_block(), b.create_block(), b.create_block());
        b.append_block_param(join, types::F64);
        let tag = b.ins().band_imm_s(v, 1);
        b.ins().brif(tag, ob, &[], fx, &[]);
        b.switch_to_block(fx);
        let i = b.ins().sshr_imm_u(v, 1);
        if exact {
            let biased = b.ins().iadd_imm_s(i, 1i64 << 53);
            let ok = b.ins().icmp_imm_s(IntCC::UnsignedLessThanOrEqual, biased, 1i64 << 54);
            let c = b.create_block();
            b.ins().brif(ok, c, &[], slow, &[]);
            b.switch_to_block(c);
        }
        let f = b.ins().fcvt_from_sint(types::F64, i);
        b.ins().jump(join, &[BlockArg::Value(f)]);
        b.switch_to_block(ob);
        is_kind(b, v, norn_rt::Kind::Float, slow);
        let f = b.ins().load(types::F64, mf, v, FLOAT_BITS);
        b.ins().jump(join, &[BlockArg::Value(f)]);
        b.switch_to_block(join);
        b.block_params(join)[0]
    };
    // A float, boxed (the helper allocates, which never collects).
    let box_float = |b: &mut FunctionBuilder, f: cranelift_codegen::ir::Value| {
        let bits = b.ins().bitcast(types::I64, MemFlagsData::new(), f);
        let fp = b.ins().load(types::I64, mf, ctx, CTX_BOX_FLOAT);
        let call = b.ins().call_indirect(box_ref, fp, &[ctx, bits]);
        b.inst_results(call)[0]
    };
    // A numeric argument's f64: an unboxed float's, or as to_f64.
    let num = |b: &mut FunctionBuilder, a: Arg, exact: bool| match a {
        Arg::F(f) => f,
        Arg::T(v) => to_f64(b, v, exact),
    };
    let float_out = |b: &mut FunctionBuilder, r: cranelift_codegen::ir::Value| match on_float {
        Some(of) => of(b, r),
        None => {
            let v = box_float(b, r);
            on_value(b, v);
        }
    };
    // (a callee not loaded from a fast primitive's var tries only the few
    // most common: trying all of them made every such call site's code
    // large, and compiling slow)
    let cands: Vec<usize> = (0..NPRIMS)
        .filter(|&k| match named {
            Some(n) => n == k,
            None => std::env::var("NORN_JIT_CASCADE").is_ok() || matches!(FAST_PRIMS[k], "+" | "-" | "<" | "=" | "inc"),
        })
        .filter(|&k| !unboxed || matches!(FAST_PRIMS[k], "+" | "-" | "*" | "/" | "inc" | "dec" | "zero?" | "<" | ">" | "<=" | ">=" | "==" | "="))
        .filter(|&k| rb != 3 || matches!(FAST_PRIMS[k], "+" | "-" | "*"))
        .filter(|&k| match FAST_PRIMS[k] {
            "+" | "-" | "*" if rb == 3 => true,
            "even?" | "odd?" => rb == 1,
            "inc" | "dec" | "zero?" | "count" | "first" | "nil?" | "some?" | "not" | "string?" | "keyword?" | "symbol?"
            | "float?" | "vector?" | "regex?" | "bytes?" | "char?" | "boolean?" | "integer?" | "reduced?" | "unreduced" | "seq"
            | "next" | "deref" => rb == 1,
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
            "+" | "-" | "*" | "/" | "inc" | "dec" if unboxed => {
                // (an argument is a float: float arithmetic, the other
                // converted, as numeric.rs's arith2)
                let p = FAST_PRIMS[k];
                let fx = num(b, xa, false);
                let fy = if matches!(p, "inc" | "dec") { b.ins().f64const(1.0) } else { num(b, ya, false) };
                let fop = |b: &mut FunctionBuilder, l, r| match p {
                    "+" | "inc" => b.ins().fadd(l, r),
                    "-" | "dec" => b.ins().fsub(l, r),
                    "*" => b.ins().fmul(l, r),
                    _ => b.ins().fdiv(l, r),
                };
                let mut r = fop(b, fx, fy);
                if let Some(z) = za {
                    let fz = num(b, z, false);
                    r = fop(b, r, fz);
                }
                float_out(b, r);
            }
            "+" | "-" | "*" | "/" | "inc" | "dec" => {
                let p = FAST_PRIMS[k];
                let (fix, flo) = (b.create_block(), b.create_block());
                let tags = b.ins().bor(x, y);
                let tag = b.ins().band_imm_s(tags, 1);
                b.ins().brif(tag, flo, &[], fix, &[]);
                // (two fixnums: as raw 2n values, overflow to the slow path;
                // a fixnum quotient may be a ratio: the slow path)
                b.switch_to_block(fix);
                if p == "/" {
                    b.ins().jump(slow, &[]);
                } else {
                    let r = if p == "*" {
                        let xs = b.ins().sshr_imm_u(x, 1);
                        let (r, ovf) = b.ins().smul_overflow(xs, y);
                        let ok = b.create_block();
                        b.ins().brif(ovf, slow, &[], ok, &[]);
                        b.switch_to_block(ok);
                        r
                    } else {
                        let (lhs, rhs) = match p {
                            "inc" | "dec" => (x, b.ins().iconst(types::I64, 2)),
                            _ => (x, y),
                        };
                        let add = matches!(p, "+" | "inc");
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
                        r
                    };
                    on_value(b, r);
                }
                // (a float and a float or fixnum: as numeric.rs's arith2, the
                // fixnum converted; anything else, the slow path)
                b.switch_to_block(flo);
                let fx = to_f64(b, x, false);
                let fy = if matches!(p, "inc" | "dec") { b.ins().f64const(1.0) } else { to_f64(b, y, false) };
                let r = match p {
                    "+" | "inc" => b.ins().fadd(fx, fy),
                    "-" | "dec" => b.ins().fsub(fx, fy),
                    "*" => b.ins().fmul(fx, fy),
                    _ => b.ins().fdiv(fx, fy),
                };
                float_out(b, r);
            }
            // (fixnums only: a zero divisor, anything else, the primitive)
            p @ ("quot" | "rem" | "mod") => {
                let tags = b.ins().bor(x, y);
                let tag = b.ins().band_imm_s(tags, 1);
                let (fix, nz) = (b.create_block(), b.create_block());
                b.ins().brif(tag, slow, &[], fix, &[]);
                b.switch_to_block(fix);
                let zero = b.ins().icmp_imm_s(IntCC::Equal, y, 0);
                b.ins().brif(zero, slow, &[], nz, &[]);
                b.switch_to_block(nz);
                // (on raw 2n values: 2a rem 2b is 2(a rem b), tagged
                // already; 2a / 2b is a / b, to tag, which overflows only
                // for the least fixnum over -1)
                let v = match p {
                    "quot" => {
                        let q = b.ins().sdiv(x, y);
                        let t = b.ins().ishl_imm_u(q, 1);
                        let back = b.ins().sshr_imm_u(t, 1);
                        let fits = b.ins().icmp(IntCC::Equal, back, q);
                        let ok = b.create_block();
                        b.ins().brif(fits, ok, &[], slow, &[]);
                        b.switch_to_block(ok);
                        t
                    }
                    "rem" => b.ins().srem(x, y),
                    _ => {
                        // (floored: a remainder whose sign isn't the
                        // divisor's gets the divisor added)
                        let r = b.ins().srem(x, y);
                        let differ = b.ins().bxor(r, y);
                        let neg = b.ins().icmp_imm_s(IntCC::SignedLessThan, differ, 0);
                        let nonzero = b.ins().icmp_imm_s(IntCC::NotEqual, r, 0);
                        let adjust = b.ins().band(neg, nonzero);
                        let ry = b.ins().iadd(r, y);
                        b.ins().select(adjust, ry, r)
                    }
                };
                on_value(b, v);
            }
            p @ ("even?" | "odd?") => {
                let tag = b.ins().band_imm_s(x, 1);
                let fix = b.create_block();
                b.ins().brif(tag, slow, &[], fix, &[]);
                b.switch_to_block(fix);
                // (n even: bit 1 of the raw 2n clear)
                let bit = b.ins().band_imm_s(x, 2);
                let cc = if p == "even?" { IntCC::Equal } else { IntCC::NotEqual };
                let c0 = b.ins().icmp_imm_s(cc, bit, 0);
                let v = boolv(b, c0);
                on_value(b, v);
            }
            "zero?" if unboxed => {
                let z = b.ins().f64const(0.0);
                let c0 = b.ins().fcmp(FloatCC::Equal, xa.value(), z);
                let v = boolv(b, c0);
                on_value(b, v);
            }
            "zero?" => {
                let (fix, flo) = (b.create_block(), b.create_block());
                let tag = b.ins().band_imm_s(x, 1);
                b.ins().brif(tag, flo, &[], fix, &[]);
                b.switch_to_block(fix);
                let c0 = b.ins().icmp_imm_s(IntCC::Equal, x, 0);
                let v = boolv(b, c0);
                on_value(b, v);
                // (a float: -0.0 is zero, NaN isn't)
                b.switch_to_block(flo);
                is_kind(b, x, norn_rt::Kind::Float, slow);
                let f = b.ins().load(types::F64, mf, x, FLOAT_BITS);
                let z = b.ins().f64const(0.0);
                let c0 = b.ins().fcmp(FloatCC::Equal, f, z);
                let v = boolv(b, c0);
                on_value(b, v);
            }
            // (a vector's count is its first word)
            "count" => {
                let not_vec = b.create_block();
                is_kind(b, x, norn_rt::Kind::Vector, not_vec);
                let n = b.ins().load(types::I64, mf, x, VEC_CNT);
                let v = b.ins().ishl_imm_u(n, 1);
                on_value(b, v);
                // (an all-ASCII string: its length word, less the flag)
                b.switch_to_block(not_vec);
                let n = ascii_len(b, x);
                let v = b.ins().ishl_imm_u(n, 1);
                on_value(b, v);
            }
            // (a vector's element in its tail: index ≥ tailoff, below count)
            "nth" => {
                let not_vec = b.create_block();
                is_kind(b, x, norn_rt::Kind::Vector, not_vec);
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
                // (an all-ASCII string's i-th byte, as a character)
                b.switch_to_block(not_vec);
                let n = ascii_len(b, x);
                let fx = b.ins().band_imm_s(y, 1);
                let ok = b.create_block();
                b.ins().brif(fx, slow, &[], ok, &[]);
                b.switch_to_block(ok);
                let i = b.ins().sshr_imm_u(y, 1);
                let inside = b.ins().icmp(IntCC::UnsignedLessThan, i, n);
                let ok2 = b.create_block();
                b.ins().brif(inside, ok2, &[], slow, &[]);
                b.switch_to_block(ok2);
                let at = b.ins().iadd(x, i);
                let byte = b.ins().uload8(types::I64, mf, at, STR_BYTES);
                let c = b.ins().ishl_imm_u(byte, 8);
                let v = b.ins().bor_imm_s(c, Value::char('\0').0 as i64);
                on_value(b, v);
            }
            // (nil, or a list: itself)
            "seq" => {
                let tag = b.ins().band_imm_s(x, norn_rt::value::TAG_MASK as i64);
                let cons = b.ins().icmp_imm_s(IntCC::Equal, tag, norn_rt::value::TAG_CONS as i64);
                let is_nil = b.ins().icmp_imm_s(IntCC::Equal, x, nil);
                let (yes, maybe) = (b.create_block(), b.create_block());
                b.ins().brif(cons, yes, &[], maybe, &[]);
                b.switch_to_block(maybe);
                b.ins().brif(is_nil, yes, &[], slow, &[]);
                b.switch_to_block(yes);
                on_value(b, x);
            }
            // (nil's is nil; a list's cdr, when that's nil or a list too)
            "next" => {
                let is_nil = b.ins().icmp_imm_s(IntCC::Equal, x, nil);
                let (none, some) = (b.create_block(), b.create_block());
                b.ins().brif(is_nil, none, &[], some, &[]);
                b.switch_to_block(none);
                let nv = b.ins().iconst(types::I64, nil);
                on_value(b, nv);
                b.switch_to_block(some);
                let tag = b.ins().band_imm_s(x, norn_rt::value::TAG_MASK as i64);
                let cons = b.ins().icmp_imm_s(IntCC::Equal, tag, norn_rt::value::TAG_CONS as i64);
                let ok = b.create_block();
                b.ins().brif(cons, ok, &[], slow, &[]);
                b.switch_to_block(ok);
                let d = b.ins().load(types::I64, mf, x, 2 * W - norn_rt::value::TAG_CONS as i32);
                let dtag = b.ins().band_imm_s(d, norn_rt::value::TAG_MASK as i64);
                let dcons = b.ins().icmp_imm_s(IntCC::Equal, dtag, norn_rt::value::TAG_CONS as i64);
                let dnil = b.ins().icmp_imm_s(IntCC::Equal, d, nil);
                let (yes, maybe) = (b.create_block(), b.create_block());
                b.ins().brif(dcons, yes, &[], maybe, &[]);
                b.switch_to_block(maybe);
                b.ins().brif(dnil, yes, &[], slow, &[]);
                b.switch_to_block(yes);
                on_value(b, d);
            }
            // (an atom's value, as atom_get: an acquire)
            "deref" => {
                is_kind(b, x, norn_rt::Kind::Atom, slow);
                let at = b.ins().iadd_imm_s(x, 2 * W as i64 - norn_rt::value::TAG_OBJ as i64);
                let v = b.ins().atomic_load(types::I64, mf, at);
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
            p @ ("string?" | "keyword?" | "symbol?" | "float?" | "vector?" | "regex?" | "bytes?" | "reduced?") => {
                use norn_rt::Kind;
                let k = match p {
                    "reduced?" => Kind::Reduced,
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
            // (a reduced value's payload; anything else, itself)
            "unreduced" => {
                let (yes, no) = (b.create_block(), b.create_block());
                is_kind(b, x, norn_rt::Kind::Reduced, no);
                b.ins().jump(yes, &[]);
                b.switch_to_block(yes);
                let v = b.ins().load(types::I64, mf, x, 2 * W - 1);
                on_value(b, v);
                b.switch_to_block(no);
                on_value(b, x);
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
            "=" if unboxed => {
                // (a float register against a float: as numbers, or both
                // NaN; against a fixnum: never; else the slow path)
                let fx = match (xa, ya) {
                    (Arg::F(f), _) => f,
                    (_, Arg::F(f)) => f,
                    _ => unreachable!(),
                };
                let fy = match (xa, ya) {
                    (Arg::F(_), Arg::F(g)) => g,
                    (Arg::F(_), Arg::T(v)) | (Arg::T(v), Arg::F(_)) => {
                        let (fixb, objb) = (b.create_block(), b.create_block());
                        let t = b.ins().band_imm_s(v, 1);
                        b.ins().brif(t, objb, &[], fixb, &[]);
                        b.switch_to_block(fixb);
                        let ff = b.ins().iconst(types::I64, fal);
                        on_value(b, ff);
                        b.switch_to_block(objb);
                        is_kind(b, v, norn_rt::Kind::Float, slow);
                        b.ins().load(types::F64, mf, v, FLOAT_BITS)
                    }
                    _ => unreachable!(),
                };
                let eq = b.ins().fcmp(FloatCC::Equal, fx, fy);
                let xn = b.ins().fcmp(FloatCC::Unordered, fx, fx);
                let yn = b.ins().fcmp(FloatCC::Unordered, fy, fy);
                let nan2 = b.ins().band(xn, yn);
                let c0 = b.ins().bor(eq, nan2);
                bool_out(b, c0);
            }
            cmp if unboxed => {
                let fx = num(b, xa, true);
                let fy = num(b, ya, true);
                let cc = match cmp {
                    "<" => FloatCC::LessThan,
                    ">" => FloatCC::GreaterThan,
                    "<=" => FloatCC::LessThanOrEqual,
                    ">=" => FloatCC::GreaterThanOrEqual,
                    _ => FloatCC::Equal,
                };
                let c0 = b.ins().fcmp(cc, fx, fy);
                bool_out(b, c0);
            }
            cmp => {
                let (fix, other) = (b.create_block(), b.create_block());
                let tags = b.ins().bor(x, y);
                let tag = b.ins().band_imm_s(tags, 1);
                b.ins().brif(tag, other, &[], fix, &[]);
                b.switch_to_block(fix);
                let cc = match cmp {
                    "<" => IntCC::SignedLessThan,
                    ">" => IntCC::SignedGreaterThan,
                    "<=" => IntCC::SignedLessThanOrEqual,
                    ">=" => IntCC::SignedGreaterThanOrEqual,
                    _ => IntCC::Equal, // == and = on fixnums
                };
                let c0 = b.ins().icmp(cc, x, y);
                bool_out(b, c0);
                b.switch_to_block(other);
                if cmp == "=" {
                    // (=: the same value is equal, NaN too; two immediates that
                    // differ (characters, booleans, nil) aren't; nor two
                    // keywords that differ: they're interned)
                    let same = b.ins().icmp(IntCC::Equal, x, y);
                    let (eq_blk, differ) = (b.create_block(), b.create_block());
                    b.ins().brif(same, eq_blk, &[], differ, &[]);
                    b.switch_to_block(eq_blk);
                    let t = b.ins().iconst(types::I8, 1);
                    bool_out(b, t);
                    b.switch_to_block(differ);
                    let xt = b.ins().band_imm_s(x, norn_rt::value::TAG_MASK as i64);
                    let yt = b.ins().band_imm_s(y, norn_rt::value::TAG_MASK as i64);
                    let ximm = b.ins().icmp_imm_s(IntCC::Equal, xt, norn_rt::value::TAG_IMM as i64);
                    let yimm = b.ins().icmp_imm_s(IntCC::Equal, yt, norn_rt::value::TAG_IMM as i64);
                    let both = b.ins().band(ximm, yimm);
                    let (ne_blk, objs) = (b.create_block(), b.create_block());
                    b.ins().brif(both, ne_blk, &[], objs, &[]);
                    b.switch_to_block(ne_blk);
                    let f = b.ins().iconst(types::I8, 0);
                    bool_out(b, f);
                    b.switch_to_block(objs);
                    let (xkw, kws, not_kws) = (b.create_block(), b.create_block(), b.create_block());
                    is_kind(b, x, norn_rt::Kind::Keyword, not_kws);
                    b.ins().jump(xkw, &[]);
                    b.switch_to_block(xkw);
                    is_kind(b, y, norn_rt::Kind::Keyword, not_kws);
                    b.ins().jump(kws, &[]);
                    b.switch_to_block(kws);
                    let f = b.ins().iconst(types::I8, 0);
                    bool_out(b, f);
                    b.switch_to_block(not_kws);
                    // (=: two floats are equal as numbers, or both NaN; a
                    // fixnum never equals a float; anything else, the slow
                    // path)
                    let (xfix, xobj) = (b.create_block(), b.create_block());
                    let xt = b.ins().band_imm_s(x, 1);
                    b.ins().brif(xt, xobj, &[], xfix, &[]);
                    b.switch_to_block(xfix);
                    is_kind(b, y, norn_rt::Kind::Float, slow);
                    let ff = b.ins().iconst(types::I64, fal);
                    on_value(b, ff);
                    b.switch_to_block(xobj);
                    is_kind(b, x, norn_rt::Kind::Float, slow);
                    let (yfix, yobj) = (b.create_block(), b.create_block());
                    let yt = b.ins().band_imm_s(y, 1);
                    b.ins().brif(yt, yobj, &[], yfix, &[]);
                    b.switch_to_block(yfix);
                    let ff = b.ins().iconst(types::I64, fal);
                    on_value(b, ff);
                    b.switch_to_block(yobj);
                    is_kind(b, y, norn_rt::Kind::Float, slow);
                    let fx = b.ins().load(types::F64, mf, x, FLOAT_BITS);
                    let fy = b.ins().load(types::F64, mf, y, FLOAT_BITS);
                    let eq = b.ins().fcmp(FloatCC::Equal, fx, fy);
                    let xn = b.ins().fcmp(FloatCC::Unordered, fx, fx);
                    let yn = b.ins().fcmp(FloatCC::Unordered, fy, fy);
                    let nan2 = b.ins().band(xn, yn);
                    let c0 = b.ins().bor(eq, nan2);
                    bool_out(b, c0);
                } else {
                    // (as floats, NaN unordered: false; a fixnum converted
                    // only where that's exact, as numeric.rs's partial_cmp
                    // compares)
                    let fx = to_f64(b, x, true);
                    let fy = to_f64(b, y, true);
                    let cc = match cmp {
                        "<" => FloatCC::LessThan,
                        ">" => FloatCC::GreaterThan,
                        "<=" => FloatCC::LessThanOrEqual,
                        ">=" => FloatCC::GreaterThanOrEqual,
                        _ => FloatCC::Equal,
                    };
                    let c0 = b.ins().fcmp(cc, fx, fy);
                    bool_out(b, c0);
                }
            }
        }
        b.switch_to_block(miss);
    }
    b.ins().jump(slow, &[]);
}

// ---- registers in SSA (M10 part 5.1) ------------------------------------------------------

/// Enter native code only at some instructions (`entries`), keeping
/// registers in SSA values and floats unboxed in between? (`NORN_JIT_SSA=0`:
/// anywhere, as before part 5, for comparison.)
fn ssa_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("NORN_JIT_SSA").as_deref() != Ok("0"))
}

/// A set of registers (at most 256).
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct RegSet([u64; 4]);

impl RegSet {
    fn insert(&mut self, r: usize) {
        self.0[r >> 6] |= 1 << (r & 63);
    }
    fn intersect(&self, o: &RegSet) -> RegSet {
        RegSet([self.0[0] & o.0[0], self.0[1] & o.0[1], self.0[2] & o.0[2], self.0[3] & o.0[3]])
    }
    fn minus(&self, o: &RegSet) -> RegSet {
        RegSet([self.0[0] & !o.0[0], self.0[1] & !o.0[1], self.0[2] & !o.0[2], self.0[3] & !o.0[3]])
    }
    fn contains(&self, r: usize) -> bool {
        r < 256 && self.0[r >> 6] & (1 << (r & 63)) != 0
    }
    fn set(&mut self, r: usize, on: bool) {
        if on {
            self.insert(r);
        } else {
            self.0[r >> 6] &= !(1 << (r & 63));
        }
    }
    fn is_empty(&self) -> bool {
        self.0 == [0; 4]
    }
    fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        (0..256).filter(|&r| self.0[r >> 6] & (1 << (r & 63)) != 0)
    }
}

/// Where native code may be entered (compile_inner keeps registers in SSA
/// values between entries: reads don't go back to the frame; writes go
/// through to it, except unboxed floats', FloFlow). The fewer entries, the
/// more an analysis of the code between them knows: at an entry, the
/// interpreter may have left anything in any register.
///
/// Entries: the first instruction; the one after each instruction native
/// code can stop at (the interpreter runs that one, then comes back), which
/// includes every call (its frame resumes after it); and a loop's head
/// (after the safepoint at its back-edge stopped). At any other pc native
/// code stops at once, and the interpreter runs on to an entry.
fn entries(ins: &[u32], ssa: bool) -> Vec<bool> {
    let n = ins.len();
    if !ssa {
        return vec![true; n];
    }
    let mut entries = vec![false; n];
    if n > 0 {
        entries[0] = true;
    }
    for (i, &w) in ins.iter().enumerate() {
        match op(w) {
            MOVE | LOADK | LOADI | LOADNIL | LOADTRUE | LOADFALSE | GETUP | SELF | CLOSURE | JMP | JMPF | RETURN => {}
            GUARD if i + 2 < n => {}
            LOOP => {
                if let Some(t) = jump_target(i, w, n) {
                    entries[t] = true;
                }
            }
            // (GETVAR's slow path, calls, and what the interpreter runs)
            _ => {
                if i + 1 < n {
                    entries[i + 1] = true;
                }
            }
        }
    }
    entries
}

/// Instruction i's jump target, if inside the code.
fn jump_target(i: usize, w: u32, n: usize) -> Option<usize> {
    let t = i as isize + 1 + sbx(w);
    (t >= 0 && (t as usize) < n).then_some(t as usize)
}

/// Every register an instruction reads or writes natively is below nregs?
/// (else it isn't compiled)
fn regs_in_range(ins: &[u32], nregs: usize) -> bool {
    ins.iter().enumerate().all(|(i, &w)| match op(w) {
        MOVE => a(w) < nregs && b(w) < nregs,
        LOADK | LOADI | LOADNIL | LOADTRUE | LOADFALSE | GETVAR | GETUP | SELF | CLOSURE | JMPF | RETURN => a(w) < nregs,
        GUARD if i + 2 < ins.len() => a(w) < nregs,
        CALL | TAILCALL => a(w) + b(w) < nregs,
        _ => true,
    })
}

// ---- floats unboxed (M10 part 5.2) --------------------------------------------------------

/// Which registers surely hold floats, as each instruction starts (`fin`)
/// and ends (`fout`), and which calls are assumed to give a float
/// (`spec`). Such a register is kept as an f64 (an unboxed value), boxed
/// only when something needs the value itself (a generic call, a return,
/// a stop, an edge into code that doesn't know it's a float), at most once
/// per value (a flag says whether it has been: identical? stays true of
/// it).
///
/// A float comes from a float constant, a move of one, a call assumed to
/// give one, or an entry that finds one. A call is assumed to give a float
/// when it's of + - * / with an argument surely a float (inc, dec: its
/// argument), its callee loaded from that primitive's var: natively, it
/// does when the callee is that primitive and the other argument a
/// number; otherwise the call is made the general way, and when its value
/// isn't a float native code stops after it. An entry other than the
/// first checks the frame's registers that are floats here; when one
/// isn't, it refuses (the interpreter runs on), and after a few refusals
/// the function is compiled again without any of this (despecialise).
///
/// The greatest fixpoint: every such register's value is made a float by
/// what wrote it, or native code stops, so assuming it in a loop's head
/// holds.
struct FloFlow {
    fin: Vec<RegSet>,
    fout: Vec<RegSet>,
    spec: Vec<bool>,
    /// Which float registers hold the same value, as each instruction
    /// starts and ends: a label per register, equal for copies (a move
    /// copies the value, as the interpreter copies its pointer). Boxing one
    /// boxes its copies with the same box, so identical? holds of them.
    ain: Vec<Vec<u32>>,
    aout: Vec<Vec<u32>>,
}

/// How many times a function's compiled code may have its assumptions
/// fail before it's compiled without them.
const DESPEC_LIMIT: u32 = 4;
static DESPECS: Mutex<Option<std::collections::HashMap<u64, u32>>> = Mutex::new(None);
pub static DESPECIALISED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn despecs(proto: Value) -> u32 {
    DESPECS.lock().unwrap().as_ref().and_then(|m| m.get(&proto.0).copied()).unwrap_or(0)
}

/// From native code: an entry found a register that isn't the float it
/// assumed. Uninstall the code (as relink does); it's compiled again with
/// fewer assumptions, none after DESPEC_LIMIT.
extern "C" fn despecialise(proto: u64, cell: u64) -> u64 {
    let proto = Value(proto);
    // SAFETY: cell is the compiled code's own entry cell (leaked).
    let entry = unsafe { (*(cell as *const std::sync::atomic::AtomicU64)).load(std::sync::atomic::Ordering::Acquire) } as usize;
    if entry >= MIN_ENTRY && proto.proto_native_cas(entry, 0) {
        let mut g = DESPECS.lock().unwrap();
        *g.get_or_insert_with(Default::default).entry(proto.0).or_insert(0) += 1;
        DESPECIALISED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    0
}

/// The registers a proto's debug locals name: its own function's, and each
/// inlined call's (vm::inline_records' [start end fname debug holes]).
fn named_registers(proto: Value) -> RegSet {
    let mut set = RegSet::default();
    let mut add = |pairs: &mut dyn Iterator<Item = (Value, Value)>| {
        for (_, r) in pairs {
            if r.is_fixnum() && (0..256).contains(&r.as_fixnum()) {
                set.insert(r.as_fixnum() as usize);
            }
        }
    };
    let dbg = proto.proto_locals();
    if dbg.is(norn_rt::Kind::Array) {
        for p in dbg.arr_slice().chunks(2) {
            if p[0].is(norn_rt::Kind::Vector) && p[0].vec_count() >= 4 {
                let d = p[0].vec_nth(3);
                if d.is(norn_rt::Kind::Vector) {
                    add(&mut (0..d.vec_count() / 2).map(|i| (d.vec_nth(2 * i), d.vec_nth(2 * i + 1))));
                }
            } else if p.len() == 2 {
                add(&mut std::iter::once((p[0], p[1])));
            }
        }
    }
    set
}

/// The registers live as each instruction starts: read before written on
/// some way on from it through the bytecode (as the interpreter runs it, so
/// past where native code stops too). An edge into an instruction that
/// doesn't know a register is a float boxes it only if it's live there.
/// (A closure's captures, and what the interpreter alone runs, count as
/// reading every register.)
fn live_in(ins: &[u32], nregs: usize) -> Vec<RegSet> {
    let n = ins.len();
    let mut all = RegSet::default();
    for r in 0..nregs {
        all.insert(r);
    }
    let succ = |i: usize, w: u32| -> [Option<usize>; 2] {
        let next = (i + 1 < n).then_some(i + 1);
        match op(w) {
            JMP | LOOP => [jump_target(i, w, n), None],
            JMPF => [jump_target(i, w, n), next],
            GUARD => [next, (i + 2 < n).then_some(i + 2)],
            RETURN | TAILCALL => [None, None],
            _ => [next, None],
        }
    };
    let mut live = vec![RegSet::default(); n];
    loop {
        let mut changed = false;
        for (i, &w) in ins.iter().enumerate().rev() {
            let mut out = RegSet::default();
            for s in succ(i, w).into_iter().flatten() {
                out = RegSet([out.0[0] | live[s].0[0], out.0[1] | live[s].0[1], out.0[2] | live[s].0[2], out.0[3] | live[s].0[3]]);
            }
            let ra = a(w);
            let mut l = out;
            match op(w) {
                MOVE => {
                    l.set(ra, false);
                    l.insert(b(w));
                }
                LOADK | LOADI | LOADNIL | LOADTRUE | LOADFALSE | GETVAR | GETUP | SELF => l.set(ra, false),
                JMP | LOOP => {}
                JMPF | GUARD | RETURN | DEFVAR => l.insert(ra),
                CALL | TAILCALL => {
                    if op(w) == CALL {
                        l.set(ra, false);
                    }
                    for r in ra..=ra + b(w) {
                        l.insert(r);
                    }
                }
                _ => l = all,
            }
            if l != live[i] {
                live[i] = l;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    live
}

/// From native code (M10 part 6.4): `(kw x)` for a keyword kw, when x is an
/// instance that isn't mutable (no owner to check) or a map; UNBOUND, which
/// no slot or map holds, for anything else: then the call is made the
/// general way.
extern "C" fn kw_get(x: u64, kw: u64) -> u64 {
    let (x, kw) = (Value(x), Value(kw));
    if x.is(norn_rt::Kind::Instance) && !x.instance_mutable() {
        return norn_rt::clos::slot_get(x, kw).unwrap_or(Value::NIL).0;
    }
    if x.is(norn_rt::Kind::Map) {
        return x.map_get(kw).unwrap_or(Value::NIL).0;
    }
    Value::UNBOUND.0
}

/// Is FAST_PRIMS[k] arithmetic that gives a float when an argument is one?
fn float_arith(k: usize, rb: usize) -> bool {
    match FAST_PRIMS[k] {
        "+" | "-" | "*" => rb == 2 || rb == 3,
        "/" => rb == 2,
        "inc" | "dec" => rb == 1,
        _ => false,
    }
}

fn flo_flow(ins: &[u32], consts: Value, named_prim: &[Option<usize>], entries: &[bool], nregs: usize, on: bool) -> FloFlow {
    let n = ins.len();
    let none = RegSet::default();
    if !on {
        return FloFlow { fin: vec![none; n], fout: vec![none; n], spec: vec![false; n], ain: vec![vec![0; nregs]; n], aout: vec![vec![0; nregs]; n] };
    }
    let mut all = RegSet::default();
    for r in 0..nregs {
        all.insert(r);
    }
    let succ = |i: usize, w: u32| -> [Option<usize>; 2] {
        let next = (i + 1 < n).then_some(i + 1);
        match op(w) {
            JMP | LOOP => [jump_target(i, w, n), None],
            JMPF => [jump_target(i, w, n), next],
            GUARD if i + 2 < n => [next, Some(i + 2)],
            MOVE | LOADK | LOADI | LOADNIL | LOADTRUE | LOADFALSE | GETVAR | GETUP | SELF | CLOSURE | CALL => [next, None],
            _ => [None, None],
        }
    };
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, &w) in ins.iter().enumerate() {
        for s in succ(i, w).into_iter().flatten() {
            preds[s].push(i);
        }
    }
    let is_float_const = |k: usize| consts.arr_slice().get(k).is_some_and(|v| v.is(norn_rt::Kind::Float));
    // (the first instruction and an entry nothing else leads to start with
    // nothing known; everything else, optimistically, with everything)
    let start = |k: usize| if k == 0 || (entries[k] && preds[k].is_empty()) { none } else { all };
    let mut fin: Vec<RegSet> = (0..n).map(start).collect();
    // (optimistic too: a loop's back-edge, not yet visited, mustn't empty
    // its head on the first pass)
    let mut fout = vec![all; n];
    let mut spec = vec![false; n];
    loop {
        let mut changed = false;
        for (i, &w) in ins.iter().enumerate() {
            let mut s = start(i);
            for &p in &preds[i] {
                s = s.intersect(&fout[p]);
            }
            if s != fin[i] {
                fin[i] = s;
                changed = true;
            }
            let ra = a(w);
            let mut o = s;
            spec[i] = false;
            match op(w) {
                MOVE => o.set(ra, s.contains(b(w))),
                LOADK => o.set(ra, is_float_const(bx(w))),
                CALL => {
                    let rb = b(w);
                    // (with three arguments, (x op y) op z: x or y a float makes
                    // both steps float arithmetic, as numeric.rs's arith does)
                    let sp = named_prim[i].is_some_and(|k| float_arith(k, rb)) && (s.contains(ra + 1) || (rb >= 2 && s.contains(ra + 2)));
                    spec[i] = sp;
                    o.set(ra, sp);
                }
                LOADI | LOADNIL | LOADTRUE | LOADFALSE | GETVAR | GETUP | SELF | CLOSURE => o.set(ra, false),
                _ => {}
            }
            fout[i] = o;
        }
        if !changed {
            break;
        }
    }
    // Copies (must-alias, optimistic as above): a value made at pc i is
    // labelled PRODUCED + i; a move gives its destination the source's
    // label; where ways meet, registers with the same labels on every way
    // in share the lowest such register's number; at an entry nothing is
    // known to be a copy (the edges into it box what they must, goto in
    // compile_inner).
    const PRODUCED: u32 = 1 << 20;
    const ENTERED: u32 = 1 << 21;
    let mut ain = vec![vec![0u32; nregs]; n];
    let mut aout = vec![vec![0u32; nregs]; n];
    loop {
        let mut changed = false;
        for (i, &w) in ins.iter().enumerate() {
            let mut l = vec![u32::MAX; nregs];
            if entries[i] || preds[i].is_empty() {
                for r in fin[i].iter() {
                    l[r] = ENTERED + r as u32;
                }
            } else if preds[i].len() == 1 {
                for r in fin[i].iter() {
                    l[r] = aout[preds[i][0]][r];
                }
            } else {
                let key = |r: usize| preds[i].iter().map(|&p| aout[p][r]).collect::<Vec<_>>();
                let regs: Vec<usize> = fin[i].iter().collect();
                for &r in &regs {
                    let k = key(r);
                    l[r] = regs.iter().copied().find(|&q| key(q) == k).unwrap_or(r) as u32;
                }
            }
            if l != ain[i] {
                ain[i] = l.clone();
                changed = true;
            }
            let ra = a(w);
            match op(w) {
                MOVE if fout[i].contains(ra) => l[ra] = l[b(w)],
                LOADK | CALL if fout[i].contains(ra) => l[ra] = PRODUCED + i as u32,
                MOVE | LOADK | LOADI | LOADNIL | LOADTRUE | LOADFALSE | GETVAR | GETUP | SELF | CLOSURE | CALL if ra < nregs => l[ra] = u32::MAX,
                _ => {}
            }
            aout[i] = l;
        }
        if !changed {
            break;
        }
    }
    FloFlow { fin, fout, spec, ain, aout }
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
/// or, from QUEUED up, its count since it was queued; 1 means it can't be
/// compiled; at or above MIN_ENTRY it is its entry. (Code is mapped far
/// above it.)
pub const MIN_ENTRY: usize = 1 << 20;

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

/// The native word of a proto waiting on the compile queue, or being
/// compiled, is QUEUED plus how often it has been entered since (up to
/// MIN_ENTRY - 1): it runs in the interpreter meanwhile, and the compile
/// thread takes the hottest first. (Above any threshold.)
const QUEUED: usize = 1 << 19;

fn queued(n: usize) -> bool {
    (QUEUED..MIN_ENTRY).contains(&n)
}

/// Compile on the carrier that reached the threshold (`NORN_JIT_SYNC=1`),
/// not on the compile thread?
fn sync_compile() -> bool {
    static S: OnceLock<bool> = OnceLock::new();
    *S.get_or_init(|| std::env::var("NORN_JIT_SYNC").as_deref() == Ok("1"))
}

/// The native entry for proto, counting this entry; once it has been
/// entered `threshold` times, queued for the compile thread (or compiled
/// here, with NORN_JIT_SYNC=1). 1: run it in the interpreter (not yet,
/// waiting to be compiled, or it can't be). Allocates nothing on the Norn
/// heap.
pub fn entry_for(proto: Value) -> usize {
    loop {
        let n = proto.proto_native();
        if n >= MIN_ENTRY || n == 1 {
            return n;
        }
        if queued(n) {
            // (counted while it waits: how hot it is decides when it's
            // compiled; a lost race costs only a count)
            if n + 1 < MIN_ENTRY {
                proto.proto_native_cas(n, n + 1);
            }
            return 1;
        }
        let seen = if n == 0 { 1 } else { n };
        if seen >= threshold() {
            if !sync_compile() {
                if proto.proto_native_cas(n, QUEUED) {
                    enqueue(proto);
                    return 1;
                }
                continue;
            }
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

// ---- background compilation ----

/// Protos waiting for the compile thread: GC roots (for_each_queued), as
/// a proto may lose its last other reference while it waits. The hottest
/// (the most entries since it was queued) is compiled first, the newest
/// of equals: first in, first out, a loop started after start-up's 300-odd
/// functions were queued waited a second behind them; newest first, a
/// function warm but no longer running could wait behind newer ones
/// indefinitely.
static QUEUE: Mutex<std::collections::VecDeque<u64>> = Mutex::new(std::collections::VecDeque::new());
static QUEUE_CV: std::sync::Condvar = std::sync::Condvar::new();

/// Every proto waiting to be compiled (GC roots: sched's for_each_root).
pub fn for_each_queued(f: &mut dyn FnMut(Value)) {
    QUEUE.lock().unwrap().iter().for_each(|&v| f(Value(v)));
}

fn enqueue(proto: Value) {
    static THREAD: OnceLock<()> = OnceLock::new();
    THREAD.get_or_init(|| {
        // (Cranelift recurses on large functions: a stack to match a
        // carrier's)
        std::thread::Builder::new().name("norn-jit".into()).stack_size(16 << 20).spawn(compile_thread).expect("the compile thread");
    });
    COMPILES_PENDING.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    QUEUE.lock().unwrap().push_back(proto.0);
    QUEUE_CV.notify_one();
}

/// The compile thread: takes protos off the queue, compiles each and
/// installs its code (unless its native word has changed meanwhile: the
/// code is then dropped). It reads the heap (the proto, its constants,
/// what its vars hold), so while compiling it's a running safepoint
/// participant: a collection waits for the compile in hand to finish (a
/// few ms) and can't free what it reads. Waiting for work, it's safe; the
/// queue keeps what's on it alive.
fn compile_thread() {
    loop {
        {
            let mut q = QUEUE.lock().unwrap();
            while q.is_empty() {
                q = QUEUE_CV.wait(q).unwrap();
            }
        }
        // (running before the proto leaves the queue, so it's a root
        // throughout: the world may stop between the wait and this)
        norn_rt::safepoint::leave_safe();
        let next = {
            let mut q = QUEUE.lock().unwrap();
            let hottest = (0..q.len()).max_by_key(|&i| Value(q[i]).proto_native());
            hottest.and_then(|i| q.remove(i))
        };
        let Some(p) = next else {
            norn_rt::safepoint::enter_safe();
            continue;
        };
        let proto = Value(p);
        let entry = std::panic::catch_unwind(|| compile(proto)).ok().flatten().unwrap_or(1);
        // (installed while it's still queued, whatever its count; if it
        // has been compiled meanwhile, or set back to count again, this
        // code isn't used)
        loop {
            let m = proto.proto_native();
            if !queued(m) || proto.proto_native_cas(m, entry) {
                break;
            }
        }
        COMPILES_PENDING.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        norn_rt::safepoint::enter_safe();
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
    /// A generic function's call (M10 part 6.4): f is the effective method
    /// its one dispatch-cache entry held, and the guard is on the generic
    /// function, the argument's class and the epochs instead.
    gf: Option<GfLink>,
}

/// A linked generic call's guard: the callee register holds `gf`, argument
/// `pos` is an instance of `class`, and gf's methods and the classes
/// haven't changed since (`epoch`, `class_epoch`).
struct GfLink {
    gf: Value,
    pos: usize,
    class: Value,
    epoch: u64,
    class_epoch: u64,
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
            .then_some(Link { f: None, proto: this, nregs, gf: None });
    }
    if op(ins[k]) != GETVAR {
        return None;
    }
    let var = consts.arr_slice().get(bx(ins[k])).copied()?;
    if !var.is(Kind::Var) {
        return None;
    }
    let f = var.var_root();
    // (a generic function: linked to the effective method its dispatch has
    // seen, for an instance of that one class; M10 part 6.4)
    let (f, gf) = if f.is(Kind::Generic) {
        if f.gf_rest() || f.gf_nreq() != rb {
            return None;
        }
        let (pos, class, em, epoch, class_epoch) = norn_rt::clos::single_dispatch(f)?;
        if pos >= rb || !class.is(Kind::Class) || class.word(norn_rt::clos::class::BUILTIN) != norn_rt::clos::class::NOT_BUILTIN {
            return None;
        }
        (em, Some(GfLink { gf: f, pos, class, epoch, class_epoch }))
    } else {
        (f, None)
    };
    if !f.is(Kind::Closure) {
        return None;
    }
    let ar = f.closure_template().template_arities();
    let proto = ar.arr_slice().iter().copied().find(|p| !p.proto_variadic() && p.proto_nfixed() == rb)?;
    let nregs = proto.proto_nregs();
    (nregs <= LINK_MAX_REGS).then_some(Link { f: Some(f), proto, nregs, gf })
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
