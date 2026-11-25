use crate::protocol::DirtyTile;
use anyhow::Result;

const TILE_SIZE: usize = 128;

/// Represents a single captured raw frame
#[derive(Debug, Clone)]
pub struct CaptureFrame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>, // Packed RGB8 format ready for direct encoding
    pub stride: u32,
    pub is_dirty: bool, // Indicates if screen content changed since last frame
}

/// Incremental batch of changed screen tiles
#[derive(Debug, Clone)]
pub struct TileBatch {
    pub native_w: u32,
    pub native_h: u32,
    pub out_w: u16,
    pub out_h: u16,
    pub tiles: Vec<DirtyTile>,
}

/// Abstract trait for screen capture
pub trait ScreenCapturer: Send {
    fn new() -> Result<Self>
    where
        Self: Sized;
    fn capture_frame(&mut self) -> Result<CaptureFrame>;
    fn capture_dirty_tiles(
        &mut self,
        max_width: u32,
        quality: u8,
        force_keyframe: bool,
    ) -> Result<TileBatch>;
    fn resolution(&self) -> (u32, u32);
}

#[cfg(target_os = "windows")]
mod windows_impl {
    use super::{CaptureFrame, DirtyTile, ScreenCapturer, TileBatch, TILE_SIZE};
    use anyhow::{anyhow, Result};
    use image::codecs::jpeg::JpegEncoder;
    use image::ColorType;
    use scrap::{Capturer, Display};
    use std::thread;
    use std::time::Duration;

    pub struct WindowsCapturer {
        capturer: Capturer,
        width: usize,
        height: usize,
        rgb_buffer: Vec<u8>,
        tile_rgb_scratch: Vec<u8>,
        tile_hashes: Vec<u64>,
        last_out_dims: (usize, usize),
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
                tile_rgb_scratch: Vec::with_capacity(TILE_SIZE * TILE_SIZE * 3),
                tile_hashes: Vec::new(),
                last_out_dims: (0, 0),
                last_hash: 0,
                force_interval_counter: 0,
            })
        }

        fn capture_dirty_tiles(
            &mut self,
            max_width: u32,
            quality: u8,
            force_keyframe: bool,
        ) -> Result<TileBatch> {
            let mut retries = 0;
            let frame = loop {
                match self.capturer.frame() {
                    Ok(frame) => break frame,
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        retries += 1;
                        if retries > 40 {
                            return Ok(TileBatch {
                                native_w: self.width as u32,
                                native_h: self.height as u32,
                                out_w: self.last_out_dims.0 as u16,
                                out_h: self.last_out_dims.1 as u16,
                                tiles: Vec::new(),
                            });
                        }
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(e) => return Err(anyhow!("Failed to capture screen frame: {}", e)),
                }
            };

            let stride = frame.len() / self.height.max(1);
            let out_w = if max_width > 0 && self.width > max_width as usize {
                max_width as usize
            } else {
                self.width
            };
            let out_h = ((self.height as u64 * out_w as u64) / self.width.max(1) as u64).max(1) as usize;

            let cols = (out_w + TILE_SIZE - 1) / TILE_SIZE;
            let rows = (out_h + TILE_SIZE - 1) / TILE_SIZE;
            let total_tiles = cols * rows;

            let dim_changed = self.last_out_dims != (out_w, out_h);
            if dim_changed || self.tile_hashes.len() != total_tiles {
                self.tile_hashes = vec![0; total_tiles];
                self.last_out_dims = (out_w, out_h);
            }

            let mut dirty_tiles = Vec::new();

            for ty in 0..rows {
                let dst_y0 = ty * TILE_SIZE;
                let tile_h = (out_h - dst_y0).min(TILE_SIZE);

                for tx in 0..cols {
                    let dst_x0 = tx * TILE_SIZE;
                    let tile_w = (out_w - dst_x0).min(TILE_SIZE);
                    let tile_idx = ty * cols + tx;

                    // Fast 64-bit FNV-1a hash over sampled pixels in this tile (zero allocation)
                    let mut hash: u64 = 0xcbf29ce484222325;
                    let sample_step_y = 2.max(tile_h / 32);
                    let sample_step_x = 2.max(tile_w / 32);

                    let mut dy = 0;
                    while dy < tile_h {
                        let sy = ((dst_y0 + dy) * self.height) / out_h;
                        let row_off = sy * stride;
                        let mut dx = 0;
                        while dx < tile_w {
                            let sx = ((dst_x0 + dx) * self.width) / out_w;
                            let px_off = row_off + sx * 4;
                            if px_off + 3 < frame.len() {
                                let packed = ((frame[px_off] as u64) << 16)
                                    | ((frame[px_off + 1] as u64) << 8)
                                    | (frame[px_off + 2] as u64);
                                hash = hash.wrapping_mul(0x100000001b3) ^ packed;
                            }
                            dx += sample_step_x;
                        }
                        dy += sample_step_y;
                    }

                    if !force_keyframe && !dim_changed && self.tile_hashes[tile_idx] == hash {
                        continue;
                    }
                    self.tile_hashes[tile_idx] = hash;

                    // Convert only this dirty tile from BGRA to RGB8
                    self.tile_rgb_scratch.clear();
                    self.tile_rgb_scratch.reserve(tile_w * tile_h * 3);
                    for dy in 0..tile_h {
                        let sy = ((dst_y0 + dy) * self.height) / out_h;
                        let row_off = sy * stride;
                        for dx in 0..tile_w {
                            let sx = ((dst_x0 + dx) * self.width) / out_w;
                            let px_off = row_off + sx * 4;
                            if px_off + 3 < frame.len() {
                                self.tile_rgb_scratch.push(frame[px_off + 2]); // R
                                self.tile_rgb_scratch.push(frame[px_off + 1]); // G
                                self.tile_rgb_scratch.push(frame[px_off]);     // B
                            } else {
                                self.tile_rgb_scratch.extend_from_slice(&[0, 0, 0]);
                            }
                        }
                    }

                    let mut jpeg_buf = Vec::with_capacity(2048);
                    let mut encoder = JpegEncoder::new_with_quality(&mut jpeg_buf, quality.clamp(20, 95));
                    if encoder
                        .encode(
                            &self.tile_rgb_scratch,
                            tile_w as u32,
                            tile_h as u32,
                            ColorType::Rgb8.into(),
                        )
                        .is_ok()
                    {
                        dirty_tiles.push(DirtyTile {
                            x: dst_x0 as u16,
                            y: dst_y0 as u16,
                            w: tile_w as u16,
                            h: tile_h as u16,
                            data: jpeg_buf,
                        });
                    }
                }
            }

            Ok(TileBatch {
                native_w: self.width as u32,
                native_h: self.height as u32,
                out_w: out_w as u16,
                out_h: out_h as u16,
                tiles: dirty_tiles,
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
    use super::{CaptureFrame, ScreenCapturer, TileBatch};
    use anyhow::{anyhow, Result};

    pub struct StubCapturer;

    impl ScreenCapturer for StubCapturer {
        fn new() -> Result<Self> {
            Err(anyhow!("Screen capture is only supported on Windows in this build"))
        }

        fn capture_frame(&mut self) -> Result<CaptureFrame> {
            Err(anyhow!("Screen capture is not supported on this platform"))
        }

        fn capture_dirty_tiles(
            &mut self,
            _max_width: u32,
            _quality: u8,
            _force_keyframe: bool,
        ) -> Result<TileBatch> {
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
