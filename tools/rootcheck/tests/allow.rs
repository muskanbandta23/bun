//! The way out.

use std::marker::PhantomData;
use std::ptr::NonNull;

use bun_engine_vm::*;

#[cfg_attr(rootcheck, rootcheck::allow("a reason"))]
pub fn function(value: Value, list: &mut RootVec<Value>, vm: &Vm) -> usize {
    list.sort_by_key(|_| JsString::from_literal(vm, "new").len());
    let in_closure = || vec![value].iter().len();
    vec![value].iter().len() + in_closure()
}

#[cfg_attr(rootcheck, rootcheck::allow("a reason"))]
static mut STATIC: Value = Value::NULL;

#[cfg_attr(rootcheck, rootcheck::allow("a reason"))]
pub struct Type(Part);

pub struct Part(Vec<Value>);

pub struct HasField {
    #[cfg_attr(rootcheck, rootcheck::allow("a reason"))]
    allowed: Vec<Value>,
    other: Vec<Value>, //~ hidden: neither traced
}

pub fn locals(a: Type, b: HasField, c: Part) -> usize { //~ hidden: borrowed here
    a.0.0.iter().len() + b.allowed.iter().len() + c.0.iter().len()
}

pub fn not_inherited_by_containers(a: Type) -> usize {
    vec![a].iter().len() //~ hidden: borrowed here
}

#[cfg_attr(rootcheck, rootcheck::allow)] //~ allow: takes a reason
pub fn no_reason(value: Value) -> usize {
    vec![value].iter().len()
}

#[cfg_attr(rootcheck, rootcheck::allow(" "))] //~ allow: takes a reason
pub fn blank_reason(value: Value) -> usize {
    vec![value].iter().len()
}

#[cfg_attr(rootcheck, rootcheck::allow("a reason"))] //~ allow: allows nothing
pub fn nothing_to_allow() {}

#[cfg_attr(rootcheck, rootcheck::allow("a reason"))] //~ allow: goes on a function
pub mod module {}

#[cfg_attr(rootcheck, rootcheck::alow("a reason"))] //~ allow: unknown attribute
pub fn misspelt() {}

#[deny(unsafe_code)]
#[cfg_attr(rootcheck, rootcheck::allow("a reason"))] //~ allow: only for where `unsafe` is allowed
pub fn denied(value: Value) -> usize {
    vec![value].iter().len()
}

pub struct Visited(Vec<Value>);

// SAFETY: a test.
unsafe impl GcFree for Visited {}

pub fn implemented_by_hand(visited: Visited) -> usize {
    vec![visited].iter().len()
}

pub struct OwnBox<T>(NonNull<T>, PhantomData<T>);

pub struct Counted<T>(NonNull<T>);

impl<T> Drop for Counted<T> {
    fn drop(&mut self) {}
}

pub struct Borrowing<'a, T>(NonNull<T>, PhantomData<&'a T>);

pub fn own_pointers(
    a: OwnBox<Value>, //~ hidden: OwnBox<
    b: Counted<Value>, //~ hidden: Counted<
    c: Borrowing<'_, Value>,
    d: NonNull<Value>,
) {
    let _ = (&a, &b, &c, &d);
}
