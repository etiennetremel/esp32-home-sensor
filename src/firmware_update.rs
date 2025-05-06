use embassy_net::Stack;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embassy_time::Timer;
use embedded_io_async::{Read, Write};
use esp_hal_ota::Ota;
use esp_mbedtls::Tls;
use esp_storage::FlashStorage;

use crate::config::CONFIG;
use crate::constants::*;
use crate::transport::Transport;

#[derive(Debug)]
pub enum Error {
    Connection, // Unified connection errors
    Firmware,   // Unified firmware errors
    Info,       // Unified info errors
    Ota,        // Unified OTA errors
    Config,     // Configuration errors
}

pub struct FirmwareUpdate {
    stack: &'static Mutex<NoopRawMutex, Stack<'static>>,
    tls: &'static Tls<'static>,
    rx_buf: &'static Mutex<NoopRawMutex, [u8; RX_BUFFER_SIZE]>,
    tx_buf: &'static Mutex<NoopRawMutex, [u8; TX_BUFFER_SIZE]>,
    ota_hostname: &'static str,
    ota_port: u16,
}

impl FirmwareUpdate {
    pub fn new(
        stack: &'static Mutex<NoopRawMutex, Stack<'static>>,
        tls: &'static Tls<'static>,
        rx_buf: &'static Mutex<NoopRawMutex, [u8; RX_BUFFER_SIZE]>,
        tx_buf: &'static Mutex<NoopRawMutex, [u8; TX_BUFFER_SIZE]>,
    ) -> Result<Self, Error> {
        // Mark app valid on startup - no separate function call needed
        let _ = Ota::new(FlashStorage::new())
            .map_err(|_| Error::Ota)?
            .ota_mark_app_valid();

        let ota_hostname = CONFIG.ota_hostname.ok_or(Error::Config)?;
        let ota_port = CONFIG.ota_port.ok_or(Error::Config)?;

        Ok(Self {
            stack,
            tls,
            rx_buf,
            tx_buf,
            ota_hostname,
            ota_port,
        })
    }

    pub async fn check(&mut self) -> Result<(), Error> {
        let stack_guard = self.stack.lock().await;
        let mut rx_buf = self.rx_buf.lock().await;
        let mut tx_buf = self.tx_buf.lock().await;

        // Create transport session
        let mut session = Transport::new(
            *stack_guard,
            self.tls,
            &mut *rx_buf,
            &mut *tx_buf,
            self.ota_hostname,
            self.ota_port,
        )
        .await
        .map_err(|_| Error::Connection)?;

        // Use a static string slice for the HTTP request template - optimized for memory
        const INFO_REQ_PREFIX: &str = "GET /version HTTP/1.1\r\nHost: ";
        const INFO_REQ_SUFFIX: &str = "\r\nConnection: close\r\n\r\n";

        // Write request in chunks to avoid dynamic allocation
        session
            .write_all(INFO_REQ_PREFIX.as_bytes())
            .await
            .map_err(|_| Error::Connection)?;
        session
            .write_all(self.ota_hostname.as_bytes())
            .await
            .map_err(|_| Error::Connection)?;
        session
            .write_all(INFO_REQ_SUFFIX.as_bytes())
            .await
            .map_err(|_| Error::Connection)?;

        // Use a single buffer for all operations
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
        let version =
            core::str::from_utf8(lines.next().ok_or(Error::Info)?).map_err(|_| Error::Info)?;
        let current_version = env!("CARGO_PKG_VERSION");

        if version == current_version {
            log::info!(
                "Already running latest version {}. Skipping update.",
                current_version
            );
            return Ok(());
        }

        // We only continue if an update is needed
        let crc32 = parse_number::<u32>(lines.next().ok_or(Error::Info)?)?;
        let size = parse_number::<usize>(lines.next().ok_or(Error::Info)?)?;

        log::info!(
            "OTA: server reports version={}, crc32={:#x}, size={}",
            version,
            crc32,
            size
        );

        // Drop session and create a new one for firmware download
        drop(session);

        let mut session = Transport::new(
            *stack_guard,
            self.tls,
            &mut *rx_buf,
            &mut *tx_buf,
            self.ota_hostname,
            self.ota_port,
        )
        .await
        .map_err(|_| Error::Connection)?;

        // Same approach for firmware request
        const FIRMWARE_REQ_PREFIX: &str = "GET /firmware HTTP/1.1\r\nHost: ";

        session
            .write_all(FIRMWARE_REQ_PREFIX.as_bytes())
            .await
            .map_err(|_| Error::Firmware)?;
        session
            .write_all(self.ota_hostname.as_bytes())
            .await
            .map_err(|_| Error::Firmware)?;
        session
            .write_all(INFO_REQ_SUFFIX.as_bytes())
            .await
            .map_err(|_| Error::Firmware)?;

        // Reuse buffer for firmware download
        let mut buf = [0u8; OTA_CHUNK_BUFFER_SIZE];
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

        // Initialize OTA once with proper error handling
        let mut ota = Ota::new(FlashStorage::new()).map_err(|_| Error::Ota)?;
        ota.ota_begin(size as u32, crc32).map_err(|_| Error::Ota)?;

        let mut bytes_written = 0;

        // Write leftover bytes from first read
        if total_read > body_start {
            let leftover = &buf[body_start..total_read];
            ota.ota_write_chunk(leftover).map_err(|_| Error::Ota)?;
            bytes_written += leftover.len();
        }

        // Process firmware in chunks
        process_firmware_chunks(&mut session, &mut ota, &mut buf, size, bytes_written).await?;

        // Verify and finalize
        if ota.ota_verify().map_err(|_| Error::Ota)? {
            log::info!("CRC OK. Finalizing OTA.");
            if ota.ota_flush(false, true).is_ok() {
                log::info!("OTA complete. Rebooting...");
                Timer::after_millis(1_000).await;
                esp_hal::system::software_reset();
            } else {
                log::error!("Failed to finalize OTA.");
                return Err(Error::Ota);
            }
        } else {
            log::error!("CRC mismatch after flash!");
            return Err(Error::Ota);
        }
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

// Extract firmware processing into a separate function
async fn process_firmware_chunks<R: Read + Write>(
    session: &mut R,
    ota: &mut Ota<FlashStorage>,
    buf: &mut [u8],
    size: usize,
    mut bytes_written: usize,
) -> Result<(), Error>
where
    R::Error: core::fmt::Debug,
{
    while bytes_written < size {
        let to_read = (size - bytes_written).min(buf.len());
        let buf_slice = &mut buf[..to_read];
        let mut read_off = 0;

        // Read as much as possible
        while read_off < to_read {
            match session.read(&mut buf_slice[read_off..]).await {
                Ok(0) => break, // EOF
                Ok(n) => read_off += n,
                Err(e) => {
                    log::error!("Error reading firmware chunk: {:?}", e);
                    return Err(Error::Firmware);
                }
            }
        }

        if read_off == 0 {
            break;
        }

        // Write chunk to flash
        ota.ota_write_chunk(&buf_slice[..read_off])
            .map_err(|_| Error::Ota)?;

        bytes_written += read_off;

        // Log progress every 10%
        if bytes_written % (size / 10) < buf.len() {
            log::info!("Progress: {}%", (bytes_written * 100) / size);
        }
    }

    Ok(())
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|pos| pos + 4)
}
