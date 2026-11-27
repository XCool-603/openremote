use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Automatically ensure Windows Defender Firewall allows inbound UDP/TCP for the current executable
/// Uses distinct rule names for SYSTEM Service vs GUI so installing/running one never deletes the other's rule!
pub fn ensure_windows_firewall_rule() {
    #[cfg(target_os = "windows")]
    {
        std::thread::spawn(|| {
            if let Ok(exe_path) = std::env::current_exe() {
                let exe_str = exe_path.to_string_lossy().to_string();
                let is_service_bin = crate::service::is_running_as_system()
                    || exe_str.to_ascii_lowercase().contains("programdata\\openremote");
                let rule_name = if is_service_bin {
                    "OpenRemote-P2P-Service"
                } else {
                    "OpenRemote-P2P-GUI"
                };
                let name_arg = format!("name={}", rule_name);
                use std::os::windows::process::CommandExt;
                const CREATE_NO_WINDOW: u32 = 0x0800_0000;

                // Silently delete stale rule for this role and add fresh rule for current exe path
                let _ = std::process::Command::new("netsh")
                    .args(["advfirewall", "firewall", "delete", "rule", &name_arg])
                    .creation_flags(CREATE_NO_WINDOW)
                    .output();

                let res = std::process::Command::new("netsh")
                    .args([
                        "advfirewall",
                        "firewall",
                        "add",
                        "rule",
                        &name_arg,
                        "dir=in",
                        "action=allow",
                        &format!("program={}", exe_str),
                        "enable=yes",
                        "profile=any",
                    ])
                    .creation_flags(CREATE_NO_WINDOW)
                    .output();

                if let Ok(out) = res {
                    if out.status.success() {
                        log::info!("Auto-configured Windows Firewall inbound rule {} for {}", rule_name, exe_str);
                    }
                }
            }
        });
    }
}

/// Parse STUN server UDP address using Clash-bypass direct IPv4 resolution (never 198.18.x.x Fake-IP)
pub async fn resolve_stun_addr_direct(signal_url: &str) -> Option<SocketAddr> {
    let cleaned = signal_url
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_end_matches('/');
    let (host, port) = if let Some((h, p)) = cleaned.rsplit_once(':') {
        (h, p.parse::<u16>().unwrap_or(28844))
    } else {
        (cleaned, 28844)
    };
    let ip = crate::signaling::resolve_direct_ipv4(host).await?;
    Some(SocketAddr::new(std::net::IpAddr::V4(ip), port))
}

/// Parse STUN server UDP address from signaling server URL (prefers cached direct IPv4)
pub fn parse_stun_addr(signal_url: &str) -> Option<SocketAddr> {
    let cleaned = signal_url
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_end_matches('/');
    let (host, port) = if let Some((h, p)) = cleaned.rsplit_once(':') {
        (h, p.parse::<u16>().unwrap_or(28844))
    } else {
        (cleaned, 28844)
    };
    if let Some(ip) = crate::signaling::resolve_direct_ipv4_cached(host) {
        return Some(SocketAddr::new(std::net::IpAddr::V4(ip), port));
    }
    format!("{}:{}", host, port)
        .to_socket_addrs()
        .ok()?
        .find(|a| a.is_ipv4())
}

fn is_usable_public_ipv4(ip_str: &str) -> bool {
    if let Ok(ip) = ip_str.parse::<std::net::Ipv4Addr>() {
        let oct = ip.octets();
        let is_fake_tun = oct[0] == 198 && (oct[1] == 18 || oct[1] == 19);
        !ip.is_private()
            && !ip.is_loopback()
            && !ip.is_link_local()
            && !ip.is_unspecified()
            && !ip.is_multicast()
            && !is_fake_tun
    } else {
        false
    }
}

/// Bind a UDP IPv4 socket STRICTLY on the real physical LAN interface (bypassing Clash/Mihomo TUN 198.18.x.x!).
/// Never silently falls back to 0.0.0.0 when a physical LAN IP exists (so busy ports fail cleanly and increment).
pub fn bind_udp_direct(port: u16) -> std::io::Result<UdpSocket> {
    if let Some(lan_ip) = crate::signaling::detect_best_local_ipv4() {
        let addr = SocketAddr::new(std::net::IpAddr::V4(lan_ip), port);
        return UdpSocket::bind(addr);
    }
    UdpSocket::bind(format!("0.0.0.0:{}", port))
}

/// Probe a standard RFC 5389 STUN server by direct SocketAddr and parse XOR-MAPPED-ADDRESS / MAPPED-ADDRESS
fn probe_rfc5389_stun_addr(socket: &UdpSocket, addr: SocketAddr) -> Option<(String, u16)> {
    let mut req = [0u8; 20];
    // Binding Request (0x0001), length 0, Magic Cookie 0x2112A442
    req[0] = 0x00;
    req[1] = 0x01;
    req[2] = 0x00;
    req[3] = 0x00;
    req[4..8].copy_from_slice(&0x2112_A442u32.to_be_bytes());
    let tx_id: [u8; 12] = rand::random();
    req[8..20].copy_from_slice(&tx_id);

    socket.send_to(&req, addr).ok()?;
    let mut buf = [0u8; 512];
    let (n, _) = match socket.recv_from(&mut buf) {
        Ok(res) => res,
        Err(_) => {
            // Fast 1x retry in case initial UDP packet dropped on busy Wi-Fi / NAT
            socket.send_to(&req, addr).ok()?;
            socket.recv_from(&mut buf).ok()?
        }
    };
    if n < 20 || buf[0] != 0x01 || buf[1] != 0x01 || &buf[8..20] != &tx_id {
        return None;
    }

    let mut off = 20usize;
    while off + 4 <= n {
        let attr_type = u16::from_be_bytes([buf[off], buf[off + 1]]);
        let attr_len = u16::from_be_bytes([buf[off + 2], buf[off + 3]]) as usize;
        let val_start = off + 4;
        if val_start + attr_len > n {
            break;
        }
        let val = &buf[val_start..val_start + attr_len];
        // XOR-MAPPED-ADDRESS (0x0020), IPv4 family (0x01)
        if attr_type == 0x0020 && val.len() >= 8 && val[1] == 0x01 {
            let port = u16::from_be_bytes([val[2], val[3]]) ^ 0x2112;
            let ip = format!(
                "{}.{}.{}.{}",
                val[4] ^ 0x21,
                val[5] ^ 0x12,
                val[6] ^ 0xA4,
                val[7] ^ 0x42
            );
            if is_usable_public_ipv4(&ip) && port > 0 {
                return Some((ip, port));
            }
        }
        // MAPPED-ADDRESS (0x0001), IPv4 family (0x01)
        if attr_type == 0x0001 && val.len() >= 8 && val[1] == 0x01 {
            let port = u16::from_be_bytes([val[2], val[3]]);
            let ip = format!("{}.{}.{}.{}", val[4], val[5], val[6], val[7]);
            if is_usable_public_ipv4(&ip) && port > 0 {
                return Some((ip, port));
            }
        }
        off = val_start + ((attr_len + 3) & !3);
    }
    None
}

#[derive(Debug, Clone)]
pub struct StunDiagnostic {
    pub rfc_mapped: Option<(String, u16)>,
    pub custom_mapped: Option<(String, u16)>,
    pub nat_type: String,
}

impl StunDiagnostic {
    pub fn best_addr(&self) -> Option<(String, u16)> {
        // Prefer custom signaling-server STUN first: when Controller is behind Symmetric NAT,
        // signaling IP's NAT mapping is closest to the Host WAN mapping (or identical IP).
        self.custom_mapped.clone().or_else(|| self.rfc_mapped.clone())
    }
}

/// Probe both public RFC 5389 STUN and OpenRemote signaling STUN on the same bound socket
/// to detect Cone vs Symmetric NAT behavior cleanly while bypassing Clash TUN/Fake-IP!
pub async fn probe_stun_detailed_direct(socket: &UdpSocket, signal_url: &str) -> StunDiagnostic {
    let cleaned = signal_url
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_end_matches('/');
    let (sig_host, sig_port) = if let Some((h, p)) = cleaned.rsplit_once(':') {
        (h, p.parse::<u16>().unwrap_or(28844))
    } else {
        (cleaned, 28844)
    };

    let custom_stun_addr = crate::signaling::resolve_direct_ipv4(sig_host)
        .await
        .map(|ip| SocketAddr::new(std::net::IpAddr::V4(ip), sig_port));

    let mut rfc_addrs = Vec::new();
    for (host, port) in &[
        ("stun.miwifi.com", 3478u16),
        ("stun.chat.bilibili.com", 3478u16),
        ("stun.qq.com", 3478u16),
    ] {
        if let Some(ip) = crate::signaling::resolve_direct_ipv4(host).await {
            rfc_addrs.push(SocketAddr::new(std::net::IpAddr::V4(ip), *port));
        }
    }

    let sock_clone = match socket.try_clone() {
        Ok(s) => s,
        Err(_) => {
            return StunDiagnostic {
                rfc_mapped: None,
                custom_mapped: None,
                nat_type: "SocketCloneError".to_string(),
            }
        }
    };

    tokio::task::spawn_blocking(move || {
        let orig_timeout = sock_clone.read_timeout().ok().flatten();
        let _ = sock_clone.set_read_timeout(Some(Duration::from_millis(700)));
        let mut rfc_mapped = None;
        let mut custom_mapped = None;

        for addr in rfc_addrs {
            if let Some((ip, port)) = probe_rfc5389_stun_addr(&sock_clone, addr) {
                log::info!("Direct RFC 5389 STUN ({}) succeeded: {}:{}", addr, ip, port);
                rfc_mapped = Some((ip, port));
                break;
            }
        }

        if let Some(stun_addr) = custom_stun_addr {
            for _ in 0..2 {
                if sock_clone.send_to(b"STUN", stun_addr).is_ok() {
                    let mut buf = [0u8; 512];
                    if let Ok((n, _)) = sock_clone.recv_from(&mut buf) {
                        if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&buf[..n]) {
                            let ip = json.get("ip").and_then(|v| v.as_str()).map(|s| s.to_string());
                            let port = json.get("port").and_then(|v| v.as_u64()).map(|p| p as u16);
                            if let (Some(ip), Some(port)) = (ip, port) {
                                if is_usable_public_ipv4(&ip) && port > 0 {
                                    log::info!("Direct custom STUN succeeded: {}:{}", ip, port);
                                    custom_mapped = Some((ip, port));
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }

        let _ = sock_clone.set_read_timeout(orig_timeout);

        let nat_type = match (&rfc_mapped, &custom_mapped) {
            (Some((ip1, p1)), Some((ip2, p2))) => {
                if ip1 == ip2 && p1 == p2 {
                    "Cone NAT (端口一致，支持 P2P 打洞)".to_string()
                } else if ip1 == ip2 {
                    let delta = (*p2 as i32) - (*p1 as i32);
                    format!("Symmetric NAT (端口变化 delta={:+}, {} -> {})", delta, p1, p2)
                } else {
                    format!("Multi-WAN NAT ({}:{} vs {}:{})", ip1, p1, ip2, p2)
                }
            }
            (Some(_), None) => "RFC-STUN-Only (信令UDP未回包)".to_string(),
            (None, Some(_)) => "Custom-STUN-Only (公网STUN未回包)".to_string(),
            (None, None) => "UDP Blocked / STUN Failed".to_string(),
        };

        StunDiagnostic {
            rfc_mapped,
            custom_mapped,
            nat_type,
        }
    })
    .await
    .unwrap_or(StunDiagnostic {
        rfc_mapped: None,
        custom_mapped: None,
        nat_type: "STUN Task Panicked".to_string(),
    })
}

/// Async direct STUN probe that bypasses Clash Fake-IP DNS and Clash TUN completely!
pub async fn probe_stun_direct(socket: &UdpSocket, signal_url: &str) -> Option<(String, u16)> {
    probe_stun_detailed_direct(socket, signal_url)
        .await
        .best_addr()
}

/// Probe a standard RFC 5389 STUN server on the given UdpSocket and parse XOR-MAPPED-ADDRESS / MAPPED-ADDRESS
fn probe_rfc5389_stun(socket: &UdpSocket, stun_host_port: &str) -> Option<(String, u16)> {
    let addr = stun_host_port.to_socket_addrs().ok()?.find(|a| a.is_ipv4())?;
    probe_rfc5389_stun_addr(socket, addr)
}

/// Probe real public IPv4 and NAT-mapped port using the exact UdpSocket before handing it to Quinn.
/// Tries OpenRemote custom STUN first, then falls back to public RFC 5389 STUN servers.
pub fn probe_stun_on_socket(socket: &UdpSocket, signal_url: &str) -> Option<(String, u16)> {
    let orig_timeout = socket.read_timeout().ok().flatten();
    let _ = socket.set_read_timeout(Some(Duration::from_millis(800)));

    let mut result = None;
    if let Some(stun_addr) = parse_stun_addr(signal_url) {
        for _ in 0..2 {
            if socket.send_to(b"STUN", stun_addr).is_ok() {
                let mut buf = [0u8; 512];
                if let Ok((n, _from)) = socket.recv_from(&mut buf) {
                    if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&buf[..n]) {
                        let ip = json.get("ip").and_then(|v| v.as_str()).map(|s| s.to_string());
                        let port = json.get("port").and_then(|v| v.as_u64()).map(|p| p as u16);
                        if let (Some(ip), Some(port)) = (ip, port) {
                            if is_usable_public_ipv4(&ip) && port > 0 {
                                log::info!("Custom STUN probe succeeded on bound socket: {}:{}", ip, port);
                                result = Some((ip, port));
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    if result.is_none() {
        for server in &[
            "stun.miwifi.com:3478",
            "stun.chat.bilibili.com:3478",
            "stun.qq.com:3478",
        ] {
            if let Some((ip, port)) = probe_rfc5389_stun(socket, server) {
                log::info!("RFC 5389 STUN ({}) probe succeeded: {}:{}", server, ip, port);
                result = Some((ip, port));
                break;
            }
        }
    }

    let _ = socket.set_read_timeout(orig_timeout);
    result
}

/// Automatically request UPnP IGD UDP port mapping on the local router (zero manual router config)
pub async fn try_upnp_port_mapping(local_ip: &str, port: u16) -> bool {
    let ssdp_addr: SocketAddr = "239.255.255.250:1900".parse().unwrap();
    let search_targets = [
        "urn:schemas-upnp-org:service:WANIPConnection:1",
        "urn:schemas-upnp-org:service:WANPPPConnection:1",
    ];

    let socket = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(_) => return false,
    };
    let _ = socket.set_read_timeout(Some(Duration::from_millis(1000)));

    let mut location_url = None;
    for st in &search_targets {
        let req = format!(
            "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {}\r\n\r\n",
            st
        );
        let _ = socket.send_to(req.as_bytes(), ssdp_addr);
        let mut buf = [0u8; 2048];
        if let Ok((n, _)) = socket.recv_from(&mut buf) {
            let text = String::from_utf8_lossy(&buf[..n]);
            for line in text.lines() {
                if line.to_ascii_lowercase().starts_with("location:") {
                    location_url = Some(line[9..].trim().to_string());
                    break;
                }
            }
        }
        if location_url.is_some() {
            break;
        }
    }

    let loc = match location_url {
        Some(u) => u,
        None => {
            log::debug!("UPnP: No IGD router found on LAN");
            return false;
        }
    };

    // Parse http://host:port/path from location
    let trimmed = loc.trim_start_matches("http://");
    let (host_port, desc_path) = match trimmed.find('/') {
        Some(idx) => (&trimmed[..idx], &trimmed[idx..]),
        None => (trimmed, "/"),
    };

    // Fetch device description XML to extract controlURL
    let xml = match http_raw("GET", host_port, desc_path, None, &[]).await {
        Ok(body) => body,
        Err(_) => return false,
    };

    let service_type = if xml.contains("WANIPConnection:1") {
        "urn:schemas-upnp-org:service:WANIPConnection:1"
    } else if xml.contains("WANPPPConnection:1") {
        "urn:schemas-upnp-org:service:WANPPPConnection:1"
    } else {
        return false;
    };

    let control_url = match extract_control_url(&xml, service_type) {
        Some(c) => c,
        None => return false,
    };

    let control_path = if control_url.starts_with("http://") {
        let t = control_url.trim_start_matches("http://");
        match t.find('/') {
            Some(i) => t[i..].to_string(),
            None => "/".to_string(),
        }
    } else if control_url.starts_with('/') {
        control_url
    } else {
        format!("/{}", control_url)
    };

    let soap_body = format!(
        "<?xml version=\"1.0\"?>\r\n\
        <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">\r\n\
        <s:Body>\r\n\
        <u:AddPortMapping xmlns:u=\"{}\">\r\n\
        <NewRemoteHost></NewRemoteHost>\r\n\
        <NewExternalPort>{}</NewExternalPort>\r\n\
        <NewProtocol>UDP</NewProtocol>\r\n\
        <NewInternalPort>{}</NewInternalPort>\r\n\
        <NewInternalClient>{}</NewInternalClient>\r\n\
        <NewEnabled>1</NewEnabled>\r\n\
        <NewPortMappingDescription>OpenRemote P2P</NewPortMappingDescription>\r\n\
        <NewLeaseDuration>0</NewLeaseDuration>\r\n\
        </u:AddPortMapping>\r\n\
        </s:Body>\r\n\
        </s:Envelope>",
        service_type, port, port, local_ip
    );

    let soap_action = format!("\"{}#AddPortMapping\"", service_type);
    match http_raw(
        "POST",
        host_port,
        &control_path,
        Some(&soap_body),
        &[
            ("Content-Type", "text/xml; charset=\"utf-8\""),
            ("SOAPAction", &soap_action),
        ],
    )
    .await
    {
        Ok(resp) if resp.contains("200 OK") || resp.contains("AddPortMappingResponse") => {
            log::info!("UPnP: Successfully mapped router UDP port {} -> {}:{}", port, local_ip, port);
            true
        }
        Ok(resp) => {
            log::debug!("UPnP AddPortMapping response: {}", resp.lines().next().unwrap_or(""));
            false
        }
        Err(_) => false,
    }
}

fn extract_control_url(xml: &str, service_type: &str) -> Option<String> {
    let pos = xml.find(service_type)?;
    let slice = &xml[pos..];
    let end = slice.find("</service>").unwrap_or(slice.len());
    let service_block = &slice[..end];
    let start_tag = "<controlURL>";
    let end_tag = "</controlURL>";
    let s = service_block.find(start_tag)? + start_tag.len();
    let e = service_block[s..].find(end_tag)? + s;
    Some(service_block[s..e].trim().to_string())
}

async fn http_raw(
    method: &str,
    host_port: &str,
    path: &str,
    body: Option<&str>,
    extra_headers: &[(&str, &str)],
) -> anyhow::Result<String> {
    let mut stream = tokio::time::timeout(Duration::from_secs(2), TcpStream::connect(host_port)).await??;
    let mut req = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
        method, path, host_port
    );
    for (k, v) in extra_headers {
        req.push_str(&format!("{}: {}\r\n", k, v));
    }
    if let Some(b) = body {
        req.push_str(&format!("Content-Length: {}\r\n\r\n{}", b.len(), b));
    } else {
        req.push_str("\r\n");
    }
    stream.write_all(req.as_bytes()).await?;
    let mut resp = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut resp)).await;
    Ok(String::from_utf8_lossy(&resp).to_string())
}
