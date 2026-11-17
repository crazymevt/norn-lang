//! M0 platform spike (runtime.md §7.4, §3.2): before building tier 1 on
//! Cranelift, check on this machine that
//!
//! 1. Cranelift's JIT works (W^X / MAP_JIT on Apple Silicon),
//! 2. the `tail` calling convention gives real tail calls — the "push frame +
//!    tail call" calling convention that tier 1 depends on, and
//! 3. machine code written to a file can be mmap'd executable and run, which
//!    is how images will carry native code.
//!
//! Run: `cargo run -p jit-spike --release`

use cranelift_codegen::control::ControlPlane;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{AbiParam, Function, InstBuilder, Signature, UserFuncName, types};
use cranelift_codegen::isa::{CallConv, OwnedTargetIsa};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module, default_libcall_names};
use std::time::Instant;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// `pic`: position-independent code. Images need it; cranelift-jit rejects it
/// on aarch64 (only x86_64 supports PIC there), so the JIT checks use `false`.
fn isa(pic: bool) -> Res<OwnedTargetIsa> {
    let mut flags = settings::builder();
    flags.set("opt_level", "speed")?;
    flags.set("is_pic", if pic { "true" } else { "false" })?;
    let isa = cranelift_native::builder()?.finish(settings::Flags::new(flags))?;
    Ok(isa)
}

fn sig(cc: CallConv, params: usize) -> Signature {
    let mut s = Signature::new(cc);
    for _ in 0..params {
        s.params.push(AbiParam::new(types::I64));
    }
    s.returns.push(AbiParam::new(types::I64));
    s
}

/// 1. A plain JIT-compiled function.
fn check_basic_jit() -> Res<()> {
    let isa = isa(false)?;
    let cc = isa.default_call_conv();
    let fc = isa.frontend_config();
    let mut module = JITModule::new(JITBuilder::with_isa(isa, default_libcall_names()));
    let s = sig(cc, 2);
    let id = module.declare_function("add", Linkage::Local, &s)?;
    let mut ctx = module.make_context();
    ctx.func.signature = s;
    let mut fbc = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbc);
        let blk = b.create_block();
        b.append_block_params_for_function_params(blk);
        b.switch_to_block(blk);
        b.seal_block(blk);
        let (x, y) = (b.block_params(blk)[0], b.block_params(blk)[1]);
        let r = b.ins().iadd(x, y);
        b.ins().return_(&[r]);
        b.finalize(fc);
    }
    module.define_function(id, &mut ctx)?;
    module.finalize_definitions()?;
    let f: extern "C" fn(i64, i64) -> i64 = unsafe { std::mem::transmute(module.get_finalized_function(id)) };
    let r = f(40, 2);
    assert_eq!(r, 42);
    println!("[1] JIT basic call: add(40, 2) = {r}  ✓");
    Ok(())
}

/// 2. Proper tail calls with the `tail` calling convention.
///
/// `sum(n, acc)` tail-calls itself n times. With n = 100M and no TCO this
/// would need ~100M stack frames and crash; with TCO it runs in constant stack.
fn check_tail_calls() -> Res<()> {
    let isa = isa(false)?;
    let cc = isa.default_call_conv();
    let fc = isa.frontend_config();
    let mut module = JITModule::new(JITBuilder::with_isa(isa, default_libcall_names()));

    let tail_sig = sig(CallConv::Tail, 2);
    let sum_id = module.declare_function("sum", Linkage::Local, &tail_sig)?;
    let entry_sig = sig(cc, 1);
    let entry_id = module.declare_function("entry", Linkage::Local, &entry_sig)?;
    let mut fbc = FunctionBuilderContext::new();

    // sum(n, acc) = if n == 0 { acc } else { return_call sum(n - 1, acc + n) }
    let mut ctx = module.make_context();
    ctx.func.signature = tail_sig;
    {
        let me = module.declare_func_in_func(sum_id, &mut ctx.func);
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbc);
        let (start, done, again) = (b.create_block(), b.create_block(), b.create_block());
        b.append_block_params_for_function_params(start);
        b.switch_to_block(start);
        b.seal_block(start);
        let (n, acc) = (b.block_params(start)[0], b.block_params(start)[1]);
        let is_zero = b.ins().icmp_imm_s(IntCC::Equal, n, 0);
        b.ins().brif(is_zero, done, &[], again, &[]);
        b.switch_to_block(done);
        b.seal_block(done);
        b.ins().return_(&[acc]);
        b.switch_to_block(again);
        b.seal_block(again);
        let n1 = b.ins().iadd_imm_s(n, -1);
        let acc1 = b.ins().iadd(acc, n);
        b.ins().return_call(me, &[n1, acc1]);
        b.finalize(fc);
    }
    module.define_function(sum_id, &mut ctx)?;
    module.clear_context(&mut ctx);

    // entry(n) = sum(n, 0), called with the platform convention from Rust.
    ctx.func.signature = entry_sig;
    {
        let callee = module.declare_func_in_func(sum_id, &mut ctx.func);
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbc);
        let blk = b.create_block();
        b.append_block_params_for_function_params(blk);
        b.switch_to_block(blk);
        b.seal_block(blk);
        let n = b.block_params(blk)[0];
        let zero = b.ins().iconst(types::I64, 0);
        let call = b.ins().call(callee, &[n, zero]);
        let r = b.inst_results(call)[0];
        b.ins().return_(&[r]);
        b.finalize(fc);
    }
    module.define_function(entry_id, &mut ctx)?;
    module.finalize_definitions()?;

    let entry: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(module.get_finalized_function(entry_id)) };
    let n: i64 = 100_000_000;
    let t = Instant::now();
    let r = entry(n);
    let dt = t.elapsed();
    assert_eq!(r, n * (n + 1) / 2);
    // Without TCO, 100M frames would overflow the 8 MiB stack. (The time is
    // not a per-call cost: Cranelift may lower a self tail call to a jump.)
    println!("[2] Tail calls: 100M self tail calls in {:.0} ms, constant stack  ✓", dt.as_secs_f64() * 1e3);
    Ok(())
}

/// 3. Native code loaded from a file — how images will carry tier-1 code.
fn check_file_mapped_code() -> Res<()> {
    let isa = isa(true)?;
    let fc = isa.frontend_config();
    let mut func = Function::with_name_signature(UserFuncName::user(0, 0), sig(isa.default_call_conv(), 3));
    let mut fbc = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut func, &mut fbc);
        let blk = b.create_block();
        b.append_block_params_for_function_params(blk);
        b.switch_to_block(blk);
        b.seal_block(blk);
        let p = b.block_params(blk).to_vec();
        let prod = b.ins().imul(p[0], p[1]);
        let r = b.ins().iadd(prod, p[2]);
        b.ins().return_(&[r]);
        b.finalize(fc);
    }
    let mut ctx = cranelift_codegen::Context::for_function(func);
    let compiled = ctx.compile(&*isa, &mut ControlPlane::default()).map_err(|e| format!("{:?}", e.inner))?;
    let bytes = compiled.code_buffer().to_vec();
    assert!(compiled.buffer.relocs().is_empty(), "leaf function should need no relocations");

    let dir = std::env::temp_dir().join(format!("skein-jit-spike-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("mul_add.code");
    std::fs::write(&path, &bytes)?;

    let read_back = std::fs::read(&path)?;
    std::fs::remove_dir_all(&dir)?;

    // 3a. Map the file's pages executable directly (zero-copy).
    match map_file_exec(&read_back) {
        Ok(r) => println!("[3a] Direct mmap of a code file as PROT_EXEC works: mul_add(6, 7, 0) = {r}  ✓"),
        Err(e) => println!("[3a] Direct mmap of a code file as PROT_EXEC is refused: {e}  ✗ (expected on macOS arm64)"),
    }

    // 3b. Copy the file's code into JIT memory (one memcpy at load).
    let r = map_copy_exec(&read_back)?;
    assert_eq!(r, 42);
    println!("[3b] Code read from a file and copied into JIT memory runs: mul_add(6, 7, 0) = {r}  ✓");
    Ok(())
}

fn call_mul_add(p: *mut libc::c_void) -> i64 {
    // SAFETY: p points at mul_add's machine code, mapped executable.
    let f: extern "C" fn(i64, i64, i64) -> i64 = unsafe { std::mem::transmute(p) };
    f(6, 7, 0)
}

fn map_file_exec(bytes: &[u8]) -> Res<i64> {
    use std::os::fd::AsRawFd;
    let path = std::env::temp_dir().join(format!("skein-jit-spike-{}.code", std::process::id()));
    std::fs::write(&path, bytes)?;
    let file = std::fs::File::open(&path)?;
    // SAFETY: mapping a file we just wrote; length matches its size.
    let result = unsafe {
        let p = libc::mmap(
            std::ptr::null_mut(),
            bytes.len(),
            libc::PROT_READ | libc::PROT_EXEC,
            libc::MAP_PRIVATE,
            file.as_raw_fd(),
            0,
        );
        if p == libc::MAP_FAILED {
            Err(format!("{}", std::io::Error::last_os_error()).into())
        } else {
            let r = call_mul_add(p);
            libc::munmap(p, bytes.len());
            Ok(r)
        }
    };
    let _ = std::fs::remove_file(&path);
    result
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn sys_icache_invalidate(start: *mut libc::c_void, len: libc::size_t);
}

/// Copy code into fresh executable memory. On macOS arm64 that means MAP_JIT
/// pages, toggling the calling thread between writable and executable.
fn map_copy_exec(bytes: &[u8]) -> Res<i64> {
    let len = bytes.len();
    // SAFETY: fresh anonymous mapping of `len` bytes; we write then execute it.
    unsafe {
        #[cfg(target_os = "macos")]
        let p = {
            let p = libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_JIT,
                -1,
                0,
            );
            if p == libc::MAP_FAILED {
                return Err(format!("mmap MAP_JIT failed: {}", std::io::Error::last_os_error()).into());
            }
            libc::pthread_jit_write_protect_np(0);
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p as *mut u8, len);
            libc::pthread_jit_write_protect_np(1);
            sys_icache_invalidate(p, len);
            p
        };
        #[cfg(not(target_os = "macos"))]
        let p = {
            let p = libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            );
            if p == libc::MAP_FAILED {
                return Err(format!("mmap failed: {}", std::io::Error::last_os_error()).into());
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p as *mut u8, len);
            if libc::mprotect(p, len, libc::PROT_READ | libc::PROT_EXEC) != 0 {
                return Err(format!("mprotect failed: {}", std::io::Error::last_os_error()).into());
            }
            p
        };
        let r = call_mul_add(p);
        libc::munmap(p, len);
        Ok(r)
    }
}

fn main() -> Res<()> {
    println!("Skein JIT spike on {}-{}", std::env::consts::ARCH, std::env::consts::OS);
    check_basic_jit()?;
    check_tail_calls()?;
    check_file_mapped_code()?;
    println!("All platform checks passed.");
    Ok(())
}
