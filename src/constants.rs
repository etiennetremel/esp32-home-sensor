/// Size of the heap allocated for the application (110KB)
pub const HEAP_SIZE: usize = 110 * 1024;

/// Size of the TCP socket receive buffer for encrypted data
pub const RX_BUFFER_SIZE: usize = 2048;
/// Size of the TCP socket transmit buffer for encrypted data
pub const TX_BUFFER_SIZE: usize = 2048;

/// Maximum size for TLS processing buffer (for TLS records)
pub const TLS_BUFFER_MAX: usize = 2048;

/// Size of the MQTT client receive buffer for application data
pub const MQTT_RX_BUFFER_SIZE: usize = 1024;
/// Size of the MQTT client transmit buffer for application data
pub const MQTT_TX_BUFFER_SIZE: usize = 1024;

/// Buffer size for OTA firmware update chunks
pub const OTA_CHUNK_BUFFER_SIZE: usize = 1024;

/// Buffer size for UART read operations (for SDS011 sensor)
pub const UART_READ_BUFFER_SIZE: usize = 64;
/// AT command character for UART configuration
pub const UART_AT_CMD: u8 = 0xAB;

/// Interval in seconds between firmware update checks (3600 = 1 hour)
pub const FIRMWARE_CHECK_INTERVAL: u64 = 3600;
