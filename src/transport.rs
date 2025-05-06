use core::marker::PhantomData;
use embassy_net::tcp::ConnectError;
use embassy_net::{
    dns::{DnsQueryType, Error as DNSError},
    tcp::TcpSocket,
    Stack,
};
use embassy_time::Duration;
use embedded_io_async::{ErrorType, Read, ReadExactError, Write};
use esp_mbedtls::{asynch::Session, Certificates, Mode, Tls, TlsError, TlsVersion::Tls1_2, X509};

use crate::config::CONFIG;
use crate::cstr::{build_trimmed_c_str_vec, write_trimmed_c_str};

const MAX_RETRIES: usize = 3;

#[derive(Debug)]
pub enum Error {
    CACertificateMissing,
    ClientCertificateMissing,
    ClientPrivateKeyMissing,
    #[allow(dead_code)]
    DNSQueryFailed(DNSError),
    DNSLookupFailed,
    HostnameCstrConversionError,
    #[allow(dead_code)]
    SocketConnectionError(ConnectError),
    #[allow(dead_code)]
    TLSSessionFailed(TlsError),
    #[allow(dead_code)]
    TLSHandshakeFailed(TlsError),
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
            .map_err(|e| Error::DNSQueryFailed(e))?
            .get(0)
            .copied()
            .ok_or(Error::DNSLookupFailed)?;
        socket
            .connect((addr, port))
            .await
            .map_err(|e| Error::SocketConnectionError(e))?;

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
        .map_err(|e| Error::TLSSessionFailed(e))?;

        session
            .connect()
            .await
            .map_err(|e| Error::TLSHandshakeFailed(e))?;

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
    ) -> Result<Self, Error> {
        let mut socket = TcpSocket::new(stack, rx_buffer, tx_buffer);
        socket.set_timeout(Some(Duration::from_secs(30)));

        let addr = stack
            .dns_query(CONFIG.mqtt_hostname, DnsQueryType::A)
            .await
            .map_err(|_| Error::DNSLookupFailed)?
            .get(0)
            .copied()
            .ok_or(Error::DNSLookupFailed)?;
        socket
            .connect((addr, CONFIG.mqtt_port))
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
