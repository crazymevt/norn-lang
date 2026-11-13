//! Norn tier 0: the bootstrap reader, seed compiler, bytecode interpreter
//! and primitives. See docs/runtime.md §8.1 and §9.

pub mod blocking;
pub mod bytecode;
pub mod codec;
pub mod text;
pub mod compiler;
pub mod conc;
pub mod eval;
pub mod ffi;
pub mod image;
pub mod jit;
pub mod net;
pub mod numeric;
pub mod os;
pub mod owned;
pub mod prims;
pub mod reactor;
pub mod reader;
pub mod sched;
pub mod server;
pub mod sorted;
pub mod sys;
pub mod vm;

pub use reader::{ReadError, Reader};
pub use sched::{Norn, RootDone, RootTask, start, start_with, subscribe};
pub use vm::{Error, Result, Vm};
