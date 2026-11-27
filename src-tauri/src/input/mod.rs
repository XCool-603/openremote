use crate::protocol::{KeyAction, KeyEvent, MouseAction, MouseButton, MouseEvent};

/// Result of attempting to change the remote host's physical display resolution
#[derive(Debug, Clone)]
pub struct DisplayChangeResult {
    pub applied_os: bool,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
    pub message: String,
}

/// Live Windows session status (detects RDP / Disconnected RDP sessions that cause BitBlt/SendInput os error 5)
#[derive(Debug, Clone, Copy)]
pub struct RdpSessionStatus {
    pub session_id: u32,
    pub is_rdp: bool,
    pub is_disconnected: bool,
}

#[cfg(target_os = "windows")]
mod win_input {
    use super::{DisplayChangeResult, KeyAction, KeyEvent, MouseAction, MouseButton, MouseEvent};
    use anyhow::{anyhow, Result};
    use std::mem::{size_of, zeroed};
    use std::ptr;
    use std::sync::mpsc::{self, SyncSender};
    use std::thread;

    const INPUT_MOUSE: u32 = 0;
    const INPUT_KEYBOARD: u32 = 1;

    const MOUSEEVENTF_MOVE: u32 = 0x0001;
    const MOUSEEVENTF_LEFTDOWN: u32 = 0x0002;
    const MOUSEEVENTF_LEFTUP: u32 = 0x0004;
    const MOUSEEVENTF_RIGHTDOWN: u32 = 0x0008;
    const MOUSEEVENTF_RIGHTUP: u32 = 0x0010;
    const MOUSEEVENTF_MIDDLEDOWN: u32 = 0x0020;
    const MOUSEEVENTF_MIDDLEUP: u32 = 0x0040;
    const MOUSEEVENTF_XDOWN: u32 = 0x0080;
    const MOUSEEVENTF_XUP: u32 = 0x0100;
    const MOUSEEVENTF_WHEEL: u32 = 0x0800;
    const MOUSEEVENTF_ABSOLUTE: u32 = 0x8000;

    const KEYEVENTF_EXTENDEDKEY: u32 = 0x0001;
    const KEYEVENTF_KEYUP: u32 = 0x0002;
    const KEYEVENTF_UNICODE: u32 = 0x0004;

    const WHEEL_DELTA: i32 = 120;
    const SM_CXSCREEN: i32 = 0;
    const SM_CYSCREEN: i32 = 1;
    const MAPVK_VK_TO_VSC: u32 = 0;

    // Display settings constants
    const DM_BITSPERPEL: u32 = 0x0004_0000;
    const DM_PELSWIDTH: u32 = 0x0008_0000;
    const DM_PELSHEIGHT: u32 = 0x0010_0000;
    const DM_DISPLAYFREQUENCY: u32 = 0x0040_0000;
    const CDS_TEST: u32 = 0x0000_0002;
    const DISP_CHANGE_SUCCESSFUL: i32 = 0;

    #[repr(C)]
    #[derive(Copy, Clone)]
    pub(super) struct MouseInput {
        dx: i32,
        dy: i32,
        mouse_data: u32,
        dw_flags: u32,
        time: u32,
        dw_extra_info: usize,
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    pub(super) struct KeybdInput {
        w_vk: u16,
        w_scan: u16,
        dw_flags: u32,
        time: u32,
        dw_extra_info: usize,
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    pub(super) struct HardwareInput {
        u_msg: u32,
        w_param_l: u16,
        w_param_h: u16,
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    pub(super) union InputUnion {
        mi: MouseInput,
        ki: KeybdInput,
        hi: HardwareInput,
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    pub(super) struct Input {
        r#type: u32,
        u: InputUnion,
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    pub(super) struct DevModeW {
        pub dm_device_name: [u16; 32],
        pub dm_spec_version: u16,
        pub dm_driver_version: u16,
        pub dm_size: u16,
        pub dm_driver_extra: u16,
        pub dm_fields: u32,
        pub dm_union1: [u32; 4],
        pub dm_color: i16,
        pub dm_duplex: i16,
        pub dm_y_resolution: i16,
        pub dm_tt_option: i16,
        pub dm_collate: i16,
        pub dm_form_name: [u16; 32],
        pub dm_log_pixels: u16,
        pub dm_bits_per_pel: u32,
        pub dm_pels_width: u32,
        pub dm_pels_height: u32,
        pub dm_display_flags: u32,
        pub dm_display_frequency: u32,
        pub dm_icm_method: u32,
        pub dm_icm_intent: u32,
        pub dm_media_type: u32,
        pub dm_dither_type: u32,
        pub dm_reserved1: u32,
        pub dm_reserved2: u32,
        pub dm_panning_width: u32,
        pub dm_panning_height: u32,
    }

    #[link(name = "user32")]
    extern "system" {
        fn SendInput(c_inputs: u32, p_inputs: *const Input, cb_size: i32) -> u32;
        fn SetCursorPos(x: i32, y: i32) -> i32;
        fn GetSystemMetrics(n_index: i32) -> i32;
        fn MapVirtualKeyW(u_code: u32, u_map_type: u32) -> u32;
        fn OpenInputDesktop(dw_flags: u32, f_inherit: i32, dw_desired_access: u32) -> isize;
        fn GetThreadDesktop(dw_thread_id: u32) -> isize;
        fn GetUserObjectInformationW(
            h_obj: isize,
            n_index: i32,
            pv_info: *mut u8,
            n_length: u32,
            lpn_length_needed: *mut u32,
        ) -> i32;
        fn SetThreadDesktop(h_desktop: isize) -> i32;
        fn CloseDesktop(h_desktop: isize) -> i32;
        fn EnumDisplaySettingsW(
            lpsz_device_name: *const u16,
            i_mode_num: u32,
            lp_dev_mode: *mut DevModeW,
        ) -> i32;
        fn ChangeDisplaySettingsW(lp_dev_mode: *mut DevModeW, dw_flags: u32) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentThreadId() -> u32;
        fn LoadLibraryW(lp_lib_file_name: *const u16) -> isize;
        fn GetProcAddress(h_module: isize, lp_proc_name: *const u8) -> *const std::ffi::c_void;
    }

    fn input_desktop_name(hdesk: isize) -> Option<String> {
        if hdesk == 0 {
            return None;
        }
        let mut buf = [0u16; 128];
        let mut needed: u32 = 0;
        let ok = unsafe {
            GetUserObjectInformationW(
                hdesk,
                2,
                buf.as_mut_ptr() as *mut u8,
                (buf.len() * 2) as u32,
                &mut needed,
            )
        };
        if ok != 0 && needed >= 2 {
            let units = ((needed as usize) / 2).min(buf.len());
            let trimmed = buf[..units].iter().position(|&c| c == 0).unwrap_or(units);
            Some(String::from_utf16_lossy(&buf[..trimmed]))
        } else {
            None
        }
    }

    /// Re-attach the dedicated input thread to the active Windows input desktop (Default OR Winlogon Lock Screen / UAC).
    fn reattach_input_desktop() -> bool {
        unsafe {
            let mut hdesk = OpenInputDesktop(0, 0, 0x000F_01FF);
            if hdesk == 0 {
                hdesk = OpenInputDesktop(0, 0, 0x0001 | 0x0080 | 0x0100 | 0x0002 | 0x0040);
            }
            if hdesk == 0 {
                hdesk = OpenInputDesktop(0, 0, 0x0100);
            }
            if hdesk != 0 {
                let cur = GetThreadDesktop(GetCurrentThreadId());
                let in_name = input_desktop_name(hdesk);
                let cur_name = input_desktop_name(cur);
                if in_name.is_some() && in_name == cur_name {
                    CloseDesktop(hdesk);
                    return true;
                }
                let ok = SetThreadDesktop(hdesk) != 0;
                CloseDesktop(hdesk);
                ok
            } else {
                false
            }
        }
    }

    /// Trigger real Windows Secure Attention Sequence (Ctrl+Alt+Del) via `sas.dll!SendSAS(FALSE)`.
    /// Works natively on Lock Screen / Logon UI when running under `LocalSystem` service!
    pub fn send_sas_ctrl_alt_del() -> Result<()> {
        let dll_name: Vec<u16> = "sas.dll\0".encode_utf16().collect();
        unsafe {
            let hmod = LoadLibraryW(dll_name.as_ptr());
            if hmod == 0 {
                return Err(anyhow!("Failed to load sas.dll"));
            }
            let proc = GetProcAddress(hmod, b"SendSAS\0".as_ptr());
            if proc.is_null() {
                return Err(anyhow!("SendSAS symbol not found in sas.dll"));
            }
            let send_sas: extern "system" fn(i32) = std::mem::transmute(proc);
            send_sas(0); // FALSE = called from LocalSystem service
        }
        Ok(())
    }

    fn send_inputs_with_retry(inputs: &[Input]) -> Result<()> {
        if inputs.is_empty() {
            return Ok(());
        }
        let sent = unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                size_of::<Input>() as i32,
            )
        };
        if sent == inputs.len() as u32 {
            return Ok(());
        }

        // First failure: re-attach thread to current active Windows input desktop and retry once!
        reattach_input_desktop();
        let sent_retry = unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                size_of::<Input>() as i32,
            )
        };
        if sent_retry == inputs.len() as u32 {
            return Ok(());
        }

        let err = std::io::Error::last_os_error();
        Err(anyhow!(
            "SendInput injected {}/{} events (OS error: {})",
            sent_retry,
            inputs.len(),
            err
        ))
    }

    fn make_mouse_input(dx: i32, dy: i32, mouse_data: u32, dw_flags: u32) -> Input {
        Input {
            r#type: INPUT_MOUSE,
            u: InputUnion {
                mi: MouseInput {
                    dx,
                    dy,
                    mouse_data,
                    dw_flags,
                    time: 0,
                    dw_extra_info: 0,
                },
            },
        }
    }

    fn make_key_input(vk: u16, scan: u16, dw_flags: u32) -> Input {
        Input {
            r#type: INPUT_KEYBOARD,
            u: InputUnion {
                ki: KeybdInput {
                    w_vk: vk,
                    w_scan: scan,
                    dw_flags,
                    time: 0,
                    dw_extra_info: 0,
                },
            },
        }
    }

    fn current_screen_size() -> Option<(i32, i32)> {
        let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
        if w > 0 && h > 0 {
            Some((w, h))
        } else {
            None
        }
    }

    fn move_cursor_abs(x: i32, y: i32) -> Result<()> {
        let (w, h) = current_screen_size().unwrap_or((1920, 1080));
        let max_x = (w as i64 - 1).max(1);
        let max_y = (h as i64 - 1).max(1);
        let clamped_x = (x as i64).clamp(0, max_x);
        let clamped_y = (y as i64).clamp(0, max_y);
        let norm_x = ((clamped_x * 65535 + max_x / 2) / max_x) as i32;
        let norm_y = ((clamped_y * 65535 + max_y / 2) / max_y) as i32;

        let inp = make_mouse_input(norm_x, norm_y, 0, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE);
        match send_inputs_with_retry(&[inp]) {
            Ok(()) => Ok(()),
            Err(e) => {
                // Fallback to SetCursorPos if SendInput was rejected
                if unsafe { SetCursorPos(clamped_x as i32, clamped_y as i32) } != 0 {
                    Ok(())
                } else {
                    Err(e)
                }
            }
        }
    }

    fn mouse_button_flags(button: MouseButton, press: bool) -> (u32, u32) {
        match (button, press) {
            (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
            (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
            (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
            (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
            (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
            (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
            (MouseButton::Back, true) => (MOUSEEVENTF_XDOWN, 1),
            (MouseButton::Back, false) => (MOUSEEVENTF_XUP, 1),
            (MouseButton::Forward, true) => (MOUSEEVENTF_XDOWN, 2),
            (MouseButton::Forward, false) => (MOUSEEVENTF_XUP, 2),
            (MouseButton::ScrollUp | MouseButton::ScrollDown, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
            (MouseButton::ScrollUp | MouseButton::ScrollDown, false) => (MOUSEEVENTF_MIDDLEUP, 0),
        }
    }

    fn exec_mouse_event(event: &MouseEvent) -> Result<()> {
        match event.action {
            MouseAction::Move => move_cursor_abs(event.x, event.y),
            MouseAction::Down => {
                move_cursor_abs(event.x, event.y)?;
                if let Some(btn) = event.button {
                    let (flags, data) = mouse_button_flags(btn, true);
                    send_inputs_with_retry(&[make_mouse_input(0, 0, data, flags)])?;
                }
                Ok(())
            }
            MouseAction::Up => {
                move_cursor_abs(event.x, event.y)?;
                if let Some(btn) = event.button {
                    let (flags, data) = mouse_button_flags(btn, false);
                    send_inputs_with_retry(&[make_mouse_input(0, 0, data, flags)])?;
                }
                Ok(())
            }
            MouseAction::Click => {
                move_cursor_abs(event.x, event.y)?;
                if let Some(btn) = event.button {
                    let (df, dd) = mouse_button_flags(btn, true);
                    let (uf, ud) = mouse_button_flags(btn, false);
                    send_inputs_with_retry(&[
                        make_mouse_input(0, 0, dd, df),
                        make_mouse_input(0, 0, ud, uf),
                    ])?;
                }
                Ok(())
            }
            MouseAction::DoubleClick => {
                move_cursor_abs(event.x, event.y)?;
                if let Some(btn) = event.button {
                    let (df, dd) = mouse_button_flags(btn, true);
                    let (uf, ud) = mouse_button_flags(btn, false);
                    send_inputs_with_retry(&[
                        make_mouse_input(0, 0, dd, df),
                        make_mouse_input(0, 0, ud, uf),
                        make_mouse_input(0, 0, dd, df),
                        make_mouse_input(0, 0, ud, uf),
                    ])?;
                }
                Ok(())
            }
            MouseAction::Scroll => {
                if let Some(dy) = event.delta_y {
                    // Match protocol convention: positive delta_y scrolls DOWN (-WHEEL_DELTA),
                    // negative delta_y scrolls UP (+WHEEL_DELTA), identical to Enigo Windows.
                    let wheel_data = (-dy * WHEEL_DELTA) as u32;
                    send_inputs_with_retry(&[make_mouse_input(0, 0, wheel_data, MOUSEEVENTF_WHEEL)])?;
                }
                Ok(())
            }
        }
    }

    enum ParsedKey {
        Vk(u16, bool), // (virtual_key, is_extended)
        Unicode(char),
        Ignore,
    }

    /// Map DOM `e.key` or `e.code` names to exact Windows Virtual Key codes + extended key flag.
    /// Avoids enigo 0.2's broken `MapVirtualKeyW(VkKeyScanW(ch))` bug on shifted symbols and IME keys.
    fn parse_win_key(key: &str) -> ParsedKey {
        match key {
            "Process" | "Dead" | "Unidentified" | "Compose" | "" => return ParsedKey::Ignore,
            // DOM KeyboardEvent.code physical mappings (preferred when provided)
            "ShiftLeft" => return ParsedKey::Vk(0xA0, false),
            "ShiftRight" => return ParsedKey::Vk(0xA1, false),
            "ControlLeft" => return ParsedKey::Vk(0xA2, false),
            "ControlRight" => return ParsedKey::Vk(0xA3, true),
            "AltLeft" => return ParsedKey::Vk(0xA4, false),
            "AltRight" => return ParsedKey::Vk(0xA5, true),
            "MetaLeft" | "OSLeft" => return ParsedKey::Vk(0x5B, true),
            "MetaRight" | "OSRight" => return ParsedKey::Vk(0x5C, true),
            "Minus" => return ParsedKey::Vk(0xBD, false),
            "Equal" => return ParsedKey::Vk(0xBB, false),
            "BracketLeft" => return ParsedKey::Vk(0xDB, false),
            "BracketRight" => return ParsedKey::Vk(0xDD, false),
            "Backslash" => return ParsedKey::Vk(0xDC, false),
            "Semicolon" => return ParsedKey::Vk(0xBA, false),
            "Quote" => return ParsedKey::Vk(0xDE, false),
            "Backquote" => return ParsedKey::Vk(0xC0, false),
            "Comma" => return ParsedKey::Vk(0xBC, false),
            "Period" => return ParsedKey::Vk(0xBE, false),
            "Slash" => return ParsedKey::Vk(0xBF, false),
            "NumpadEnter" => return ParsedKey::Vk(0x0D, true),
            "NumpadAdd" => return ParsedKey::Vk(0x6B, false),
            "NumpadSubtract" => return ParsedKey::Vk(0x6D, false),
            "NumpadMultiply" => return ParsedKey::Vk(0x6A, false),
            "NumpadDivide" => return ParsedKey::Vk(0x6F, true),
            "NumpadDecimal" => return ParsedKey::Vk(0x6E, false),
            _ => {}
        }

        if let Some(rest) = key.strip_prefix("Key") {
            if rest.len() == 1 {
                let b = rest.as_bytes()[0].to_ascii_uppercase();
                if b.is_ascii_uppercase() {
                    return ParsedKey::Vk(b as u16, false);
                }
            }
        }
        if let Some(rest) = key.strip_prefix("Digit") {
            if rest.len() == 1 {
                let b = rest.as_bytes()[0];
                if b.is_ascii_digit() {
                    return ParsedKey::Vk(b as u16, false);
                }
            }
        }
        if let Some(rest) = key.strip_prefix("Numpad") {
            if rest.len() == 1 {
                let b = rest.as_bytes()[0];
                if b.is_ascii_digit() {
                    return ParsedKey::Vk(0x60 + (b - b'0') as u16, false);
                }
            }
        }

        let lower = key.to_ascii_lowercase();
        match lower.as_str() {
            "enter" | "return" => ParsedKey::Vk(0x0D, false),
            "backspace" => ParsedKey::Vk(0x08, false),
            "tab" => ParsedKey::Vk(0x09, false),
            " " | "space" | "spacebar" => ParsedKey::Vk(0x20, false),
            "escape" | "esc" => ParsedKey::Vk(0x1B, false),
            "shift" => ParsedKey::Vk(0x10, false),
            "control" | "ctrl" => ParsedKey::Vk(0x11, false),
            "alt" => ParsedKey::Vk(0x12, false),
            "meta" | "win" | "os" | "command" | "cmd" => ParsedKey::Vk(0x5B, true),
            "capslock" => ParsedKey::Vk(0x14, false),
            "numlock" => ParsedKey::Vk(0x90, true),
            "scrolllock" => ParsedKey::Vk(0x91, false),
            "printscreen" => ParsedKey::Vk(0x2C, true),
            "pause" => ParsedKey::Vk(0x13, false),
            "contextmenu" => ParsedKey::Vk(0x5D, true),
            "insert" => ParsedKey::Vk(0x2D, true),
            "delete" => ParsedKey::Vk(0x2E, true),
            "home" => ParsedKey::Vk(0x24, true),
            "end" => ParsedKey::Vk(0x23, true),
            "pageup" => ParsedKey::Vk(0x21, true),
            "pagedown" => ParsedKey::Vk(0x22, true),
            "up" | "arrowup" => ParsedKey::Vk(0x26, true),
            "down" | "arrowdown" => ParsedKey::Vk(0x28, true),
            "left" | "arrowleft" => ParsedKey::Vk(0x25, true),
            "right" | "arrowright" => ParsedKey::Vk(0x27, true),
            "f1" => ParsedKey::Vk(0x70, false),
            "f2" => ParsedKey::Vk(0x71, false),
            "f3" => ParsedKey::Vk(0x72, false),
            "f4" => ParsedKey::Vk(0x73, false),
            "f5" => ParsedKey::Vk(0x74, false),
            "f6" => ParsedKey::Vk(0x75, false),
            "f7" => ParsedKey::Vk(0x76, false),
            "f8" => ParsedKey::Vk(0x77, false),
            "f9" => ParsedKey::Vk(0x78, false),
            "f10" => ParsedKey::Vk(0x79, false),
            "f11" => ParsedKey::Vk(0x7A, false),
            "f12" => ParsedKey::Vk(0x7B, false),
            _ => {
                let mut chars = key.chars();
                if let (Some(ch), None) = (chars.next(), chars.next()) {
                    match ch {
                        'a'..='z' => ParsedKey::Vk((ch.to_ascii_uppercase()) as u16, false),
                        'A'..='Z' => ParsedKey::Vk(ch as u16, false),
                        '0'..='9' => ParsedKey::Vk(ch as u16, false),
                        // Shifted number-row symbols (Shift is already held by controller)
                        ')' => ParsedKey::Vk(0x30, false),
                        '!' => ParsedKey::Vk(0x31, false),
                        '@' => ParsedKey::Vk(0x32, false),
                        '#' => ParsedKey::Vk(0x33, false),
                        '$' => ParsedKey::Vk(0x34, false),
                        '%' => ParsedKey::Vk(0x35, false),
                        '^' => ParsedKey::Vk(0x36, false),
                        '&' => ParsedKey::Vk(0x37, false),
                        '*' => ParsedKey::Vk(0x38, false),
                        '(' => ParsedKey::Vk(0x39, false),
                        // US-ANSI OEM punctuation
                        '-' | '_' => ParsedKey::Vk(0xBD, false),
                        '=' | '+' => ParsedKey::Vk(0xBB, false),
                        '[' | '{' => ParsedKey::Vk(0xDB, false),
                        ']' | '}' => ParsedKey::Vk(0xDD, false),
                        '\\' | '|' => ParsedKey::Vk(0xDC, false),
                        ';' | ':' => ParsedKey::Vk(0xBA, false),
                        '\'' | '"' => ParsedKey::Vk(0xDE, false),
                        '`' | '~' => ParsedKey::Vk(0xC0, false),
                        ',' | '<' => ParsedKey::Vk(0xBC, false),
                        '.' | '>' => ParsedKey::Vk(0xBE, false),
                        '/' | '?' => ParsedKey::Vk(0xBF, false),
                        other => ParsedKey::Unicode(other),
                    }
                } else {
                    ParsedKey::Ignore
                }
            }
        }
    }

    fn exec_key_event(event: &KeyEvent) -> Result<()> {
        match parse_win_key(&event.key) {
            ParsedKey::Ignore => Ok(()),
            ParsedKey::Vk(vk, is_ext) => {
                // Always populate wScan alongside wVk so games, UWP, Electron, and RDP accept input
                let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) as u16 };
                let base_flags = if is_ext { KEYEVENTF_EXTENDEDKEY } else { 0 };
                match event.action {
                    KeyAction::Down => {
                        send_inputs_with_retry(&[make_key_input(vk, scan, base_flags)])
                    }
                    KeyAction::Up => send_inputs_with_retry(&[make_key_input(
                        vk,
                        scan,
                        base_flags | KEYEVENTF_KEYUP,
                    )]),
                    KeyAction::Click => send_inputs_with_retry(&[
                        make_key_input(vk, scan, base_flags),
                        make_key_input(vk, scan, base_flags | KEYEVENTF_KEYUP),
                    ]),
                }
            }
            ParsedKey::Unicode(ch) => {
                let mut utf16 = [0u16; 2];
                let units = ch.encode_utf16(&mut utf16);
                let mut inputs = Vec::with_capacity(units.len() * 2);
                match event.action {
                    KeyAction::Down => {
                        for &u in units.iter() {
                            inputs.push(make_key_input(0, u, KEYEVENTF_UNICODE));
                        }
                    }
                    KeyAction::Up => {
                        for &u in units.iter() {
                            inputs.push(make_key_input(0, u, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
                        }
                    }
                    KeyAction::Click => {
                        for &u in units.iter() {
                            inputs.push(make_key_input(0, u, KEYEVENTF_UNICODE));
                            inputs.push(make_key_input(0, u, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
                        }
                    }
                }
                send_inputs_with_retry(&inputs)
            }
        }
    }

    fn exec_type_text(text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let mut inputs = Vec::with_capacity(text.len() * 2);
        let mut utf16 = [0u16; 2];
        for ch in text.chars() {
            if ch == '\n' {
                let scan = unsafe { MapVirtualKeyW(0x0D, MAPVK_VK_TO_VSC) as u16 };
                inputs.push(make_key_input(0x0D, scan, 0));
                inputs.push(make_key_input(0x0D, scan, KEYEVENTF_KEYUP));
            } else if ch == '\r' || ch == '\0' {
                continue;
            } else {
                let units = ch.encode_utf16(&mut utf16);
                for &u in units.iter() {
                    inputs.push(make_key_input(0, u, KEYEVENTF_UNICODE));
                    inputs.push(make_key_input(0, u, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
                }
            }
        }
        send_inputs_with_retry(&inputs)
    }

    enum InputJob {
        Mouse(MouseEvent, Option<mpsc::Sender<Result<()>>>),
        Key(KeyEvent, mpsc::Sender<Result<()>>),
        Text(String, mpsc::Sender<Result<()>>),
    }

    /// Dedicated Windows OS-thread input simulator.
    /// Guarantees SetThreadDesktop(OpenInputDesktop) can safely re-attach after UAC/lock/RDP changes,
    /// preserves strict FIFO event order, and never blocks Tokio worker threads.
    pub struct InputSimulator {
        tx: SyncSender<InputJob>,
    }

    impl InputSimulator {
        pub fn new() -> Result<Self> {
            let (tx, rx) = mpsc::sync_channel::<InputJob>(256);
            thread::Builder::new()
                .name("openremote-input".to_string())
                .spawn(move || {
                    reattach_input_desktop();
                    let mut last_desk_check = std::time::Instant::now();
                    while let Ok(job) = rx.recv() {
                        // Proactively follow Default <-> Winlogon (Lock Screen / UAC) every 120ms
                        if last_desk_check.elapsed() >= std::time::Duration::from_millis(120) {
                            reattach_input_desktop();
                            last_desk_check = std::time::Instant::now();
                        }
                        match job {
                            InputJob::Mouse(ev, reply) => {
                                let res = exec_mouse_event(&ev);
                                if let Some(ch) = reply {
                                    let _ = ch.send(res);
                                } else if let Err(e) = res {
                                    log::warn!("Mouse move injection error: {}", e);
                                }
                            }
                            InputJob::Key(ev, reply) => {
                                let _ = reply.send(exec_key_event(&ev));
                            }
                            InputJob::Text(txt, reply) => {
                                let _ = reply.send(exec_type_text(&txt));
                            }
                        }
                    }
                })
                .map_err(|e| anyhow!("Failed to spawn openremote-input thread: {}", e))?;

            Ok(Self { tx })
        }

        /// Query live primary display dimensions (never cached stale across resolution switches)
        pub fn screen_size(&self) -> Option<(i32, i32)> {
            current_screen_size()
        }

        pub fn handle_mouse_event(&self, event: &MouseEvent) -> Result<()> {
            if event.action == MouseAction::Move {
                // Non-blocking try_send for high-frequency mouse moves so queue never backs up;
                // click/scroll/key events still wait for deterministic error feedback.
                match self.tx.try_send(InputJob::Mouse(event.clone(), None)) {
                    Ok(()) => Ok(()),
                    Err(mpsc::TrySendError::Full(_)) => Ok(()),
                    Err(mpsc::TrySendError::Disconnected(_)) => {
                        Err(anyhow!("Input thread disconnected"))
                    }
                }
            } else {
                let (rtx, rrx) = mpsc::channel();
                self.tx
                    .send(InputJob::Mouse(event.clone(), Some(rtx)))
                    .map_err(|_| anyhow!("Input thread disconnected"))?;
                rrx.recv().map_err(|_| anyhow!("Input thread crashed"))?
            }
        }

        pub fn handle_key_event(&self, event: &KeyEvent) -> Result<()> {
            let (rtx, rrx) = mpsc::channel();
            self.tx
                .send(InputJob::Key(event.clone(), rtx))
                .map_err(|_| anyhow!("Input thread disconnected"))?;
            rrx.recv().map_err(|_| anyhow!("Input thread crashed"))?
        }

        pub fn type_text(&self, text: &str) -> Result<()> {
            let (rtx, rrx) = mpsc::channel();
            self.tx
                .send(InputJob::Text(text.to_string(), rtx))
                .map_err(|_| anyhow!("Input thread disconnected"))?;
            rrx.recv().map_err(|_| anyhow!("Input thread crashed"))?
        }
    }

    /// Attempt to change the Windows primary monitor's physical resolution dynamically.
    /// Searches supported modes via EnumDisplaySettingsW, validates with CDS_TEST, and applies via ChangeDisplaySettingsW.
    pub fn change_display_resolution(target_w: u32, target_h: u32) -> DisplayChangeResult {
        if target_w < 640 || target_h < 480 {
            return DisplayChangeResult {
                applied_os: false,
                width: target_w,
                height: target_h,
                refresh_hz: 0,
                message: "目标分辨率过小，已切换为推流缩放模式".to_string(),
            };
        }

        unsafe {
            let mut best: Option<DevModeW> = None;
            let mut mode_idx: u32 = 0;

            loop {
                let mut dm: DevModeW = zeroed();
                dm.dm_size = size_of::<DevModeW>() as u16;
                if EnumDisplaySettingsW(ptr::null(), mode_idx, &mut dm) == 0 {
                    break;
                }
                mode_idx += 1;

                if dm.dm_pels_width == target_w && dm.dm_pels_height == target_h {
                    let is_better = match best {
                        None => true,
                        Some(cur) => {
                            (dm.dm_bits_per_pel > cur.dm_bits_per_pel)
                                || (dm.dm_bits_per_pel == cur.dm_bits_per_pel
                                    && dm.dm_display_frequency > cur.dm_display_frequency)
                        }
                    };
                    if is_better {
                        best = Some(dm);
                    }
                }
            }

            let mut mode = match best {
                Some(m) => m,
                None => {
                    return DisplayChangeResult {
                        applied_os: false,
                        width: target_w,
                        height: target_h,
                        refresh_hz: 0,
                        message: format!(
                            "远程显示器不支持原生 {}x{}，已自动切换画面推流分辨率",
                            target_w, target_h
                        ),
                    };
                }
            };

            mode.dm_fields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_BITSPERPEL | DM_DISPLAYFREQUENCY;
            let test_rc = ChangeDisplaySettingsW(&mut mode, CDS_TEST);
            if test_rc != DISP_CHANGE_SUCCESSFUL {
                return DisplayChangeResult {
                    applied_os: false,
                    width: target_w,
                    height: target_h,
                    refresh_hz: mode.dm_display_frequency,
                    message: format!(
                        "远程系统拒绝切换原生 {}x{}（可能处于 RDP 会话，错误码 {}），已改为推流分辨率缩放",
                        target_w, target_h, test_rc
                    ),
                };
            }

            let apply_rc = ChangeDisplaySettingsW(&mut mode, 0);
            if apply_rc == DISP_CHANGE_SUCCESSFUL {
                DisplayChangeResult {
                    applied_os: true,
                    width: mode.dm_pels_width,
                    height: mode.dm_pels_height,
                    refresh_hz: mode.dm_display_frequency,
                    message: format!(
                        "远程桌面分辨率已切换为 {}x{} ({}Hz)",
                        mode.dm_pels_width, mode.dm_pels_height, mode.dm_display_frequency
                    ),
                }
            } else {
                DisplayChangeResult {
                    applied_os: false,
                    width: target_w,
                    height: target_h,
                    refresh_hz: mode.dm_display_frequency,
                    message: format!(
                        "远程系统分辨率切换失败（错误码 {}），已改为推流分辨率缩放",
                        apply_rc
                    ),
                }
            }
        }
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcessId() -> u32;
        fn ProcessIdToSessionId(dw_process_id: u32, p_session_id: *mut u32) -> i32;
    }

    #[link(name = "wtsapi32")]
    extern "system" {
        fn WTSQuerySessionInformationW(
            h_server: isize,
            session_id: u32,
            wts_info_class: u32,
            pp_buffer: *mut *mut u8,
            p_bytes_returned: *mut u32,
        ) -> i32;
        fn WTSFreeMemory(p_memory: *mut u8);
    }

    pub fn rdp_session_status() -> super::RdpSessionStatus {
        const WTS_CURRENT_SERVER_HANDLE: isize = 0;
        const WTS_CURRENT_SESSION: u32 = u32::MAX;
        const WTS_CONNECT_STATE: u32 = 8;
        const WTS_CLIENT_PROTOCOL_TYPE: u32 = 16;

        let mut session_id = 0u32;
        let mut is_rdp = false;
        let mut is_disconnected = false;

        unsafe {
            let pid = GetCurrentProcessId();
            let _ = ProcessIdToSessionId(pid, &mut session_id);

            let mut buf: *mut u8 = ptr::null_mut();
            let mut bytes: u32 = 0;
            if WTSQuerySessionInformationW(
                WTS_CURRENT_SERVER_HANDLE,
                WTS_CURRENT_SESSION,
                WTS_CLIENT_PROTOCOL_TYPE,
                &mut buf,
                &mut bytes,
            ) != 0
                && !buf.is_null()
                && bytes >= 2
            {
                let proto = *(buf as *const u16);
                is_rdp = proto == 2; // 2 = RDP
                WTSFreeMemory(buf);
            }

            let mut buf2: *mut u8 = ptr::null_mut();
            let mut bytes2: u32 = 0;
            if WTSQuerySessionInformationW(
                WTS_CURRENT_SERVER_HANDLE,
                WTS_CURRENT_SESSION,
                WTS_CONNECT_STATE,
                &mut buf2,
                &mut bytes2,
            ) != 0
                && !buf2.is_null()
                && bytes2 >= 4
            {
                let state = *(buf2 as *const i32);
                is_disconnected = state == 4; // 4 = WTSDisconnected
                WTSFreeMemory(buf2);
            }
        }

        super::RdpSessionStatus {
            session_id,
            is_rdp,
            is_disconnected,
        }
    }

    /// Hand off the current RDP/disconnected session to the physical local Console (unlocked)
    /// via `tscon.exe <session_id> /dest:console`, immediately resolving BitBlt & SendInput os error 5!
    pub fn detach_rdp_to_console() -> Result<String> {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        let st = rdp_session_status();
        if !st.is_rdp && !st.is_disconnected {
            return Ok(format!(
                "当前会话 (#{}) 已位于本地物理桌面 (Console)，无需切换",
                st.session_id
            ));
        }

        let windir = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
        let tscon_path = format!("{}\\System32\\tscon.exe", windir);
        let output = std::process::Command::new(&tscon_path)
            .args([&st.session_id.to_string(), "/dest:console"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| anyhow!("无法启动 tscon.exe: {}", e))?;

        if output.status.success() {
            Ok(format!(
                "已成功将会话 #{} 从 Windows RDP 切回本地桌面 (Console)，画面与键鼠将在 1 秒内恢复！",
                st.session_id
            ))
        } else {
            let code = output.status.code().unwrap_or(-1);
            Err(anyhow!(
                "tscon 切回本地桌面失败（退出码 {}），请在被控端管理员终端手动执行: tscon {} /dest:console",
                code,
                st.session_id
            ))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn verify_win32_struct_sizes_and_key_mapping() {
            #[cfg(target_pointer_width = "64")]
            assert_eq!(size_of::<Input>(), 40, "Win64 INPUT struct must be 40 bytes");
            assert_eq!(size_of::<DevModeW>(), 220, "Win32 DEVMODEW struct must be 220 bytes");

            let st = rdp_session_status();
            println!(
                "Current Windows session status: id={}, is_rdp={}, is_disconnected={}",
                st.session_id, st.is_rdp, st.is_disconnected
            );

            // Verify previously broken shifted symbols & special keys now map to exact VKs
            assert!(matches!(parse_win_key("!"), ParsedKey::Vk(0x31, false)));
            assert!(matches!(parse_win_key(":"), ParsedKey::Vk(0xBA, false)));
            assert!(matches!(parse_win_key("?"), ParsedKey::Vk(0xBF, false)));
            assert!(matches!(parse_win_key("KeyA"), ParsedKey::Vk(0x41, false)));
            assert!(matches!(parse_win_key("Meta"), ParsedKey::Vk(0x5B, true)));
            assert!(matches!(parse_win_key("ArrowUp"), ParsedKey::Vk(0x26, true)));
            assert!(matches!(parse_win_key("Process"), ParsedKey::Ignore));
            assert!(matches!(parse_win_key("Dead"), ParsedKey::Ignore));
            assert!(matches!(parse_win_key("中"), ParsedKey::Unicode('中')));
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod fallback_input {
    use super::{DisplayChangeResult, KeyAction, KeyEvent, MouseAction, MouseButton, MouseEvent};
    use anyhow::Result;
    use enigo::{Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
    use std::sync::Mutex;

    pub struct InputSimulator {
        enigo: Mutex<Enigo>,
    }

    impl InputSimulator {
        pub fn new() -> Result<Self> {
            let settings = Settings::default();
            let enigo = Enigo::new(&settings)
                .map_err(|e| anyhow::anyhow!("Failed to initialize Enigo: {:?}", e))?;
            Ok(Self {
                enigo: Mutex::new(enigo),
            })
        }

        pub fn screen_size(&self) -> Option<(i32, i32)> {
            let enigo = self.enigo.lock().unwrap();
            enigo.main_display().ok()
        }

        pub fn handle_mouse_event(&self, event: &MouseEvent) -> Result<()> {
            let mut enigo = self.enigo.lock().unwrap();
            match event.action {
                MouseAction::Move => {
                    enigo
                        .move_mouse(event.x, event.y, Coordinate::Abs)
                        .map_err(|e| anyhow::anyhow!("{:?}", e))?;
                }
                MouseAction::Down => {
                    enigo
                        .move_mouse(event.x, event.y, Coordinate::Abs)
                        .map_err(|e| anyhow::anyhow!("{:?}", e))?;
                    if let Some(btn) = event.button {
                        enigo
                            .button(map_mouse_button(btn), Direction::Press)
                            .map_err(|e| anyhow::anyhow!("{:?}", e))?;
                    }
                }
                MouseAction::Up => {
                    enigo
                        .move_mouse(event.x, event.y, Coordinate::Abs)
                        .map_err(|e| anyhow::anyhow!("{:?}", e))?;
                    if let Some(btn) = event.button {
                        enigo
                            .button(map_mouse_button(btn), Direction::Release)
                            .map_err(|e| anyhow::anyhow!("{:?}", e))?;
                    }
                }
                MouseAction::Click => {
                    enigo
                        .move_mouse(event.x, event.y, Coordinate::Abs)
                        .map_err(|e| anyhow::anyhow!("{:?}", e))?;
                    if let Some(btn) = event.button {
                        enigo
                            .button(map_mouse_button(btn), Direction::Click)
                            .map_err(|e| anyhow::anyhow!("{:?}", e))?;
                    }
                }
                MouseAction::DoubleClick => {
                    enigo
                        .move_mouse(event.x, event.y, Coordinate::Abs)
                        .map_err(|e| anyhow::anyhow!("{:?}", e))?;
                    if let Some(btn) = event.button {
                        enigo
                            .button(map_mouse_button(btn), Direction::Click)
                            .map_err(|e| anyhow::anyhow!("{:?}", e))?;
                        enigo
                            .button(map_mouse_button(btn), Direction::Click)
                            .map_err(|e| anyhow::anyhow!("{:?}", e))?;
                    }
                }
                MouseAction::Scroll => {
                    if let Some(dy) = event.delta_y {
                        enigo
                            .scroll(dy, enigo::Axis::Vertical)
                            .map_err(|e| anyhow::anyhow!("{:?}", e))?;
                    }
                }
            }
            Ok(())
        }

        pub fn handle_key_event(&self, event: &KeyEvent) -> Result<()> {
            let mut enigo = self.enigo.lock().unwrap();
            let key = parse_key(&event.key);
            let dir = match event.action {
                KeyAction::Down => Direction::Press,
                KeyAction::Up => Direction::Release,
                KeyAction::Click => Direction::Click,
            };
            enigo
                .key(key, dir)
                .map_err(|e| anyhow::anyhow!("{:?}", e))?;
            Ok(())
        }

        pub fn type_text(&self, text: &str) -> Result<()> {
            let mut enigo = self.enigo.lock().unwrap();
            enigo.text(text).map_err(|e| anyhow::anyhow!("{:?}", e))?;
            Ok(())
        }
    }

    pub fn change_display_resolution(target_w: u32, target_h: u32) -> DisplayChangeResult {
        DisplayChangeResult {
            applied_os: false,
            width: target_w,
            height: target_h,
            refresh_hz: 0,
            message: "已切换画面推流分辨率".to_string(),
        }
    }

    pub fn rdp_session_status() -> super::RdpSessionStatus {
        super::RdpSessionStatus {
            session_id: 0,
            is_rdp: false,
            is_disconnected: false,
        }
    }

    pub fn detach_rdp_to_console() -> Result<String> {
        Ok("当前系统非 Windows RDP 会话".to_string())
    }

    pub fn send_sas_ctrl_alt_del() -> Result<()> {
        Err(anyhow::anyhow!("SendSAS is Windows-only"))
    }

    fn map_mouse_button(button: MouseButton) -> Button {
        match button {
            MouseButton::Left => Button::Left,
            MouseButton::Middle => Button::Middle,
            MouseButton::Right => Button::Right,
            MouseButton::Back => Button::Back,
            MouseButton::Forward => Button::Forward,
            MouseButton::ScrollUp | MouseButton::ScrollDown => Button::Middle,
        }
    }

    fn parse_key(key: &str) -> Key {
        match key.to_lowercase().as_str() {
            "enter" | "return" => Key::Return,
            "backspace" => Key::Backspace,
            "tab" => Key::Tab,
            " " | "space" | "spacebar" => Key::Space,
            "escape" | "esc" => Key::Escape,
            "shift" => Key::Shift,
            "control" | "ctrl" => Key::Control,
            "alt" => Key::Alt,
            "meta" | "win" | "command" | "cmd" => Key::Meta,
            "capslock" => Key::CapsLock,
            "up" | "arrowup" => Key::UpArrow,
            "down" | "arrowdown" => Key::DownArrow,
            "left" | "arrowleft" => Key::LeftArrow,
            "right" | "arrowright" => Key::RightArrow,
            "home" => Key::Home,
            "end" => Key::End,
            "pageup" => Key::PageUp,
            "pagedown" => Key::PageDown,
            "delete" => Key::Delete,
            "f1" => Key::F1,
            "f2" => Key::F2,
            "f3" => Key::F3,
            "f4" => Key::F4,
            "f5" => Key::F5,
            "f6" => Key::F6,
            "f7" => Key::F7,
            "f8" => Key::F8,
            "f9" => Key::F9,
            "f10" => Key::F10,
            "f11" => Key::F11,
            "f12" => Key::F12,
            _ => Key::Unicode(key.chars().next().unwrap_or(' ')),
        }
    }
}

#[cfg(target_os = "windows")]
pub use win_input::{
    change_display_resolution, detach_rdp_to_console, rdp_session_status, send_sas_ctrl_alt_del,
    InputSimulator,
};

#[cfg(not(target_os = "windows"))]
pub use fallback_input::{
    change_display_resolution, detach_rdp_to_console, rdp_session_status, send_sas_ctrl_alt_del,
    InputSimulator,
};

/// Detect the current remote host OS cursor shape and map it to a standard CSS cursor keyword.
/// Runs in <1 microsecond using Win32 GetCursorInfo compared against cached system cursor handles.
pub fn current_cursor_css() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        use std::sync::OnceLock;

        #[repr(C)]
        struct Point {
            x: i32,
            y: i32,
        }

        #[repr(C)]
        struct CursorInfo {
            cb_size: u32,
            flags: u32,
            h_cursor: isize,
            pt_screen_pos: Point,
        }

        #[link(name = "user32")]
        extern "system" {
            fn GetCursorInfo(pci: *mut CursorInfo) -> i32;
            fn LoadCursorW(hInstance: isize, lpCursorName: usize) -> isize;
        }

        struct SystemCursors {
            arrow: isize,
            ibeam: isize,
            wait: isize,
            cross: isize,
            uparrow: isize,
            sizenwse: isize,
            sizenesw: isize,
            sizewe: isize,
            sizens: isize,
            sizeall: isize,
            no: isize,
            hand: isize,
            appstarting: isize,
            help: isize,
        }

        static CURSORS: OnceLock<SystemCursors> = OnceLock::new();
        let sys = CURSORS.get_or_init(|| unsafe {
            SystemCursors {
                arrow: LoadCursorW(0, 32512),       // IDC_ARROW
                ibeam: LoadCursorW(0, 32513),       // IDC_IBEAM
                wait: LoadCursorW(0, 32514),        // IDC_WAIT
                cross: LoadCursorW(0, 32515),       // IDC_CROSS
                uparrow: LoadCursorW(0, 32516),     // IDC_UPARROW
                sizenwse: LoadCursorW(0, 32642),    // IDC_SIZENWSE
                sizenesw: LoadCursorW(0, 32643),    // IDC_SIZENESW
                sizewe: LoadCursorW(0, 32644),      // IDC_SIZEWE
                sizens: LoadCursorW(0, 32645),      // IDC_SIZENS
                sizeall: LoadCursorW(0, 32646),     // IDC_SIZEALL
                no: LoadCursorW(0, 32648),          // IDC_NO
                hand: LoadCursorW(0, 32649),        // IDC_HAND
                appstarting: LoadCursorW(0, 32650), // IDC_APPSTARTING
                help: LoadCursorW(0, 32651),        // IDC_HELP
            }
        });

        unsafe {
            let mut ci = CursorInfo {
                cb_size: std::mem::size_of::<CursorInfo>() as u32,
                flags: 0,
                h_cursor: 0,
                pt_screen_pos: Point { x: 0, y: 0 },
            };
            if GetCursorInfo(&mut ci) == 0 || ci.h_cursor == 0 {
                return "default";
            }
            // Note: Never return "none" when CURSOR_SHOWING is 0 (e.g. Windows CURSOR_SUPPRESSED
            // on headless/RDP/no-local-mouse hosts), otherwise the controller's cursor disappears!
            // Always inspect ci.h_cursor so I-beam / pointer / resize shapes still sync cleanly.
            let h = ci.h_cursor;
            if h == sys.arrow {
                "default"
            } else if h == sys.ibeam {
                "text"
            } else if h == sys.hand {
                "pointer"
            } else if h == sys.sizewe {
                "ew-resize"
            } else if h == sys.sizens {
                "ns-resize"
            } else if h == sys.sizenwse {
                "nwse-resize"
            } else if h == sys.sizenesw {
                "nesw-resize"
            } else if h == sys.sizeall {
                "move"
            } else if h == sys.wait {
                "wait"
            } else if h == sys.appstarting {
                "progress"
            } else if h == sys.no {
                "not-allowed"
            } else if h == sys.cross {
                "crosshair"
            } else if h == sys.help {
                "help"
            } else if h == sys.uparrow {
                "n-resize"
            } else {
                "default"
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        "default"
    }
}
