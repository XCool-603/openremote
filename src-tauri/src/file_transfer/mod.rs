use crate::protocol::Message;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

pub const CHUNK_SIZE: usize = 64 * 1024; // 64 KB

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferStatus {
    Pending,
    Transferring,
    Paused,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferProgress {
    pub id: String,
    pub filename: String,
    pub total_bytes: u64,
    pub transferred_bytes: u64,
    pub speed_bps: u64,
    pub status: TransferStatus,
}

/// Sender for streaming files across connections
pub struct FileTransferSender {
    pub id: String,
    pub file_path: PathBuf,
    pub total_bytes: u64,
    pub transferred_bytes: u64,
    pub file_hash: String,
}

impl FileTransferSender {
    pub fn new(id: String, path: impl AsRef<Path>) -> Result<Self> {
        let path_buf = path.as_ref().to_path_buf();
        let metadata = std::fs::metadata(&path_buf)
            .with_context(|| format!("File not found: {:?}", path_buf))?;

        let total_bytes = metadata.len();

        // Calculate initial hash
        let mut file = File::open(&path_buf)?;
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        let file_hash = format!("{:x}", hasher.finalize());

        Ok(Self {
            id,
            file_path: path_buf,
            total_bytes,
            transferred_bytes: 0,
            file_hash,
        })
    }

    /// Read next chunk at offset
    pub fn read_chunk(&mut self, offset: u64) -> Result<Option<Message>> {
        if offset >= self.total_bytes {
            return Ok(None);
        }

        let mut file = File::open(&self.file_path)?;
        file.seek(SeekFrom::Start(offset))?;

        let mut chunk_buf = vec![0u8; CHUNK_SIZE];
        let bytes_read = file.read(&mut chunk_buf)?;

        if bytes_read == 0 {
            return Ok(None);
        }

        chunk_buf.truncate(bytes_read);
        self.transferred_bytes = offset + bytes_read as u64;
        let is_last = self.transferred_bytes >= self.total_bytes;

        Ok(Some(Message::FileChunk {
            id: self.id.clone(),
            offset,
            data: chunk_buf,
            is_last,
        }))
    }

    pub fn progress(&self, speed_bps: u64) -> TransferProgress {
        let filename = self
            .file_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let status = if self.transferred_bytes >= self.total_bytes {
            TransferStatus::Completed
        } else {
            TransferStatus::Transferring
        };

        TransferProgress {
            id: self.id.clone(),
            filename,
            total_bytes: self.total_bytes,
            transferred_bytes: self.transferred_bytes,
            speed_bps,
            status,
        }
    }
}

/// Receiver for writing chunks and validating file checksum
pub struct FileTransferReceiver {
    pub id: String,
    pub save_path: PathBuf,
    pub total_bytes: u64,
    pub received_bytes: u64,
    pub expected_hash: Option<String>,
    pub last_update: Instant,
    pub status: TransferStatus,
}

impl FileTransferReceiver {
    pub fn new(id: String, save_path: PathBuf, total_bytes: u64, expected_hash: Option<String>) -> Result<Self> {
        if let Some(parent) = save_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        Ok(Self {
            id,
            save_path,
            total_bytes,
            received_bytes: 0,
            expected_hash,
            last_update: Instant::now(),
            status: TransferStatus::Pending,
        })
    }

    /// Write chunk data to target file at specific offset
    pub fn write_chunk(&mut self, offset: u64, data: &[u8], is_last: bool) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .open(&self.save_path)
            .with_context(|| format!("Failed to open output file {:?}", self.save_path))?;

        file.seek(SeekFrom::Start(offset))?;
        file.write_all(data)?;
        file.flush()?;

        self.received_bytes = offset + data.len() as u64;
        self.status = if is_last {
            // Verify checksum if provided
            if let Some(ref expected) = self.expected_hash {
                let actual = Self::compute_hash(&self.save_path)?;
                if &actual != expected {
                    self.status = TransferStatus::Failed;
                    return Err(anyhow!("File integrity check failed: hash mismatch"));
                }
            }
            TransferStatus::Completed
        } else {
            TransferStatus::Transferring
        };

        Ok(())
    }

    /// Compute SHA-256 for finished file
    pub fn compute_hash(path: &Path) -> Result<String> {
        let mut file = File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(format!("{:x}", hasher.finalize()))
    }

    pub fn progress(&self) -> TransferProgress {
        let filename = self
            .save_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        TransferProgress {
            id: self.id.clone(),
            filename,
            total_bytes: self.total_bytes,
            transferred_bytes: self.received_bytes,
            speed_bps: 0,
            status: self.status,
        }
    }
}
