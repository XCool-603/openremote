use anyhow::{anyhow, Context, Result};
use std::net::UdpSocket;

/// Parse MAC address formatted with colons or hyphens (e.g., "AA:BB:CC:DD:EE:FF" or "aa-bb-cc-dd-ee-ff")
pub fn parse_mac_address(mac_str: &str) -> Result<[u8; 6]> {
    let clean = mac_str.replace([':', '-'], "");
    if clean.len() != 12 {
        return Err(anyhow!(
            "Invalid MAC address format: expected 12 hex characters, got '{}'",
            mac_str
        ));
    }

    let mut mac = [0u8; 6];
    for i in 0..6 {
        mac[i] = u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16)
            .map_err(|e| anyhow!("Failed to parse hex in MAC address: {}", e))?;
    }

    Ok(mac)
}

/// Build Wake-on-LAN Magic Packet: 6 bytes of 0xFF followed by target MAC address repeated 16 times
pub fn build_magic_packet(mac: &[u8; 6]) -> [u8; 102] {
    let mut packet = [0u8; 102];
    packet[..6].fill(0xFF);

    for i in 0..16 {
        let start = 6 + i * 6;
        packet[start..start + 6].copy_from_slice(mac);
    }

    packet
}

/// Send Wake-on-LAN magic packet to target MAC address via UDP broadcast
pub fn send_wol_packet(mac_address: &str) -> Result<()> {
    let mac = parse_mac_address(mac_address)?;
    let packet = build_magic_packet(&mac);

    let socket = UdpSocket::bind("0.0.0.0:0")
        .context("Failed to bind UDP socket for Wake-on-LAN broadcast")?;
    socket
        .set_broadcast(true)
        .context("Failed to enable broadcast on UDP socket")?;

    // Broadcast on port 9 (standard WoL discard port) and port 7
    let target_addrs = ["255.255.255.255:9", "255.255.255.255:7"];
    for addr in target_addrs {
        let _ = socket.send_to(&packet, addr);
    }

    log::info!("Wake-on-LAN magic packet sent for {}", mac_address);
    Ok(())
}
