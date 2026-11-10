use anyhow::Result;

#[link(name = "user32")]
extern "system" {
    fn GetDC(hWnd: isize) -> isize;
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

fn test_capture_null_dc() -> Result<()> {
    unsafe {
        let width = GetSystemMetrics(0);
        let height = GetSystemMetrics(1);
        println!("Resolution: {}x{}", width, height);

        let hdc_screen = GetDC(0);
        if hdc_screen == 0 {
            return Err(anyhow::anyhow!("GetDC(NULL) failed"));
        }

        let hdc_mem = CreateCompatibleDC(hdc_screen);
        let hbm = CreateCompatibleBitmap(hdc_screen, width, height);
        let old = SelectObject(hdc_mem, hbm);

        // Try standard SRCCOPY (0x00CC0020) without CAPTUREBLT
        let ok = BitBlt(hdc_mem, 0, 0, width, height, hdc_screen, 0, 0, 0x00CC0020);
        let err = if ok == 0 { GetLastError() } else { 0 };

        SelectObject(hdc_mem, old);
        DeleteObject(hbm);
        DeleteDC(hdc_mem);
        ReleaseDC(0, hdc_screen);

        if ok != 0 {
            println!("🎉 GetDC(0) + BitBlt(SRCCOPY) 成功抓取！");
            Ok(())
        } else {
            Err(anyhow::anyhow!("BitBlt failed: error {}", err))
        }
    }
}

fn main() {
    match test_capture_null_dc() {
        Ok(_) => println!("✅ GDI 测试通过！"),
        Err(e) => eprintln!("❌ GDI 测试失败: {}", e),
    }
}
