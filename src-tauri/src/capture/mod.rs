use anyhow::Result;

/// Represents a single captured raw frame
#[derive(Debug, Clone)]
pub struct CaptureFrame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>, // Packed RGB8 format ready for direct encoding
    pub stride: u32,
    pub is_dirty: bool, // Indicates if screen content changed since last frame
}

/// Abstract trait for screen capture
pub trait ScreenCapturer: Send {
    fn new() -> Result<Self>
    where
        Self: Sized;
    fn capture_frame(&mut self) -> Result<CaptureFrame>;
    fn resolution(&self) -> (u32, u32);
}

#[cfg(target_os = "windows")]
mod windows_impl {
    use super::{CaptureFrame, ScreenCapturer};
    use anyhow::{anyhow, Result};
    use scrap::{Capturer, Display};
    use std::thread;
    use std::time::Duration;

    pub struct WindowsCapturer {
        capturer: Capturer,
        width: usize,
        height: usize,
        rgb_buffer: Vec<u8>,
        last_hash: u64,
        force_interval_counter: u32,
    }

    impl ScreenCapturer for WindowsCapturer {
        fn new() -> Result<Self> {
            let display = Display::primary()
                .map_err(|e| anyhow!("Failed to get primary display: {}", e))?;
            let width = display.width();
            let height = display.height();
            let capturer = Capturer::new(display)
                .map_err(|e| anyhow!("Failed to initialize scrap DXGI capturer: {}", e))?;

            let buffer_size = width * height * 3;
            Ok(Self {
                capturer,
                width,
                height,
                rgb_buffer: vec![0u8; buffer_size],
                last_hash: 0,
                force_interval_counter: 0,
            })
        }

        fn capture_frame(&mut self) -> Result<CaptureFrame> {
            let mut retries = 0;
            let frame = loop {
                match self.capturer.frame() {
                    Ok(frame) => break frame,
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        retries += 1;
                        if retries > 60 {
                            return Err(anyhow!("Timeout waiting for frame after 60 retries"));
                        }
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(e) => return Err(anyhow!("Failed to capture screen frame: {}", e)),
                }
            };

            // Fast sample hash for dirty detection (samples every 64 bytes)
            let mut sample_hash: u64 = 0xcbf29ce484222325;
            let step = 64.min(frame.len());
            for chunk in frame.chunks(step) {
                if let Some(&first) = chunk.first() {
                    sample_hash = sample_hash.wrapping_mul(0x100000001b3) ^ (first as u64);
                }
            }

            self.force_interval_counter = (self.force_interval_counter + 1) % 60;
            let is_dirty = sample_hash != self.last_hash || self.force_interval_counter == 0;
            self.last_hash = sample_hash;

            // Direct zero-intermediate transformation from DXGI BGRA to RGB8
            let stride = frame.len() / self.height;
            let mut out_idx = 0;
            let rgb_slice = &mut self.rgb_buffer;

            for y in 0..self.height {
                let row_start = y * stride;
                let row_end = row_start + self.width * 4;
                if row_end <= frame.len() {
                    let row = &frame[row_start..row_end];
                    for px in row.chunks_exact(4) {
                        rgb_slice[out_idx] = px[2];     // R
                        rgb_slice[out_idx + 1] = px[1]; // G
                        rgb_slice[out_idx + 2] = px[0]; // B
                        out_idx += 3;
                    }
                }
            }

            Ok(CaptureFrame {
                width: self.width as u32,
                height: self.height as u32,
                data: self.rgb_buffer.clone(),
                stride: (self.width * 3) as u32,
                is_dirty,
            })
        }

        fn resolution(&self) -> (u32, u32) {
            (self.width as u32, self.height as u32)
        }
    }

    unsafe impl Send for WindowsCapturer {}
}

#[cfg(not(target_os = "windows"))]
mod stub_impl {
    use super::{CaptureFrame, ScreenCapturer};
    use anyhow::{anyhow, Result};

    pub struct StubCapturer;

    impl ScreenCapturer for StubCapturer {
        fn new() -> Result<Self> {
            Err(anyhow!("Screen capture is only supported on Windows in this build"))
        }

        fn capture_frame(&mut self) -> Result<CaptureFrame> {
            Err(anyhow!("Screen capture is not supported on this platform"))
        }

        fn resolution(&self) -> (u32, u32) {
            (0, 0)
        }
    }
}

#[cfg(target_os = "windows")]
pub type PlatformCapturer = windows_impl::WindowsCapturer;

#[cfg(not(target_os = "windows"))]
pub type PlatformCapturer = stub_impl::StubCapturer;

/// Factory function to instantiate a platform-specific screen capturer
pub fn create_capturer() -> Result<Box<dyn ScreenCapturer>> {
    let capturer = PlatformCapturer::new()?;
    Ok(Box::new(capturer))
}
