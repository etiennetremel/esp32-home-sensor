use alloc::format;
use core::marker::PhantomData;
use embassy_net::tcp::ConnectError;
use embassy_net::{
    dns::{DnsQueryType, Error as DNSError},
    tcp::TcpSocket,
    Stack,
};
use embassy_time::Duration;
use embedded_io_async::{ErrorType, Read, Write};
use esp_mbedtls::{asynch::Session, Certificates, Mode, Tls, TlsError, TlsVersion::Tls1_2, X509};

use crate::config::CONFIG;
use crate::cstr::{build_trimmed_c_str_vec, write_trimmed_c_str};

const MAX_RETRIES: usize = 3;

#[derive(Debug)]
pub enum Error {
    CACertificateMissing,
    ClientCertificateMissing,
    ClientPrivateKeyMissing,
    DNSLookupFailed,
    DNSQueryFailed(DNSError),
    HostnameCstrConversionError,
    SocketConnectionError(ConnectError),
    TLSHandshakeFailed(TlsError),
    TLSSessionFailed(TlsError),
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
impl<'a> Transport<'a, Session<'a, TcpSocket<'a>>> {
    pub async fn new(
        stack: Stack<'static>,
        tls: &'a Tls<'static>,
        rx_buffer: &'a mut [u8],
        tx_buffer: &'a mut [u8],
        hostname: &str,
        port: u16,
    ) -> Result<Self, Error> {
        let mut socket = TcpSocket::new(stack, rx_buffer, tx_buffer);
        socket.set_timeout(Some(Duration::from_secs(30)));

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

        let ca_chain = if let Some(ca_chain) = CONFIG.tls_ca {
            ca_chain
        } else {
            return Err(Error::CACertificateMissing);
        };

        let tls_cert = if let Some(tls_cert) = CONFIG.tls_cert {
            tls_cert
        } else {
            return Err(Error::ClientCertificateMissing);
        };

        let tls_key = if let Some(tls_key) = CONFIG.tls_key {
            tls_key
        } else {
            return Err(Error::ClientPrivateKeyMissing);
        };

        let ca_chain = build_trimmed_c_str_vec(ca_chain);
        let cert = build_trimmed_c_str_vec(tls_cert);
        let key = build_trimmed_c_str_vec(tls_key);

        let certificates = if cfg!(feature = "mtls") {
            Certificates {
                ca_chain: X509::pem(&ca_chain).ok(),
                certificate: X509::pem(&cert).ok(),
                private_key: X509::pem(&key).ok(),
                password: None,
            }
        } else {
            Certificates {
                ca_chain: X509::pem(&ca_chain).ok(),
                ..Default::default()
            }
        };

        // convert servername to c-string
        let mut host_buf = [0u8; 64];
        let servername = write_trimmed_c_str(hostname, &mut host_buf)
            .map_err(|_| Error::HostnameCstrConversionError)?;

        let mut session = Session::new(
            socket,
            Mode::Client { servername },
            Tls1_2,
            certificates,
            tls.reference(),
        )
        .map_err(Error::TLSSessionFailed)?;

        session.connect().await.map_err(Error::TLSHandshakeFailed)?;

        Ok(Self {
            session,
            _marker: PhantomData,
        })
    }
}

#[cfg(not(feature = "tls"))]
impl<'a> Transport<'a, TcpSocket<'a>> {
    pub async fn new(
        stack: Stack<'static>,
        _tls: &'a Tls<'static>,
        rx_buffer: &'a mut [u8],
        tx_buffer: &'a mut [u8],
        hostname: &str,
        port: u16,
    ) -> Result<Self, Error> {
        let mut socket = TcpSocket::new(stack, rx_buffer, tx_buffer);
        socket.set_timeout(Some(Duration::from_secs(30)));

        let addr = stack
            .dns_query(hostname, DnsQueryType::A)
            .await
            .map_err(|_| Error::DNSLookupFailed)?
            .get(0)
            .copied()
            .ok_or(Error::DNSLookupFailed)?;
        socket
            .connect((addr, port))
            .await
            .map_err(|_| Error::SocketConnectionError)?;

        Ok(Self {
            session: socket,
            _marker: PhantomData,
        })
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
        for attempt in 0..MAX_RETRIES {
            match self.session.read(buf).await {
                Ok(n) => return Ok(n),
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
}

// Helper function to identify EOF-related errors
// You'll need to adapt this based on your specific error types
fn is_eof_error<E: core::fmt::Debug>(error: &E) -> bool {
    let error_str = format!("{:?}", error);
    error_str.contains("Eof")
        || error_str.contains("UnexpectedEof")
        || error_str.contains("ConnectionClosed")
        || error_str.contains("BrokenPipe")
}

impl<'a, S> Write for Transport<'a, S>
where
    S: ErrorType + Read + Write + 'a,
    S::Error: core::fmt::Debug,
{
    async fn write(&mut self, buf: &[u8]) -> Result<usize, S::Error> {
        for attempt in 0..MAX_RETRIES {
            match self.session.write(buf).await {
                Ok(n) => return Ok(n),
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
        for attempt in 0..MAX_RETRIES {
            match self.session.flush().await {
                Ok(()) => return Ok(()),
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
                    log::error!("write_all: zero bytes written, likely connection closed");
                    return Err(self
                        .session
                        .write(&[])
                        .await
                        .err()
                        .unwrap_or_else(|| panic!("write_all failed, and no error available")));
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
