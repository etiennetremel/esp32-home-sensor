use std::{env, error::Error, fs, path::Path};

use serde::Deserialize;

#[derive(Deserialize)]
struct RawConfig {
    wifi_ssid: String,
    wifi_psk: String,
    hostname: String,
    location: String,
    mqtt_hostname: String,
    mqtt_port: u16,
    mqtt_username: String,
    mqtt_password: String,
    mqtt_topic: String,
    tls_ca: Option<String>,
    tls_cert: Option<String>,
    tls_key: Option<String>,
    measurement_interval_seconds: u16,
}

fn main() -> Result<(), Box<dyn Error>> {
    // Tell Cargo to rerun if toml changes
    println!("cargo:rerun-if-changed=cfg.toml");

    // Read and parse
    let toml_str = fs::read_to_string("cfg.toml")?;
    let raw: RawConfig = toml::from_str(&toml_str)?;

    // Generate Rust code
    let out_dir = env::var("OUT_DIR")?;
    let dest_path = Path::new(&out_dir).join("config.rs");
    let code = format!(
        r#"
        pub const CONFIG: Config = Config {{
            wifi_ssid: {ssid:?},
            wifi_psk: {psk:?},
            hostname: {host:?},
            location: {loc:?},
            mqtt_hostname: {mh:?},
            mqtt_port: {mp},
            mqtt_username: {mu:?},
            mqtt_password: {mpw:?},
            mqtt_topic: {mt:?},
            tls_ca: {ca:?},
            tls_cert: {cert:?},
            tls_key: {key:?},
            measurement_interval_seconds: {intv},
        }};
    "#,
        ssid = raw.wifi_ssid,
        psk = raw.wifi_psk,
        host = raw.hostname,
        loc = raw.location,
        mh = raw.mqtt_hostname,
        mp = raw.mqtt_port,
        mu = raw.mqtt_username,
        mpw = raw.mqtt_password,
        mt = raw.mqtt_topic,
        ca = raw.tls_ca,
        cert = raw.tls_cert,
        key = raw.tls_key,
        intv = raw.measurement_interval_seconds
    );

    let out_dir = env::var("OUT_DIR")?;
    println!("cargo:warning=OUT_DIR={}", out_dir);

    fs::write(dest_path, code)?;
    Ok(())
}
