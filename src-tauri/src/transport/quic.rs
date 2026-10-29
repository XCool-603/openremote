use crate::transport::ConnectionChannel;
use anyhow::{anyhow, Context, Result};
use quinn::{ClientConfig, Endpoint, ServerConfig};
use rustls::pki_types::{CertificateDer, ServerName};
use std::net::SocketAddr;
use std::sync::Arc;

/// Generate self-signed certificate and private key using rcgen
pub fn generate_self_signed_cert() -> Result<(rustls::pki_types::CertificateDer<'static>, rustls::pki_types::PrivateKeyDer<'static>)> {
    let subject_alt_names = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    let certified_key = rcgen::generate_simple_self_signed(subject_alt_names)
        .context("Failed to generate self-signed certificate")?;
    
    let cert_der = CertificateDer::from(certified_key.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
        certified_key.key_pair.serialize_der(),
    ));

    Ok((cert_der, key_der))
}

/// QUIC Server to listen for incoming OpenRemote connections
pub struct QuicServer {
    pub endpoint: Endpoint,
    pub listen_addr: SocketAddr,
}

impl QuicServer {
    /// Bind QUIC server on given socket address
    pub fn bind(addr: SocketAddr) -> Result<Self> {
        let (cert, key) = generate_self_signed_cert()?;
        let server_crypto = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .context("Failed to configure rustls ServerConfig")?;

        let mut server_config = ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)
                .map_err(|e| anyhow!("Failed to create QuicServerConfig: {}", e))?,
        ));

        // Transport config tweaks for fast low-latency streaming
        let mut transport_config = quinn::TransportConfig::default();
        transport_config.max_idle_timeout(Some(std::time::Duration::from_secs(10).try_into().unwrap()));
        transport_config.keep_alive_interval(Some(std::time::Duration::from_secs(2)));
        server_config.transport_config(Arc::new(transport_config));

        let endpoint = Endpoint::server(server_config, addr)
            .with_context(|| format!("Failed to bind QUIC server on {}", addr))?;

        let listen_addr = endpoint.local_addr()?;
        log::info!("QUIC server listening on {}", listen_addr);

        Ok(Self {
            endpoint,
            listen_addr,
        })
    }

    /// Accept the next incoming connection and open bi-directional channel
    pub async fn accept_channel(&self) -> Result<ConnectionChannel> {
        let incoming = self
            .endpoint
            .accept()
            .await
            .ok_or_else(|| anyhow!("QUIC endpoint stopped accepting"))?;

        let connection = incoming.await.context("QUIC handshake failed")?;
        let (send_stream, recv_stream) = connection
            .accept_bi()
            .await
            .context("Failed to accept bi-directional stream")?;

        Ok(ConnectionChannel::new(send_stream, recv_stream))
    }
}

/// QUIC Client to connect to remote host
pub struct QuicClient {
    pub endpoint: Endpoint,
}

#[derive(Debug)]
struct SkipServerVerification(Arc<rustls::crypto::CryptoProvider>);

impl SkipServerVerification {
    fn new() -> Arc<Self> {
        Arc::new(Self(Arc::new(rustls::crypto::ring::default_provider())))
    }
}

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0
            .signature_verification_algorithms
            .supported_schemes()
    }
}

impl QuicClient {
    /// Create QUIC client endpoint matching the target address family (IPv4 or IPv6)
    pub fn new_for_target(target: &SocketAddr) -> Result<Self> {
        let bind_addr = if target.is_ipv6() {
            "[::]:0".parse()?
        } else {
            "0.0.0.0:0".parse()?
        };
        Self::bind_client(bind_addr)
    }

    /// Default client bound to dual-stack IPv6 or fallback to IPv4
    pub fn new() -> Result<Self> {
        if let Ok(c) = Self::bind_client("[::]:0".parse()?) {
            return Ok(c);
        }
        Self::bind_client("0.0.0.0:0".parse()?)
    }

    fn bind_client(bind_addr: SocketAddr) -> Result<Self> {
        let mut endpoint = Endpoint::client(bind_addr)
            .context("Failed to create client QUIC endpoint")?;

        let client_crypto = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(SkipServerVerification::new())
            .with_no_client_auth();

        let mut client_config = ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto)
                .map_err(|e| anyhow!("Failed to create QuicClientConfig: {}", e))?,
        ));

        let mut transport_config = quinn::TransportConfig::default();
        transport_config.max_idle_timeout(Some(std::time::Duration::from_secs(10).try_into().unwrap()));
        transport_config.keep_alive_interval(Some(std::time::Duration::from_secs(2)));
        client_config.transport_config(Arc::new(transport_config));

        endpoint.set_default_client_config(client_config);

        Ok(Self { endpoint })
    }

    /// Connect to remote QUIC server and establish bidirectional stream channel
    pub async fn connect(&self, addr: SocketAddr) -> Result<ConnectionChannel> {
        let connection = self
            .endpoint
            .connect(addr, "localhost")
            .with_context(|| format!("Failed to initiate connection to {}", addr))?
            .await
            .with_context(|| format!("Connection handshake with {} failed", addr))?;

        let (send_stream, recv_stream) = connection
            .open_bi()
            .await
            .context("Failed to open bi-directional stream")?;

        Ok(ConnectionChannel::new(send_stream, recv_stream))
    }
}
