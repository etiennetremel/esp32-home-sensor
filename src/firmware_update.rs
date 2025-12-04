use alloc::boxed::Box;
use alloc::format;
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

pub struct FirmwareUpdate<'a> {
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

impl<'a> FirmwareUpdate<'a> {
    pub fn new(
        stack: &'static Mutex<NoopRawMutex, Stack<'static>>,
        rng: ChaCha20Rng,
        rx_buf: &'static Mutex<NoopRawMutex, [u8; RX_BUFFER_SIZE]>,
        tx_buf: &'static Mutex<NoopRawMutex, [u8; TX_BUFFER_SIZE]>,
        tls_read_buf: &'static Mutex<NoopRawMutex, [u8; TLS_BUFFER_MAX]>,
        tls_write_buf: &'static Mutex<NoopRawMutex, [u8; TLS_BUFFER_MAX]>,
        mut flash: FlashStorage<'a>,
    ) -> Result<Self, Error> {
        // Mark app valid on startup
        let mut buffer = Box::new([0u8; PARTITION_TABLE_MAX_LEN]);
        let mut ota = OtaUpdater::new(&mut flash, &mut buffer).map_err(|_| Error::Ota)?;
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

        // Create transport session
        let mut session = Box::new(Transport::new(
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
        .map_err(|_| Error::Connection)?);

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
        let remote_version =
            core::str::from_utf8(lines.next().ok_or(Error::Info)?).map_err(|_| Error::Info)?;

        if remote_version == VERSION {
            log::info!(
                "Already running latest version {}. Skipping update.",
                VERSION
            );
            return Ok(());
        }

        // We only continue if an update is needed
        let _crc32 = parse_number::<u32>(lines.next().ok_or(Error::Info)?)?;
        let size = parse_number::<usize>(lines.next().ok_or(Error::Info)?)?;

        log::info!(
            "OTA: server reports version={}, size={}",
            remote_version,
            size
        );

        // Drop session and create a new one for firmware download
        drop(session);

        let mut session = Box::new(Transport::new(
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
        .map_err(|_| Error::Connection)?);

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

        // Initialize OTA
        let mut table_buffer = Box::new([0u8; PARTITION_TABLE_MAX_LEN]);
        let mut ota =
            OtaUpdater::new(&mut self.flash, &mut table_buffer).map_err(|_| Error::Ota)?;

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
             log::error!("Size mismatch!");
             return Err(Error::Firmware);
        }

        // Finalize
        ota.activate_next_partition().map_err(|_| Error::Ota)?;
        ota.set_current_ota_state(OtaImageState::New)
            .map_err(|_| Error::Ota)?;

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
