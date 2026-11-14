//! Owned mutable objects (design §4.2): who may read and write a mutable
//! instance.
//!
//! The owner word holds the owning fiber, nil (free: the next fiber to touch
//! it claims it with a compare-and-swap), or `[lock-channel holder-atom]`
//! (shared). An owner that has finished counts as free. Outside any fiber
//! (boot) there is no owner to check.
//!
//! A failed check is an error whose message starts with `Ownership:`, which
//! `%runtime-error` signals as an `OwnershipError`. Reading another fiber's
//! object is allowed while `*inspecting*` is bound true (the debugger's
//! eval-in-frame and the inspector); the printer and bulk views (`keys`,
//! `vals`, `seq`) only observe and aren't checked.

use crate::vm::{Error, Result, Vm};
use norn_rt::clos::{layout_index, slot_mutable};
use norn_rt::conc::fiber;
use norn_rt::print::pr_str;
use norn_rt::runtime::rt;
use norn_rt::{Kind, Value};
use std::sync::atomic::Ordering;

fn ownership(msg: String) -> Error {
    Error::of("OwnershipError", format!("Ownership: {msg}"))
}

fn describe(inst: Value) -> String {
    pr_str(inst.instance_class().class_field(norn_rt::clos::class::NAME), true)
}

/// Is `*inspecting*` bound true in this fiber?
fn inspecting(vm: &mut Vm) -> bool {
    let sym = rt().symbol(&mut vm.m, None, "*inspecting*");
    let Some(var) = vm.core.lookup(sym) else { return false };
    let bound = if vm.dyn_env.is(Kind::Map) { vm.dyn_env.map_get(var) } else { None };
    bound.unwrap_or_else(|| var.var_root()).truthy()
}

/// May the current fiber read (`write` false) or change `inst`? Claims it if
/// it is free.
pub(crate) fn check(vm: &mut Vm, inst: Value, write: bool) -> Result<()> {
    if !inst.is(Kind::Instance) || !inst.instance_mutable() {
        return Ok(());
    }
    let me = vm.current;
    if me.is_nil() {
        return Ok(());
    }
    let cell = inst.instance_owner();
    loop {
        let o = Value(cell.load(Ordering::Acquire));
        if o == me {
            return Ok(());
        }
        let free = o.is_nil() || (o.is(Kind::Fiber) && o.fiber_state().load(Ordering::Acquire) == fiber::DONE);
        if free {
            if cell.compare_exchange(o.0, me.0, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                return Ok(());
            }
            continue;
        }
        if o.is(Kind::Vector) {
            // Shared: changes need the object's lock; single reads don't.
            let holder = o.vec_nth(1).atom_get();
            if holder == me || !write {
                return Ok(());
            }
            return Err(ownership(format!("{} is shared: change it inside (locking obj …)", describe(inst))));
        }
        if !write && inspecting(vm) {
            return Ok(());
        }
        let who = if o.is(Kind::Fiber) { format!("fiber {}", o.fiber_id()) } else { "another fiber".into() };
        return Err(ownership(format!(
            "{} belongs to {who}; hand it over with release! (or send it on a channel), or share it",
            describe(inst)
        )));
    }
}

/// `(%set-slot! inst :k v)`: set a `:mutable` slot.
pub(crate) fn set_slot(vm: &mut Vm, inst: Value, key: Value, v: Value) -> Result<Value> {
    if !inst.is(Kind::Instance) || !inst.instance_mutable() {
        return Err(Error::new(format!("set-slot! needs an instance of a (:mutable) class, got {}", pr_str(inst, true))));
    }
    let class = inst.instance_class();
    if !slot_mutable(class, key) {
        return Err(Error::new(format!("Slot {} of {} isn't :mutable", pr_str(key, true), describe(inst))));
    }
    check(vm, inst, true)?;
    let i = layout_index(inst.instance_layout(), key).ok_or_else(|| {
        Error::new(format!("This {} was made before {} had slot {}; make a new one", describe(inst), describe(inst), pr_str(key, true)))
    })?;
    inst.instance_set_raw(i, v);
    Ok(v)
}

/// `(%release! inst)`: give up ownership; the next fiber to touch it claims it.
pub(crate) fn release(vm: &mut Vm, inst: Value) -> Result<Value> {
    if inst.is(Kind::Instance) && inst.instance_mutable() {
        check(vm, inst, true)?;
        let cell = inst.instance_owner();
        if !Value(cell.load(Ordering::Acquire)).is(Kind::Vector) {
            cell.store(Value::NIL.0, Ordering::Release);
        }
    }
    Ok(inst)
}

/// `(%share! inst lock)`: from now on any fiber may use it, changing it
/// under `lock` (`[channel holder-atom]`).
pub(crate) fn share(vm: &mut Vm, inst: Value, lock: Value) -> Result<Value> {
    if !inst.is(Kind::Instance) || !inst.instance_mutable() {
        return Err(Error::new(format!("share needs an instance of a (:mutable) class, got {}", pr_str(inst, true))));
    }
    check(vm, inst, true)?;
    let cell = inst.instance_owner();
    if !Value(cell.load(Ordering::Acquire)).is(Kind::Vector) {
        cell.store(lock.0, Ordering::Release);
    }
    Ok(inst)
}

/// `(%owner inst)`: the owner word (a fiber, nil, or the shared lock).
pub(crate) fn owner(inst: Value) -> Value {
    if inst.is(Kind::Instance) && inst.instance_mutable() { Value(inst.instance_owner().load(Ordering::Acquire)) } else { Value::NIL }
}

/// Putting a mutable object the current fiber owns on a channel releases it,
/// so the receiver can claim it.
pub(crate) fn release_on_send(vm: &Vm, v: Value) {
    if v.is(Kind::Instance) && v.instance_mutable() && !vm.current.is_nil() {
        let _ = v.instance_owner().compare_exchange(vm.current.0, Value::NIL.0, Ordering::AcqRel, Ordering::Acquire);
    }
}
