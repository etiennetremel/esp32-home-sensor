pub struct Config {
    pub wifi_ssid: &'static str,
    pub wifi_psk: &'static str,
    pub hostname: &'static str,
    pub location: &'static str,
    pub mqtt_hostname: &'static str,
    pub mqtt_port: u16,
    pub mqtt_username: &'static str,
    pub mqtt_password: &'static str,
    pub mqtt_topic: &'static str,
    pub tls_ca: Option<&'static str>,
    pub tls_cert: Option<&'static str>,
    pub tls_key: Option<&'static str>,
    pub measurement_interval_seconds: u16,
}

// config values are generated at compile time
include!(concat!(env!("OUT_DIR"), "/config.rs"));
