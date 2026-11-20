//! A tiny Windows program for skein.pe's tests: it writes a line to its
//! standard output (a console, a pipe or a file) and exits with 7. No
//! runtime library: it links with rust-lld alone (see README.md).
#![no_std]
#![no_main]

use core::ffi::c_void;

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn GetStdHandle(which: u32) -> *mut c_void;
    fn WriteFile(h: *mut c_void, buf: *const u8, n: u32, written: *mut u32, overlapped: *mut c_void) -> i32;
    fn ExitProcess(code: u32) -> !;
}

static MESSAGE: &[u8] = b"hello from skein.pe's test program\r\n";

#[unsafe(no_mangle)]
pub extern "system" fn mainCRTStartup() -> ! {
    // (read through a pointer in .rdata, so the program has relocations
    // and a .reloc section, as a linker's usual output does)
    let msg: &[u8] = unsafe { core::ptr::read_volatile(&MESSAGE) };
    unsafe {
        let out = GetStdHandle(-11i32 as u32); // STD_OUTPUT_HANDLE
        let mut n = 0u32;
        WriteFile(out, msg.as_ptr(), msg.len() as u32, &mut n, core::ptr::null_mut());
        ExitProcess(7)
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    unsafe { ExitProcess(101) }
}
