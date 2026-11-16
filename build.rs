fn main() {
    println!("cargo:rerun-if-changed=config/wifi.local.toml");
    println!("cargo:rerun-if-env-changed=THEO_WIFI_CONFIG");
    if std::env::var_os("CARGO_FEATURE_LOCAL_NETWORK").is_some() {
        let path =
            std::env::var("THEO_WIFI_CONFIG").unwrap_or_else(|_| "config/wifi.local.toml".into());
        println!("cargo:rerun-if-changed={path}");
        let text = std::fs::read_to_string(path)
            .expect("local-network requires ignored config/wifi.local.toml");
        let config: toml::Table = text.parse().expect("valid local Wi-Fi TOML");
        assert!(
            config
                .keys()
                .all(|key| matches!(key.as_str(), "ssid" | "password")),
            "only ssid and password are supported"
        );
        for (key, env, max) in [
            ("ssid", "THEO_WIFI_SSID", 32),
            ("password", "THEO_WIFI_PASSWORD", 63),
        ] {
            let value = config
                .get(key)
                .and_then(toml::Value::as_str)
                .expect("ssid and password are required strings");
            assert!(
                !value.is_empty() && value.len() <= max && !value.contains(['\r', '\n']),
                "invalid local Wi-Fi field"
            );
            if key == "password" {
                assert!(value.len() >= 8, "WPA2 password requires8to63bytes");
            }
            println!("cargo:rustc-env={env}={value}");
        }
    }

    if std::env::var_os("CARGO_FEATURE_AUDIO_BENCH").is_some() {
        audio_bench();
    }

    slint_build::compile_with_config(
        "ui/main.slint",
        slint_build::CompilerConfiguration::new()
            .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer),
    )
    .unwrap();
}

/// Builds Helix AAC (LC, no SBR) and Helix MP3 for `audio-bench` (t-164, ADR-0026).
///
/// The RPSL/RCSL sources are fetched into `target/helix` by `just audio-bench`
/// and never committed until the licence review; the clips stay in
/// `target/audio-bench` (copyrighted audio).
fn audio_bench() {
    let root = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let helix = root.join("target/helix");
    let shim = root.join("tools/audio-bench/shim");
    let clips = root.join("target/audio-bench");
    println!("cargo:rerun-if-changed={}", shim.display());
    println!("cargo:rerun-if-changed={}", clips.display());
    println!("cargo:rustc-env=THEO_AUDIO_CLIPS={}", clips.display());
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let mut objects = Vec::new();
    // Each decoder has its own coder.h/assembly.h: compile each directory
    // with only its own headers; symbols are prefixed (raac_, xmp3_).
    for dir in ["libhelix-aac", "libhelix-mp3"] {
        let mut sources: Vec<_> = std::fs::read_dir(helix.join(dir))
            .expect("run `just audio-bench` to fetch Helix into target/helix")
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.extension().is_some_and(|ext| ext == "c")
                    && !path
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("sbr")
            })
            .collect();
        if dir == "libhelix-aac" {
            sources.push(shim.join("theo_helix.c"));
        }
        for source in &sources {
            let stem = source.file_stem().unwrap().to_string_lossy();
            let object = out.join(format!("{dir}-{stem}.o"));
            let status = std::process::Command::new("xtensa-esp32s3-elf-gcc")
                .args([
                    "-O2",
                    "-mlongcalls",
                    "-ffreestanding",
                    "-ffunction-sections",
                ])
                .args(["-DESP32", "-DUSE_DEFAULT_STDLIB"])
                .arg(format!("-I{}", shim.display()))
                .arg(format!("-I{}", helix.join(dir).display()))
                .arg(format!("-I{}", helix.display()))
                .arg("-c")
                .arg(source)
                .arg("-o")
                .arg(&object)
                .status()
                .expect("xtensa-esp32s3-elf-gcc on PATH (just recipes set it)");
            assert!(status.success(), "compiling {}", source.display());
            objects.push(object);
        }
    }
    let archive = out.join("libhelix.a");
    let _ = std::fs::remove_file(&archive);
    let status = std::process::Command::new("xtensa-esp32s3-elf-ar")
        .arg("crs")
        .arg(&archive)
        .args(&objects)
        .status()
        .expect("xtensa-esp32s3-elf-ar on PATH");
    assert!(status.success(), "archiving Helix");
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=helix");
    opencore(&root, &shim, &out);
}

/// Builds the PacketVideo AAC decoder (opencore-aacdec, Apache-2.0) with
/// HE-AAC v1/v2 for `audio-bench` (t-167).
///
/// `just audio-bench-inputs` fetches it from AOSP into `target/opencore-aacdec`
/// and applies `tools/opencore-aacdec/patches` (t-169, ADR-0027); the Android
/// wrapper `SoftAAC.cpp` is left out, `theo_opencore.cpp` replaces it.
fn opencore(root: &std::path::Path, shim: &std::path::Path, out: &std::path::Path) {
    let dir = root.join("target/opencore-aacdec");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut sources: Vec<_> = std::fs::read_dir(&dir)
        .expect("run `just audio-bench-inputs` to fetch opencore-aacdec")
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "cpp")
                && path.file_name().is_some_and(|name| name != "SoftAAC.cpp")
        })
        .collect();
    sources.push(shim.join("theo_opencore.cpp"));
    let mut objects = Vec::new();
    for source in &sources {
        let stem = source.file_stem().unwrap().to_string_lossy();
        let object = out.join(format!("opencore-{stem}.o"));
        let status = std::process::Command::new("xtensa-esp32s3-elf-g++")
            .args(["-O2", "-mlongcalls", "-ffunction-sections"])
            // The fixed-point code relies on wrapping signed arithmetic
            // (ADR-0027).
            .arg("-fwrapv")
            .args(["-fno-exceptions", "-fno-rtti", "-Wno-narrowing"])
            .args(["-DAAC_PLUS", "-DHQ_SBR", "-DPARAMETRICSTEREO"])
            .args(["-DOSCL_IMPORT_REF=", "-DOSCL_EXPORT_REF="])
            .arg("-DOSCL_UNUSED_ARG(x)=(void)(x)")
            .arg(format!("-I{}", dir.display()))
            .arg("-c")
            .arg(source)
            .arg("-o")
            .arg(&object)
            .status()
            .expect("xtensa-esp32s3-elf-g++ on PATH (just recipes set it)");
        assert!(status.success(), "compiling {}", source.display());
        objects.push(object);
    }
    let archive = out.join("libopencore.a");
    let _ = std::fs::remove_file(&archive);
    let status = std::process::Command::new("xtensa-esp32s3-elf-ar")
        .arg("crs")
        .arg(&archive)
        .args(&objects)
        .status()
        .expect("xtensa-esp32s3-elf-ar on PATH");
    assert!(status.success(), "archiving opencore-aacdec");
    println!("cargo:rustc-link-lib=static=opencore");
}
