//! Extensions must be registered before the runtime starts.

#[test]
fn registering_after_start_is_refused() {
    norn_vm::start().unwrap();
    let r = norn_vm::ext::register(vec![]);
    assert!(r.unwrap_err().contains("before the runtime starts"));
}
