use anyhow::{anyhow, Result};
use arboard::Clipboard;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::thread;
use std::time::Duration;

static LAST_APPLIED_HASH: StdMutex<String> = StdMutex::new(String::new());

fn calc_hash(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// ClipboardMonitor periodically checks the system clipboard for new text content
pub struct ClipboardMonitor {
    is_running: Arc<AtomicBool>,
}

impl ClipboardMonitor {
    pub fn new() -> Self {
        Self {
            is_running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Read current clipboard text
    pub fn get_text() -> Result<String> {
        let mut clipboard =
            Clipboard::new().map_err(|e| anyhow!("Failed to access clipboard: {}", e))?;
        clipboard
            .get_text()
            .map_err(|e| anyhow!("Failed to get clipboard text: {}", e))
    }

    /// Set system clipboard text and mark hash to avoid echo loop
    pub async fn set_text_silent(content: &str) -> Result<()> {
        let hash = calc_hash(content);
        if let Ok(mut applied) = LAST_APPLIED_HASH.lock() {
            *applied = hash;
        }

        let mut clipboard =
            Clipboard::new().map_err(|e| anyhow!("Failed to access clipboard: {}", e))?;
        clipboard
            .set_text(content.to_string())
            .map_err(|e| anyhow!("Failed to set clipboard text: {}", e))
    }

    /// Legacy set_text
    pub fn set_text(content: &str) -> Result<()> {
        let mut clipboard =
            Clipboard::new().map_err(|e| anyhow!("Failed to access clipboard: {}", e))?;
        clipboard
            .set_text(content.to_string())
            .map_err(|e| anyhow!("Failed to set clipboard text: {}", e))
    }

    /// Start polling clipboard content every 300ms and invoke callback on local change
    pub fn start_monitoring<F>(&mut self, callback: F)
    where
        F: Fn(String) + Send + 'static,
    {
        if self.is_running.swap(true, Ordering::SeqCst) {
            return; // Already running
        }

        let is_running = Arc::clone(&self.is_running);

        thread::spawn(move || {
            let mut last_local_hash = String::new();

            // Initialize last_local_hash with current clipboard if any
            if let Ok(mut clipboard) = Clipboard::new() {
                if let Ok(text) = clipboard.get_text() {
                    last_local_hash = calc_hash(&text);
                }
            }

            while is_running.load(Ordering::SeqCst) {
                if let Ok(mut clipboard) = Clipboard::new() {
                    if let Ok(text) = clipboard.get_text() {
                        let current_hash = calc_hash(&text);

                        if !current_hash.is_empty() && current_hash != last_local_hash {
                            let is_remote_echo = if let Ok(applied) = LAST_APPLIED_HASH.lock() {
                                *applied == current_hash
                            } else {
                                false
                            };

                            if !is_remote_echo {
                                log::info!("Local clipboard changed, syncing to remote ({} bytes)", text.len());
                                callback(text.clone());
                            }
                            last_local_hash = current_hash;
                        }
                    }
                }

                thread::sleep(Duration::from_millis(300));
            }
        });
    }

    /// Stop monitoring the clipboard
    pub fn stop_monitoring(&self) {
        self.is_running.store(false, Ordering::SeqCst);
    }
}
