use crate::capture::create_capturer;
use crate::clipboard::ClipboardMonitor;
use crate::codec::FrameEncoder;
use crate::config::AppConfig;
use crate::file_transfer::{FileTransferSender, TransferProgress};
use crate::input::InputSimulator;
use crate::protocol::Message;
use crate::transport::{ConnectionChannel, QuicClient, QuicServer};
use crate::wol::send_wol_packet;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{Emitter, State};
use tokio::sync::Mutex;
use uuid::Uuid;
use base64::Engine;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub device_id: String,
    pub device_code: String,
    pub access_password: String,
    pub device_name: String,
    pub local_ip: String,
    pub listen_port: u16,
    pub public_ipv6: Option<String>,
}

pub struct AppState {
    pub config: Mutex<AppConfig>,
    pub is_sharing: Arc<AtomicBool>,
    pub active_channel: Arc<Mutex<Option<ConnectionChannel>>>,
    pub quic_server: Arc<Mutex<Option<Arc<QuicServer>>>>,
    pub input_simulator: Arc<Mutex<Option<Arc<InputSimulator>>>>,
    pub clipboard_monitor: Mutex<Option<ClipboardMonitor>>,
    pub transfer_senders: Mutex<HashMap<String, Arc<Mutex<FileTransferSender>>>>,
    pub transfer_progresses: Arc<Mutex<HashMap<String, TransferProgress>>>,
    pub transfer_receivers: Arc<Mutex<HashMap<String, crate::file_transfer::FileTransferReceiver>>>,
}

pub fn get_default_download_dir() -> std::path::PathBuf {
    let base = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")));
    let download_dir = base.join("Downloads").join("OpenRemote");
    let _ = std::fs::create_dir_all(&download_dir);
    download_dir
}

pub async fn handle_file_start(
    id: String,
    filename: String,
    total_bytes: u64,
    file_hash: Option<String>,
    receivers: Arc<Mutex<HashMap<String, crate::file_transfer::FileTransferReceiver>>>,
) {
    let download_dir = get_default_download_dir();
    let safe_name = std::path::Path::new(&filename)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("received_{}", id));
    let target_path = download_dir.join(safe_name);
    match crate::file_transfer::FileTransferReceiver::new(id.clone(), target_path.clone(), total_bytes, file_hash) {
        Ok(receiver) => {
            log::info!("Starting file reception for {} into {:?}", id, target_path);
            receivers.lock().await.insert(id, receiver);
        }
        Err(e) => {
            log::error!("Failed to initialize file receiver for {}: {}", id, e);
        }
    }
}

pub async fn handle_file_chunk(
    id: String,
    offset: u64,
    data: Vec<u8>,
    is_last: bool,
    receivers: Arc<Mutex<HashMap<String, crate::file_transfer::FileTransferReceiver>>>,
) {
    let mut map = receivers.lock().await;
    if let Some(receiver) = map.get_mut(&id) {
        if let Err(e) = receiver.write_chunk(offset, &data, is_last) {
            log::error!("Failed to write chunk for {}: {}", id, e);
        } else if is_last {
            log::info!("Successfully received and saved file {} at {:?}", id, receiver.save_path);
            map.remove(&id);
        }
    }
}

impl AppState {
    pub fn new(config: AppConfig) -> Self {
        let input_sim = InputSimulator::new().ok().map(Arc::new);

        Self {
            config: Mutex::new(config),
            is_sharing: Arc::new(AtomicBool::new(false)),
            active_channel: Arc::new(Mutex::new(None)),
            quic_server: Arc::new(Mutex::new(None)),
            input_simulator: Arc::new(Mutex::new(input_sim)),
            clipboard_monitor: Mutex::new(None),
            transfer_senders: Mutex::new(HashMap::new()),
            transfer_progresses: Arc::new(Mutex::new(HashMap::new())),
            transfer_receivers: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

/// Retrieve local device information
#[tauri::command]
pub async fn get_device_info(state: State<'_, AppState>) -> Result<DeviceInfo, String> {
    let config = state.config.lock().await;
    let local_ip = crate::signaling::detect_best_local_ip();

    let public_ipv6 = crate::signaling::detect_public_ipv6();

    let signal_url = config.signal_server_url.clone();
    let d_id = config.device_id.clone();
    let d_code = config.device_code.clone();
    let d_name = config.device_name.clone();
    let l_ip = local_ip.clone();
    let l_port = config.listen_port;
    let v6_clone = public_ipv6.clone();

    // Asynchronously register / heartbeat with signaling server
    tokio::spawn(async move {
        let _ = crate::signaling::register_to_signaling(
            &signal_url, &d_id, &d_code, &d_name, &l_ip, l_port, v6_clone,
        ).await;
    });

    Ok(DeviceInfo {
        device_id: config.device_id.clone(),
        device_code: config.device_code.clone(),
        access_password: config.access_password.clone(),
        device_name: config.device_name.clone(),
        local_ip,
        listen_port: config.listen_port,
        public_ipv6,
    })
}

/// Start screen sharing loop: capture display, encode as JPEG, stream over active connection
#[tauri::command]
pub async fn start_screen_sharing(state: State<'_, AppState>) -> Result<(), String> {
    if state.is_sharing.load(Ordering::SeqCst) {
        return Ok(());
    }

    state.is_sharing.store(true, Ordering::SeqCst);
    let is_sharing = Arc::clone(&state.is_sharing);

    // Get current connection channel or start QUIC server if not already running
    let channel_opt = state.active_channel.lock().await.clone();

    tokio::spawn(async move {
        let mut capturer = match create_capturer() {
            Ok(c) => c,
            Err(e) => {
                log::error!("Failed to create screen capturer: {}", e);
                is_sharing.store(false, Ordering::SeqCst);
                return;
            }
        };

        let encoder = FrameEncoder::new(75);

        while is_sharing.load(Ordering::SeqCst) {
            let start = Instant::now();

            match capturer.capture_frame() {
                Ok(frame) => {
                    if frame.is_dirty {
                        match encoder.encode(frame.width, frame.height, &frame.data) {
                            Ok(jpeg_bytes) => {
                                if let Some(ref channel) = channel_opt {
                                    let msg = Message::ScreenFrame {
                                        data: jpeg_bytes,
                                        width: frame.width,
                                        height: frame.height,
                                        timestamp: chrono::Utc::now().timestamp_millis(),
                                    };

                                    if let Err(e) = channel.send(&msg).await {
                                        log::warn!("Failed to stream frame over channel: {}", e);
                                    }
                                }
                            }
                            Err(e) => {
                                log::error!("Frame encoding error: {}", e);
                            }
                        }
                    }
                }
                Err(e) => {
                    log::debug!("Screen capture frame wait: {}", e);
                }
            }

            // Target roughly ~30-60 FPS
            let elapsed = start.elapsed();
            let frame_target = Duration::from_millis(33);
            if elapsed < frame_target {
                tokio::time::sleep(frame_target - elapsed).await;
            }
        }
    });

    Ok(())
}

/// Stop the screen sharing capture & transport loop
#[tauri::command]
pub async fn stop_screen_sharing(state: State<'_, AppState>) -> Result<(), String> {
    state.is_sharing.store(false, Ordering::SeqCst);
    Ok(())
}

/// Connect to a remote OpenRemote peer via QUIC (supports device code or direct IP:port)
#[tauri::command]
pub async fn connect_to_device(
    address: String,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let target_addrs: Vec<SocketAddr> = if crate::signaling::is_device_code(&address) {
        let signal_url = {
            let config = state.config.lock().await;
            config.signal_server_url.clone()
        };
        log::info!("Looking up candidates for device code '{}' via signaling server {}", address, signal_url);
        let candidates = crate::signaling::lookup_peer_candidates(&signal_url, &address)
            .await
            .map_err(|e| format!("信令服务器查询设备 '{}' 失败: {}", address, e))?;

        candidates
            .into_iter()
            .filter_map(|addr_str| addr_str.parse::<SocketAddr>().ok())
            .collect()
    } else {
        let normalized = {
            let trimmed = address.trim();
            if trimmed == "localhost" {
                "127.0.0.1:28843".to_string()
            } else if !trimmed.contains(':') {
                format!("{}:28843", trimmed)
            } else {
                trimmed.to_string()
            }
        };
        vec![normalized
            .parse::<SocketAddr>()
            .map_err(|e| format!("无效的目标网络地址 '{}': {}", normalized, e))?]
    };

    if target_addrs.is_empty() {
        return Err(format!("未找到目标设备 '{}' 的可用候选网络地址", address));
    }

    // Try connecting to candidate addresses sequentially (local LAN first, then public IPv6, then NAT reflexive)
    let mut last_err = String::new();
    let mut connected_channel = None;

    for target_addr in target_addrs {
        log::info!("Attempting QUIC peer connection to candidate {}", target_addr);
        let client = match QuicClient::new_for_target(&target_addr) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("Failed creating client endpoint for {}: {}", target_addr, e);
                continue;
            }
        };

        let connect_fut = client.connect(target_addr);
        match tokio::time::timeout(std::time::Duration::from_millis(1200), connect_fut).await {
            Ok(Ok(chan)) => {
                log::info!("Successfully connected to candidate {}", target_addr);
                connected_channel = Some(chan);
                break;
            }
            Ok(Err(e)) => {
                log::warn!("Failed connecting to candidate {}: {}", target_addr, e);
                last_err = e.to_string();
            }
            Err(_) => {
                log::warn!("Connection to candidate {} timed out after 1.2s", target_addr);
                last_err = format!("连接候选地址 {} 超时", target_addr);
            }
        }
    }

    let channel = match connected_channel {
        Some(c) => c,
        None => {
            // Direct QUIC connection to all candidates failed.
            // Automatic fallback to TCP Relay Tunnel via dxcool.cn:28845!
            if crate::signaling::is_device_code(&address) {
                let signal_url = {
                    let config = state.config.lock().await;
                    config.signal_server_url.clone()
                };
                let relay_addr = crate::signaling::parse_relay_address(&signal_url);
                log::info!(
                    "Direct connection failed. Falling back to TCP Relay tunnel at {} for device {}",
                    relay_addr,
                    address
                );
                match crate::signaling::join_relay_tunnel(&relay_addr, &address).await {
                    Ok(tcp_stream) => {
                        log::info!("Successfully established TCP Relay tunnel to {}", address);
                        ConnectionChannel::new_tcp(tcp_stream)
                    }
                    Err(e) => {
                        return Err(format!(
                            "直连与中继穿透均失败: 直连最后错误: {}, 中继错误: {}",
                            last_err, e
                        ));
                    }
                }
            } else {
                return Err(format!("连接所有候选网络地址均失败: {}", last_err));
            }
        }
    };

    // Perform handshake
    let my_id = {
        let config = state.config.lock().await;
        config.device_id.clone()
    };

    let handshake = Message::Handshake {
        client_id: my_id,
        token: None,
    };
    channel.send(&handshake).await.map_err(|e| e.to_string())?;

    // Store connection channel
    *state.active_channel.lock().await = Some(channel.clone());

    // Spawn receiver task for incoming messages (e.g. screen frames or clipboard or file reception)
    let app_handle_clone = app_handle.clone();
    let receivers_clone = Arc::clone(&state.transfer_receivers);
    tokio::spawn(async move {
        loop {
            match channel.recv().await {
                Ok(msg) => match msg {
                    Message::ScreenFrame {
                        data,
                        width,
                        height,
                        timestamp,
                    } => {
                        let base64_str = base64::engine::general_purpose::STANDARD.encode(&data);
                        let _ = app_handle_clone.emit(
                            "screen-frame",
                            serde_json::json!({
                                "image": format!("data:image/jpeg;base64,{}", base64_str),
                                "width": width,
                                "height": height,
                                "timestamp": timestamp,
                            }),
                        );
                    }
                    Message::FileTransferStart {
                        id,
                        filename,
                        total_bytes,
                        file_hash,
                    } => {
                        handle_file_start(id, filename, total_bytes, file_hash, Arc::clone(&receivers_clone)).await;
                    }
                    Message::FileChunk {
                        id,
                        offset,
                        data,
                        is_last,
                    } => {
                        handle_file_chunk(id, offset, data, is_last, Arc::clone(&receivers_clone)).await;
                    }
                    Message::ClipboardUpdate { content } => {
                        let _ = ClipboardMonitor::set_text(&content);
                    }
                    Message::Ping { timestamp } => {
                        let _ = channel.send(&Message::Pong { timestamp }).await;
                    }
                    _ => {}
                },
                Err(e) => {
                    log::warn!("Channel receive ended: {}", e);
                    break;
                }
            }
        }
    });

    Ok(())
}

/// Send mouse input event to remote peer
#[tauri::command]
pub async fn send_mouse_event(
    event: crate::protocol::MouseEvent,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if let Some(ref channel) = *state.active_channel.lock().await {
        let msg = Message::MouseEvent {
            x: event.x,
            y: event.y,
            button: event.button,
            action: event.action,
            delta_x: event.delta_x,
            delta_y: event.delta_y,
        };
        channel.send(&msg).await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Send keyboard event to remote peer
#[tauri::command]
pub async fn send_key_event(
    key: String,
    action: crate::protocol::KeyAction,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if let Some(ref channel) = *state.active_channel.lock().await {
        let msg = Message::KeyEvent { key, action };
        channel.send(&msg).await.map_err(|e| e.to_string())?;
    }
    Ok(())
}


/// Disconnect current active session
#[tauri::command]
pub async fn disconnect(state: State<'_, AppState>) -> Result<(), String> {
    state.is_sharing.store(false, Ordering::SeqCst);
    *state.active_channel.lock().await = None;
    Ok(())
}

/// Initiate file transfer to peer
#[tauri::command]
pub async fn send_file(
    path: String,
    peer_address: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let transfer_id = Uuid::new_v4().to_string();
    let sender =
        FileTransferSender::new(transfer_id.clone(), &path).map_err(|e| e.to_string())?;

    let initial_progress = sender.progress(0);
    state
        .transfer_progresses
        .lock()
        .await
        .insert(transfer_id.clone(), initial_progress);

    let sender_arc = Arc::new(Mutex::new(sender));
    state
        .transfer_senders
        .lock()
        .await
        .insert(transfer_id.clone(), Arc::clone(&sender_arc));

    let active_chan = state.active_channel.lock().await.clone();
    let progresses_ref = Arc::clone(&state.transfer_progresses);

    let (filename, total_bytes, file_hash) = {
        let sender = sender_arc.lock().await;
        let filename = sender
            .file_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "unnamed_file".to_string());
        (filename, sender.total_bytes, Some(sender.file_hash.clone()))
    };

    // Spawn file sending task
    let tid = transfer_id.clone();
    tokio::spawn(async move {
        let channel = match active_chan {
            Some(ch) => ch,
            None => {
                if let Ok(addr) = peer_address.parse::<SocketAddr>() {
                    if let Ok(client) = QuicClient::new() {
                        match client.connect(addr).await {
                            Ok(ch) => ch,
                            Err(_) => return,
                        }
                    } else {
                        return;
                    }
                } else {
                    return;
                }
            }
        };

        // Send metadata first
        let start_msg = Message::FileTransferStart {
            id: tid.clone(),
            filename,
            total_bytes,
            file_hash,
        };
        if let Err(e) = channel.send(&start_msg).await {
            log::error!("Failed sending FileTransferStart: {}", e);
            let mut progresses = progresses_ref.lock().await;
            if let Some(p) = progresses.get_mut(&tid) {
                p.status = crate::file_transfer::TransferStatus::Failed;
            }
            return;
        }

        let mut offset = 0u64;
        let start_time = Instant::now();
        loop {
            let chunk_opt = {
                let mut sender = sender_arc.lock().await;
                sender.read_chunk(offset).ok().flatten()
            };

            match chunk_opt {
                Some(msg) => {
                    if let Err(e) = channel.send(&msg).await {
                        log::error!("Failed sending file chunk: {}", e);
                        let mut progresses = progresses_ref.lock().await;
                        if let Some(p) = progresses.get_mut(&tid) {
                            p.status = crate::file_transfer::TransferStatus::Failed;
                        }
                        break;
                    }

                    if let Message::FileChunk {
                        data,
                        is_last,
                        ..
                    } = msg
                    {
                        offset += data.len() as u64;
                        let elapsed_sec = start_time.elapsed().as_secs_f64();
                        let speed = if elapsed_sec > 0.1 {
                            (offset as f64 / elapsed_sec) as u64
                        } else {
                            0
                        };

                        {
                            let mut progresses = progresses_ref.lock().await;
                            if let Some(p) = progresses.get_mut(&tid) {
                                p.transferred_bytes = offset;
                                p.speed_bps = speed * 8;
                                if is_last {
                                    p.status = crate::file_transfer::TransferStatus::Completed;
                                } else {
                                    p.status = crate::file_transfer::TransferStatus::Transferring;
                                }
                            }
                        }

                        if is_last {
                            break;
                        }
                    }
                }
                None => break,
            }
        }
        log::info!("File transfer {} complete", tid);
    });

    Ok(transfer_id)
}

/// Query progress of an ongoing file transfer
#[tauri::command]
pub async fn get_transfer_progress(
    id: String,
    state: State<'_, AppState>,
) -> Result<TransferProgress, String> {
    let map = state.transfer_progresses.lock().await;
    map.get(&id)
        .cloned()
        .ok_or_else(|| format!("Transfer ID '{}' not found", id))
}

/// Sync clipboard text to remote peer
#[tauri::command]
pub async fn sync_clipboard(
    content: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    ClipboardMonitor::set_text(&content).map_err(|e| e.to_string())?;

    if let Some(ref channel) = *state.active_channel.lock().await {
        channel
            .send(&Message::ClipboardUpdate { content })
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Send Wake-on-LAN magic packet to power on a remote machine
#[tauri::command]
pub async fn wake_device(mac_address: String) -> Result<(), String> {
    send_wol_packet(&mac_address).map_err(|e| e.to_string())
}

/// Retrieve saved application configuration
#[tauri::command]
pub async fn get_config(state: State<'_, AppState>) -> Result<AppConfig, String> {
    let cfg = state.config.lock().await;
    Ok(cfg.clone())
}

/// Update and persist application configuration
#[tauri::command]
pub async fn save_config(
    config: AppConfig,
    state: State<'_, AppState>,
) -> Result<(), String> {
    config.save().map_err(|e| e.to_string())?;
    let mut current = state.config.lock().await;
    *current = config;
    Ok(())
}

/// Open local download folder in system file manager
#[tauri::command]
pub async fn open_download_dir() -> Result<(), String> {
    let dir = get_default_download_dir();
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(dir)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Send common system shortcut combinations to remote peer
#[tauri::command]
pub async fn send_shortcut(
    shortcut: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if let Some(ref channel) = *state.active_channel.lock().await {
        match shortcut.as_str() {
            "CtrlAltDel" => {
                let _ = channel.send(&Message::KeyEvent { key: "Control".to_string(), action: crate::protocol::KeyAction::Down }).await;
                let _ = channel.send(&Message::KeyEvent { key: "Alt".to_string(), action: crate::protocol::KeyAction::Down }).await;
                let _ = channel.send(&Message::KeyEvent { key: "Delete".to_string(), action: crate::protocol::KeyAction::Click }).await;
                let _ = channel.send(&Message::KeyEvent { key: "Alt".to_string(), action: crate::protocol::KeyAction::Up }).await;
                let _ = channel.send(&Message::KeyEvent { key: "Control".to_string(), action: crate::protocol::KeyAction::Up }).await;
            }
            "WinD" => {
                let _ = channel.send(&Message::KeyEvent { key: "Meta".to_string(), action: crate::protocol::KeyAction::Down }).await;
                let _ = channel.send(&Message::KeyEvent { key: "d".to_string(), action: crate::protocol::KeyAction::Click }).await;
                let _ = channel.send(&Message::KeyEvent { key: "Meta".to_string(), action: crate::protocol::KeyAction::Up }).await;
            }
            "WinL" => {
                let _ = channel.send(&Message::KeyEvent { key: "Meta".to_string(), action: crate::protocol::KeyAction::Down }).await;
                let _ = channel.send(&Message::KeyEvent { key: "l".to_string(), action: crate::protocol::KeyAction::Click }).await;
                let _ = channel.send(&Message::KeyEvent { key: "Meta".to_string(), action: crate::protocol::KeyAction::Up }).await;
            }
            "AltTab" => {
                let _ = channel.send(&Message::KeyEvent { key: "Alt".to_string(), action: crate::protocol::KeyAction::Down }).await;
                let _ = channel.send(&Message::KeyEvent { key: "Tab".to_string(), action: crate::protocol::KeyAction::Click }).await;
                let _ = channel.send(&Message::KeyEvent { key: "Alt".to_string(), action: crate::protocol::KeyAction::Up }).await;
            }
            _ => {}
        }
    }
    Ok(())
}
