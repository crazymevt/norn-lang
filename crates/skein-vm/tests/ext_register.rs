//! Extensions must be registered before the runtime starts.

#[test]
fn registering_after_start_is_refused() {
    skein_vm::start().unwrap();
    let r = skein_vm::ext::register(vec![]);
    assert!(r.unwrap_err().contains("before the runtime starts"));
}
