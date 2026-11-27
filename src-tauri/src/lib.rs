pub mod capture;
pub mod clipboard;
pub mod codec;
pub mod commands;
pub mod config;
pub mod file_transfer;
pub mod input;
pub mod nat;
pub mod protocol;
pub mod renderer;
pub mod service;
pub mod transport;
pub mod wol;
pub mod signaling;

use commands::AppState;
use config::AppConfig;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicI8, AtomicU32, AtomicU8, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tauri::Manager;
use transport::QuicServer;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HostControlStatus {
    pub active: bool,
    pub host_pid: u32,
    pub controller_id: String,
    pub transport_mode: String,
    pub remote_addr: String,
    pub connected_at_ms: i64,
}

fn host_status_path() -> std::path::PathBuf {
    AppConfig::shared_data_dir().join("host_session.json")
}

fn host_disconnect_req_path() -> std::path::PathBuf {
    AppConfig::shared_data_dir().join("disconnect_host.req")
}

pub fn write_host_control_status(status: &HostControlStatus) {
    let path = host_status_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(status) {
        let _ = std::fs::write(&path, json);
    }
}

pub fn clear_host_control_status() {
    write_host_control_status(&HostControlStatus {
        active: false,
        host_pid: std::process::id(),
        ..Default::default()
    });
}

fn is_pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(target_os = "windows")]
    unsafe {
        #[link(name = "kernel32")]
        extern "system" {
            fn OpenProcess(dw_desired_access: u32, b_inherit_handle: i32, dw_process_id: u32) -> isize;
            fn GetExitCodeProcess(h_process: isize, lp_exit_code: *mut u32) -> i32;
            fn CloseHandle(h_object: isize) -> i32;
        }
        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h == 0 {
            // If OpenProcess failed with ERROR_ACCESS_DENIED (5) because the worker runs as SYSTEM,
            // that actually means the SYSTEM process IS alive!
            return std::io::Error::last_os_error().raw_os_error() == Some(5);
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code) != 0 && code == 259;
        CloseHandle(h);
        return ok;
    }
    #[cfg(not(target_os = "windows"))]
    {
        true
    }
}

pub fn read_host_control_status() -> HostControlStatus {
    let path = host_status_path();
    if let Ok(data) = std::fs::read_to_string(&path) {
        if let Ok(st) = serde_json::from_str::<HostControlStatus>(&data) {
            if st.active && !is_pid_alive(st.host_pid) {
                return HostControlStatus::default();
            }
            return st;
        }
    }
    HostControlStatus::default()
}

pub fn request_host_disconnect() {
    let req_path = host_disconnect_req_path();
    if let Some(parent) = req_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&req_path, b"disconnect");
    // Optimistically mark UI inactive immediately while worker closes in <150ms
    clear_host_control_status();
}

static DIAG_LOCK: StdMutex<()> = StdMutex::new(());

/// Lightweight rolling diagnostic log at %APPDATA%\openremote\openremote.log (capped at 256KB)
/// so GUI release builds always leave clear ground-truth diagnostics for capture/input/network issues.
pub fn diag_log(msg: &str) {
    let _g = match DIAG_LOCK.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let path = AppConfig::config_path()
        .parent()
        .map(|p| p.join("openremote.log"))
        .unwrap_or_else(|| std::path::PathBuf::from("openremote.log"));
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > 256 * 1024 {
            let _ = std::fs::write(&path, b"");
        }
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let _ = writeln!(f, "[{}] {}", ts, msg);
    }
}

struct StreamProfile {
    quality: AtomicU8,
    quality_backoff: AtomicI8, // Negative adjustment applied dynamically under network congestion
    max_width: AtomicU32,
    fps: AtomicU8,
    native_w: AtomicU32,
    native_h: AtomicU32,
    stream_w: AtomicU32,
    stream_h: AtomicU32,
    need_keyframe: AtomicBool,
    supports_cursor: AtomicBool,
    supports_notice: AtomicBool,
    supports_h264: AtomicBool,
    ack_active: AtomicBool,
    last_sent_frame_ts: AtomicI64,
    last_acked_frame_ts: AtomicI64,
    first_unacked_sent_ms: AtomicI64,
    last_input_warn_ms: AtomicI64,
    is_relay: bool,
}

impl StreamProfile {
    fn new(is_relay: bool) -> Arc<Self> {
        Arc::new(Self {
            // Direct LAN/IPv6/P2P: crisp 1080p @ 60FPS; Relay: balanced 720p @ 30FPS with ABR
            quality: AtomicU8::new(if is_relay { 52 } else { 72 }),
            quality_backoff: AtomicI8::new(0),
            max_width: AtomicU32::new(if is_relay { 1280 } else { 1920 }),
            fps: AtomicU8::new(if is_relay { 30 } else { 60 }),
            native_w: AtomicU32::new(0),
            native_h: AtomicU32::new(0),
            stream_w: AtomicU32::new(0),
            stream_h: AtomicU32::new(0),
            need_keyframe: AtomicBool::new(true),
            supports_cursor: AtomicBool::new(false),
            supports_notice: AtomicBool::new(false),
            supports_h264: AtomicBool::new(false),
            ack_active: AtomicBool::new(false),
            last_sent_frame_ts: AtomicI64::new(0),
            last_acked_frame_ts: AtomicI64::new(0),
            first_unacked_sent_ms: AtomicI64::new(0),
            last_input_warn_ms: AtomicI64::new(0),
            is_relay,
        })
    }

    fn update(&self, quality: u8, max_width: u32, fps: u8) {
        self.quality.store(quality.clamp(25, 95), Ordering::Relaxed);
        self.quality_backoff.store(0, Ordering::Relaxed);
        self.max_width.store(max_width, Ordering::Relaxed);
        self.fps.store(fps.clamp(10, 60), Ordering::Relaxed);
        self.need_keyframe.store(true, Ordering::Release);
    }

    fn effective_quality(&self) -> u8 {
        let base = self.quality.load(Ordering::Relaxed) as i16;
        let backoff = self.quality_backoff.load(Ordering::Relaxed) as i16;
        (base + backoff).clamp(25, 95) as u8
    }

    fn effective_max_width(&self) -> u32 {
        let base = self.max_width.load(Ordering::Relaxed);
        let backoff = self.quality_backoff.load(Ordering::Relaxed);
        // Under heavy Relay congestion, temporarily scale down to 960px to preserve 30FPS responsiveness
        if self.is_relay && backoff <= -16 && (base == 0 || base > 960) {
            960
        } else {
            base
        }
    }

    fn map_coords(&self, x: i32, y: i32) -> (i32, i32) {
        let nw = self.native_w.load(Ordering::Relaxed) as i64;
        let nh = self.native_h.load(Ordering::Relaxed) as i64;
        let sw = self.stream_w.load(Ordering::Relaxed) as i64;
        let sh = self.stream_h.load(Ordering::Relaxed) as i64;
        if nw > 0 && nh > 0 && sw > 0 && sh > 0 && (nw != sw || nh != sh) {
            let mx = ((x as i64 * nw) / sw).clamp(0, nw - 1) as i32;
            let my = ((y as i64 * nh) / sh).clamp(0, nh - 1) as i32;
            (mx, my)
        } else {
            (x, y)
        }
    }
}

fn maybe_send_input_warning(
    channel_rx: &transport::ConnectionChannel,
    profile: &Arc<StreamProfile>,
    err_text: String,
) {
    if !profile.supports_notice.load(Ordering::Acquire) {
        return;
    }
    let now_ms = chrono::Utc::now().timestamp_millis();
    let prev = profile.last_input_warn_ms.load(Ordering::Relaxed);
    if now_ms - prev < 5000 {
        return;
    }
    profile.last_input_warn_ms.store(now_ms, Ordering::Relaxed);
    let chan = channel_rx.clone();
    tokio::spawn(async move {
        let _ = chan
            .send(&protocol::Message::HostNotice {
                level: "warn".to_string(),
                message: format!("被控端键鼠注入受限（请确认被控端以管理员运行且未锁屏/断开RDP）: {}", err_text),
            })
            .await;
    });
}

async fn handle_host_incoming_message(
    msg: protocol::Message,
    channel_rx: &transport::ConnectionChannel,
    input_sim_rx: &Option<Arc<input::InputSimulator>>,
    transfer_receivers_rx: &Arc<tokio::sync::Mutex<std::collections::HashMap<String, file_transfer::FileTransferReceiver>>>,
    profile: &Arc<StreamProfile>,
) {
    match msg {
        protocol::Message::MouseEvent {
            x,
            y,
            button,
            action,
            delta_x,
            delta_y,
        } => {
            if let Some(ref sim) = input_sim_rx {
                let sw = profile.stream_w.load(Ordering::Relaxed) as i64;
                let sh = profile.stream_h.load(Ordering::Relaxed) as i64;
                // Always read live display size (<100ns) so coordinate mapping stays exact after resolution changes
                let (mx, my) = if let Some((dw, dh)) = sim.screen_size() {
                    if sw > 0 && sh > 0 && dw > 0 && dh > 0 {
                        (
                            ((x as i64 * dw as i64) / sw).clamp(0, dw as i64 - 1) as i32,
                            ((y as i64 * dh as i64) / sh).clamp(0, dh as i64 - 1) as i32,
                        )
                    } else {
                        profile.map_coords(x, y)
                    }
                } else {
                    profile.map_coords(x, y)
                };
                if let Err(e) = sim.handle_mouse_event(&protocol::MouseEvent {
                    x: mx,
                    y: my,
                    button,
                    action,
                    delta_x,
                    delta_y,
                }) {
                    log::warn!("Mouse input error at ({}, {}): {}", mx, my, e);
                    maybe_send_input_warning(channel_rx, profile, e.to_string());
                }
            }
        }
        protocol::Message::KeyEvent { key, action } => {
            if let Some(ref sim) = input_sim_rx {
                if let Err(e) = sim.handle_key_event(&protocol::KeyEvent { key, action }) {
                    log::warn!("Key input error: {}", e);
                    maybe_send_input_warning(channel_rx, profile, e.to_string());
                }
            }
        }
        protocol::Message::TypeText { text } => {
            if let Some(ref sim) = input_sim_rx {
                if let Err(e) = sim.type_text(&text) {
                    log::warn!("TypeText error: {}", e);
                    maybe_send_input_warning(channel_rx, profile, e.to_string());
                }
            }
        }
        protocol::Message::ClipboardUpdate { content } => {
            tokio::spawn(async move {
                let _ = clipboard::ClipboardMonitor::set_text_silent(&content).await;
            });
        }
        protocol::Message::FileTransferStart {
            id,
            filename,
            total_bytes,
            file_hash,
        } => {
            commands::handle_file_start(
                id,
                filename,
                total_bytes,
                file_hash,
                transfer_receivers_rx.clone(),
            )
            .await;
        }
        protocol::Message::FileChunk {
            id,
            offset,
            data,
            is_last,
        } => {
            let chan = channel_rx.clone();
            let prof = Arc::clone(profile);
            commands::handle_file_chunk_with_callback(
                id,
                offset,
                data,
                is_last,
                transfer_receivers_rx.clone(),
                move |count| {
                    if prof.supports_notice.load(Ordering::Acquire) {
                        tokio::spawn(async move {
                            let _ = chan
                                .send(&protocol::Message::HostNotice {
                                    level: "info".to_string(),
                                    message: format!(
                                        "已同步主控端复制的 {} 个文件到远程剪贴板，远程端可直接 Ctrl+V 粘贴",
                                        count
                                    ),
                                })
                                .await;
                        });
                    }
                },
            )
            .await;
        }
        protocol::Message::Ping { timestamp } => {
            // Reply asynchronously so heavy video writes never stall mouse/keyboard reception
            let chan = channel_rx.clone();
            tokio::spawn(async move {
                let _ = chan.send(&protocol::Message::Pong { timestamp }).await;
            });
        }
        protocol::Message::Handshake { client_id, token } => {
            if let Some(tok) = token {
                if tok.contains("cap:cursor") {
                    profile.supports_cursor.store(true, Ordering::Release);
                }
                if tok.contains("cap:notice") {
                    profile.supports_notice.store(true, Ordering::Release);
                }
                if tok.contains("cap:h264") {
                    profile.supports_h264.store(true, Ordering::Release);
                }
            }
            // Update shared host control status with actual controller_id
            let mut st = read_host_control_status();
            if st.active && st.host_pid == std::process::id() {
                if !client_id.is_empty() {
                    st.controller_id = client_id;
                    write_host_control_status(&st);
                }
            }
            // Always force a fresh keyframe upon Handshake so controller never misses Frame 1!
            profile.need_keyframe.store(true, Ordering::Release);
        }
        protocol::Message::QualityConfig {
            quality,
            max_width,
            fps,
        } => {
            log::info!(
                "Applying dynamic stream profile: quality={}, max_width={}, fps={}",
                quality,
                max_width,
                fps
            );
            profile.update(quality, max_width, fps);
        }
        protocol::Message::SetResolution { width, height } => {
            let prof = Arc::clone(profile);
            let chan = channel_rx.clone();
            tokio::spawn(async move {
                if width == 0 || height == 0 {
                    let restored = tokio::task::spawn_blocking(input::restore_display_resolution)
                        .await
                        .ok()
                        .flatten();
                    prof.max_width.store(0, Ordering::Relaxed);
                    prof.quality_backoff.store(0, Ordering::Relaxed);
                    prof.need_keyframe.store(true, Ordering::Release);
                    let notice_msg = restored
                        .as_ref()
                        .map(|r| r.message.clone())
                        .unwrap_or_else(|| "已切换为远程原生分辨率输出".to_string());
                    if prof.supports_notice.load(Ordering::Acquire) {
                        let _ = chan
                            .send(&protocol::Message::HostNotice {
                                level: "info".to_string(),
                                message: notice_msg,
                            })
                            .await;
                    }
                    return;
                }

                let res = tokio::task::spawn_blocking(move || {
                    input::change_display_resolution(width, height)
                })
                .await;

                match res {
                    Ok(change) => {
                        log::info!(
                            "SetResolution({}x{}) -> applied_os={}, msg={}",
                            width,
                            height,
                            change.applied_os,
                            change.message
                        );
                        if change.applied_os {
                            // Stream the newly applied OS resolution 1:1 natively
                            prof.max_width.store(0, Ordering::Relaxed);
                        } else {
                            // If a larger preset wasn't natively listed while OS was previously downshifted, restore native OS mode first
                            if width > prof.native_w.load(Ordering::Relaxed) {
                                let _ = tokio::task::spawn_blocking(input::restore_display_resolution).await;
                            }
                            // Fallback cleanly to adjusting streaming resolution width
                            prof.max_width.store(width, Ordering::Relaxed);
                        }
                        prof.quality_backoff.store(0, Ordering::Relaxed);
                        prof.need_keyframe.store(true, Ordering::Release);

                        if prof.supports_notice.load(Ordering::Acquire) {
                            let _ = chan
                                .send(&protocol::Message::HostNotice {
                                    level: "info".to_string(),
                                    message: change.message,
                                })
                                .await;
                        }
                    }
                    Err(e) => {
                        log::warn!("SetResolution task failed: {}", e);
                    }
                }
            });
        }
        protocol::Message::DetachRdp => {
            let prof = Arc::clone(profile);
            let chan = channel_rx.clone();
            tokio::spawn(async move {
                let res = tokio::task::spawn_blocking(input::detach_rdp_to_console).await;
                let (level, msg) = match res {
                    Ok(Ok(ok_msg)) => {
                        diag_log(&format!("DetachRdp succeeded: {}", ok_msg));
                        prof.need_keyframe.store(true, Ordering::Release);
                        ("info".to_string(), ok_msg)
                    }
                    Ok(Err(err)) => {
                        diag_log(&format!("DetachRdp failed: {}", err));
                        ("warn".to_string(), err.to_string())
                    }
                    Err(e) => ("warn".to_string(), format!("切换任务异常: {}", e)),
                };
                if prof.supports_notice.load(Ordering::Acquire) {
                    let _ = chan
                        .send(&protocol::Message::HostNotice {
                            level,
                            message: msg,
                        })
                        .await;
                }
            });
        }
        protocol::Message::SendSas => {
            let prof = Arc::clone(profile);
            let chan = channel_rx.clone();
            tokio::spawn(async move {
                let res = tokio::task::spawn_blocking(input::send_sas_ctrl_alt_del).await;
                match res {
                    Ok(Ok(())) => {
                        diag_log("Native SendSAS (Ctrl+Alt+Del) triggered successfully");
                    }
                    Ok(Err(e)) => {
                        diag_log(&format!("SendSAS unavailable: {}", e));
                        if prof.supports_notice.load(Ordering::Acquire) {
                            let _ = chan
                                .send(&protocol::Message::HostNotice {
                                    level: "warn".to_string(),
                                    message: "原生 Ctrl+Alt+Del 需要安装并启动 OpenRemote SYSTEM 系统服务".to_string(),
                                })
                                .await;
                        }
                    }
                    Err(_) => {}
                }
            });
        }
        protocol::Message::ListDirReq { request_id, path } => {
            let chan = channel_rx.clone();
            tokio::spawn(async move {
                let listing = tokio::task::spawn_blocking(move || {
                    commands::read_local_dir_entries(path.as_deref())
                })
                .await
                .unwrap_or_else(|e| commands::DirListingResult {
                    current_path: String::new(),
                    parent_path: None,
                    entries: Vec::new(),
                    error: Some(format!("读取目录失败: {}", e)),
                });
                let _ = chan
                    .send(&protocol::Message::ListDirResp {
                        request_id,
                        current_path: listing.current_path,
                        parent_path: listing.parent_path,
                        entries: listing.entries,
                        error: listing.error,
                    })
                    .await;
            });
        }
        protocol::Message::PullFileReq { path } => {
            let chan = channel_rx.clone();
            tokio::spawn(async move {
                let tid = uuid::Uuid::new_v4().to_string();
                commands::stream_file_to_channel(path, tid, chan, None).await;
            });
        }
        protocol::Message::VideoFrameAck { timestamp } => {
            profile.ack_active.store(true, Ordering::Release);
            profile.last_acked_frame_ts.store(timestamp, Ordering::Release);
            let last_sent = profile.last_sent_frame_ts.load(Ordering::Acquire);
            if timestamp >= last_sent {
                // Queue drained: clear oldest unacked timestamp so idle gaps never fake congestion
                profile.first_unacked_sent_ms.store(0, Ordering::Release);
            } else {
                profile
                    .first_unacked_sent_ms
                    .store((timestamp + 16).min(last_sent), Ordering::Release);
            }
        }
        _ => {}
    }
}

/// Pipelined screen capture + parallel encode (dedicated OS thread) -> bounded async network sender.
/// Completely eliminates Tokio worker thread starvation and 640ms WouldBlock stalls.
fn spawn_screen_stream_task(
    stream_channel: transport::ConnectionChannel,
    session_active: Arc<AtomicBool>,
    profile: Arc<StreamProfile>,
) -> tokio::task::JoinHandle<()> {
    // Bounded capacity = 1: overlaps frame N+1 capture/encode with frame N send
    // while keeping host-side queuing latency strictly <= 1 frame!
    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::channel::<protocol::Message>(1);

    let capture_active = Arc::clone(&session_active);
    let capture_profile = Arc::clone(&profile);

    let _ = std::thread::Builder::new()
        .name("openremote-capture".to_string())
        .spawn(move || {
            log::info!("Starting dedicated high-FPS screen capture thread...");

            while capture_active.load(Ordering::Acquire) {
                let mut capturer = match capture::create_capturer() {
                    Ok(c) => c,
                    Err(e) => {
                        log::warn!(
                            "Screen capturer standby (temporary desktop unavailable: {}). Sending keepalive...",
                            e
                        );
                        let (w, h) = (1280u32, 720u32);
                        let standby_rgb = vec![32u8; (w * h * 3) as usize];
                        let encoder = codec::FrameEncoder::new(45);
                        if let Ok(jpeg) = encoder.encode(w, h, &standby_rgb) {
                            let msg = protocol::Message::ScreenFrame {
                                data: jpeg,
                                width: w,
                                height: h,
                                timestamp: chrono::Utc::now().timestamp_millis(),
                            };
                            if frame_tx.blocking_send(msg).is_err() {
                                break;
                            }
                        }
                        std::thread::sleep(std::time::Duration::from_millis(400));
                        continue;
                    }
                };

                log::info!("DXGI/GDI capturer initialized on dedicated thread (parallel tile/strip pipeline).");
                let (init_w, init_h) = capturer.resolution();
                diag_log(&format!("Host capturer initialized: {}x{}", init_w, init_h));
                capture_profile.need_keyframe.store(true, Ordering::Release);
                let mut sent_any_frame = false;
                let mut last_cap_err_at: Option<std::time::Instant> = None;
                let mut h264_encoder: Option<codec::h264::H264Encoder> = None;
                let mut h264_failed = false;
                let mut stat_win_start = std::time::Instant::now();
                let mut stat_frames: u32 = 0;
                let mut stat_enc_ms_sum: f64 = 0.0;
                let mut stat_enc_ms_max: f64 = 0.0;
                let mut stat_bytes: u64 = 0;
                let mut stat_ack_pauses: u32 = 0;
                let mut last_backoff_recover = std::time::Instant::now();
                let mut last_ack_backoff_at = std::time::Instant::now();
                let mut last_congested_probe_at = std::time::Instant::now();

                while capture_active.load(Ordering::Acquire) {
                    let frame_start = std::time::Instant::now();
                    let cur_quality = capture_profile.effective_quality();
                    let cur_max_width = capture_profile.effective_max_width();
                    let cur_fps = capture_profile.fps.load(Ordering::Relaxed).max(1) as u64;
                    let force_kf = capture_profile.need_keyframe.load(Ordering::Acquire);
                    let use_h264 = capture_profile.supports_h264.load(Ordering::Acquire) && !h264_failed;

                    let report_cap_err = |cap_err: &str, last_at: &mut Option<std::time::Instant>| {
                        let rdp_st = input::rdp_session_status();
                        let should_notify = match *last_at {
                            None => true,
                            Some(t) => t.elapsed() >= std::time::Duration::from_secs(5),
                        };
                        if should_notify {
                            *last_at = Some(std::time::Instant::now());
                            diag_log(&format!(
                                "Host capture unavailable (sid={}, rdp={}, disc={}): {}",
                                rdp_st.session_id, rdp_st.is_rdp, rdp_st.is_disconnected, cap_err
                            ));
                            if capture_profile.supports_notice.load(Ordering::Acquire) {
                                let msg_text = if rdp_st.is_rdp || rdp_st.is_disconnected {
                                    format!(
                                        "被控端当前正处于 Windows 远程桌面 (RDP #{}) 且窗口被最小化/断开 ({})，恢复 RDP 窗口或手动点击「一键切回本地桌面」即可恢复画面",
                                        rdp_st.session_id, cap_err
                                    )
                                } else {
                                    format!(
                                        "被控端当前无法抓取画面（{}），请检查显示器是否已息屏或唤醒桌面",
                                        cap_err
                                    )
                                };
                                let notice = protocol::Message::HostNotice {
                                    level: "warn".to_string(),
                                    message: msg_text,
                                };
                                let _ = frame_tx.try_send(notice);
                            }
                        }
                    };

                    if use_h264 {
                        // Prevent Relay/slow-link bufferbloat without ever choking normal WAN jitter or stalling:
                        // allow 750ms inflight budget on Relay, reset stale >3.5s unacked markers, and step quality gently.
                        if !force_kf && capture_profile.ack_active.load(Ordering::Acquire) {
                            let oldest_unacked = capture_profile.first_unacked_sent_ms.load(Ordering::Acquire);
                            let now_ms = chrono::Utc::now().timestamp_millis();
                            let max_inflight_ms = if capture_profile.is_relay { 750 } else { 220 };
                            let inflight_age = if oldest_unacked > 0 { now_ms - oldest_unacked } else { 0 };
                            if oldest_unacked > 0 && inflight_age > 3500 {
                                // Clear stale unacked timestamp (e.g. after BitBlt/Winlogon desktop pause) so capture never deadlocks
                                capture_profile.first_unacked_sent_ms.store(0, Ordering::Release);
                                capture_profile.quality_backoff.store(-8, Ordering::Relaxed);
                                capture_profile.need_keyframe.store(true, Ordering::Release);
                            } else if oldest_unacked > 0 && inflight_age > max_inflight_ms {
                                let floor_b = if capture_profile.is_relay && inflight_age < 1600 { -16 } else { -26 };
                                if last_ack_backoff_at.elapsed() >= std::time::Duration::from_millis(350) {
                                    let cur_b = capture_profile.quality_backoff.load(Ordering::Relaxed);
                                    if cur_b > floor_b {
                                        capture_profile.quality_backoff.store((cur_b - 4).max(floor_b), Ordering::Relaxed);
                                    }
                                    last_ack_backoff_at = std::time::Instant::now();
                                }
                                let pause_ms = if capture_profile.is_relay { 120 } else { 200 };
                                if last_congested_probe_at.elapsed() < std::time::Duration::from_millis(pause_ms) {
                                    stat_ack_pauses += 1;
                                    std::thread::sleep(std::time::Duration::from_millis(15));
                                    continue;
                                }
                                last_congested_probe_at = std::time::Instant::now();
                            } else if inflight_age < 260 && last_backoff_recover.elapsed() >= std::time::Duration::from_millis(700) {
                                let cur_b = capture_profile.quality_backoff.load(Ordering::Relaxed);
                                if cur_b < 0 {
                                    capture_profile.quality_backoff.store((cur_b + 3).min(0), Ordering::Relaxed);
                                }
                                last_backoff_recover = std::time::Instant::now();
                            }
                        }
                        match capturer.capture_nv12(cur_max_width, force_kf) {
                            Ok(nv12) => {
                                capture_profile.native_w.store(nv12.native_w, Ordering::Relaxed);
                                capture_profile.native_h.store(nv12.native_h, Ordering::Relaxed);
                                if nv12.out_w > 0 && nv12.out_h > 0 {
                                    capture_profile.stream_w.store(nv12.out_w as u32, Ordering::Relaxed);
                                    capture_profile.stream_h.store(nv12.out_h as u32, Ordering::Relaxed);
                                }

                                if !nv12.nv12.is_empty() {
                                    let bitrate_bps = if capture_profile.is_relay {
                                        (cur_quality as u32) * 22_000
                                    } else {
                                        (cur_quality as u32) * 120_000
                                    };
                                    let enc_t0 = std::time::Instant::now();
                                    let enc_res = (|| -> anyhow::Result<(Vec<u8>, bool, &'static str)> {
                                        if h264_encoder.is_none() {
                                            h264_encoder = Some(codec::h264::H264Encoder::new(
                                                nv12.out_w as u32,
                                                nv12.out_h as u32,
                                                cur_fps as u32,
                                                bitrate_bps,
                                            )?);
                                        }
                                        let enc = h264_encoder.as_mut().unwrap();
                                        let (data, is_key) = enc.encode_nv12(
                                            nv12.out_w as u32,
                                            nv12.out_h as u32,
                                            cur_fps as u32,
                                            bitrate_bps,
                                            &nv12.nv12,
                                            force_kf,
                                        )?;
                                        Ok((data, is_key, enc.backend_name()))
                                    })();

                                    match enc_res {
                                        Ok((data, is_key, enc_backend)) => {
                                            let enc_ms = enc_t0.elapsed().as_secs_f64() * 1000.0;
                                            if !data.is_empty() {
                                                stat_frames += 1;
                                                stat_bytes += data.len() as u64;
                                                stat_enc_ms_sum += enc_ms;
                                                if enc_ms > stat_enc_ms_max {
                                                    stat_enc_ms_max = enc_ms;
                                                }
                                                if !sent_any_frame {
                                                    sent_any_frame = true;
                                                    diag_log(&format!(
                                                        "Host H.264 first frame ready ({} + {}): {}x{} -> {}x{}, bytes={}",
                                                        nv12.backend,
                                                        enc_backend,
                                                        nv12.native_w,
                                                        nv12.native_h,
                                                        nv12.out_w,
                                                        nv12.out_h,
                                                        data.len()
                                                    ));
                                                }
                                                last_cap_err_at = None;
                                                if force_kf {
                                                    capture_profile.need_keyframe.store(false, Ordering::Release);
                                                }
                                                let ts = chrono::Utc::now().timestamp_millis();
                                                capture_profile.last_sent_frame_ts.store(ts, Ordering::Release);
                                                let _ = capture_profile.first_unacked_sent_ms.compare_exchange(
                                                    0,
                                                    ts,
                                                    Ordering::AcqRel,
                                                    Ordering::Relaxed,
                                                );
                                                let msg = protocol::Message::VideoFrameH264 {
                                                    data,
                                                    width: nv12.out_w,
                                                    height: nv12.out_h,
                                                    is_keyframe: is_key || force_kf,
                                                    timestamp: ts,
                                                };
                                                if frame_tx.blocking_send(msg).is_err() {
                                                    capture_active.store(false, Ordering::Release);
                                                    return;
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            diag_log(&format!(
                                                "H.264 encoder failed ({}), falling back to dirty-tile JPEG",
                                                e
                                            ));
                                            h264_failed = true;
                                            h264_encoder = None;
                                        }
                                    }
                                } else if nv12.would_block {
                                    if let Some(ref cap_err) = nv12.capture_error {
                                        report_cap_err(cap_err, &mut last_cap_err_at);
                                    }
                                    std::thread::sleep(std::time::Duration::from_millis(2));
                                    continue;
                                }
                            }
                            Err(e) => {
                                log::warn!("H.264 frame capture failed ({}), reinitializing capturer in 80ms...", e);
                                std::thread::sleep(std::time::Duration::from_millis(80));
                                break;
                            }
                        }

                        if stat_win_start.elapsed() >= std::time::Duration::from_secs(3) {
                            let secs = stat_win_start.elapsed().as_secs_f64().max(0.1);
                            let avg_enc = if stat_frames > 0 { stat_enc_ms_sum / stat_frames as f64 } else { 0.0 };
                            let kbps = ((stat_bytes * 8) as f64 / (secs * 1000.0)).round() as u64;
                            let oldest_unacked = capture_profile.first_unacked_sent_ms.load(Ordering::Relaxed);
                            let inflight = if oldest_unacked > 0 {
                                (chrono::Utc::now().timestamp_millis() - oldest_unacked).max(0)
                            } else {
                                0
                            };
                            diag_log(&format!(
                                "[HostStream] mode={} fps={:.1} kbps={} avg_enc={:.1}ms max_enc={:.1}ms inflight={}ms ack_pauses={} q={}(backoff={})",
                                if capture_profile.is_relay { "Relay" } else { "QUIC" },
                                stat_frames as f64 / secs,
                                kbps,
                                avg_enc,
                                stat_enc_ms_max,
                                inflight,
                                stat_ack_pauses,
                                cur_quality,
                                capture_profile.quality_backoff.load(Ordering::Relaxed),
                            ));
                            stat_win_start = std::time::Instant::now();
                            stat_frames = 0;
                            stat_enc_ms_sum = 0.0;
                            stat_enc_ms_max = 0.0;
                            stat_bytes = 0;
                            stat_ack_pauses = 0;
                        }
                    } else {
                        match capturer.capture_dirty_tiles(cur_max_width, cur_quality, force_kf) {
                            Ok(batch) => {
                                capture_profile.native_w.store(batch.native_w, Ordering::Relaxed);
                                capture_profile.native_h.store(batch.native_h, Ordering::Relaxed);
                                if batch.out_w > 0 && batch.out_h > 0 {
                                    capture_profile.stream_w.store(batch.out_w as u32, Ordering::Relaxed);
                                    capture_profile.stream_h.store(batch.out_h as u32, Ordering::Relaxed);
                                }

                                if !batch.tiles.is_empty() {
                                    if !sent_any_frame {
                                        sent_any_frame = true;
                                        diag_log(&format!(
                                            "Host first frame ready: {}x{} -> {}x{}, tiles={}",
                                            batch.native_w,
                                            batch.native_h,
                                            batch.out_w,
                                            batch.out_h,
                                            batch.tiles.len()
                                        ));
                                    }
                                    last_cap_err_at = None;
                                    if force_kf {
                                        capture_profile.need_keyframe.store(false, Ordering::Release);
                                    }

                                    let msg = protocol::Message::TileFrames {
                                        screen_w: batch.out_w,
                                        screen_h: batch.out_h,
                                        tiles: batch.tiles,
                                        timestamp: chrono::Utc::now().timestamp_millis(),
                                    };
                                    if frame_tx.blocking_send(msg).is_err() {
                                        capture_active.store(false, Ordering::Release);
                                        return;
                                    }
                                } else if batch.would_block {
                                    if let Some(ref cap_err) = batch.capture_error {
                                        report_cap_err(cap_err, &mut last_cap_err_at);
                                    }
                                    std::thread::sleep(std::time::Duration::from_millis(2));
                                    continue;
                                }
                            }
                            Err(e) => {
                                log::warn!("Frame capture failed ({}), reinitializing capturer in 80ms...", e);
                                std::thread::sleep(std::time::Duration::from_millis(80));
                                break;
                            }
                        }
                    }

                    let elapsed = frame_start.elapsed();
                    let target = std::time::Duration::from_millis(1000 / cur_fps);
                    if elapsed < target {
                        std::thread::sleep(target - elapsed);
                    }
                }
            }
            log::info!("Dedicated screen capture thread exited cleanly.");
        });

    // Async network sender task with lightweight adaptive quality feedback (ABR) + real-time cursor shape sync
    tokio::spawn(async move {
        let mut ema_send_ms: f64 = 5.0;
        let mut cursor_tick = tokio::time::interval(std::time::Duration::from_millis(30));
        let mut last_cursor: &'static str = "";

        while session_active.load(Ordering::Acquire) {
            tokio::select! {
                biased;
                _ = cursor_tick.tick() => {
                    if profile.supports_cursor.load(Ordering::Acquire) {
                        let cur = input::current_cursor_css();
                        if cur != last_cursor {
                            last_cursor = cur;
                            let msg = protocol::Message::CursorShape {
                                cursor: cur.to_string(),
                            };
                            if stream_channel.send(&msg).await.is_err() {
                                session_active.store(false, Ordering::Release);
                                break;
                            }
                        }
                    }
                }
                maybe_msg = frame_rx.recv() => {
                    match maybe_msg {
                        Some(msg) => {
                            let send_start = std::time::Instant::now();
                            if let Err(e) = stream_channel.send(&msg).await {
                                log::info!("Stream channel closed: {}", e);
                                session_active.store(false, Ordering::Release);
                                break;
                            }
                            let send_ms = send_start.elapsed().as_secs_f64() * 1000.0;
                            ema_send_ms = ema_send_ms * 0.7 + send_ms * 0.3;

                            // Adaptive Bitrate (ABR): smoothly dial back JPEG quality if network send lags,
                            // and quickly recover once the link catches up.
                            let cur_backoff = profile.quality_backoff.load(Ordering::Relaxed);
                            let threshold_hi = if profile.is_relay { 35.0 } else { 22.0 };
                            let threshold_lo = if profile.is_relay { 14.0 } else { 8.0 };

                            if ema_send_ms > threshold_hi && cur_backoff > -24 {
                                profile
                                    .quality_backoff
                                    .store((cur_backoff - 4).max(-24), Ordering::Relaxed);
                            } else if ema_send_ms < threshold_lo
                                && cur_backoff < 0
                                && profile.first_unacked_sent_ms.load(Ordering::Relaxed) == 0
                            {
                                profile
                                    .quality_backoff
                                    .store((cur_backoff + 2).min(0), Ordering::Relaxed);
                            }
                        }
                        None => break,
                    }
                }
            }
        }
        session_active.store(false, Ordering::Release);
    })
}

struct HostSessionHandle {
    active: Arc<AtomicBool>,
    channel: transport::ConnectionChannel,
    sender_task: tokio::task::JoinHandle<()>,
}

impl HostSessionHandle {
    async fn stop(self) {
        self.active.store(false, Ordering::Release);
        self.sender_task.abort();
        self.channel.close().await;
    }
}

fn spawn_disconnect_watcher(
    session_active: Arc<AtomicBool>,
    channel: transport::ConnectionChannel,
) -> tokio::task::JoinHandle<()> {
    let _ = std::fs::remove_file(host_disconnect_req_path());
    tokio::spawn(async move {
        let req_path = host_disconnect_req_path();
        while session_active.load(Ordering::Acquire) {
            if req_path.exists() {
                let _ = std::fs::remove_file(&req_path);
                diag_log("Controlled host user clicked Disconnect -> closing incoming session.");
                session_active.store(false, Ordering::Release);
                channel.close().await;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
    })
}

fn spawn_quic_accept_loop(
    server_arc: Arc<QuicServer>,
    is_relay_endpoint: bool,
    state_channel: Arc<tokio::sync::Mutex<Option<transport::ConnectionChannel>>>,
    active_host_session: Arc<tokio::sync::Mutex<Option<HostSessionHandle>>>,
    input_sim: Arc<tokio::sync::Mutex<Option<Arc<input::InputSimulator>>>>,
    transfer_receivers: Arc<tokio::sync::Mutex<std::collections::HashMap<String, file_transfer::FileTransferReceiver>>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match server_arc.accept_incoming().await {
                Ok(incoming) => {
                    let state_channel = state_channel.clone();
                    let active_host_session = Arc::clone(&active_host_session);
                    let input_sim = input_sim.clone();
                    let transfer_receivers = transfer_receivers.clone();

                    // Spawn handshake per incoming connection so a stalled peer NEVER blocks future reconnects!
                    tokio::spawn(async move {
                        let channel = match QuicServer::finish_incoming(incoming).await {
                            Ok(ch) => ch,
                            Err(e) => {
                                diag_log(&format!("Host QUIC handshake failed: {}", e));
                                return;
                            }
                        };

                        diag_log(&format!(
                            "Host accepted {} session from {}",
                            if is_relay_endpoint { "UDP-QUIC Relay" } else { "QUIC" },
                            channel.peer_addr_label()
                        ));
                        let _ = &state_channel; // Keep AppState::active_channel strictly owned by outgoing controller sessions

                        // Immediately stop & close any previous host session so DXGI & channels release in <5ms
                        let prev_handle = active_host_session.lock().await.take();
                        if let Some(prev) = prev_handle {
                            prev.stop().await;
                            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        }

                        let session_active = Arc::new(AtomicBool::new(true));
                        let profile = StreamProfile::new(is_relay_endpoint);
                        let transport_mode = if is_relay_endpoint {
                            "UDP-QUIC 中继"
                        } else if channel.peer_addr_label().starts_with('[') {
                            "IPv6 直连"
                        } else {
                            "P2P 打洞直连"
                        };
                        write_host_control_status(&HostControlStatus {
                            active: true,
                            host_pid: std::process::id(),
                            controller_id: "远程控制端".to_string(),
                            transport_mode: transport_mode.to_string(),
                            remote_addr: channel.peer_addr_label().to_string(),
                            connected_at_ms: chrono::Utc::now().timestamp_millis(),
                        });
                        let disconnect_watch = spawn_disconnect_watcher(
                            Arc::clone(&session_active),
                            channel.clone(),
                        );
                        let capture_task = spawn_screen_stream_task(
                            channel.clone(),
                            Arc::clone(&session_active),
                            Arc::clone(&profile),
                        );

                        *active_host_session.lock().await = Some(HostSessionHandle {
                            active: Arc::clone(&session_active),
                            channel: channel.clone(),
                            sender_task: capture_task,
                        });

                        // Start clipboard sync on host for direct QUIC session
                        let mut host_clipboard = clipboard::ClipboardMonitor::new();
                        let rt_cb_text = tokio::runtime::Handle::current();
                        let rt_cb_files = rt_cb_text.clone();
                        let channel_cb_text = channel.clone();
                        let channel_cb_files = channel.clone();
                        host_clipboard.start_monitoring_dual(
                            move |text| {
                                let chan = channel_cb_text.clone();
                                rt_cb_text.spawn(async move {
                                    let msg = protocol::Message::ClipboardUpdate { content: text };
                                    let _ = chan.send(&msg).await;
                                });
                            },
                            move |files| {
                                let chan = channel_cb_files.clone();
                                rt_cb_files.spawn(async move {
                                    commands::send_clipboard_files_over_channel(files, chan).await;
                                });
                            },
                        );

                        let channel_rx = channel.clone();
                        let input_sim_rx = input_sim.lock().await.clone();
                        let transfer_receivers_rx = transfer_receivers.clone();

                        loop {
                            match channel_rx.recv().await {
                                Ok(msg) => {
                                    handle_host_incoming_message(
                                        msg,
                                        &channel_rx,
                                        &input_sim_rx,
                                        &transfer_receivers_rx,
                                        &profile,
                                    )
                                    .await;
                                }
                                Err(e) => {
                                    log::info!("QUIC client disconnected: {}", e);
                                    break;
                                }
                            }
                        }

                        session_active.store(false, Ordering::Release);
                        disconnect_watch.abort();
                        host_clipboard.stop_monitoring();
                        channel_rx.close().await;
                        // If we are still the registered active session, abort our sender task & clear it
                        let mut guard = active_host_session.lock().await;
                        if let Some(ref cur) = *guard {
                            if Arc::ptr_eq(&cur.active, &session_active) {
                                if let Some(handle) = guard.take() {
                                    handle.sender_task.abort();
                                }
                                clear_host_control_status();
                                let _ = tokio::task::spawn_blocking(input::restore_display_resolution).await;
                            }
                        }
                    });
                }
                Err(e) => {
                    log::warn!("QUIC endpoint accept ended: {}", e);
                    break;
                }
            }
        }
    })
}

/// Handle to a running host daemon (can be stopped cleanly when switching to OpenRemoteService)
pub struct HostDaemonHandle {
    stop_flag: Arc<AtomicBool>,
    tasks: Arc<tokio::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    active_host_session: Arc<tokio::sync::Mutex<Option<HostSessionHandle>>>,
    quic_endpoints: Arc<tokio::sync::Mutex<Vec<Arc<QuicServer>>>>,
}

impl HostDaemonHandle {
    pub async fn stop(self) {
        self.stop_flag.store(true, Ordering::Release);
        if let Some(session) = self.active_host_session.lock().await.take() {
            session.stop().await;
        }
        {
            let mut eps = self.quic_endpoints.lock().await;
            for ep in eps.drain(..) {
                ep.endpoint.close(0u32.into(), b"host_shutdown");
            }
        }
        {
            let mut tasks = self.tasks.lock().await;
            for t in tasks.drain(..) {
                t.abort();
            }
        }
    }
}

fn init_windows_host_runtime() {
    #[cfg(target_os = "windows")]
    unsafe {
        #[link(name = "user32")]
        extern "system" {
            fn SetProcessDPIAware() -> i32;
        }
        #[link(name = "winmm")]
        extern "system" {
            fn timeBeginPeriod(uPeriod: u32) -> u32;
        }
        SetProcessDPIAware();
        // Enable 1ms Windows system timer resolution (eliminates 15.6ms sleep quantization)
        timeBeginPeriod(1);
    }
    nat::ensure_windows_firewall_rule();
}

/// Spawn the complete host daemon (dual-stack QUIC + STUN + UPnP + signaling + UDP punch + TCP Relay).
/// Used identically by both the headless LocalSystem `--host-worker` AND the GUI fallback host!
pub fn spawn_host_daemon(
    config: AppConfig,
    bg_state_channel: Arc<tokio::sync::Mutex<Option<transport::ConnectionChannel>>>,
    bg_input_sim: Arc<tokio::sync::Mutex<Option<Arc<input::InputSimulator>>>>,
    bg_transfer_receivers: Arc<tokio::sync::Mutex<std::collections::HashMap<String, file_transfer::FileTransferReceiver>>>,
    bg_quic_server: Arc<tokio::sync::Mutex<Option<Arc<QuicServer>>>>,
) -> HostDaemonHandle {
    let stop_flag = Arc::new(AtomicBool::new(false));
    let tasks: Arc<tokio::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>> =
        Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let active_host_session: Arc<tokio::sync::Mutex<Option<HostSessionHandle>>> =
        Arc::new(tokio::sync::Mutex::new(None));
    let quic_endpoints: Arc<tokio::sync::Mutex<Vec<Arc<QuicServer>>>> =
        Arc::new(tokio::sync::Mutex::new(Vec::new()));

    let stop_main = Arc::clone(&stop_flag);
    let tasks_main = Arc::clone(&tasks);
    let active_session_main = Arc::clone(&active_host_session);
    let endpoints_main = Arc::clone(&quic_endpoints);

    let initial_port = if config.listen_port > 0 { config.listen_port } else { 28843 };
    let (signal_url, d_id, d_code, d_name, auth_token_cfg) = (
        config.signal_server_url.clone(),
        config.device_id.clone(),
        config.device_code.clone(),
        config.device_name.clone(),
        config.auth_token.clone(),
    );

    let main_task = tokio::spawn(async move {
        let mut bound_v4: Option<QuicServer> = None;
        let mut bound_v6: Option<QuicServer> = None;
        let mut punch_socket: Option<std::net::UdpSocket> = None;
        let mut stun_endpoint: Option<(String, u16)> = None;
        let mut actual_port = initial_port;

        let sig_port = signaling::parse_server_port(&signal_url);
        let mut candidate_ports: Vec<u16> = Vec::with_capacity(52);
        if initial_port == sig_port {
            diag_log(&format!(
                "Host port note: listen_port={} matches signaling port {}; preferring 28843 first to avoid router NAT conflict",
                initial_port, sig_port
            ));
            if sig_port != 28843 {
                candidate_ports.push(28843);
            }
        }
        for offset in 0..50u16 {
            let p = initial_port.wrapping_add(offset);
            if p != sig_port && !candidate_ports.contains(&p) {
                candidate_ports.push(p);
            }
        }
        if !candidate_ports.contains(&initial_port) {
            candidate_ports.push(initial_port);
        }

        for try_port in candidate_ports {
            if stop_main.load(Ordering::Acquire) {
                return;
            }
            if let Ok(udp_v4) = nat::bind_udp_direct(try_port) {
                let stun_diag = nat::probe_stun_detailed_direct(&udp_v4, &signal_url).await;
                stun_endpoint = stun_diag.best_addr();
                punch_socket = udp_v4.try_clone().ok();
                if let Ok(srv4) = QuicServer::bind_with_socket(udp_v4) {
                    log::info!("Bound direct IPv4 QUIC server on {} (STUN: {:?})", srv4.listen_addr, stun_endpoint);
                    diag_log(&format!(
                        "Host NAT/STUN: {} | rfc={:?} custom={:?} | local={} | system={}",
                        stun_diag.nat_type,
                        stun_diag.rfc_mapped,
                        stun_diag.custom_mapped,
                        srv4.listen_addr,
                        service::is_running_as_system()
                    ));
                    actual_port = try_port;
                    bound_v4 = Some(srv4);
                    break;
                }
            }
        }

        if bound_v4.is_none() && !stop_main.load(Ordering::Acquire) {
            if let Ok(udp_v4) = nat::bind_udp_direct(0) {
                stun_endpoint = nat::probe_stun_direct(&udp_v4, &signal_url).await;
                punch_socket = udp_v4.try_clone().ok();
                if let Ok(srv4) = QuicServer::bind_with_socket(udp_v4) {
                    actual_port = srv4.listen_addr.port();
                    bound_v4 = Some(srv4);
                }
            }
        }

        if !stop_main.load(Ordering::Acquire) {
            if let Ok(addr_v6) = format!("[::]:{}", actual_port).parse::<SocketAddr>() {
                if let Ok(srv6) = QuicServer::bind(addr_v6) {
                    log::info!("Bound IPv6 QUIC server on {}", srv6.listen_addr);
                    bound_v6 = Some(srv6);
                }
            }
        }

        if stop_main.load(Ordering::Acquire) {
            return;
        }

        if let Some(srv4) = bound_v4 {
            let srv4_arc = Arc::new(srv4);
            endpoints_main.lock().await.push(Arc::clone(&srv4_arc));
            *bg_quic_server.lock().await = Some(Arc::clone(&srv4_arc));
            let h = spawn_quic_accept_loop(
                srv4_arc,
                false,
                bg_state_channel.clone(),
                Arc::clone(&active_session_main),
                bg_input_sim.clone(),
                bg_transfer_receivers.clone(),
            );
            tasks_main.lock().await.push(h);
        }

        if let Some(srv6) = bound_v6 {
            let srv6_arc = Arc::new(srv6);
            endpoints_main.lock().await.push(Arc::clone(&srv6_arc));
            if bg_quic_server.lock().await.is_none() {
                *bg_quic_server.lock().await = Some(Arc::clone(&srv6_arc));
            }
            let h = spawn_quic_accept_loop(
                srv6_arc,
                false,
                bg_state_channel.clone(),
                Arc::clone(&active_session_main),
                bg_input_sim.clone(),
                bg_transfer_receivers.clone(),
            );
            tasks_main.lock().await.push(h);
        }

        // Start persistent UDP-QUIC Relay server + datagram bridge (zero TCP HOL blocking across WAN!)
        let relay_addr_quic = crate::signaling::parse_relay_address(&signal_url);
        match transport::quic_relay::create_host_quic_relay_server(
            relay_addr_quic.clone(),
            d_code.clone(),
            Arc::clone(&stop_main),
        ) {
            Ok((srv_relay, bridge_task)) => {
                endpoints_main.lock().await.push(Arc::clone(&srv_relay));
                let accept_h = spawn_quic_accept_loop(
                    srv_relay,
                    true,
                    bg_state_channel.clone(),
                    Arc::clone(&active_session_main),
                    bg_input_sim.clone(),
                    bg_transfer_receivers.clone(),
                );
                let mut tm = tasks_main.lock().await;
                tm.push(bridge_task);
                tm.push(accept_h);
            }
            Err(e) => {
                diag_log(&format!("Host UDP-QUIC Relay server init failed: {}", e));
            }
        }

        // Auto UPnP router port mapping
        let upnp_ip = crate::signaling::detect_best_local_ip();
        let upnp_task = tokio::spawn(async move {
            let ok = nat::try_upnp_port_mapping(&upnp_ip, actual_port).await;
            diag_log(&format!("Host UPnP IGD map ({}:{}) -> {}", upnp_ip, actual_port, ok));
        });
        tasks_main.lock().await.push(upnp_task);

        // Periodic signaling registration with true STUN public IP:Port
        let signal_url_clone = signal_url.clone();
        let d_code_clone = d_code.clone();
        let stun_clone = stun_endpoint.clone();
        let stop_reg = Arc::clone(&stop_main);
        let reg_task = tokio::spawn(async move {
            let (s_ip, s_port) = match stun_clone {
                Some((ip, port)) => (Some(ip), Some(port)),
                None => (None, None),
            };
            while !stop_reg.load(Ordering::Acquire) {
                let l_ip = crate::signaling::detect_best_local_ip();
                let v6 = crate::signaling::detect_public_ipv6();
                let _ = crate::signaling::register_to_signaling_with_stun(
                    &signal_url_clone,
                    &d_id,
                    &d_code_clone,
                    &d_name,
                    &l_ip,
                    actual_port,
                    v6,
                    s_ip.as_deref(),
                    s_port,
                    auth_token_cfg.as_deref(),
                )
                .await;
                tokio::time::sleep(std::time::Duration::from_secs(20)).await;
            }
        });
        tasks_main.lock().await.push(reg_task);

        // Background NAT keepalive (10s) and fast coordinated UDP hole-punching responder (250ms poll)
        if let Some(punch_sock) = punch_socket {
            let signal_url_punch = signal_url.clone();
            let d_code_punch = d_code.clone();
            let stun_addr_opt = nat::resolve_stun_addr_direct(&signal_url).await;
            let stop_punch = Arc::clone(&stop_main);
            let punch_task = tokio::spawn(async move {
                let mut tick = 0u32;
                while !stop_punch.load(Ordering::Acquire) {
                    if tick % 32 == 0 {
                        if let Some(stun_addr) = stun_addr_opt {
                            let _ = punch_sock.send_to(b"KEEPALIVE", stun_addr);
                        }
                    }
                    let targets = crate::signaling::poll_udp_punches(&signal_url_punch, &d_code_punch).await;
                    if !targets.is_empty() {
                        // Group & deduplicate target ports per peer IP, prioritized closest-first
                        // so overlapping symmetric STUN endpoints never emit duplicate 160-pkt floods.
                        let mut by_ip: std::collections::HashMap<std::net::IpAddr, Vec<u16>> =
                            std::collections::HashMap::new();
                        for addr in targets {
                            by_ip.entry(addr.ip()).or_default().push(addr.port());
                        }
                        for (ip, base_ports) in by_ip {
                            let mut seen = std::collections::HashSet::<u16>::new();
                            let mut ordered_ports = Vec::<u16>::new();
                            let mut push_port = |p_i32: i32| {
                                if (1024..=65535).contains(&p_i32) {
                                    let p = p_i32 as u16;
                                    if seen.insert(p) {
                                        ordered_ports.push(p);
                                    }
                                }
                            };
                            // Tier 1: exact reported ports first
                            for &bp in &base_ports {
                                push_port(bp as i32);
                            }
                            // Tier 2: interleaved small offsets [-4..-1, +1..+24] around each base port
                            for step in 1i32..=24i32 {
                                for &bp in &base_ports {
                                    push_port(bp as i32 + step);
                                    if step <= 4 {
                                        push_port(bp as i32 - step);
                                    }
                                }
                            }
                            // Tier 3: if bases are within <=72 ports (e.g. [2058, 2096]), bridge the entire gap continuously
                            // so pooled Symmetric NAT never lands in an unprobed gap between rfc and custom ports!
                            if let (Some(&min_p), Some(&max_p)) = (base_ports.iter().min(), base_ports.iter().max()) {
                                if max_p.saturating_sub(min_p) <= 72 {
                                    for p in (min_p as i32 - 12)..=(max_p as i32 + 36) {
                                        push_port(p);
                                    }
                                }
                            }
                            // Tier 4: wider tail [+25..+44, -20..-5]
                            for step in 5i32..=44i32 {
                                for &bp in &base_ports {
                                    if step >= 25 {
                                        push_port(bp as i32 + step);
                                    }
                                    if step <= 20 {
                                        push_port(bp as i32 - step);
                                    }
                                }
                            }
                            let port_count = ordered_ports.len();
                            diag_log(&format!(
                                "[HostPunch] received punch request -> peer={} bases={:?} -> 18x110ms paced fan ({} unique ports)",
                                ip, base_ports, port_count
                            ));
                            if let Ok(sock) = punch_sock.try_clone() {
                                tokio::spawn(async move {
                                    for round in 0..18 {
                                        let limit = if round < 12 {
                                            ordered_ports.len().min(115)
                                        } else {
                                            ordered_ports.len().min(base_ports.len() + 36)
                                        };
                                        for (idx, &port) in ordered_ports[..limit].iter().enumerate() {
                                            let _ = sock.send_to(b"OPRM_PUNCH", SocketAddr::new(ip, port));
                                            if idx % 10 == 9 {
                                                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                                            }
                                        }
                                        tokio::time::sleep(std::time::Duration::from_millis(110)).await;
                                    }
                                });
                            }
                        }
                    }
                    tick = tick.wrapping_add(1);
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            });
            tasks_main.lock().await.push(punch_task);
        }

        // Background continuous TCP Relay listener on dxcool.cn:28845
        let relay_addr = crate::signaling::parse_relay_address(&signal_url);
        while !stop_main.load(Ordering::Acquire) {
            log::info!("Connecting to TCP Relay server at {} to stand by for remote control...", relay_addr);

            match crate::signaling::register_relay_tunnel(&relay_addr, &d_code).await {
                Ok(tcp_stream) => {
                    if stop_main.load(Ordering::Acquire) {
                        break;
                    }
                    diag_log(&format!("Host Relay standby connected ({})", relay_addr));
                    let channel = transport::ConnectionChannel::new_tcp(tcp_stream);

                    match channel.recv().await {
                        Ok(msg) => {
                            diag_log("Host accepted incoming Relay session");
                            let _ = &bg_state_channel;

                            let prev_handle = active_session_main.lock().await.take();
                            if let Some(prev) = prev_handle {
                                prev.stop().await;
                                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                            }

                            let session_active = Arc::new(AtomicBool::new(true));
                            let profile = StreamProfile::new(true);
                            write_host_control_status(&HostControlStatus {
                                active: true,
                                host_pid: std::process::id(),
                                controller_id: "远程控制端".to_string(),
                                transport_mode: "云端中继".to_string(),
                                remote_addr: channel.peer_addr_label().to_string(),
                                connected_at_ms: chrono::Utc::now().timestamp_millis(),
                            });
                            let disconnect_watch = spawn_disconnect_watcher(
                                Arc::clone(&session_active),
                                channel.clone(),
                            );
                            let capture_task = spawn_screen_stream_task(
                                channel.clone(),
                                Arc::clone(&session_active),
                                Arc::clone(&profile),
                            );

                            *active_session_main.lock().await = Some(HostSessionHandle {
                                active: Arc::clone(&session_active),
                                channel: channel.clone(),
                                sender_task: capture_task,
                            });

                            let mut host_clipboard = clipboard::ClipboardMonitor::new();
                            let rt_cb_text = tokio::runtime::Handle::current();
                            let rt_cb_files = rt_cb_text.clone();
                            let channel_cb_text = channel.clone();
                            let channel_cb_files = channel.clone();
                            host_clipboard.start_monitoring_dual(
                                move |text| {
                                    let chan = channel_cb_text.clone();
                                    rt_cb_text.spawn(async move {
                                        let msg = protocol::Message::ClipboardUpdate { content: text };
                                        let _ = chan.send(&msg).await;
                                    });
                                },
                                move |files| {
                                    let chan = channel_cb_files.clone();
                                    rt_cb_files.spawn(async move {
                                        commands::send_clipboard_files_over_channel(files, chan).await;
                                    });
                                },
                            );

                            let channel_rx = channel.clone();
                            let input_sim_rx = bg_input_sim.lock().await.clone();
                            let transfer_receivers_rx = bg_transfer_receivers.clone();

                            handle_host_incoming_message(
                                msg,
                                &channel_rx,
                                &input_sim_rx,
                                &transfer_receivers_rx,
                                &profile,
                            )
                            .await;

                            loop {
                                if stop_main.load(Ordering::Acquire) {
                                    break;
                                }
                                match channel_rx.recv().await {
                                    Ok(msg) => {
                                        handle_host_incoming_message(
                                            msg,
                                            &channel_rx,
                                            &input_sim_rx,
                                            &transfer_receivers_rx,
                                            &profile,
                                        )
                                        .await;
                                    }
                                    Err(e) => {
                                        diag_log(&format!("Host Relay peer session ended: {}", e));
                                        break;
                                    }
                                }
                            }
                            session_active.store(false, Ordering::Release);
                            disconnect_watch.abort();
                            host_clipboard.stop_monitoring();
                            channel_rx.close().await;
                            let mut guard = active_session_main.lock().await;
                            if let Some(ref cur) = *guard {
                                if Arc::ptr_eq(&cur.active, &session_active) {
                                    if let Some(handle) = guard.take() {
                                        handle.sender_task.abort();
                                    }
                                    clear_host_control_status();
                                }
                            }
                        }
                        Err(e) => {
                            diag_log(&format!("Host Relay standby socket disconnected: {}", e));
                        }
                    }
                }
                Err(e) => {
                    diag_log(&format!("Host failed to register on TCP Relay ({}): {}", relay_addr, e));
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
    });

    if let Ok(mut guard) = tasks.try_lock() {
        guard.push(main_task);
    }

    HostDaemonHandle {
        stop_flag,
        tasks,
        active_host_session,
        quic_endpoints,
    }
}

/// Entrypoint for `openremote.exe --service` (Session 0 Windows LocalSystem Service)
pub fn run_service() {
    let _ = env_logger::try_init();
    if let Err(e) = service::run_windows_service_dispatcher() {
        diag_log(&format!("Windows service dispatcher exited with error: {}", e));
    }
}

/// Entrypoint for `openremote.exe --host-worker` (Headless LocalSystem host daemon inside active user/logon session)
pub fn run_host_worker() {
    let _ = env_logger::try_init();
    init_windows_host_runtime();

    let config = AppConfig::load().unwrap_or_default();
    let rdp_st = input::rdp_session_status();
    diag_log(&format!(
        "SYSTEM host-worker started (code={}, port={}, sid={}, system={})",
        config.device_code,
        config.listen_port,
        rdp_st.session_id,
        service::is_running_as_system()
    ));

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            diag_log(&format!("Host worker Tokio runtime failed: {}", e));
            return;
        }
    };

    rt.block_on(async move {
        let state_channel = Arc::new(tokio::sync::Mutex::new(None));
        let input_sim = Arc::new(tokio::sync::Mutex::new(input::InputSimulator::new().ok().map(Arc::new)));
        let transfer_receivers = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        let quic_server = Arc::new(tokio::sync::Mutex::new(None));

        let _handle = spawn_host_daemon(
            config,
            state_channel,
            input_sim,
            transfer_receivers,
            quic_server,
        );

        // Stay alive until terminated by OpenRemoteService on session switch or stop
        std::future::pending::<()>().await;
    });
}

pub fn run() {
    let _ = env_logger::try_init();
    init_windows_host_runtime();

    let config = AppConfig::load().unwrap_or_default();
    let app_state = AppState::new(config.clone());

    let svc_status = service::query_service_status();
    if svc_status.running {
        diag_log("OpenRemoteService (LocalSystem) is already running; GUI skipping duplicate host sockets.");
    } else {
        let bg_state_channel = app_state.active_channel.clone();
        let bg_input_sim = app_state.input_simulator.clone();
        let bg_transfer_receivers = app_state.transfer_receivers.clone();
        let bg_quic_server = app_state.quic_server.clone();
        let fallback_slot = app_state.fallback_host.clone();

        tauri::async_runtime::spawn(async move {
            let handle = spawn_host_daemon(
                config,
                bg_state_channel,
                bg_input_sim,
                bg_transfer_receivers,
                bg_quic_server,
            );
            *fallback_slot.lock().await = Some(handle);
        });
    }

    tauri::Builder::default()
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
            Ok(())
        })
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::get_device_info,
            commands::start_screen_sharing,
            commands::stop_screen_sharing,
            commands::connect_to_device,
            commands::disconnect,
            commands::send_file,
            commands::get_transfer_progress,
            commands::sync_clipboard,
            commands::wake_device,
            commands::get_config,
            commands::save_config,
            commands::send_mouse_event,
            commands::send_key_event,
            commands::open_download_dir,
            commands::send_shortcut,
            commands::get_auth_status,
            commands::login_account,
            commands::register_account,
            commands::logout_account,
            commands::set_stream_quality,
            commands::set_remote_resolution,
            commands::request_keyframe,
            commands::detach_local_rdp,
            commands::detach_remote_rdp,
            commands::get_service_status,
            commands::install_system_service,
            commands::uninstall_system_service,
            commands::get_host_control_status,
            commands::stop_incoming_control,
            commands::list_local_files,
            commands::list_remote_files,
            commands::download_remote_file,
            commands::sync_native_viewport,
            commands::set_native_input_locked,
            commands::get_diag_logs,
            commands::clear_diag_logs,
            commands::open_diag_log_dir,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
