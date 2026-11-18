//! Settings of the process. They are fixed when the first `Vm` is made.

use std::sync::OnceLock;

use bun_engine_platform::machine;

macro_rules! options {
    ($($name:ident / $external:ident: $type:ty = $default:expr,)*) => {
        #[derive(Clone)]
        #[repr(C)]
        pub struct Options {
            $(pub $name: $type,)*
        }

        impl Default for Options {
            fn default() -> Options {
                Options { $($name: $default,)* }
            }
        }

        impl Options {
            /// False if there is no such option or the value does not suit it.
            pub fn set(&mut self, name: &[u8], value: &[u8]) -> bool {
                $(if name.eq_ignore_ascii_case(stringify!($external).as_bytes()) {
                    return Parse::parse(value).map(|value| self.$name = value).is_some();
                })*
                false
            }
        }
    };
}

trait Parse: Sized {
    fn parse(text: &[u8]) -> Option<Self>;
}

impl Parse for bool {
    fn parse(text: &[u8]) -> Option<bool> {
        let is_any_of = |words: [&[u8]; 3]| words.iter().any(|it| it.eq_ignore_ascii_case(text));
        if is_any_of([b"true", b"yes", b"1"]) {
            Some(true)
        } else {
            is_any_of([b"false", b"no", b"0"]).then_some(false)
        }
    }
}

macro_rules! parse_numbers {
    ($($type:ty)*) => {$(
        impl Parse for $type {
            fn parse(text: &[u8]) -> Option<$type> {
                core::str::from_utf8(text).ok()?.parse().ok()
            }
        }
    )*};
}

parse_numbers!(i32 u32 usize f64);

options! {
    use_jit / useJIT: bool = true,
    use_baseline_jit / useBaselineJIT: bool = true,
    use_quick_optimizer / useDFGJIT: bool = true,
    use_full_optimizer / useFTLJIT: bool = true,
    use_regexp_jit / useRegExpJIT: bool = true,
    use_eager_regexp_jit / useEagerRegExpJIT: bool = false,
    max_regexp_code_size / maximumRegExpJITCodeSize: usize = 16 << 20,
    max_regexp_stack_size / maxRegExpStackSize: usize = 192 << 20,
    use_shared_regexp_literals / useSharedRegExpLiteralObjects: bool = true,
    threshold_for_replace_cache / thresholdForStringReplaceCache: usize = 4096,
    use_concurrent_jit / useConcurrentJIT: bool = true,
    threshold_for_baseline / thresholdForJITAfterWarmUp: i32 = 500,
    threshold_for_baseline_soon / thresholdForJITSoon: i32 = 100,
    threshold_for_quick_optimizer / thresholdForOptimizeAfterWarmUp: i32 = 1000,
    threshold_for_full_optimizer / thresholdForFTLOptimizeAfterWarmUp: i32 = 64000,
    use_wasm / useWasm: bool = true,
    use_wasm_baseline_jit / useBBQJIT: bool = true,
    use_wasm_optimizer / useOMGJIT: bool = true,
    use_wasm_interpreter / useWasmIPInt: bool = true,
    use_wasm_memory64 / useWasmMemory64: bool = false,
    use_wasm_fast_memory / useWasmFastMemory: bool = true,
    use_shadow_realm / useShadowRealm: bool = true,
    use_temporal / useTemporal: bool = true,
    use_async_stack_trace / useAsyncStackTrace: bool = true,
    use_tail_calls / useTailCalls: bool = true,
    use_code_cache / useCodeCache: bool = true,
    eval_mode / evalMode: bool = false,
    show_private_scripts_in_stack_traces / showPrivateScriptsInStackTraces: bool = false,
    default_error_stack_trace_limit / defaultErrorStackTraceLimit: u32 = 100,
    exception_stack_trace_limit / exceptionStackTraceLimit: u32 = 100,
    max_per_thread_stack_usage / maxPerThreadStackUsage: usize = 5 << 20,
    soft_reserved_zone_size / softReservedZoneSize: usize = 128 << 10,
    reserved_zone_size / reservedZoneSize: usize = 64 << 10,
    use_gc / useGC: bool = true,
    use_generational_gc / useGenerationalGC: bool = true,
    use_concurrent_gc / useConcurrentGC: bool = true,
    number_of_gc_markers / numberOfGCMarkers: u32 = machine::processor_count().min(8),
    sweep_synchronously / sweepSynchronously: bool = false,
    force_mini_vm_mode / forceMiniVMMode: bool = false,
    gc_max_heap_size / gcMaxHeapSize: usize = 0,
    ram_size / forceRAMSize: usize = 0,
    large_heap_size / largeHeapSize: usize = 32 << 20,
    medium_heap_size / mediumHeapSize: usize = 4 << 20,
    small_heap_size / smallHeapSize: usize = 1 << 20,
    small_heap_ram_fraction / smallHeapRAMFraction: f64 = 0.25,
    small_heap_growth_factor / smallHeapGrowthFactor: f64 = 2.0,
    medium_heap_ram_fraction / mediumHeapRAMFraction: f64 = 0.5,
    medium_heap_growth_factor / mediumHeapGrowthFactor: f64 = 1.5,
    large_heap_growth_factor / largeHeapGrowthFactor: f64 = 1.24,
    mini_vm_heap_growth_factor / miniVMHeapGrowthFactor: f64 = 1.20,
    heap_growth_steepness_factor / heapGrowthSteepnessFactor: f64 = 2.0,
    heap_growth_max_increase / heapGrowthMaxIncrease: f64 = 3.0,
    min_eden_to_old_generation_ratio / minEdenToOldGenerationRatio: f64 = 1.0 / 3.0,
    collect_every_n_allocations / slowPathAllocsBetweenGCs: u32 = 0,
    log_gc / logGC: bool = false,
    dump_bytecode / dumpGeneratedBytecodes: bool = false,
    dump_disassembly / dumpDisassembly: bool = false,
    validate_bytecode / validateBytecode: bool = cfg!(debug_assertions),
}

static OPTIONS: OnceLock<Options> = OnceLock::new();

/// Does nothing the second time.
pub fn set_options(mut options: Options) {
    if options.ram_size == 0 {
        options.ram_size = machine::ram_size();
    }
    options.number_of_gc_markers = options.number_of_gc_markers.max(1);
    options.use_jit &= bun_engine_exec_memory::is_available();
    let _ = OPTIONS.set(options);
}

#[inline(always)]
pub fn options() -> &'static Options {
    OPTIONS.get().expect("set before the first Vm is made")
}
