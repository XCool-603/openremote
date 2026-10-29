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

                                let stream_channel = channel.clone();
                                let is_sharing_stream = Arc::clone(&is_sharing);
                                is_sharing_stream.store(true, std::sync::atomic::Ordering::SeqCst);

                                tokio::spawn(async move {
                                    let mut capturer = match capture::create_capturer() {
                                        Ok(c) => c,
                                        Err(e) => {
                                            log::error!("Failed to create capturer for connection: {}", e);
                                            return;
                                        }
                                    };
                                    let encoder = codec::FrameEncoder::new(70);

                                    while is_sharing_stream.load(std::sync::atomic::Ordering::SeqCst) {
                                        let start = std::time::Instant::now();
                                        if let Ok(frame) = capturer.capture_frame() {
                                            if frame.is_dirty {
                                                if let Ok(jpeg) = encoder.encode(frame.width, frame.height, &frame.data) {
                                                    let msg = protocol::Message::ScreenFrame {
                                                        data: jpeg,
                                                        width: frame.width,
                                                        height: frame.height,
                                                        timestamp: chrono::Utc::now().timestamp_millis(),
                                                    };
                                                    if let Err(e) = stream_channel.send(&msg).await {
                                                        log::info!("Stream channel closed: {}", e);
                                                        break;
                                                    }
                                                }
                                            }
                                        }
                                        let elapsed = start.elapsed();
                                        let target = std::time::Duration::from_millis(33);
                                        if elapsed < target {
                                            tokio::time::sleep(target - elapsed).await;
                                        }
                                    }
                                });

                                let channel_rx = channel.clone();
                                let input_sim_rx = input_sim.lock().await.clone();
                                let is_sharing_clone = Arc::clone(&is_sharing);
                                let transfer_receivers_rx = transfer_receivers.clone();

                                tokio::spawn(async move {
                                    loop {
                                        match channel_rx.recv().await {
                                            Ok(msg) => match msg {
                                                protocol::Message::MouseEvent {
                                                    x,
                                                    y,
                                                    button,
                                                    action,
                                                    delta_x,
                                                    delta_y,
                                                } => {
                                                    if let Some(ref sim) = input_sim_rx {
                                                        let _ = sim.handle_mouse_event(
                                                            &protocol::MouseEvent {
                                                                x,
                                                                y,
                                                                button,
                                                                action,
                                                                delta_x,
                                                                delta_y,
                                                            },
                                                        );
                                                    }
                                                }
                                                protocol::Message::KeyEvent { key, action } => {
                                                    if let Some(ref sim) = input_sim_rx {
                                                        let _ = sim.handle_key_event(
                                                            &protocol::KeyEvent { key, action },
                                                        );
                                                    }
                                                }
                                                protocol::Message::TypeText { text } => {
                                                    if let Some(ref sim) = input_sim_rx {
                                                        let _ = sim.type_text(&text);
                                                    }
                                                }
                                                protocol::Message::ClipboardUpdate { content } => {
                                                    let _ =
                                                        clipboard::ClipboardMonitor::set_text(
                                                            &content,
                                                        );
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
                                                    let _ = channel_rx
                                                        .send(&protocol::Message::Pong { timestamp })
                                                        .await;
                                                }
                                                _ => {}
                                            },
                                            Err(e) => {
                                                log::info!("Client disconnected: {}", e);
                                                is_sharing_clone
                                                    .store(false, std::sync::atomic::Ordering::SeqCst);
                                                break;
                                            }
                                        }
                                    }
                                });
                            }
                            Err(e) => {
                                log::warn!("Failed accepting QUIC channel: {}", e);
                            }
                        }
                    }
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
