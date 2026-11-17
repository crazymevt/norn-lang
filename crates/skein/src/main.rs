//! The `skein` command (see lib.rs).

fn main() {
    // The first thread serves foreign calls that need it (windows, events,
    // graphics: :main-thread functions); the command line runs on another.
    skein::run_with_main_thread(|_| skein_vm::main_lane::serve());
}
