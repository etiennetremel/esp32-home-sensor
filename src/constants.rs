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

/// Timeout for WiFi connection attempts during reconnection
pub const WIFI_CONNECT_TIMEOUT_SECS: u64 = 10;

/// Delay between WiFi reconnection attempts
pub const WIFI_RECONNECT_DELAY_MS: u64 = 5000;

/// Timeout for initial WiFi connection at boot (longer than reconnect to allow
/// for slower network initialization)
pub const WIFI_INITIAL_CONNECT_TIMEOUT_SECS: u64 = 20;

/// TCP socket timeout for network operations (reads/writes)
pub const TCP_SOCKET_TIMEOUT_SECS: u64 = 30;

/// Delay after MQTT disconnect to allow socket cleanup before next connection
pub const MQTT_DISCONNECT_CLEANUP_DELAY_MS: u64 = 100;

/// Maximum number of MQTT topic subscriptions the client can manage
pub const MQTT_MAX_SUBSCRIBES: usize = 5;
/// Maximum number of QoS 1/2 messages the client can receive concurrently
pub const MQTT_RECEIVE_MAXIMUM: usize = 1;
/// Maximum number of QoS 1/2 messages the client can send concurrently
pub const MQTT_SEND_MAXIMUM: usize = 1;
