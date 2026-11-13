//! The basic FFI (runtime.md §5.6): load a library and call C functions with
//! integer, float, pointer and byte-buffer arguments.
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
//!
//! **Fast or blocking.** A fast call runs on the carrier in a native region,
//! so the GC can run meanwhile and the monitor starts a spare carrier if it
//! takes too long. A `:blocking` function runs on the blocking pool. Either
//! way the Norn values it points into are rooted until it returns.
//!
//! Not yet (M9): callbacks from C, structs, variadic functions.

use crate::reactor::{self, Io};
use crate::vm::{Error, Result, Vm};
use cranelift_codegen::control::ControlPlane;
use cranelift_codegen::ir::{AbiParam, Function, InstBuilder, MemFlagsData, Signature, UserFuncName, types};
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
            "i64" | "long" | "isize" | "ssize" => CType::I64,
            "u64" | "ulong" | "usize" | "size" => CType::U64,
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
}

pub(crate) struct Foreign {
    addr: usize,
    stub: usize,
    params: Vec<Param>,
    ret: (CType, bool), // (type, is :string)
    blocking: bool,
    /// Keeps the library loaded while the function exists.
    _lib: Arc<libloading::Library>,
}

type Stub = unsafe extern "C" fn(u64, *const u64, *mut u64);

/// (parameter types, return type) → stub address.
type StubCache = HashMap<(Vec<CType>, CType), usize>;

static STUBS: Mutex<Option<StubCache>> = Mutex::new(None);

/// The call stub for a signature (compiled once).
fn stub_for(params: &[CType], ret: CType) -> Result<usize> {
    let key = (params.to_vec(), ret);
    if let Some(&s) = STUBS.lock().unwrap().get_or_insert_with(HashMap::new).get(&key) {
        return Ok(s);
    }
    let isa = crate::jit::isa().ok_or_else(|| Error::new("No native code generator for this machine"))?;
    let cc = isa.default_call_conv();
    let mut target = Signature::new(cc);
    target.params.extend(params.iter().filter_map(|t| t.abi()));
    target.returns.extend(ret.abi());
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
        for (i, t) in params.iter().enumerate() {
            let off = (i * 8) as i32;
            let v = match t {
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
            };
            vals.push(v);
        }
        let sref = b.import_signature(target);
        let call = b.ins().call_indirect(sref, fnptr, &vals);
        if ret != CType::Void {
            let r = b.inst_results(call)[0];
            let stored = match ret {
                CType::F32 | CType::F64 | CType::I64 | CType::U64 | CType::Ptr => r,
                CType::I8 | CType::I16 | CType::I32 => b.ins().sextend(types::I64, r),
                _ => b.ins().uextend(types::I64, r),
            };
            b.ins().store(flags, stored, retp, 0);
        }
        b.ins().return_(&[]);
        b.finalize(isa.frontend_config());
    }
    let mut ctx = cranelift_codegen::Context::for_function(func);
    let code = ctx.compile(&**isa, &mut ControlPlane::default()).map_err(|e| Error::new(format!("Can't compile an FFI stub: {e:?}")))?;
    let at = crate::jit::install_code(code.code_buffer()).ok_or_else(|| Error::new("Out of executable memory"))?;
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

/// `(%ffi-fn library name [param-types] ret-type blocking?)`: a `:foreign` handle.
pub(crate) fn function(vm: &mut Vm, lib: Value, name: &str, params: Value, ret: Value, blocking: bool) -> Result<Value> {
    let e = reactor::entry_of(lib, "library")?;
    let lib_arc = match e.io.lock().unwrap().as_ref() {
        Some(Io::Library(l)) => l.clone(),
        _ => return Err(Error::new("The library is closed")),
    };
    let mut ps = Vec::new();
    let mut cts = Vec::new();
    for p in norn_rt::seq::SeqIter::new(params).map(|it| it.collect::<Vec<_>>()).unwrap_or_default() {
        let k = keyword_name(p)?;
        let ct = CType::parse(&k).filter(|&t| t != CType::Void).ok_or_else(|| Error::new(format!("Unknown parameter type :{k}")))?;
        ps.push(match k.as_str() {
            "pointer" => Param::Pointer,
            "bytes" => Param::Bytes,
            "string" => Param::Str,
            "out" => Param::Out,
            _ if matches!(ct, CType::F32 | CType::F64) => Param::Float(ct),
            _ => Param::Int(ct),
        });
        cts.push(ct);
    }
    let rk = keyword_name(ret)?;
    let rt_ = CType::parse(&rk).filter(|_| !matches!(rk.as_str(), "bytes" | "out")).ok_or_else(|| Error::new(format!("Unknown return type :{rk}")))?;
    let stub = stub_for(&cts, rt_)?;
    let cname = std::ffi::CString::new(name).map_err(|_| Error::new("A symbol name can't contain NUL"))?;
    // SAFETY: we only take the symbol's address.
    let addr = unsafe { lib_arc.get::<*const ()>(cname.as_bytes_with_nul()) }
        .map(|s| *s as usize)
        .map_err(|e| Error::new(format!("No symbol {name}: {e}")))?;
    let f = Foreign { addr, stub, params: ps, ret: (rt_, rk == "string"), blocking, _lib: lib_arc };
    let entries = [("library", lib), ("name", str_new(&mut vm.m, name)), ("params", params), ("ret", ret), ("blocking", Value::bool(blocking))];
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
}

// SAFETY: the Values in `keep` are rooted for the call; the rest is owned data.
unsafe impl Send for Prepared {}

fn prepare(f: &Foreign, args: &[Value]) -> Result<Prepared> {
    if args.len() != f.params.len() {
        return Err(Error::new(format!("The foreign function takes {} arguments, got {}", f.params.len(), args.len())));
    }
    let mut p = Prepared { raw: Vec::with_capacity(args.len()), keep: Vec::new(), _strings: Vec::new(), outs: Vec::new() };
    for (&a, &param) in args.iter().zip(&f.params) {
        let w = match param {
            Param::Int(t) => int_arg(a, t)?,
            Param::Float(t) => {
                let x = if a.is_fixnum() { a.as_fixnum() as f64 } else if a.is(Kind::Float) { a.as_f64() } else { return Err(Error::new("Expected a number argument")) };
                if t == CType::F32 { (x as f32).to_bits() as u64 } else { x.to_bits() }
            }
            Param::Pointer => if a.is_nil() { 0 } else { int_arg(a, CType::U64)? },
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
    if outs.is_empty() {
        return Ok(v);
    }
    let mut vs = vec![v];
    for o in outs {
        vs.push(bytes_new(m, &o));
    }
    Ok(vec_from_slice(m, &vs))
}

/// `(%ffi-call f args…)`: the result; with `:out` parameters,
/// `[result out-bytes…]`.
pub(crate) fn call(vm: &mut Vm, h: Value, args: &[Value]) -> Result<Value> {
    let e = reactor::entry_of(h, "foreign")?;
    let (stub, addr, ret, blocking, mut prep) = {
        let io = e.io.lock().unwrap();
        let Some(Io::Foreign(f)) = io.as_ref() else { return Err(Error::new("The foreign function is closed")) };
        (f.stub, f.addr, f.ret, f.blocking, prepare(f, args)?)
    };
    // SAFETY: stub was compiled for this signature.
    let stub: Stub = unsafe { std::mem::transmute(stub) };
    if !blocking {
        let mut r: u64 = 0;
        let keep = std::mem::take(&mut prep.keep);
        // SAFETY: the arguments match the stub's signature; what they point
        // at (prep's buffers, rooted Bytes) outlives the call.
        vm.native_region(&keep, || unsafe { stub(addr as u64, prep.raw.as_ptr(), &mut r) });
        return result(&mut vm.m, ret, r, std::mem::take(&mut prep.outs)).map_err(Error::new);
    }
    let key = crate::sched::root_many(&prep.keep);
    let e2 = e.clone();
    crate::blocking::run(vm, move || {
        let _hold = e2; // the function (and its library) stays loaded
        let mut r: u64 = 0;
        // SAFETY: as above; the Bytes are rooted until the result is built.
        unsafe { stub(addr as u64, prep.raw.as_ptr(), &mut r) };
        let outs = std::mem::take(&mut prep.outs);
        Box::new(move |m: &mut Mutator| {
            drop(prep);
            crate::sched::unroot_many(key);
            result(m, ret, r, outs)
        }) as crate::blocking::Finish
    })
}
