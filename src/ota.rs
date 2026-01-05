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
use heapless::String;

use crate::config::CONFIG;
use crate::constants::*;
use crate::transport::Transport;

/// Static buffer for OTA partition table operations to avoid heap allocation.
/// This is shared across all OTA operations since they're serialized through `&mut self`.
static OTA_TABLE_BUFFER: StaticCell<[u8; PARTITION_TABLE_MAX_LEN]> = StaticCell::new();
static OTA_TABLE_PTR: AtomicPtr<[u8; PARTITION_TABLE_MAX_LEN]> =
    AtomicPtr::new(core::ptr::null_mut());

#[derive(Debug)]
pub enum Error {
    Connection, // Network/TLS connection errors
    Firmware,   // Firmware download or validation errors
    Info,       // Version info parsing errors
    Ota,        // Flash/partition operation errors
    Config,     // Configuration errors (missing OTA settings)
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
        // Initialize static buffer for OTA operations. This buffer persists
        // for the lifetime of the program and avoids heap allocation.
        // Check if already initialized to avoid panic on re-entry (e.g. soft reset)
        let ptr = OTA_TABLE_PTR.load(Ordering::Acquire);
        let table_buffer = if ptr.is_null() {
            let buf = OTA_TABLE_BUFFER.init([0u8; PARTITION_TABLE_MAX_LEN]);
            OTA_TABLE_PTR.store(buf as *mut _, Ordering::Release);
            buf
        } else {
            // SAFETY: The pointer is only set once and the buffer is valid for 'static
            unsafe { &mut *ptr }
        };

        // Mark current app as valid on startup. This confirms the boot was
        // successful after an OTA update.
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

    /// Check for and apply firmware updates from the OTA server.
    ///
    /// This method:
    /// 1. Connects to the OTA server over TLS
    /// 2. Queries the current version available for this device
    /// 3. If a newer version exists, downloads and flashes it
    /// 4. Reboots the device to apply the update
    ///
    /// IMPORTANT: This method ensures the TLS session is properly closed on ALL
    /// code paths (success or error) to prevent orphaned TCP sockets from
    /// corrupting the network stack for subsequent connections.
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

        // HTTP request templates as static strings to avoid dynamic allocation.
        // The leading \r\n in INFO_REQ_PREFIX handles potential leftover data.
        const INFO_REQ_PREFIX: &str = "\r\nGET /version?device=";
        const REQ_PREFIX: &str = " HTTP/1.1\r\nHost: ";
        const REQ_SUFFIX_KEEPALIVE: &str = "\r\nConnection: keep-alive\r\n\r\n";
        const REQ_SUFFIX_CLOSE: &str = "\r\nConnection: close\r\n\r\n";

        // Send version check request. Close session on error to prevent
        // orphaned sockets.
        if let Err(e) = self.send_version_request(&mut session, INFO_REQ_PREFIX, REQ_PREFIX, REQ_SUFFIX_KEEPALIVE).await {
            session.close().await;
            return Err(e);
        }

        // Read and parse version info response
        let mut buf = [0u8; 1024];
        let (mut total_read, body_start) = match self.read_http_response(&mut session, &mut buf).await {
            Ok(result) => result,
            Err(e) => {
                session.close().await;
                return Err(e);
            }
        };

        // Ensure we have at least 3 lines of body (Version, CRC, Size)
        // or 2 newlines separating them (Ver\nCRC\nSize...)
        loop {
            let body = &buf[body_start..total_read];
            let newlines = body.iter().filter(|&&b| b == b'\n').count();

            // We need 2 newlines to guarantee we have "Line1\nLine2\n..." which
            // corresponds to "Version\nCRC\nSize(maybe partial)"
            if newlines >= 2 {
                break;
            }

            if total_read == buf.len() {
                // Buffer full but still not enough data
                log::error!("OTA buffer full ({} bytes) while waiting for version info body", total_read);
                session.close().await;
                return Err(Error::Info);
            }

            let n = session.read(&mut buf[total_read..]).await.map_err(|e| {
                log::error!("Failed to read from session: {:?}", e);
                Error::Info
            })?;
            if n == 0 {
                // EOF - if we have some data, we try to parse what we have
                break;
            }
            total_read += n;
        }

        let info_body = &buf[body_start..total_read];
        let mut lines = info_body.split(|&b| b == b'\n');

        // Parse version string from response. Server returns:
        // Line 1: version (e.g., "1.2.3" or "No firmware for device 'xxx'")
        // Line 2: CRC32 checksum
        // Line 3: firmware size in bytes
        let remote_version_str = match lines.next()
            .and_then(|line| core::str::from_utf8(line).ok())
            .map(|s| s.trim())
        {
            Some(v) => v,
            None => {
                log::error!("Failed to parse version response");
                session.close().await;
                return Err(Error::Info);
            }
        };

        // Parse current version from Cargo.toml (VERSION constant)
        let current_version = match SemVer::parse(VERSION) {
            Some(v) => v,
            None => {
                log::error!("Failed to parse current version '{}' as semver", VERSION);
                session.close().await;
                return Err(Error::Info);
            }
        };

        // Parse remote version - may fail if server returns error message
        // instead of a version string
        let remote_version = match SemVer::parse(remote_version_str) {
            Some(v) => v,
            None => {
                log::error!("Failed to parse remote version '{}' as semver", remote_version_str);
                session.close().await;
                return Err(Error::Info);
            }
        };

        // Only update if remote version is strictly greater (handles pre-release correctly)
        if !remote_version.is_greater_than(&current_version) {
            if remote_version == current_version {
                log::info!("Already running latest version {}. Skipping update.", VERSION);
            } else {
                log::info!(
                    "Remote version {} is not newer than current {}. Skipping update.",
                    remote_version_str,
                    VERSION
                );
            }
            session.close().await;
            return Ok(());
        }

        // Parse CRC32 and size for firmware validation
        let _crc32 = match lines.next().and_then(|line| parse_number::<u32>(line).ok()) {
            Some(v) => v,
            None => {
                log::error!("Failed to parse CRC32 from version info");
                session.close().await;
                return Err(Error::Info);
            }
        };

        let size = match lines.next().and_then(|line| parse_number::<usize>(line).ok()) {
            Some(v) => v,
            None => {
                log::error!("Failed to parse size from version info");
                session.close().await;
                return Err(Error::Info);
            }
        };

        log::info!(
            "OTA: upgrading from {} to {} (size={} bytes)",
            VERSION,
            remote_version_str,
            size
        );

        // Reuse the same TLS session for firmware download (keep-alive)
        const FIRMWARE_REQ_PREFIX: &str = "GET /firmware?device=";

        if let Err(e) = self.send_version_request(&mut session, FIRMWARE_REQ_PREFIX, REQ_PREFIX, REQ_SUFFIX_CLOSE).await {
            session.close().await;
            return Err(e);
        }

        // Read firmware HTTP response headers
        let mut buf = [0u8; 1024];
        let (total_read, body_start) = match self.read_http_response(&mut session, &mut buf).await {
            Ok(result) => result,
            Err(_) => {
                session.close().await;
                return Err(Error::Firmware);
            }
        };

        // Initialize OTA using the static table buffer.
        // SAFETY: OTA_TABLE_PTR is set once in `new()` via OTA_TABLE_BUFFER.init() which returns
        // a 'static reference. OTA operations are serialized through `&mut self`.
        let table_buffer = unsafe { &mut *OTA_TABLE_PTR.load(Ordering::Acquire) };
        let mut ota = match OtaUpdater::new(&mut self.flash, table_buffer) {
            Ok(o) => o,
            Err(_) => {
                session.close().await;
                return Err(Error::Ota);
            }
        };

        let (mut next_app_partition, _part_type) = match ota.next_partition() {
            Ok(p) => p,
            Err(_) => {
                session.close().await;
                return Err(Error::Ota);
            }
        };

        // Erase partition in chunks to avoid watchdog timeout.
        // Flash erase is a blocking operation that can take several seconds for
        // large partitions. Erasing in 64KB chunks allows yielding to keep the
        // watchdog and WiFi tasks running.
        let erase_len = (size + 4095) & !4095; // Round up to 4KB page boundary
        log::info!("Erasing OTA partition ({} bytes)...", erase_len);
        
        const ERASE_CHUNK_SIZE: u32 = 65536; // 64KB chunks
        let mut erased: u32 = 0;
        while erased < erase_len as u32 {
            let chunk = core::cmp::min(ERASE_CHUNK_SIZE, erase_len as u32 - erased);
            if let Err(e) = next_app_partition.erase(erased, erased + chunk) {
                log::error!("Flash erase failed at offset {}: {:?}", erased, e);
                session.close().await;
                return Err(Error::Ota);
            }
            erased += chunk;
            
            // Yield to let the watchdog and WiFi tasks run
            Timer::after(embassy_time::Duration::from_millis(10)).await;
            if erased % (256 * 1024) == 0 {
                log::info!("Erase progress: {}KB / {}KB", erased / 1024, erase_len / 1024);
            }
        }

        let mut bytes_written: usize = 0;

        // ESP32 flash requires 4-byte aligned writes. We buffer incoming data
        // and only write when we have a multiple of 4 bytes. Any remainder is
        // kept in the buffer for the next iteration.
        const WRITE_ALIGN: usize = 4;
        let mut write_buf = [0u8; OTA_CHUNK_BUFFER_SIZE];
        let mut write_buf_len: usize = 0;

        // Process any leftover bytes from the HTTP header read. These are the
        // first bytes of the firmware binary that were read along with headers.
        if total_read > body_start {
            let leftover = &buf[body_start..total_read];
            write_buf[..leftover.len()].copy_from_slice(leftover);
            write_buf_len = leftover.len();
            log::info!("OTA: buffered {} leftover bytes from header read", leftover.len());
        }

        // Download and flash firmware in chunks
        let mut chunk_buf = [0u8; OTA_CHUNK_BUFFER_SIZE];
        loop {
            // Only read as much as we have space for in write_buf
            let max_read = OTA_CHUNK_BUFFER_SIZE - write_buf_len;
            let read_result = session.read(&mut chunk_buf[..max_read]).await;

            match read_result {
                Ok(0) => {
                    // EOF reached - flush any remaining buffered data with padding
                    if write_buf_len > 0 {
                        let padded_len = (write_buf_len + WRITE_ALIGN - 1) & !(WRITE_ALIGN - 1);
                        // Pad with 0xFF (erased flash state) for alignment
                        for i in write_buf_len..padded_len {
                            write_buf[i] = 0xFF;
                        }
                        if let Err(e) = next_app_partition.write(bytes_written as u32, &write_buf[..padded_len]) {
                            log::error!("Flash write failed at offset {}: {:?}", bytes_written, e);
                            session.close().await;
                            return Err(Error::Ota);
                        }
                        bytes_written += write_buf_len; // Count actual bytes, not padding
                    }
                    break;
                }
                Ok(bytes_read) => {
                    // Add new data to write buffer
                    write_buf[write_buf_len..write_buf_len + bytes_read]
                        .copy_from_slice(&chunk_buf[..bytes_read]);
                    write_buf_len += bytes_read;

                    // Write the aligned portion (multiple of 4 bytes)
                    let aligned_len = write_buf_len & !(WRITE_ALIGN - 1);
                    if aligned_len > 0 {
                        if let Err(e) = next_app_partition.write(bytes_written as u32, &write_buf[..aligned_len]) {
                            log::error!("Flash write failed at offset {}: {:?}", bytes_written, e);
                            session.close().await;
                            return Err(Error::Ota);
                        }
                        bytes_written += aligned_len;

                        // Move remaining unaligned bytes to start of buffer
                        let remaining = write_buf_len - aligned_len;
                        if remaining > 0 {
                            write_buf.copy_within(aligned_len..write_buf_len, 0);
                        }
                        write_buf_len = remaining;
                    }

                    // Log progress every ~10%
                    if size > 0 && bytes_written % (size / 10) < OTA_CHUNK_BUFFER_SIZE {
                        log::info!("Progress: {}%", (bytes_written * 100) / size);
                        // Yield to let other tasks (wifi, watchdog) run
                        Timer::after(embassy_time::Duration::from_millis(10)).await;
                    }
                }
                Err(e) => {
                    // Some TLS implementations signal EOF via error instead of Ok(0).
                    // Treat EOF/ConnectionClosed as successful completion.
                    let error_str = format!("{:?}", e);
                    if error_str.contains("Eof")
                        || error_str.contains("EOF")
                        || error_str.contains("ConnectionClosed")
                        || error_str.contains("Closed")
                    {
                        // EOF via error - flush remaining data with padding
                        if write_buf_len > 0 {
                            let padded_len = (write_buf_len + WRITE_ALIGN - 1) & !(WRITE_ALIGN - 1);
                            for i in write_buf_len..padded_len {
                                write_buf[i] = 0xFF;
                            }
                            if let Err(write_err) = next_app_partition.write(bytes_written as u32, &write_buf[..padded_len]) {
                                log::error!("Flash write failed at offset {}: {:?}", bytes_written, write_err);
                                session.close().await;
                                return Err(Error::Ota);
                            }
                            bytes_written += write_buf_len;
                        }
                        break;
                    } else {
                        log::error!("Error reading firmware chunk: {:?}", e);
                        session.close().await;
                        return Err(Error::Firmware);
                    }
                }
            }
        }

        // Done reading firmware - close the TLS session to release the socket
        session.close().await;

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

        // Activate the new partition and mark it for boot
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

    /// Send an HTTP request to the OTA server.
    /// Buffers the request to send in a single packet for efficiency.
    async fn send_version_request<T: Read + Write>(
        &self,
        session: &mut T,
        req_prefix: &str,
        host_prefix: &str,
        req_suffix: &str,
    ) -> Result<(), Error> {
        // Buffer the request to send in a single packet
        // Max length: prefix (~20) + device_id (32) + host_prefix (~20) + hostname (~64) + suffix (~25) = ~161
        // 512 is plenty safe
        let mut request: String<512> = String::new();
        use core::fmt::Write as FmtWrite;

        write!(
            request,
            "{}{}{}{}{}",
            req_prefix,
            self.device_id,
            host_prefix,
            self.ota_hostname,
            req_suffix
        ).map_err(|_| Error::Connection)?;

        session.write_all(request.as_bytes()).await.map_err(|_| Error::Connection)?;
        Ok(())
    }

    /// Read an HTTP response until the header/body boundary (\r\n\r\n).
    /// Returns (total_bytes_read, body_start_offset).
    async fn read_http_response<T: Read>(
        &self,
        session: &mut T,
        buf: &mut [u8],
    ) -> Result<(usize, usize), Error> {
        let mut total_read = 0;

        loop {
            let n = session.read(&mut buf[total_read..]).await.map_err(|e| {
                log::error!("Failed to read HTTP response: {:?}", e);
                Error::Info
            })?;

            if n == 0 {
                break;
            }

            total_read += n;
            if let Some(pos) = find_header_end(&buf[..total_read]) {
                return Ok((total_read, pos));
            }

            // Buffer full but no header end found
            if total_read == buf.len() {
                log::error!("OTA buffer full ({} bytes) while reading HTTP headers", total_read);
                return Err(Error::Info);
            }
        }

        Err(Error::Info)
    }
}

/// Parse a number from a byte slice (UTF-8 string).
fn parse_number<T: core::str::FromStr>(bytes: &[u8]) -> Result<T, Error> {
    let s = core::str::from_utf8(bytes).map_err(|_| {
        log::error!("Failed to parse bytes as UTF-8");
        Error::Info
    })?;
    s.trim().parse::<T>().map_err(|_| {
        log::error!("Failed to parse number from string: '{}'", s.trim());
        Error::Info
    })
}

/// Find the HTTP header/body boundary (\r\n\r\n) in a buffer.
/// Returns the offset where the body starts (after the boundary).
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|pos| pos + 4)
}

/// Semantic version representation for comparing firmware versions.
/// Supports standard semver format: major.minor.patch[-prerelease]
/// Examples: "1.2.3", "v1.2.3", "1.2.3-beta", "v1.2.3-rc.1"
#[derive(Debug, PartialEq, Eq)]
struct SemVer {
    major: u32,
    minor: u32,
    patch: u32,
    /// Pre-release identifier (e.g., "beta.0", "rc.1", "alpha")
    /// None means it's a stable release (stable > pre-release)
    pre_release: Option<PreRelease>,
}

/// Pre-release version component (e.g., "beta.1", "rc.2", "alpha")
#[derive(Debug, PartialEq, Eq)]
struct PreRelease {
    /// The type of pre-release (alpha < beta < rc < other)
    kind: PreReleaseKind,
    /// Optional numeric suffix (e.g., the "1" in "beta.1")
    number: Option<u32>,
}

/// Pre-release type ordering: alpha < beta < rc < other < stable
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
enum PreReleaseKind {
    Alpha,
    Beta,
    Rc,
    Other, // Unknown pre-release types sort after rc but before stable
}

impl PreRelease {
    /// Parse a pre-release string like "beta", "beta.0", "rc1", "alpha"
    fn parse(s: &str) -> Option<Self> {
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

    /// Compare pre-releases by kind first, then by number.
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

        // Ensure no extra parts in version (e.g., reject "1.2.3.4")
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

    /// Returns true if self is strictly greater than other.
    /// Stable releases are greater than pre-releases with same version.
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
            (None, None) => false,             // Equal versions
            (None, Some(_)) => true,           // Stable > pre-release
            (Some(_), None) => false,          // Pre-release < stable
            (Some(a), Some(b)) => a.cmp(b) == Ordering::Greater,
        }
    }
}
