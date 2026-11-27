pub mod h264;

use anyhow::{anyhow, Result};
use image::codecs::jpeg::JpegEncoder;
use image::{ColorType, ImageBuffer, ImageReader, Rgba};
use std::io::Cursor;

/// FrameEncoder compresses raw RGBA frames into compressed JPEG bytes
pub struct FrameEncoder {
    pub quality: u8,
}

impl FrameEncoder {
    pub fn new(quality: u8) -> Self {
        Self {
            quality: quality.clamp(1, 100),
        }
    }

    /// Set compression quality (1-100)
    pub fn set_quality(&mut self, quality: u8) {
        self.quality = quality.clamp(1, 100);
    }

    /// Encode raw RGB8 bytes directly into a JPEG payload with optional fast downscaling
    pub fn encode(&self, width: u32, height: u32, rgb_data: &[u8]) -> Result<Vec<u8>> {
        self.encode_scaled(width, height, rgb_data, 0).map(|(buf, _, _)| buf)
    }

    /// Encode raw RGB8 bytes with fast nearest-neighbor downscaling if width > max_width (0 = no scaling)
    pub fn encode_scaled(&self, width: u32, height: u32, rgb_data: &[u8], max_width: u32) -> Result<(Vec<u8>, u32, u32)> {
        let expected_size = (width * height * 3) as usize;
        if rgb_data.len() < expected_size {
            return Err(anyhow!(
                "Invalid RGB buffer length: expected at least {}, got {}",
                expected_size,
                rgb_data.len()
            ));
        }

        if max_width > 0 && width > max_width {
            let dst_w = max_width;
            let dst_h = ((height as u64 * dst_w as u64) / width as u64).max(1) as u32;
            let mut scaled = Vec::with_capacity((dst_w * dst_h * 3) as usize);

            for dy in 0..dst_h {
                let sy = (dy as u64 * height as u64) / dst_h as u64;
                let row_offset = (sy as usize) * (width as usize) * 3;
                for dx in 0..dst_w {
                    let sx = (dx as u64 * width as u64) / dst_w as u64;
                    let idx = row_offset + (sx as usize) * 3;
                    scaled.push(rgb_data[idx]);
                    scaled.push(rgb_data[idx + 1]);
                    scaled.push(rgb_data[idx + 2]);
                }
            }

            let mut output = Vec::with_capacity(scaled.len() / 10);
            let mut encoder = JpegEncoder::new_with_quality(&mut output, self.quality);
            encoder
                .encode(&scaled, dst_w, dst_h, ColorType::Rgb8.into())
                .map_err(|e| anyhow!("JPEG encoding failed: {}", e))?;
            Ok((output, dst_w, dst_h))
        } else {
            let mut output = Vec::with_capacity(expected_size / 10);
            let mut encoder = JpegEncoder::new_with_quality(&mut output, self.quality);
            encoder
                .encode(&rgb_data[..expected_size], width, height, ColorType::Rgb8.into())
                .map_err(|e| anyhow!("JPEG encoding failed: {}", e))?;
            Ok((output, width, height))
        }
    }
}

/// FrameDecoder decompresses JPEG/PNG payloads back to raw RGBA bytes
pub struct FrameDecoder;

impl FrameDecoder {
    pub fn new() -> Self {
        Self
    }

    /// Decode compressed JPEG/PNG into (width, height, rgba_data)
    pub fn decode(&self, compressed_data: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
        let cursor = Cursor::new(compressed_data);
        let reader = ImageReader::new(cursor)
            .with_guessed_format()
            .map_err(|e| anyhow!("Failed to guess image format: {}", e))?;

        let dynamic_image = reader
            .decode()
            .map_err(|e| anyhow!("Failed to decode image frame: {}", e))?;

        let rgba_image: ImageBuffer<Rgba<u8>, Vec<u8>> = dynamic_image.to_rgba8();
        let width = rgba_image.width();
        let height = rgba_image.height();
        let raw_bytes = rgba_image.into_raw();

        Ok((width, height, raw_bytes))
    }
}
