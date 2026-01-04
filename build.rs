use std::{env, error::Error, fs, path::Path};

use serde::Deserialize;

#[derive(Deserialize)]
struct RawConfig {
    device_id: String,
    location: String,
    measurement_interval_seconds: u16,
    mqtt_hostname: String,
    mqtt_password: String,
    mqtt_port: u16,
    mqtt_topic: String,
    mqtt_username: String,
    ota_hostname: Option<String>,
    ota_port: Option<u16>,
    tls_ca: Option<String>,
    tls_cert: Option<String>,
    tls_key: Option<String>,
    wifi_psk: String,
    wifi_ssid: String,
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
        r"
        pub const CONFIG: Config = Config {{
            device_id: {id:?},
            location: {loc:?},
            measurement_interval_seconds: {intv},
            mqtt_hostname: {mh:?},
            mqtt_password: {mpw:?},
            mqtt_port: {mp},
            mqtt_topic: {mt:?},
            mqtt_username: {mu:?},
            ota_hostname: {oh:?},
            ota_port: {op:?},
            tls_ca: {ca:?},
            tls_cert: {cert:?},
            tls_key: {key:?},
            wifi_psk: {psk:?},
            wifi_ssid: {ssid:?},
        }};
    ",
        ca = raw.tls_ca,
        cert = raw.tls_cert,
        id = raw.device_id,
        intv = raw.measurement_interval_seconds,
        key = raw.tls_key,
        loc = raw.location,
        mh = raw.mqtt_hostname,
        mp = raw.mqtt_port,
        mpw = raw.mqtt_password,
        mt = raw.mqtt_topic,
        mu = raw.mqtt_username,
        oh = raw.ota_hostname,
        op = raw.ota_port,
        psk = raw.wifi_psk,
        ssid = raw.wifi_ssid,
    );

    let out_dir = env::var("OUT_DIR")?;
    println!("cargo:warning=OUT_DIR={out_dir}");

    fs::write(dest_path, code)?;
    Ok(())
}
