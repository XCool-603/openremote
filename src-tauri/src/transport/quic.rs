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
        let socket = std::net::UdpSocket::bind(addr)
            .with_context(|| format!("Failed to bind UDP socket on {}", addr))?;
        Self::bind_with_socket(socket)
    }

    /// Bind QUIC server using an existing UdpSocket (allows STUN probing on the same port first)
    pub fn bind_with_socket(socket: std::net::UdpSocket) -> Result<Self> {
        let (cert, key) = generate_self_signed_cert()?;
        let server_crypto = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .context("Failed to configure rustls ServerConfig")?;

        let mut server_config = ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)
                .map_err(|e| anyhow!("Failed to create QuicServerConfig: {}", e))?,
        ));

        // Transport config tuned for 60FPS ultra-low-latency desktop streaming
        let mut transport_config = quinn::TransportConfig::default();
        transport_config.max_idle_timeout(Some(std::time::Duration::from_secs(15).try_into().unwrap()));
        transport_config.keep_alive_interval(Some(std::time::Duration::from_secs(2)));
        transport_config.initial_rtt(std::time::Duration::from_millis(15));
        transport_config.stream_receive_window((8 * 1024 * 1024u32).into());
        transport_config.receive_window((16 * 1024 * 1024u32).into());
        transport_config.send_window(8 * 1024 * 1024);
        server_config.transport_config(Arc::new(transport_config));

        let runtime = quinn::default_runtime()
            .ok_or_else(|| anyhow!("No tokio runtime found for QUIC server"))?;
        let endpoint = Endpoint::new(
            quinn::EndpointConfig::default(),
            Some(server_config),
            socket,
            runtime,
        )
        .context("Failed to create QUIC server endpoint from socket")?;

        let listen_addr = endpoint.local_addr()?;
        log::info!("QUIC server listening on {}", listen_addr);

        Ok(Self {
            endpoint,
            listen_addr,
        })
    }

    /// Bind QUIC server using a custom `AsyncUdpSocket` (e.g. UDP-QUIC Relay bridge)
    pub fn bind_with_abstract_socket(
        socket: Arc<dyn quinn::AsyncUdpSocket>,
        transport_config: Arc<quinn::TransportConfig>,
    ) -> Result<Self> {
        let (cert, key) = generate_self_signed_cert()?;
        let server_crypto = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .context("Failed to configure rustls ServerConfig")?;

        let mut server_config = ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)
                .map_err(|e| anyhow!("Failed to create QuicServerConfig: {}", e))?,
        ));
        server_config.transport_config(transport_config);

        let runtime = quinn::default_runtime()
            .ok_or_else(|| anyhow!("No tokio runtime found for QUIC server"))?;
        let endpoint = Endpoint::new_with_abstract_socket(
            quinn::EndpointConfig::default(),
            Some(server_config),
            socket,
            runtime,
        )
        .context("Failed to create QUIC server endpoint from abstract socket")?;

        let listen_addr = endpoint.local_addr()?;
        Ok(Self {
            endpoint,
            listen_addr,
        })
    }

    /// Accept the next raw incoming QUIC connection without blocking on its handshake
    pub async fn accept_incoming(&self) -> Result<quinn::Incoming> {
        self.endpoint
            .accept()
            .await
            .ok_or_else(|| anyhow!("QUIC endpoint stopped accepting"))
    }

    /// Complete QUIC handshake and accept bi-directional stream for a single incoming connection
    pub async fn finish_incoming(incoming: quinn::Incoming) -> Result<ConnectionChannel> {
        let connection = tokio::time::timeout(std::time::Duration::from_secs(4), incoming)
            .await
            .map_err(|_| anyhow!("QUIC handshake timed out"))?
            .context("QUIC handshake failed")?;
        let (send_stream, recv_stream) = tokio::time::timeout(
            std::time::Duration::from_secs(4),
            connection.accept_bi(),
        )
        .await
        .map_err(|_| anyhow!("QUIC accept_bi timed out"))?
        .context("Failed to accept bi-directional stream")?;

        Ok(ConnectionChannel::new(connection, send_stream, recv_stream))
    }

    /// Accept the next incoming connection and open bi-directional channel (convenience wrapper)
    pub async fn accept_channel(&self) -> Result<ConnectionChannel> {
        let incoming = self.accept_incoming().await?;
        Self::finish_incoming(incoming).await
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
    /// Create QUIC client endpoint matching the target address family
    /// (binds physical LAN IPv4 or active global IPv6 to bypass Clash/Mihomo TUN!)
    pub fn new_for_target(target: &SocketAddr) -> Result<Self> {
        let bind_addr: SocketAddr = if target.is_ipv6() {
            if !target.ip().is_loopback() {
                if let Some(pub_v6) = crate::signaling::detect_public_ipv6()
                    .and_then(|s| s.parse::<std::net::IpAddr>().ok())
                {
                    let candidate = SocketAddr::new(pub_v6, 0);
                    if let Ok(c) = Self::bind_client(candidate) {
                        return Ok(c);
                    }
                }
            }
            "[::]:0".parse()?
        } else if !target.ip().is_loopback() {
            if let Some(lan_ip) = crate::signaling::detect_best_local_ipv4() {
                SocketAddr::new(std::net::IpAddr::V4(lan_ip), 0)
            } else {
                "0.0.0.0:0".parse()?
            }
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

    /// Create QUIC client using a pre-bound UdpSocket (for STUN + coordinated UDP hole punching)
    pub fn new_with_socket(socket: std::net::UdpSocket) -> Result<Self> {
        let runtime = quinn::default_runtime()
            .ok_or_else(|| anyhow!("No tokio runtime found for QUIC client"))?;
        let mut endpoint = Endpoint::new(
            quinn::EndpointConfig::default(),
            None,
            socket,
            runtime,
        )
        .context("Failed to create client QUIC endpoint from socket")?;

        Self::apply_client_config(&mut endpoint)?;
        Ok(Self { endpoint })
    }

    fn bind_client(bind_addr: SocketAddr) -> Result<Self> {
        let mut endpoint = Endpoint::client(bind_addr)
            .context("Failed to create client QUIC endpoint")?;
        Self::apply_client_config(&mut endpoint)?;
        Ok(Self { endpoint })
    }

    pub fn apply_client_config(endpoint: &mut Endpoint) -> Result<()> {
        let mut transport_config = quinn::TransportConfig::default();
        transport_config.max_idle_timeout(Some(std::time::Duration::from_secs(15).try_into().unwrap()));
        transport_config.keep_alive_interval(Some(std::time::Duration::from_secs(2)));
        transport_config.initial_rtt(std::time::Duration::from_millis(15));
        transport_config.stream_receive_window((8 * 1024 * 1024u32).into());
        transport_config.receive_window((16 * 1024 * 1024u32).into());
        transport_config.send_window(8 * 1024 * 1024);
        Self::apply_client_config_with_transport(endpoint, Arc::new(transport_config))
    }

    pub fn apply_client_config_with_transport(
        endpoint: &mut Endpoint,
        transport_config: Arc<quinn::TransportConfig>,
    ) -> Result<()> {
        let client_crypto = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(SkipServerVerification::new())
            .with_no_client_auth();

        let mut client_config = ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto)
                .map_err(|e| anyhow!("Failed to create QuicClientConfig: {}", e))?,
        ));
        client_config.transport_config(transport_config);
        endpoint.set_default_client_config(client_config);
        Ok(())
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

        Ok(ConnectionChannel::new(connection, send_stream, recv_stream))
    }
}
