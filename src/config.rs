pub struct Config {
    // Device ID (used as DHCP hostname and passed to the OTA for firmware identification)
    pub device_id: &'static str,

    // Location identifier (used in MQTT payloads)
    pub location: &'static str,

    // Measurement interval in seconds
    pub measurement_interval_seconds: u16,

    // MQTT broker hostname or IP address
    pub mqtt_hostname: &'static str,

    // MQTT password for authentication
    pub mqtt_password: &'static str,

    // MQTT port (usually 1883 or 8883 for TLS)
    pub mqtt_port: u16,

    // MQTT topic to publish sensor data to
    pub mqtt_topic: &'static str,

    // MQTT username for authentication
    pub mqtt_username: &'static str,

    // OTA server hostname (optional)
    pub ota_hostname: Option<&'static str>,

    // OTA server port
    pub ota_port: Option<u16>,

    // TLS CA certificate (optional)
    pub tls_ca: Option<&'static str>,

    // TLS client certificate (optional)
    pub tls_cert: Option<&'static str>,

    // TLS private key for client auth (optional)
    pub tls_key: Option<&'static str>,

    // Wi-Fi pre-shared key (password)
    pub wifi_psk: &'static str,

    // Wi-Fi SSID to connect to
    pub wifi_ssid: &'static str,
}

// config values are generated at compile time
include!(concat!(env!("OUT_DIR"), "/config.rs"));
