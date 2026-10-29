use anyhow::{anyhow, Result};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Clean device code to 9-digit format (e.g. "739-281-904" -> "739281904")
pub fn clean_code(input: &str) -> String {
    input.chars().filter(|c| c.is_ascii_digit()).collect()
}

/// Check if an address input is a 9-digit device code rather than an IP:port
pub fn is_device_code(input: &str) -> bool {
    let digits = clean_code(input);
    digits.len() == 9 && !input.contains('.') && !input.contains(':')
        || (input.contains('-') && digits.len() == 9)
}

/// Helper for sending raw HTTP requests with zero external HTTP dependencies
async fn send_http_request(
    method: &str,
    url_str: &str,
    path: &str,
    body: Option<&str>,
) -> Result<String> {
    // Parse host and port from url_str (e.g. "http://127.0.0.1:28844" or "http://signal.example.com")
    let cleaned = url_str
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_end_matches('/');

    let parts: Vec<&str> = cleaned.split(':').collect();
    let host = parts[0];
    let port: u16 = if parts.len() > 1 {
        parts[1].parse().unwrap_or(80)
    } else {
        80
    };

    let addr = format!("{}:{}", host, port);
    let mut stream = tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(&addr))
        .await
        .map_err(|_| anyhow!("Connection to signaling server {} timed out", addr))?
        .map_err(|e| anyhow!("Failed to connect to signaling server {}: {}", addr, e))?;

    let content_len = body.map(|b| b.len()).unwrap_or(0);
    let request_header = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        method, path, host, content_len
    );

    stream.write_all(request_header.as_bytes()).await?;
    if let Some(b) = body {
        stream.write_all(b.as_bytes()).await?;
    }

    let mut response_bytes = Vec::new();
    let _ = tokio::time::timeout(
        Duration::from_secs(3),
        stream.read_to_end(&mut response_bytes),
    )
    .await;

    let response_str = String::from_utf8_lossy(&response_bytes);
    let body_str = if let Some(pos) = response_str.find("\r\n\r\n") {
        &response_str[pos + 4..]
    } else {
        &response_str
    };

    // Robust JSON extraction: strip HTTP Chunked transfer encoding or proxy noise
    if let (Some(start), Some(end)) = (body_str.find('{'), body_str.rfind('}')) {
        if start <= end {
            return Ok(body_str[start..=end].to_string());
        }
    }

    Ok(body_str.trim().to_string())
}

/// Enumerate all network interfaces to find the true physical LAN IPv4 (excluding VPN/Proxy TUNs like 198.18.x.x)
pub fn detect_best_local_ip() -> String {
    if let Ok(interfaces) = local_ip_address::list_afinet_netifas() {
        for (_name, ip) in &interfaces {
            if let std::net::IpAddr::V4(ipv4) = ip {
                let s = ipv4.to_string();
                if !ipv4.is_loopback() && !ipv4.is_link_local() && !s.starts_with("198.18.") && !s.starts_with("198.19.") {
                    if s.starts_with("192.168.") || s.starts_with("10.") || s.starts_with("172.") {
                        return s;
                    }
                }
            }
        }
    }
    local_ip_address::local_ip()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}

/// Enumerate all network interfaces to find the true global unicast IPv6 address (starting with 2 or 3, e.g. 2408:..., 2409:..., 240e:...)
pub fn detect_public_ipv6() -> Option<String> {
    if let Ok(interfaces) = local_ip_address::list_afinet_netifas() {
        for (_name, ip) in interfaces {
            if let std::net::IpAddr::V6(ipv6) = ip {
                let s = ipv6.to_string();
                if (s.starts_with('2') || s.starts_with('3')) && !s.starts_with("fe80") && !s.starts_with("fd") {
                    return Some(s);
                }
            }
        }
    }

    // Fallback: UDP connect probe
    for target in &["[2400:3200::1]:53", "[2001:4860:4860::8888]:53"] {
        if let Ok(socket) = std::net::UdpSocket::bind("[::]:0") {
            if socket.connect(target).is_ok() {
                if let Ok(local) = socket.local_addr() {
                    let ip = local.ip();
                    if ip.is_ipv6() && !ip.is_loopback() {
                        let s = ip.to_string();
                        if !s.starts_with("fe80") && !s.starts_with("fd") {
                            return Some(s);
                        }
                    }
                }
            }
        }
    }
    None
}

/// Register local device to the signaling server (including public IPv6 if available)
pub async fn register_to_signaling(
    server_url: &str,
    device_id: &str,
    device_code: &str,
    device_name: &str,
    local_ip: &str,
    listen_port: u16,
    ipv6: Option<String>,
) -> Result<()> {
    let payload = serde_json::json!({
        "device_id": device_id,
        "device_code": device_code,
        "device_name": device_name,
        "local_ip": local_ip,
        "listen_port": listen_port,
        "ipv6": ipv6,
    });

    let body = payload.to_string();
    let _ = send_http_request("POST", server_url, "/api/register", Some(&body)).await?;
    log::info!("Registered device {} with signaling server {} (IPv6: {:?})", device_code, server_url, ipv6);
    Ok(())
}

/// Lookup target device by device code and retrieve candidate addresses
pub async fn lookup_peer_candidates(
    server_url: &str,
    device_code: &str,
) -> Result<Vec<String>> {
    let code = clean_code(device_code);
    let path = format!("/api/lookup/{}", code);
    let res = send_http_request("GET", server_url, &path, None).await?;

    let json: serde_json::Value = serde_json::from_str(&res)
        .map_err(|e| anyhow!("Invalid JSON from signaling server: {} (raw: {})", e, res))?;

    if json.get("found").and_then(|v| v.as_bool()).unwrap_or(false) {
        if let Some(candidates) = json
            .get("peer")
            .and_then(|p| p.get("candidates"))
            .and_then(|c| c.as_array())
        {
            let addrs: Vec<String> = candidates
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            if !addrs.is_empty() {
                return Ok(addrs);
            }
        }
    }

    let msg = json
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("Device not found or offline");
    Err(anyhow!("{}", msg))
}

/// Parse relay host from signaling server URL (defaults to relay port 28845)
pub fn parse_relay_address(signal_url: &str) -> String {
    let cleaned = signal_url
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_end_matches('/');

    let host = cleaned.split(':').next().unwrap_or("dxcool.cn");
    format!("{}:28845", host)
}

/// Connect to TCP Relay server and join a session by device code
pub async fn join_relay_tunnel(relay_addr: &str, device_code: &str) -> Result<TcpStream> {
    let clean = clean_code(device_code);
    let mut stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(relay_addr))
        .await
        .map_err(|_| anyhow!("Connection to relay server {} timed out", relay_addr))?
        .map_err(|e| anyhow!("Failed connecting to relay {}: {}", relay_addr, e))?;

    let cmd = format!("JOIN {}\n", clean);
    stream.write_all(cmd.as_bytes()).await?;

    let mut buf = [0u8; 64];
    let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf))
        .await
        .map_err(|_| anyhow!("Relay handshake timed out"))?
        .map_err(|e| anyhow!("Relay read error: {}", e))?;

    let resp = String::from_utf8_lossy(&buf[..n]);
    if resp.contains("OK") {
        Ok(stream)
    } else {
        Err(anyhow!("Relay server error: {}", resp.trim()))
    }
}

/// Connect to TCP Relay server and register as host waiting for peer join
pub async fn register_relay_tunnel(relay_addr: &str, device_code: &str) -> Result<TcpStream> {
    let clean = clean_code(device_code);
    let mut stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(relay_addr))
        .await
        .map_err(|_| anyhow!("Connection to relay server {} timed out", relay_addr))?
        .map_err(|e| anyhow!("Failed connecting to relay {}: {}", relay_addr, e))?;

    let cmd = format!("REGISTER {}\n", clean);
    stream.write_all(cmd.as_bytes()).await?;

    let mut buf = [0u8; 64];
    let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf))
        .await
        .map_err(|_| anyhow!("Relay host register timed out"))?
        .map_err(|e| anyhow!("Relay read error: {}", e))?;

    let resp = String::from_utf8_lossy(&buf[..n]);
    if resp.contains("OK") {
        Ok(stream)
    } else {
        Err(anyhow!("Relay host registration failed: {}", resp.trim()))
    }
}
