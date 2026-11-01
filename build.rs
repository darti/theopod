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

    slint_build::compile_with_config(
        "ui/main.slint",
        slint_build::CompilerConfiguration::new()
            .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer),
    )
    .unwrap();
}
