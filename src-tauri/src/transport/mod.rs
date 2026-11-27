use crate::protocol::{FrameHeader, Message};
use anyhow::{anyhow, Result};
use quinn::{RecvStream, SendStream};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

pub mod quic;
pub mod quic_relay;

pub use quic::{QuicClient, QuicServer};

pub enum ChannelSender {
    Quic(SendStream),
    Tcp(WriteHalf<TcpStream>),
}

pub enum ChannelReceiver {
    Quic(RecvStream),
    Tcp(ReadHalf<TcpStream>),
}

/// Thread-safe wrapper for bi-directional connection channels (supports both QUIC and TCP Relay)
#[derive(Clone)]
pub struct ConnectionChannel {
    pub sender: Arc<Mutex<ChannelSender>>,
    pub receiver: Arc<Mutex<ChannelReceiver>>,
    quic_conn: Option<quinn::Connection>,
    peer_label: String,
}

impl ConnectionChannel {
    /// Create a channel from a QUIC connection and stream pair
    pub fn new(connection: quinn::Connection, sender: SendStream, receiver: RecvStream) -> Self {
        let peer_label = connection.remote_address().to_string();
        Self {
            sender: Arc::new(Mutex::new(ChannelSender::Quic(sender))),
            receiver: Arc::new(Mutex::new(ChannelReceiver::Quic(receiver))),
            quic_conn: Some(connection),
            peer_label,
        }
    }

    /// Create a channel from a full-duplex TCP stream (used by TCP Relay)
    pub fn new_tcp(stream: TcpStream) -> Self {
        let _ = stream.set_nodelay(true);
        let peer_label = stream
            .peer_addr()
            .map(|a| a.to_string())
            .unwrap_or_else(|_| "Relay".to_string());
        let (reader, writer) = tokio::io::split(stream);
        Self {
            sender: Arc::new(Mutex::new(ChannelSender::Tcp(writer))),
            receiver: Arc::new(Mutex::new(ChannelReceiver::Tcp(reader))),
            quic_conn: None,
            peer_label,
        }
    }

    /// Human-readable remote peer address/endpoint label
    pub fn peer_addr_label(&self) -> &str {
        &self.peer_label
    }

    pub fn is_quic(&self) -> bool {
        self.quic_conn.is_some()
    }

    /// Immediately close the underlying QUIC connection or TCP stream so the peer receives EOF in <1ms
    pub async fn close(&self) {
        if let Some(ref conn) = self.quic_conn {
            conn.close(0u32.into(), b"disconnect");
        }
        if let Ok(mut sender) = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            self.sender.lock(),
        )
        .await
        {
            match &mut *sender {
                ChannelSender::Quic(s) => {
                    let _ = s.finish();
                }
                ChannelSender::Tcp(w) => {
                    let _ = w.shutdown().await;
                }
            }
        }
    }

    /// Send a length-prefixed Message frame using high-performance bincode binary serialization
    /// Builds header+payload in a single contiguous buffer before locking so every frame is a single write.
    pub async fn send(&self, message: &Message) -> Result<()> {
        let payload_len = bincode::serialized_size(message)
            .map_err(|e| anyhow!("Failed to compute bincode size: {}", e))? as usize;

        let mut framed = Vec::with_capacity(10 + payload_len);
        let header = FrameHeader::new(1, payload_len as u32);
        framed.extend_from_slice(&header.to_bytes());
        bincode::serialize_into(&mut framed, message)
            .map_err(|e| anyhow!("Failed to serialize message with bincode: {}", e))?;

        let mut sender = self.sender.lock().await;
        match &mut *sender {
            ChannelSender::Quic(s) => {
                s.write_all(&framed).await?;
            }
            ChannelSender::Tcp(w) => {
                w.write_all(&framed).await?;
            }
        }
        Ok(())
    }

    /// Receive a length-prefixed Message frame using high-performance bincode binary deserialization
    pub async fn recv(&self) -> Result<Message> {
        let mut header_buf = [0u8; 10];
        let mut receiver = self.receiver.lock().await;

        let payload_len = match &mut *receiver {
            ChannelReceiver::Quic(r) => {
                r.read_exact(&mut header_buf).await?;
                let header = FrameHeader::from_bytes(&header_buf)
                    .ok_or_else(|| anyhow!("Invalid protocol frame header magic"))?;
                header.payload_len as usize
            }
            ChannelReceiver::Tcp(r) => {
                r.read_exact(&mut header_buf).await?;
                let header = FrameHeader::from_bytes(&header_buf)
                    .ok_or_else(|| anyhow!("Invalid protocol frame header magic"))?;
                header.payload_len as usize
            }
        };

        let mut payload = vec![0u8; payload_len];
        match &mut *receiver {
            ChannelReceiver::Quic(r) => {
                r.read_exact(&mut payload).await?;
            }
            ChannelReceiver::Tcp(r) => {
                r.read_exact(&mut payload).await?;
            }
        }

        let message: Message = bincode::deserialize(&payload)
            .map_err(|e| anyhow!("Failed to deserialize frame payload with bincode: {}", e))?;

        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::DirtyTile;
    use std::time::{Duration, Instant};

    #[tokio::test]
    async fn bench_quic_60fps_streaming_with_concurrent_ping() {
        #[cfg(target_os = "windows")]
        unsafe {
            #[link(name = "winmm")]
            extern "system" {
                fn timeBeginPeriod(uPeriod: u32) -> u32;
            }
            timeBeginPeriod(1);
        }

        let server = QuicServer::bind("127.0.0.1:0".parse().unwrap()).expect("bind server");
        let addr = server.listen_addr;

        let server_task = tokio::spawn(async move {
            let chan = server.accept_channel().await.expect("accept");
            let chan_rx = chan.clone();
            let pong_task = tokio::spawn(async move {
                while let Ok(msg) = chan_rx.recv().await {
                    if let Message::Ping { timestamp } = msg {
                        let _ = chan_rx.send(&Message::Pong { timestamp }).await;
                    }
                }
            });

            // Stream 60 frames of realistic 4-strip 1080p payloads (~80KB each)
            let tile_payload = vec![0xABu8; 20 * 1024];
            for _ in 0..60 {
                let tiles: Vec<DirtyTile> = (0..4)
                    .map(|i| DirtyTile {
                        x: 0,
                        y: (i * 270) as u16,
                        w: 1920,
                        h: 270,
                        data: tile_payload.clone(),
                    })
                    .collect();
                let msg = Message::TileFrames {
                    screen_w: 1920,
                    screen_h: 1080,
                    tiles,
                    timestamp: chrono::Utc::now().timestamp_millis(),
                };
                chan.send(&msg).await.expect("send frame");
                tokio::time::sleep(Duration::from_millis(16)).await;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            pong_task.abort();
        });

        let client = QuicClient::new_for_target(&addr).expect("client");
        let t_conn = Instant::now();
        let chan = client.connect(addr).await.expect("connect");
        let conn_ms = t_conn.elapsed().as_secs_f64() * 1000.0;

        let ping_chan = chan.clone();
        let ping_task = tokio::spawn(async move {
            for _ in 0..5 {
                let ts = chrono::Utc::now().timestamp_millis();
                if ping_chan.send(&Message::Ping { timestamp: ts }).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(180)).await;
            }
        });

        let t0 = Instant::now();
        let mut frames = 0usize;
        let mut total_bytes = 0usize;
        let mut rtts = Vec::new();

        while frames < 60 {
            match tokio::time::timeout(Duration::from_secs(3), chan.recv()).await {
                Ok(Ok(Message::TileFrames { tiles, .. })) => {
                    frames += 1;
                    total_bytes += tiles.iter().map(|t| t.data.len()).sum::<usize>();
                }
                Ok(Ok(Message::Pong { timestamp })) => {
                    let rtt = (chrono::Utc::now().timestamp_millis() - timestamp).max(0);
                    rtts.push(rtt);
                }
                Ok(Ok(_)) => {}
                Ok(Err(e)) => panic!("recv err: {}", e),
                Err(_) => panic!("timeout at frame {}", frames),
            }
        }
        let dur = t0.elapsed().as_secs_f64();
        ping_task.abort();
        server_task.abort();

        let fps = frames as f64 / dur;
        let mbps = (total_bytes as f64 * 8.0 / 1_000_000.0) / dur;
        println!(
            "E2E QUIC 60FPS Test: connect={:.2}ms, frames={}, fps={:.1}, throughput={:.1} Mbps, RTTs={:?}",
            conn_ms, frames, fps, mbps, rtts
        );
        assert_eq!(frames, 60);
        assert!(fps >= 45.0, "Expected smooth >=45 FPS, got {:.1}", fps);
        for &r in &rtts {
            assert!(r < 50, "Expected low ping RTT under 60FPS load, got {}ms", r);
        }
    }

    #[tokio::test]
    async fn test_rapid_disconnect_and_reconnect_5_times() {
        let server = Arc::new(QuicServer::bind("127.0.0.1:0".parse().unwrap()).expect("bind server"));
        let addr = server.listen_addr;

        // Non-blocking accept loop mirroring lib.rs production host behavior
        let srv_clone = Arc::clone(&server);
        let accept_task = tokio::spawn(async move {
            loop {
                match srv_clone.accept_incoming().await {
                    Ok(incoming) => {
                        tokio::spawn(async move {
                            if let Ok(chan) = QuicServer::finish_incoming(incoming).await {
                                while let Ok(msg) = chan.recv().await {
                                    if let Message::Ping { timestamp } = msg {
                                        let _ = chan.send(&Message::Pong { timestamp }).await;
                                    }
                                }
                                chan.close().await;
                            }
                        });
                    }
                    Err(_) => break,
                }
            }
        });

        for round in 1..=5 {
            let t0 = Instant::now();
            let client = QuicClient::new_for_target(&addr).expect("client");
            let chan = tokio::time::timeout(Duration::from_millis(500), client.connect(addr))
                .await
                .expect("connect timeout")
                .expect("connect failed");
            let conn_ms = t0.elapsed().as_secs_f64() * 1000.0;

            let ts = chrono::Utc::now().timestamp_millis();
            chan.send(&Message::Ping { timestamp: ts }).await.expect("send ping");
            let pong = tokio::time::timeout(Duration::from_millis(500), chan.recv())
                .await
                .expect("pong timeout")
                .expect("pong recv failed");
            match pong {
                Message::Pong { timestamp } => {
                    let rtt = (chrono::Utc::now().timestamp_millis() - timestamp).max(0);
                    println!("Reconnect round #{}: connect={:.2}ms, RTT={}ms", round, conn_ms, rtt);
                    assert!(conn_ms < 100.0, "Reconnect #{} too slow: {:.2}ms", round, conn_ms);
                }
                other => panic!("Unexpected msg: {:?}", other),
            }

            // Explicit close (mirrors commands::disconnect)
            chan.close().await;
        }

        accept_task.abort();
    }
}
