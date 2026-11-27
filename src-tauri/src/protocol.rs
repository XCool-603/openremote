use serde::{Deserialize, Serialize};

/// Binary frame header for efficient stream parsing
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[repr(C)]
pub struct FrameHeader {
    pub magic: [u8; 4],   // b"OPRM"
    pub version: u8,      // Protocol version, e.g. 1
    pub msg_type: u8,     // 1 = Protocol JSON/bincode, 2 = Raw video, 3 = Raw file
    pub payload_len: u32, // Length of following payload in bytes
}

impl FrameHeader {
    pub const MAGIC: [u8; 4] = *b"OPRM";
    pub const CURRENT_VERSION: u8 = 1;

    pub fn new(msg_type: u8, payload_len: u32) -> Self {
        Self {
            magic: Self::MAGIC,
            version: Self::CURRENT_VERSION,
            msg_type,
            payload_len,
        }
    }

    pub fn to_bytes(&self) -> [u8; 10] {
        let mut buf = [0u8; 10];
        buf[0..4].copy_from_slice(&self.magic);
        buf[4] = self.version;
        buf[5] = self.msg_type;
        buf[6..10].copy_from_slice(&self.payload_len.to_be_bytes());
        buf
    }

    pub fn from_bytes(bytes: &[u8; 10]) -> Option<Self> {
        let mut magic = [0u8; 4];
        magic.copy_from_slice(&bytes[0..4]);
        if magic != Self::MAGIC {
            return None;
        }
        let version = bytes[4];
        let msg_type = bytes[5];
        let payload_len = u32::from_be_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]);

        Some(Self {
            magic,
            version,
            msg_type,
            payload_len,
        })
    }
}

/// Mouse button identifier
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    Back,
    Forward,
    ScrollUp,
    ScrollDown,
}

/// Mouse action identifier
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum MouseAction {
    Move,
    Down,
    Up,
    Click,
    DoubleClick,
    Scroll,
}

/// Key action identifier
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum KeyAction {
    Down,
    Up,
    Click,
}

/// Mouse event payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MouseEvent {
    pub x: i32,
    pub y: i32,
    pub button: Option<MouseButton>,
    pub action: MouseAction,
    pub delta_x: Option<i32>,
    pub delta_y: Option<i32>,
}

/// Key event payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyEvent {
    pub key: String,
    pub action: KeyAction,
}

/// Single changed rectangular tile encoded as JPEG
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirtyTile {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    pub data: Vec<u8>,
}

/// File or folder entry returned by directory browsing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirEntryInfo {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size_bytes: u64,
    pub modified: String,
}

/// Core protocol messages exchanged between OpenRemote clients & host
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    /// Screen video frame containing encoded image data
    ScreenFrame {
        data: Vec<u8>,
        width: u32,
        height: u32,
        timestamp: i64,
    },
    /// Mouse movement and click event
    MouseEvent {
        x: i32,
        y: i32,
        button: Option<MouseButton>,
        action: MouseAction,
        delta_x: Option<i32>,
        delta_y: Option<i32>,
    },
    /// Keyboard key event
    KeyEvent {
        key: String,
        action: KeyAction,
    },
    /// Text typing event
    TypeText {
        text: String,
    },
    /// File transfer start metadata
    FileTransferStart {
        id: String,
        filename: String,
        total_bytes: u64,
        file_hash: Option<String>,
    },
    /// File transfer chunk
    FileChunk {
        id: String,
        offset: u64,
        data: Vec<u8>,
        is_last: bool,
    },
    /// Clipboard synchronization update
    ClipboardUpdate {
        content: String,
    },
    /// Ping message for latency measurement & heartbeat
    Ping {
        timestamp: i64,
    },
    /// Pong response for latency measurement & heartbeat
    Pong {
        timestamp: i64,
    },
    /// Auth request or handshake
    Handshake {
        client_id: String,
        token: Option<String>,
    },
    /// Handshake acknowledgment
    HandshakeAck {
        success: bool,
        message: Option<String>,
    },
    /// Dynamically adjust stream quality and max resolution
    QualityConfig {
        quality: u8,
        max_width: u32,
        fps: u8,
    },
    /// High-efficiency incremental dirty-tile frame batch
    TileFrames {
        screen_w: u16,
        screen_h: u16,
        tiles: Vec<DirtyTile>,
        timestamp: i64,
    },
    /// Remote host cursor shape update (standard CSS cursor keyword, e.g. "default", "text", "pointer")
    CursorShape {
        cursor: String,
    },
    /// Request remote host to change display resolution (0x0 = native/restore stream scale)
    SetResolution {
        width: u32,
        height: u32,
    },
    /// Host diagnostic/status notice sent back to controller UI
    HostNotice {
        level: String,
        message: String,
    },
    /// Request remote host to detach its Windows RDP session back to local Console (`tscon /dest:console`)
    DetachRdp,
    /// Trigger native Windows Secure Attention Sequence (Ctrl+Alt+Del) via LocalSystem SendSAS
    SendSas,
    /// Request remote host directory listing (empty/None path = user Desktop/Drives root)
    ListDirReq {
        request_id: String,
        path: Option<String>,
    },
    /// Directory listing response from remote host
    ListDirResp {
        request_id: String,
        current_path: String,
        parent_path: Option<String>,
        entries: Vec<DirEntryInfo>,
        error: Option<String>,
    },
    /// Request remote host to send a file back to the controller
    PullFileReq {
        path: String,
    },
    /// Hardware-encoded H.264 Annex-B video frame (keyframes include SPS+PPS)
    VideoFrameH264 {
        data: Vec<u8>,
        width: u16,
        height: u16,
        is_keyframe: bool,
        timestamp: i64,
    },
    /// Controller acknowledgment of received H.264 frame timestamp for bufferbloat prevention
    VideoFrameAck {
        timestamp: i64,
    },
}
