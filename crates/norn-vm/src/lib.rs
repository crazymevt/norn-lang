//! Norn tier 0: the bootstrap reader, seed compiler, bytecode interpreter
//! and primitives. See docs/runtime.md §8.1 and §9.

pub mod bytecode;
pub mod compiler;
pub mod conc;
pub mod eval;
pub mod prims;
pub mod reader;
pub mod sched;
pub mod vm;

pub use reader::{ReadError, Reader};
pub use sched::{Norn, RootDone, RootTask, start};
pub use vm::{Error, Result, Vm};
