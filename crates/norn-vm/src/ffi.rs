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
//! **M9 part 9.8** adds memory access (addresses as integers) and callbacks
//! from C (below); struct layouts are Norn (`norn.ffi`), on the memory
//! primitives. Not yet: structs passed by value, variadic functions.

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
    /// Keeps the library loaded while the function exists (none for a
    /// function known only by its address).
    _lib: Option<Arc<libloading::Library>>,
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

/// `(%ffi-fn library name [param-types] ret-type blocking?)`: a `:foreign`
/// handle. With nil for the library, name is the function's address.
pub(crate) fn function(vm: &mut Vm, lib: Value, name: Value, params: Value, ret: Value, blocking: bool) -> Result<Value> {
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
    let addr = match &lib_arc {
        None => address(name, "function-at")?,
        Some(l) => {
            let name = name.as_str();
            let cname = std::ffi::CString::new(name).map_err(|_| Error::new("A symbol name can't contain NUL"))?;
            // SAFETY: we only take the symbol's address.
            unsafe { l.get::<*const ()>(cname.as_bytes_with_nul()) }.map(|s| *s as usize).map_err(|e| Error::new(format!("No symbol {name}: {e}")))?
        }
    };
    let f = Foreign { addr, stub, params: ps, ret: (rt_, rk == "string"), blocking, _lib: lib_arc };
    let entries = [("library", lib), ("name", name), ("params", params), ("ret", ret), ("blocking", Value::bool(blocking))];
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
        // (a callback C makes meanwhile, on this thread, runs on this Vm)
        let prev = CALLING_VM.with(|c| c.replace(vm as *mut Vm));
        // SAFETY: the arguments match the stub's signature; what they point
        // at (prep's buffers, rooted Bytes) outlives the call.
        vm.native_region(&keep, || unsafe { stub(addr as u64, prep.raw.as_ptr(), &mut r) });
        CALLING_VM.with(|c| c.set(prev));
        if let Some(e) = CALLBACK_ERROR.with(|c| c.borrow_mut().take()) {
            return Err(e);
        }
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
            result(m, ret, r, outs).map_err(crate::vm::Failure::from)
        }) as crate::blocking::Finish
    })
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
// An error in f can't unwind through C: the callback returns 0, and in the
// first case the error is signalled when the FFI call returns.

struct Callback {
    params: Vec<CType>,
    ret: CType,
    f: Value,
    port: crate::ext::Port,
    root: u64,
}

// SAFETY: f is rooted for as long as the callback lives; it's only used on
// a carrier, by a running participant.
unsafe impl Send for Callback {}
unsafe impl Sync for Callback {}

static CALLBACKS: Mutex<Option<HashMap<u64, Arc<Callback>>>> = Mutex::new(None);
static NEXT_CALLBACK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

type Reply = Arc<(Mutex<Option<u64>>, std::sync::Condvar)>;
static REPLIES: Mutex<Option<HashMap<u64, Reply>>> = Mutex::new(None);

thread_local! {
    /// The Vm making a fast FFI call on this thread (null otherwise).
    static CALLING_VM: std::cell::Cell<*mut Vm> = const { std::cell::Cell::new(std::ptr::null_mut()) };
    /// An error a callback raised, for the FFI call to signal once C returns.
    static CALLBACK_ERROR: std::cell::RefCell<Option<Error>> = const { std::cell::RefCell::new(None) };
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

fn run_callback(id: u64, args: *const u64) -> u64 {
    let Some(cb) = callback(id) else {
        eprintln!("norn: a C callback was called after it was freed");
        return 0;
    };
    // SAFETY: the trampoline stores one word per parameter.
    let raw: Vec<u64> = (0..cb.params.len()).map(|i| unsafe { *args.add(i) }).collect();
    let vm = CALLING_VM.with(|c| c.get());
    if !vm.is_null() {
        // SAFETY: set by `call` for the duration of a fast call on this
        // thread, which is inside native_region: nothing else uses the Vm.
        let vm = unsafe { &mut *vm };
        crate::sched::leave_native(vm.carrier);
        let r = (|| -> Result<u64> {
            let mut argv = Vec::with_capacity(raw.len());
            for (&w, &t) in raw.iter().zip(&cb.params) {
                argv.push(from_c(&mut vm.m, t, w).map_err(Error::new)?);
            }
            let v = vm.apply(cb.f, &argv)?;
            to_c(v, cb.ret)
        })();
        crate::sched::enter_native(vm.carrier);
        return match r {
            Ok(w) => w,
            Err(e) => {
                CALLBACK_ERROR.with(|c| {
                    let mut c = c.borrow_mut();
                    if c.is_none() {
                        *c = Some(e);
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
    let params = cb.params.clone();
    let posted = cb.port.post(|m| {
        let vs: Vec<Value> = raw.iter().zip(&params).map(|(&w, &t)| from_c(m, t, w).unwrap_or(Value::NIL)).collect();
        let args = vec_from_slice(m, &vs);
        vec_from_slice(m, &[args, Value::fix(token as i64)])
    });
    if !posted {
        REPLIES.lock().unwrap().as_mut().map(|m| m.remove(&token));
        eprintln!("norn: a C callback from another thread was dropped (its channel is full or closed)");
        return 0;
    }
    let (lock, cv) = &*reply;
    let mut got = lock.lock().unwrap();
    while got.is_none() {
        got = cv.wait(got).unwrap();
    }
    got.unwrap_or(0)
}

/// The trampoline for callback `id`: a C function of this signature.
fn trampoline(id: u64, params: &[CType], ret: CType) -> Result<usize> {
    use cranelift_codegen::ir::{StackSlotData, StackSlotKind};
    let isa = crate::jit::isa().ok_or_else(|| Error::new("No native code generator for this machine"))?;
    let cc = isa.default_call_conv();
    let mut sig = Signature::new(cc);
    sig.params.extend(params.iter().filter_map(|t| t.abi()));
    sig.returns.extend(ret.abi());
    let mut target = Signature::new(cc);
    target.params.extend([AbiParam::new(types::I64), AbiParam::new(types::I64), AbiParam::new(types::I64)]);
    let mut func = Function::with_name_signature(UserFuncName::user(0, 2), sig);
    let mut fbc = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut func, &mut fbc);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        b.seal_block(entry);
        let slot = b.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, (params.len() as u32 + 1) * 8, 3));
        let ps: Vec<_> = b.block_params(entry).to_vec();
        for (i, (&t, &v)) in params.iter().zip(&ps).enumerate() {
            let off = (i * 8) as i32;
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
        let ret_off = (params.len() * 8) as i32;
        let args_p = b.ins().stack_addr(types::I64, slot, 0);
        let ret_p = b.ins().stack_addr(types::I64, slot, ret_off);
        let idv = b.ins().iconst(types::I64, id as i64);
        let f = b.ins().iconst(types::I64, dispatch as *const () as usize as i64);
        let sref = b.import_signature(target);
        b.ins().call_indirect(sref, f, &[idv, args_p, ret_p]);
        let out = match ret {
            CType::Void => None,
            CType::F32 => Some(b.ins().stack_load(types::I64, types::F32, slot, ret_off)),
            CType::F64 => Some(b.ins().stack_load(types::I64, types::F64, slot, ret_off)),
            CType::I8 | CType::U8 => {
                let w = b.ins().stack_load(types::I64, types::I64, slot, ret_off);
                Some(b.ins().ireduce(types::I8, w))
            }
            CType::I16 | CType::U16 => {
                let w = b.ins().stack_load(types::I64, types::I64, slot, ret_off);
                Some(b.ins().ireduce(types::I16, w))
            }
            CType::I32 | CType::U32 => {
                let w = b.ins().stack_load(types::I64, types::I64, slot, ret_off);
                Some(b.ins().ireduce(types::I32, w))
            }
            _ => Some(b.ins().stack_load(types::I64, types::I64, slot, ret_off)),
        };
        match out {
            Some(v) => b.ins().return_(&[v]),
            None => b.ins().return_(&[]),
        };
        b.finalize(isa.frontend_config());
    }
    let mut ctx = cranelift_codegen::Context::for_function(func);
    let code = ctx.compile(&**isa, &mut ControlPlane::default()).map_err(|e| Error::new(format!("Can't compile a callback: {e:?}")))?;
    crate::jit::install_code(code.code_buffer()).ok_or_else(|| Error::new("Out of executable memory"))
}

/// `(%ffi-callback [param-types] ret-type f chan)`: [id address].
pub(crate) fn make_callback(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let mut params = Vec::new();
    for p in norn_rt::seq::SeqIter::new(a[0]).map(|it| it.collect::<Vec<_>>()).unwrap_or_default() {
        params.push(type_arg(p, "callback")?);
    }
    let rk = keyword_name(a[1])?;
    let ret = if rk == "void" { CType::Void } else { type_arg(a[1], "callback")? };
    let id = NEXT_CALLBACK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let at = trampoline(id, &params, ret)?;
    let port = crate::ext::Port::new(a[3])?;
    let root = crate::sched::root(a[2]);
    let cb = Callback { params, ret, f: a[2], port, root };
    CALLBACKS.lock().unwrap().get_or_insert_with(HashMap::new).insert(id, Arc::new(cb));
    Ok(vec_from_slice(&mut vm.m, &[Value::fix(id as i64), Value::fix(at as i64)]))
}

/// `(%ffi-callback-reply! id token value)`: the answer to a call from
/// another thread (nil when f failed: 0 to C).
pub(crate) fn callback_reply(a: &[Value]) -> Result<Value> {
    let id = int_arg(a[0], CType::U64)?;
    let token = int_arg(a[1], CType::U64)?;
    let w = match callback(id) {
        Some(cb) if !a[2].is_nil() || cb.ret == CType::Ptr => to_c(a[2], cb.ret).unwrap_or(0),
        _ => 0,
    };
    if let Some(reply) = REPLIES.lock().unwrap().as_mut().and_then(|m| m.remove(&token)) {
        let (lock, cv) = &*reply;
        *lock.lock().unwrap() = Some(w);
        cv.notify_all();
    }
    Ok(Value::NIL)
}

/// `(%ffi-callback-free! id)`: C must not call it after this. (Its code
/// stays: executable memory isn't reused.)
pub(crate) fn callback_free(a: &[Value]) -> Result<Value> {
    let id = int_arg(a[0], CType::U64)?;
    if let Some(cb) = CALLBACKS.lock().unwrap().as_mut().and_then(|m| m.remove(&id)) {
        crate::sched::unroot(cb.root);
        cb.port.close();
    }
    Ok(Value::NIL)
}
