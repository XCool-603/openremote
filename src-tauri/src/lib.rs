pub mod capture;
pub mod clipboard;
pub mod codec;
pub mod commands;
pub mod config;
pub mod file_transfer;
pub mod input;
pub mod protocol;
pub mod transport;
pub mod wol;
pub mod signaling;

use commands::AppState;
use config::AppConfig;
use std::net::SocketAddr;
use std::sync::Arc;
use tauri::Manager;
use transport::QuicServer;

struct StreamProfile {
    quality: std::sync::atomic::AtomicU8,
    max_width: std::sync::atomic::AtomicU32,
    fps: std::sync::atomic::AtomicU8,
    native_w: std::sync::atomic::AtomicU32,
    native_h: std::sync::atomic::AtomicU32,
    stream_w: std::sync::atomic::AtomicU32,
    stream_h: std::sync::atomic::AtomicU32,
}

impl StreamProfile {
    fn new(is_relay: bool) -> Arc<Self> {
        Arc::new(Self {
            // Relay defaults to balanced low-latency (1280px, Q50); direct QUIC defaults to high quality (1920px, Q70)
            quality: std::sync::atomic::AtomicU8::new(if is_relay { 50 } else { 70 }),
            max_width: std::sync::atomic::AtomicU32::new(if is_relay { 1280 } else { 1920 }),
            fps: std::sync::atomic::AtomicU8::new(30),
            native_w: std::sync::atomic::AtomicU32::new(0),
            native_h: std::sync::atomic::AtomicU32::new(0),
            stream_w: std::sync::atomic::AtomicU32::new(0),
            stream_h: std::sync::atomic::AtomicU32::new(0),
        })
    }

    fn update(&self, quality: u8, max_width: u32, fps: u8) {
        self.quality.store(quality.clamp(20, 95), std::sync::atomic::Ordering::Relaxed);
        self.max_width.store(max_width, std::sync::atomic::Ordering::Relaxed);
        self.fps.store(fps.clamp(10, 60), std::sync::atomic::Ordering::Relaxed);
    }

    fn map_coords(&self, x: i32, y: i32) -> (i32, i32) {
        let nw = self.native_w.load(std::sync::atomic::Ordering::Relaxed) as i64;
        let nh = self.native_h.load(std::sync::atomic::Ordering::Relaxed) as i64;
        let sw = self.stream_w.load(std::sync::atomic::Ordering::Relaxed) as i64;
        let sh = self.stream_h.load(std::sync::atomic::Ordering::Relaxed) as i64;
        if nw > 0 && nh > 0 && sw > 0 && sh > 0 && (nw != sw || nh != sh) {
            let mx = ((x as i64 * nw) / sw).clamp(0, nw - 1) as i32;
            let my = ((y as i64 * nh) / sh).clamp(0, nh - 1) as i32;
            (mx, my)
        } else {
            (x, y)
        }
    }
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
                let (mx, my) = profile.map_coords(x, y);
                let _ = sim.handle_mouse_event(&protocol::MouseEvent {
                    x: mx,
                    y: my,
                    button,
                    action,
                    delta_x,
                    delta_y,
                });
            }
        }
        protocol::Message::KeyEvent { key, action } => {
            if let Some(ref sim) = input_sim_rx {
                let _ = sim.handle_key_event(&protocol::KeyEvent { key, action });
            }
        }
        protocol::Message::TypeText { text } => {
            if let Some(ref sim) = input_sim_rx {
                let _ = sim.type_text(&text);
            }
        }
        protocol::Message::ClipboardUpdate { content } => {
            let _ = clipboard::ClipboardMonitor::set_text_silent(&content).await;
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
            commands::handle_file_chunk(
                id,
                offset,
                data,
                is_last,
                transfer_receivers_rx.clone(),
            )
            .await;
        }
        protocol::Message::Ping { timestamp } => {
            let _ = channel_rx.send(&protocol::Message::Pong { timestamp }).await;
        }
        protocol::Message::QualityConfig {
            quality,
            max_width,
            fps,
        } => {
            log::info!("Applying dynamic stream profile: quality={}, max_width={}, fps={}", quality, max_width, fps);
            profile.update(quality, max_width, fps);
        }
        _ => {}
    }
}

fn spawn_screen_stream_task(
    stream_channel: transport::ConnectionChannel,
    is_sharing_stream: Arc<std::sync::atomic::AtomicBool>,
    profile: Arc<StreamProfile>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        log::info!("Starting adaptive screen streaming pipeline...");

        while is_sharing_stream.load(std::sync::atomic::Ordering::SeqCst) {
            let capturer_res = capture::create_capturer();
            let mut capturer = match capturer_res {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("Screen capturer standby (temporary desktop unavailable: {}). Sending keepalive frame...", e);
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
                        if let Err(err) = stream_channel.send(&msg).await {
                            log::info!("Channel closed during keepalive: {}", err);
                            break;
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    continue;
                }
            };

            log::info!("Screen capturer successfully initialized!");

            while is_sharing_stream.load(std::sync::atomic::Ordering::SeqCst) {
                let start = std::time::Instant::now();
                let cur_quality = profile.quality.load(std::sync::atomic::Ordering::Relaxed);
                let cur_max_width = profile.max_width.load(std::sync::atomic::Ordering::Relaxed);
                let cur_fps = profile.fps.load(std::sync::atomic::Ordering::Relaxed).max(1) as u64;

                let encoder = codec::FrameEncoder::new(cur_quality);

                match capturer.capture_frame() {
                    Ok(frame) => {
                        profile.native_w.store(frame.width, std::sync::atomic::Ordering::Relaxed);
                        profile.native_h.store(frame.height, std::sync::atomic::Ordering::Relaxed);

                        if frame.is_dirty {
                            if let Ok((jpeg, out_w, out_h)) = encoder.encode_scaled(
                                frame.width,
                                frame.height,
                                &frame.data,
                                cur_max_width,
                            ) {
                                profile.stream_w.store(out_w, std::sync::atomic::Ordering::Relaxed);
                                profile.stream_h.store(out_h, std::sync::atomic::Ordering::Relaxed);

                                let msg = protocol::Message::ScreenFrame {
                                    data: jpeg,
                                    width: out_w,
                                    height: out_h,
                                    timestamp: chrono::Utc::now().timestamp_millis(),
                                };
                                if let Err(e) = stream_channel.send(&msg).await {
                                    log::info!("Stream channel closed: {}", e);
                                    is_sharing_stream.store(false, std::sync::atomic::Ordering::SeqCst);
                                    return;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!("Frame capture failed ({}), retrying capturer in 100ms...", e);
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        break;
                    }
                }

                let elapsed = start.elapsed();
                let target = std::time::Duration::from_millis(1000 / cur_fps);
                if elapsed < target {
                    tokio::time::sleep(target - elapsed).await;
                }
            }
        }
    })
}

pub fn run() {
    let _ = env_logger::try_init();

    let mut config = AppConfig::load().unwrap_or_default();
    let initial_port = if config.listen_port > 0 { config.listen_port } else { 28843 };

    // Dynamically search and bind to an available free port starting from initial_port
    let mut bound_server = None;
    let mut actual_port = initial_port;

    for offset in 0..50 {
        let try_port = initial_port.wrapping_add(offset);
        // Try IPv6 dual-stack first ([::]:port accepts both IPv6 and IPv4)
        let addr_v6: Result<SocketAddr, _> = format!("[::]:{}", try_port).parse();
        let bound = if let Ok(addr) = addr_v6 {
            QuicServer::bind(addr).ok()
        } else {
            None
        };

        let bound = match bound {
            Some(s) => Some(s),
            None => {
                // Fallback to pure IPv4
                if let Ok(addr_v4) = format!("0.0.0.0:{}", try_port).parse::<SocketAddr>() {
                    QuicServer::bind(addr_v4).ok()
                } else {
                    None
                }
            }
        };

        if let Some(server) = bound {
            log::info!("Successfully bound QUIC server on free port {} ({})", try_port, server.listen_addr);
            bound_server = Some(server);
            actual_port = try_port;
            break;
        }
    }

    // Ephemeral port fallback if range is occupied
    if bound_server.is_none() {
        if let Ok(addr) = "[::]:0".parse::<SocketAddr>().or_else(|_| "0.0.0.0:0".parse()) {
            if let Ok(server) = QuicServer::bind(addr) {
                actual_port = server.listen_addr.port();
                log::info!("OS allocated free ephemeral port {}", actual_port);
                bound_server = Some(server);
            }
        }
    }

    if actual_port != config.listen_port {
        config.listen_port = actual_port;
        let _ = config.save();
    }

    let app_state = AppState::new(config);

    if let Some(server) = bound_server {
        let server_arc = Arc::new(server);
        *app_state.quic_server.blocking_lock() = Some(Arc::clone(&server_arc));

        let state_channel = app_state.active_channel.clone();
        let is_sharing = Arc::clone(&app_state.is_sharing);
        let input_sim = app_state.input_simulator.clone();
        let transfer_receivers = app_state.transfer_receivers.clone();

                tokio::spawn(async move {
                    loop {
                        match server_arc.accept_channel().await {
                            Ok(channel) => {
                                log::info!("Accepted incoming remote desktop connection");
                                *state_channel.lock().await = Some(channel.clone());

                                let is_sharing_stream = Arc::clone(&is_sharing);
                                is_sharing_stream.store(true, std::sync::atomic::Ordering::SeqCst);

                                let profile = StreamProfile::new(false);
                                let capture_task = spawn_screen_stream_task(
                                    channel.clone(),
                                    is_sharing_stream,
                                    Arc::clone(&profile),
                                );

                                // Start clipboard sync on host for direct QUIC session
                                let mut host_clipboard = clipboard::ClipboardMonitor::new();
                                let channel_cb = channel.clone();
                                host_clipboard.start_monitoring(move |text| {
                                    let chan = channel_cb.clone();
                                    tokio::spawn(async move {
                                        let msg = protocol::Message::ClipboardUpdate { content: text };
                                        let _ = chan.send(&msg).await;
                                    });
                                });

                                let channel_rx = channel.clone();
                                let input_sim_rx = input_sim.lock().await.clone();
                                let is_sharing_clone = Arc::clone(&is_sharing);
                                let transfer_receivers_rx = transfer_receivers.clone();

                                tokio::spawn(async move {
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
                                                log::info!("Client disconnected: {}", e);
                                                is_sharing_clone
                                                    .store(false, std::sync::atomic::Ordering::SeqCst);
                                                break;
                                            }
                                        }
                                    }
                                    host_clipboard.stop_monitoring();
                                    capture_task.abort();
                                });
                            }
                            Err(e) => {
                                log::warn!("Failed accepting QUIC channel: {}", e);
                            }
                        }
                    }
                });
    }

    // Background continuous heartbeat registration with signaling server
    let (signal_url, d_id, d_code, d_name, l_port) = {
        let cfg = app_state.config.blocking_lock();
        (
            cfg.signal_server_url.clone(),
            cfg.device_id.clone(),
            cfg.device_code.clone(),
            cfg.device_name.clone(),
            cfg.listen_port,
        )
    };

    let signal_url_clone = signal_url.clone();
    let d_code_clone = d_code.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let l_ip = crate::signaling::detect_best_local_ip();
            let v6 = crate::signaling::detect_public_ipv6();
            let _ = crate::signaling::register_to_signaling(
                &signal_url_clone, &d_id, &d_code_clone, &d_name, &l_ip, l_port, v6,
            )
            .await;
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    });

    // Background continuous TCP Relay listener on dxcool.cn:28845
    let relay_channel = app_state.active_channel.clone();
    let relay_is_sharing = Arc::clone(&app_state.is_sharing);
    let relay_input_sim = app_state.input_simulator.clone();
    let relay_transfer_receivers = app_state.transfer_receivers.clone();

    tauri::async_runtime::spawn(async move {
        let relay_addr = crate::signaling::parse_relay_address(&signal_url);
        loop {
            log::info!("Connecting to TCP Relay server at {} to stand by for remote control...", relay_addr);

            match crate::signaling::register_relay_tunnel(&relay_addr, &d_code).await {
                Ok(tcp_stream) => {
                    log::info!("Successfully registered as host on TCP Relay! Standing by for controller...");
                    let channel = transport::ConnectionChannel::new_tcp(tcp_stream);

                    match channel.recv().await {
                        Ok(msg) => {
                            log::info!("Controller connected via TCP Relay tunnel! Message: {:?}", msg);
                            *relay_channel.lock().await = Some(channel.clone());

                            let is_sharing_stream = Arc::clone(&relay_is_sharing);
                            is_sharing_stream.store(true, std::sync::atomic::Ordering::SeqCst);

                            let profile = StreamProfile::new(true);
                            let capture_task = spawn_screen_stream_task(
                                channel.clone(),
                                Arc::clone(&relay_is_sharing),
                                Arc::clone(&profile),
                            );

                            // Start clipboard sync on host for TCP Relay session
                            let mut host_clipboard = clipboard::ClipboardMonitor::new();
                            let channel_cb = channel.clone();
                            host_clipboard.start_monitoring(move |text| {
                                let chan = channel_cb.clone();
                                tokio::spawn(async move {
                                    let msg = protocol::Message::ClipboardUpdate { content: text };
                                    let _ = chan.send(&msg).await;
                                });
                            });

                            let channel_rx = channel.clone();
                            let input_sim_rx = relay_input_sim.lock().await.clone();
                            let is_sharing_clone = Arc::clone(&relay_is_sharing);
                            let transfer_receivers_rx = relay_transfer_receivers.clone();

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
                                        log::info!("Relay peer session ended: {}", e);
                                        is_sharing_clone.store(false, std::sync::atomic::Ordering::SeqCst);
                                        break;
                                    }
                                }
                            }
                            host_clipboard.stop_monitoring();
                            capture_task.abort();
                        }
                        Err(e) => {
                            log::warn!("Relay standby socket disconnected: {}", e);
                        }
                    }
                }
                Err(e) => {
                    log::warn!("Failed to register on TCP Relay: {}. Retrying in 5s...", e);
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    });

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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
