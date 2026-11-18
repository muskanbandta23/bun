use core::cell::Cell;

use bun_engine_number::{Random, float16_from_f64};

#[unsafe(no_mangle)]
extern "C" fn Support__doubleToFloat16(value: f64) -> u16 {
    float16_from_f64(value)
}

#[unsafe(no_mangle)]
extern "C" fn Support__WeakRandom__seed(random: &Cell<Random>) -> u32 {
    random.get().seed()
}

#[unsafe(no_mangle)]
extern "C" fn Support__WeakRandom__setSeed(random: &Cell<Random>, seed: u32) {
    random.set(Random::new(seed));
}
