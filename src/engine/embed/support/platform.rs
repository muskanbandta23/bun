use core::ffi::c_int;

use bun_engine_platform::{StackBounds, clock, machine};

#[unsafe(no_mangle)]
extern "C" fn Support__monotonicTime() -> f64 {
    clock::monotonic_seconds()
}

#[unsafe(no_mangle)]
extern "C" fn Support__wallTime() -> f64 {
    clock::wall_seconds()
}

#[unsafe(no_mangle)]
extern "C" fn Support__numberOfProcessorCores() -> c_int {
    machine::processor_count() as c_int
}

#[unsafe(no_mangle)]
extern "C" fn Support__ramSize() -> usize {
    machine::ram_size()
}

#[unsafe(no_mangle)]
extern "C" fn Support__currentStackBounds(origin: &mut usize, end: &mut usize) {
    let bounds = StackBounds::current();
    (*origin, *end) = (bounds.origin, bounds.end);
}
