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

    /// Encode raw RGB8 bytes directly into a JPEG payload with zero intermediate allocations
    pub fn encode(&self, width: u32, height: u32, rgb_data: &[u8]) -> Result<Vec<u8>> {
        let expected_size = (width * height * 3) as usize;
        if rgb_data.len() < expected_size {
            return Err(anyhow!(
                "Invalid RGB buffer length: expected at least {}, got {}",
                expected_size,
                rgb_data.len()
            ));
        }

        let mut output = Vec::with_capacity(expected_size / 8);
        let mut encoder = JpegEncoder::new_with_quality(&mut output, self.quality);
        encoder
            .encode(&rgb_data[..expected_size], width, height, ColorType::Rgb8.into())
            .map_err(|e| anyhow!("JPEG encoding failed: {}", e))?;

        Ok(output)
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
