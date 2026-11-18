#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::disallowed_macros
)]

use std::env;
use std::fs;
use std::path::PathBuf;

const HIGHWAY_SOURCES: [&str; 3] = ["hwy/abort.cc", "hwy/per_target.cc", "hwy/targets.cc"];
const KERNELS: [&str; 2] = ["highway_strings.cpp", "highway_byte_set.cpp"];
const HEADERS: [&str; 2] = ["highway_dispatch.h", "BufferStringSearch.h"];

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let repository = manifest
        .ancestors()
        .nth(3)
        .expect("repository root from CARGO_MANIFEST_DIR");
    let vendor = repository.join("vendor");
    let bindings = repository.join("src/jsc/bindings");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());

    for library in ["highway", "simdutf", "mimalloc"] {
        if !vendor.join(library).exists() {
            println!(
                "cargo:warning=vendor/{library} is missing. Run bun scripts/build.ts --target=clone-{library}"
            );
            return;
        }
    }

    let mut native = cc::Build::new();
    native
        .cpp(true)
        .compiler(env::var("CXX").unwrap_or_else(|_| "clang++".into()))
        .std("c++23")
        .opt_level(2)
        .warnings(false)
        .flag("-fno-exceptions")
        .flag("-fno-omit-frame-pointer")
        .include(&out)
        .include(manifest.join("include"))
        .include(vendor.join("highway"))
        .include(vendor.join("simdutf/include"))
        .include(vendor.join("simdutf/src"))
        .include(vendor.join("mimalloc/include"))
        .define("HWY_STATIC_DEFINE", None)
        .define("HWY_DISABLED_TARGETS", "HWY_ALL_SVE-HWY_SVE2_128")
        .define("MI_STATIC_LIB", None)
        .define("MI_SKIP_COLLECT_ON_EXIT", "1")
        .define("MI_NO_PROCESS_DETACH", "1")
        .define("MI_FREE_USE_PAGEMAP", "1")
        .define("MI_BUILD_RELEASE", None)
        .define("MI_DEFAULT_ALLOW_THP", "0")
        .files(HIGHWAY_SOURCES.map(|source| vendor.join("highway").join(source)))
        .file(vendor.join("simdutf/src/simdutf.cpp"))
        .file(repository.join("src/simdutf_sys/bun-simdutf.cpp"));

    // A kernel includes "root.h", which is looked for beside the source first: the copy finds this crate's.
    for name in KERNELS.into_iter().chain(HEADERS) {
        let source = bindings.join(name);
        println!("cargo:rerun-if-changed={}", source.display());
        fs::copy(&source, out.join(name)).unwrap();
    }
    native.files(KERNELS.map(|name| out.join(name)));

    let allocator = out.join("mimalloc.cpp");
    fs::write(&allocator, "#include \"../src/static.c\"\n").unwrap();
    native.file(allocator);

    native.compile("bun_engine_standalone");
    if env::var("CARGO_CFG_TARGET_OS").unwrap() == "macos" {
        println!("cargo:rustc-link-lib=icucore");
    } else {
        // A static build with `include` and `lib` folders, in place of the system's.
        if let Ok(root) = env::var("BUN_ICU_ROOT") {
            println!("cargo:rustc-link-search=native={root}/lib");
        }
        for library in ["icui18n", "icuuc", "icudata"] {
            println!("cargo:rustc-link-lib={library}");
        }
    }
    println!("cargo:rerun-if-env-changed=BUN_ICU_ROOT");
    println!("cargo:rerun-if-changed=include/root.h");
    println!("cargo:rerun-if-changed=build.rs");
}
