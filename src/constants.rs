/// Current firmware version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Size of the TCP socket receive buffer for encrypted data
pub const RX_BUFFER_SIZE: usize = 3072;
/// Size of the TCP socket transmit buffer for encrypted data
pub const TX_BUFFER_SIZE: usize = 3072;

/// Maximum size for TLS processing buffer (for TLS records)
/// embedded-tls 0.17.0 requires at least 16640 bytes for TLS 1.3 handshakes
/// (16384 bytes for TLS record + 256 bytes overhead)
pub const TLS_BUFFER_MAX: usize = 16640;

/// Size of the MQTT client receive buffer for application data
pub const MQTT_RX_BUFFER_SIZE: usize = 1024;
/// Size of the MQTT client transmit buffer for application data
pub const MQTT_TX_BUFFER_SIZE: usize = 1024;

/// Buffer size for OTA firmware update chunks
pub const OTA_CHUNK_BUFFER_SIZE: usize = 2048;

/// Buffer size for UART read operations (for SDS011 sensor)
pub const UART_READ_BUFFER_SIZE: usize = 64;
/// AT command character for UART configuration
pub const UART_AT_CMD: u8 = 0xAB;

/// Interval in seconds between firmware update checks (3600 = 1 hour)
pub const FIRMWARE_CHECK_INTERVAL: u64 = 3600;

/// Watchdog timeout in seconds. Must be long enough to accommodate:
/// - TLS 1.3 handshake (can take 10-20+ seconds on ESP32)
/// - Sensor measurements (SCD30 data ready wait up to 30 seconds)
/// - Network operations during poor connectivity
pub const WATCHDOG_TIMEOUT_SECS: u64 = 60;
