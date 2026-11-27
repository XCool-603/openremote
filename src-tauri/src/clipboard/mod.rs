use anyhow::{anyhow, Result};
use arboard::Clipboard;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::thread;
use std::time::Duration;

static LAST_APPLIED_HASH: StdMutex<String> = StdMutex::new(String::new());
static LAST_APPLIED_FILES_HASH: StdMutex<String> = StdMutex::new(String::new());

fn calc_hash(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn calc_files_hash(paths: &[PathBuf]) -> String {
    let mut hasher = Sha256::new();
    for p in paths {
        hasher.update(p.to_string_lossy().as_bytes());
        hasher.update(b"|");
        if let Ok(meta) = std::fs::metadata(p) {
            hasher.update(&meta.len().to_le_bytes());
            if let Ok(mtime) = meta.modified() {
                if let Ok(dur) = mtime.duration_since(std::time::UNIX_EPOCH) {
                    hasher.update(&dur.as_nanos().to_le_bytes());
                }
            }
        }
    }
    format!("{:x}", hasher.finalize())
}

#[cfg(target_os = "windows")]
mod win_clip_files {
    use super::PathBuf;
    use anyhow::{anyhow, Result};
    use std::ffi::OsStr;
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;

    const CF_HDROP: u32 = 15;
    const GMEM_MOVEABLE: u32 = 0x0002;
    const GMEM_ZEROINIT: u32 = 0x0040;
    const HWND_MESSAGE: isize = -3;

    #[repr(C)]
    #[derive(Copy, Clone)]
    pub(super) struct Point {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    pub(super) struct DropFiles {
        pub p_files: u32,
        pub pt: Point,
        pub f_nc: i32,
        pub f_wide: i32,
    }

    #[link(name = "user32")]
    extern "system" {
        fn OpenClipboard(h_wnd_new_owner: isize) -> i32;
        fn CloseClipboard() -> i32;
        fn EmptyClipboard() -> i32;
        fn IsClipboardFormatAvailable(format: u32) -> i32;
        fn GetClipboardData(u_format: u32) -> isize;
        fn SetClipboardData(u_format: u32, h_mem: isize) -> isize;
        fn GetClipboardSequenceNumber() -> u32;
        fn OpenInputDesktop(dw_flags: u32, f_inherit: i32, dw_desired_access: u32) -> isize;
        fn SetThreadDesktop(h_desktop: isize) -> i32;
        fn CloseDesktop(h_desktop: isize) -> i32;
        fn CreateWindowExW(
            dw_ex_style: u32,
            lp_class_name: *const u16,
            lp_window_name: *const u16,
            dw_style: u32,
            x: i32,
            y: i32,
            n_width: i32,
            n_height: i32,
            h_wnd_parent: isize,
            h_menu: isize,
            h_instance: isize,
            lp_param: *const u8,
        ) -> isize;
        fn DestroyWindow(h_wnd: isize) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalAlloc(u_flags: u32, dw_bytes: usize) -> isize;
        fn GlobalLock(h_mem: isize) -> *mut u8;
        fn GlobalUnlock(h_mem: isize) -> i32;
        fn GlobalFree(h_mem: isize) -> isize;
    }

    #[link(name = "shell32")]
    extern "system" {
        fn DragQueryFileW(h_drop: isize, i_file: u32, lpsz_file: *mut u16, cch: u32) -> u32;
    }

    fn to_wide(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(Some(0)).collect()
    }

    /// Re-attach current thread (when no windows exist on it) to active interactive desktop.
    /// Important when running under LocalSystem `--host-worker` across lock/unlock/session changes.
    pub fn ensure_input_desktop() {
        unsafe {
            let mut hdesk = OpenInputDesktop(0, 0, 0x000F_01FF);
            if hdesk == 0 {
                hdesk = OpenInputDesktop(0, 0, 0x0001 | 0x0100);
            }
            if hdesk != 0 {
                let _ = SetThreadDesktop(hdesk);
                let _ = CloseDesktop(hdesk);
            }
        }
    }

    pub fn sequence_number() -> u32 {
        unsafe { GetClipboardSequenceNumber() }
    }

    pub fn has_files() -> bool {
        unsafe { IsClipboardFormatAvailable(CF_HDROP) != 0 }
    }

    fn create_message_hwnd() -> isize {
        let cls = to_wide("STATIC");
        let title = to_wide("OpenRemoteClipWnd");
        unsafe {
            CreateWindowExW(
                0,
                cls.as_ptr(),
                title.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                0,
                0,
                ptr::null(),
            )
        }
    }

    /// Build a DROPFILES + UTF-16 double-null-terminated byte buffer for CF_HDROP
    pub(super) fn build_dropfiles_payload(paths: &[PathBuf]) -> Vec<u8> {
        let hdr = DropFiles {
            p_files: size_of::<DropFiles>() as u32,
            pt: Point { x: 0, y: 0 },
            f_nc: 0,
            f_wide: 1,
        };
        let mut wide_chars: Vec<u16> = Vec::new();
        for p in paths {
            wide_chars.extend(OsStr::new(p).encode_wide());
            wide_chars.push(0);
        }
        wide_chars.push(0); // double null terminator

        let total_bytes = size_of::<DropFiles>() + wide_chars.len() * 2;
        let mut buf = Vec::with_capacity(total_bytes);
        let hdr_bytes =
            unsafe { std::slice::from_raw_parts(&hdr as *const DropFiles as *const u8, size_of::<DropFiles>()) };
        buf.extend_from_slice(hdr_bytes);
        for u in wide_chars {
            buf.extend_from_slice(&u.to_le_bytes());
        }
        buf
    }

    pub fn get_clipboard_files() -> Result<Vec<PathBuf>> {
        unsafe {
            if IsClipboardFormatAvailable(CF_HDROP) == 0 {
                return Ok(Vec::new());
            }
            let mut opened = false;
            for _ in 0..5 {
                if OpenClipboard(0) != 0 {
                    opened = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            if !opened {
                return Err(anyhow!(
                    "OpenClipboard failed: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let mut result = Vec::new();
            let h_drop = GetClipboardData(CF_HDROP);
            if h_drop != 0 {
                let count = DragQueryFileW(h_drop, u32::MAX, ptr::null_mut(), 0);
                let capped = count.min(64);
                for idx in 0..capped {
                    let len = DragQueryFileW(h_drop, idx, ptr::null_mut(), 0);
                    if len > 0 && len < 32768 {
                        let mut buf = vec![0u16; (len + 1) as usize];
                        let copied = DragQueryFileW(h_drop, idx, buf.as_mut_ptr(), len + 1);
                        if copied > 0 {
                            let s = String::from_utf16_lossy(&buf[..copied as usize]);
                            result.push(PathBuf::from(s));
                        }
                    }
                }
            }
            CloseClipboard();
            Ok(result)
        }
    }

    pub fn set_clipboard_files(paths: &[PathBuf]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let paths_owned = paths.to_vec();
        // Run on a clean short-lived OS thread attached to active desktop with its own HWND_MESSAGE
        // owner window so MSDN's OpenClipboard(hwnd) + EmptyClipboard() + SetClipboardData() contract holds.
        let res = std::thread::spawn(move || -> Result<()> {
            ensure_input_desktop();
            let payload = build_dropfiles_payload(&paths_owned);
            unsafe {
                let hwnd = create_message_hwnd();
                let mut opened = false;
                for _ in 0..8 {
                    if OpenClipboard(hwnd) != 0 {
                        opened = true;
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                if !opened {
                    let err = std::io::Error::last_os_error();
                    if hwnd != 0 {
                        DestroyWindow(hwnd);
                    }
                    return Err(anyhow!("OpenClipboard(hwnd={}) failed: {}", hwnd, err));
                }

                if EmptyClipboard() == 0 {
                    let err = std::io::Error::last_os_error();
                    CloseClipboard();
                    if hwnd != 0 {
                        DestroyWindow(hwnd);
                    }
                    return Err(anyhow!("EmptyClipboard failed: {}", err));
                }

                let h_mem = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, payload.len());
                if h_mem == 0 {
                    let err = std::io::Error::last_os_error();
                    CloseClipboard();
                    if hwnd != 0 {
                        DestroyWindow(hwnd);
                    }
                    return Err(anyhow!("GlobalAlloc failed: {}", err));
                }

                let ptr_mem = GlobalLock(h_mem);
                if ptr_mem.is_null() {
                    let err = std::io::Error::last_os_error();
                    GlobalFree(h_mem);
                    CloseClipboard();
                    if hwnd != 0 {
                        DestroyWindow(hwnd);
                    }
                    return Err(anyhow!("GlobalLock failed: {}", err));
                }
                ptr::copy_nonoverlapping(payload.as_ptr(), ptr_mem, payload.len());
                GlobalUnlock(h_mem);

                if SetClipboardData(CF_HDROP, h_mem) == 0 {
                    let err = std::io::Error::last_os_error();
                    GlobalFree(h_mem);
                    CloseClipboard();
                    if hwnd != 0 {
                        DestroyWindow(hwnd);
                    }
                    return Err(anyhow!("SetClipboardData(CF_HDROP) failed: {}", err));
                }

                // Clipboard owns h_mem once SetClipboardData succeeds
                CloseClipboard();
                if hwnd != 0 {
                    DestroyWindow(hwnd);
                }
                Ok(())
            }
        })
        .join()
        .map_err(|_| anyhow!("Clipboard writer thread panicked"))?;

        res
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn verify_dropfiles_struct_and_payload() {
            assert_eq!(size_of::<DropFiles>(), 20, "Win32 DROPFILES header must be 20 bytes");
            let payload = build_dropfiles_payload(&[PathBuf::from("C:\\test\\a.txt")]);
            assert_eq!(u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]), 20);
            assert_eq!(u32::from_le_bytes([payload[16], payload[17], payload[18], payload[19]]), 1);
            assert_eq!(&payload[payload.len() - 4..], &[0, 0, 0, 0]);

            // Verify HWND_MESSAGE + GetClipboardSequenceNumber works without touching clipboard contents
            let hwnd = create_message_hwnd();
            assert_ne!(hwnd, 0, "HWND_MESSAGE creation should succeed");
            unsafe { DestroyWindow(hwnd); }
            let _seq = sequence_number();
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod win_clip_files {
    use super::PathBuf;
    use anyhow::Result;

    pub fn ensure_input_desktop() {}
    pub fn sequence_number() -> u32 {
        0
    }
    pub fn has_files() -> bool {
        false
    }
    pub fn get_clipboard_files() -> Result<Vec<PathBuf>> {
        Ok(Vec::new())
    }
    pub fn set_clipboard_files(_paths: &[PathBuf]) -> Result<()> {
        Ok(())
    }
}

/// ClipboardMonitor periodically checks the system clipboard for text AND copied files (CF_HDROP)
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

    /// Read current clipboard file paths (Windows CF_HDROP)
    pub fn get_files() -> Result<Vec<PathBuf>> {
        win_clip_files::get_clipboard_files()
    }

    /// Set system clipboard text and mark hash to avoid echo loop
    pub async fn set_text_silent(content: &str) -> Result<()> {
        let hash = calc_hash(content);
        if let Ok(mut applied) = LAST_APPLIED_HASH.lock() {
            *applied = hash;
        }

        let owned = content.to_string();
        tokio::task::spawn_blocking(move || -> Result<()> {
            win_clip_files::ensure_input_desktop();
            let mut clipboard =
                Clipboard::new().map_err(|e| anyhow!("Failed to access clipboard: {}", e))?;
            clipboard
                .set_text(owned)
                .map_err(|e| anyhow!("Failed to set clipboard text: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Clipboard text task failed: {}", e))?
    }

    /// Set system clipboard files (CF_HDROP) and mark hash to avoid echo loop
    pub fn set_files_silent(paths: &[PathBuf]) -> Result<()> {
        let hash = calc_files_hash(paths);
        if let Ok(mut applied) = LAST_APPLIED_FILES_HASH.lock() {
            *applied = hash;
        }
        win_clip_files::set_clipboard_files(paths)
    }

    /// Legacy set_text
    pub fn set_text(content: &str) -> Result<()> {
        let mut clipboard =
            Clipboard::new().map_err(|e| anyhow!("Failed to access clipboard: {}", e))?;
        clipboard
            .set_text(content.to_string())
            .map_err(|e| anyhow!("Failed to set clipboard text: {}", e))
    }

    /// Start polling clipboard content (text + files) via zero-lock sequence counter
    pub fn start_monitoring_dual<F, G>(&mut self, on_text: F, on_files: G)
    where
        F: Fn(String) + Send + 'static,
        G: Fn(Vec<PathBuf>) + Send + 'static,
    {
        if self.is_running.swap(true, Ordering::SeqCst) {
            return; // Already running
        }

        let is_running = Arc::clone(&self.is_running);

        thread::spawn(move || {
            win_clip_files::ensure_input_desktop();

            let mut last_seq = win_clip_files::sequence_number();
            let mut last_local_hash = String::new();
            let mut last_local_files_hash = String::new();

            if let Ok(files) = win_clip_files::get_clipboard_files() {
                if !files.is_empty() {
                    last_local_files_hash = calc_files_hash(&files);
                }
            }
            if last_local_files_hash.is_empty() {
                if let Ok(mut clipboard) = Clipboard::new() {
                    if let Ok(text) = clipboard.get_text() {
                        last_local_hash = calc_hash(&text);
                    }
                }
            }

            let mut poll_ticks: u32 = 0;

            while is_running.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(200));
                if !is_running.load(Ordering::SeqCst) {
                    break;
                }

                poll_ticks = poll_ticks.wrapping_add(1);
                let cur_seq = win_clip_files::sequence_number();
                // Inspect only when sequence counter changes (or periodic safety poll every 3s if seq==0)
                let changed = cur_seq != last_seq || (cur_seq == 0 && poll_ticks % 15 == 0);
                if !changed {
                    continue;
                }
                last_seq = cur_seq;

                // Give Explorer a brief moment to finish populating delayed CF_HDROP formats
                thread::sleep(Duration::from_millis(45));
                win_clip_files::ensure_input_desktop();

                // 1. Check copied files (Windows Explorer Ctrl+C / CF_HDROP)
                let mut handled_files = false;
                if win_clip_files::has_files() {
                    match win_clip_files::get_clipboard_files() {
                        Ok(files) if !files.is_empty() => {
                            handled_files = true;
                            let f_hash = calc_files_hash(&files);
                            if f_hash != last_local_files_hash {
                                let is_remote_echo = LAST_APPLIED_FILES_HASH
                                    .lock()
                                    .map(|applied| *applied == f_hash)
                                    .unwrap_or(false);

                                if !is_remote_echo {
                                    crate::diag_log(&format!(
                                        "Clipboard detected {} copied file(s), syncing to peer...",
                                        files.len()
                                    ));
                                    on_files(files);
                                }
                                last_local_files_hash = f_hash;
                            }
                        }
                        Ok(_) => {}
                        Err(e) => {
                            crate::diag_log(&format!("Clipboard CF_HDROP read error: {}", e));
                        }
                    }
                } else {
                    last_local_files_hash.clear();
                }

                // 2. Check copied text (skip if CF_HDROP was just handled so text never clobbers CF_HDROP)
                if !handled_files {
                    if let Ok(mut clipboard) = Clipboard::new() {
                        if let Ok(text) = clipboard.get_text() {
                            let current_hash = calc_hash(&text);

                            if !current_hash.is_empty() && current_hash != last_local_hash {
                                let is_remote_echo = LAST_APPLIED_HASH
                                    .lock()
                                    .map(|applied| *applied == current_hash)
                                    .unwrap_or(false);

                                if !is_remote_echo {
                                    log::info!("Local clipboard changed, syncing to remote ({} bytes)", text.len());
                                    on_text(text.clone());
                                }
                                last_local_hash = current_hash;
                            }
                        }
                    }
                }
            }
        });
    }

    /// Backwards-compatible text-only monitor wrapper
    pub fn start_monitoring<F>(&mut self, callback: F)
    where
        F: Fn(String) + Send + 'static,
    {
        self.start_monitoring_dual(callback, |_| {});
    }

    /// Stop monitoring the clipboard
    pub fn stop_monitoring(&self) {
        self.is_running.store(false, Ordering::SeqCst);
    }
}
