use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Automatically ensure Windows Defender Firewall allows inbound UDP/TCP for the current executable
pub fn ensure_windows_firewall_rule() {
    #[cfg(target_os = "windows")]
    {
        std::thread::spawn(|| {
            if let Ok(exe_path) = std::env::current_exe() {
                let exe_str = exe_path.to_string_lossy().to_string();
                use std::os::windows::process::CommandExt;
                const CREATE_NO_WINDOW: u32 = 0x08000000;

                // Silently delete stale rule and add fresh rule for current exe path (succeeds if elevated, harmless otherwise)
                let _ = std::process::Command::new("netsh")
                    .args([
                        "advfirewall",
                        "firewall",
                        "delete",
                        "rule",
                        "name=OpenRemote-P2P",
                    ])
                    .creation_flags(CREATE_NO_WINDOW)
                    .output();

                let res = std::process::Command::new("netsh")
                    .args([
                        "advfirewall",
                        "firewall",
                        "add",
                        "rule",
                        "name=OpenRemote-P2P",
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
                        log::info!("Auto-configured Windows Firewall inbound rule for {}", exe_str);
                    }
                }
            }
        });
    }
}

/// Parse STUN server UDP address from signaling server URL (e.g. "http://dxcool.cn:28844" -> "dxcool.cn:28844")
pub fn parse_stun_addr(signal_url: &str) -> Option<SocketAddr> {
    let cleaned = signal_url
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_end_matches('/');
    let host_port = if cleaned.contains(':') {
        cleaned.to_string()
    } else {
        format!("{}:28844", cleaned)
    };
    host_port.to_socket_addrs().ok()?.find(|a| a.is_ipv4())
}

/// Probe real public IPv4 and NAT-mapped port using the exact UdpSocket before handing it to Quinn
pub fn probe_stun_on_socket(socket: &UdpSocket, signal_url: &str) -> Option<(String, u16)> {
    let stun_addr = parse_stun_addr(signal_url)?;
    let orig_timeout = socket.read_timeout().ok().flatten();
    let _ = socket.set_read_timeout(Some(Duration::from_millis(800)));

    let mut result = None;
    for _ in 0..2 {
        if socket.send_to(b"STUN", stun_addr).is_ok() {
            let mut buf = [0u8; 512];
            if let Ok((n, _from)) = socket.recv_from(&mut buf) {
                if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&buf[..n]) {
                    let ip = json.get("ip").and_then(|v| v.as_str()).map(|s| s.to_string());
                    let port = json.get("port").and_then(|v| v.as_u64()).map(|p| p as u16);
                    if let (Some(ip), Some(port)) = (ip, port) {
                        log::info!("STUN probe succeeded on bound socket: {}:{}", ip, port);
                        result = Some((ip, port));
                        break;
                    }
                }
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
