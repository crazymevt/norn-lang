//! Tier 1: native code with Cranelift (runtime.md §8.2).
//!
//! Each proto's bytecode becomes one native function
//!
//! ```text
//! extern "C" fn(regs: *mut Value, consts: *const Value, ctx: *const JitCtx, pc: u64) -> u64
//! ```
//!
//! that starts at instruction `pc` (an entry for every instruction, through a
//! jump table) and runs until it reaches an instruction it doesn't handle.
//! It returns that instruction's index, and the interpreter executes it.
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
//!   arguments are fixnums.
//!
//! Everything else (generic calls, returns, closures, `def`, failed guards)
//! goes back to the interpreter. The full "push frame + tail call" calling
//! convention is a later step that keeps this frame layout.

use crate::bytecode::*;
use cranelift_codegen::control::ControlPlane;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{AbiParam, Block, BlockArg, Function, InstBuilder, JumpTableData, MemFlagsData, Signature, UserFuncName, types};
use cranelift_codegen::isa::OwnedTargetIsa;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use norn_rt::Value;
use norn_rt::func::{HDR, VAR_DYNBOUND};
use std::sync::{Mutex, OnceLock};

/// What native code needs from its carrier. `repr(C)`: offsets are baked
/// into the generated code.
#[repr(C)]
pub struct JitCtx {
    pub budget: *mut i64,
    pub attention: *const u64,
    pub seen: *const u64,
    pub prims: [u64; NPRIMS],
}

pub const NPRIMS: usize = 11;
/// The primitives with native fast paths, in `JitCtx::prims` order.
pub const FAST_PRIMS: [&str; NPRIMS] = ["+", "-", "<", ">", "<=", ">=", "==", "=", "inc", "dec", "zero?"];

pub type NativeFn = unsafe extern "C" fn(*mut Value, *const Value, *const JitCtx, u64) -> u64;

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

    let mut sig = Signature::new(isa.default_call_conv());
    for _ in 0..4 {
        sig.params.push(AbiParam::new(types::I64));
    }
    sig.returns.push(AbiParam::new(types::I64));
    let mut func = Function::with_name_signature(UserFuncName::user(0, 0), sig);
    let mut fbc = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut func, &mut fbc);
    let mf = MemFlagsData::trusted();

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

    for (i, &w) in ins.iter().enumerate() {
        b.switch_to_block(blocks[i]);
        let (o, ra, rb) = (op(w), a(w) as i32, b_(w) as i32);
        let bail = |b: &mut FunctionBuilder| {
            let v = b.ins().iconst(types::I64, i as i64);
            b.ins().jump(exit, &[BlockArg::Value(v)]);
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
                let f = b.ins().load(types::I64, mf, regs, ra * W);
                let x = b.ins().load(types::I64, mf, regs, (ra + 1) * W);
                let y = if rb == 2 { b.ins().load(types::I64, mf, regs, (ra + 2) * W) } else { x };
                // Both fixnums (low bit 0)?
                let tags = b.ins().bor(x, y);
                let tag = b.ins().band_imm_s(tags, 1);
                let s = b.create_block();
                let checked = b.create_block();
                b.ins().brif(tag, s, &[], checked, &[]);
                b.switch_to_block(s);
                bail(&mut b);
                b.switch_to_block(checked);
                let cands: Vec<usize> = if rb == 2 { (0..8).collect() } else { vec![8, 9, 10] };
                let store_next = |b: &mut FunctionBuilder, v| {
                    b.ins().store(mf, v, regs, ra * W);
                    goto(b, next(i));
                };
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
                            let (ok, of) = (b.create_block(), b.create_block());
                            b.ins().brif(neg, of, &[], ok, &[]);
                            b.switch_to_block(of);
                            bail(&mut b);
                            b.switch_to_block(ok);
                            store_next(&mut b, r);
                        }
                        "zero?" => {
                            let c0 = b.ins().icmp_imm_s(IntCC::Equal, x, 0);
                            let v = boolv(&mut b, c0);
                            store_next(&mut b, v);
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
                            let v = boolv(&mut b, c0);
                            store_next(&mut b, v);
                        }
                    }
                    b.switch_to_block(miss);
                }
                bail(&mut b);
            }
            _ => bail(&mut b),
        }
    }

    b.switch_to_block(exit);
    let rv = b.block_params(exit)[0];
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
