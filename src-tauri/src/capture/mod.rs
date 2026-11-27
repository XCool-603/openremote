use crate::protocol::DirtyTile;
use anyhow::Result;

const TILE_SIZE: usize = 256;

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
    /// True when the capture backend had no new compositor frame yet (WouldBlock)
    pub would_block: bool,
    /// True when heavy full-screen motion was coalesced into horizontal strips
    pub is_full_motion: bool,
    /// Populated when desktop capture failed (e.g., minimized/disconnected RDP or locked screen)
    pub capture_error: Option<String>,
}

/// Raw NV12 frame ready for hardware H.264 encoding (empty `nv12` means screen unchanged)
#[derive(Debug, Clone)]
pub struct Nv12Frame {
    pub native_w: u32,
    pub native_h: u32,
    pub out_w: u16,
    pub out_h: u16,
    pub nv12: Vec<u8>,
    pub would_block: bool,
    pub capture_error: Option<String>,
    pub backend: &'static str,
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
    fn capture_nv12(&mut self, max_width: u32, force_keyframe: bool) -> Result<Nv12Frame>;
    fn resolution(&self) -> (u32, u32);
}

#[cfg(target_os = "windows")]
mod windows_impl {
    use super::{CaptureFrame, DirtyTile, Nv12Frame, ScreenCapturer, TileBatch, TILE_SIZE};
    use anyhow::{anyhow, Result};
    use image::codecs::jpeg::JpegEncoder;
    use image::ColorType;
    use scrap::{Capturer, Display};
    use std::cell::RefCell;
    use std::ptr;
    use std::slice;
    use std::thread;
    use std::time::{Duration, Instant};

    thread_local! {
        static RGB_SCRATCH: RefCell<Vec<u8>> = RefCell::new(Vec::new());
    }

    #[link(name = "user32")]
    extern "system" {
        fn GetDC(hWnd: isize) -> isize;
        fn ReleaseDC(hWnd: isize, hDC: isize) -> i32;
        fn GetSystemMetrics(nIndex: i32) -> i32;
        fn OpenInputDesktop(dwFlags: u32, fInherit: i32, dwDesiredAccess: u32) -> isize;
        fn OpenDesktopW(lpszDesktop: *const u16, dwFlags: u32, fInherit: i32, dwDesiredAccess: u32) -> isize;
        fn GetThreadDesktop(dwThreadId: u32) -> isize;
        fn GetUserObjectInformationW(
            hObj: isize,
            nIndex: i32,
            pvInfo: *mut u8,
            nLength: u32,
            lpnLengthNeeded: *mut u32,
        ) -> i32;
        fn SetThreadDesktop(hDesktop: isize) -> i32;
        fn CloseDesktop(hDesktop: isize) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentThreadId() -> u32;
    }

    #[link(name = "gdi32")]
    extern "system" {
        fn CreateDCW(
            pwszDriver: *const u16,
            pwszDevice: *const u16,
            pszPort: *const u16,
            pdm: *const u8,
        ) -> isize;
        fn CreateCompatibleDC(hDC: isize) -> isize;
        fn SelectObject(hDC: isize, hGDIOBJ: isize) -> isize;
        fn DeleteObject(hGDIOBJ: isize) -> i32;
        fn DeleteDC(hDC: isize) -> i32;
        fn SetStretchBltMode(hdc: isize, mode: i32) -> i32;
        fn BitBlt(
            hdcDest: isize,
            nXDest: i32,
            nYDest: i32,
            nWidth: i32,
            nHeight: i32,
            hdcSrc: isize,
            nXSrc: i32,
            nYSrc: i32,
            dwRop: u32,
        ) -> i32;
        fn StretchBlt(
            hdcDest: isize,
            xDest: i32,
            yDest: i32,
            wDest: i32,
            hDest: i32,
            hdcSrc: isize,
            xSrc: i32,
            ySrc: i32,
            wSrc: i32,
            hSrc: i32,
            rop: u32,
        ) -> i32;
        fn CreateDIBSection(
            hdc: isize,
            pbmi: *const BitmapInfo,
            usage: u32,
            ppvBits: *mut *mut u8,
            hSection: isize,
            offset: u32,
        ) -> isize;
    }

    #[repr(C)]
    struct BitmapInfoHeader {
        bi_size: u32,
        bi_width: i32,
        bi_height: i32,
        bi_planes: u16,
        bi_bit_count: u16,
        bi_compression: u32,
        bi_size_image: u32,
        bi_xpels_per_meter: i32,
        bi_ypels_per_meter: i32,
        bi_clr_used: u32,
        bi_clr_important: u32,
    }

    #[repr(C)]
    struct BitmapInfo {
        bmi_header: BitmapInfoHeader,
        bmi_colors: [u32; 1],
    }

    fn desktop_name(hdesk: isize) -> Option<String> {
        if hdesk == 0 {
            return None;
        }
        let mut buf = [0u16; 128];
        let mut needed: u32 = 0;
        let ok = unsafe {
            GetUserObjectInformationW(
                hdesk,
                2, // UOI_NAME
                buf.as_mut_ptr() as *mut u8,
                (buf.len() * 2) as u32,
                &mut needed,
            )
        };
        if ok != 0 && needed >= 2 {
            let units = ((needed as usize) / 2).min(buf.len());
            let trimmed = buf[..units]
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(units);
            Some(String::from_utf16_lossy(&buf[..trimmed]))
        } else {
            None
        }
    }

    /// Detect if the active Windows input desktop changed (e.g. Default <-> Winlogon Lock Screen / UAC)
    /// and seamlessly switch this capture thread to it! Returns `Some(new_name)` when a switch occurred.
    fn sync_thread_to_active_input_desktop() -> Option<String> {
        unsafe {
            let mut h_in = OpenInputDesktop(0, 0, 0x000F_01FF); // MAXIMUM/ALL for SYSTEM on Winlogon
            if h_in == 0 {
                h_in = OpenInputDesktop(0, 0, 0x0001 | 0x0100 | 0x0002);
            }
            if h_in == 0 {
                h_in = OpenInputDesktop(0, 0, 0x0100);
            }
            if h_in == 0 {
                return None;
            }
            let cur_desk = GetThreadDesktop(GetCurrentThreadId());
            let in_name = desktop_name(h_in);
            let cur_name = desktop_name(cur_desk);
            if in_name.is_some() && in_name != cur_name {
                let switched = SetThreadDesktop(h_in) != 0;
                CloseDesktop(h_in);
                if switched {
                    return in_name;
                }
                return None;
            }
            CloseDesktop(h_in);
            None
        }
    }

    /// Re-attach current capture thread to the active Windows input desktop (handles UAC / Lock Screen / session switches)
    fn try_reattach_input_desktop() {
        unsafe {
            let mut hdesk = OpenInputDesktop(0, 0, 0x000F_01FF);
            if hdesk == 0 {
                hdesk = OpenInputDesktop(0, 0, 0x0001 | 0x0100 | 0x0002);
            }
            if hdesk == 0 {
                hdesk = OpenInputDesktop(0, 0, 0x0100);
            }
            if hdesk == 0 {
                let default_name: [u16; 8] = [
                    b'D' as u16,
                    b'e' as u16,
                    b'f' as u16,
                    b'a' as u16,
                    b'u' as u16,
                    b'l' as u16,
                    b't' as u16,
                    0,
                ];
                hdesk = OpenDesktopW(default_name.as_ptr(), 0, 0, 0x0001 | 0x0100);
            }
            if hdesk != 0 {
                SetThreadDesktop(hdesk);
                CloseDesktop(hdesk);
            }
        }
    }

    /// Persistent zero-allocation GDI DIBSection surface.
    /// Eliminates per-frame CreateCompatibleBitmap + GetDIBits + 33MB 4K copies!
    struct PersistentGdiSurface {
        hdc_mem: isize,
        hbm_dib: isize,
        old_obj: isize,
        bits_ptr: *mut u8,
        out_w: usize,
        out_h: usize,
        last_err: Option<String>,
    }

    impl PersistentGdiSurface {
        fn new() -> Self {
            Self {
                hdc_mem: 0,
                hbm_dib: 0,
                old_obj: 0,
                bits_ptr: ptr::null_mut(),
                out_w: 0,
                out_h: 0,
                last_err: None,
            }
        }

        fn release_resources(&mut self) {
            unsafe {
                if self.hdc_mem != 0 && self.old_obj != 0 {
                    SelectObject(self.hdc_mem, self.old_obj);
                    self.old_obj = 0;
                }
                if self.hbm_dib != 0 {
                    DeleteObject(self.hbm_dib);
                    self.hbm_dib = 0;
                }
                if self.hdc_mem != 0 {
                    DeleteDC(self.hdc_mem);
                    self.hdc_mem = 0;
                }
                self.bits_ptr = ptr::null_mut();
                self.out_w = 0;
                self.out_h = 0;
            }
        }

        fn ensure_size(&mut self, hdc_screen: isize, out_w: usize, out_h: usize) -> bool {
            if self.hbm_dib != 0 && self.out_w == out_w && self.out_h == out_h && !self.bits_ptr.is_null() {
                return true;
            }
            self.release_resources();
            if out_w == 0 || out_h == 0 {
                return false;
            }
            unsafe {
                let hdc_mem = CreateCompatibleDC(hdc_screen);
                if hdc_mem == 0 {
                    return false;
                }
                SetStretchBltMode(hdc_mem, 3); // COLORONCOLOR (fast hardware/GDI downscale)

                let bmi = BitmapInfo {
                    bmi_header: BitmapInfoHeader {
                        bi_size: std::mem::size_of::<BitmapInfoHeader>() as u32,
                        bi_width: out_w as i32,
                        bi_height: -(out_h as i32), // Top-down BGRA
                        bi_planes: 1,
                        bi_bit_count: 32,
                        bi_compression: 0, // BI_RGB
                        bi_size_image: (out_w * out_h * 4) as u32,
                        bi_xpels_per_meter: 0,
                        bi_ypels_per_meter: 0,
                        bi_clr_used: 0,
                        bi_clr_important: 0,
                    },
                    bmi_colors: [0],
                };

                let mut bits_ptr: *mut u8 = ptr::null_mut();
                let hbm = CreateDIBSection(
                    hdc_screen,
                    &bmi,
                    0, // DIB_RGB_COLORS
                    &mut bits_ptr,
                    0,
                    0,
                );
                if hbm == 0 || bits_ptr.is_null() {
                    if hbm != 0 {
                        DeleteObject(hbm);
                    }
                    DeleteDC(hdc_mem);
                    return false;
                }
                let old_obj = SelectObject(hdc_mem, hbm);
                self.hdc_mem = hdc_mem;
                self.hbm_dib = hbm;
                self.old_obj = old_obj;
                self.bits_ptr = bits_ptr;
                self.out_w = out_w;
                self.out_h = out_h;
                true
            }
        }

        /// Capture desktop directly into persistent (out_w x out_h) BGRA DIB memory (zero GetDIBits copy!)
        /// Automatically rebuilds invalidated GDI handles and tries CreateDCW("DISPLAY") fallback if GetDC(0) fails.
        fn capture_scaled(&mut self, src_w: usize, src_h: usize, out_w: usize, out_h: usize) -> Option<&[u8]> {
            unsafe {
                let mut try_from_dc = |hdc_screen: isize, is_created_dc: bool| -> bool {
                    if hdc_screen == 0 {
                        self.last_err = Some(format!("GetDC/CreateDC failed ({})", std::io::Error::last_os_error()));
                        return false;
                    }
                    if !self.ensure_size(hdc_screen, out_w, out_h) {
                        self.last_err = Some(format!("CreateDIBSection failed ({})", std::io::Error::last_os_error()));
                        if is_created_dc {
                            DeleteDC(hdc_screen);
                        } else {
                            ReleaseDC(0, hdc_screen);
                        }
                        return false;
                    }
                    let ok = if src_w == out_w && src_h == out_h {
                        BitBlt(
                            self.hdc_mem,
                            0,
                            0,
                            out_w as i32,
                            out_h as i32,
                            hdc_screen,
                            0,
                            0,
                            0x00CC0020, // SRCCOPY
                        )
                    } else {
                        StretchBlt(
                            self.hdc_mem,
                            0,
                            0,
                            out_w as i32,
                            out_h as i32,
                            hdc_screen,
                            0,
                            0,
                            src_w as i32,
                            src_h as i32,
                            0x00CC0020, // SRCCOPY
                        )
                    };
                    let err_if_fail = if ok == 0 {
                        Some(std::io::Error::last_os_error())
                    } else {
                        None
                    };
                    if is_created_dc {
                        DeleteDC(hdc_screen);
                    } else {
                        ReleaseDC(0, hdc_screen);
                    }
                    if ok != 0 && !self.bits_ptr.is_null() {
                        self.last_err = None;
                        true
                    } else {
                        self.last_err = Some(format!(
                            "BitBlt/StretchBlt failed ({})",
                            err_if_fail
                                .map(|e| e.to_string())
                                .unwrap_or_else(|| "unknown".to_string())
                        ));
                        // Always release invalidated GDI resources so desktop/RDP reconnects recover cleanly!
                        self.release_resources();
                        false
                    }
                };

                let hdc0 = GetDC(0);
                if try_from_dc(hdc0, false) {
                    return Some(slice::from_raw_parts(self.bits_ptr, out_w * out_h * 4));
                }

                // Secondary fallback: CreateDCW("DISPLAY", NULL, NULL, NULL)
                let display_w: [u16; 8] = [
                    b'D' as u16,
                    b'I' as u16,
                    b'S' as u16,
                    b'P' as u16,
                    b'L' as u16,
                    b'A' as u16,
                    b'Y' as u16,
                    0,
                ];
                let hdc_disp = CreateDCW(display_w.as_ptr(), ptr::null(), ptr::null(), ptr::null());
                if try_from_dc(hdc_disp, true) {
                    return Some(slice::from_raw_parts(self.bits_ptr, out_w * out_h * 4));
                }

                None
            }
        }
    }

    impl Drop for PersistentGdiSurface {
        fn drop(&mut self) {
            self.release_resources();
        }
    }

    unsafe impl Send for PersistentGdiSurface {}

    /// Encode a single rectangular region (BGRA source -> RGB8 -> JPEG) using thread-local reusable scratch buffer
    fn encode_region_jpeg(
        frame: &[u8],
        stride: usize,
        x_map: &[usize],
        y_map: &[usize],
        is_native_1to1: bool,
        rx: usize,
        ry: usize,
        rw: usize,
        rh: usize,
        quality: u8,
    ) -> Option<DirtyTile> {
        let needed = rw * rh * 3;
        let frame_len = frame.len();

        RGB_SCRATCH.with(|cell| {
            let mut rgb = cell.borrow_mut();
            if rgb.len() < needed {
                rgb.resize(needed, 0);
            }
            let rgb_slice = &mut rgb[..needed];

            if is_native_1to1 {
                let mut dst_off = 0usize;
                for dy in 0..rh {
                    let row_start = (ry + dy) * stride + rx * 4;
                    let row_end = row_start + rw * 4;
                    if row_end <= frame_len {
                        let src_row = &frame[row_start..row_end];
                        let dst_row = &mut rgb_slice[dst_off..dst_off + rw * 3];
                        for (px, out_px) in src_row.chunks_exact(4).zip(dst_row.chunks_exact_mut(3)) {
                            out_px[0] = px[2]; // R
                            out_px[1] = px[1]; // G
                            out_px[2] = px[0]; // B
                        }
                    }
                    dst_off += rw * 3;
                }
            } else {
                let x_slice = &x_map[rx..rx + rw];
                let mut dst_off = 0usize;
                for dy in 0..rh {
                    let row_off = y_map[ry + dy] * stride;
                    if row_off + stride <= frame_len {
                        let src_row = &frame[row_off..row_off + stride];
                        let dst_row = &mut rgb_slice[dst_off..dst_off + rw * 3];
                        for (&sx4, out_px) in x_slice.iter().zip(dst_row.chunks_exact_mut(3)) {
                            out_px[0] = src_row[sx4 + 2]; // R
                            out_px[1] = src_row[sx4 + 1]; // G
                            out_px[2] = src_row[sx4];     // B
                        }
                    }
                    dst_off += rw * 3;
                }
            }

            let mut jpeg_buf = Vec::with_capacity((rw * rh / 14).max(2048));
            let mut encoder = JpegEncoder::new_with_quality(&mut jpeg_buf, quality.clamp(22, 95));
            encoder
                .encode(rgb_slice, rw as u32, rh as u32, ColorType::Rgb8.into())
                .ok()?;

            Some(DirtyTile {
                x: rx as u16,
                y: ry as u16,
                w: rw as u16,
                h: rh as u16,
                data: jpeg_buf,
            })
        })
    }

    #[cfg(test)]
    pub(super) fn encode_region_jpeg_for_test(
        frame: &[u8],
        stride: usize,
        x_map: &[usize],
        y_map: &[usize],
        is_native_1to1: bool,
        rx: usize,
        ry: usize,
        rw: usize,
        rh: usize,
        quality: u8,
    ) -> Option<DirtyTile> {
        encode_region_jpeg(frame, stride, x_map, y_map, is_native_1to1, rx, ry, rw, rh, quality)
    }

    /// Disjoint encoding/hashing state so DXGI `&frame` can be borrowed directly with ZERO 4K memcpy!
    struct TileProcessor {
        tile_hashes: Vec<u64>,
        x_map: Vec<usize>,
        y_map: Vec<usize>,
        last_src_dims: (usize, usize),
        last_out_dims: (usize, usize),
        worker_threads: usize,
        needs_sharp_polish: bool,
        last_motion_at: Instant,
    }

    impl TileProcessor {
        fn new(worker_threads: usize) -> Self {
            Self {
                tile_hashes: Vec::new(),
                x_map: Vec::new(),
                y_map: Vec::new(),
                last_src_dims: (0, 0),
                last_out_dims: (0, 0),
                worker_threads,
                needs_sharp_polish: false,
                last_motion_at: Instant::now(),
            }
        }

        fn compute_out_dims(native_w: usize, native_h: usize, max_width: u32) -> (usize, usize) {
            let out_w = if max_width > 0 && native_w > max_width as usize {
                (max_width as usize) & !1
            } else {
                native_w & !1
            }
            .max(2);
            let out_h = (((native_h as u64 * out_w as u64) / native_w.max(1) as u64).max(2) as usize) & !1;
            (out_w, out_h)
        }

        fn ensure_luts(&mut self, src_w: usize, src_h: usize, out_w: usize, out_h: usize, total_tiles: usize) -> bool {
            let dim_changed = self.last_out_dims != (out_w, out_h)
                || self.last_src_dims != (src_w, src_h)
                || self.tile_hashes.len() != total_tiles;
            if dim_changed {
                self.tile_hashes = vec![0; total_tiles];
                self.last_src_dims = (src_w, src_h);
                self.last_out_dims = (out_w, out_h);

                self.x_map.clear();
                self.x_map.reserve(out_w);
                for dx in 0..out_w {
                    let sx = ((dx * src_w) / out_w).min(src_w.saturating_sub(1));
                    self.x_map.push(sx * 4);
                }

                self.y_map.clear();
                self.y_map.reserve(out_h);
                for dy in 0..out_h {
                    let sy = ((dy * src_h) / out_h).min(src_h.saturating_sub(1));
                    self.y_map.push(sy);
                }
            }
            dim_changed
        }

        fn process_bgra_frame(
            &mut self,
            frame: &[u8],
            src_w: usize,
            src_h: usize,
            stride: usize,
            native_w: u32,
            native_h: u32,
            out_w: usize,
            out_h: usize,
            quality: u8,
            force_keyframe: bool,
        ) -> TileBatch {
            let cols = (out_w + TILE_SIZE - 1) / TILE_SIZE;
            let rows = (out_h + TILE_SIZE - 1) / TILE_SIZE;
            let total_tiles = cols * rows;

            let dim_changed = self.ensure_luts(src_w, src_h, out_w, out_h, total_tiles);
            let is_native_1to1 = out_w == src_w && out_h == src_h;
            let frame_len = frame.len();

            // Pass 1: ultra-fast LUT-based dirty tile hash directly over source buffer (ZERO copy!)
            let mut dirty_rects: Vec<(usize, usize, usize, usize)> = Vec::with_capacity(total_tiles);

            for ty in 0..rows {
                let dst_y0 = ty * TILE_SIZE;
                let tile_h = (out_h - dst_y0).min(TILE_SIZE);

                for tx in 0..cols {
                    let dst_x0 = tx * TILE_SIZE;
                    let tile_w = (out_w - dst_x0).min(TILE_SIZE);
                    let tile_idx = ty * cols + tx;

                    let mut hash: u64 = 0xcbf29ce484222325;
                    let step_y = 6usize;
                    let step_x = 6usize;

                    let mut dy = 0;
                    while dy < tile_h {
                        let row_off = self.y_map[dst_y0 + dy] * stride;
                        if row_off + stride <= frame_len {
                            let row = &frame[row_off..row_off + stride];
                            let mut dx = 0;
                            while dx < tile_w {
                                let sx4 = self.x_map[dst_x0 + dx];
                                let px = u32::from_ne_bytes([
                                    row[sx4],
                                    row[sx4 + 1],
                                    row[sx4 + 2],
                                    0,
                                ]);
                                hash = hash.wrapping_mul(0x100000001b3) ^ (px as u64);
                                dx += step_x;
                            }
                        }
                        dy += step_y;
                    }

                    if !force_keyframe && !dim_changed && self.tile_hashes[tile_idx] == hash {
                        continue;
                    }
                    self.tile_hashes[tile_idx] = hash;
                    dirty_rects.push((dst_x0, dst_y0, tile_w, tile_h));
                }
            }

            // Sharp Polish Pass: 130ms after heavy motion stops, emit ONE crisp high-quality frame
            // so text snaps immediately to razor-sharp clarity!
            let do_sharp_polish = dirty_rects.is_empty()
                && self.needs_sharp_polish
                && self.last_motion_at.elapsed() >= Duration::from_millis(130);

            if dirty_rects.is_empty() && !do_sharp_polish {
                return TileBatch {
                    native_w,
                    native_h,
                    out_w: out_w as u16,
                    out_h: out_h as u16,
                    tiles: Vec::new(),
                    would_block: false,
                    is_full_motion: false,
                    capture_error: None,
                };
            }

            // Heavy-Motion Coalescing + Dynamic Motion Quality:
            // When >= 45% of screen changes at once (scrolling, dragging windows, video),
            // encode 4 parallel strips at compact fast quality (~35-55KB vs 160KB -> 3.5x faster encode,
            // 4x less WiFi/network burst, 4x faster browser decode!).
            let is_heavy_motion = !force_keyframe
                && !dim_changed
                && !do_sharp_polish
                && (dirty_rects.len() * 100 >= total_tiles * 45);
            let use_strips = force_keyframe || dim_changed || do_sharp_polish || is_heavy_motion;

            let encode_quality = if do_sharp_polish {
                self.needs_sharp_polish = false;
                quality.max(74)
            } else if is_heavy_motion {
                self.needs_sharp_polish = true;
                self.last_motion_at = Instant::now();
                quality.saturating_sub(24).clamp(32, 46)
            } else {
                // Small partial updates (mouse cursor, typing, hover): crisp full quality!
                if force_keyframe || dim_changed {
                    self.needs_sharp_polish = false;
                }
                quality
            };

            let encode_regions: Vec<(usize, usize, usize, usize)> = if use_strips {
                let strips = self.worker_threads.clamp(2, 4);
                let base_h = ((out_h / strips) + 15) & !15; // align to 16px JPEG MCU
                let mut regions = Vec::with_capacity(strips);
                let mut y = 0;
                while y < out_h {
                    let h = if regions.len() + 1 == strips {
                        out_h - y
                    } else {
                        base_h.min(out_h - y)
                    };
                    if h > 0 {
                        regions.push((0, y, out_w, h));
                    }
                    y += h;
                }
                regions
            } else {
                dirty_rects
            };

            let x_map = &self.x_map;
            let y_map = &self.y_map;

            let tiles: Vec<DirtyTile> = if encode_regions.len() == 1 {
                let (rx, ry, rw, rh) = encode_regions[0];
                encode_region_jpeg(frame, stride, x_map, y_map, is_native_1to1, rx, ry, rw, rh, encode_quality)
                    .into_iter()
                    .collect()
            } else {
                let num_workers = self.worker_threads.min(encode_regions.len());
                let chunk_size = (encode_regions.len() + num_workers - 1) / num_workers;

                thread::scope(|s| {
                    let mut handles = Vec::with_capacity(num_workers);
                    for chunk in encode_regions.chunks(chunk_size) {
                        handles.push(s.spawn(move || {
                            let mut local_tiles = Vec::with_capacity(chunk.len());
                            for &(rx, ry, rw, rh) in chunk {
                                if let Some(tile) = encode_region_jpeg(
                                    frame,
                                    stride,
                                    x_map,
                                    y_map,
                                    is_native_1to1,
                                    rx,
                                    ry,
                                    rw,
                                    rh,
                                    encode_quality,
                                ) {
                                    local_tiles.push(tile);
                                }
                            }
                            local_tiles
                        }));
                    }
                    let mut all_tiles = Vec::with_capacity(encode_regions.len());
                    for h in handles {
                        if let Ok(mut part) = h.join() {
                            all_tiles.append(&mut part);
                        }
                    }
                    all_tiles
                })
            };

            TileBatch {
                native_w,
                native_h,
                out_w: out_w as u16,
                out_h: out_h as u16,
                tiles,
                would_block: false,
                is_full_motion: is_heavy_motion,
                capture_error: None,
            }
        }

        /// Check dirty tiles via LUT and convert BGRA -> BT.601 Studio NV12 in parallel (0 bytes when idle)
        fn process_bgra_to_nv12(
            &mut self,
            frame: &[u8],
            src_w: usize,
            src_h: usize,
            stride: usize,
            native_w: u32,
            native_h: u32,
            out_w: usize,
            out_h: usize,
            force_keyframe: bool,
            backend: &'static str,
        ) -> Nv12Frame {
            let out_w = (out_w & !1).max(2);
            let out_h = (out_h & !1).max(2);
            let cols = (out_w + TILE_SIZE - 1) / TILE_SIZE;
            let rows = (out_h + TILE_SIZE - 1) / TILE_SIZE;
            let total_tiles = cols * rows;

            let dim_changed = self.ensure_luts(src_w, src_h, out_w, out_h, total_tiles);
            let frame_len = frame.len();
            let mut any_dirty = force_keyframe || dim_changed;

            for ty in 0..rows {
                let dst_y0 = ty * TILE_SIZE;
                let tile_h = (out_h - dst_y0).min(TILE_SIZE);
                for tx in 0..cols {
                    let dst_x0 = tx * TILE_SIZE;
                    let tile_w = (out_w - dst_x0).min(TILE_SIZE);
                    let tile_idx = ty * cols + tx;

                    let mut hash: u64 = 0xcbf29ce484222325;
                    let mut dy = 0;
                    while dy < tile_h {
                        let row_off = self.y_map[dst_y0 + dy] * stride;
                        if row_off + stride <= frame_len {
                            let row = &frame[row_off..row_off + stride];
                            let mut dx = 0;
                            while dx < tile_w {
                                let sx4 = self.x_map[dst_x0 + dx];
                                let px = u32::from_ne_bytes([row[sx4], row[sx4 + 1], row[sx4 + 2], 0]);
                                hash = hash.wrapping_mul(0x100000001b3) ^ (px as u64);
                                dx += 6;
                            }
                        }
                        dy += 6;
                    }

                    if self.tile_hashes[tile_idx] != hash {
                        self.tile_hashes[tile_idx] = hash;
                        any_dirty = true;
                    }
                }
            }

            if !any_dirty {
                return Nv12Frame {
                    native_w,
                    native_h,
                    out_w: out_w as u16,
                    out_h: out_h as u16,
                    nv12: Vec::new(),
                    would_block: false,
                    capture_error: None,
                    backend,
                };
            }

            let y_size = out_w * out_h;
            let uv_size = y_size / 2;
            let mut nv12 = vec![0u8; y_size + uv_size];
            let (y_plane, uv_plane) = nv12.split_at_mut(y_size);

            let y_map = &self.y_map;
            let x_map = &self.x_map;
            let strips = self.worker_threads.clamp(2, 4);
            let pair_rows = out_h / 2;
            let pairs_per_strip = ((pair_rows + strips - 1) / strips).max(1);
            let y_chunk_bytes = pairs_per_strip * 2 * out_w;
            let uv_chunk_bytes = pairs_per_strip * out_w;

            thread::scope(|s| {
                for (strip_idx, (y_chunk, uv_chunk)) in y_plane
                    .chunks_mut(y_chunk_bytes)
                    .zip(uv_plane.chunks_mut(uv_chunk_bytes))
                    .enumerate()
                {
                    let start_y = strip_idx * pairs_per_strip * 2;
                    s.spawn(move || {
                        let chunk_rows = y_chunk.len() / out_w;
                        for pair_idx in 0..(chunk_rows / 2) {
                            let dy0 = start_y + pair_idx * 2;
                            let dy1 = dy0 + 1;
                            let sy0 = y_map[dy0] * stride;
                            let sy1 = y_map[dy1] * stride;
                            let y_pair = &mut y_chunk[pair_idx * 2 * out_w..(pair_idx + 1) * 2 * out_w];
                            let (y_row0, y_row1) = y_pair.split_at_mut(out_w);
                            let uv_row = &mut uv_chunk[pair_idx * out_w..(pair_idx + 1) * out_w];

                            if sy0 + stride > frame_len || sy1 + stride > frame_len {
                                continue;
                            }
                            let src0 = &frame[sy0..sy0 + stride];
                            let src1 = &frame[sy1..sy1 + stride];

                            let mut dx = 0;
                            while dx < out_w {
                                let sx0 = x_map[dx];
                                let sx1 = x_map[dx + 1];

                                let b00 = src0[sx0] as i32;
                                let g00 = src0[sx0 + 1] as i32;
                                let r00 = src0[sx0 + 2] as i32;
                                let b01 = src0[sx1] as i32;
                                let g01 = src0[sx1 + 1] as i32;
                                let r01 = src0[sx1 + 2] as i32;

                                let b10 = src1[sx0] as i32;
                                let g10 = src1[sx0 + 1] as i32;
                                let r10 = src1[sx0 + 2] as i32;
                                let b11 = src1[sx1] as i32;
                                let g11 = src1[sx1 + 1] as i32;
                                let r11 = src1[sx1 + 2] as i32;

                                y_row0[dx] = (((66 * r00 + 129 * g00 + 25 * b00 + 128) >> 8) + 16) as u8;
                                y_row0[dx + 1] = (((66 * r01 + 129 * g01 + 25 * b01 + 128) >> 8) + 16) as u8;
                                y_row1[dx] = (((66 * r10 + 129 * g10 + 25 * b10 + 128) >> 8) + 16) as u8;
                                y_row1[dx + 1] = (((66 * r11 + 129 * g11 + 25 * b11 + 128) >> 8) + 16) as u8;

                                let u = (((-38 * r00 - 74 * g00 + 112 * b00 + 128) >> 8) + 128).clamp(16, 240) as u8;
                                let v = (((112 * r00 - 94 * g00 - 18 * b00 + 128) >> 8) + 128).clamp(16, 240) as u8;
                                uv_row[dx] = u;
                                uv_row[dx + 1] = v;

                                dx += 2;
                            }
                        }
                    });
                }
            });

            Nv12Frame {
                native_w,
                native_h,
                out_w: out_w as u16,
                out_h: out_h as u16,
                nv12,
                would_block: false,
                capture_error: None,
                backend,
            }
        }
    }

    pub struct WindowsCapturer {
        capturer: Option<Capturer>,
        gdi: PersistentGdiSurface,
        processor: TileProcessor,
        width: usize,
        height: usize,
        rgb_buffer: Vec<u8>,
        last_hash: u64,
        force_interval_counter: u32,
        dxgi_retry_counter: u32,
    }

    impl WindowsCapturer {
        /// Shared DXGI / GDI capture acquisition used by both JPEG dirty tiles and NV12 H.264 frames.
        fn capture_scaled_frame<R, F, G>(
            &mut self,
            max_width: u32,
            mut force_keyframe: bool,
            mut on_frame: F,
            mut on_blocked: G,
        ) -> Result<R>
        where
            F: FnMut(&mut TileProcessor, &[u8], usize, usize, usize, u32, u32, usize, usize, bool, &'static str) -> R,
            G: FnMut(&TileProcessor, u32, u32, Option<String>, &'static str) -> R,
        {
            // Proactively track Default <-> Winlogon (Lock Screen / UAC Secure Desktop) transitions
            self.dxgi_retry_counter = self.dxgi_retry_counter.wrapping_add(1);
            if force_keyframe || self.dxgi_retry_counter % 10 == 0 {
                if let Some(new_desk) = sync_thread_to_active_input_desktop() {
                    crate::diag_log(&format!(
                        "Capture thread switched to active desktop '{}' (rebuilding DXGI/GDI)",
                        new_desk
                    ));
                    self.capturer = None;
                    self.gdi.release_resources();
                    force_keyframe = true;
                }
            }

            // Recover DXGI rapidly (~every 0.8s) if currently in GDI fallback
            if self.capturer.is_none() && self.dxgi_retry_counter % 40 == 0 {
                if let Ok(display) = Display::primary() {
                    let dw = display.width();
                    let dh = display.height();
                    if dw > 0 && dh > 0 {
                        if let Ok(c) = Capturer::new(display) {
                            log::info!("Recovered DXGI Desktop Duplication capturer ({}x{})", dw, dh);
                            self.capturer = Some(c);
                            self.width = dw;
                            self.height = dh;
                        }
                    }
                }
            }

            // GDI DIBSection fast path
            if self.capturer.is_none() {
                let (gw, gh) = unsafe {
                    (
                        GetSystemMetrics(0).max(0) as usize,
                        GetSystemMetrics(1).max(0) as usize,
                    )
                };
                if gw > 0 && gh > 0 && (gw != self.width || gh != self.height) {
                    self.width = gw;
                    self.height = gh;
                }
                let (out_w, out_h) = TileProcessor::compute_out_dims(self.width, self.height, max_width);
                let mut captured = self.gdi.capture_scaled(self.width, self.height, out_w, out_h);
                if captured.is_none() {
                    try_reattach_input_desktop();
                    captured = self.gdi.capture_scaled(self.width, self.height, out_w, out_h);
                }
                if let Some(bgra) = captured {
                    let stride = out_w * 4;
                    return Ok(on_frame(
                        &mut self.processor,
                        bgra,
                        out_w,
                        out_h,
                        stride,
                        self.width as u32,
                        self.height as u32,
                        out_w,
                        out_h,
                        force_keyframe,
                        "GDI",
                    ));
                }
                thread::sleep(Duration::from_millis(80));
                return Ok(on_blocked(
                    &self.processor,
                    self.width as u32,
                    self.height as u32,
                    self.gdi.last_err.clone(),
                    "GDI",
                ));
            }

            // DXGI path: detect OS display resolution changes immediately before grabbing
            let (cur_gw, cur_gh) = unsafe {
                (
                    GetSystemMetrics(0).max(0) as usize,
                    GetSystemMetrics(1).max(0) as usize,
                )
            };
            if cur_gw > 0 && cur_gh > 0 && (cur_gw != self.width || cur_gh != self.height) {
                log::info!(
                    "Detected display resolution change: {}x{} -> {}x{}, reinitializing capturer",
                    self.width,
                    self.height,
                    cur_gw,
                    cur_gh
                );
                self.width = cur_gw;
                self.height = cur_gh;
                self.capturer = None;
                self.dxgi_retry_counter = 38;
                let (out_w, out_h) = TileProcessor::compute_out_dims(self.width, self.height, max_width);
                if let Some(bgra) = self.gdi.capture_scaled(self.width, self.height, out_w, out_h) {
                    let stride = out_w * 4;
                    return Ok(on_frame(
                        &mut self.processor,
                        bgra,
                        out_w,
                        out_h,
                        stride,
                        self.width as u32,
                        self.height as u32,
                        out_w,
                        out_h,
                        true,
                        "GDI",
                    ));
                }
            }

            let (out_w, out_h) = TileProcessor::compute_out_dims(self.width, self.height, max_width);
            let max_retries = if force_keyframe { 6 } else { 1 };
            let mut retries = 0;

            loop {
                let frame_res = match self.capturer.as_mut() {
                    Some(c) => c.frame(),
                    None => break,
                };
                match frame_res {
                    Ok(frame) => {
                        let stride = frame.len() / self.height.max(1);
                        return Ok(on_frame(
                            &mut self.processor,
                            &frame,
                            self.width,
                            self.height,
                            stride,
                            self.width as u32,
                            self.height as u32,
                            out_w,
                            out_h,
                            force_keyframe,
                            "DXGI",
                        ));
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        retries += 1;
                        if retries > max_retries {
                            let need_gdi_snap = force_keyframe
                                || (self.processor.needs_sharp_polish
                                    && self.processor.last_motion_at.elapsed() >= Duration::from_millis(130));
                            if need_gdi_snap {
                                if let Some(bgra) = self.gdi.capture_scaled(self.width, self.height, out_w, out_h) {
                                    let stride = out_w * 4;
                                    return Ok(on_frame(
                                        &mut self.processor,
                                        bgra,
                                        out_w,
                                        out_h,
                                        stride,
                                        self.width as u32,
                                        self.height as u32,
                                        out_w,
                                        out_h,
                                        force_keyframe,
                                        "GDI",
                                    ));
                                }
                            }
                            return Ok(on_blocked(
                                &self.processor,
                                self.width as u32,
                                self.height as u32,
                                if need_gdi_snap { self.gdi.last_err.clone() } else { None },
                                "DXGI",
                            ));
                        }
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(e) => {
                        log::warn!("DXGI capture lost ({}), switching seamlessly to persistent GDI DIBSection", e);
                        self.capturer = None;
                        self.dxgi_retry_counter = 1;
                        break;
                    }
                }
            }

            if let Some(bgra) = self.gdi.capture_scaled(self.width, self.height, out_w, out_h) {
                let stride = out_w * 4;
                return Ok(on_frame(
                    &mut self.processor,
                    bgra,
                    out_w,
                    out_h,
                    stride,
                    self.width as u32,
                    self.height as u32,
                    out_w,
                    out_h,
                    force_keyframe,
                    "GDI",
                ));
            }
            Ok(on_blocked(
                &self.processor,
                self.width as u32,
                self.height as u32,
                self.gdi.last_err.clone(),
                "GDI",
            ))
        }
    }

    impl ScreenCapturer for WindowsCapturer {
        fn new() -> Result<Self> {
            let worker_threads = thread::available_parallelism()
                .map(|n| n.get().clamp(2, 6))
                .unwrap_or(4);

            let (capturer, width, height) = match Display::primary().and_then(|display| {
                let w = display.width();
                let h = display.height();
                Capturer::new(display).map(|c| (c, w, h))
            }) {
                Ok((c, w, h)) if w > 0 && h > 0 => {
                    log::info!("Initialized DXGI Desktop Duplication capturer ({}x{})", w, h);
                    (Some(c), w, h)
                }
                Err(e) => {
                    let (gw, gh) = unsafe {
                        (
                            GetSystemMetrics(0).max(0) as usize,
                            GetSystemMetrics(1).max(0) as usize,
                        )
                    };
                    if gw == 0 || gh == 0 {
                        return Err(anyhow!("DXGI failed ({}) and GDI screen metrics unavailable", e));
                    }
                    log::warn!(
                        "DXGI capturer unavailable ({}), using persistent GDI DIBSection ({}x{})",
                        e,
                        gw,
                        gh
                    );
                    (None, gw, gh)
                }
                Ok(_) => {
                    let (gw, gh) = unsafe {
                        (
                            GetSystemMetrics(0).max(0) as usize,
                            GetSystemMetrics(1).max(0) as usize,
                        )
                    };
                    (None, gw.max(1280), gh.max(720))
                }
            };

            Ok(Self {
                capturer,
                gdi: PersistentGdiSurface::new(),
                processor: TileProcessor::new(worker_threads),
                width,
                height,
                rgb_buffer: Vec::new(),
                last_hash: 0,
                force_interval_counter: 0,
                dxgi_retry_counter: 0,
            })
        }

        fn capture_dirty_tiles(
            &mut self,
            max_width: u32,
            quality: u8,
            force_keyframe: bool,
        ) -> Result<TileBatch> {
            self.capture_scaled_frame(
                max_width,
                force_keyframe,
                |proc, frame, sw, sh, stride, nw, nh, ow, oh, kf, _| {
                    proc.process_bgra_frame(frame, sw, sh, stride, nw, nh, ow, oh, quality, kf)
                },
                |proc, nw, nh, err, _| TileBatch {
                    native_w: nw,
                    native_h: nh,
                    out_w: proc.last_out_dims.0 as u16,
                    out_h: proc.last_out_dims.1 as u16,
                    tiles: Vec::new(),
                    would_block: true,
                    is_full_motion: false,
                    capture_error: err,
                },
            )
        }

        fn capture_nv12(&mut self, max_width: u32, force_keyframe: bool) -> Result<Nv12Frame> {
            self.capture_scaled_frame(
                max_width,
                force_keyframe,
                |proc, frame, sw, sh, stride, nw, nh, ow, oh, kf, backend| {
                    proc.process_bgra_to_nv12(frame, sw, sh, stride, nw, nh, ow, oh, kf, backend)
                },
                |proc, nw, nh, err, backend| Nv12Frame {
                    native_w: nw,
                    native_h: nh,
                    out_w: proc.last_out_dims.0 as u16,
                    out_h: proc.last_out_dims.1 as u16,
                    nv12: Vec::new(),
                    would_block: true,
                    capture_error: err,
                    backend,
                },
            )
        }

        fn capture_frame(&mut self) -> Result<CaptureFrame> {
            let mut retries = 0;
            let raw_bgra: Vec<u8> = if let Some(ref mut cap) = self.capturer {
                loop {
                    match cap.frame() {
                        Ok(frame) => break frame.to_vec(),
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            retries += 1;
                            if retries > 10 {
                                return Err(anyhow!("Timeout waiting for frame"));
                            }
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(e) => return Err(anyhow!("Failed to capture screen frame: {}", e)),
                    }
                }
            } else {
                self.gdi
                    .capture_scaled(self.width, self.height, self.width, self.height)
                    .map(|s| s.to_vec())
                    .ok_or_else(|| anyhow!("GDI capture failed"))?
            };
            let frame = raw_bgra.as_slice();

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

            let stride = frame.len() / self.height.max(1);
            let needed = self.width * self.height * 3;
            if self.rgb_buffer.len() != needed {
                self.rgb_buffer.resize(needed, 0);
            }
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
    use super::{CaptureFrame, Nv12Frame, ScreenCapturer, TileBatch};
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

        fn capture_nv12(&mut self, _max_width: u32, _force_keyframe: bool) -> Result<Nv12Frame> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn bench_parallel_strip_encoding() {
        let w = 1920usize;
        let h = 1080usize;
        let stride = w * 4;
        let mut frame = vec![0u8; stride * h];
        for y in 0..h {
            for x in 0..w {
                let off = y * stride + x * 4;
                let grid = if x % 32 == 0 || y % 32 == 0 { 220 } else { 0 };
                frame[off] = ((x * 255 / w) as u8).saturating_add(grid / 4);
                frame[off + 1] = ((y * 255 / h) as u8).saturating_add(grid / 4);
                frame[off + 2] = (((x ^ y) & 0xff) as u8 / 3).saturating_add(grid / 2);
                frame[off + 3] = 255;
            }
        }

        #[cfg(target_os = "windows")]
        {
            let x_map: Vec<usize> = (0..w).map(|x| x * 4).collect();
            let y_map: Vec<usize> = (0..h).collect();

            let strips = 4usize;
            let base_h = ((h / strips) + 15) & !15;
            let mut regions = Vec::new();
            let mut y = 0;
            while y < h {
                let rh = if regions.len() + 1 == strips { h - y } else { base_h.min(h - y) };
                regions.push((0usize, y, w, rh));
                y += rh;
            }
            let frame_ref = &frame;
            let x_ref = &x_map;
            let y_ref = &y_map;
            // Warm up thread-local scratch once, then measure steady-state motion quality (q=42)
            let t0 = Instant::now();
            let tiles: Vec<DirtyTile> = std::thread::scope(|s| {
                let handles: Vec<_> = regions
                    .iter()
                    .map(|&(rx, ry, rw, rh)| {
                        s.spawn(move || {
                            windows_impl::encode_region_jpeg_for_test(
                                frame_ref, stride, x_ref, y_ref, true, rx, ry, rw, rh, 42,
                            )
                        })
                    })
                    .collect();
                handles.into_iter().filter_map(|h| h.join().ok().flatten()).collect()
            });
            let dt = t0.elapsed();
            let bytes: usize = tiles.iter().map(|t| t.data.len()).sum();
            println!(
                "Parallel 4-strip 1080p motion encode (q=42): {:?}, tiles={}, bytes={} ({:.1} KB)",
                dt,
                tiles.len(),
                bytes,
                bytes as f64 / 1024.0
            );
            assert_eq!(tiles.len(), 4);
            assert!(bytes > 0);
        }
    }

    #[test]
    fn test_real_capturer_handles_desktop_gracefully() {
        let mut cap = create_capturer().expect("create_capturer must succeed (DXGI or GDI)");
        let (w, h) = cap.resolution();
        assert!(w > 0 && h > 0);
        let t0 = Instant::now();
        let batch = cap.capture_dirty_tiles(1920, 65, true).expect("capture_dirty_tiles must not panic");
        println!(
            "Real capturer {}x{} -> {}x{} in {:?}: tiles={}, bytes={}, would_block={}",
            w,
            h,
            batch.out_w,
            batch.out_h,
            t0.elapsed(),
            batch.tiles.len(),
            batch.tiles.iter().map(|t| t.data.len()).sum::<usize>(),
            batch.would_block
        );
    }
}
