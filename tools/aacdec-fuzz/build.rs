//! Builds opencore-aacdec, the bench shim and the fuzz driver for the host,
//! with ASan and UBSan, for the regression tests (t-169, ADR-0028).
//!
//! The Apache-2.0 sources are fetched and patched into
//! `target/opencore-aacdec` by `just opencore-aacdec`; they are never
//! committed; `THEO_OPENCORE_DIR` points to another tree, e.g. unpatched to
//! see a regression test fail. The host C++ compiler is `$CXX`, else `c++`:
//! Apple's clang has both sanitizers (libFuzzer itself is only needed by
//! `just aacdec-fuzz`).

use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
    thread,
};

/// Same defines as the firmware build (`opencore` in the root `build.rs`)
/// and `defines` in `build-fuzzer.sh`.
const DEFINES: &[&str] = &[
    "-DAAC_PLUS",
    "-DHQ_SBR",
    "-DPARAMETRICSTEREO",
    "-DOSCL_IMPORT_REF=",
    "-DOSCL_EXPORT_REF=",
    "-DOSCL_UNUSED_ARG(x)=(void)(x)",
];

/// Sanitizers; must match `sanitize` in `build-fuzzer.sh`. Negative left
/// shifts (`shift-base`) are this fixed-point code's idiom: defined in C++20
/// and two's complement in GCC, which builds the firmware. Signed overflow
/// wraps (`-fwrapv`, as in the firmware build, ADR-0028).
const SANITIZE: &[&str] = &[
    "-fsanitize=address,undefined",
    "-fno-sanitize-recover=undefined",
    "-fno-sanitize=shift-base",
    "-fno-sanitize=signed-integer-overflow",
];

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let root = manifest.join("../..");
    // Another tree (e.g. unpatched) shows a regression input crashing it.
    let source = env::var_os("THEO_OPENCORE_DIR")
        .map_or_else(|| root.join("target/opencore-aacdec"), PathBuf::from);
    let harness = manifest.join("harness");
    let shim = root.join("tools/audio-bench/shim/theo_opencore.cpp");
    for path in [&source, &harness, &shim] {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rerun-if-env-changed=CXX");
    println!("cargo:rerun-if-env-changed=THEO_OPENCORE_DIR");

    let mut sources: Vec<PathBuf> = std::fs::read_dir(&source)
        .expect("run `just opencore-aacdec` to fetch and patch opencore-aacdec")
        .map(|entry| entry.expect("readable source directory").path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "cpp")
                && path.file_name().is_some_and(|name| name != "SoftAAC.cpp")
        })
        .collect();
    sources.sort();
    sources.push(shim);
    sources.push(harness.join("driver.cpp"));

    let out = PathBuf::from(env::var("OUT_DIR").expect("set by cargo"));
    let cxx = env::var("CXX").unwrap_or_else(|_| "c++".into());
    let workers = thread::available_parallelism().map_or(4, usize::from);
    let objects: Vec<PathBuf> = thread::scope(|scope| {
        let chunks: Vec<_> = sources
            .chunks(sources.len().div_ceil(workers))
            .map(|chunk| {
                let (cxx, source, out) = (&cxx, &source, &out);
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|file| compile(cxx, source, file, out))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        chunks
            .into_iter()
            .flat_map(|chunk| chunk.join().expect("compiler thread"))
            .collect()
    });

    let archive = out.join("libaacdec_fuzz.a");
    let _ = std::fs::remove_file(&archive);
    let status = Command::new("ar")
        .arg("crs")
        .arg(&archive)
        .args(&objects)
        .status()
        .expect("ar on PATH");
    assert!(status.success(), "archiving opencore-aacdec");
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=aacdec_fuzz");
    // rustc links with -nodefaultlibs, which drops the sanitizer runtimes
    // the compiler driver would add: link the compiler's own ones by path.
    let apple = env::var("CARGO_CFG_TARGET_VENDOR").as_deref() == Ok("apple");
    let runtimes: &[&str] = if apple {
        // Apple's ASan runtime carries the UBSan handlers too.
        &["libclang_rt.asan_osx_dynamic.dylib"]
    } else {
        &["libasan.so", "libubsan.so"]
    };
    for runtime in runtimes {
        let path = runtime_path(&cxx, runtime);
        let dir = path.parent().expect("runtime directory");
        println!("cargo:rustc-link-arg={}", path.display());
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", dir.display());
    }
    println!(
        "cargo:rustc-link-lib=dylib={}",
        if apple { "c++" } else { "stdc++" }
    );
}

/// Asks the compiler where one of its runtime libraries is.
fn runtime_path(cxx: &str, name: &str) -> PathBuf {
    let output = Command::new(cxx)
        .arg(format!("-print-file-name={name}"))
        .output()
        .unwrap_or_else(|error| panic!("running {cxx}: {error}"));
    let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    assert!(
        path.is_absolute() && path.exists(),
        "{cxx} has no {name}: the regression tests need its ASan/UBSan runtime"
    );
    path
}

fn compile(cxx: &str, include: &Path, source: &Path, out: &Path) -> PathBuf {
    let stem = source.file_stem().expect("source file").to_string_lossy();
    let object = out.join(format!("{stem}.o"));
    let status = Command::new(cxx)
        .args(["-g", "-O1", "-fno-omit-frame-pointer", "-fwrapv"])
        .args(["-fno-exceptions", "-fno-rtti", "-Wno-narrowing", "-w"])
        .args(DEFINES)
        .args(SANITIZE)
        .arg(format!("-I{}", include.display()))
        .arg("-c")
        .arg(source)
        .arg("-o")
        .arg(&object)
        .status()
        .unwrap_or_else(|error| panic!("running {cxx}: {error}"));
    assert!(status.success(), "compiling {}", source.display());
    object
}
