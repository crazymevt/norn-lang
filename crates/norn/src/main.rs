//! The `norn` command (see lib.rs).

fn main() {
    // The first thread serves foreign calls that need it (windows, events,
    // graphics: :main-thread functions); the command line runs on another.
    norn::run_with_main_thread(|_| norn_vm::main_lane::serve());
}
