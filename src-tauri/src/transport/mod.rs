use crate::protocol::{FrameHeader, Message};
use anyhow::{anyhow, Result};
use quinn::{RecvStream, SendStream};
use std::sync::Arc;
use tokio::sync::Mutex;

pub mod quic;

pub use quic::{QuicClient, QuicServer};

/// High-level transport message wrapper
#[derive(Debug, Clone)]
pub struct TransportMessage {
    pub message: Message,
}

impl TransportMessage {
    pub fn new(message: Message) -> Self {
        Self { message }
    }
}

/// Send a length-prefixed Message frame over a Quinn SendStream using high-performance bincode binary serialization
pub async fn send_message(send_stream: &mut SendStream, message: &Message) -> Result<()> {
    let payload = bincode::serialize(message)
        .map_err(|e| anyhow!("Failed to serialize message with bincode: {}", e))?;

    let header = FrameHeader::new(1, payload.len() as u32);
    let header_bytes = header.to_bytes();

    send_stream
        .write_all(&header_bytes)
        .await
        .map_err(|e| anyhow!("Failed to write header: {}", e))?;

    send_stream
        .write_all(&payload)
        .await
        .map_err(|e| anyhow!("Failed to write payload: {}", e))?;

    Ok(())
}

/// Receive a length-prefixed Message frame from a Quinn RecvStream using high-performance bincode binary deserialization
pub async fn recv_message(recv_stream: &mut RecvStream) -> Result<Message> {
    let mut header_buf = [0u8; 10];
    recv_stream
        .read_exact(&mut header_buf)
        .await
        .map_err(|e| anyhow!("Failed to read frame header: {}", e))?;

    let header = FrameHeader::from_bytes(&header_buf)
        .ok_or_else(|| anyhow!("Invalid protocol frame header magic"))?;

    let mut payload = vec![0u8; header.payload_len as usize];
    recv_stream
        .read_exact(&mut payload)
        .await
        .map_err(|e| anyhow!("Failed to read frame payload: {}", e))?;

    let message: Message = bincode::deserialize(&payload)
        .map_err(|e| anyhow!("Failed to deserialize frame payload with bincode: {}", e))?;

    Ok(message)
}

/// Thread-safe wrapper for bi-directional connection streams
#[derive(Clone)]
pub struct ConnectionChannel {
    pub sender: Arc<Mutex<SendStream>>,
    pub receiver: Arc<Mutex<RecvStream>>,
}

impl ConnectionChannel {
    pub fn new(sender: SendStream, receiver: RecvStream) -> Self {
        Self {
            sender: Arc::new(Mutex::new(sender)),
            receiver: Arc::new(Mutex::new(receiver)),
        }
    }

    pub async fn send(&self, message: &Message) -> Result<()> {
        let mut sender = self.sender.lock().await;
        send_message(&mut sender, message).await
    }

    pub async fn recv(&self) -> Result<Message> {
        let mut receiver = self.receiver.lock().await;
        recv_message(&mut receiver).await
    }
}
