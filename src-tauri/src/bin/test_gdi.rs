use anyhow::Result;

#[link(name = "user32")]
extern "system" {
    fn GetDesktopWindow() -> isize;
    fn GetWindowDC(hWnd: isize) -> isize;
    fn ReleaseDC(hWnd: isize, hDC: isize) -> i32;
    fn GetSystemMetrics(nIndex: i32) -> i32;
    fn GetLastError() -> u32;
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateCompatibleDC(hDC: isize) -> isize;
    fn CreateCompatibleBitmap(hDC: isize, nWidth: i32, nHeight: i32) -> isize;
    fn SelectObject(hDC: isize, hGDIOBJ: isize) -> isize;
    fn DeleteObject(hGDIOBJ: isize) -> i32;
    fn DeleteDC(hDC: isize) -> i32;
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
    fn GetDIBits(
        hdc: isize,
        hbm: isize,
        start: u32,
        cLines: u32,
        lpvBits: *mut u8,
        lpbmi: *mut BITMAPINFO,
        usage: u32,
    ) -> i32;
}

#[repr(C)]
struct BITMAPINFOHEADER {
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
struct BITMAPINFO {
    bmi_header: BITMAPINFOHEADER,
    bmi_colors: [u32; 1],
}

fn capture_screen() -> Result<(u32, u32, usize)> {
    unsafe {
        let width = GetSystemMetrics(0); // SM_CXSCREEN
        let height = GetSystemMetrics(1); // SM_CYSCREEN
        println!("Screen resolution: {}x{}", width, height);

        let hwnd = GetDesktopWindow();
        let hdc_screen = GetWindowDC(hwnd);
        if hdc_screen == 0 {
            return Err(anyhow::anyhow!("GetWindowDC(desktop) returned NULL"));
        }

        let hdc_mem = CreateCompatibleDC(hdc_screen);
        if hdc_mem == 0 {
            ReleaseDC(hwnd, hdc_screen);
            return Err(anyhow::anyhow!("CreateCompatibleDC failed"));
        }

        let hbitmap = CreateCompatibleBitmap(hdc_screen, width, height);
        if hbitmap == 0 {
            DeleteDC(hdc_mem);
            ReleaseDC(hwnd, hdc_screen);
            return Err(anyhow::anyhow!("CreateCompatibleBitmap failed"));
        }

        let old_bitmap = SelectObject(hdc_mem, hbitmap);

        // SRCCOPY | CAPTUREBLT (0x40000000 ensures layered windows/RDP windows are captured)
        let rop = 0x00CC0020 | 0x40000000;
        let success = BitBlt(hdc_mem, 0, 0, width, height, hdc_screen, 0, 0, rop);

        if success == 0 {
            let err = GetLastError();
            SelectObject(hdc_mem, old_bitmap);
            DeleteObject(hbitmap);
            DeleteDC(hdc_mem);
            ReleaseDC(hwnd, hdc_screen);
            return Err(anyhow::anyhow!("BitBlt failed: GetLastError = {}", err));
        }

        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmi_header.bi_size = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmi_header.bi_width = width;
        bmi.bmi_header.bi_height = -height; // Negative for top-down DIB
        bmi.bmi_header.bi_planes = 1;
        bmi.bmi_header.bi_bit_count = 32;
        bmi.bmi_header.bi_compression = 0; // BI_RGB

        let mut buffer = vec![0u8; (width * height * 4) as usize];
        let lines = GetDIBits(
            hdc_mem,
            hbitmap,
            0,
            height as u32,
            buffer.as_mut_ptr(),
            &mut bmi,
            0,
        );

        SelectObject(hdc_mem, old_bitmap);
        DeleteObject(hbitmap);
        DeleteDC(hdc_mem);
        ReleaseDC(hwnd, hdc_screen);

        if lines == 0 {
            return Err(anyhow::anyhow!("GetDIBits failed"));
        }

        Ok((width as u32, height as u32, buffer.len()))
    }
}

fn main() {
    match capture_screen() {
        Ok((w, h, sz)) => println!("🎉 捕获成功！分辨率: {}x{}, 缓冲区大小: {} 字节", w, h, sz),
        Err(e) => eprintln!("❌ 捕获失败: {}", e),
    }
}
