# OpenRemote 接口与通信协议规范 (API Specification)

本文档定义了 OpenRemote 的前后端 IPC 接口、信令服务器 RESTful API 以及 TCP Relay 穿透中继协议。

---

## 1. 信令服务器 API (HTTP RESTful)

信令服务默认监听端口：`28844`（可通过环境变量 `PORT` 修改）。

### 1.1 健康检查
- **路径**：`GET /health`
- **响应**：
```json
{
  "status": "ok",
  "version": "0.1.0",
  "online_devices": 3
}
```

### 1.2 设备心跳注册与更新
- **路径**：`POST /register`
- **请求头**：`Content-Type: application/json`
- **请求体**：
```json
{
  "device_id": "849201948",
  "name": "My-Workstation",
  "ipv6": "2408:832e:e89:xxxx:xxxx:xxxx:xxxx:xxxx",
  "lan_ip": "192.168.1.100",
  "port": 28843
}
```
- **响应**：
```json
{
  "success": true,
  "registered_at": 1730170800000
}
```

### 1.3 查找被控端设备信息
- **路径**：`GET /device/:device_id`
- **示例**：`GET /device/849201948`
- **响应 (成功)**：
```json
{
  "device_id": "849201948",
  "name": "My-Workstation",
  "ipv6": "2408:832e:e89:xxxx:xxxx:xxxx:xxxx:xxxx",
  "lan_ip": "192.168.1.100",
  "port": 28843,
  "last_seen": 1730170805120
}
```
- **响应 (设备不存在或离线)**：
```json
{
  "error": "Device not found or offline"
}
```

---

## 2. STUN / 端口反射服务 (UDP)

- **协议**：UDP
- **端口**：`28844`
- **行为**：客户端向服务器发送任意 UDP 数据包，服务器直接原样回送客户端当前的公网出口 IP 与反射端口，用于 NAT 类型分析与打洞。

---

## 3. TCP Relay 穿透中继协议

Relay 中继隧道默认监听端口：`28845`（可通过环境变量 `RELAY_PORT` 修改）。

### 3.1 握手指令行规范
连接建立后，前置通过 ASCII 行指令进行会话配对：

1. **被控端发起监听 (Host/Agent)**：
   - 发送：`REGISTER <device_id>\n`
   - 服务器响应：`OK\n`
   - 服务器将该 TCP 连接放入待连接挂起池中等待接入。

2. **控制端接入连接 (Client/Viewer)**：
   - 发送：`JOIN <device_id>\n`
   - 服务器在内存中查找对应待连接通道：
     - 若找到：响应 `OK\n`，立即将控制端与被控端的两个 Socket 通过全双工流管道粘合（`socketA.pipe(socketB); socketB.pipe(socketA);`）；
     - 若未找到或已超时：响应 `ERROR: Peer offline\n` 并断开连接。

3. **进入全双工二进制透传**：
   配对成功后，中继服务器不介入任何业务解析，透明全速转发底层 Bincode 帧。

---

## 4. Tauri IPC 接口 (前端与 Rust 后端)

前端通过 `@tauri-apps/api/core` 中的 `invoke` 发起指令调用。

### 4.1 屏幕与捕获管理

#### `get_screen_info`
- **说明**：获取系统主屏幕分辨率与显示器基本信息。
- **参数**：无
- **返回**：
```typescript
interface ScreenInfo {
  width: number;
  height: number;
  scale_factor: number;
  name: string;
}
```

#### `start_capture`
- **说明**：开启屏幕截屏并以指定帧率串流。
- **参数**：
```typescript
{
  fps: number; // 默认 30 或 60
}
```

#### `stop_capture`
- **说明**：停止屏幕截屏。

---

### 4.2 键盘与鼠标模拟

#### `send_mouse_input`
- **说明**：模拟鼠标移动、单击、双击、滚轮滑动。
- **参数**：
```typescript
{
  event: {
    event_type: "MouseMove" | "MouseDown" | "MouseUp" | "MouseScroll";
    x: number;
    y: number;
    button?: "Left" | "Right" | "Middle";
    scroll_delta?: number;
  }
}
```

#### `send_keyboard_input`
- **说明**：模拟键盘按键按下与释放。
- **参数**：
```typescript
{
  event: {
    event_type: "KeyDown" | "KeyUp";
    key: string;       // 字符如 'a', 'b', 或键名如 'Enter', 'Escape', 'F1'-'F12'
    modifiers?: {
      ctrl: boolean;
      alt: boolean;
      shift: boolean;
      meta: boolean;
    };
  }
}
```

---

### 4.3 文件传输与管理

#### `send_file`
- **说明**：向远程对端发送指定路径的本地文件。
- **参数**：
```typescript
{
  path: string; // 本地待发送文件的绝对路径
}
```

#### `open_download_dir`
- **说明**：调用系统文件管理器（Windows Explorer / macOS Finder）打开 OpenRemote 文件接收下载目录。
- **参数**：无

---

### 4.4 网络与设备管理

#### `get_device_id`
- **说明**：获取本机唯一的 9 位设备代码。
- **返回**：`string` (例如 `"849-201-948"`)

#### `get_network_info`
- **说明**：获取本机的网络环境信息。
- **返回**：
```typescript
interface NetworkInfo {
  lan_ip: string;
  public_ipv6: string | null;
  quic_port: number;
  signaling_server: string;
}
```

#### `connect_remote`
- **说明**：建立对远端设备的控制连接。
- **参数**：
```typescript
{
  target: string;       // 远程 IPv4、IPv6 地址或 9 位设备码
  use_relay?: boolean;  // 是否强制使用中继服务
}
```

---

### 4.5 Tauri 事件监听 (Backend -> Frontend)

前端使用 `listen(event_name, handler)` 监听后端主动推送的事件：

| 事件名 | 负载结构 (Payload) | 说明 |
| :--- | :--- | :--- |
| `screen-frame` | `{ data: string, width: number, height: number }` | 屏幕压缩帧 Base64 图像流 |
| `file-transfer-progress` | `{ file_id: string, name: string, progress: number, speed: number }` | 文件发送/接收实时进度与传输速率 |
| `file-transfer-complete` | `{ file_id: string, name: string, path: string }` | 文件传输完成通知 |
| `connection-state` | `{ state: "connected" \| "disconnected" \| "reconnecting" }` | 传输链路连通状态变动 |
