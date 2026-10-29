use crate::protocol::{FrameHeader, Message};
use anyhow::{anyhow, Result};
use quinn::{RecvStream, SendStream};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

pub mod quic;

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
}

impl ConnectionChannel {
    /// Create a channel from a QUIC stream pair
    pub fn new(sender: SendStream, receiver: RecvStream) -> Self {
        Self {
            sender: Arc::new(Mutex::new(ChannelSender::Quic(sender))),
            receiver: Arc::new(Mutex::new(ChannelReceiver::Quic(receiver))),
        }
    }

    /// Create a channel from a full-duplex TCP stream (used by TCP Relay)
    pub fn new_tcp(stream: TcpStream) -> Self {
        let (reader, writer) = tokio::io::split(stream);
        Self {
            sender: Arc::new(Mutex::new(ChannelSender::Tcp(writer))),
            receiver: Arc::new(Mutex::new(ChannelReceiver::Tcp(reader))),
        }
    }

    /// Send a length-prefixed Message frame using high-performance bincode binary serialization
    pub async fn send(&self, message: &Message) -> Result<()> {
        let payload = bincode::serialize(message)
            .map_err(|e| anyhow!("Failed to serialize message with bincode: {}", e))?;

        let header = FrameHeader::new(1, payload.len() as u32);
        let header_bytes = header.to_bytes();

        let mut sender = self.sender.lock().await;
        match &mut *sender {
            ChannelSender::Quic(s) => {
                s.write_all(&header_bytes).await?;
                s.write_all(&payload).await?;
            }
            ChannelSender::Tcp(w) => {
                w.write_all(&header_bytes).await?;
                w.write_all(&payload).await?;
                w.flush().await?;
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
