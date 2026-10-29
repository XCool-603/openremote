use anyhow::{anyhow, Result};
use arboard::Clipboard;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

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

    /// Set system clipboard text
    pub fn set_text(content: &str) -> Result<()> {
        let mut clipboard =
            Clipboard::new().map_err(|e| anyhow!("Failed to access clipboard: {}", e))?;
        clipboard
            .set_text(content.to_string())
            .map_err(|e| anyhow!("Failed to set clipboard text: {}", e))
    }

    /// Start polling clipboard content every 500ms and invoke callback on change
    pub fn start_monitoring<F>(&mut self, callback: F)
    where
        F: Fn(String) + Send + 'static,
    {
        if self.is_running.swap(true, Ordering::SeqCst) {
            return; // Already running
        }

        let is_running = Arc::clone(&self.is_running);

        thread::spawn(move || {
            let mut last_hash = String::new();

            while is_running.load(Ordering::SeqCst) {
                if let Ok(mut clipboard) = Clipboard::new() {
                    if let Ok(text) = clipboard.get_text() {
                        let mut hasher = Sha256::new();
                        hasher.update(text.as_bytes());
                        let current_hash = format!("{:x}", hasher.finalize());

                        if !last_hash.is_empty() && current_hash != last_hash {
                            callback(text.clone());
                        }
                        last_hash = current_hash;
                    }
                }

                thread::sleep(Duration::from_millis(500));
            }
        });
    }

    /// Stop monitoring the clipboard
    pub fn stop_monitoring(&self) {
        self.is_running.store(false, Ordering::SeqCst);
    }
}
