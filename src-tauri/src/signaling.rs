use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::sync::{Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpSocket, TcpStream};

static DIRECT_DNS_CACHE: OnceLock<StdMutex<HashMap<String, (Ipv4Addr, Instant)>>> = OnceLock::new();

fn dns_cache() -> &'static StdMutex<HashMap<String, (Ipv4Addr, Instant)>> {
    DIRECT_DNS_CACHE.get_or_init(|| StdMutex::new(HashMap::new()))
}

pub fn is_clash_fake_ip(ip: Ipv4Addr) -> bool {
    let oct = ip.octets();
    oct[0] == 198 && (oct[1] == 18 || oct[1] == 19)
}

/// Detect the real physical LAN IPv4 address (excluding Clash/TUN 198.18.x.x, loopback, link-local)
pub fn detect_best_local_ipv4() -> Option<Ipv4Addr> {
    if let Ok(interfaces) = local_ip_address::list_afinet_netifas() {
        for (_name, ip) in &interfaces {
            if let IpAddr::V4(ipv4) = ip {
                if !ipv4.is_loopback() && !ipv4.is_link_local() && !is_clash_fake_ip(*ipv4) {
                    let s = ipv4.to_string();
                    if s.starts_with("192.168.") || s.starts_with("10.") || s.starts_with("172.") {
                        return Some(*ipv4);
                    }
                }
            }
        }
    }
    None
}

/// Direct raw UDP DNS A-record query to `dns_server` (e.g. 223.5.5.5:53) bound to physical LAN NIC,
/// completely bypassing Clash/Mihomo Fake-IP DNS and TCP port-80 proxies!
fn query_udp_dns_a_direct(host: &str, dns_server: &str) -> Option<Ipv4Addr> {
    let sock = crate::nat::bind_udp_direct(0).ok()?;
    let _ = sock.set_read_timeout(Some(Duration::from_millis(900)));

    let mut pkt = Vec::with_capacity(64);
    let txid: [u8; 2] = [0x4F, 0x52];
    pkt.extend_from_slice(&txid);
    pkt.extend_from_slice(&[0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]); // QDCOUNT=1
    for label in host.trim_end_matches('.').split('.') {
        let bytes = label.as_bytes();
        if bytes.is_empty() || bytes.len() > 63 {
            return None;
        }
        pkt.push(bytes.len() as u8);
        pkt.extend_from_slice(bytes);
    }
    pkt.push(0); // root
    pkt.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]); // QTYPE=A, QCLASS=IN

    sock.send_to(&pkt, dns_server).ok()?;
    let mut resp = [0u8; 512];
    let (n, _) = sock.recv_from(&mut resp).ok()?;
    if n < 12 || resp[0..2] != txid {
        return None;
    }

    let qdcount = u16::from_be_bytes([resp[4], resp[5]]) as usize;
    let ancount = u16::from_be_bytes([resp[6], resp[7]]) as usize;
    let mut off = 12usize;

    let skip_name = |buf: &[u8], mut p: usize| -> Option<usize> {
        let mut jumps = 0;
        loop {
            if p >= buf.len() || jumps > 16 {
                return None;
            }
            let len = buf[p];
            if len & 0xC0 == 0xC0 {
                return (p + 2 <= buf.len()).then_some(p + 2);
            }
            if len == 0 {
                return Some(p + 1);
            }
            p += 1 + len as usize;
            jumps += 1;
        }
    };

    for _ in 0..qdcount {
        off = skip_name(&resp[..n], off)? + 4;
    }
    for _ in 0..ancount {
        off = skip_name(&resp[..n], off)?;
        if off + 10 > n {
            break;
        }
        let rtype = u16::from_be_bytes([resp[off], resp[off + 1]]);
        let rdlen = u16::from_be_bytes([resp[off + 8], resp[off + 9]]) as usize;
        off += 10;
        if off + rdlen > n {
            break;
        }
        if rtype == 1 && rdlen == 4 {
            let ip = Ipv4Addr::new(resp[off], resp[off + 1], resp[off + 2], resp[off + 3]);
            if !is_clash_fake_ip(ip) && !ip.is_unspecified() && !ip.is_loopback() {
                return Some(ip);
            }
        }
        off += rdlen;
    }
    None
}

/// Resolve hostname to its REAL direct IPv4 (automatically bypassing Clash Fake-IP 198.18.x.x).
/// Never returns a Clash Fake-IP; falls back across DNSPod HTTPDNS (119.29.29.29) and AliDNS (223.5.5.5:53).
/// Cached for 10 minutes so subsequent calls have 0ms overhead.
pub async fn resolve_direct_ipv4(host: &str) -> Option<Ipv4Addr> {
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return (!is_clash_fake_ip(ip)).then_some(ip);
    }

    let key = host.to_ascii_lowercase();
    if let Ok(cache) = dns_cache().lock() {
        if let Some(&(ip, ts)) = cache.get(&key) {
            if ts.elapsed() < Duration::from_secs(600) {
                return Some(ip);
            }
        }
    }

    // 1. Try system DNS first; if it returns a real non-Fake-IP, use it immediately!
    let host_owned = host.to_string();
    let sys_ip = tokio::task::spawn_blocking(move || {
        format!("{}:80", host_owned)
            .to_socket_addrs()
            .ok()?
            .find_map(|a| match a {
                SocketAddr::V4(v4) => Some(*v4.ip()),
                SocketAddr::V6(_) => None,
            })
    })
    .await
    .ok()
    .flatten();

    if let Some(ip) = sys_ip {
        if !is_clash_fake_ip(ip) {
            if let Ok(mut cache) = dns_cache().lock() {
                cache.insert(key, (ip, Instant::now()));
            }
            return Some(ip);
        }
        log::info!(
            "Detected Clash/TUN Fake-IP {} for '{}' -> resolving real direct IP via HTTPDNS/DirectUDP",
            ip,
            host
        );
    }

    // 2. Direct UDP DNS to 223.5.5.5:53 / 119.29.29.29:53 bound to physical LAN NIC (fast, zero proxy!)
    let host_udp = host.to_string();
    let udp_resolved = tokio::task::spawn_blocking(move || {
        query_udp_dns_a_direct(&host_udp, "223.5.5.5:53")
            .or_else(|| query_udp_dns_a_direct(&host_udp, "119.29.29.29:53"))
    })
    .await
    .ok()
    .flatten();

    if let Some(ip) = udp_resolved {
        log::info!("Direct UDP DNS resolved '{}' -> {} (bypassed Clash Fake-IP)", host, ip);
        if let Ok(mut cache) = dns_cache().lock() {
            cache.insert(key, (ip, Instant::now()));
        }
        return Some(ip);
    }

    // 3. Fallback: DNSPod HTTPDNS (119.29.29.29:80) bound to physical LAN NIC
    let dns_target: SocketAddr = "119.29.29.29:80".parse().ok()?;
    let mut stream = connect_addr_direct(dns_target, Duration::from_millis(1200)).await.ok()?;
    let req = format!(
        "GET /d?dn={} HTTP/1.1\r\nHost: 119.29.29.29\r\nConnection: close\r\n\r\n",
        host
    );
    stream.write_all(req.as_bytes()).await.ok()?;
    let mut buf = Vec::with_capacity(512);
    let _ = tokio::time::timeout(Duration::from_millis(1200), stream.read_to_end(&mut buf)).await;
    let text = String::from_utf8_lossy(&buf);
    let body = text.split("\r\n\r\n").nth(1)?.trim();
    for part in body.split(';') {
        if let Ok(ip) = part.trim().parse::<Ipv4Addr>() {
            if !is_clash_fake_ip(ip) && !ip.is_unspecified() && !ip.is_loopback() {
                log::info!("Direct HTTPDNS resolved '{}' -> {} (bypassed Clash Fake-IP)", host, ip);
                if let Ok(mut cache) = dns_cache().lock() {
                    cache.insert(key, (ip, Instant::now()));
                }
                return Some(ip);
            }
        }
    }
    None
}

/// Sync helper reading the shared direct DNS cache (or falling back to non-Fake-IP OS DNS)
pub fn resolve_direct_ipv4_cached(host: &str) -> Option<Ipv4Addr> {
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return Some(ip);
    }
    let key = host.to_ascii_lowercase();
    if let Ok(cache) = dns_cache().lock() {
        if let Some(&(ip, ts)) = cache.get(&key) {
            if ts.elapsed() < Duration::from_secs(600) {
                return Some(ip);
            }
        }
    }
    format!("{}:80", host)
        .to_socket_addrs()
        .ok()?
        .find_map(|a| match a {
            SocketAddr::V4(v4) if !is_clash_fake_ip(*v4.ip()) => Some(*v4.ip()),
            _ => None,
        })
}

/// Connect a TcpStream to a SocketAddr bound to the physical LAN NIC so Clash TUN does NOT hijack traffic!
async fn connect_addr_direct(target: SocketAddr, timeout: Duration) -> Result<TcpStream> {
    if let (SocketAddr::V4(_), Some(lan_ip)) = (target, detect_best_local_ipv4()) {
        if let Ok(sock) = TcpSocket::new_v4() {
            if sock.bind(SocketAddr::new(IpAddr::V4(lan_ip), 0)).is_ok() {
                if let Ok(Ok(stream)) = tokio::time::timeout(timeout, sock.connect(target)).await {
                    let _ = stream.set_nodelay(true);
                    return Ok(stream);
                }
            }
        }
    }
    // Graceful fallback to default routing if physical NIC bind fails
    let stream = tokio::time::timeout(timeout, TcpStream::connect(target))
        .await
        .map_err(|_| anyhow!("Connection to {} timed out", target))?
        .map_err(|e| anyhow!("Failed to connect to {}: {}", target, e))?;
    let _ = stream.set_nodelay(true);
    Ok(stream)
}

/// Connect directly to host:port bypassing Clash Fake-IP DNS and Clash TUN interface!
pub async fn connect_tcp_direct(host: &str, port: u16, timeout: Duration) -> Result<TcpStream> {
    if let Some(real_ip) = resolve_direct_ipv4(host).await {
        let target = SocketAddr::new(IpAddr::V4(real_ip), port);
        if let Ok(stream) = connect_addr_direct(target, timeout).await {
            return Ok(stream);
        }
    }
    let addr = format!("{}:{}", host, port);
    let stream = tokio::time::timeout(timeout, TcpStream::connect(&addr))
        .await
        .map_err(|_| anyhow!("Connection to {} timed out", addr))?
        .map_err(|e| anyhow!("Failed to connect to {}: {}", addr, e))?;
    let _ = stream.set_nodelay(true);
    Ok(stream)
}

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

/// Parse port number from HTTP signaling URL (defaults to 80)
pub fn parse_server_port(url_str: &str) -> u16 {
    let cleaned = url_str
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_end_matches('/');
    cleaned
        .split(':')
        .nth(1)
        .and_then(|p| p.split('/').next())
        .and_then(|p| p.parse().ok())
        .unwrap_or(80)
}

/// Helper for sending raw HTTP requests and returning `(status_code, body)` (auto-bypasses Clash TUN!)
async fn send_http_request_with_status(
    method: &str,
    url_str: &str,
    path: &str,
    body: Option<&str>,
) -> Result<(u16, String)> {
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

    let mut stream = connect_tcp_direct(host, port, Duration::from_secs(3)).await?;

    let content_len = body.map(|b| b.len()).unwrap_or(0);
    let request_header = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        method, path, host, content_len
    );

    stream.write_all(request_header.as_bytes()).await?;
    if let Some(b) = body {
        stream.write_all(b.as_bytes()).await?;
    }

    // Read response incrementally and return IMMEDIATELY once Content-Length bytes arrive
    // (avoids waiting 3s for proxies/keep-alive sockets that delay TCP FIN!)
    let mut response_bytes = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    let mut header_end: Option<usize> = None;
    let mut expected_body_len: Option<usize> = None;

    let read_fut = async {
        loop {
            match stream.read(&mut chunk).await {
                Ok(0) => break,
                Ok(n) => {
                    response_bytes.extend_from_slice(&chunk[..n]);
                    if header_end.is_none() {
                        if let Some(pos) = response_bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                            let h_end = pos + 4;
                            header_end = Some(h_end);
                            let hdr_str = String::from_utf8_lossy(&response_bytes[..pos]);
                            for line in hdr_str.lines() {
                                if let Some((k, v)) = line.split_once(':') {
                                    if k.trim().eq_ignore_ascii_case("content-length") {
                                        if let Ok(len) = v.trim().parse::<usize>() {
                                            expected_body_len = Some(len);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if let (Some(h_end), Some(cl)) = (header_end, expected_body_len) {
                        if response_bytes.len() >= h_end + cl {
                            break;
                        }
                    }
                }
                Err(_) => break,
            }
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(3), read_fut).await;

    let response_str = String::from_utf8_lossy(&response_bytes);
    let status_code = response_str
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);

    let body_str = if let Some(pos) = response_str.find("\r\n\r\n") {
        &response_str[pos + 4..]
    } else {
        &response_str
    };

    // Robust JSON extraction: strip HTTP Chunked transfer encoding or proxy noise
    if let (Some(start), Some(end)) = (body_str.find('{'), body_str.rfind('}')) {
        if start <= end {
            return Ok((status_code, body_str[start..=end].to_string()));
        }
    }

    Ok((status_code, body_str.trim().to_string()))
}

async fn send_http_request(
    method: &str,
    url_str: &str,
    path: &str,
    body: Option<&str>,
) -> Result<String> {
    let (_status, body) = send_http_request_with_status(method, url_str, path, body).await?;
    Ok(body)
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

/// Detect the active preferred global unicast IPv6 address (starting with 2 or 3, e.g. 2408:..., 2409:..., 240e:...).
/// Runs the OS routing-table UDP connect probe FIRST so Deprecated SLAAC or Clash `fdfe:` TUN addresses are skipped!
pub fn detect_public_ipv6() -> Option<String> {
    for target in &["[2400:3200::1]:53", "[240e:4c:4008::1]:53", "[2001:4860:4860::8888]:53"] {
        if let Ok(socket) = std::net::UdpSocket::bind("[::]:0") {
            if socket.connect(target).is_ok() {
                if let Ok(local) = socket.local_addr() {
                    let ip = local.ip();
                    if ip.is_ipv6() && !ip.is_loopback() {
                        let s = ip.to_string();
                        if (s.starts_with('2') || s.starts_with('3')) && !s.starts_with("fe80") && !s.starts_with("fd") {
                            return Some(s);
                        }
                    }
                }
            }
        }
    }

    // Fallback: scan interfaces, preferring shorter/DHCPv6 addresses over long SLAAC temporary addresses
    if let Ok(interfaces) = local_ip_address::list_afinet_netifas() {
        let mut best: Option<String> = None;
        for (_name, ip) in interfaces {
            if let std::net::IpAddr::V6(ipv6) = ip {
                let s = ipv6.to_string();
                if (s.starts_with('2') || s.starts_with('3')) && !s.starts_with("fe80") && !s.starts_with("fd") {
                    if best.as_ref().map(|b| s.len() < b.len()).unwrap_or(true) {
                        best = Some(s);
                    }
                }
            }
        }
        if best.is_some() {
            return best;
        }
    }
    None
}

/// Register local device to the signaling server (including STUN public IP:port and IPv6)
pub async fn register_to_signaling_with_stun(
    server_url: &str,
    device_id: &str,
    device_code: &str,
    device_name: &str,
    local_ip: &str,
    listen_port: u16,
    ipv6: Option<String>,
    stun_ip: Option<&str>,
    stun_port: Option<u16>,
    auth_token: Option<&str>,
) -> Result<()> {
    let payload = serde_json::json!({
        "device_id": device_id,
        "device_code": device_code,
        "device_name": device_name,
        "local_ip": local_ip,
        "listen_port": listen_port,
        "ipv6": ipv6,
        "stun_ip": stun_ip,
        "stun_port": stun_port,
        "auth_token": auth_token,
    });

    let body = payload.to_string();
    let _ = send_http_request("POST", server_url, "/api/register", Some(&body)).await?;
    log::info!(
        "Registered device {} with signaling server {} (STUN: {:?}:{:?}, IPv6: {:?})",
        device_code,
        server_url,
        stun_ip,
        stun_port,
        ipv6
    );
    Ok(())
}

/// Register local device to the signaling server (legacy wrapper)
pub async fn register_to_signaling(
    server_url: &str,
    device_id: &str,
    device_code: &str,
    device_name: &str,
    local_ip: &str,
    listen_port: u16,
    ipv6: Option<String>,
) -> Result<()> {
    register_to_signaling_with_stun(
        server_url, device_id, device_code, device_name, local_ip, listen_port, ipv6, None, None, None,
    )
    .await
}

/// Ask signaling server to instruct target host to punch UDP hole toward our controller STUN IP:Port
pub async fn request_udp_punch(
    server_url: &str,
    target_code: &str,
    ctrl_ip: &str,
    ctrl_port: u16,
) -> Result<()> {
    let payload = serde_json::json!({
        "target_code": clean_code(target_code),
        "ctrl_ip": ctrl_ip,
        "ctrl_port": ctrl_port,
    });
    let (status, body) = send_http_request_with_status(
        "POST",
        server_url,
        "/api/punch",
        Some(&payload.to_string()),
    )
    .await?;
    if status == 0 || status >= 400 {
        return Err(anyhow!("HTTP {} from /api/punch: {}", status, body));
    }
    Ok(())
}

/// Poll signaling server for incoming UDP hole-punching targets from controllers
pub async fn poll_udp_punches(
    server_url: &str,
    device_code: &str,
) -> Vec<std::net::SocketAddr> {
    let code = clean_code(device_code);
    let path = format!("/api/punch/poll/{}", code);
    let (status, res) = match send_http_request_with_status("GET", server_url, &path, None).await {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    if status < 200 || status >= 300 {
        return Vec::new();
    }
    let json: serde_json::Value = match serde_json::from_str(&res) {
        Ok(j) => j,
        Err(_) => return Vec::new(),
    };
    let mut addrs = Vec::new();
    if let Some(arr) = json.get("punches").and_then(|v| v.as_array()) {
        for item in arr {
            if let (Some(ip), Some(port)) = (
                item.get("ctrlIp").and_then(|v| v.as_str()),
                item.get("ctrlPort").and_then(|v| v.as_u64()),
            ) {
                if let Ok(addr) = format!("{}:{}", ip, port).parse::<std::net::SocketAddr>() {
                    addrs.push(addr);
                }
            }
        }
    }
    addrs
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

/// Login to signaling server and retrieve (username, token)
pub async fn auth_login(server_url: &str, username: &str, password: &str) -> Result<(String, String)> {
    let payload = serde_json::json!({
        "username": username,
        "password": password,
    });
    let res = send_http_request("POST", server_url, "/api/auth/login", Some(&payload.to_string())).await?;
    let json: serde_json::Value = serde_json::from_str(&res)
        .map_err(|e| anyhow!("服务器登录响应解析失败: {} (raw: {})", e, res))?;

    if json.get("success").and_then(|v| v.as_bool()).unwrap_or(false) {
        let user = json
            .get("username")
            .and_then(|v| v.as_str())
            .unwrap_or(username)
            .to_string();
        let token = json
            .get("token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("服务器未返回有效登录凭证 Token"))?
            .to_string();
        Ok((user, token))
    } else {
        let err_msg = json
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("登录失败，请检查账号或密码");
        Err(anyhow!("{}", err_msg))
    }
}

/// Register a new account on the signaling server and retrieve (username, token)
pub async fn auth_register(server_url: &str, username: &str, password: &str) -> Result<(String, String)> {
    let payload = serde_json::json!({
        "username": username,
        "password": password,
    });
    let res = send_http_request("POST", server_url, "/api/auth/register", Some(&payload.to_string())).await?;
    let json: serde_json::Value = serde_json::from_str(&res)
        .map_err(|e| anyhow!("服务器注册响应解析失败: {} (raw: {})", e, res))?;

    if json.get("success").and_then(|v| v.as_bool()).unwrap_or(false) {
        let user = json
            .get("username")
            .and_then(|v| v.as_str())
            .unwrap_or(username)
            .to_string();
        let token = json
            .get("token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("服务器未返回有效登录凭证 Token"))?
            .to_string();
        Ok((user, token))
    } else {
        let err_msg = json
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("注册失败，请稍后重试");
        Err(anyhow!("{}", err_msg))
    }
}

fn split_host_port(addr: &str, default_port: u16) -> (&str, u16) {
    if let Some((h, p)) = addr.rsplit_once(':') {
        (h, p.parse().unwrap_or(default_port))
    } else {
        (addr, default_port)
    }
}

/// Connect to TCP Relay server and join a session by device code with authentication token
pub async fn join_relay_tunnel_with_token(
    relay_addr: &str,
    device_code: &str,
    auth_token: Option<&str>,
) -> Result<TcpStream> {
    let token = auth_token
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| anyhow!("请先登录账号！连接云端中继服务器需要账号登录鉴权"))?;

    let clean = clean_code(device_code);
    let (host, port) = split_host_port(relay_addr, 28845);
    let mut stream = connect_tcp_direct(host, port, Duration::from_secs(4))
        .await
        .map_err(|e| anyhow!("无法连接中继服务器 {}: {}", relay_addr, e))?;

    // Protocol: "OPRM_RELAY <sessionId> CTRL <token>\n"
    let cmd = format!("OPRM_RELAY {} CTRL {}\n", clean, token);
    stream.write_all(cmd.as_bytes()).await?;
    stream.flush().await?;

    Ok(stream)
}

/// Connect to TCP Relay server and join a session by device code (legacy helper for CLI tests)
pub async fn join_relay_tunnel(relay_addr: &str, device_code: &str) -> Result<TcpStream> {
    let clean = clean_code(device_code);
    let (host, port) = split_host_port(relay_addr, 28845);
    let mut stream = connect_tcp_direct(host, port, Duration::from_secs(4))
        .await
        .map_err(|e| anyhow!("无法连接中继服务器 {}: {}", relay_addr, e))?;

    // Protocol: "OPRM_RELAY <sessionId> CTRL\n"
    let cmd = format!("OPRM_RELAY {} CTRL\n", clean);
    stream.write_all(cmd.as_bytes()).await?;
    stream.flush().await?;

    Ok(stream)
}

/// Connect to TCP Relay server and register as host waiting for peer join (with optional token)
pub async fn register_relay_tunnel_with_token(
    relay_addr: &str,
    device_code: &str,
    auth_token: Option<&str>,
) -> Result<TcpStream> {
    let clean = clean_code(device_code);
    let (host, port) = split_host_port(relay_addr, 28845);
    let mut stream = connect_tcp_direct(host, port, Duration::from_secs(4))
        .await
        .map_err(|e| anyhow!("无法连接中继服务器 {}: {}", relay_addr, e))?;

    let cmd = if let Some(tok) = auth_token.map(|t| t.trim()).filter(|t| !t.is_empty()) {
        format!("OPRM_RELAY {} HOST {}\n", clean, tok)
    } else {
        format!("OPRM_RELAY {} HOST\n", clean)
    };
    stream.write_all(cmd.as_bytes()).await?;
    stream.flush().await?;

    Ok(stream)
}

/// Connect to TCP Relay server and register as host waiting for peer join
pub async fn register_relay_tunnel(relay_addr: &str, device_code: &str) -> Result<TcpStream> {
    register_relay_tunnel_with_token(relay_addr, device_code, None).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_direct_bypass_clash_dns_and_stun() {
        let t0 = Instant::now();
        let udp_dns = query_udp_dns_a_direct("dxcool.cn", "223.5.5.5:53");
        println!("Direct 223.5.5.5 UDP DNS dxcool.cn -> {:?} in {}ms", udp_dns, t0.elapsed().as_millis());
        let udp_ip = udp_dns.expect("Direct 223.5.5.5 UDP DNS should resolve dxcool.cn");
        assert!(!is_clash_fake_ip(udp_ip), "UDP DNS must not return Clash Fake-IP!");

        let ip = resolve_direct_ipv4("dxcool.cn").await.expect("resolve dxcool.cn failed");
        let dns_ms = t0.elapsed().as_millis();
        println!("Direct resolved dxcool.cn -> {} in {}ms (is_fake={})", ip, dns_ms, is_clash_fake_ip(ip));
        assert!(!is_clash_fake_ip(ip), "Must bypass Clash Fake-IP 198.18.x.x!");

        let t1 = Instant::now();
        let res = send_http_request("GET", "http://dxcool.cn:28844", "/api/status", None)
            .await
            .expect("direct signaling /api/status failed");
        let http_ms = t1.elapsed().as_millis();
        println!("Direct /api/status HTTP took {}ms (len={})", http_ms, res.len());
        assert!(res.contains("online"));

        let sock = crate::nat::bind_udp_direct(0).expect("bind_udp_direct failed");
        let t2 = Instant::now();
        let stun = crate::nat::probe_stun_direct(&sock, "http://dxcool.cn:28844").await;
        println!("Direct STUN probe took {}ms -> {:?}", t2.elapsed().as_millis(), stun);
        assert!(stun.is_some(), "Direct STUN should return real public IPv4!");
    }
}
