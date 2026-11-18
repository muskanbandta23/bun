use core::ptr;

use bun_engine_macros::native_abi;
use bun_engine_wasm_compiler::abi::Trap;
use bun_engine_wasm_format::TagIndex;

use crate::host::Thrown;
use crate::instance::Instance;
use crate::outcome::Outcome;
use crate::value::Reference;

#[native_abi]
pub(super) unsafe fn throw(instance: &Instance, tag: TagIndex, payload: *const u8) -> Outcome {
    let tag = instance.tag(tag);
    let size = tag.payload_size() as usize;
    // SAFETY: compiled code laid out the payload for the tag, for which there is room.
    let exception = unsafe {
        instance.new_exception(tag, |place| ptr::copy_nonoverlapping(payload, place, size))
    };
    let Some(exception) = exception else {
        return Outcome::failure(Trap::OutOfMemory);
    };
    instance.engine().host().throw(instance, exception);
    Outcome::failure(Thrown)
}

#[native_abi]
pub(super) unsafe fn throw_ref(instance: &Instance, exception: Reference) -> Outcome {
    let Some(exception) = exception.cell() else {
        return Outcome::failure(Trap::NullExnref);
    };
    instance.engine().host().throw(instance, exception);
    Outcome::failure(Thrown)
}

#[native_abi]
pub(super) unsafe fn take(instance: &Instance) -> Outcome {
    let exception = instance.engine().host().take_exception(instance);
    Outcome::value(exception.address() as u64)
}
