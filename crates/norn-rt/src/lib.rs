//! Norn runtime core. See docs/runtime.md.
//!
//! M0 scope: value representation, NoGC heap, object model, persistent
//! collections, structural equality/hashing, printer, namespaces and vars.

pub mod array;
pub mod clos;
pub mod conc;
pub mod equal;
pub mod func;
pub mod heap;
pub mod image;
pub mod list;
pub mod map;
pub mod object;
pub mod print;
pub mod runtime;
pub mod safepoint;
pub mod seq;
pub mod string;
pub mod value;
pub mod vector;

pub use heap::Mutator;
pub use object::Kind;
pub use runtime::rt;
pub use value::Value;
