//! Settings of the process. They are fixed when the first `Vm` is made.

use core::fmt::Display;
use std::io::Write;
use std::sync::OnceLock;

use bun_engine_platform::machine;

macro_rules! options {
    ($($name:ident / $external:ident: $type:ty = $default:expr,)*) => {
        const COUNT: usize = [$(stringify!($external),)*].len();

        #[derive(Clone)]
        #[repr(C)]
        pub struct Options {
            $(pub $name: $type,)*
            /// By name, that is.
            were_set: [bool; COUNT],
        }

        impl Default for Options {
            fn default() -> Options {
                Options { $($name: $default,)* were_set: [false; COUNT] }
            }
        }

        impl Options {
            fn settings(&mut self) -> [(&'static str, &mut dyn Setting); COUNT] {
                [$((stringify!($external), &mut self.$name),)*]
            }

            /// The name and the value of each.
            pub fn all(&self) -> impl Iterator<Item = (&'static str, crate::Value)> {
                [$((stringify!($external), self.$name.into()),)*].into_iter()
            }
        }
    };
}

trait Setting: Display {
    /// False if the text does not suit.
    fn parse(&mut self, text: &[u8]) -> bool;
}

pub fn parse_flag(text: &[u8]) -> Option<bool> {
    let is_any_of = |words: [&[u8]; 3]| words.iter().any(|it| it.eq_ignore_ascii_case(text));
    if is_any_of([b"true", b"yes", b"1"]) {
        Some(true)
    } else {
        is_any_of([b"false", b"no", b"0"]).then_some(false)
    }
}

/// The number that the text starts with.
pub fn parse_number(text: &[u8]) -> Option<f64> {
    let text = text.trim_ascii_start();
    (1..=text.len())
        .rev()
        .find_map(|length| core::str::from_utf8(&text[..length]).ok()?.parse().ok())
}

/// The integer that the text starts with.
fn parse_integer(text: &[u8]) -> Option<i128> {
    let text = text.trim_ascii_start();
    let sign = usize::from(matches!(text.first(), Some(b'+' | b'-')));
    let digits = text[sign..].iter().take_while(|it| it.is_ascii_digit());
    let integer = &text[..sign + digits.count()];
    core::str::from_utf8(integer).ok()?.parse().ok()
}

macro_rules! settings {
    ($($type:ty = $parse:ident,)*) => {$(
        impl Setting for $type {
            fn parse(&mut self, text: &[u8]) -> bool {
                $parse(text).map(|value| *self = value as $type).is_some()
            }
        }
    )*};
}

settings! {
    bool = parse_flag,
    i32 = parse_integer,
    u32 = parse_integer,
    usize = parse_integer,
    f64 = parse_number,
}

options! {
    dump_options / dumpOptions: u32 = 0,
    use_jit / useJIT: bool = true,
    use_baseline_jit / useBaselineJIT: bool = true,
    use_quick_optimizer / useDFGJIT: bool = false,
    use_full_optimizer / useFTLJIT: bool = false,
    use_regexp_jit / useRegExpJIT: bool = true,
    use_eager_regexp_jit / useEagerRegExpJIT: bool = false,
    max_regexp_code_size / maximumRegExpJITCodeSize: usize = 16 << 20,
    max_regexp_stack_size / maxRegExpStackSize: usize = 192 << 20,
    use_shared_regexp_literals / useSharedRegExpLiteralObjects: bool = true,
    threshold_for_replace_cache / thresholdForStringReplaceCache: usize = 4096,
    use_concurrent_jit / useConcurrentJIT: bool = true,
    jit_policy_scale / jitPolicyScale: f64 = 1.0,
    force_eager_compilation / forceEagerCompilation: bool = false,
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
    use_shared_array_buffer / alwaysUseSharedArrayBuffer: bool = true,
    use_async_stack_trace / useAsyncStackTrace: bool = true,
    use_tail_calls / useTailCalls: bool = true,
    use_code_cache / useCodeCache: bool = true,
    use_source_cache / useSourceProviderCache: bool = true,
    log_bytecode_cache / verboseDiskCache: bool = false,
    log_module_loading / dumpModuleLoadingState: bool = false,
    runs_only_static_code / disallowNonStaticCode: bool = false,
    eval_mode / evalMode: bool = false,
    show_private_scripts_in_stack_traces / showPrivateScriptsInStackTraces: bool = false,
    default_error_stack_trace_limit / defaultErrorStackTraceLimit: u32 = 100,
    exception_stack_trace_limit / exceptionStackTraceLimit: u32 = 100,
    max_per_thread_stack_usage / maxPerThreadStackUsage: usize = 5 << 20,
    soft_reserved_zone_size / softReservedZoneSize: usize = 128 << 10,
    reserved_zone_size / reservedZoneSize: usize = if cfg!(bun_asan) { 192 << 10 } else { 64 << 10 },
    use_gc / useGC: bool = true,
    use_generational_gc / useGenerationalGC: bool = true,
    use_concurrent_gc / useConcurrentGC: bool = true,
    number_of_gc_markers / numberOfGCMarkers: u32 = machine::processor_count().min(8),
    sweep_synchronously / sweepSynchronously: bool = false,
    use_zombie_mode / useZombieMode: bool = false,
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
    critical_gc_memory_threshold / criticalGCMemoryThreshold: f64 = 0.8,
    eden_timer_share_per_megabyte / percentCPUPerMBForEdenTimer: f64 = 0.0025,
    full_timer_share_per_megabyte / percentCPUPerMBForFullTimer: f64 = 0.000_312_5,
    collection_timer_max_share / collectionTimerMaxPercentCPU: f64 = 0.1,
    discard_unused_pages / decommitUnusedMarkedBlockPages: bool = true,
    use_warm_blocks / useWarmUpMarkedBlocks: bool = true,
    shape_range_kilobytes / structureHeapSizeInKB: usize = 0,
    collect_every_n_allocations / slowPathAllocsBetweenGCs: u32 = 0,
    use_only_eden_collections_under_stress / useEdenCollectionsAtSlowPathAllocs: bool = false,
    collect_continuously / collectContinuously: bool = false,
    scribble_free_cells / scribbleFreeCells: bool = cfg!(debug_assertions),
    initial_heap_version / initialHeapVersion: u32 = 0,
    verify_gc / verifyGC: bool = false,
    use_sampling_profiler / useSamplingProfiler: bool = false,
    sample_interval / sampleInterval: u32 = 1000,
    collect_coverage / useControlFlowProfiler: bool = false,
    validate_exception_checks / validateExceptionChecks: bool = false,
    dump_simulated_throws / dumpSimulatedThrows: bool = false,
    log_gc / logGC: bool = false,
    dump_bytecode / dumpGeneratedBytecodes: bool = false,
    dump_disassembly / dumpDisassembly: bool = false,
    validate_bytecode / validateBytecode: bool = cfg!(debug_assertions),
}

impl Options {
    /// False if there is no such option or the value does not suit it.
    pub fn set(&mut self, name: &[u8], value: &[u8]) -> bool {
        let found = (self.settings().into_iter().enumerate())
            .find(|(_, (it, _))| it.as_bytes().eq_ignore_ascii_case(name));
        let set = found.and_then(|(index, (_, setting))| setting.parse(value).then_some(index));
        set.map(|index| self.were_set[index] = true).is_some()
    }

    /// With what follows from other options and from the machine.
    fn settle(&mut self) {
        const MIN_RESERVED_ZONE_SIZE: usize = 16 << 10;

        if self.ram_size == 0 {
            self.ram_size = machine::ram_size();
        }
        self.number_of_gc_markers = self.number_of_gc_markers.max(1);
        self.sweep_synchronously |= self.use_zombie_mode;
        self.reserved_zone_size = self.reserved_zone_size.max(MIN_RESERVED_ZONE_SIZE);
        self.soft_reserved_zone_size = (self.soft_reserved_zone_size).max(
            self.reserved_zone_size
                .saturating_add(MIN_RESERVED_ZONE_SIZE),
        );

        self.use_jit = self.use_jit && bun_engine_exec_memory::is_available();
        for tier in [
            &mut self.use_baseline_jit,
            &mut self.use_quick_optimizer,
            &mut self.use_full_optimizer,
            &mut self.use_regexp_jit,
            &mut self.use_concurrent_jit,
            &mut self.use_wasm_baseline_jit,
            &mut self.use_wasm_optimizer,
        ] {
            *tier &= self.use_jit;
        }
        for part in [
            &mut self.use_wasm_interpreter,
            &mut self.use_wasm_baseline_jit,
            &mut self.use_wasm_optimizer,
            &mut self.use_wasm_fast_memory,
        ] {
            *part &= self.use_wasm;
        }

        self.jit_policy_scale = self.jit_policy_scale.clamp(0.0, 1.0);
        let scale = self.jit_policy_scale;
        for (threshold, min, eager) in [
            (&mut self.threshold_for_baseline, 0, 10),
            (&mut self.threshold_for_baseline_soon, 0, 10),
            (&mut self.threshold_for_quick_optimizer, 1, 20),
            (&mut self.threshold_for_full_optimizer, 2, 20),
        ] {
            if self.force_eager_compilation {
                *threshold = eager;
            } else if scale != 1.0 {
                *threshold = ((f64::from(*threshold) * scale) as i32).max(min);
            }
        }
        self.use_concurrent_jit &= !self.force_eager_compilation;
    }

    /// One line for each option, or for each that was set by name, as it would take effect.
    pub fn dump(&self, out: &mut dyn Write, only_set: bool) -> std::io::Result<()> {
        let (mut options, mut defaults) = (self.clone(), Options::default());
        options.settle();
        let pairs = options.settings().into_iter().zip(defaults.settings());
        for (((name, value), (_, default)), was_set) in pairs.zip(self.were_set) {
            if was_set {
                writeln!(out, "   {name}={value} (default: {default})")?;
            } else if !only_set {
                writeln!(out, "   {name}={value}")?;
            }
        }
        Ok(())
    }
}

static OPTIONS: OnceLock<Options> = OnceLock::new();

/// Does nothing the second time.
pub fn set_options(mut options: Options) {
    options.settle();
    let _ = OPTIONS.set(options);
}

#[inline(always)]
pub fn options() -> &'static Options {
    OPTIONS.get().expect("set before the first Vm is made")
}
