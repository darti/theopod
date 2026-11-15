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

/// Builds Helix AAC (LC, no SBR) for `audio-bench` (t-164, ADR-0026).
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
    let mut sources: Vec<_> = std::fs::read_dir(helix.join("libhelix-aac"))
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
    sources.push(shim.join("theo_helix.c"));
    let mut objects = Vec::new();
    for source in &sources {
        let object = out.join(source.file_stem().unwrap()).with_extension("o");
        let status = std::process::Command::new("xtensa-esp32s3-elf-gcc")
            .args([
                "-O2",
                "-mlongcalls",
                "-ffreestanding",
                "-ffunction-sections",
            ])
            .args(["-DESP32", "-DUSE_DEFAULT_STDLIB"])
            .arg(format!("-I{}", shim.display()))
            .arg(format!("-I{}", helix.join("libhelix-aac").display()))
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
    let archive = out.join("libhelix_aac.a");
    let _ = std::fs::remove_file(&archive);
    let status = std::process::Command::new("xtensa-esp32s3-elf-ar")
        .arg("crs")
        .arg(&archive)
        .args(&objects)
        .status()
        .expect("xtensa-esp32s3-elf-ar on PATH");
    assert!(status.success(), "archiving Helix");
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=helix_aac");
}
