//! The FFI (runtime.md §5.6): load a library and call C functions, read and
//! write C memory, and let C call Norn back.
//!
//! **Call stubs.** For each signature, Cranelift compiles a stub
//! `extern "C" fn(target, args: *const u64, ret: *mut u64)`. It loads each
//! argument from the array as its C type (extending narrow integers as the
//! platform ABI requires; Apple arm64 has the caller do it), calls `target`
//! with the C calling convention, and stores the result. Stubs are cached by
//! signature and live in the JIT's executable memory.
//!
//! **Arguments** beyond numbers:
//! - `:bytes` passes a pointer into a `Bytes` value, which never moves.
//! - `:string` passes a NUL-terminated copy.
//! - `:out` takes a size and passes a fresh zeroed buffer, which comes back
//!   as `Bytes`.
//! - A struct by value (`[:struct size leaves]`, from `norn.ffi`'s layouts)
//!   takes the address of a copy of it, and the stub passes it as the
//!   platform says (see `Agg`). A function returning a struct takes, first,
//!   the address to put it at.
//! - After `:...`, a variadic function takes any numbers, strings, Bytes and
//!   nil, as C's default promotions make them: 64-bit integers, doubles and
//!   pointers. Their types are each call's own, so is the stub.
//!
//! **Fast or blocking.** A fast call runs on the carrier in a native region,
//! so the GC can run meanwhile and the monitor starts a spare carrier if it
//! takes too long. A `:blocking` function runs on the blocking pool, and a
//! `:main-thread` one on the main-thread lane (`main_lane.rs`), which copies
//! a string result there. Either way the Norn values it points into are
//! rooted until it returns.
//!
//! **Memory** is addressed by integers, and struct layouts are Norn
//! (`norn.ffi`). **Callbacks** are below.
//!
//! Structs by value and variadic functions are done for arm64 (Apple's and
//! AAPCS64); on other machines they're an error for now.

use crate::reactor::{self, Io};
use crate::vm::{Error, Result, Vm};
use cranelift_codegen::control::ControlPlane;
use cranelift_codegen::ir::{AbiParam, ArgumentPurpose, Function, InstBuilder, MemFlagsData, Signature, UserFuncName, types};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use norn_rt::map::{map_assoc, map_empty};
use norn_rt::runtime::rt;
use norn_rt::string::{bytes_new, float_new, str_new};
use norn_rt::vector::vec_from_slice;
use norn_rt::{Kind, Mutator, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A C type at the ABI level.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum CType {
    Void,
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
    Ptr,
}

impl CType {
    fn parse(k: &str) -> Option<CType> {
        Some(match k {
            "void" => CType::Void,
            "i8" | "char" => CType::I8,
            "u8" | "uchar" | "bool" => CType::U8,
            "i16" | "short" => CType::I16,
            "u16" | "ushort" => CType::U16,
            "i32" | "int" => CType::I32,
            "u32" | "uint" => CType::U32,
            "i64" | "isize" | "ssize" => CType::I64,
            "u64" | "usize" | "size" => CType::U64,
            // C's long: 64 bits on Unix, 32 on Windows (LLP64)
            "long" if cfg!(windows) => CType::I32,
            "ulong" if cfg!(windows) => CType::U32,
            "long" => CType::I64,
            "ulong" => CType::U64,
            "f32" | "float" => CType::F32,
            "f64" | "double" => CType::F64,
            "pointer" | "bytes" | "string" | "out" => CType::Ptr,
            _ => return None,
        })
    }

    fn abi(self) -> Option<AbiParam> {
        Some(match self {
            CType::Void => return None,
            CType::I8 => AbiParam::new(types::I8).sext(),
            CType::U8 => AbiParam::new(types::I8).uext(),
            CType::I16 => AbiParam::new(types::I16).sext(),
            CType::U16 => AbiParam::new(types::I16).uext(),
            CType::I32 => AbiParam::new(types::I32).sext(),
            CType::U32 => AbiParam::new(types::I32).uext(),
            CType::I64 | CType::U64 | CType::Ptr => AbiParam::new(types::I64),
            CType::F32 => AbiParam::new(types::F32),
            CType::F64 => AbiParam::new(types::F64),
        })
    }

    fn is_float(self) -> bool {
        matches!(self, CType::F32 | CType::F64)
    }
}

/// How a struct goes by value, by arm64's rules (AAPCS64; Apple's agree):
/// one to four floats or doubles of one type (a homogeneous floating-point
/// aggregate) in float registers; anything else of up to 16 bytes in one or
/// two integer registers, as words of its memory; larger, as a pointer to a
/// copy (and a larger result through the pointer in x8).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Agg {
    Hfa(CType, u8),
    Words(u8),
    Indirect,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct SAbi {
    size: u32,
    agg: Agg,
}

/// A parameter's or result's type as the stubs see it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Ty {
    C(CType),
    S(SAbi),
}

const APPLE_ARM64: bool = cfg!(all(target_arch = "aarch64", target_vendor = "apple"));

fn float_type(t: CType) -> cranelift_codegen::ir::Type {
    if t == CType::F32 { types::F32 } else { types::F64 }
}

fn classify(size: u32, leaves: &[(u32, CType)]) -> Result<Agg> {
    if !cfg!(target_arch = "aarch64") {
        return Err(Error::new("Structs by value aren't supported on this machine yet (only arm64): pass a pointer"));
    }
    let n = leaves.len() as u32;
    if (1..=4).contains(&n) {
        let t = leaves[0].1;
        let es = if t == CType::F32 { 4 } else { 8 };
        if t.is_float() && leaves.iter().enumerate().all(|(i, l)| l.1 == t && l.0 == i as u32 * es) && size == n * es {
            return Ok(Agg::Hfa(t, n as u8));
        }
    }
    Ok(if size <= 16 { Agg::Words(size.div_ceil(8).max(1) as u8) } else { Agg::Indirect })
}

/// `[:struct size [[offset type]…]]` (a struct by value), or None.
fn parse_struct(v: Value) -> Result<Option<SAbi>> {
    if !v.is(Kind::Vector) || v.vec_count() != 3 || !v.vec_nth(0).is_keyword() || v.vec_nth(0).sym_name() != "struct" {
        return Ok(None);
    }
    let size = int_arg(v.vec_nth(1), CType::U32)? as u32;
    let mut leaves = Vec::new();
    for l in norn_rt::seq::SeqIter::new(v.vec_nth(2)).map(|it| it.collect::<Vec<_>>()).unwrap_or_default() {
        if !l.is(Kind::Vector) || l.vec_count() != 2 {
            return Err(Error::new("A struct's leaves are [offset type] pairs"));
        }
        let t = type_arg(l.vec_nth(1), "struct")?;
        leaves.push((int_arg(l.vec_nth(0), CType::U32)? as u32, t));
    }
    Ok(Some(SAbi { size, agg: classify(size, &leaves)? }))
}

/// One parameter of the C signature: an argument word, a piece of a struct
/// (its argument, offset and type), a struct's address, or a filler that
/// uses up a register so what follows goes on the stack.
#[derive(Clone, Copy, Debug)]
enum Piece {
    Word(usize, CType),
    Part(usize, u32, cranelift_codegen::ir::Type),
    Ptr(usize),
    Filler(cranelift_codegen::ir::Type),
}

/// The C signature's parameters for params, by the register rules: an
/// argument that doesn't fit the registers left goes on the stack whole,
/// and so does everything after it of its kind (fillers take the rest of
/// the registers). On Apple arm64, every variadic argument goes on the
/// stack too.
fn lower(params: &[Ty], variadic_from: Option<usize>) -> Vec<Piece> {
    let (mut ngrn, mut nsrn) = (0u32, 0u32);
    let mut out = Vec::new();
    let fill_ints = |out: &mut Vec<Piece>, ngrn: &mut u32| {
        while *ngrn < 8 {
            out.push(Piece::Filler(types::I64));
            *ngrn += 1;
        }
    };
    let fill_floats = |out: &mut Vec<Piece>, nsrn: &mut u32| {
        while *nsrn < 8 {
            out.push(Piece::Filler(types::F64));
            *nsrn += 1;
        }
    };
    for (i, &t) in params.iter().enumerate() {
        if APPLE_ARM64 && variadic_from == Some(i) {
            fill_ints(&mut out, &mut ngrn);
            fill_floats(&mut out, &mut nsrn);
        }
        match t {
            Ty::C(c) if c.is_float() => {
                nsrn = (nsrn + 1).min(8);
                out.push(Piece::Word(i, c));
            }
            Ty::C(c) => {
                ngrn = (ngrn + 1).min(8);
                out.push(Piece::Word(i, c));
            }
            Ty::S(SAbi { agg: Agg::Hfa(c, n), size }) => {
                let n = n as u32;
                if nsrn + n <= 8 {
                    nsrn += n;
                    let es = if c == CType::F32 { 4 } else { 8 };
                    out.extend((0..n).map(|k| Piece::Part(i, k * es, float_type(c))));
                } else {
                    fill_floats(&mut out, &mut nsrn);
                    if APPLE_ARM64 || c == CType::F64 {
                        let es = if c == CType::F32 { 4 } else { 8 };
                        out.extend((0..n).map(|k| Piece::Part(i, k * es, float_type(c))));
                    } else {
                        // (AAPCS64 elsewhere: its memory image, in 8-byte slots)
                        out.extend((0..size.div_ceil(8)).map(|k| Piece::Part(i, k * 8, types::I64)));
                    }
                }
            }
            Ty::S(SAbi { agg: Agg::Words(w), .. }) => {
                let w = w as u32;
                if ngrn + w <= 8 {
                    ngrn += w;
                } else {
                    fill_ints(&mut out, &mut ngrn);
                }
                out.extend((0..w).map(|k| Piece::Part(i, k * 8, types::I64)));
            }
            Ty::S(SAbi { agg: Agg::Indirect, .. }) => {
                ngrn = (ngrn + 1).min(8);
                out.push(Piece::Ptr(i));
            }
        }
    }
    out
}

fn piece_abi(p: Piece) -> AbiParam {
    match p {
        Piece::Word(_, c) => c.abi().expect("a parameter isn't void"),
        Piece::Part(_, _, t) | Piece::Filler(t) => AbiParam::new(t),
        Piece::Ptr(_) => AbiParam::new(types::I64),
    }
}

/// The C signature: parameters (a struct-result pointer first, when it
/// goes by reference) and results.
fn c_signature(cc: cranelift_codegen::isa::CallConv, pieces: &[Piece], ret: Ty) -> Signature {
    let mut sig = Signature::new(cc);
    if let Ty::S(SAbi { agg: Agg::Indirect, .. }) = ret {
        sig.params.push(AbiParam::special(types::I64, ArgumentPurpose::StructReturn));
    }
    sig.params.extend(pieces.iter().map(|&p| piece_abi(p)));
    match ret {
        Ty::C(c) => sig.returns.extend(c.abi()),
        Ty::S(SAbi { agg: Agg::Hfa(c, n), .. }) => sig.returns.extend((0..n).map(|_| AbiParam::new(float_type(c)))),
        Ty::S(SAbi { agg: Agg::Words(w), .. }) => sig.returns.extend((0..w).map(|_| AbiParam::new(types::I64))),
        Ty::S(SAbi { agg: Agg::Indirect, .. }) => {}
    }
    sig
}

/// How a Norn argument becomes a C one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Param {
    Int(CType),
    Float(CType),
    Pointer,
    Bytes,
    Str,
    Out,
    /// The address of a struct's copy, passed by value.
    Struct,
}

pub(crate) struct Foreign {
    addr: usize,
    /// The stub (none for a variadic function: each call's types have one).
    stub: usize,
    params: Vec<Param>,
    tys: Vec<Ty>,
    ret: Ty,
    /// The result is a string (copied from the char* C returns).
    ret_string: bool,
    /// The fixed parameters' count, for a variadic function.
    variadic: Option<usize>,
    mode: CallMode,
    /// Keeps the library loaded while the function exists (none for a
    /// function known only by its address).
    _lib: Option<Arc<libloading::Library>>,
}

type Stub = unsafe extern "C" fn(u64, *const u64, *mut u64);

/// (parameter types, result type, variadic from) → stub address.
type StubCache = HashMap<(Vec<Ty>, Ty, Option<usize>), usize>;

static STUBS: Mutex<Option<StubCache>> = Mutex::new(None);

fn compile(func: Function, what: &str) -> Result<usize> {
    let isa = crate::jit::isa().ok_or_else(|| Error::new("No native code generator for this machine"))?;
    let mut ctx = cranelift_codegen::Context::for_function(func);
    let code = ctx.compile(&**isa, &mut ControlPlane::default()).map_err(|e| Error::new(format!("Can't compile {what}: {e:?}")))?;
    crate::jit::install_code(code.code_buffer()).ok_or_else(|| Error::new("Out of executable memory"))
}

/// The call stub for a signature (compiled once).
fn stub_for(params: &[Ty], ret: Ty, variadic_from: Option<usize>) -> Result<usize> {
    let key = (params.to_vec(), ret, variadic_from);
    if let Some(&s) = STUBS.lock().unwrap().get_or_insert_with(HashMap::new).get(&key) {
        return Ok(s);
    }
    let isa = crate::jit::isa().ok_or_else(|| Error::new("No native code generator for this machine"))?;
    let cc = isa.default_call_conv();
    let pieces = lower(params, variadic_from);
    let target = c_signature(cc, &pieces, ret);
    let mut sig = Signature::new(cc);
    sig.params.extend([AbiParam::new(types::I64), AbiParam::new(types::I64), AbiParam::new(types::I64)]);
    let mut func = Function::with_name_signature(UserFuncName::user(0, 1), sig);
    let mut fbc = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut func, &mut fbc);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        b.seal_block(entry);
        let (fnptr, args, retp) = {
            let p = b.block_params(entry);
            (p[0], p[1], p[2])
        };
        let flags = MemFlagsData::trusted();
        let mut vals = Vec::new();
        if let Ty::S(SAbi { agg: Agg::Indirect, .. }) = ret {
            vals.push(retp);
        }
        for &p in &pieces {
            let v = match p {
                Piece::Word(i, t) => {
                    let off = (i * 8) as i32;
                    match t {
                        CType::F32 => b.ins().load(types::F32, flags, args, off),
                        CType::F64 => b.ins().load(types::F64, flags, args, off),
                        CType::I8 | CType::U8 => {
                            let w = b.ins().load(types::I64, flags, args, off);
                            b.ins().ireduce(types::I8, w)
                        }
                        CType::I16 | CType::U16 => {
                            let w = b.ins().load(types::I64, flags, args, off);
                            b.ins().ireduce(types::I16, w)
                        }
                        CType::I32 | CType::U32 => {
                            let w = b.ins().load(types::I64, flags, args, off);
                            b.ins().ireduce(types::I32, w)
                        }
                        _ => b.ins().load(types::I64, flags, args, off),
                    }
                }
                Piece::Part(i, off, t) => {
                    let base = b.ins().load(types::I64, flags, args, (i * 8) as i32);
                    b.ins().load(t, flags, base, off as i32)
                }
                Piece::Ptr(i) => b.ins().load(types::I64, flags, args, (i * 8) as i32),
                Piece::Filler(t) if t == types::F64 => b.ins().f64const(0.0),
                Piece::Filler(_) => b.ins().iconst(types::I64, 0),
            };
            vals.push(v);
        }
        let sref = b.import_signature(target);
        let call = b.ins().call_indirect(sref, fnptr, &vals);
        let results: Vec<_> = b.inst_results(call).to_vec();
        match ret {
            Ty::C(CType::Void) => {}
            Ty::C(c) => {
                let r = results[0];
                let stored = match c {
                    CType::F32 | CType::F64 | CType::I64 | CType::U64 | CType::Ptr => r,
                    CType::I8 | CType::I16 | CType::I32 => b.ins().sextend(types::I64, r),
                    _ => b.ins().uextend(types::I64, r),
                };
                b.ins().store(flags, stored, retp, 0);
            }
            Ty::S(SAbi { agg: Agg::Hfa(c, _), .. }) => {
                let es = if c == CType::F32 { 4 } else { 8 };
                for (k, &r) in results.iter().enumerate() {
                    b.ins().store(flags, r, retp, (k * es) as i32);
                }
            }
            Ty::S(SAbi { agg: Agg::Words(_), .. }) => {
                for (k, &r) in results.iter().enumerate() {
                    b.ins().store(flags, r, retp, (k * 8) as i32);
                }
            }
            Ty::S(SAbi { agg: Agg::Indirect, .. }) => {}
        }
        b.ins().return_(&[]);
        b.finalize(isa.frontend_config());
    }
    let at = compile(func, "an FFI stub")?;
    STUBS.lock().unwrap().get_or_insert_with(HashMap::new).insert(key, at);
    Ok(at)
}

fn kw(m: &mut Mutator, k: &str) -> Value {
    rt().keyword(m, None, k)
}

/// `(%ffi-open path)`: a `:library` handle.
pub(crate) fn open(vm: &mut Vm, path: &str) -> Result<Value> {
    // SAFETY: loading a library runs its initialisers; the caller chose it.
    let lib = unsafe { libloading::Library::new(path) }.map_err(|e| Error::new(format!("Can't load {path}: {e}")))?;
    let p = str_new(&mut vm.m, path);
    let k = kw(&mut vm.m, "path");
    let info = map_assoc(&mut vm.m, map_empty(), k, p);
    reactor::register(&mut vm.m, Io::Library(Arc::new(lib)), "library", info).map_err(Error::new)
}

fn keyword_name(v: Value) -> Result<String> {
    if v.is_keyword() { Ok(v.sym_name().to_owned()) } else { Err(Error::new(format!("Expected a type keyword, got {}", norn_rt::print::pr_str(v, true)))) }
}

/// Where a foreign function's calls run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CallMode {
    /// On the calling carrier, in a native region.
    Carrier,
    /// On the blocking pool.
    Blocking,
    /// On the thread serving the main-thread lane (`main_lane.rs`).
    Main,
}

/// `(%ffi-fn library name [param-types] ret-type mode)`: a `:foreign`
/// handle, its calls run as mode says: false on the carrier, :main on the
/// main-thread lane, otherwise (true) on the blocking pool. With nil for
/// the library, name is the function's address.
pub(crate) fn function(vm: &mut Vm, lib: Value, name: Value, params: Value, ret: Value, mode: CallMode) -> Result<Value> {
    let lib_arc = if lib.is_nil() {
        None
    } else {
        let e = reactor::entry_of(lib, "library")?;
        match e.io.lock().unwrap().as_ref() {
            Some(Io::Library(l)) => Some(l.clone()),
            _ => return Err(Error::new("The library is closed")),
        }
    };
    let mut ps = Vec::new();
    let mut tys = Vec::new();
    let mut variadic = None;
    for p in norn_rt::seq::SeqIter::new(params).map(|it| it.collect::<Vec<_>>()).unwrap_or_default() {
        if variadic.is_some() {
            return Err(Error::new(":... comes last, after the fixed parameters"));
        }
        if let Some(s) = parse_struct(p)? {
            ps.push(Param::Struct);
            tys.push(Ty::S(s));
            continue;
        }
        let k = keyword_name(p)?;
        if k == "..." {
            if !cfg!(target_arch = "aarch64") {
                return Err(Error::new("Variadic functions aren't supported on this machine yet (only arm64)"));
            }
            variadic = Some(ps.len());
            continue;
        }
        let ct = CType::parse(&k).filter(|&t| t != CType::Void).ok_or_else(|| Error::new(format!("Unknown parameter type :{k}")))?;
        ps.push(match k.as_str() {
            "pointer" => Param::Pointer,
            "bytes" => Param::Bytes,
            "string" => Param::Str,
            "out" => Param::Out,
            _ if ct.is_float() => Param::Float(ct),
            _ => Param::Int(ct),
        });
        tys.push(Ty::C(ct));
    }
    let (ret_ty, ret_string) = match parse_struct(ret)? {
        Some(s) => (Ty::S(s), false),
        None => {
            let rk = keyword_name(ret)?;
            let t = CType::parse(&rk).filter(|_| !matches!(rk.as_str(), "bytes" | "out")).ok_or_else(|| Error::new(format!("Unknown return type :{rk}")))?;
            (Ty::C(t), rk == "string")
        }
    };
    let stub = if variadic.is_some() { 0 } else { stub_for(&tys, ret_ty, None)? };
    let addr = match &lib_arc {
        None => address(name, "function-at")?,
        Some(l) => {
            let name = name.as_str();
            let cname = std::ffi::CString::new(name).map_err(|_| Error::new("A symbol name can't contain NUL"))?;
            // SAFETY: we only take the symbol's address.
            unsafe { l.get::<*const ()>(cname.as_bytes_with_nul()) }.map(|s| *s as usize).map_err(|e| Error::new(format!("No symbol {name}: {e}")))?
        }
    };
    let f = Foreign { addr, stub, params: ps, tys, ret: ret_ty, ret_string, variadic, mode, _lib: lib_arc };
    let entries = [
        ("library", lib),
        ("name", name),
        ("params", params),
        ("ret", ret),
        ("blocking", Value::bool(mode == CallMode::Blocking)),
        ("main-thread", Value::bool(mode == CallMode::Main)),
    ];
    let mut info = map_empty();
    for (k, v) in entries {
        let k = kw(&mut vm.m, k);
        info = map_assoc(&mut vm.m, info, k, v);
    }
    reactor::register(&mut vm.m, Io::Foreign(Box::new(f)), "foreign", info).map_err(Error::new)
}

fn int_arg(v: Value, t: CType) -> Result<u64> {
    // A 64-bit unsigned value above the fixnum range arrives as a bignum.
    if v.is(Kind::BigInt) && t == CType::U64 {
        use num_traits::ToPrimitive;
        return norn_rt::numbers::int_of(v).and_then(|b| b.to_u64()).ok_or_else(|| Error::new("The integer doesn't fit a C u64"));
    }
    if !v.is_fixnum() {
        return Err(Error::new(format!("Expected an integer argument, got {}", v.type_name())));
    }
    let n = v.as_fixnum();
    let ok = match t {
        CType::I8 => (-128..=127).contains(&n),
        CType::U8 => (0..=255).contains(&n),
        CType::I16 => (-32768..=32767).contains(&n),
        CType::U16 => (0..=65535).contains(&n),
        CType::I32 => (i32::MIN as i64..=i32::MAX as i64).contains(&n),
        CType::U32 => (0..=u32::MAX as i64).contains(&n),
        CType::U64 => n >= 0,
        _ => true,
    };
    if ok { Ok(n as u64) } else { Err(Error::new(format!("{n} doesn't fit a C {t:?}"))) }
}

/// What a call needs after it returns.
struct Prepared {
    raw: Vec<u64>,
    keep: Vec<Value>,
    _strings: Vec<std::ffi::CString>,
    outs: Vec<Vec<u8>>,
    /// The stub (a variadic call's, for its arguments' types).
    stub: usize,
}

// SAFETY: the Values in `keep` are rooted for the call; the rest is owned data.
unsafe impl Send for Prepared {}

fn prepare(f: &Foreign, args: &[Value]) -> Result<Prepared> {
    let fixed = f.params.len();
    let ok = match f.variadic {
        Some(_) => args.len() >= fixed,
        None => args.len() == fixed,
    };
    if !ok {
        let more = if f.variadic.is_some() { " or more" } else { "" };
        return Err(Error::new(format!("The foreign function takes {fixed}{more} arguments, got {}", args.len())));
    }
    let mut p = Prepared { raw: Vec::with_capacity(args.len()), keep: Vec::new(), _strings: Vec::new(), outs: Vec::new(), stub: f.stub };
    let mut tys = f.tys.clone();
    for (i, &a) in args.iter().enumerate() {
        // (a variadic argument: C's default promotions)
        let param = match f.params.get(i) {
            Some(&param) => param,
            None => {
                let (param, t) = if a.is_fixnum() || a.is(Kind::BigInt) {
                    (Param::Int(if a.is(Kind::BigInt) { CType::U64 } else { CType::I64 }), CType::I64)
                } else if a.is(Kind::Float) {
                    (Param::Float(CType::F64), CType::F64)
                } else if a.is(Kind::String) {
                    (Param::Str, CType::Ptr)
                } else if a.is(Kind::Bytes) {
                    (Param::Bytes, CType::Ptr)
                } else if a.is_nil() {
                    (Param::Pointer, CType::Ptr)
                } else {
                    return Err(Error::new(format!("A variadic argument is a number, string, bytes or nil, not {}", a.type_name())));
                };
                tys.push(Ty::C(t));
                param
            }
        };
        let w = match param {
            Param::Int(t) => int_arg(a, t)?,
            Param::Float(t) => {
                let x = if a.is_fixnum() { a.as_fixnum() as f64 } else if a.is(Kind::Float) { a.as_f64() } else { return Err(Error::new("Expected a number argument")) };
                if t == CType::F32 { (x as f32).to_bits() as u64 } else { x.to_bits() }
            }
            Param::Pointer => if a.is_nil() { 0 } else { int_arg(a, CType::U64)? },
            Param::Struct => address(a, "a struct argument")? as u64,
            Param::Bytes => {
                if !a.is(Kind::Bytes) {
                    return Err(Error::new(format!("Expected bytes, got {}", a.type_name())));
                }
                p.keep.push(a);
                a.as_bytes().as_ptr() as u64
            }
            Param::Str => {
                let s = if a.is(Kind::String) { a.as_str().to_owned() } else { norn_rt::print::pr_str(a, false) };
                let c = std::ffi::CString::new(s).map_err(|_| Error::new("A C string can't contain NUL"))?;
                let ptr = c.as_ptr() as u64;
                p._strings.push(c);
                ptr
            }
            Param::Out => {
                let n = int_arg(a, CType::U32)? as usize;
                let mut buf = vec![0u8; n.max(1)];
                buf.truncate(n.max(1));
                let ptr = buf.as_mut_ptr() as u64;
                p.outs.push(buf);
                ptr
            }
        };
        p.raw.push(w);
    }
    if f.variadic.is_some() {
        p.stub = stub_for(&tys, f.ret, f.variadic)?;
    }
    Ok(p)
}

fn result(m: &mut Mutator, ret: (CType, bool), r: u64, outs: Vec<Vec<u8>>) -> std::result::Result<Value, String> {
    let v = match ret {
        (CType::Void, _) => Value::NIL,
        (CType::Ptr, true) => {
            if r == 0 {
                Value::NIL
            } else {
                // SAFETY: the function promised a NUL-terminated string.
                let s = unsafe { std::ffi::CStr::from_ptr(r as *const libc::c_char) };
                str_new(m, &s.to_string_lossy())
            }
        }
        (CType::F64, _) => float_new(m, f64::from_bits(r)),
        (CType::F32, _) => float_new(m, f32::from_bits(r as u32) as f64),
        (CType::U64, _) => norn_rt::numbers::int_value(m, &num_bigint::BigInt::from(r)),
        (CType::I64, _) => norn_rt::numbers::int_value(m, &num_bigint::BigInt::from(r as i64)),
        _ => Value::fix(r as i64),
    };
    Ok(with_outs(m, v, outs))
}

/// The result v, or with Bytes outs `[v out…]`.
fn with_outs(m: &mut Mutator, v: Value, outs: Vec<Vec<u8>>) -> Value {
    if outs.is_empty() {
        return v;
    }
    let mut vs = vec![v];
    for o in outs {
        vs.push(bytes_new(m, &o));
    }
    vec_from_slice(m, &vs)
}

/// `(%ffi-call f args…)`: the result; with `:out` parameters,
/// `[result out-bytes…]`. A function returning a struct takes first the
/// address to put it at (and gives nil).
pub(crate) fn call(vm: &mut Vm, h: Value, args: &[Value]) -> Result<Value> {
    let e = reactor::entry_of(h, "foreign")?;
    let (addr, ret, mode, ret_at, mut prep) = {
        let io = e.io.lock().unwrap();
        let Some(Io::Foreign(f)) = io.as_ref() else { return Err(Error::new("The foreign function is closed")) };
        let (ret_at, args) = match f.ret {
            Ty::S(_) => {
                let Some(&first) = args.first() else { return Err(Error::new("A function returning a struct takes the address for it first")) };
                (Some(address(first, "a struct result")?), &args[1..])
            }
            Ty::C(_) => (None, args),
        };
        let ret = match f.ret {
            Ty::C(c) => (c, f.ret_string),
            Ty::S(_) => (CType::Void, false),
        };
        (f.addr, ret, f.mode, ret_at, prepare(f, args)?)
    };
    // SAFETY: the stub was compiled for this signature.
    let stub: Stub = unsafe { std::mem::transmute(prep.stub) };
    if mode == CallMode::Carrier {
        let mut r: u64 = 0;
        let rp: *mut u64 = match ret_at {
            Some(a) => a as *mut u64,
            None => &mut r,
        };
        let keep = std::mem::take(&mut prep.keep);
        // (a callback C makes meanwhile, on this thread, runs on this Vm)
        let prev = CALLING_VM.with(|c| c.replace(vm as *mut Vm));
        // SAFETY: the arguments match the stub's signature; what they point
        // at (prep's buffers, rooted Bytes, the struct memory) outlives the call.
        vm.native_region(&keep, || unsafe { stub(addr as u64, prep.raw.as_ptr(), rp) });
        CALLING_VM.with(|c| c.set(prev));
        if let Some((e, root)) = CALLBACK_ERROR.with(|c| c.borrow_mut().take()) {
            if let Some(k) = root {
                crate::sched::unroot(k);
            }
            return Err(e);
        }
        return result(&mut vm.m, ret, r, std::mem::take(&mut prep.outs)).map_err(Error::new);
    }
    let key = crate::sched::root_many(&prep.keep);
    let e2 = e.clone();
    let main = mode == CallMode::Main;
    let job = move || {
        let _hold = e2; // the function (and its library) stays loaded
        let mut r: u64 = 0;
        let rp: *mut u64 = match ret_at {
            Some(a) => a as *mut u64,
            None => &mut r,
        };
        // SAFETY: as above; the Bytes are rooted until the result is built.
        unsafe { stub(addr as u64, prep.raw.as_ptr(), rp) };
        // On the main thread, a string result is copied before the next
        // call runs: C libraries often return text in a static buffer.
        // SAFETY: the function promised a NUL-terminated string.
        let text = (main && ret.1 && r != 0)
            .then(|| unsafe { std::ffi::CStr::from_ptr(r as *const libc::c_char) }.to_string_lossy().into_owned());
        let outs = std::mem::take(&mut prep.outs);
        Box::new(move |m: &mut Mutator| {
            drop(prep);
            crate::sched::unroot_many(key);
            match text {
                Some(s) => {
                    let v = str_new(m, &s);
                    Ok(with_outs(m, v, outs))
                }
                None => result(m, ret, r, outs).map_err(crate::vm::Failure::from),
            }
        }) as crate::blocking::Finish
    };
    if main { crate::main_lane::run(vm, job) } else { crate::blocking::run(vm, job) }
}

// ---- memory (M9 part 9.8) ----
//
// Addresses are integers. These read and write C memory as C types: as
// unsafe as C (a wrong address can crash the process), so `norn.ffi`
// gives them only to modules that say `(:unsafe)`.

fn address(v: Value, op: &str) -> Result<usize> {
    if v.is_nil() {
        return Err(Error::new(format!("{op}: a NULL pointer")));
    }
    let a = int_arg(v, CType::U64).map_err(|_| Error::new(format!("{op} expects an address (an integer), got {}", v.type_name())))?;
    if a == 0 {
        return Err(Error::new(format!("{op}: a NULL pointer")));
    }
    Ok(a as usize)
}

fn type_arg(v: Value, op: &str) -> Result<CType> {
    let k = keyword_name(v)?;
    CType::parse(&k).filter(|t| *t != CType::Void && !matches!(k.as_str(), "bytes" | "string" | "out")).ok_or_else(|| Error::new(format!("{op}: not a C type: :{k}")))
}

fn offset_arg(v: Value) -> Result<usize> {
    if v.is_nil() {
        return Ok(0);
    }
    Ok(int_arg(v, CType::U32)? as usize)
}

/// `(%mem-alloc n)`: n zeroed bytes from C's allocator: their address.
pub(crate) fn mem_alloc(a: &[Value]) -> Result<Value> {
    let n = int_arg(a[0], CType::U64)? as usize;
    // SAFETY: calloc of at least one byte.
    let p = unsafe { libc::calloc(n.max(1), 1) };
    if p.is_null() {
        return Err(Error::new(format!("Out of memory allocating {n} bytes")));
    }
    Ok(Value::fix(p as i64))
}

/// `(%mem-free addr)`.
pub(crate) fn mem_free(a: &[Value]) -> Result<Value> {
    if !a[0].is_nil() {
        let p = address(a[0], "free!")?;
        // SAFETY: the caller promises p came from %mem-alloc and is freed once.
        unsafe { libc::free(p as *mut libc::c_void) };
    }
    Ok(Value::NIL)
}

/// `(%mem-get addr type offset)`: the C value at addr + offset.
pub(crate) fn mem_get(m: &mut Mutator, a: &[Value]) -> Result<Value> {
    let t = type_arg(a[1], "mem-get")?;
    let p = address(a[0], "mem-get")? + offset_arg(a[2])?;
    // SAFETY: the caller promises p points at a readable value of type t.
    let raw = unsafe {
        match t {
            CType::I8 => *(p as *const i8) as i64 as u64,
            CType::U8 => *(p as *const u8) as u64,
            CType::I16 => (p as *const i16).read_unaligned() as i64 as u64,
            CType::U16 => (p as *const u16).read_unaligned() as u64,
            CType::I32 => (p as *const i32).read_unaligned() as i64 as u64,
            CType::U32 => (p as *const u32).read_unaligned() as u64,
            CType::F32 => (p as *const u32).read_unaligned() as u64,
            _ => (p as *const u64).read_unaligned(),
        }
    };
    from_c(m, t, raw).map_err(Error::new)
}

/// `(%mem-set! addr type offset value)`.
pub(crate) fn mem_set(a: &[Value]) -> Result<Value> {
    let t = type_arg(a[1], "mem-set!")?;
    let p = address(a[0], "mem-set!")? + offset_arg(a[2])?;
    let w = to_c(a[3], t)?;
    // SAFETY: the caller promises p points at writable memory for type t.
    unsafe {
        match t {
            CType::I8 | CType::U8 => *(p as *mut u8) = w as u8,
            CType::I16 | CType::U16 => (p as *mut u16).write_unaligned(w as u16),
            CType::I32 | CType::U32 | CType::F32 => (p as *mut u32).write_unaligned(w as u32),
            _ => (p as *mut u64).write_unaligned(w),
        }
    }
    Ok(a[3])
}

/// `(%mem-string addr)`: the NUL-terminated string at addr (nil for NULL).
pub(crate) fn mem_string(m: &mut Mutator, a: &[Value]) -> Result<Value> {
    if a[0].is_nil() || a[0] == Value::fix(0) {
        return Ok(Value::NIL);
    }
    let p = address(a[0], "string-at")?;
    // SAFETY: the caller promises a NUL-terminated string.
    let s = unsafe { std::ffi::CStr::from_ptr(p as *const libc::c_char) };
    Ok(str_new(m, &s.to_string_lossy()))
}

/// `(%mem-bytes addr n)`: a copy of n bytes at addr.
pub(crate) fn mem_bytes(m: &mut Mutator, a: &[Value]) -> Result<Value> {
    let n = int_arg(a[1], CType::U64)? as usize;
    if n == 0 {
        return Ok(bytes_new(m, &[]));
    }
    let p = address(a[0], "bytes-at")?;
    // SAFETY: the caller promises n readable bytes.
    let s = unsafe { std::slice::from_raw_parts(p as *const u8, n) };
    Ok(bytes_new(m, s))
}

/// `(%mem-write-bytes! addr bytes-or-string)`: copy them to addr (a string
/// as UTF-8, without a NUL). The count copied.
pub(crate) fn mem_write_bytes(a: &[Value]) -> Result<Value> {
    let data: &[u8] = if a[1].is(Kind::Bytes) {
        a[1].as_bytes()
    } else if a[1].is(Kind::String) {
        a[1].as_str().as_bytes()
    } else {
        return Err(Error::new(format!("write-bytes! expects bytes or a string, got {}", a[1].type_name())));
    };
    if !data.is_empty() {
        let p = address(a[0], "write-bytes!")?;
        // SAFETY: the caller promises room for them.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), p as *mut u8, data.len()) };
    }
    Ok(Value::fix(data.len() as i64))
}

/// A Norn value as a C one of type t (a word).
fn to_c(v: Value, t: CType) -> Result<u64> {
    Ok(match t {
        CType::Void => 0,
        CType::F32 | CType::F64 => {
            let x = if v.is_fixnum() {
                v.as_fixnum() as f64
            } else if v.is(Kind::Float) {
                v.as_f64()
            } else {
                return Err(Error::new(format!("Expected a number, got {}", v.type_name())));
            };
            if t == CType::F32 { (x as f32).to_bits() as u64 } else { x.to_bits() }
        }
        CType::Ptr => {
            if v.is_nil() {
                0
            } else {
                int_arg(v, CType::U64)?
            }
        }
        _ => int_arg(v, t)?,
    })
}

/// A C value of type t (a word, sign- or zero-extended) as a Norn one.
fn from_c(m: &mut Mutator, t: CType, r: u64) -> std::result::Result<Value, String> {
    result(m, (t, false), r, Vec::new())
}

// ---- callbacks (M9 part 9.8) ----
//
// `(%ffi-callback [param-types] ret-type f chan)` compiles a trampoline: a C
// function of that signature that stores its arguments in an array of words
// and calls `dispatch` with the callback's id. Then:
//
// - On the carrier making a fast FFI call (the usual case: qsort's
//   comparator), the call is a nested run of f on that thread: the carrier
//   leaves its native region for it, and enters it again after.
// - On any other thread (the blocking pool, or a thread of C's own), the
//   arguments are posted to the callback's channel with a reply token; a
//   fiber serving the channel (norn.ffi starts one) calls f and answers with
//   `%ffi-callback-reply!`, while the C thread waits.
//
// A struct argument by value reaches f as the address of its copy (on the
// trampoline's stack). For a struct result, f gets first the address to
// write it at (norn.ffi's callback converts both ways).
//
// An error in f can't unwind through C: the callback returns 0, and in the
// first case the error is signalled when the FFI call returns.

struct Callback {
    /// Its trampoline's address, for reuse once it's freed.
    code: usize,
    params: Vec<Ty>,
    ret: Ty,
    f: Value,
    port: crate::ext::Port,
    root: u64,
}

// SAFETY: f is rooted for as long as the callback lives; it's only used on
// a carrier, by a running participant.
unsafe impl Send for Callback {}
unsafe impl Sync for Callback {}

static CALLBACKS: Mutex<Option<HashMap<u64, Arc<Callback>>>> = Mutex::new(None);

/// Trampolines of freed callbacks, by signature: (id, address), reused
/// for the next callback of that signature (executable memory isn't freed).
type FreeTrampolines = HashMap<(Vec<Ty>, Ty), Vec<(u64, usize)>>;
static FREE_TRAMPOLINES: Mutex<Option<FreeTrampolines>> = Mutex::new(None);
static NEXT_CALLBACK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

type Reply = Arc<(Mutex<Option<u64>>, std::sync::Condvar)>;
static REPLIES: Mutex<Option<HashMap<u64, Reply>>> = Mutex::new(None);

thread_local! {
    /// The Vm making a fast FFI call on this thread (null otherwise).
    static CALLING_VM: std::cell::Cell<*mut Vm> = const { std::cell::Cell::new(std::ptr::null_mut()) };
    /// An error a callback raised, for the FFI call to signal once C returns.
    /// (with the root that keeps its condition alive meanwhile)
    static CALLBACK_ERROR: std::cell::RefCell<Option<(Error, Option<u64>)>> = const { std::cell::RefCell::new(None) };
}

fn callback(id: u64) -> Option<Arc<Callback>> {
    CALLBACKS.lock().unwrap().as_ref().and_then(|m| m.get(&id).cloned())
}

/// Called by every trampoline.
extern "C" fn dispatch(id: u64, args: *const u64, ret: *mut u64) {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_callback(id, args)));
    // SAFETY: the trampoline passes a word for the result.
    unsafe { *ret = r.unwrap_or(0) };
}

/// The words f is called with, as their C types: first the result's
/// address for a struct result, then a struct argument's address.
fn word_types(cb: &Callback) -> Vec<CType> {
    let mut ts = Vec::new();
    if let Ty::S(_) = cb.ret {
        ts.push(CType::Ptr);
    }
    ts.extend(cb.params.iter().map(|t| match t {
        Ty::C(c) => *c,
        Ty::S(_) => CType::Ptr,
    }));
    ts
}

fn ret_word(cb: &Callback, v: Value) -> Result<u64> {
    match cb.ret {
        Ty::C(c) => to_c(v, c),
        Ty::S(_) => Ok(0),
    }
}

fn run_callback(id: u64, args: *const u64) -> u64 {
    let Some(cb) = callback(id) else {
        eprintln!("norn: a C callback was called after it was freed");
        return 0;
    };
    let wts = word_types(&cb);
    // SAFETY: the trampoline stores one word per parameter (and the result's address).
    let raw: Vec<u64> = (0..wts.len()).map(|i| unsafe { *args.add(i) }).collect();
    let vm = CALLING_VM.with(|c| c.get());
    if !vm.is_null() {
        // SAFETY: set by `call` for the duration of a fast call on this
        // thread, which is inside native_region: nothing else uses the Vm.
        let vm = unsafe { &mut *vm };
        crate::sched::leave_native(vm.carrier);
        let r = (|| -> Result<u64> {
            let mut argv = Vec::with_capacity(raw.len());
            for (&w, &t) in raw.iter().zip(&wts) {
                argv.push(from_c(&mut vm.m, t, w).map_err(Error::new)?);
            }
            let v = vm.apply(cb.f, &argv)?;
            ret_word(&cb, v)
        })();
        // (the nested run may have cached a segment: not a root, see native_region)
        vm.drop_spare();
        let r = r.map_err(|e| {
            // (a condition the error carries stays alive until it's signalled)
            let root = match &e.class {
                Some(crate::vm::ErrClass::Value(v)) => Some(crate::sched::root(*v)),
                _ => None,
            };
            (e, root)
        });
        crate::sched::enter_native(vm.carrier);
        return match r {
            Ok(w) => w,
            Err((e, root)) => {
                CALLBACK_ERROR.with(|c| {
                    let mut c = c.borrow_mut();
                    if c.is_none() {
                        *c = Some((e, root));
                    } else if let Some(k) = root {
                        crate::sched::unroot(k);
                    }
                });
                0
            }
        };
    }
    // Another thread: through the channel, waiting for the answer.
    let token = NEXT_CALLBACK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let reply: Reply = Arc::new((Mutex::new(None), std::sync::Condvar::new()));
    REPLIES.lock().unwrap().get_or_insert_with(HashMap::new).insert(token, reply.clone());
    let posted = cb.port.post(|m| {
        let vs: Vec<Value> = raw.iter().zip(&wts).map(|(&w, &t)| from_c(m, t, w).unwrap_or(Value::NIL)).collect();
        let args = vec_from_slice(m, &vs);
        vec_from_slice(m, &[args, Value::fix(token as i64)])
    });
    if !posted {
        REPLIES.lock().unwrap().as_mut().map(|m| m.remove(&token));
        eprintln!("norn: a C callback from another thread was dropped (its channel is full or closed)");
        return 0;
    }
    let (lock, cv) = &*reply;
    if crate::main_lane::on_lane_thread() {
        // Keep running main-thread jobs meanwhile: the callback may make
        // main-thread calls itself.
        crate::main_lane::run_until(&|| lock.lock().unwrap().is_some());
        return lock.lock().unwrap().unwrap_or(0);
    }
    let mut got = lock.lock().unwrap();
    while got.is_none() {
        got = cv.wait(got).unwrap();
    }
    got.unwrap_or(0)
}

/// The trampoline for callback `id`: a C function of this signature.
fn trampoline(id: u64, params: &[Ty], ret: Ty) -> Result<usize> {
    use cranelift_codegen::ir::{StackSlotData, StackSlotKind};
    let isa = crate::jit::isa().ok_or_else(|| Error::new("No native code generator for this machine"))?;
    let cc = isa.default_call_conv();
    let pieces = lower(params, None);
    let sig = c_signature(cc, &pieces, ret);
    let mut target = Signature::new(cc);
    target.params.extend([AbiParam::new(types::I64), AbiParam::new(types::I64), AbiParam::new(types::I64)]);
    let shift = matches!(ret, Ty::S(_)) as usize; // (the result's address comes first)
    let nwords = params.len() + shift;
    // The stack: the words (and one for a scalar result), then room for
    // each struct argument and the struct result, 16 bytes at least each.
    let round16 = |n: u32| n.div_ceil(16).max(1) * 16;
    let mut at = ((nwords + 1) * 8) as u32;
    let mut areas = vec![None; params.len()];
    for (i, t) in params.iter().enumerate() {
        if let Ty::S(s) = t
            && s.agg != Agg::Indirect
        {
            areas[i] = Some(at);
            at += round16(s.size);
        }
    }
    let ret_area = match ret {
        Ty::S(s) if s.agg != Agg::Indirect => {
            let a = at;
            at += round16(s.size);
            Some(a)
        }
        _ => None,
    };
    let mut func = Function::with_name_signature(UserFuncName::user(0, 2), sig);
    let mut fbc = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut func, &mut fbc);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        b.seal_block(entry);
        let slot = b.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, at, 4));
        let mut ps: Vec<_> = b.block_params(entry).to_vec();
        let sret = if let Ty::S(SAbi { agg: Agg::Indirect, .. }) = ret { Some(ps.remove(0)) } else { None };
        for (&p, &v) in pieces.iter().zip(&ps) {
            match p {
                Piece::Word(i, t) => {
                    let off = ((shift + i) * 8) as i32;
                    let w = match t {
                        CType::F32 | CType::F64 => v,
                        CType::I8 | CType::I16 | CType::I32 => b.ins().sextend(types::I64, v),
                        CType::U8 | CType::U16 | CType::U32 => b.ins().uextend(types::I64, v),
                        _ => v,
                    };
                    if t == CType::F32 {
                        // (the low 32 bits of the word)
                        let z = b.ins().iconst(types::I64, 0);
                        b.ins().stack_store(types::I64, z, slot, off);
                    }
                    b.ins().stack_store(types::I64, w, slot, off);
                }
                Piece::Part(i, off, _) => {
                    let area = areas[i].expect("a struct in pieces has an area");
                    b.ins().stack_store(types::I64, v, slot, (area + off) as i32);
                }
                Piece::Ptr(i) => {
                    b.ins().stack_store(types::I64, v, slot, ((shift + i) * 8) as i32);
                }
                Piece::Filler(_) => {}
            }
        }
        for (i, a) in areas.iter().enumerate() {
            if let Some(a) = a {
                let addr = b.ins().stack_addr(types::I64, slot, *a as i32);
                b.ins().stack_store(types::I64, addr, slot, ((shift + i) * 8) as i32);
            }
        }
        if shift == 1 {
            let r = match (sret, ret_area) {
                (Some(p), _) => p,
                (None, Some(a)) => b.ins().stack_addr(types::I64, slot, a as i32),
                _ => unreachable!("a struct result has a place"),
            };
            b.ins().stack_store(types::I64, r, slot, 0);
        }
        let ret_off = (nwords * 8) as i32;
        let args_p = b.ins().stack_addr(types::I64, slot, 0);
        let ret_p = b.ins().stack_addr(types::I64, slot, ret_off);
        let idv = b.ins().iconst(types::I64, id as i64);
        let f = b.ins().iconst(types::I64, dispatch as *const () as usize as i64);
        let sref = b.import_signature(target);
        b.ins().call_indirect(sref, f, &[idv, args_p, ret_p]);
        let mut out = Vec::new();
        match ret {
            Ty::C(CType::Void) => {}
            Ty::C(CType::F32) => out.push(b.ins().stack_load(types::I64, types::F32, slot, ret_off)),
            Ty::C(CType::F64) => out.push(b.ins().stack_load(types::I64, types::F64, slot, ret_off)),
            Ty::C(c) => {
                let w = b.ins().stack_load(types::I64, types::I64, slot, ret_off);
                out.push(match c {
                    CType::I8 | CType::U8 => b.ins().ireduce(types::I8, w),
                    CType::I16 | CType::U16 => b.ins().ireduce(types::I16, w),
                    CType::I32 | CType::U32 => b.ins().ireduce(types::I32, w),
                    _ => w,
                });
            }
            Ty::S(SAbi { agg: Agg::Hfa(c, n), .. }) => {
                let es = if c == CType::F32 { 4 } else { 8 };
                let a = ret_area.unwrap();
                for k in 0..n as u32 {
                    out.push(b.ins().stack_load(types::I64, float_type(c), slot, (a + k * es) as i32));
                }
            }
            Ty::S(SAbi { agg: Agg::Words(w), .. }) => {
                let a = ret_area.unwrap();
                for k in 0..w as u32 {
                    out.push(b.ins().stack_load(types::I64, types::I64, slot, (a + k * 8) as i32));
                }
            }
            Ty::S(SAbi { agg: Agg::Indirect, .. }) => {}
        }
        b.ins().return_(&out);
        b.finalize(isa.frontend_config());
    }
    compile(func, "a callback")
}

fn parse_ty(v: Value, op: &str) -> Result<Ty> {
    if let Some(s) = parse_struct(v)? {
        return Ok(Ty::S(s));
    }
    if v.is_keyword() && v.sym_name() == "void" {
        return Ok(Ty::C(CType::Void));
    }
    Ok(Ty::C(type_arg(v, op)?))
}

/// `(%ffi-callback [param-types] ret-type f chan)`: [id address].
pub(crate) fn make_callback(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let mut params = Vec::new();
    for p in norn_rt::seq::SeqIter::new(a[0]).map(|it| it.collect::<Vec<_>>()).unwrap_or_default() {
        match parse_ty(p, "callback")? {
            Ty::C(CType::Void) => return Err(Error::new("callback: a parameter can't be :void")),
            t => params.push(t),
        }
    }
    let ret = parse_ty(a[1], "callback")?;
    let sig = (params.clone(), ret);
    let reused = FREE_TRAMPOLINES.lock().unwrap().as_mut().and_then(|m| m.get_mut(&sig).and_then(|v| v.pop()));
    let (id, at) = match reused {
        Some(t) => t,
        None => {
            let id = NEXT_CALLBACK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            (id, trampoline(id, &params, ret)?)
        }
    };
    let port = crate::ext::Port::new(a[3])?;
    let root = crate::sched::root(a[2]);
    let cb = Callback { code: at, params, ret, f: a[2], port, root };
    CALLBACKS.lock().unwrap().get_or_insert_with(HashMap::new).insert(id, Arc::new(cb));
    Ok(vec_from_slice(&mut vm.m, &[Value::fix(id as i64), Value::fix(at as i64)]))
}

/// `(%ffi-callback-reply! id token value)`: the answer to a call from
/// another thread (nil when f failed: 0 to C).
pub(crate) fn callback_reply(a: &[Value]) -> Result<Value> {
    let id = int_arg(a[0], CType::U64)?;
    let token = int_arg(a[1], CType::U64)?;
    let w = match callback(id) {
        Some(cb) if !a[2].is_nil() || cb.ret == Ty::C(CType::Ptr) => ret_word(&cb, a[2]).unwrap_or(0),
        _ => 0,
    };
    if let Some(reply) = REPLIES.lock().unwrap().as_mut().and_then(|m| m.remove(&token)) {
        let (lock, cv) = &*reply;
        *lock.lock().unwrap() = Some(w);
        cv.notify_all();
        // (the lane thread waits for an answer on the lane's queue instead)
        crate::main_lane::wake();
    }
    Ok(Value::NIL)
}

/// `(%ffi-callback-free! id)`: C must not call it after this. Its
/// trampoline is kept for the next callback of the same signature (which
/// then answers a call to the old address).
pub(crate) fn callback_free(a: &[Value]) -> Result<Value> {
    let id = int_arg(a[0], CType::U64)?;
    if let Some(cb) = CALLBACKS.lock().unwrap().as_mut().and_then(|m| m.remove(&id)) {
        crate::sched::unroot(cb.root);
        cb.port.close();
        let sig = (cb.params.clone(), cb.ret);
        FREE_TRAMPOLINES.lock().unwrap().get_or_insert_with(HashMap::new).entry(sig).or_default().push((id, cb.code));
    }
    Ok(Value::NIL)
}
