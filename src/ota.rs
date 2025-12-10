use alloc::format;
use core::sync::atomic::{AtomicPtr, Ordering};
use embassy_net::Stack;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embassy_time::Timer;
use embedded_io_async::{Read, Write};
use embedded_storage::nor_flash::NorFlash;
use esp_bootloader_esp_idf::{
    ota::OtaImageState, ota_updater::OtaUpdater, partitions::PARTITION_TABLE_MAX_LEN,
};
use esp_storage::FlashStorage;
use rand_chacha::ChaCha20Rng;
use static_cell::StaticCell;

use crate::config::CONFIG;
use crate::constants::*;
use crate::transport::Transport;

/// Static buffer for OTA partition table operations to avoid heap allocation.
/// This is shared across all OTA operations since they're serialized.
static OTA_TABLE_BUFFER: StaticCell<[u8; PARTITION_TABLE_MAX_LEN]> = StaticCell::new();
static OTA_TABLE_PTR: AtomicPtr<[u8; PARTITION_TABLE_MAX_LEN]> =
    AtomicPtr::new(core::ptr::null_mut());

#[derive(Debug)]
pub enum Error {
    Connection, // Unified connection errors
    Firmware,   // Unified firmware errors
    Info,       // Unified info errors
    Ota,        // Unified OTA errors
    Config,     // Configuration errors
}

pub struct Ota<'a> {
    stack: &'static Mutex<NoopRawMutex, Stack<'static>>,
    rng: ChaCha20Rng,
    rx_buf: &'static Mutex<NoopRawMutex, [u8; RX_BUFFER_SIZE]>,
    tx_buf: &'static Mutex<NoopRawMutex, [u8; TX_BUFFER_SIZE]>,
    tls_read_buf: &'static Mutex<NoopRawMutex, [u8; TLS_BUFFER_MAX]>,
    tls_write_buf: &'static Mutex<NoopRawMutex, [u8; TLS_BUFFER_MAX]>,
    device_id: &'static str,
    ota_hostname: &'static str,
    ota_port: u16,
    flash: FlashStorage<'a>,
}

impl<'a> Ota<'a> {
    pub fn new(
        stack: &'static Mutex<NoopRawMutex, Stack<'static>>,
        rng: ChaCha20Rng,
        rx_buf: &'static Mutex<NoopRawMutex, [u8; RX_BUFFER_SIZE]>,
        tx_buf: &'static Mutex<NoopRawMutex, [u8; TX_BUFFER_SIZE]>,
        tls_read_buf: &'static Mutex<NoopRawMutex, [u8; TLS_BUFFER_MAX]>,
        tls_write_buf: &'static Mutex<NoopRawMutex, [u8; TLS_BUFFER_MAX]>,
        mut flash: FlashStorage<'a>,
    ) -> Result<Self, Error> {
        // Initialize static buffer for OTA operations
        let table_buffer = OTA_TABLE_BUFFER.init([0u8; PARTITION_TABLE_MAX_LEN]);
        OTA_TABLE_PTR.store(table_buffer as *mut _, Ordering::Release);

        // Mark app valid on startup
        let mut ota = OtaUpdater::new(&mut flash, table_buffer).map_err(|_| Error::Ota)?;
        ota.set_current_ota_state(OtaImageState::Valid).ok();

        let device_id = CONFIG.device_id;
        let ota_hostname = CONFIG.ota_hostname.ok_or(Error::Config)?;
        let ota_port = CONFIG.ota_port.ok_or(Error::Config)?;

        Ok(Self {
            stack,
            rng,
            rx_buf,
            tx_buf,
            tls_read_buf,
            tls_write_buf,
            device_id,
            ota_hostname,
            ota_port,
            flash,
        })
    }

    pub async fn check(&mut self) -> Result<(), Error> {
        let stack_guard = self.stack.lock().await;
        let mut rx_buf = self.rx_buf.lock().await;
        let mut tx_buf = self.tx_buf.lock().await;
        let mut tls_read_buf = self.tls_read_buf.lock().await;
        let mut tls_write_buf = self.tls_write_buf.lock().await;

        // Create transport session (stack-allocated, no Box needed)
        let mut session = Transport::new(
            *stack_guard,
            &mut self.rng,
            &mut *rx_buf,
            &mut *tx_buf,
            &mut *tls_read_buf,
            &mut *tls_write_buf,
            self.ota_hostname,
            self.ota_port,
        )
        .await
        .map_err(|e| {
            log::error!("OTA transport connection failed: {:?}", e);
            Error::Connection
        })?;

        // Use a static string slice for the HTTP request template (optimized
        // for memory)
        const INFO_REQ_PREFIX: &str = "\r\nGET /version?device=";
        const REQ_PREFIX: &str = " HTTP/1.1\r\nHost: ";
        const REQ_SUFFIX: &str = "\r\nConnection: close\r\n\r\n";

        // Write request in chunks to avoid dynamic allocation
        session
            .write_all(INFO_REQ_PREFIX.as_bytes())
            .await
            .map_err(|_| Error::Connection)?;
        session
            .write_all(self.device_id.as_bytes())
            .await
            .map_err(|_| Error::Connection)?;
        session
            .write_all(REQ_PREFIX.as_bytes())
            .await
            .map_err(|_| Error::Connection)?;
        session
            .write_all(self.ota_hostname.as_bytes())
            .await
            .map_err(|_| Error::Connection)?;
        session
            .write_all(REQ_SUFFIX.as_bytes())
            .await
            .map_err(|_| Error::Connection)?;

        let mut buf = [0u8; 256];
        let mut total_read = 0;
        let mut body_start = None;

        // Read and parse HTTP response
        loop {
            let n = session
                .read(&mut buf[total_read..])
                .await
                .map_err(|_| Error::Info)?;

            if n == 0 {
                break;
            }

            total_read += n;
            if let Some(pos) = find_header_end(&buf[..total_read]) {
                body_start = Some(pos);
                break;
            }

            if total_read == buf.len() {
                return Err(Error::Info);
            }
        }

        let body_start = body_start.ok_or(Error::Info)?;
        let info_body = &buf[body_start..total_read];

        // Parse version, CRC and size
        let mut lines = info_body.split(|&b| b == b'\n');

        // Version check with current version defined in Cargo package
        let remote_version_str =
            core::str::from_utf8(lines.next().ok_or(Error::Info)?).map_err(|_| Error::Info)?;
        let remote_version_str = remote_version_str.trim();

        // Parse both versions as semver
        let current_version = SemVer::parse(VERSION).ok_or_else(|| {
            log::error!("Failed to parse current version '{}' as semver", VERSION);
            Error::Info
        })?;

        let remote_version = SemVer::parse(remote_version_str).ok_or_else(|| {
            log::error!(
                "Failed to parse remote version '{}' as semver",
                remote_version_str
            );
            Error::Info
        })?;

        // Only update if remote version is strictly greater
        if !remote_version.is_greater_than(&current_version) {
            if remote_version == current_version {
                log::info!(
                    "Already running latest version {}. Skipping update.",
                    VERSION
                );
            } else {
                log::info!(
                    "Remote version {} is not newer than current {}. Skipping update.",
                    remote_version_str,
                    VERSION
                );
            }
            return Ok(());
        }

        // We only continue if an update is needed
        let _crc32 = parse_number::<u32>(lines.next().ok_or(Error::Info)?)?;
        let size = parse_number::<usize>(lines.next().ok_or(Error::Info)?)?;

        log::info!(
            "OTA: upgrading from {} to {} (size={} bytes)",
            VERSION,
            remote_version_str,
            size
        );

        // Drop session and create a new one for firmware download
        drop(session);

        let mut session = Transport::new(
            *stack_guard,
            &mut self.rng,
            &mut *rx_buf,
            &mut *tx_buf,
            &mut *tls_read_buf,
            &mut *tls_write_buf,
            self.ota_hostname,
            self.ota_port,
        )
        .await
        .map_err(|e| {
            log::error!("OTA firmware transport connection failed: {:?}", e);
            Error::Connection
        })?;

        // Same approach for firmware request
        const FIRMWARE_REQ_PREFIX: &str = "GET /firmware?device=";

        session
            .write_all(FIRMWARE_REQ_PREFIX.as_bytes())
            .await
            .map_err(|_| Error::Firmware)?;
        session
            .write_all(self.device_id.as_bytes())
            .await
            .map_err(|_| Error::Connection)?;
        session
            .write_all(REQ_PREFIX.as_bytes())
            .await
            .map_err(|_| Error::Connection)?;
        session
            .write_all(self.ota_hostname.as_bytes())
            .await
            .map_err(|_| Error::Connection)?;
        session
            .write_all(REQ_SUFFIX.as_bytes())
            .await
            .map_err(|_| Error::Connection)?;

        let mut buf = [0u8; 256];
        let mut total_read = 0;
        let mut body_start = None;

        // Extract HTTP headers
        loop {
            let n = session
                .read(&mut buf[total_read..])
                .await
                .map_err(|_| Error::Firmware)?;
            if n == 0 {
                break;
            }

            total_read += n;
            if let Some(pos) = find_header_end(&buf[..total_read]) {
                body_start = Some(pos);
                break;
            }

            if total_read == buf.len() {
                return Err(Error::Firmware);
            }
        }

        let body_start = body_start.ok_or(Error::Firmware)?;

        // Initialize OTA using the static table buffer
        // SAFETY: OTA_TABLE_PTR was set in `new()` and OTA operations are serialized
        let table_buffer = unsafe { &mut *OTA_TABLE_PTR.load(Ordering::Acquire) };
        let mut ota =
            OtaUpdater::new(&mut self.flash, table_buffer).map_err(|_| Error::Ota)?;

        let (mut next_app_partition, _part_type) = ota.next_partition().map_err(|_| Error::Ota)?;

        // Erase the partition in chunks to avoid watchdog timeout
        // Flash erase is blocking and can take several seconds for large partitions
        let erase_len = (size + 4095) & !4095;
        log::info!("Erasing OTA partition ({} bytes)...", erase_len);

        const ERASE_CHUNK_SIZE: u32 = 65536; // 64KB chunks
        let mut erased: u32 = 0;
        while erased < erase_len as u32 {
            let chunk = core::cmp::min(ERASE_CHUNK_SIZE, erase_len as u32 - erased);
            next_app_partition
                .erase(erased, erased + chunk)
                .map_err(|e| {
                    log::error!("Flash erase failed at offset {}: {:?}", erased, e);
                    Error::Ota
                })?;
            erased += chunk;
            // Yield to let the watchdog and WiFi tasks run
            Timer::after(embassy_time::Duration::from_millis(10)).await;
            if erased % (256 * 1024) == 0 {
                log::info!("Erase progress: {}KB / {}KB", erased / 1024, erase_len / 1024);
            }
        }

        let mut bytes_written: usize = 0;

        // ESP32 flash requires 4-byte aligned writes, so we buffer data
        const WRITE_ALIGN: usize = 4;
        let mut write_buf = [0u8; OTA_CHUNK_BUFFER_SIZE];
        let mut write_buf_len: usize = 0;

        // Process leftover bytes from first read into the buffer
        if total_read > body_start {
            let leftover = &buf[body_start..total_read];
            write_buf[..leftover.len()].copy_from_slice(leftover);
            write_buf_len = leftover.len();
            log::info!("OTA: buffered {} leftover bytes from header read", leftover.len());
        }

        // Process firmware in chunks
        let mut chunk_buf = [0u8; OTA_CHUNK_BUFFER_SIZE];
        loop {
            // Only read as much as we have space for in write_buf
            let max_read = OTA_CHUNK_BUFFER_SIZE - write_buf_len;
            let n = session.read(&mut chunk_buf[..max_read]).await;

            match n {
                Ok(0) => {
                    // EOF - flush remaining data with padding
                    if write_buf_len > 0 {
                        let padded_len = (write_buf_len + WRITE_ALIGN - 1) & !(WRITE_ALIGN - 1);
                        // Pad with 0xFF (erased flash state)
                        for i in write_buf_len..padded_len {
                            write_buf[i] = 0xFF;
                        }
                        next_app_partition
                            .write(bytes_written as u32, &write_buf[..padded_len])
                            .map_err(|e| {
                                log::error!("Flash write failed at offset {}: {:?}", bytes_written, e);
                                Error::Ota
                            })?;
                        bytes_written += write_buf_len; // Count actual bytes, not padding
                    }
                    break;
                }
                Ok(bytes_read) => {
                    // Add new data to write buffer
                    write_buf[write_buf_len..write_buf_len + bytes_read]
                        .copy_from_slice(&chunk_buf[..bytes_read]);
                    write_buf_len += bytes_read;

                    // Write aligned portion
                    let aligned_len = write_buf_len & !(WRITE_ALIGN - 1);
                    if aligned_len > 0 {
                        next_app_partition
                            .write(bytes_written as u32, &write_buf[..aligned_len])
                            .map_err(|e| {
                                log::error!("Flash write failed at offset {}: {:?}", bytes_written, e);
                                Error::Ota
                            })?;
                        bytes_written += aligned_len;

                        // Move remaining bytes to start of buffer
                        let remaining = write_buf_len - aligned_len;
                        if remaining > 0 {
                            write_buf.copy_within(aligned_len..write_buf_len, 0);
                        }
                        write_buf_len = remaining;
                    }

                    if size > 0 && bytes_written % (size / 10) < OTA_CHUNK_BUFFER_SIZE {
                        log::info!("Progress: {}%", (bytes_written * 100) / size);
                        // Yield to let other tasks (wifi) run
                        Timer::after(embassy_time::Duration::from_millis(10)).await;
                    }
                }
                Err(e) => {
                    let error_str = format!("{:?}", e);
                    // Treat EOF, connection closed, and similar as successful completion
                    if error_str.contains("Eof")
                        || error_str.contains("EOF")
                        || error_str.contains("ConnectionClosed")
                        || error_str.contains("Closed")
                    {
                        // EOF - flush remaining data with padding
                        if write_buf_len > 0 {
                            let padded_len = (write_buf_len + WRITE_ALIGN - 1) & !(WRITE_ALIGN - 1);
                            for i in write_buf_len..padded_len {
                                write_buf[i] = 0xFF;
                            }
                            next_app_partition
                                .write(bytes_written as u32, &write_buf[..padded_len])
                                .map_err(|e| {
                                    log::error!("Flash write failed at offset {}: {:?}", bytes_written, e);
                                    Error::Ota
                                })?;
                            bytes_written += write_buf_len;
                        }
                        break;
                    } else {
                        log::error!("Error reading firmware chunk: {:?}", e);
                        return Err(Error::Firmware);
                    }
                }
            }
        }

        log::info!(
            "Firmware download complete: {} bytes written (expected {})",
            bytes_written,
            size
        );

        if bytes_written != size {
            log::error!(
                "Size mismatch: wrote {} bytes, expected {}",
                bytes_written,
                size
            );
            return Err(Error::Firmware);
        }

        // Finalize
        ota.activate_next_partition().map_err(|e| {
            log::error!("Failed to activate next partition: {:?}", e);
            Error::Ota
        })?;
        ota.set_current_ota_state(OtaImageState::New).map_err(|e| {
            log::error!("Failed to set OTA state to New: {:?}", e);
            Error::Ota
        })?;

        log::info!("OTA complete. Rebooting...");
        Timer::after(embassy_time::Duration::from_millis(1_000)).await;
        esp_hal::system::software_reset();
    }
}

// Helper function to parse numbers from byte slices
fn parse_number<T: core::str::FromStr>(bytes: &[u8]) -> Result<T, Error> {
    core::str::from_utf8(bytes)
        .map_err(|_| Error::Info)?
        .trim()
        .parse::<T>()
        .map_err(|_| Error::Info)
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|pos| pos + 4)
}

/// Represents a parsed semantic version (major.minor.patch with optional pre-release)
#[derive(Debug, PartialEq, Eq)]
struct SemVer {
    major: u32,
    minor: u32,
    patch: u32,
    /// Pre-release identifier (e.g., "beta.0", "rc.1", "alpha")
    /// None means it's a stable release
    pre_release: Option<PreRelease>,
}

/// Pre-release version component
#[derive(Debug, PartialEq, Eq)]
struct PreRelease {
    /// The type of pre-release (alpha, beta, rc, etc.)
    kind: PreReleaseKind,
    /// Optional numeric suffix (e.g., the "0" in "beta.0")
    number: Option<u32>,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
enum PreReleaseKind {
    Alpha,
    Beta,
    Rc,
    Other, // Unknown pre-release types sort after rc but before stable
}

impl PreRelease {
    fn parse(s: &str) -> Option<Self> {
        // Handle formats like "beta", "beta.0", "beta0", "rc.1", "rc1", "alpha"
        let s = s.to_ascii_lowercase();

        // Try to find a known pre-release kind
        let (kind, remainder) = if s.starts_with("alpha") {
            (PreReleaseKind::Alpha, &s[5..])
        } else if s.starts_with("beta") {
            (PreReleaseKind::Beta, &s[4..])
        } else if s.starts_with("rc") {
            (PreReleaseKind::Rc, &s[2..])
        } else {
            (PreReleaseKind::Other, s.as_str())
        };

        // Parse optional numeric suffix (handles ".0", "0", ".1", "1", etc.)
        let number = if remainder.is_empty() {
            None
        } else {
            let num_str = remainder.trim_start_matches('.');
            num_str.parse().ok()
        };

        Some(PreRelease { kind, number })
    }

    /// Compare pre-releases. Returns Ordering.
    fn cmp(&self, other: &PreRelease) -> core::cmp::Ordering {
        use core::cmp::Ordering;

        // First compare by kind (alpha < beta < rc < other)
        match self.kind.cmp(&other.kind) {
            Ordering::Equal => {}
            ord => return ord,
        }

        // Then by number (None < Some(0) < Some(1) < ...)
        match (&self.number, &other.number) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some(a), Some(b)) => a.cmp(b),
        }
    }
}

impl SemVer {
    /// Parse a semver string like "1.2.3", "v1.2.3", "1.2.3-beta", "v1.2.3-beta.0"
    fn parse(version: &str) -> Option<Self> {
        let version = version.trim();

        // Strip optional 'v' or 'V' prefix
        let version = version
            .strip_prefix('v')
            .or_else(|| version.strip_prefix('V'))
            .unwrap_or(version);

        // Split on '-' to separate version from pre-release
        let (version_part, pre_release_part) = match version.split_once('-') {
            Some((v, p)) => (v, Some(p)),
            None => (version, None),
        };

        let mut parts = version_part.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;

        // Ensure no extra parts in version
        if parts.next().is_some() {
            return None;
        }

        let pre_release = pre_release_part.and_then(PreRelease::parse);

        Some(SemVer {
            major,
            minor,
            patch,
            pre_release,
        })
    }

    /// Returns true if self is greater than other
    fn is_greater_than(&self, other: &SemVer) -> bool {
        use core::cmp::Ordering;

        // Compare major.minor.patch first
        if self.major != other.major {
            return self.major > other.major;
        }
        if self.minor != other.minor {
            return self.minor > other.minor;
        }
        if self.patch != other.patch {
            return self.patch > other.patch;
        }

        // Same major.minor.patch - compare pre-release
        // A stable release (no pre-release) is greater than any pre-release
        match (&self.pre_release, &other.pre_release) {
            (None, None) => false,             // Equal
            (None, Some(_)) => true,           // Stable > pre-release
            (Some(_), None) => false,          // Pre-release < stable
            (Some(a), Some(b)) => a.cmp(b) == Ordering::Greater,
        }
    }
}
