#[toml_cfg::toml_config]
pub struct Config {
    #[default("")]
    wifi_ssid: &'static str,

    #[default("")]
    wifi_psk: &'static str,

    // smoltcp currently doesn't have a way of giving a hostname through DHCP
    #[default("esp32")]
    hostname: &'static str,

    #[default("")]
    location: &'static str,

    #[default("")]
    mqtt_hostname: &'static str,

    #[default(1883)]
    mqtt_port: u16,

    #[default("")]
    mqtt_username: &'static str,

    #[default("")]
    mqtt_password: &'static str,

    #[default("sensor")]
    mqtt_topic: &'static str,

    #[default(60)]
    measurement_interval_seconds: u64,
}
