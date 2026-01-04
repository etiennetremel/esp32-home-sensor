use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicPtr, Ordering};
use embassy_net::tcp::ConnectError;
use embassy_net::{
    dns::{DnsQueryType, Error as DNSError},
    tcp::TcpSocket,
    Stack,
};
use embassy_time::Duration;
use embedded_io_async::{ErrorType, Read, ReadExactError, Write};
use embedded_tls::{Aes128GcmSha256, TlsConfig, TlsConnection, TlsContext, UnsecureProvider};
#[cfg(feature = "mtls")]
use p256::elliptic_curve::SecretKey;
use rand_core::{CryptoRng, RngCore};
use static_cell::StaticCell;

use crate::config::CONFIG;
use crate::constants::TCP_SOCKET_TIMEOUT_SECS;

const MAX_RETRIES: usize = 3;

/// Cached DER-encoded certificates to avoid repeated PEM parsing and allocation.
/// These are decoded once on first use and reused for all subsequent connections.
#[cfg(feature = "tls")]
struct CachedCerts {
    ca_der: Vec<u8>,
    #[cfg(feature = "mtls")]
    client_cert_der: Vec<u8>,
    #[cfg(feature = "mtls")]
    client_key_der: Vec<u8>,
}

#[cfg(feature = "tls")]
static CACHED_CERTS: StaticCell<CachedCerts> = StaticCell::new();
#[cfg(feature = "tls")]
static CERTS_PTR: AtomicPtr<CachedCerts> = AtomicPtr::new(core::ptr::null_mut());

/// Initialize and cache TLS certificates. Called once, results are reused.
#[cfg(feature = "tls")]
fn get_or_init_cached_certs() -> Result<&'static CachedCerts, Error> {
    let ptr = CERTS_PTR.load(Ordering::Acquire);
    if !ptr.is_null() {
        // SAFETY: CERTS_PTR is only set via CACHED_CERTS.init() which returns a 'static reference.
        // The pointer is never modified after initialization (write-once pattern).
        return Ok(unsafe { &*ptr });
    }

    // First time - decode PEM certificates
    let ca_chain = CONFIG.tls_ca.ok_or(Error::CACertificateMissing)?;
    let ca_der = decode_pem(ca_chain)?;
    log::info!("CA certificate decoded and cached: {} bytes", ca_der.len());

    #[cfg(feature = "mtls")]
    let client_cert_der = {
        let tls_cert = CONFIG.tls_cert.ok_or(Error::ClientCertificateMissing)?;
        let cert = decode_pem(tls_cert)?;
        log::info!(
            "Client certificate decoded and cached: {} bytes",
            cert.len()
        );
        cert
    };

    #[cfg(feature = "mtls")]
    let client_key_der = {
        let tls_key = CONFIG.tls_key.ok_or(Error::ClientPrivateKeyMissing)?;
        let key = decode_pem(tls_key)?;
        // Validate the key can be parsed
        match SecretKey::<p256::NistP256>::from_sec1_der(&key) {
            Ok(_) => log::info!("Private key decoded and cached: {} bytes", key.len()),
            Err(e) => log::error!("Failed to parse private key as SEC1 DER: {:?}", e),
        }
        key
    };

    let certs = CachedCerts {
        ca_der,
        #[cfg(feature = "mtls")]
        client_cert_der,
        #[cfg(feature = "mtls")]
        client_key_der,
    };

    let cached: &'static mut CachedCerts = CACHED_CERTS.init(certs);
    CERTS_PTR.store(cached as *mut CachedCerts, Ordering::Release);
    Ok(cached)
}

#[derive(Debug)]
pub enum Error {
    CACertificateMissing,
    ClientCertificateMissing,
    ClientPrivateKeyMissing,
    DNSQueryFailed(DNSError),
    DNSLookupFailed,
    SocketConnectionError(ConnectError),
    TLSHandshakeFailed,
    PEMParseError,
}

/// Wrap Transport (plain TCP or a TLS session)
pub struct Transport<'a, S>
where
    S: Read + Write + 'a,
{
    pub session: S,
    _marker: PhantomData<&'a ()>,
}

#[cfg(feature = "tls")]
impl<'a> Transport<'a, TlsConnection<'a, TcpSocket<'a>, Aes128GcmSha256>> {
    pub async fn new<RNG>(
        stack: Stack<'static>,
        rng: &mut RNG,
        rx_buffer: &'a mut [u8],
        tx_buffer: &'a mut [u8],
        tls_read_buffer: &'a mut [u8],
        tls_write_buffer: &'a mut [u8],
        hostname: &str,
        port: u16,
    ) -> Result<Self, Error>
    where
        RNG: CryptoRng + RngCore,
    {
        let mut socket = TcpSocket::new(stack, rx_buffer, tx_buffer);
        socket.set_timeout(Some(Duration::from_secs(TCP_SOCKET_TIMEOUT_SECS)));

        let addr = stack
            .dns_query(hostname, DnsQueryType::A)
            .await
            .map_err(Error::DNSQueryFailed)?
            .first()
            .copied()
            .ok_or(Error::DNSLookupFailed)?;

        log::info!("Connecting TCP socket to {}:{}", hostname, port);
        socket
            .connect((addr, port))
            .await
            .map_err(Error::SocketConnectionError)?;
        log::info!("TCP connected");

        // Get cached certificates (decoded once, reused for all connections)
        let certs = get_or_init_cached_certs()?;

        let mut config = TlsConfig::new().with_server_name(hostname);
        config = config.with_ca(embedded_tls::Certificate::X509(&certs.ca_der));

        #[cfg(feature = "mtls")]
        {
            config = config.with_cert(embedded_tls::Certificate::X509(&certs.client_cert_der));
            config = config.with_priv_key(&certs.client_key_der);
            log::debug!(
                "mTLS enabled: cert {} bytes, key {} bytes",
                certs.client_cert_der.len(),
                certs.client_key_der.len()
            );
        }

        let mut tls: TlsConnection<TcpSocket, Aes128GcmSha256> =
            TlsConnection::new(socket, tls_read_buffer, tls_write_buffer);

        log::info!(
            "Starting TLS handshake with {} (TLS 1.3, AES-128-GCM-SHA256)",
            hostname
        );

        let crypto_provider = UnsecureProvider::new::<Aes128GcmSha256>(rng);
        tls.open(TlsContext::new(&config, crypto_provider))
            .await
            .map_err(|e| {
                log::error!("TLS handshake failed: {:?}", e);
                log::error!("This could be due to:");
                log::error!("  - Server certificate verification failure");
                log::error!("  - Cipher suite mismatch");
                log::error!("  - Protocol version mismatch (ensure server supports TLS 1.3)");
                log::error!("  - Buffer size too small (minimum 16384 bytes required)");
                Error::TLSHandshakeFailed
            })?;
        log::info!("TLS handshake complete");

        Ok(Self {
            session: tls,
            _marker: PhantomData,
        })
    }

    /// Properly close the TLS connection and underlying TCP socket.
    /// Sends TLS close_notify alert and closes the TCP socket.
    pub async fn close(self) {
        log::debug!("Closing TLS connection...");
        match self.session.close().await {
            Ok(mut socket) => {
                log::debug!("TLS close_notify sent, closing TCP socket");
                socket.close();
            }
            Err((mut socket, e)) => {
                log::warn!("TLS close failed: {:?}, aborting socket", e);
                socket.abort();
            }
        }
        log::debug!("Transport closed");
    }
}

fn decode_pem(pem: &str) -> Result<Vec<u8>, Error> {
    use base64::Engine;
    let start_marker = "-----BEGIN";
    let end_marker = "-----END";
    let start = pem.find(start_marker).ok_or(Error::PEMParseError)?;
    let begin_end = pem[start..].find('\n').ok_or(Error::PEMParseError)? + start + 1;
    let end = pem.find(end_marker).ok_or(Error::PEMParseError)?;

    let base64_content: String = pem[begin_end..end]
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();

    base64::engine::general_purpose::STANDARD
        .decode(base64_content)
        .map_err(|_| Error::PEMParseError)
}

#[cfg(not(feature = "tls"))]
impl<'a> Transport<'a, TcpSocket<'a>> {
    pub async fn new<RNG>(
        stack: Stack<'static>,
        _rng: &mut RNG,
        rx_buffer: &'a mut [u8],
        tx_buffer: &'a mut [u8],
        _tls_read_buffer: &'a mut [u8],
        _tls_write_buffer: &'a mut [u8],
        hostname: &str,
        port: u16,
    ) -> Result<Self, Error> {
        let mut socket = TcpSocket::new(stack, rx_buffer, tx_buffer);
        socket.set_timeout(Some(Duration::from_secs(TCP_SOCKET_TIMEOUT_SECS)));

        let addr = stack
            .dns_query(hostname, DnsQueryType::A)
            .await
            .map_err(Error::DNSQueryFailed)?
            .first()
            .copied()
            .ok_or(Error::DNSLookupFailed)?;
        socket
            .connect((addr, port))
            .await
            .map_err(Error::SocketConnectionError)?;

        Ok(Self {
            session: socket,
            _marker: PhantomData,
        })
    }

    /// Close the TCP socket.
    pub async fn close(mut self) {
        log::debug!("Closing TCP socket...");
        self.session.close();
        log::debug!("Transport closed");
    }
}

impl<'a, S> ErrorType for Transport<'a, S>
where
    S: ErrorType + Read + Write + 'a,
{
    type Error = S::Error;
}

impl<'a, S> Read for Transport<'a, S>
where
    S: ErrorType + Read + Write + 'a,
    S::Error: core::fmt::Debug,
{
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, S::Error> {
        log::trace!("Transport read: buffer size {} bytes", buf.len());
        for attempt in 0..MAX_RETRIES {
            match self.session.read(buf).await {
                Ok(n) => {
                    log::trace!("Transport read success: {} bytes", n);
                    return Ok(n);
                }
                Err(e) => {
                    // Check if this is an EOF-related error that shouldn't be retried
                    if is_eof_error(&e) {
                        log::debug!("EOF encountered, not retrying: {:?}", e);
                        return Err(e);
                    }

                    log::warn!("read attempt {} failed: {:?}", attempt + 1, e);
                    if attempt + 1 == MAX_RETRIES {
                        return Err(e);
                    }
                }
            }
        }
        unreachable!()
    }

    async fn read_exact(&mut self, mut buf: &mut [u8]) -> Result<(), ReadExactError<S::Error>> {
        while !buf.is_empty() {
            let mut retry = 0;
            loop {
                match self.session.read(buf).await {
                    Ok(0) => return Err(ReadExactError::UnexpectedEof),
                    Ok(n) => {
                        buf = &mut buf[n..];
                        break;
                    }
                    Err(e) => {
                        // Don't retry EOF errors
                        if is_eof_error(&e) {
                            log::trace!("EOF encountered in read_exact: {:?}", e);
                            return Err(ReadExactError::UnexpectedEof);
                        }

                        retry += 1;
                        log::warn!("read_exact attempt {} failed: {:?}", retry, e);
                        if retry >= MAX_RETRIES {
                            return Err(ReadExactError::Other(e));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

// Helper function to identify EOF-related errors
// You'll need to adapt this based on your specific error types
fn is_eof_error<E: core::fmt::Debug>(error: &E) -> bool {
    let error_str = format!("{:?}", error);
    error_str.contains("Eof")
        || error_str.contains("UnexpectedEof")
        || error_str.contains("ConnectionClosed")
        || error_str.contains("BrokenPipe")
        || error_str.contains("ConnectionReset")
}

impl<'a, S> Write for Transport<'a, S>
where
    S: ErrorType + Read + Write + 'a,
    S::Error: core::fmt::Debug,
{
    async fn write(&mut self, buf: &[u8]) -> Result<usize, S::Error> {
        for attempt in 0..MAX_RETRIES {
            match self.session.write(buf).await {
                Ok(n) => {
                    log::trace!("Transport write success: {} bytes buffered", n);
                    // Auto-flush after write - rust-mqtt doesn't call flush(),
                    // but embedded-tls buffers data until flush() is called.
                    // Without this, MQTT packets never get sent over the wire.
                    if let Err(e) = self.session.flush().await {
                        log::error!("Auto-flush after write failed: {:?}", e);
                        return Err(e);
                    }
                    log::trace!("Transport auto-flush success, data sent");
                    return Ok(n);
                }
                Err(e) => {
                    log::warn!("write attempt {} failed: {:?}", attempt + 1, e);
                    if attempt + 1 == MAX_RETRIES {
                        return Err(e);
                    }
                }
            }
        }
        unreachable!()
    }

    async fn flush(&mut self) -> Result<(), S::Error> {
        log::trace!("Transport flush called");
        for attempt in 0..MAX_RETRIES {
            match self.session.flush().await {
                Ok(()) => {
                    log::trace!("Transport flush success");
                    return Ok(());
                }
                Err(e) => {
                    log::warn!("flush attempt {} failed: {:?}", attempt + 1, e);
                    if attempt + 1 == MAX_RETRIES {
                        return Err(e);
                    }
                }
            }
        }
        unreachable!()
    }

    async fn write_all(&mut self, mut buf: &[u8]) -> Result<(), S::Error> {
        while !buf.is_empty() {
            match self.write(buf).await {
                Ok(0) => {
                    log::error!("write_all: zero bytes written, connection likely closed");
                    // Try one more write to get the actual error from the underlying transport
                    return self.session.write(&[]).await.map(|_| ());
                }
                Ok(n) => {
                    buf = &buf[n..];
                }
                Err(e) => {
                    log::warn!("write_all failed: {:?}", e);
                    return Err(e);
                }
            }
        }
        Ok(())
    }
}
