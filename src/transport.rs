use core::marker::PhantomData;
use embassy_net::{dns::DnsQueryType, tcp::TcpSocket, Stack};
use embassy_time::Duration;
use embedded_io_async::{ErrorType, Read, Write};
use esp_mbedtls::{asynch::Session, Certificates, Mode, Tls, TlsVersion, X509};

use crate::config::CONFIG;
use crate::cstr::{build_trimmed_c_str_vec, write_trimmed_c_str};

#[derive(Debug)]
pub enum Error {
    CACertificateMissing,
    ClientCertificateMissing,
    ClientPrivateKeyMissing,
    DNSLookupFailed,
    HostnameCstrConversionError,
    SocketConnectionError,
    TLSHandshakeFailed,
    TLSSessionFailed,
}

/// Wrap Transport (plain TCP or a TLS session)
pub struct Transport<'a, S>
where
    S: Read + Write + 'a,
{
    session: S,
    _marker: PhantomData<&'a ()>,
}

#[cfg(feature = "tls")]
impl<'a> Transport<'a, Session<'a, TcpSocket<'a>>> {
    pub async fn new(
        stack: Stack<'static>,
        tls: &'a Tls<'static>,
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
        let servername = write_trimmed_c_str(CONFIG.mqtt_hostname, &mut host_buf)
            .map_err(|_| Error::HostnameCstrConversionError)?;

        let mut session = Session::new(
            socket,
            Mode::Client { servername },
            TlsVersion::Tls1_2,
            certificates,
            tls.reference(),
        )
        .map_err(|_| Error::TLSSessionFailed)?;
        session
            .connect()
            .await
            .map_err(|_| Error::TLSHandshakeFailed)?;

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
        self.session.read(buf).await
    }
}

impl<'a, S> Write for Transport<'a, S>
where
    S: ErrorType + Read + Write + 'a,
    S::Error: core::fmt::Debug,
{
    async fn write(&mut self, buf: &[u8]) -> Result<usize, S::Error> {
        self.session.write(buf).await
    }

    async fn flush(&mut self) -> Result<(), S::Error> {
        self.session.flush().await
    }
}
