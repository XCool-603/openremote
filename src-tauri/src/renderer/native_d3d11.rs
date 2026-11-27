#[cfg(target_os = "windows")]
mod win_d3d11 {
    use crate::codec::h264::{create_d3d11_video_device, DecodedNv12Frame, H264Decoder};
    use crate::protocol::{KeyAction, Message, MouseAction, MouseButton};
    use anyhow::{anyhow, Result};
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};
    use std::ptr;
    use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex as StdMutex, Once};
    use std::time::{Duration, Instant};
    use tauri::{Emitter, Manager};
    use windows::core::{w, Interface, PCWSTR};
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
    use windows::Win32::Graphics::Direct3D::{ID3DBlob, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP, D3D11_SRV_DIMENSION_TEXTURE2D};
    use windows::Win32::Graphics::Direct3D11::{
        ID3D11Buffer, ID3D11Device, ID3D11PixelShader,
        ID3D11RenderTargetView, ID3D11SamplerState, ID3D11ShaderResourceView, ID3D11Texture2D,
        ID3D11VertexShader, D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_SHADER_RESOURCE,
        D3D11_BUFFER_DESC, D3D11_CPU_ACCESS_WRITE, D3D11_FILTER_MIN_MAG_MIP_LINEAR,
        D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_WRITE_DISCARD, D3D11_SAMPLER_DESC,
        D3D11_SHADER_RESOURCE_VIEW_DESC, D3D11_SHADER_RESOURCE_VIEW_DESC_0, D3D11_TEX2D_SRV,
        D3D11_TEXTURE2D_DESC, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_USAGE_DEFAULT,
        D3D11_USAGE_DYNAMIC, D3D11_VIEWPORT,
    };
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12,
        DXGI_FORMAT_R8G8_UNORM, DXGI_FORMAT_R8_UNORM, DXGI_SAMPLE_DESC,
    };
    use windows::Win32::Graphics::Dxgi::{
        IDXGIDevice, IDXGIFactory2, IDXGISwapChain1, DXGI_MWA_NO_ALT_ENTER, DXGI_PRESENT,
        DXGI_PRESENT_DO_NOT_WAIT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1,
        DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
    };
    use windows::Win32::Graphics::Gdi::ValidateRect;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, GetClientRect, GetWindowLongPtrW, LoadCursorW,
        RegisterClassExW, SetCursor, SetWindowLongPtrW, SetWindowPos, ShowWindow, GWLP_USERDATA,
        HWND_TOP, IDC_ARROW, IDC_HAND, IDC_IBEAM, IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS,
        IDC_SIZENWSE, IDC_SIZEWE, IDC_WAIT, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE,
        SWP_NOSIZE, SWP_SHOWWINDOW, SW_HIDE, SW_SHOWNOACTIVATE, WM_ERASEBKGND, WM_KEYDOWN,
        WM_KEYUP, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
        WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR,
        WM_SIZE, WM_SYSKEYDOWN, WM_SYSKEYUP, WNDCLASSEXW, WS_CHILD, WS_CLIPSIBLINGS,
    };

    const CLASS_NAME: PCWSTR = w!("OpenRemoteD3D11Viewport");
    static REGISTER_CLASS_ONCE: Once = Once::new();
    const WM_CHAR_MSG: u32 = 0x0102;
    const WM_IME_CHAR_MSG: u32 = 0x0286;
    const WM_GETDLGCODE_MSG: u32 = 0x0087;
    const DLGC_WANTALLKEYS_FLAGS: isize = 0x0007; // DLGC_WANTARROWS | DLGC_WANTTAB | DLGC_WANTALLKEYS

    #[link(name = "imm32")]
    extern "system" {
        fn ImmAssociateContext(hwnd: isize, himc: isize) -> isize;
    }

    const VS_HLSL: &str = r#"
cbuffer CropCB : register(b0) {
    float2 uvScale;
    float2 pad;
};
struct VSOut {
    float4 pos : SV_Position;
    float2 uv  : TEXCOORD0;
};
VSOut main(uint id : SV_VertexID) {
    VSOut o;
    float2 grid = float2((id & 1) ? 1.0 : 0.0, (id & 2) ? 1.0 : 0.0);
    o.pos = float4(grid.x * 2.0 - 1.0, 1.0 - grid.y * 2.0, 0.0, 1.0);
    o.uv  = grid * uvScale;
    return o;
}
"#;

    const PS_HLSL: &str = r#"
Texture2D<float> texY   : register(t0);
Texture2D<float2> texUV : register(t1);
SamplerState samp       : register(s0);
float4 main(float4 pos : SV_Position, float2 uv : TEXCOORD0) : SV_Target {
    float y = texY.Sample(samp, uv);
    float2 uvVal = texUV.Sample(samp, uv);
    float yy = 1.16438356 * max(0.0, y - 0.0625);
    float u = uvVal.x - 0.5;
    float v = uvVal.y - 0.5;
    float r = saturate(yy + 1.59602678 * v);
    float g = saturate(yy - 0.39176229 * u - 0.81296765 * v);
    float b = saturate(yy + 2.01723214 * u);
    return float4(r, g, b, 1.0);
}
"#;

    #[repr(C)]
    #[derive(Copy, Clone)]
    struct CropConstants {
        uv_scale: [f32; 2],
        pad: [f32; 2],
    }

    pub struct H264Packet {
        pub data: Vec<u8>,
        pub width: u16,
        pub height: u16,
        pub is_keyframe: bool,
    }

    struct SharedViewportState {
        child_hwnd: AtomicU64,
        client_w: AtomicU32,
        client_h: AtomicU32,
        video_w: AtomicU16,
        video_h: AtomicU16,
        visible: AtomicBool,
        has_first_frame: AtomicBool,
        input_locked: AtomicBool,
        is_relay: AtomicBool,
        cursor_kind: AtomicU8, // 0=arrow,1=ibeam,2=hand,3=we,4=ns,5=nesw,6=nwse,7=all,8=wait
        modifier_mask: AtomicU8, // bitmask of held modifiers on this viewport HWND
        last_move_ms: AtomicI64,
        pending_move: AtomicU64,
        move_notify: Arc<tokio::sync::Notify>,
        input_tx: StdMutex<Option<tokio::sync::mpsc::UnboundedSender<Message>>>,
    }

    impl SharedViewportState {
        fn new() -> Self {
            Self {
                child_hwnd: AtomicU64::new(0),
                client_w: AtomicU32::new(1280),
                client_h: AtomicU32::new(720),
                video_w: AtomicU16::new(1920),
                video_h: AtomicU16::new(1080),
                visible: AtomicBool::new(false),
                has_first_frame: AtomicBool::new(false),
                input_locked: AtomicBool::new(true),
                is_relay: AtomicBool::new(false),
                cursor_kind: AtomicU8::new(0),
                modifier_mask: AtomicU8::new(0),
                last_move_ms: AtomicI64::new(0),
                pending_move: AtomicU64::new(0),
                move_notify: Arc::new(tokio::sync::Notify::new()),
                input_tx: StdMutex::new(None),
            }
        }

        fn push_coalesced_move(&self, x: i32, y: i32) {
            let packed = (1u64 << 63)
                | (((x.max(0) as u32 as u64) & 0x7FFF_FFFF) << 31)
                | ((y.max(0) as u32 as u64) & 0x7FFF_FFFF);
            self.pending_move.store(packed, Ordering::Release);
            self.move_notify.notify_one();
        }

        fn take_coalesced_move(&self) -> Option<(i32, i32)> {
            let packed = self.pending_move.swap(0, Ordering::AcqRel);
            if (packed & (1u64 << 63)) == 0 {
                return None;
            }
            let x = ((packed >> 31) & 0x7FFF_FFFF) as i32;
            let y = (packed & 0x7FFF_FFFF) as i32;
            Some((x, y))
        }

        fn send_input(&self, msg: Message) {
            if let Ok(guard) = self.input_tx.lock() {
                if let Some(ref tx) = *guard {
                    let _ = tx.send(msg);
                }
            }
        }

        fn map_client_to_video(&self, mx: i32, my: i32) -> (i32, i32) {
            let cw = self.client_w.load(Ordering::Relaxed).max(1) as f64;
            let ch = self.client_h.load(Ordering::Relaxed).max(1) as f64;
            let vw = self.video_w.load(Ordering::Relaxed).max(1) as f64;
            let vh = self.video_h.load(Ordering::Relaxed).max(1) as f64;

            let rect_aspect = cw / ch;
            let vid_aspect = vw / vh;

            let (draw_w, draw_h, off_x, off_y) = if rect_aspect > vid_aspect {
                let dw = ch * vid_aspect;
                (dw, ch, (cw - dw) * 0.5, 0.0)
            } else {
                let dh = cw / vid_aspect;
                (cw, dh, 0.0, (ch - dh) * 0.5)
            };

            let rel_x = ((mx as f64) - off_x).clamp(0.0, draw_w);
            let rel_y = ((my as f64) - off_y).clamp(0.0, draw_h);

            let rx = ((rel_x / draw_w.max(1.0)) * vw).round() as i32;
            let ry = ((rel_y / draw_h.max(1.0)) * vh).round() as i32;
            (rx.clamp(0, (vw as i32) - 1), ry.clamp(0, (vh as i32) - 1))
        }
    }

    fn css_cursor_to_kind(css: &str) -> u8 {
        match css {
            "text" => 1,
            "pointer" => 2,
            "ew-resize" | "col-resize" => 3,
            "ns-resize" | "row-resize" => 4,
            "nesw-resize" => 5,
            "nwse-resize" => 6,
            "move" | "all-scroll" => 7,
            "wait" | "progress" => 8,
            _ => 0,
        }
    }

    fn modifier_bit(key: &str) -> u8 {
        match key {
            "ShiftLeft" | "Shift" => 1 << 0,
            "ShiftRight" => 1 << 1,
            "ControlLeft" | "Control" => 1 << 2,
            "ControlRight" => 1 << 3,
            "AltLeft" | "Alt" => 1 << 4,
            "AltRight" => 1 << 5,
            "MetaLeft" | "Meta" => 1 << 6,
            "MetaRight" => 1 << 7,
            _ => 0,
        }
    }

    fn vk_to_key_string(vk: u16, lparam: isize) -> Option<String> {
        let extended = (lparam & (1 << 24)) != 0;
        let scancode = ((lparam >> 16) & 0xFF) as u16;
        let s = match vk {
            0x08 => "Backspace",
            0x09 => "Tab",
            0x0D => if extended { "NumpadEnter" } else { "Enter" },
            0x10 => if scancode == 0x36 { "ShiftRight" } else { "ShiftLeft" },
            0xA0 => "ShiftLeft",
            0xA1 => "ShiftRight",
            0x11 => if extended { "ControlRight" } else { "ControlLeft" },
            0xA2 => "ControlLeft",
            0xA3 => "ControlRight",
            0x12 => if extended { "AltRight" } else { "AltLeft" },
            0xA4 => "AltLeft",
            0xA5 => "AltRight",
            0x13 => "Pause",
            0x14 => "CapsLock",
            0x1B => "Escape",
            0x20 => " ",
            0x21 => "PageUp",
            0x22 => "PageDown",
            0x23 => "End",
            0x24 => "Home",
            0x25 => "ArrowLeft",
            0x26 => "ArrowUp",
            0x27 => "ArrowRight",
            0x28 => "ArrowDown",
            0x2C => "PrintScreen",
            0x2D => "Insert",
            0x2E => "Delete",
            0x5B => "MetaLeft",
            0x5C => "MetaRight",
            0x5D => "ContextMenu",
            0x6A => "NumpadMultiply",
            0x6B => "NumpadAdd",
            0x6D => "NumpadSubtract",
            0x6E => "NumpadDecimal",
            0x6F => "NumpadDivide",
            0x90 => "NumLock",
            0x91 => "ScrollLock",
            0xBA => ";",
            0xBB => "=",
            0xBC => ",",
            0xBD => "-",
            0xBE => ".",
            0xBF => "/",
            0xC0 => "`",
            0xDB => "[",
            0xDC => "\\",
            0xDD => "]",
            0xDE => "'",
            0xE2 => "IntlBackslash",
            0x41..=0x5A => {
                let ch = ((vk - 0x41) as u8 + b'a') as char;
                return Some(ch.to_string());
            }
            0x30..=0x39 => {
                let ch = ((vk - 0x30) as u8 + b'0') as char;
                return Some(ch.to_string());
            }
            0x70..=0x7B => {
                return Some(format!("F{}", vk - 0x70 + 1));
            }
            0x60..=0x69 => {
                return Some(format!("Numpad{}", vk - 0x60));
            }
            _ => return None,
        };
        Some(s.to_string())
    }

    #[inline]
    fn xy_from_lparam(lparam: LPARAM) -> (i32, i32) {
        let raw = lparam.0 as i32;
        let x = (raw & 0xFFFF) as i16 as i32;
        let y = ((raw >> 16) & 0xFFFF) as i16 as i32;
        (x, y)
    }

    unsafe extern "system" fn viewport_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const SharedViewportState;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let shared = &*ptr;

        match msg {
            WM_ERASEBKGND => LRESULT(1),
            WM_PAINT => {
                let _ = ValidateRect(hwnd, None);
                LRESULT(0)
            }
            WM_SIZE => {
                let mut rc: RECT = zeroed();
                if GetClientRect(hwnd, &mut rc).is_ok() {
                    let w = (rc.right - rc.left).max(1) as u32;
                    let h = (rc.bottom - rc.top).max(1) as u32;
                    shared.client_w.store(w, Ordering::Relaxed);
                    shared.client_h.store(h, Ordering::Relaxed);
                }
                LRESULT(0)
            }
            WM_SETCURSOR => {
                let cursor_res = if shared.input_locked.load(Ordering::Relaxed) {
                    match shared.cursor_kind.load(Ordering::Relaxed) {
                        1 => IDC_IBEAM,
                        2 => IDC_HAND,
                        3 => IDC_SIZEWE,
                        4 => IDC_SIZENS,
                        5 => IDC_SIZENESW,
                        6 => IDC_SIZENWSE,
                        7 => IDC_SIZEALL,
                        8 => IDC_WAIT,
                        _ => IDC_ARROW,
                    }
                } else {
                    IDC_ARROW
                };
                if let Ok(hcur) = LoadCursorW(None, cursor_res) {
                    SetCursor(hcur);
                }
                LRESULT(1)
            }
            WM_MOUSEMOVE => {
                if shared.input_locked.load(Ordering::Relaxed) {
                    let now = chrono::Utc::now().timestamp_millis();
                    let min_ms = if shared.is_relay.load(Ordering::Relaxed) { 18 } else { 7 };
                    let prev = shared.last_move_ms.load(Ordering::Relaxed);
                    if now - prev >= min_ms {
                        shared.last_move_ms.store(now, Ordering::Relaxed);
                        let (mx, my) = xy_from_lparam(lparam);
                        let (rx, ry) = shared.map_client_to_video(mx, my);
                        shared.push_coalesced_move(rx, ry);
                    }
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN | WM_MBUTTONUP => {
                if msg == WM_LBUTTONDOWN || msg == WM_RBUTTONDOWN || msg == WM_MBUTTONDOWN {
                    let _ = SetFocus(hwnd);
                }
                if shared.input_locked.load(Ordering::Relaxed) {
                    let (mx, my) = xy_from_lparam(lparam);
                    let (rx, ry) = shared.map_client_to_video(mx, my);
                    let (button, action) = match msg {
                        WM_LBUTTONDOWN => (MouseButton::Left, MouseAction::Down),
                        WM_LBUTTONUP => (MouseButton::Left, MouseAction::Up),
                        WM_RBUTTONDOWN => (MouseButton::Right, MouseAction::Down),
                        WM_RBUTTONUP => (MouseButton::Right, MouseAction::Up),
                        WM_MBUTTONDOWN => (MouseButton::Middle, MouseAction::Down),
                        _ => (MouseButton::Middle, MouseAction::Up),
                    };
                    shared.send_input(Message::MouseEvent {
                        x: rx,
                        y: ry,
                        button: Some(button),
                        action,
                        delta_x: None,
                        delta_y: None,
                    });
                }
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                if shared.input_locked.load(Ordering::Relaxed) {
                    let delta = ((wparam.0 >> 16) as i16) as i32;
                    if delta != 0 {
                        let notches = (delta.abs() / 120).clamp(1, 5);
                        let dy = if delta > 0 { -notches } else { notches };
                        let vw = shared.video_w.load(Ordering::Relaxed) as i32 / 2;
                        let vh = shared.video_h.load(Ordering::Relaxed) as i32 / 2;
                        shared.send_input(Message::MouseEvent {
                            x: vw,
                            y: vh,
                            button: None,
                            action: MouseAction::Scroll,
                            delta_x: None,
                            delta_y: Some(dy),
                        });
                    }
                }
                LRESULT(0)
            }
            WM_GETDLGCODE_MSG => LRESULT(DLGC_WANTALLKEYS_FLAGS),
            WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
                if shared.input_locked.load(Ordering::Relaxed) {
                    let vk = wparam.0 as u16;
                    if let Some(key) = vk_to_key_string(vk, lparam.0) {
                        let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
                        let bit = modifier_bit(&key);
                        if bit != 0 {
                            if is_up {
                                shared.modifier_mask.fetch_and(!bit, Ordering::Relaxed);
                            } else {
                                shared.modifier_mask.fetch_or(bit, Ordering::Relaxed);
                            }
                        }
                        let action = if is_up { KeyAction::Up } else { KeyAction::Down };
                        shared.send_input(Message::KeyEvent { key, action });
                        return LRESULT(0);
                    }
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_IME_CHAR_MSG | WM_CHAR_MSG => {
                if shared.input_locked.load(Ordering::Relaxed) {
                    let code = wparam.0 as u32;
                    // ASCII printable/control keys are already sent as exact raw WM_KEYDOWN/UP;
                    // only forward non-ASCII characters (IME/Unicode tools) via TypeText.
                    if code >= 0x80 || (msg == WM_IME_CHAR_MSG && code >= 0x20) {
                        if let Some(ch) = char::from_u32(code) {
                            shared.send_input(Message::TypeText {
                                text: ch.to_string(),
                            });
                            return LRESULT(0);
                        }
                    }
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_KILLFOCUS => {
                let mask = shared.modifier_mask.swap(0, Ordering::AcqRel);
                if shared.input_locked.load(Ordering::Relaxed) {
                    let mods = [
                        (1 << 0, "ShiftLeft"),
                        (1 << 1, "ShiftRight"),
                        (1 << 2, "ControlLeft"),
                        (1 << 3, "ControlRight"),
                        (1 << 4, "AltLeft"),
                        (1 << 5, "AltRight"),
                        (1 << 6, "MetaLeft"),
                        (1 << 7, "MetaRight"),
                    ];
                    for (bit, mod_key) in mods {
                        if (mask & bit) != 0 {
                            shared.send_input(Message::KeyEvent {
                                key: mod_key.to_string(),
                                action: KeyAction::Up,
                            });
                        }
                    }
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    fn compile_shaders(
        device: &ID3D11Device,
    ) -> Result<(ID3D11VertexShader, ID3D11PixelShader, ID3D11Buffer, ID3D11SamplerState)> {
        unsafe {
            let mut vs_blob: Option<ID3DBlob> = None;
            let mut err_blob: Option<ID3DBlob> = None;
            D3DCompile(
                VS_HLSL.as_ptr() as *const c_void,
                VS_HLSL.len(),
                None,
                None,
                None,
                windows::core::s!("main"),
                windows::core::s!("vs_5_0"),
                0,
                0,
                &mut vs_blob,
                Some(&mut err_blob),
            )
            .map_err(|e| anyhow!("VS compile failed: {}", e))?;
            let vs_blob = vs_blob.ok_or_else(|| anyhow!("Missing VS blob"))?;
            let vs_slice =
                std::slice::from_raw_parts(vs_blob.GetBufferPointer() as *const u8, vs_blob.GetBufferSize());
            let mut vs: Option<ID3D11VertexShader> = None;
            device.CreateVertexShader(vs_slice, None, Some(&mut vs))?;
            let vs = vs.ok_or_else(|| anyhow!("CreateVertexShader failed"))?;

            let mut ps_blob: Option<ID3DBlob> = None;
            D3DCompile(
                PS_HLSL.as_ptr() as *const c_void,
                PS_HLSL.len(),
                None,
                None,
                None,
                windows::core::s!("main"),
                windows::core::s!("ps_5_0"),
                0,
                0,
                &mut ps_blob,
                Some(&mut err_blob),
            )
            .map_err(|e| anyhow!("PS compile failed: {}", e))?;
            let ps_blob = ps_blob.ok_or_else(|| anyhow!("Missing PS blob"))?;
            let ps_slice =
                std::slice::from_raw_parts(ps_blob.GetBufferPointer() as *const u8, ps_blob.GetBufferSize());
            let mut ps: Option<ID3D11PixelShader> = None;
            device.CreatePixelShader(ps_slice, None, Some(&mut ps))?;
            let ps = ps.ok_or_else(|| anyhow!("CreatePixelShader failed"))?;

            let cb_desc = D3D11_BUFFER_DESC {
                ByteWidth: size_of::<CropConstants>() as u32,
                Usage: D3D11_USAGE_DYNAMIC,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                MiscFlags: 0,
                StructureByteStride: 0,
            };
            let mut cb: Option<ID3D11Buffer> = None;
            device.CreateBuffer(&cb_desc, None, Some(&mut cb))?;
            let cb = cb.ok_or_else(|| anyhow!("CreateBuffer(CropCB) failed"))?;

            let samp_desc = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                MipLODBias: 0.0,
                MaxAnisotropy: 1,
                ComparisonFunc: windows::Win32::Graphics::Direct3D11::D3D11_COMPARISON_NEVER,
                BorderColor: [0.0; 4],
                MinLOD: 0.0,
                MaxLOD: f32::MAX,
            };
            let mut samp: Option<ID3D11SamplerState> = None;
            device.CreateSamplerState(&samp_desc, Some(&mut samp))?;
            let samp = samp.ok_or_else(|| anyhow!("CreateSamplerState failed"))?;

            Ok((vs, ps, cb, samp))
        }
    }

    struct Nv12SrvCache {
        tex_w: u32,
        tex_h: u32,
        tex: ID3D11Texture2D,
        srv_y: ID3D11ShaderResourceView,
        srv_uv: ID3D11ShaderResourceView,
    }

    impl Nv12SrvCache {
        fn ensure<'a>(
            cache: &'a mut Option<Self>,
            device: &ID3D11Device,
            w: u32,
            h: u32,
        ) -> Result<&'a Self> {
            let need_rebuild = match cache {
                Some(c) => c.tex_w != w || c.tex_h != h,
                None => true,
            };
            if need_rebuild {
                unsafe {
                    let desc = D3D11_TEXTURE2D_DESC {
                        Width: w,
                        Height: h,
                        MipLevels: 1,
                        ArraySize: 1,
                        Format: DXGI_FORMAT_NV12,
                        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                        Usage: D3D11_USAGE_DEFAULT,
                        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                        CPUAccessFlags: 0,
                        MiscFlags: 0,
                    };
                    let mut tex: Option<ID3D11Texture2D> = None;
                    device.CreateTexture2D(&desc, None, Some(&mut tex))?;
                    let tex = tex.ok_or_else(|| anyhow!("CreateTexture2D(NV12) failed"))?;

                    let y_srv_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
                        Format: DXGI_FORMAT_R8_UNORM,
                        ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2D,
                        Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                            Texture2D: D3D11_TEX2D_SRV {
                                MostDetailedMip: 0,
                                MipLevels: 1,
                            },
                        },
                    };
                    let mut srv_y: Option<ID3D11ShaderResourceView> = None;
                    device.CreateShaderResourceView(&tex, Some(&y_srv_desc), Some(&mut srv_y))?;
                    let srv_y = srv_y.ok_or_else(|| anyhow!("CreateSRV(Y) failed"))?;

                    let uv_srv_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
                        Format: DXGI_FORMAT_R8G8_UNORM,
                        ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2D,
                        Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                            Texture2D: D3D11_TEX2D_SRV {
                                MostDetailedMip: 0,
                                MipLevels: 1,
                            },
                        },
                    };
                    let mut srv_uv: Option<ID3D11ShaderResourceView> = None;
                    device.CreateShaderResourceView(&tex, Some(&uv_srv_desc), Some(&mut srv_uv))?;
                    let srv_uv = srv_uv.ok_or_else(|| anyhow!("CreateSRV(UV) failed"))?;

                    *cache = Some(Self {
                        tex_w: w,
                        tex_h: h,
                        tex,
                        srv_y,
                        srv_uv,
                    });
                }
            }
            Ok(cache.as_ref().unwrap())
        }
    }

    struct SwapChainState {
        swap_chain: IDXGISwapChain1,
        rtv: Option<ID3D11RenderTargetView>,
        width: u32,
        height: u32,
    }

    impl SwapChainState {
        fn new(device: &ID3D11Device, hwnd: HWND, w: u32, h: u32) -> Result<Self> {
            unsafe {
                let dxgi_dev: IDXGIDevice = device.cast()?;
                let adapter = dxgi_dev.GetAdapter()?;
                let factory: IDXGIFactory2 = adapter.GetParent()?;
                let _ = factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER);

                let desc = DXGI_SWAP_CHAIN_DESC1 {
                    Width: w.max(1),
                    Height: h.max(1),
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    Stereo: false.into(),
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                    BufferCount: 3,
                    Scaling: DXGI_SCALING_STRETCH,
                    SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                    AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                    Flags: 0,
                };

                let swap_chain = factory.CreateSwapChainForHwnd(device, hwnd, &desc, None, None)?;
                let back_buf: ID3D11Texture2D = swap_chain.GetBuffer(0)?;
                let mut rtv: Option<ID3D11RenderTargetView> = None;
                device.CreateRenderTargetView(&back_buf, None, Some(&mut rtv))?;
                let rtv = rtv.ok_or_else(|| anyhow!("CreateRenderTargetView failed"))?;

                Ok(Self {
                    swap_chain,
                    rtv: Some(rtv),
                    width: w.max(1),
                    height: h.max(1),
                })
            }
        }

        fn ensure_size(&mut self, device: &ID3D11Device, w: u32, h: u32) -> Result<()> {
            let tw = w.max(1);
            let th = h.max(1);
            if self.width == tw && self.height == th {
                return Ok(());
            }
            unsafe {
                self.rtv = None;
                self.swap_chain.ResizeBuffers(
                    0,
                    tw,
                    th,
                    DXGI_FORMAT_B8G8R8A8_UNORM,
                    DXGI_SWAP_CHAIN_FLAG(0),
                )?;
                let back_buf: ID3D11Texture2D = self.swap_chain.GetBuffer(0)?;
                let mut rtv: Option<ID3D11RenderTargetView> = None;
                device.CreateRenderTargetView(&back_buf, None, Some(&mut rtv))?;
                self.rtv = Some(rtv.ok_or_else(|| anyhow!("Resize CreateRenderTargetView failed"))?);
                self.width = tw;
                self.height = th;
                Ok(())
            }
        }
    }

    fn run_d3d11_render_loop(
        rx: Receiver<H264Packet>,
        active_flag: Arc<AtomicBool>,
        shared: Arc<SharedViewportState>,
        child_hwnd: HWND,
        app_handle: tauri::AppHandle,
    ) {
        let (device, context, mgr) = match create_d3d11_video_device() {
            Ok(v) => v,
            Err(e) => {
                crate::diag_log(&format!("D3D11 device creation failed: {}", e));
                return;
            }
        };

        let (vs, ps, cb, samp) = match compile_shaders(&device) {
            Ok(v) => v,
            Err(e) => {
                crate::diag_log(&format!("D3D11 HLSL shader compile failed: {}", e));
                return;
            }
        };

        let cw = shared.client_w.load(Ordering::Relaxed).max(1);
        let ch = shared.client_h.load(Ordering::Relaxed).max(1);
        let mut sc = match SwapChainState::new(&device, child_hwnd, cw, ch) {
            Ok(s) => s,
            Err(e) => {
                crate::diag_log(&format!("DXGI CreateSwapChainForHwnd failed: {}", e));
                return;
            }
        };

        let mut decoder = match H264Decoder::new(device.clone(), mgr.clone()) {
            Ok(d) => d,
            Err(e) => {
                crate::diag_log(&format!("H264Decoder init failed: {}", e));
                return;
            }
        };

        let mut srv_cache: Option<Nv12SrvCache> = None;
        let mut frames_rendered: u64 = 0;
        let mut bytes_this_window: u64 = 0;
        let mut stats_start = Instant::now();
        let mut diag_start = Instant::now();
        let mut diag_frames: u64 = 0;
        let mut diag_bytes: u64 = 0;
        let mut diag_dec_ms_sum: f64 = 0.0;
        let mut diag_skips: u64 = 0;
        let mut notified_first_frame = false;

        while active_flag.load(Ordering::Acquire) {
            let mut pkt = match rx.recv_timeout(Duration::from_millis(80)) {
                Ok(p) => p,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // Keep swapchain size responsive during resize even when screen is static
                    let nw = shared.client_w.load(Ordering::Relaxed).max(1);
                    let nh = shared.client_h.load(Ordering::Relaxed).max(1);
                    let _ = sc.ensure_size(&device, nw, nh);
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };

            // Fast backlog skip: if a fresh keyframe is already queued behind stale frames, jump to it!
            let mut drained = Vec::new();
            while let Ok(newer) = rx.try_recv() {
                drained.push(newer);
            }
            let mut start_idx = 0usize;
            if drained.len() > 4 {
                if let Some(last_kf) = drained.iter().rposition(|f| f.is_keyframe) {
                    pkt = drained.swap_remove(last_kf);
                    start_idx = drained.len(); // skip frames older than keyframe
                }
            }

            let mut to_decode = Vec::with_capacity(1 + drained.len().saturating_sub(start_idx));
            to_decode.push(pkt);
            if start_idx < drained.len() {
                to_decode.extend(drained.into_iter().skip(start_idx));
            }

            let mut last_decoded: Option<DecodedNv12Frame> = None;
            if to_decode.len() > 1 {
                diag_skips += (to_decode.len() - 1) as u64;
            }
            for frame_pkt in to_decode {
                bytes_this_window += frame_pkt.data.len() as u64;
                diag_bytes += frame_pkt.data.len() as u64;
                let dec_t0 = Instant::now();
                match decoder.decode(&frame_pkt.data, frame_pkt.width, frame_pkt.height) {
                    Ok(Some(dec)) => {
                        diag_dec_ms_sum += dec_t0.elapsed().as_secs_f64() * 1000.0;
                        last_decoded = Some(dec);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        crate::diag_log(&format!(
                            "H264 decode error at {}x{} (keyframe={}): {} -> recreating decoder",
                            frame_pkt.width, frame_pkt.height, frame_pkt.is_keyframe, e
                        ));
                        if let Ok(fresh) = H264Decoder::new(device.clone(), mgr.clone()) {
                            decoder = fresh;
                            if frame_pkt.is_keyframe {
                                if let Ok(Some(dec)) = decoder.decode(&frame_pkt.data, frame_pkt.width, frame_pkt.height) {
                                    diag_dec_ms_sum += dec_t0.elapsed().as_secs_f64() * 1000.0;
                                    last_decoded = Some(dec);
                                }
                            }
                        }
                    }
                }
            }

            let Some(decoded) = last_decoded else {
                continue;
            };
            let is_gpu_tex = matches!(decoded, DecodedNv12Frame::GpuTexture { .. });

            let (tex_w, tex_h, vis_w, vis_h) = match &decoded {
                DecodedNv12Frame::GpuTexture {
                    texture,
                    subresource,
                    tex_w,
                    tex_h,
                    visible_w,
                    visible_h,
                } => {
                    let cache = match Nv12SrvCache::ensure(&mut srv_cache, &device, *tex_w, *tex_h) {
                        Ok(c) => c,
                        Err(_) => continue,
                    };
                    unsafe {
                        context.CopySubresourceRegion(
                            &cache.tex,
                            0,
                            0,
                            0,
                            0,
                            texture,
                            *subresource,
                            None,
                        );
                    }
                    (*tex_w, *tex_h, *visible_w, *visible_h)
                }
                DecodedNv12Frame::CpuBuffer {
                    data,
                    tex_w,
                    tex_h,
                    visible_w,
                    visible_h,
                } => {
                    let cache = match Nv12SrvCache::ensure(&mut srv_cache, &device, *tex_w, *tex_h) {
                        Ok(c) => c,
                        Err(_) => continue,
                    };
                    unsafe {
                        context.UpdateSubresource(
                            &cache.tex,
                            0,
                            None,
                            data.as_ptr() as *const c_void,
                            *tex_w,
                            0,
                        );
                    }
                    (*tex_w, *tex_h, *visible_w, *visible_h)
                }
            };

            shared.video_w.store(vis_w as u16, Ordering::Relaxed);
            shared.video_h.store(vis_h as u16, Ordering::Relaxed);

            let cw = shared.client_w.load(Ordering::Relaxed).max(1);
            let ch = shared.client_h.load(Ordering::Relaxed).max(1);
            if sc.ensure_size(&device, cw, ch).is_err() {
                continue;
            }

            let cache = srv_cache.as_ref().unwrap();
            unsafe {
                let mut mapped: D3D11_MAPPED_SUBRESOURCE = zeroed();
                if context
                    .Map(&cb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped))
                    .is_ok()
                {
                    let cdata = CropConstants {
                        uv_scale: [
                            (vis_w as f32) / (tex_w.max(1) as f32),
                            (vis_h as f32) / (tex_h.max(1) as f32),
                        ],
                        pad: [0.0, 0.0],
                    };
                    ptr::copy_nonoverlapping(&cdata as *const CropConstants, mapped.pData as *mut CropConstants, 1);
                    context.Unmap(&cb, 0);
                }

                // Aspect-preserving letterbox D3D11 viewport
                let rect_aspect = (cw as f32) / (ch as f32);
                let vid_aspect = (vis_w.max(1) as f32) / (vis_h.max(1) as f32);
                let (vp_w, vp_h, vp_x, vp_y) = if rect_aspect > vid_aspect {
                    let dw = (ch as f32) * vid_aspect;
                    (dw, ch as f32, ((cw as f32) - dw) * 0.5, 0.0)
                } else {
                    let dh = (cw as f32) / vid_aspect;
                    (cw as f32, dh, 0.0, ((ch as f32) - dh) * 0.5)
                };

                let vp = D3D11_VIEWPORT {
                    TopLeftX: vp_x,
                    TopLeftY: vp_y,
                    Width: vp_w.max(1.0),
                    Height: vp_h.max(1.0),
                    MinDepth: 0.0,
                    MaxDepth: 1.0,
                };

                if let Some(ref rtv) = sc.rtv {
                    context.ClearRenderTargetView(rtv, &[0.0, 0.0, 0.0, 1.0]);
                    context.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
                }
                context.RSSetViewports(Some(&[vp]));
                context.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
                context.VSSetShader(&vs, None);
                context.VSSetConstantBuffers(0, Some(&[Some(cb.clone())]));
                context.PSSetShader(&ps, None);
                context.PSSetShaderResources(
                    0,
                    Some(&[Some(cache.srv_y.clone()), Some(cache.srv_uv.clone())]),
                );
                context.PSSetSamplers(0, Some(&[Some(samp.clone())]));
                context.Draw(4, 0);

                // Non-blocking flip-model present after first frame (prevents DWM/WARP stall!)
                if !notified_first_frame {
                    let _ = sc.swap_chain.Present(0, DXGI_PRESENT(0));
                } else {
                    let _ = sc.swap_chain.Present(0, DXGI_PRESENT_DO_NOT_WAIT);
                }
            }

            if !notified_first_frame {
                notified_first_frame = true;
                shared.has_first_frame.store(true, Ordering::Release);
                let hwnd_val = shared.child_hwnd.load(Ordering::Acquire) as isize;
                let vis = shared.visible.load(Ordering::Acquire);
                if hwnd_val != 0 && vis {
                    let app_clone = app_handle.clone();
                    let _ = app_clone.run_on_main_thread(move || unsafe {
                        let _ = ShowWindow(HWND(hwnd_val as *mut c_void), SW_SHOWNOACTIVATE);
                        let _ = SetWindowPos(
                            HWND(hwnd_val as *mut c_void),
                            HWND_TOP,
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                        );
                    });
                }
                let _ = app_handle.emit(
                    "native-first-frame",
                    serde_json::json!({ "width": vis_w, "height": vis_h }),
                );
                crate::diag_log(&format!(
                    "[CtrlD3D11] First frame rendered ({}): {}x{} (tex={}x{})",
                    if is_gpu_tex { "DXVA-GPU" } else { "CPU-NV12-Shader" },
                    vis_w,
                    vis_h,
                    tex_w,
                    tex_h
                ));
            }

            frames_rendered += 1;
            diag_frames += 1;
            let elapsed = stats_start.elapsed().as_secs_f64();
            if elapsed >= 0.5 {
                let fps = ((frames_rendered as f64) / elapsed).round().min(120.0) as u32;
                let kbps = (((bytes_this_window * 8) as f64) / (elapsed * 1000.0)).round() as u32;
                let _ = app_handle.emit(
                    "native-stream-stats",
                    serde_json::json!({
                        "fps": fps,
                        "bitrate_kbps": kbps,
                        "width": vis_w,
                        "height": vis_h,
                        "renderer": "D3D11 H.264"
                    }),
                );
                frames_rendered = 0;
                bytes_this_window = 0;
                stats_start = Instant::now();
            }

            let d_elapsed = diag_start.elapsed().as_secs_f64();
            if d_elapsed >= 3.0 {
                let d_fps = (diag_frames as f64) / d_elapsed.max(0.1);
                let d_kbps = ((diag_bytes * 8) as f64 / (d_elapsed * 1000.0)).round() as u64;
                let avg_dec = if diag_frames > 0 { diag_dec_ms_sum / diag_frames as f64 } else { 0.0 };
                crate::diag_log(&format!(
                    "[CtrlD3D11] fps={:.1} rx_kbps={} avg_dec={:.1}ms queue_skips={} mode={}",
                    d_fps,
                    d_kbps,
                    avg_dec,
                    diag_skips,
                    if is_gpu_tex { "DXVA-GPU" } else { "CPU-NV12" }
                ));
                diag_frames = 0;
                diag_bytes = 0;
                diag_dec_ms_sum = 0.0;
                diag_skips = 0;
                diag_start = Instant::now();
            }
        }
    }

    pub struct NativeD3D11Controller {
        shared: Arc<SharedViewportState>,
        active_flag: StdMutex<Option<Arc<AtomicBool>>>,
        frame_tx: StdMutex<Option<Sender<H264Packet>>>,
    }

    impl NativeD3D11Controller {
        pub fn new() -> Self {
            Self {
                shared: Arc::new(SharedViewportState::new()),
                active_flag: StdMutex::new(None),
                frame_tx: StdMutex::new(None),
            }
        }

        fn ensure_child_hwnd(&self, app_handle: &tauri::AppHandle) -> Result<HWND> {
            let existing = self.shared.child_hwnd.load(Ordering::Acquire) as isize;
            if existing != 0 {
                return Ok(HWND(existing as *mut c_void));
            }

            let window = app_handle
                .get_webview_window("main")
                .ok_or_else(|| anyhow!("Main window not found"))?;
            let parent_hwnd = window
                .hwnd()
                .map_err(|e| anyhow!("Failed getting main HWND: {}", e))?;
            let parent_raw = parent_hwnd.0 as isize;
            let shared_ptr = Arc::as_ptr(&self.shared) as isize;

            let (tx, rx) = mpsc::channel::<Result<isize>>();
            app_handle
                .run_on_main_thread(move || unsafe {
                    let hinst: HINSTANCE = GetModuleHandleW(None)
                        .map(|h| HINSTANCE(h.0))
                        .unwrap_or(HINSTANCE(ptr::null_mut()));

                    REGISTER_CLASS_ONCE.call_once(|| {
                        let wc = WNDCLASSEXW {
                            cbSize: size_of::<WNDCLASSEXW>() as u32,
                            style: windows::Win32::UI::WindowsAndMessaging::CS_OWNDC,
                            lpfnWndProc: Some(viewport_wnd_proc),
                            cbClsExtra: 0,
                            cbWndExtra: 0,
                            hInstance: hinst,
                            hIcon: Default::default(),
                            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                            hbrBackground: Default::default(),
                            lpszMenuName: PCWSTR::null(),
                            lpszClassName: CLASS_NAME,
                            hIconSm: Default::default(),
                        };
                        let _ = RegisterClassExW(&wc);
                    });

                    let res = CreateWindowExW(
                        Default::default(),
                        CLASS_NAME,
                        w!("OpenRemoteVideo"),
                        WS_CHILD | WS_CLIPSIBLINGS,
                        0,
                        0,
                        1,
                        1,
                        HWND(parent_raw as *mut c_void),
                        None,
                        hinst,
                        None,
                    );

                    match res {
                        Ok(child) => {
                            SetWindowLongPtrW(child, GWLP_USERDATA, shared_ptr);
                            // Disable local IME on the remote viewport child HWND so Windows delivers
                            // raw WM_KEYDOWN virtual keys (never VK_PROCESSKEY) and lets the remote OS IME handle text.
                            let _ = ImmAssociateContext(child.0 as isize, 0);
                            let _ = tx.send(Ok(child.0 as isize));
                        }
                        Err(e) => {
                            let _ = tx.send(Err(anyhow!("CreateWindowExW failed: {}", e)));
                        }
                    }
                })
                .map_err(|e| anyhow!("run_on_main_thread failed: {}", e))?;

            let hwnd_val = rx
                .recv_timeout(Duration::from_secs(3))
                .map_err(|_| anyhow!("Timed out waiting for native viewport HWND"))??;
            self.shared.child_hwnd.store(hwnd_val as u64, Ordering::Release);
            Ok(HWND(hwnd_val as *mut c_void))
        }

        pub fn start_session(
            &self,
            app_handle: &tauri::AppHandle,
            input_tx: tokio::sync::mpsc::UnboundedSender<Message>,
            is_relay: bool,
        ) -> Result<()> {
            self.stop_session(app_handle);

            self.shared.has_first_frame.store(false, Ordering::Release);
            self.shared.is_relay.store(is_relay, Ordering::Relaxed);
            if let Ok(mut guard) = self.shared.input_tx.lock() {
                *guard = Some(input_tx);
            }

            let child_hwnd = self.ensure_child_hwnd(app_handle)?;
            let (tx, rx) = mpsc::channel::<H264Packet>();
            let active = Arc::new(AtomicBool::new(true));

            if let Ok(mut slot) = self.active_flag.lock() {
                *slot = Some(Arc::clone(&active));
            }
            if let Ok(mut ftx) = self.frame_tx.lock() {
                *ftx = Some(tx);
            }

            let child_hwnd_raw = child_hwnd.0 as isize;
            let shared_clone = Arc::clone(&self.shared);
            let app_clone = app_handle.clone();
            std::thread::Builder::new()
                .name("openremote-d3d11-render".to_string())
                .spawn(move || {
                    run_d3d11_render_loop(
                        rx,
                        active,
                        shared_clone,
                        HWND(child_hwnd_raw as *mut c_void),
                        app_clone,
                    );
                })
                .map_err(|e| anyhow!("Spawn D3D11 render thread failed: {}", e))?;

            Ok(())
        }

        pub fn stop_session(&self, app_handle: &tauri::AppHandle) {
            if let Ok(mut slot) = self.active_flag.lock() {
                if let Some(flag) = slot.take() {
                    flag.store(false, Ordering::Release);
                }
            }
            if let Ok(mut ftx) = self.frame_tx.lock() {
                *ftx = None;
            }
            if let Ok(mut itx) = self.shared.input_tx.lock() {
                *itx = None;
            }
            self.shared.has_first_frame.store(false, Ordering::Release);
            self.shared.visible.store(false, Ordering::Release);

            let hwnd_val = self.shared.child_hwnd.load(Ordering::Acquire) as isize;
            if hwnd_val != 0 {
                let _ = app_handle.run_on_main_thread(move || unsafe {
                    let _ = ShowWindow(HWND(hwnd_val as *mut c_void), SW_HIDE);
                });
            }
        }

        pub fn push_h264_frame(&self, data: Vec<u8>, width: u16, height: u16, is_keyframe: bool) {
            if let Ok(guard) = self.frame_tx.lock() {
                if let Some(ref tx) = *guard {
                    let _ = tx.send(H264Packet {
                        data,
                        width,
                        height,
                        is_keyframe,
                    });
                }
            }
        }

        pub fn sync_viewport(
            &self,
            app_handle: &tauri::AppHandle,
            x: i32,
            y: i32,
            w: i32,
            h: i32,
            visible: bool,
        ) {
            self.shared.visible.store(visible, Ordering::Release);
            if w > 0 && h > 0 {
                self.shared.client_w.store(w as u32, Ordering::Relaxed);
                self.shared.client_h.store(h as u32, Ordering::Relaxed);
            }

            let hwnd_val = self.shared.child_hwnd.load(Ordering::Acquire) as isize;
            if hwnd_val == 0 {
                return;
            }
            let actually_show = visible && self.shared.has_first_frame.load(Ordering::Acquire) && w > 8 && h > 8;
            let _ = app_handle.run_on_main_thread(move || unsafe {
                let hwnd = HWND(hwnd_val as *mut c_void);
                if actually_show {
                    let _ = SetWindowPos(
                        hwnd,
                        HWND_TOP,
                        x,
                        y,
                        w.max(1),
                        h.max(1),
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                } else {
                    let _ = SetWindowPos(
                        hwnd,
                        HWND::default(),
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_HIDEWINDOW,
                    );
                }
            });
        }

        pub fn set_input_locked(&self, locked: bool) {
            self.shared.input_locked.store(locked, Ordering::Relaxed);
        }

        pub fn set_cursor_css(&self, css: &str) {
            self.shared
                .cursor_kind
                .store(css_cursor_to_kind(css), Ordering::Relaxed);
        }

        pub fn take_pending_move(&self) -> Option<(i32, i32)> {
            self.shared.take_coalesced_move()
        }

        pub fn move_notify(&self) -> Arc<tokio::sync::Notify> {
            Arc::clone(&self.shared.move_notify)
        }
    }

    unsafe impl Send for NativeD3D11Controller {}
    unsafe impl Sync for NativeD3D11Controller {}
}

#[cfg(target_os = "windows")]
pub use win_d3d11::NativeD3D11Controller;

#[cfg(not(target_os = "windows"))]
mod stub_d3d11 {
    use crate::protocol::Message;
    use anyhow::Result;

    pub struct NativeD3D11Controller;
    impl NativeD3D11Controller {
        pub fn new() -> Self {
            Self
        }
        pub fn start_session(
            &self,
            _app_handle: &tauri::AppHandle,
            _input_tx: tokio::sync::mpsc::UnboundedSender<Message>,
            _is_relay: bool,
        ) -> Result<()> {
            Ok(())
        }
        pub fn stop_session(&self, _app_handle: &tauri::AppHandle) {}
        pub fn push_h264_frame(&self, _data: Vec<u8>, _w: u16, _h: u16, _kf: bool) {}
        pub fn sync_viewport(
            &self,
            _app_handle: &tauri::AppHandle,
            _x: i32,
            _y: i32,
            _w: i32,
            _h: i32,
            _visible: bool,
        ) {
        }
        pub fn set_input_locked(&self, _locked: bool) {}
        pub fn set_cursor_css(&self, _css: &str) {}
        pub fn take_pending_move(&self) -> Option<(i32, i32)> {
            None
        }
        pub fn move_notify(&self) -> std::sync::Arc<tokio::sync::Notify> {
            std::sync::Arc::new(tokio::sync::Notify::new())
        }
    }
}

#[cfg(not(target_os = "windows"))]
pub use stub_d3d11::NativeD3D11Controller;
