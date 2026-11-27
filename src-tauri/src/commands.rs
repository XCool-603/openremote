use crate::capture::create_capturer;
use crate::clipboard::ClipboardMonitor;
use crate::codec::FrameEncoder;
use crate::config::AppConfig;
use crate::file_transfer::{FileTransferSender, TransferProgress};
use crate::input::InputSimulator;
use crate::protocol::{DirEntryInfo, Message};
use crate::transport::{ConnectionChannel, QuicClient, QuicServer};
use crate::wol::send_wol_packet;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{Emitter, State};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirListingResult {
    pub current_path: String,
    pub parent_path: Option<String>,
    pub entries: Vec<DirEntryInfo>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub device_id: String,
    pub device_code: String,
    pub access_password: String,
    pub device_name: String,
    pub local_ip: String,
    pub listen_port: u16,
    pub public_ipv6: Option<String>,
    pub is_rdp_session: bool,
    pub session_id: u32,
    pub service_installed: bool,
    pub service_running: bool,
}

pub struct AppState {
    pub config: Mutex<AppConfig>,
    pub is_sharing: Arc<AtomicBool>,
    pub active_channel: Arc<Mutex<Option<ConnectionChannel>>>,
    pub active_mode: Arc<StdMutex<String>>,
    pub receiver_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    pub ping_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    pub quic_server: Arc<Mutex<Option<Arc<QuicServer>>>>,
    pub fallback_host: Arc<Mutex<Option<crate::HostDaemonHandle>>>,
    pub input_simulator: Arc<Mutex<Option<Arc<InputSimulator>>>>,
    pub clipboard_monitor: Mutex<Option<ClipboardMonitor>>,
    pub transfer_senders: Mutex<HashMap<String, Arc<Mutex<FileTransferSender>>>>,
    pub transfer_progresses: Arc<Mutex<HashMap<String, TransferProgress>>>,
    pub transfer_receivers: Arc<Mutex<HashMap<String, crate::file_transfer::FileTransferReceiver>>>,
    pub pending_dir_list: Arc<StdMutex<HashMap<String, tokio::sync::oneshot::Sender<DirListingResult>>>>,
    pub frame_tx: tokio::sync::broadcast::Sender<Arc<Vec<u8>>>,
    pub pending_frames: Arc<StdMutex<VecDeque<Arc<Vec<u8>>>>>,
    pub last_keyframe: Arc<StdMutex<Option<Arc<Vec<u8>>>>>,
    pub peer_cache: StdMutex<HashMap<String, (Vec<SocketAddr>, Instant)>>,
    pub native_renderer: Arc<crate::renderer::native_d3d11::NativeD3D11Controller>,
    pub native_input_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    pub stream_port: u16,
}

/// Resolve the active interactive user's profile directory even when running under LocalSystem!
fn detect_interactive_user_profile() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Ok(up) = std::env::var("USERPROFILE") {
            let lower = up.to_ascii_lowercase();
            if !lower.contains("systemprofile") && !lower.contains("defaultuser0") {
                return PathBuf::from(up);
            }
        }
        // Under LocalSystem, inspect C:\Users\* for the most recently modified Desktop/Downloads user folder
        if let Ok(entries) = std::fs::read_dir("C:\\Users") {
            let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
            for ent in entries.flatten() {
                let name = ent.file_name().to_string_lossy().to_ascii_lowercase();
                if matches!(name.as_str(), "public" | "default" | "default user" | "all users") {
                    continue;
                }
                let p = ent.path();
                if p.join("Desktop").is_dir() && p.join("Downloads").is_dir() {
                    if let Ok(meta) = p.join("NTUSER.DAT").metadata().or_else(|_| p.metadata()) {
                        let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                        if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
                            best = Some((mtime, p));
                        }
                    }
                }
            }
            if let Some((_, p)) = best {
                return p;
            }
        }
    }

    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

pub fn get_default_download_dir() -> std::path::PathBuf {
    let base = detect_interactive_user_profile();
    let download_dir = base.join("Downloads").join("OpenRemote");
    let _ = std::fs::create_dir_all(&download_dir);
    download_dir
}

pub fn get_clipboard_cache_dir() -> PathBuf {
    let dir = AppConfig::shared_data_dir().join("ClipboardCache");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn parse_clipboard_batch_id(id: &str) -> Option<(&str, usize, usize)> {
    let rest = id.strip_prefix("clip:")?;
    let mut parts = rest.split(':');
    let batch_id = parts.next()?;
    let idx: usize = parts.next()?.parse().ok()?;
    let total: usize = parts.next()?.parse().ok()?;
    if total == 0 {
        return None;
    }
    Some((batch_id, idx, total))
}

struct ClipboardBatchState {
    total: usize,
    files: Vec<(usize, PathBuf)>,
    created_at: Instant,
}

static CLIPBOARD_BATCHES: OnceLock<StdMutex<HashMap<String, ClipboardBatchState>>> = OnceLock::new();

fn clipboard_batches() -> &'static StdMutex<HashMap<String, ClipboardBatchState>> {
    CLIPBOARD_BATCHES.get_or_init(|| StdMutex::new(HashMap::new()))
}

fn prune_old_clipboard_cache() {
    let cache = get_clipboard_cache_dir();
    if let Ok(entries) = std::fs::read_dir(&cache) {
        for ent in entries.flatten() {
            let p = ent.path();
            if p.is_dir() {
                if let Ok(meta) = ent.metadata() {
                    if let Ok(modified) = meta.modified() {
                        if modified.elapsed().unwrap_or_default() > Duration::from_secs(7200) {
                            let _ = std::fs::remove_dir_all(&p);
                        }
                    }
                }
            }
        }
    }
}

pub async fn handle_file_start(
    id: String,
    filename: String,
    total_bytes: u64,
    file_hash: Option<String>,
    receivers: Arc<Mutex<HashMap<String, crate::file_transfer::FileTransferReceiver>>>,
) {
    let target_dir = if let Some((batch_id, _, _)) = parse_clipboard_batch_id(&id) {
        let dir = get_clipboard_cache_dir().join(batch_id);
        let _ = std::fs::create_dir_all(&dir);
        dir
    } else {
        get_default_download_dir()
    };

    let safe_name = std::path::Path::new(&filename)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("received_{}", id));
    let target_path = target_dir.join(safe_name);
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

pub async fn handle_file_chunk_with_callback<F>(
    id: String,
    offset: u64,
    data: Vec<u8>,
    is_last: bool,
    receivers: Arc<Mutex<HashMap<String, crate::file_transfer::FileTransferReceiver>>>,
    on_clipboard_ready: F,
) where
    F: FnOnce(usize) + Send,
{
    let mut map = receivers.lock().await;
    if let Some(receiver) = map.get_mut(&id) {
        if let Err(e) = receiver.write_chunk(offset, &data, is_last) {
            log::error!("Failed to write chunk for {}: {}", id, e);
        } else if is_last {
            let saved_path = receiver.save_path.clone();
            log::info!("Successfully received and saved file {} at {:?}", id, saved_path);
            map.remove(&id);

            if let Some((batch_id, idx, total)) = parse_clipboard_batch_id(&id) {
                let completed_paths: Option<Vec<PathBuf>> = {
                    if let Ok(mut batches) = clipboard_batches().lock() {
                        batches.retain(|_, v| v.created_at.elapsed() < Duration::from_secs(600));
                        let entry = batches.entry(batch_id.to_string()).or_insert_with(|| ClipboardBatchState {
                            total,
                            files: Vec::with_capacity(total),
                            created_at: Instant::now(),
                        });
                        entry.files.push((idx, saved_path));
                        if entry.files.len() >= entry.total {
                            if let Some(mut finished) = batches.remove(batch_id) {
                                finished.files.sort_by_key(|(i, _)| *i);
                                Some(finished.files.into_iter().map(|(_, p)| p).collect())
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                };

                if let Some(paths) = completed_paths {
                    let count = paths.len();
                    match ClipboardMonitor::set_files_silent(&paths) {
                        Ok(()) => {
                            crate::diag_log(&format!("Synced {} remote clipboard files -> ready for Ctrl+V", count));
                            on_clipboard_ready(count);
                        }
                        Err(e) => {
                            crate::diag_log(&format!("Failed to write CF_HDROP clipboard files: {}", e));
                        }
                    }
                    std::thread::spawn(prune_old_clipboard_cache);
                }
            }
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
    handle_file_chunk_with_callback(id, offset, data, is_last, receivers, |_| {}).await;
}

/// Automatically stream copied Windows Explorer files (CF_HDROP) to peer's ClipboardCache (capped at 256MB per batch)
pub async fn send_clipboard_files_over_channel(paths: Vec<PathBuf>, channel: ConnectionChannel) {
    const MAX_CLIP_BATCH_BYTES: u64 = 256 * 1024 * 1024;
    let regular_files: Vec<PathBuf> = paths.into_iter().filter(|p| p.is_file()).collect();
    if regular_files.is_empty() {
        crate::diag_log("Clipboard selection has no regular files (folders are skipped for Ctrl+C sync)");
        return;
    }
    let total_bytes: u64 = regular_files
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok().map(|m| m.len()))
        .sum();
    if total_bytes > MAX_CLIP_BATCH_BYTES {
        crate::diag_log(&format!(
            "Clipboard file batch exceeds 256MB ({} bytes); skipped auto-sync",
            total_bytes
        ));
        return;
    }

    crate::diag_log(&format!(
        "Streaming {} clipboard file(s) ({} bytes) to peer...",
        regular_files.len(),
        total_bytes
    ));
    let batch_id = Uuid::new_v4().to_string();
    let total = regular_files.len().min(32);
    for (idx, path) in regular_files.into_iter().take(total).enumerate() {
        let clip_id = format!("clip:{}:{}:{}", batch_id, idx, total);
        let path_clone = path.clone();
        let sender_res = tokio::task::spawn_blocking(move || FileTransferSender::new(clip_id, &path_clone)).await;
        let mut sender = match sender_res {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                log::warn!("Clipboard file read failed ({:?}): {}", path, e);
                continue;
            }
            Err(_) => continue,
        };
        let filename = sender
            .file_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());
        let start_msg = Message::FileTransferStart {
            id: sender.id.clone(),
            filename,
            total_bytes: sender.total_bytes,
            file_hash: Some(sender.file_hash.clone()),
        };
        if channel.send(&start_msg).await.is_err() {
            break;
        }
        if sender.total_bytes == 0 {
            let _ = channel
                .send(&Message::FileChunk {
                    id: sender.id.clone(),
                    offset: 0,
                    data: Vec::new(),
                    is_last: true,
                })
                .await;
            continue;
        }
        let mut offset = 0u64;
        while let Ok(Some(chunk_msg)) = sender.read_chunk(offset) {
            let chunk_len = if let Message::FileChunk { ref data, .. } = chunk_msg {
                data.len() as u64
            } else {
                0
            };
            let is_last = matches!(chunk_msg, Message::FileChunk { is_last: true, .. });
            if channel.send(&chunk_msg).await.is_err() {
                return;
            }
            offset += chunk_len;
            if is_last {
                break;
            }
        }
    }
}

/// Read local directory entries cleanly (supports ""/None -> user Desktop + Downloads shortcut or real folder)
pub fn read_local_dir_entries(path: Option<&str>) -> DirListingResult {
    let target_dir = match path.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => detect_interactive_user_profile().join("Desktop"),
    };

    let current_path = target_dir.to_string_lossy().to_string();
    let parent_path = target_dir.parent().map(|p| p.to_string_lossy().to_string());

    let mut entries = Vec::new();
    let read_res = match std::fs::read_dir(&target_dir) {
        Ok(rd) => rd,
        Err(e) => {
            return DirListingResult {
                current_path,
                parent_path,
                entries,
                error: Some(format!("无法读取目录: {}", e)),
            };
        }
    };

    for ent in read_res.flatten().take(400) {
        let p = ent.path();
        let name = ent.file_name().to_string_lossy().to_string();
        if name.starts_with("desktop.ini") {
            continue;
        }
        let meta = match ent.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let is_dir = meta.is_dir();
        let size_bytes = if is_dir { 0 } else { meta.len() };
        let modified = meta
            .modified()
            .ok()
            .map(|t| {
                let dt: chrono::DateTime<chrono::Local> = t.into();
                dt.format("%Y-%m-%d %H:%M").to_string()
            })
            .unwrap_or_default();
        entries.push(DirEntryInfo {
            name,
            path: p.to_string_lossy().to_string(),
            is_dir,
            size_bytes,
            modified,
        });
    }

    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    DirListingResult {
        current_path,
        parent_path,
        entries,
        error: None,
    }
}

/// Stream a single file over an active channel (reused by both controller upload and host PullFileReq!)
pub async fn stream_file_to_channel(
    path: String,
    tid: String,
    channel: ConnectionChannel,
    progresses_opt: Option<Arc<Mutex<HashMap<String, TransferProgress>>>>,
) {
    let sender = match tokio::task::spawn_blocking(move || FileTransferSender::new(tid, &path)).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            log::warn!("FileTransferSender init failed: {}", e);
            return;
        }
        Err(_) => return,
    };
    let tid = sender.id.clone();
    let filename = sender
        .file_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "unnamed_file".to_string());
    let total_bytes = sender.total_bytes;
    let file_hash = Some(sender.file_hash.clone());
    let mut sender = sender;

    let start_msg = Message::FileTransferStart {
        id: tid.clone(),
        filename,
        total_bytes,
        file_hash,
    };
    if let Err(e) = channel.send(&start_msg).await {
        log::error!("Failed sending FileTransferStart: {}", e);
        if let Some(ref prog) = progresses_opt {
            if let Some(p) = prog.lock().await.get_mut(&tid) {
                p.status = crate::file_transfer::TransferStatus::Failed;
            }
        }
        return;
    }

    if total_bytes == 0 {
        let _ = channel
            .send(&Message::FileChunk {
                id: tid.clone(),
                offset: 0,
                data: Vec::new(),
                is_last: true,
            })
            .await;
        if let Some(ref prog) = progresses_opt {
            if let Some(p) = prog.lock().await.get_mut(&tid) {
                p.status = crate::file_transfer::TransferStatus::Completed;
            }
        }
        return;
    }

    let mut offset = 0u64;
    let start_time = Instant::now();
    while let Ok(Some(msg)) = sender.read_chunk(offset) {
        if let Err(e) = channel.send(&msg).await {
            log::error!("Failed sending file chunk: {}", e);
            if let Some(ref prog) = progresses_opt {
                if let Some(p) = prog.lock().await.get_mut(&tid) {
                    p.status = crate::file_transfer::TransferStatus::Failed;
                }
            }
            return;
        }
        if let Message::FileChunk { data, is_last, .. } = msg {
            offset += data.len() as u64;
            let elapsed_sec = start_time.elapsed().as_secs_f64();
            let speed = if elapsed_sec > 0.1 {
                (offset as f64 / elapsed_sec) as u64
            } else {
                0
            };
            if let Some(ref prog) = progresses_opt {
                if let Some(p) = prog.lock().await.get_mut(&tid) {
                    p.transferred_bytes = offset;
                    p.speed_bps = speed * 8;
                    p.status = if is_last {
                        crate::file_transfer::TransferStatus::Completed
                    } else {
                        crate::file_transfer::TransferStatus::Transferring
                    };
                }
            }
            if is_last {
                break;
            }
        }
    }
}

/// Pack an entire frame (1 or more dirty tiles) into a single atomic binary packet ("FRAM").
/// Eliminates per-tile broadcast lag/drops and lets the WebView decode & paint an entire frame tear-free.
fn pack_binary_frame_batch(
    screen_w: u16,
    screen_h: u16,
    tiles: &[crate::protocol::DirtyTile],
) -> Arc<Vec<u8>> {
    let payload_size: usize = tiles.iter().map(|t| 12 + t.data.len()).sum();
    let mut pkt = Vec::with_capacity(10 + payload_size);
    pkt.extend_from_slice(b"FRAM");
    pkt.extend_from_slice(&screen_w.to_le_bytes());
    pkt.extend_from_slice(&screen_h.to_le_bytes());
    pkt.extend_from_slice(&(tiles.len().min(u16::MAX as usize) as u16).to_le_bytes());
    for tile in tiles.iter().take(u16::MAX as usize) {
        pkt.extend_from_slice(&tile.x.to_le_bytes());
        pkt.extend_from_slice(&tile.y.to_le_bytes());
        pkt.extend_from_slice(&tile.w.to_le_bytes());
        pkt.extend_from_slice(&tile.h.to_le_bytes());
        pkt.extend_from_slice(&(tile.data.len() as u32).to_le_bytes());
        pkt.extend_from_slice(&tile.data);
    }
    Arc::new(pkt)
}

fn pack_single_frame_batch(
    screen_w: u16,
    screen_h: u16,
    data: &[u8],
) -> Arc<Vec<u8>> {
    let mut pkt = Vec::with_capacity(10 + 12 + data.len());
    pkt.extend_from_slice(b"FRAM");
    pkt.extend_from_slice(&screen_w.to_le_bytes());
    pkt.extend_from_slice(&screen_h.to_le_bytes());
    pkt.extend_from_slice(&1u16.to_le_bytes());
    pkt.extend_from_slice(&0u16.to_le_bytes());
    pkt.extend_from_slice(&0u16.to_le_bytes());
    pkt.extend_from_slice(&screen_w.to_le_bytes());
    pkt.extend_from_slice(&screen_h.to_le_bytes());
    pkt.extend_from_slice(&(data.len() as u32).to_le_bytes());
    pkt.extend_from_slice(data);
    Arc::new(pkt)
}

fn start_local_binary_stream_server(
    frame_tx: tokio::sync::broadcast::Sender<Arc<Vec<u8>>>,
    pending_frames: Arc<StdMutex<VecDeque<Arc<Vec<u8>>>>>,
    last_keyframe: Arc<StdMutex<Option<Arc<Vec<u8>>>>>,
) -> u16 {
    let std_listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e) => {
            log::error!("Failed to bind local binary stream server: {}", e);
            return 0;
        }
    };
    let port = std_listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let _ = std_listener.set_nonblocking(true);

    tauri::async_runtime::spawn(async move {
        let listener = match tokio::net::TcpListener::from_std(std_listener) {
            Ok(l) => l,
            Err(_) => return,
        };
        log::info!("Local zero-copy atomic frame stream listening on 127.0.0.1:{}", port);
        loop {
            if let Ok((mut sock, _)) = listener.accept().await {
                let _ = sock.set_nodelay(true);
                let mut rx = frame_tx.subscribe();
                let replay_queue = Arc::clone(&pending_frames);
                let kf_slot = Arc::clone(&last_keyframe);
                tokio::spawn(async move {
                    let mut req_buf = [0u8; 1024];
                    let n = match tokio::time::timeout(Duration::from_secs(2), sock.read(&mut req_buf)).await {
                        Ok(Ok(n)) if n > 0 => n,
                        _ => return,
                    };
                    // Handle CORS preflight without consuming buffered keyframes!
                    if req_buf[..n].starts_with(b"OPTIONS") {
                        let cors_ok = b"HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, OPTIONS\r\nAccess-Control-Allow-Headers: *\r\nConnection: close\r\n\r\n";
                        let _ = sock.write_all(cors_ok).await;
                        return;
                    }

                    let header = b"HTTP/1.1 200 OK\r\nAccess-Control-Allow-Origin: *\r\nContent-Type: application/octet-stream\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n";
                    if sock.write_all(header).await.is_err() {
                        return;
                    }

                    // Always replay last full keyframe non-destructively (survives React StrictMode & tab switches!)
                    let kf_opt = kf_slot.lock().ok().and_then(|g| g.clone());
                    if let Some(ref kf) = kf_opt {
                        if sock.write_all(kf).await.is_err() {
                            return;
                        }
                    }

                    // Replay any queued frames buffered while WebView was mounting
                    let buffered: Vec<Arc<Vec<u8>>> = {
                        if let Ok(mut q) = replay_queue.lock() {
                            q.drain(..).collect()
                        } else {
                            Vec::new()
                        }
                    };
                    for pkt in buffered {
                        if let Some(ref kf) = kf_opt {
                            if Arc::ptr_eq(&pkt, kf) {
                                continue;
                            }
                        }
                        if sock.write_all(&pkt).await.is_err() {
                            return;
                        }
                    }

                    loop {
                        match rx.recv().await {
                            Ok(pkt) => {
                                if sock.write_all(&pkt).await.is_err() {
                                    break;
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                });
            }
        }
    });

    port
}

impl AppState {
    pub fn new(config: AppConfig) -> Self {
        let input_sim = InputSimulator::new().ok().map(Arc::new);
        let (frame_tx, _) = tokio::sync::broadcast::channel(8);
        let pending_frames = Arc::new(StdMutex::new(VecDeque::with_capacity(4)));
        let last_keyframe = Arc::new(StdMutex::new(None));
        let stream_port = start_local_binary_stream_server(
            frame_tx.clone(),
            Arc::clone(&pending_frames),
            Arc::clone(&last_keyframe),
        );

        Self {
            config: Mutex::new(config),
            is_sharing: Arc::new(AtomicBool::new(false)),
            active_channel: Arc::new(Mutex::new(None)),
            active_mode: Arc::new(StdMutex::new(String::new())),
            receiver_task: Mutex::new(None),
            ping_task: Mutex::new(None),
            quic_server: Arc::new(Mutex::new(None)),
            fallback_host: Arc::new(Mutex::new(None)),
            input_simulator: Arc::new(Mutex::new(input_sim)),
            clipboard_monitor: Mutex::new(None),
            transfer_senders: Mutex::new(HashMap::new()),
            transfer_progresses: Arc::new(Mutex::new(HashMap::new())),
            transfer_receivers: Arc::new(Mutex::new(HashMap::new())),
            pending_dir_list: Arc::new(StdMutex::new(HashMap::new())),
            frame_tx,
            pending_frames,
            last_keyframe,
            peer_cache: StdMutex::new(HashMap::new()),
            native_renderer: Arc::new(crate::renderer::native_d3d11::NativeD3D11Controller::new()),
            native_input_task: Mutex::new(None),
            stream_port,
        }
    }
}

/// Retrieve local device information (signaling registration with true STUN IP:Port is owned by lib.rs)
#[tauri::command]
pub async fn get_device_info(state: State<'_, AppState>) -> Result<DeviceInfo, String> {
    let config = state.config.lock().await;
    let local_ip = crate::signaling::detect_best_local_ip();
    let public_ipv6 = crate::signaling::detect_public_ipv6();
    let rdp_st = crate::input::rdp_session_status();
    let svc_st = crate::service::query_service_status();

    Ok(DeviceInfo {
        device_id: config.device_id.clone(),
        device_code: config.device_code.clone(),
        access_password: config.access_password.clone(),
        device_name: config.device_name.clone(),
        local_ip,
        listen_port: config.listen_port,
        public_ipv6,
        is_rdp_session: rdp_st.is_rdp || rdp_st.is_disconnected,
        session_id: rdp_st.session_id,
        service_installed: svc_st.installed,
        service_running: svc_st.running,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionResult {
    pub mode: String,
    pub target: String,
    pub stream_port: u16,
}

fn classify_quic_mode(addr: &SocketAddr) -> String {
    match addr {
        SocketAddr::V6(_) => "IPv6".to_string(),
        SocketAddr::V4(v4) => {
            let ip = v4.ip();
            if ip.is_loopback() || ip.is_private() || ip.is_link_local() {
                "LAN".to_string()
            } else {
                "P2P".to_string()
            }
        }
    }
}

/// Connect to a remote OpenRemote peer via QUIC (supports device code or direct IP:port)
#[tauri::command]
pub async fn connect_to_device(
    address: String,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<ConnectionResult, String> {
    // Always cleanly tear down any previous session first so reconnects never fight stale sockets/tasks!
    if let Some(prev_ping) = state.ping_task.lock().await.take() {
        prev_ping.abort();
    }
    if let Some(prev_task) = state.receiver_task.lock().await.take() {
        prev_task.abort();
    }
    if let Some(prev_in) = state.native_input_task.lock().await.take() {
        prev_in.abort();
    }
    state.native_renderer.stop_session(&app_handle);
    if let Some(old_monitor) = state.clipboard_monitor.lock().await.take() {
        old_monitor.stop_monitoring();
    }
    if let Some(old_chan) = state.active_channel.lock().await.take() {
        old_chan.close().await;
    }
    if let Ok(mut q) = state.pending_frames.lock() {
        q.clear();
    }
    if let Ok(mut kf) = state.last_keyframe.lock() {
        *kf = None;
    }

    let mut last_err = String::new();
    let mut connected_channel = None;
    let mut connected_mode = String::new();
    let mut connected_target = String::new();

    let is_code_query = crate::signaling::is_device_code(&address);
    let signal_url_for_punch = {
        let config = state.config.lock().await;
        config.signal_server_url.clone()
    };

    // Ultra-fast cached direct reconnect (skips ~2s HTTP signaling lookup when reconnecting to recent peer!)
    if is_code_query {
        let cache_key = crate::signaling::clean_code(&address);
        let cached_addrs = state.peer_cache.lock().ok().and_then(|map| {
            map.get(&cache_key).and_then(|(addrs, ts)| {
                if ts.elapsed() < Duration::from_secs(600) {
                    Some(addrs.clone())
                } else {
                    None
                }
            })
        });

        if let Some(addrs) = cached_addrs {
            let mut join_set: tokio::task::JoinSet<Result<(String, SocketAddr, ConnectionChannel), String>> =
                tokio::task::JoinSet::new();
            for addr in addrs {
                let is_usable = match addr {
                    SocketAddr::V6(_) => true,
                    SocketAddr::V4(v4) => {
                        let oct = v4.ip().octets();
                        !v4.ip().is_loopback()
                            && !v4.ip().is_private()
                            && !v4.ip().is_link_local()
                            && !(oct[0] == 198 && (oct[1] == 18 || oct[1] == 19))
                    }
                };
                if !is_usable {
                    continue;
                }
                join_set.spawn(async move {
                    let client = QuicClient::new_for_target(&addr).map_err(|e| e.to_string())?;
                    match tokio::time::timeout(Duration::from_millis(350), client.connect(addr)).await {
                        Ok(Ok(chan)) => Ok((classify_quic_mode(&addr), addr, chan)),
                        Ok(Err(e)) => Err(e.to_string()),
                        Err(_) => Err("cached connect timeout".to_string()),
                    }
                });
            }

            while let Some(res) = join_set.join_next().await {
                if let Ok(Ok((mode, target_addr, chan))) = res {
                    log::info!("Instant cached {} reconnect winner: {}", mode, target_addr);
                    connected_mode = mode;
                    connected_target = target_addr.to_string();
                    connected_channel = Some(chan);
                    join_set.abort_all();
                    break;
                }
            }
        }
    }

    let target_addrs: Vec<SocketAddr> = if connected_channel.is_some() {
        Vec::new()
    } else if is_code_query {
        log::info!("Looking up candidates for device code '{}' via signaling server {}", address, signal_url_for_punch);
        let candidates = crate::signaling::lookup_peer_candidates(&signal_url_for_punch, &address)
            .await
            .map_err(|e| format!("信令服务器查询设备 '{}' 失败: {}", address, e))?;

        crate::diag_log(&format!(
            "Controller lookup {} candidates: {:?}",
            address, candidates
        ));

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

    if connected_channel.is_none() && target_addrs.is_empty() {
        return Err(format!("未找到目标设备 '{}' 的可用候选网络地址", address));
    }

    let mut public_v4_candidates = Vec::new();
    let mut lan_v4_candidates = Vec::new();
    let mut ipv6_candidates = Vec::new();
    let mut direct_ip_candidates = Vec::new();
    let mut seen_private_gateway_port: Option<u16> = None;

    for addr in target_addrs {
        match addr {
            SocketAddr::V4(v4) => {
                let oct = v4.ip().octets();
                let is_fake_tun = oct[0] == 198 && (oct[1] == 18 || oct[1] == 19);
                let is_private_lan = v4.ip().is_loopback()
                    || v4.ip().is_private()
                    || v4.ip().is_link_local()
                    || is_fake_tun;
                if is_code_query {
                    if !is_private_lan {
                        public_v4_candidates.push(addr);
                    } else if !is_fake_tun && !v4.ip().is_loopback() {
                        lan_v4_candidates.push(addr);
                        if v4.ip().is_private() && oct[3] == 1 {
                            seen_private_gateway_port = Some(addr.port());
                        }
                    }
                } else {
                    direct_ip_candidates.push(addr);
                }
            }
            SocketAddr::V6(_) => {
                if is_code_query {
                    ipv6_candidates.push(addr);
                } else {
                    direct_ip_candidates.push(addr);
                }
            }
        }
    }

    // If an older signaling server reported the host's LAN router gateway (e.g. 192.168.31.1),
    // infer the signaling server's real WAN IPv4 as a co-located P2P candidate!
    if is_code_query && public_v4_candidates.is_empty() {
        if let Some(gw_port) = seen_private_gateway_port {
            if let Some(sig_wan) = crate::nat::resolve_stun_addr_direct(&signal_url_for_punch).await {
                let inferred = SocketAddr::new(sig_wan.ip(), gw_port);
                crate::diag_log(&format!(
                    "Host behind signaling gateway -> inferring WAN P2P candidate {}",
                    inferred
                ));
                public_v4_candidates.push(inferred);
            }
        }
    }

    // If Host's STUN public port differs from its bound LAN port (e.g. router conntrack bump 28843 -> 51413),
    // also probe public_ip:lan_port in parallel so static port-forward rules connect reliably!
    if is_code_query && !public_v4_candidates.is_empty() && !lan_v4_candidates.is_empty() {
        let lan_port = lan_v4_candidates[0].port();
        let pub_ip = public_v4_candidates[0].ip();
        let alt = SocketAddr::new(pub_ip, lan_port);
        if !public_v4_candidates.contains(&alt) {
            public_v4_candidates.push(alt);
        }
    }

    if is_code_query && connected_channel.is_none() {
        crate::diag_log(&format!(
            "[CtrlConnect] peer={} | LAN={:?} | WAN_V4={:?} | V6={:?}",
            address, lan_v4_candidates, public_v4_candidates, ipv6_candidates
        ));
    }

    // Explicit manual IP:port input (not a 9-digit device code)
    if !direct_ip_candidates.is_empty() {
        for target_addr in direct_ip_candidates {
            if let Ok(client) = QuicClient::new_for_target(&target_addr) {
                match tokio::time::timeout(Duration::from_millis(1500), client.connect(target_addr)).await {
                    Ok(Ok(chan)) => {
                        connected_mode = classify_quic_mode(&target_addr);
                        connected_target = target_addr.to_string();
                        connected_channel = Some(chan);
                        break;
                    }
                    Ok(Err(e)) => last_err = e.to_string(),
                    Err(_) => last_err = format!("连接地址 {} 超时", target_addr),
                }
            }
        }
    }

    // Zero-delay parallel race: LAN direct + STUN UDP Hole-Punching (public IPv4) + Global Public IPv6 P2P!
    // Whichever direct P2P channel connects first wins immediately!
    if connected_channel.is_none()
        && is_code_query
        && (!lan_v4_candidates.is_empty() || !public_v4_candidates.is_empty() || !ipv6_candidates.is_empty())
    {
        let p2p_t0 = Instant::now();
        let mut join_set: tokio::task::JoinSet<Result<(String, SocketAddr, ConnectionChannel), String>> =
            tokio::task::JoinSet::new();

        for target_addr in lan_v4_candidates {
            join_set.spawn(async move {
                let client = QuicClient::new_for_target(&target_addr)
                    .map_err(|e| format!("LAN {}: {}", target_addr, e))?;
                match tokio::time::timeout(Duration::from_millis(450), client.connect(target_addr)).await {
                    Ok(Ok(chan)) => Ok(("LAN".to_string(), target_addr, chan)),
                    Ok(Err(e)) => Err(format!("LAN {}: {}", target_addr, e)),
                    Err(_) => Err(format!("LAN {} timeout (450ms)", target_addr)),
                }
            });
        }

        for target_addr in public_v4_candidates {
            let sig_url = signal_url_for_punch.clone();
            let dev_code = address.clone();
            join_set.spawn(async move {
                let ctrl_udp = crate::nat::bind_udp_direct(0)
                    .map_err(|e| format!("bind direct udp failed: {}", e))?;

                // Detailed STUN probe (RFC 5389 + custom STUN) bypassing Clash Fake-IP / TUN
                let stun_diag = crate::nat::probe_stun_detailed_direct(&ctrl_udp, &sig_url).await;
                let stun_res = stun_diag.best_addr();
                let is_symmetric = matches!(
                    (&stun_diag.rfc_mapped, &stun_diag.custom_mapped),
                    (Some((_, p1)), Some((_, p2))) if p1 != p2
                );

                // Under Symmetric NAT, pre-bind & cluster 5 extra sockets IMMEDIATELY right next to ctrl_udp
                // BEFORE signaling Host so all 6 NAT port mappings sit tight (+1..+6) around custom/rfc STUN.
                let mut extra_racers: Vec<std::net::UdpSocket> = Vec::new();
                if is_symmetric {
                    for _ in 0..5 {
                        if let Ok(extra_udp) = crate::nat::bind_udp_direct(0) {
                            extra_racers.push(extra_udp);
                        }
                    }
                }

                if let Some((ctrl_ip, ctrl_port)) = stun_res {
                    crate::diag_log(&format!(
                        "[CtrlPunch] NAT={} | rfc={:?} custom={:?} | punching Host {} @ {} (clustered={} sockets around {}:{})",
                        stun_diag.nat_type,
                        stun_diag.rfc_mapped,
                        stun_diag.custom_mapped,
                        dev_code,
                        target_addr,
                        1 + extra_racers.len(),
                        ctrl_ip,
                        ctrl_port
                    ));
                    // Open our own NAT pinholes FIRST on ctrl_udp + all clustered racers before signaling Host
                    for _ in 0..3 {
                        let _ = ctrl_udp.send_to(b"OPRM_PUNCH", target_addr);
                        for extra in &extra_racers {
                            let _ = extra.send_to(b"OPRM_PUNCH", target_addr);
                        }
                    }
                    let req_t0 = Instant::now();
                    // If RFC STUN differs from signaling STUN (Symmetric NAT), submit both endpoints so Host opens pinholes for both ranges
                    let extra_rfc = stun_diag
                        .rfc_mapped
                        .as_ref()
                        .filter(|(rip, rport)| *rip != ctrl_ip || *rport != ctrl_port)
                        .cloned();
                    let req_res = if let Some((rfc_ip, rfc_port)) = extra_rfc {
                        let (r1, _r2) = tokio::join!(
                            tokio::time::timeout(
                                Duration::from_millis(800),
                                crate::signaling::request_udp_punch(&sig_url, &dev_code, &ctrl_ip, ctrl_port),
                            ),
                            tokio::time::timeout(
                                Duration::from_millis(800),
                                crate::signaling::request_udp_punch(&sig_url, &dev_code, &rfc_ip, rfc_port),
                            )
                        );
                        r1
                    } else {
                        tokio::time::timeout(
                            Duration::from_millis(800),
                            crate::signaling::request_udp_punch(&sig_url, &dev_code, &ctrl_ip, ctrl_port),
                        )
                        .await
                    };
                    let punch_status = match &req_res {
                        Ok(Ok(())) => "Ok".to_string(),
                        Ok(Err(e)) => format!("Err({})", e),
                        Err(_) => "Timeout(800ms)".to_string(),
                    };
                    crate::diag_log(&format!(
                        "[CtrlPunch] request_udp_punch({}) took {}ms -> {}",
                        target_addr,
                        req_t0.elapsed().as_millis(),
                        punch_status
                    ));
                    // Poll all clustered sockets for up to 260ms to catch inbound OPRM_PUNCH / OPRM_PUNCH_ACK from Host
                    let _ = ctrl_udp.set_nonblocking(true);
                    for extra in &extra_racers {
                        let _ = extra.set_nonblocking(true);
                    }
                    let wait_t0 = Instant::now();
                    let mut verified_idx: Option<usize> = None;
                    let mut recv_buf = [0u8; 256];
                    while wait_t0.elapsed() < Duration::from_millis(260) {
                        while let Ok((n, src)) = ctrl_udp.recv_from(&mut recv_buf) {
                            let msg = String::from_utf8_lossy(&recv_buf[..n.min(32)]);
                            if msg.starts_with("OPRM_PUNCH") {
                                crate::diag_log(&format!(
                                    "[CtrlPunch] Socket #0 RECV '{}' from {} in {}ms -> bidirectional pinhole OPEN!",
                                    msg, src, wait_t0.elapsed().as_millis()
                                ));
                                verified_idx = Some(0);
                                break;
                            }
                        }
                        if verified_idx.is_some() {
                            break;
                        }
                        for (idx, extra) in extra_racers.iter().enumerate() {
                            while let Ok((n, src)) = extra.recv_from(&mut recv_buf) {
                                let msg = String::from_utf8_lossy(&recv_buf[..n.min(32)]);
                                if msg.starts_with("OPRM_PUNCH") {
                                    crate::diag_log(&format!(
                                        "[CtrlPunch] Racer #{} RECV '{}' from {} in {}ms -> bidirectional pinhole OPEN!",
                                        idx + 1, msg, src, wait_t0.elapsed().as_millis()
                                    ));
                                    verified_idx = Some(idx + 1);
                                    break;
                                }
                            }
                            if verified_idx.is_some() {
                                break;
                            }
                        }
                        if verified_idx.is_some() {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(12)).await;
                    }
                    let _ = ctrl_udp.set_nonblocking(false);
                    for extra in &extra_racers {
                        let _ = extra.set_nonblocking(false);
                    }
                    if verified_idx.is_none() {
                        crate::diag_log(&format!(
                            "[CtrlPunch] No punch reply across {} sockets during 260ms wait (target={}); racing QUIC sockets",
                            1 + extra_racers.len(),
                            target_addr
                        ));
                    }
                } else {
                    crate::diag_log(&format!(
                        "[CtrlPunch] STUN failed ({}), unilateral punch to {}",
                        stun_diag.nat_type, target_addr
                    ));
                    for _ in 0..4 {
                        let _ = ctrl_udp.send_to(b"OPRM_PUNCH", target_addr);
                        for extra in &extra_racers {
                            let _ = extra.send_to(b"OPRM_PUNCH", target_addr);
                        }
                    }
                }

                // Helper that sends punch packets continuously from a socket while racing QUIC handshake
                let race_socket = |sock: std::net::UdpSocket, extra_delay_ms: u64| async move {
                    if extra_delay_ms > 0 {
                        let _ = sock.send_to(b"OPRM_PUNCH", target_addr);
                        tokio::time::sleep(Duration::from_millis(extra_delay_ms)).await;
                    }
                    let punch_clone = sock.try_clone().ok();
                    let punch_bg = punch_clone.map(|ps| {
                        tokio::spawn(async move {
                            for _ in 0..22 {
                                let _ = ps.send_to(b"OPRM_PUNCH", target_addr);
                                tokio::time::sleep(Duration::from_millis(100)).await;
                            }
                        })
                    });

                    let quic_t0 = Instant::now();
                    let res = match QuicClient::new_with_socket(sock) {
                        Ok(client) => {
                            match tokio::time::timeout(Duration::from_millis(2400), client.connect(target_addr)).await {
                                Ok(Ok(chan)) => Ok(("P2P".to_string(), target_addr, chan)),
                                Ok(Err(e)) => Err(format!("P2P {} err ({}ms): {}", target_addr, quic_t0.elapsed().as_millis(), e)),
                                Err(_) => Err(format!("P2P {} timeout ({}ms)", target_addr, quic_t0.elapsed().as_millis())),
                            }
                        }
                        Err(e) => Err(format!("P2P client err: {}", e)),
                    };
                    if let Some(bg) = punch_bg {
                        bg.abort();
                    }
                    res
                };

                let mut p2p_race = tokio::task::JoinSet::new();
                p2p_race.spawn(race_socket(ctrl_udp, 0));
                for (idx, extra_udp) in extra_racers.into_iter().enumerate() {
                    p2p_race.spawn(race_socket(extra_udp, ((idx + 1) as u64) * 45));
                }

                let mut last_err = format!("P2P {} failed", target_addr);
                while let Some(joined) = p2p_race.join_next().await {
                    match joined {
                        Ok(Ok(ok)) => {
                            p2p_race.abort_all();
                            return Ok(ok);
                        }
                        Ok(Err(e)) => last_err = e,
                        Err(e) => last_err = format!("P2P join err: {}", e),
                    }
                }
                Err(last_err)
            });
        }

        for target_addr in ipv6_candidates {
            join_set.spawn(async move {
                log::info!("Attempting public IPv6 P2P QUIC to {}", target_addr);
                let client = QuicClient::new_for_target(&target_addr)
                    .map_err(|e| format!("{}: {}", target_addr, e))?;
                match tokio::time::timeout(Duration::from_millis(1500), client.connect(target_addr)).await {
                    Ok(Ok(chan)) => Ok(("IPv6".to_string(), target_addr, chan)),
                    Ok(Err(e)) => Err(format!("IPv6 {}: {}", target_addr, e)),
                    Err(_) => Err(format!("IPv6 {} timeout", target_addr)),
                }
            });
        }

        let mut attempt_errors = Vec::new();
        while let Some(res) = join_set.join_next().await {
            match res {
                Ok(Ok((mode, target_addr, chan))) => {
                    log::info!("Direct {} P2P winner: {}", mode, target_addr);
                    crate::diag_log(&format!(
                        "[CtrlConnect] WINNER mode={} target={} in {}ms",
                        mode,
                        target_addr,
                        p2p_t0.elapsed().as_millis()
                    ));
                    connected_mode = mode;
                    connected_target = target_addr.to_string();
                    connected_channel = Some(chan);
                    if let Ok(mut map) = state.peer_cache.lock() {
                        map.insert(
                            crate::signaling::clean_code(&address),
                            (vec![target_addr], Instant::now()),
                        );
                    }
                    join_set.abort_all();
                    break;
                }
                Ok(Err(e)) => {
                    attempt_errors.push(e.clone());
                    last_err = e;
                }
                Err(_) => {}
            }
        }
        if connected_channel.is_none() && !attempt_errors.is_empty() {
            crate::diag_log(&format!(
                "[CtrlConnect] Direct P2P/LAN/IPv6 failed in {}ms -> fallback Relay. Errors={:?}",
                p2p_t0.elapsed().as_millis(),
                attempt_errors
            ));
        }
    }

    let channel = match connected_channel {
        Some(c) => c,
        None => {
            // Direct QUIC connection to all candidates failed.
            // Prefer UDP-QUIC Relay via dxcool.cn:28844/udp (zero TCP head-of-line blocking!),
            // with seamless fallback to legacy TCP Relay 28845 if UDP 28844 is blocked.
            if crate::signaling::is_device_code(&address) {
                let (signal_url, auth_token) = {
                    let config = state.config.lock().await;
                    (config.signal_server_url.clone(), config.auth_token.clone())
                };
                let relay_addr = crate::signaling::parse_relay_address(&signal_url);
                let udp_t0 = Instant::now();
                match crate::transport::quic_relay::connect_quic_via_udp_relay(
                    &signal_url,
                    &address,
                    auth_token.as_deref(),
                )
                .await
                {
                    Ok(udp_quic_chan) => {
                        crate::diag_log(&format!(
                            "[CtrlConnect] WINNER mode=Relay-UDP-QUIC target={} in {}ms",
                            udp_quic_chan.peer_addr_label(),
                            udp_t0.elapsed().as_millis()
                        ));
                        connected_mode = "Relay".to_string();
                        connected_target = format!("UDP-QUIC({})", relay_addr);
                        udp_quic_chan
                    }
                    Err(udp_err) => {
                        crate::diag_log(&format!(
                            "[CtrlConnect] UDP-QUIC Relay unavailable in {}ms ({}) -> fallback TCP Relay",
                            udp_t0.elapsed().as_millis(),
                            udp_err
                        ));
                        match crate::signaling::join_relay_tunnel_with_token(
                            &relay_addr,
                            &address,
                            auth_token.as_deref(),
                        )
                        .await
                        {
                            Ok(tcp_stream) => {
                                log::info!("Successfully established TCP Relay tunnel to {}", address);
                                connected_mode = "Relay".to_string();
                                connected_target = relay_addr;
                                ConnectionChannel::new_tcp(tcp_stream)
                            }
                            Err(e) => {
                                return Err(format!("{}", e));
                            }
                        }
                    }
                }
            } else {
                return Err(format!("连接所有候选网络地址均失败: {}", last_err));
            }
        }
    };

    // Abort any previous session tasks & clear stale buffered keyframes
    if let Some(prev_ping) = state.ping_task.lock().await.take() {
        prev_ping.abort();
    }
    if let Some(prev_task) = state.receiver_task.lock().await.take() {
        prev_task.abort();
    }
    if let Ok(mut q) = state.pending_frames.lock() {
        q.clear();
    }
    if let Ok(mut kf) = state.last_keyframe.lock() {
        *kf = None;
    }
    if let Ok(mut m) = state.active_mode.lock() {
        *m = connected_mode.clone();
    }

    // Perform handshake (advertise cap:cursor/res/notice and send human-readable controller name+code)
    let my_id = {
        let config = state.config.lock().await;
        format!("{} ({})", config.device_name, config.device_code)
    };

    let handshake = Message::Handshake {
        client_id: my_id,
        token: Some("cap:cursor,cap:res,cap:notice,cap:h264".to_string()),
    };
    channel.send(&handshake).await.map_err(|e| e.to_string())?;

    // Start native Direct3D11 SwapChain H.264 decoder + prioritized coalesced Win32 input/ACK forwarder
    let (native_input_tx, mut native_input_rx) = tokio::sync::mpsc::unbounded_channel::<Message>();
    let latest_ack_ts = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let ack_notify = Arc::new(tokio::sync::Notify::new());
    let ack_ts_sender = Arc::clone(&latest_ack_ts);
    let ack_notify_sender = Arc::clone(&ack_notify);
    let move_notify = state.native_renderer.move_notify();
    let renderer_for_move = Arc::clone(&state.native_renderer);
    let input_channel = channel.clone();
    let native_in_handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                maybe_msg = native_input_rx.recv() => {
                    match maybe_msg {
                        Some(msg) => {
                            if input_channel.send(&msg).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
                _ = ack_notify_sender.notified() => {
                    let ts = ack_ts_sender.swap(0, std::sync::atomic::Ordering::AcqRel);
                    if ts != 0 && input_channel.send(&Message::VideoFrameAck { timestamp: ts }).await.is_err() {
                        break;
                    }
                }
                _ = move_notify.notified() => {
                    if let Some((rx, ry)) = renderer_for_move.take_pending_move() {
                        let msg = Message::MouseEvent {
                            x: rx,
                            y: ry,
                            button: None,
                            action: crate::protocol::MouseAction::Move,
                            delta_x: None,
                            delta_y: None,
                        };
                        if input_channel.send(&msg).await.is_err() {
                            break;
                        }
                    }
                }
            }
        }
    });
    *state.native_input_task.lock().await = Some(native_in_handle);
    if let Err(e) = state.native_renderer.start_session(
        &app_handle,
        native_input_tx,
        connected_mode == "Relay",
    ) {
        crate::diag_log(&format!("Native D3D11 session init warning (fallback canvas active): {}", e));
    }

    // Proactively negotiate optimal initial stream profile for the established transport mode
    let initial_quality = if connected_mode == "Relay" {
        Message::QualityConfig {
            quality: 50,
            max_width: 1280,
            fps: 30,
        }
    } else {
        Message::QualityConfig {
            quality: 68,
            max_width: 1920,
            fps: 60,
        }
    };
    let _ = channel.send(&initial_quality).await;

    // Store connection channel
    *state.active_channel.lock().await = Some(channel.clone());

    // Stop previous clipboard monitor if any
    if let Some(old_monitor) = state.clipboard_monitor.lock().await.take() {
        old_monitor.stop_monitoring();
    }

    // Automatically start bi-directional clipboard sync (text + files CF_HDROP) on controller
    let mut ctrl_clipboard = ClipboardMonitor::new();
    let rt_text = tokio::runtime::Handle::current();
    let rt_files = rt_text.clone();
    let channel_cb = channel.clone();
    let channel_files_cb = channel.clone();
    ctrl_clipboard.start_monitoring_dual(
        move |text| {
            let chan = channel_cb.clone();
            rt_text.spawn(async move {
                let msg = Message::ClipboardUpdate { content: text };
                if let Err(e) = chan.send(&msg).await {
                    log::warn!("Failed to sync local clipboard to peer: {}", e);
                }
            });
        },
        move |files| {
            let chan = channel_files_cb.clone();
            rt_files.spawn(async move {
                send_clipboard_files_over_channel(files, chan).await;
            });
        },
    );
    *state.clipboard_monitor.lock().await = Some(ctrl_clipboard);

    // Spawn dedicated Ping RTT task (tracked in AppState so disconnect() ALWAYS aborts it!)
    // IMPORTANT: Never place channel.recv() in tokio::select! because read_exact() is NOT cancel-safe!
    let ping_active = Arc::new(AtomicBool::new(true));
    let ping_active_flag = Arc::clone(&ping_active);
    let ping_channel = channel.clone();
    let ping_handle = tokio::spawn(async move {
        while ping_active_flag.load(Ordering::Acquire) {
            let now_ms = chrono::Utc::now().timestamp_millis();
            if ping_channel.send(&Message::Ping { timestamp: now_ms }).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1500)).await;
        }
    });
    *state.ping_task.lock().await = Some(ping_handle);

    // Spawn dedicated receiver loop (cancel-safe: never interrupted mid-read_exact!)
    let app_handle_clone = app_handle.clone();
    let native_renderer_clone = Arc::clone(&state.native_renderer);
    let receivers_clone = Arc::clone(&state.transfer_receivers);
    let pending_dir_clone = Arc::clone(&state.pending_dir_list);
    let frame_tx_clone = state.frame_tx.clone();
    let pending_frames_clone = Arc::clone(&state.pending_frames);
    let last_keyframe_clone = Arc::clone(&state.last_keyframe);
    let stream_port = state.stream_port;
    let recv_handle = tokio::spawn(async move {
        loop {
            match channel.recv().await {
                Ok(msg) => match msg {
                    Message::VideoFrameH264 {
                        data,
                        width,
                        height,
                        is_keyframe,
                        timestamp,
                    } => {
                        native_renderer_clone.push_h264_frame(data, width, height, is_keyframe);
                        latest_ack_ts.store(timestamp, std::sync::atomic::Ordering::Release);
                        ack_notify.notify_one();
                    }
                    Message::TileFrames {
                        screen_w,
                        screen_h,
                        tiles,
                        ..
                    } => {
                        if !tiles.is_empty() {
                            let is_full_keyframe = tiles.iter().any(|t| t.w >= screen_w && t.h >= screen_h / 4);
                            let pkt = pack_binary_frame_batch(screen_w, screen_h, &tiles);
                            if is_full_keyframe {
                                if let Ok(mut kf) = last_keyframe_clone.lock() {
                                    *kf = Some(Arc::clone(&pkt));
                                }
                            }
                            if frame_tx_clone.receiver_count() == 0 {
                                if let Ok(mut q) = pending_frames_clone.lock() {
                                    if q.len() >= 8 {
                                        q.pop_front();
                                    }
                                    q.push_back(pkt);
                                }
                            } else {
                                let _ = frame_tx_clone.send(pkt);
                            }
                        }
                    }
                    Message::ScreenFrame {
                        data,
                        width,
                        height,
                        ..
                    } => {
                        let pkt = pack_single_frame_batch(width as u16, height as u16, &data);
                        if let Ok(mut kf) = last_keyframe_clone.lock() {
                            *kf = Some(Arc::clone(&pkt));
                        }
                        if frame_tx_clone.receiver_count() == 0 {
                            if let Ok(mut q) = pending_frames_clone.lock() {
                                q.clear();
                                q.push_back(pkt);
                            }
                        } else {
                            let _ = frame_tx_clone.send(pkt);
                        }
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
                        let app_ev = app_handle_clone.clone();
                        handle_file_chunk_with_callback(
                            id,
                            offset,
                            data,
                            is_last,
                            Arc::clone(&receivers_clone),
                            move |count| {
                                let _ = app_ev.emit(
                                    "host-notice",
                                    serde_json::json!({
                                        "level": "info",
                                        "message": format!("已同步远程复制的 {} 个文件到本地剪贴板，可直接 Ctrl+V 粘贴", count),
                                    }),
                                );
                            },
                        )
                        .await;
                    }
                    Message::ListDirResp {
                        request_id,
                        current_path,
                        parent_path,
                        entries,
                        error,
                    } => {
                        if let Ok(mut map) = pending_dir_clone.lock() {
                            if let Some(tx) = map.remove(&request_id) {
                                let _ = tx.send(DirListingResult {
                                    current_path,
                                    parent_path,
                                    entries,
                                    error,
                                });
                            }
                        }
                    }
                    Message::ClipboardUpdate { content } => {
                        let _ = ClipboardMonitor::set_text_silent(&content).await;
                    }
                    Message::Ping { timestamp } => {
                        let _ = channel.send(&Message::Pong { timestamp }).await;
                    }
                    Message::Pong { timestamp } => {
                        let rtt = (chrono::Utc::now().timestamp_millis() - timestamp).max(1);
                        let _ = app_handle_clone.emit("connection-latency", rtt);
                    }
                    Message::CursorShape { cursor } => {
                        native_renderer_clone.set_cursor_css(&cursor);
                        let _ = app_handle_clone.emit("remote-cursor-shape", cursor);
                    }
                    Message::HostNotice { level, message } => {
                        let _ = app_handle_clone.emit(
                            "host-notice",
                            serde_json::json!({ "level": level, "message": message }),
                        );
                    }
                    _ => {}
                },
                Err(e) => {
                    crate::diag_log(&format!("[CtrlConnect] Channel receive ended: {}", e));
                    break;
                }
            }
        }
        ping_active.store(false, Ordering::Release);
        channel.close().await;
    });
    *state.receiver_task.lock().await = Some(recv_handle);

    Ok(ConnectionResult {
        mode: connected_mode,
        target: connected_target,
        stream_port,
    })
}

/// Send mouse input event to remote peer (clones channel without holding AppState lock across await)
#[tauri::command]
pub async fn send_mouse_event(
    event: crate::protocol::MouseEvent,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let channel_opt = state.active_channel.lock().await.clone();
    if let Some(channel) = channel_opt {
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
    let channel_opt = state.active_channel.lock().await.clone();
    if let Some(channel) = channel_opt {
        let msg = Message::KeyEvent { key, action };
        channel.send(&msg).await.map_err(|e| e.to_string())?;
    }
    Ok(())
}


/// Sync native D3D11 video child window rectangle (physical coordinates) with RemoteView div
#[tauri::command]
pub async fn sync_native_viewport(
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    visible: bool,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state
        .native_renderer
        .sync_viewport(&app_handle, x, y, width, height, visible);
    Ok(())
}

/// Enable or disable native Win32 mouse/keyboard capture on the D3D11 video child window
#[tauri::command]
pub async fn set_native_input_locked(
    locked: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.native_renderer.set_input_locked(locked);
    Ok(())
}

/// Disconnect current active session cleanly (immediately closes peer QUIC/TCP channel)
#[tauri::command]
pub async fn disconnect(
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.is_sharing.store(false, Ordering::SeqCst);
    if let Some(prev_ping) = state.ping_task.lock().await.take() {
        prev_ping.abort();
    }
    if let Some(prev_task) = state.receiver_task.lock().await.take() {
        prev_task.abort();
    }
    if let Some(prev_in) = state.native_input_task.lock().await.take() {
        prev_in.abort();
    }
    state.native_renderer.stop_session(&app_handle);
    if let Some(monitor) = state.clipboard_monitor.lock().await.take() {
        monitor.stop_monitoring();
    }
    if let Ok(mut q) = state.pending_frames.lock() {
        q.clear();
    }
    if let Ok(mut kf) = state.last_keyframe.lock() {
        *kf = None;
    }
    if let Some(old_chan) = state.active_channel.lock().await.take() {
        old_chan.close().await;
    }
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

        if total_bytes == 0 {
            let chunk_msg = Message::FileChunk {
                id: tid.clone(),
                offset: 0,
                data: Vec::new(),
                is_last: true,
            };
            if let Err(e) = channel.send(&chunk_msg).await {
                log::error!("Failed sending empty file chunk: {}", e);
                let mut progresses = progresses_ref.lock().await;
                if let Some(p) = progresses.get_mut(&tid) {
                    p.status = crate::file_transfer::TransferStatus::Failed;
                }
            } else {
                let mut progresses = progresses_ref.lock().await;
                if let Some(p) = progresses.get_mut(&tid) {
                    p.status = crate::file_transfer::TransferStatus::Completed;
                }
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

    let channel_opt = state.active_channel.lock().await.clone();
    if let Some(channel) = channel_opt {
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
    let channel_opt = state.active_channel.lock().await.clone();
    if let Some(channel) = channel_opt {
        match shortcut.as_str() {
            "CtrlAltDel" => {
                // Native Secure Attention Sequence via LocalSystem sas.dll!SendSAS (works on Lock Screen!)
                let _ = channel.send(&Message::SendSas).await;
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
            "WinE" => {
                let _ = channel.send(&Message::KeyEvent { key: "Meta".to_string(), action: crate::protocol::KeyAction::Down }).await;
                let _ = channel.send(&Message::KeyEvent { key: "e".to_string(), action: crate::protocol::KeyAction::Click }).await;
                let _ = channel.send(&Message::KeyEvent { key: "Meta".to_string(), action: crate::protocol::KeyAction::Up }).await;
            }
            "TaskMgr" => {
                let _ = channel.send(&Message::KeyEvent { key: "Control".to_string(), action: crate::protocol::KeyAction::Down }).await;
                let _ = channel.send(&Message::KeyEvent { key: "Shift".to_string(), action: crate::protocol::KeyAction::Down }).await;
                let _ = channel.send(&Message::KeyEvent { key: "Escape".to_string(), action: crate::protocol::KeyAction::Click }).await;
                let _ = channel.send(&Message::KeyEvent { key: "Shift".to_string(), action: crate::protocol::KeyAction::Up }).await;
                let _ = channel.send(&Message::KeyEvent { key: "Control".to_string(), action: crate::protocol::KeyAction::Up }).await;
            }
            _ => {}
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthStatus {
    pub authenticated: bool,
    pub username: Option<String>,
    pub token: Option<String>,
}

/// Get current logged-in account status
#[tauri::command]
pub async fn get_auth_status(state: State<'_, AppState>) -> Result<AuthStatus, String> {
    let cfg = state.config.lock().await;
    let token = cfg.auth_token.clone().filter(|t| !t.trim().is_empty());
    let username = cfg.auth_username.clone().filter(|u| !u.trim().is_empty());
    Ok(AuthStatus {
        authenticated: token.is_some(),
        username,
        token,
    })
}

/// Login to signaling & relay server account
#[tauri::command]
pub async fn login_account(
    username: String,
    password: String,
    state: State<'_, AppState>,
) -> Result<AuthStatus, String> {
    let signal_url = {
        let cfg = state.config.lock().await;
        cfg.signal_server_url.clone()
    };

    let (user, token) = crate::signaling::auth_login(&signal_url, &username, &password)
        .await
        .map_err(|e| e.to_string())?;

    let mut cfg = state.config.lock().await;
    cfg.auth_username = Some(user.clone());
    cfg.auth_token = Some(token.clone());
    let _ = cfg.save();

    Ok(AuthStatus {
        authenticated: true,
        username: Some(user),
        token: Some(token),
    })
}

/// Register a new account on signaling & relay server
#[tauri::command]
pub async fn register_account(
    username: String,
    password: String,
    state: State<'_, AppState>,
) -> Result<AuthStatus, String> {
    let signal_url = {
        let cfg = state.config.lock().await;
        cfg.signal_server_url.clone()
    };

    let (user, token) = crate::signaling::auth_register(&signal_url, &username, &password)
        .await
        .map_err(|e| e.to_string())?;

    let mut cfg = state.config.lock().await;
    cfg.auth_username = Some(user.clone());
    cfg.auth_token = Some(token.clone());
    let _ = cfg.save();

    Ok(AuthStatus {
        authenticated: true,
        username: Some(user),
        token: Some(token),
    })
}

/// Dynamically adjust remote stream quality mode ("ultra", "balanced", "speed")
#[tauri::command]
pub async fn set_stream_quality(
    mode: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let is_relay = state
        .active_mode
        .lock()
        .map(|m| m.as_str() == "Relay")
        .unwrap_or(false);

    let (quality, max_width, fps) = match (mode.as_str(), is_relay) {
        ("ultra", false) => (78u8, 0u32, 60u8),
        ("ultra", true) => (64u8, 1600u32, 60u8),
        ("speed", false) => (48u8, 1280u32, 60u8),
        ("speed", true) => (42u8, 960u32, 30u8),
        (_, false) => (68u8, 1920u32, 60u8),
        (_, true) => (54u8, 1280u32, 45u8),
    };

    let channel_opt = state.active_channel.lock().await.clone();
    if let Some(channel) = channel_opt {
        channel
            .send(&Message::QualityConfig {
                quality,
                max_width,
                fps,
            })
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Request remote host to change physical display resolution (0x0 restores native resolution).
/// Falls back automatically on host to adjusting streaming resolution if OS display switching is unavailable.
#[tauri::command]
pub async fn set_remote_resolution(
    width: u32,
    height: u32,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let channel_opt = state.active_channel.lock().await.clone();
    let channel = channel_opt.ok_or_else(|| "当前未建立远程连接通道".to_string())?;
    channel
        .send(&Message::SetResolution { width, height })
        .await
        .map_err(|e| e.to_string())
}

/// Ask remote host to immediately emit a fresh full-screen keyframe (100% backward compatible via QualityConfig)
#[tauri::command]
pub async fn request_keyframe(state: State<'_, AppState>) -> Result<(), String> {
    let is_relay = state
        .active_mode
        .lock()
        .map(|m| m.as_str() == "Relay")
        .unwrap_or(false);
    let msg = if is_relay {
        Message::QualityConfig {
            quality: 54,
            max_width: 1280,
            fps: 45,
        }
    } else {
        Message::QualityConfig {
            quality: 68,
            max_width: 1920,
            fps: 60,
        }
    };
    let channel_opt = state.active_channel.lock().await.clone();
    if let Some(channel) = channel_opt {
        channel.send(&msg).await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Detach this local machine's Windows RDP session back to physical Console (`tscon <sid> /dest:console`)
#[tauri::command]
pub async fn detach_local_rdp() -> Result<String, String> {
    tokio::task::spawn_blocking(crate::input::detach_rdp_to_console)
        .await
        .map_err(|e| format!("切换任务异常: {}", e))?
        .map_err(|e| e.to_string())
}

/// Ask remote host to detach its Windows RDP session back to physical Console (`tscon <sid> /dest:console`)
#[tauri::command]
pub async fn detach_remote_rdp(state: State<'_, AppState>) -> Result<(), String> {
    let channel_opt = state.active_channel.lock().await.clone();
    let channel = channel_opt.ok_or_else(|| "当前未建立远程连接通道".to_string())?;
    channel
        .send(&Message::DetachRdp)
        .await
        .map_err(|e| e.to_string())
}

/// Query Windows LocalSystem service status
#[tauri::command]
pub async fn get_service_status() -> Result<crate::service::SystemServiceStatus, String> {
    Ok(crate::service::query_service_status())
}

/// Install (or upgrade) and start OpenRemoteService as LocalSystem (supports Lock Screen, UAC, pre-login access)
#[tauri::command]
pub async fn install_system_service(
    state: State<'_, AppState>,
) -> Result<crate::service::SystemServiceStatus, String> {
    // Stop in-process fallback host daemon first so ports 28843/28844 are freed immediately for SYSTEM worker
    if let Some(handle) = state.fallback_host.lock().await.take() {
        handle.stop().await;
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let status = tokio::task::spawn_blocking(crate::service::install_and_start_service)
        .await
        .map_err(|e| format!("安装服务任务异常: {}", e))?
        .map_err(|e| e.to_string())?;

    Ok(status)
}

/// Stop and uninstall OpenRemoteService, and seamlessly resume in-process fallback host daemon
#[tauri::command]
pub async fn uninstall_system_service(
    state: State<'_, AppState>,
) -> Result<crate::service::SystemServiceStatus, String> {
    let status = tokio::task::spawn_blocking(crate::service::uninstall_service)
        .await
        .map_err(|e| format!("卸载服务任务异常: {}", e))?
        .map_err(|e| e.to_string())?;

    // Resume in-process host daemon so the user stays online immediately
    if !status.running && state.fallback_host.lock().await.is_none() {
        let config = state.config.lock().await.clone();
        let handle = crate::spawn_host_daemon(
            config,
            state.active_channel.clone(),
            state.input_simulator.clone(),
            state.transfer_receivers.clone(),
            state.quic_server.clone(),
        );
        *state.fallback_host.lock().await = Some(handle);
    }

    Ok(status)
}

/// Read live incoming host control session state (works across SYSTEM service worker and GUI)
#[tauri::command]
pub async fn get_host_control_status() -> Result<crate::HostControlStatus, String> {
    Ok(crate::read_host_control_status())
}

/// Disconnect an active remote controller currently viewing/controlling this PC
#[tauri::command]
pub async fn stop_incoming_control() -> Result<(), String> {
    crate::request_host_disconnect();
    Ok(())
}

/// Browse local directories & files for the File Transfer view
#[tauri::command]
pub async fn list_local_files(path: Option<String>) -> Result<DirListingResult, String> {
    tokio::task::spawn_blocking(move || read_local_dir_entries(path.as_deref()))
        .await
        .map_err(|e| format!("读取本地目录失败: {}", e))
}

/// Request directory listing from the currently connected remote peer
#[tauri::command]
pub async fn list_remote_files(
    path: Option<String>,
    state: State<'_, AppState>,
) -> Result<DirListingResult, String> {
    let channel = state
        .active_channel
        .lock()
        .await
        .clone()
        .ok_or_else(|| "当前未连接到任何远程设备".to_string())?;

    let request_id = Uuid::new_v4().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel();
    {
        let mut map = state
            .pending_dir_list
            .lock()
            .map_err(|_| "目录查询状态锁异常".to_string())?;
        map.insert(request_id.clone(), tx);
    }

    if let Err(e) = channel
        .send(&Message::ListDirReq {
            request_id: request_id.clone(),
            path: path.filter(|s| !s.trim().is_empty()),
        })
        .await
    {
        if let Ok(mut map) = state.pending_dir_list.lock() {
            map.remove(&request_id);
        }
        return Err(format!("请求远程目录失败: {}", e));
    }

    match tokio::time::timeout(Duration::from_secs(6), rx).await {
        Ok(Ok(res)) => Ok(res),
        Ok(Err(_)) => Err("远程目录响应通道已关闭".to_string()),
        Err(_) => {
            if let Ok(mut map) = state.pending_dir_list.lock() {
                map.remove(&request_id);
            }
            Err("读取远程目录超时（6秒）".to_string())
        }
    }
}

/// Request the connected remote peer to stream a file into the local Downloads\OpenRemote folder
#[tauri::command]
pub async fn download_remote_file(
    remote_path: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let channel = state
        .active_channel
        .lock()
        .await
        .clone()
        .ok_or_else(|| "当前未连接到任何远程设备".to_string())?;

    channel
        .send(&Message::PullFileReq { path: remote_path })
        .await
        .map_err(|e| format!("发起远程文件下载失败: {}", e))?;

    Ok(get_default_download_dir().to_string_lossy().to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagLogResponse {
    pub path: String,
    pub content: String,
}

fn active_diag_log_path() -> PathBuf {
    AppConfig::config_path()
        .parent()
        .map(|p| p.join("openremote.log"))
        .unwrap_or_else(|| PathBuf::from("openremote.log"))
}

/// Read recent lines from OpenRemote diagnostic log (for in-app NAT/P2P/Relay/FPS troubleshooting)
#[tauri::command]
pub async fn get_diag_logs(max_lines: Option<usize>) -> Result<DiagLogResponse, String> {
    let limit = max_lines.unwrap_or(200).clamp(20, 1000);
    let path = active_diag_log_path();
    let path_str = path.to_string_lossy().to_string();
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    let lines: Vec<&str> = raw.lines().collect();
    let start = lines.len().saturating_sub(limit);
    Ok(DiagLogResponse {
        path: path_str,
        content: lines[start..].join("\n"),
    })
}

/// Clear active diagnostic log
#[tauri::command]
pub async fn clear_diag_logs() -> Result<(), String> {
    let path = active_diag_log_path();
    let _ = std::fs::write(&path, b"");
    Ok(())
}

/// Open folder containing OpenRemote diagnostic logs
#[tauri::command]
pub async fn open_diag_log_dir() -> Result<(), String> {
    let path = active_diag_log_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
        #[cfg(target_os = "windows")]
        {
            std::process::Command::new("explorer")
                .arg(dir)
                .spawn()
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Logout current account
#[tauri::command]
pub async fn logout_account(state: State<'_, AppState>) -> Result<AuthStatus, String> {
    let mut cfg = state.config.lock().await;
    cfg.auth_username = None;
    cfg.auth_token = None;
    let _ = cfg.save();

    Ok(AuthStatus {
        authenticated: false,
        username: None,
        token: None,
    })
}
