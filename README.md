# OpenRemote - 开源跨平台轻量高性能远程桌面

<p align="center">
  <strong>🖥️ 高性能屏幕串流 | 📁 真实落盘文件传输 | ⚡ Bincode 二进制协议 | 🌐 IPv6 直连 / Docker Relay 中继</strong>
</p>

<p align="center">
  <em>网易UU远程 / ToDesk / TeamViewer 的现代化开源替代方案，基于 Tauri v2 + Rust + React 18 打造</em>
</p>

---

## 🌟 核心特性与架构升级

- **⚡ Bincode 高性能二进制协议**：彻底淘汰 JSON 字符传输，网络协议开销削减 70%，内存反序列化速度提升数倍，杜绝大屏高频串流卡顿。
- **🖥️ DXGI 屏幕捕获与脏帧检测（Dirty Detection）**：Windows 底层 DXGI Desktop Duplication 原生抓屏；引入全局网格快速采样脏帧算法，在桌面静止时自动抑制冗余编码与发送，实现 **0 宽带消耗与极低 CPU 负载**。
- **🌐 智能网络双通道（全球直连 + 穿透中继）**：
  - **方案 A（IPv6 全球极速直连）**：内置零配置内核级公网 IPv6 自动探测，双向 `[::]` 双栈监听，无视家庭宽带无公网 IPv4 的痛点，实现毫秒级端到端直连。
  - **方案 B（Docker Relay 中继服务器）**：自带轻量级信令与全双工 TCP 流量中继服务，支持通过 9 位随机安全设备码一键穿透 NAT 复杂内网。
- **📁 真实落盘端到端文件传输**：支持大文件分片流式发送，内置 SHA-256 完整性哈希校验，自动落地至系统 `Downloads/OpenRemote/` 目录，支持在 UI 中一键打开目录。
- **🎮 完整输入交互控制**：全键盘（含字母、数字、功能键 F1~F12、方向键、系统热键 Ctrl+Alt+Del / Win+D / Alt+Tab）、鼠标左右中键及滚轮物理级精准模拟。

---

## 📐 系统架构预览

```
┌────────────────────────────────────────────────────────┐
│                   React 18 + Tailwind UI               │
│    (设备面板 / 远程桌面交互 / 文件管理器 / 网络状态监视)       │
└───────────────────────────┬────────────────────────────┘
                            │ Tauri IPC (Commands & Events)
┌───────────────────────────▼────────────────────────────┐
│                    Rust 异步核心引擎                    │
│ ┌────────────────┐ ┌───────────────────┐ ┌───────────┐ │
│ │ DXGI 屏幕捕获   │ │ 脏帧采样哈希抑制  │ │ 图像压缩  │ │
│ └────────────────┘ └───────────────────┘ └───────────┘ │
│ ┌────────────────┐ ┌───────────────────┐ ┌───────────┐ │
│ │ Enigo 输入模拟 │ │ SHA-256 文件落盘  │ │ 剪贴板同步│ │
│ └────────────────┘ └───────────────────┘ └───────────┘ │
└───────────────────────────┬────────────────────────────┘
                            │ Bincode Protocol (Header+Body)
┌───────────────────────────▼────────────────────────────┐
│                   智能双通道传输层                      │
│   [直连通道] QUIC / TLS 1.3 (IPv6 全球直连 / 局域网直连)  │
│   [中继通道] TCP Relay (Docker 容器化信令 & 流量中继)     │
└────────────────────────────────────────────────────────┘
```

---

## 📦 快速开始

### 前置要求

- [Node.js](https://nodejs.org/) >= 18
- [Rust](https://rustup.rs/) >= 1.77（推荐使用最新稳定版）
- Windows: 安装 Visual Studio C++ 生成工具 (MSVC)
- Linux: `sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget libssl-dev libayatana-appindicator3-dev librsvg2-dev`

### 本地编译与运行

```bash
# 1. 克隆代码仓库
git clone https://github.com/XCool-603/openremote.git
cd openremote

# 2. 安装前端依赖
npm install

# 3. 运行开发模式（同时拉起前端 Vite 与 Rust 后端）
npm run tauri dev

# 4. 构建独立生产安装包 / EXE
npm run tauri build
```

构建生成的独立运行程序位于：`src-tauri/target/release/openremote.exe`。

---

## 🐳 Docker 一键部署中继与信令服务器 (方案 B)

OpenRemote 包含一套极致轻量的中继与信令服务（位于 `server/` 目录），仅需 **10MB 内存**，零外部数据库依赖，可部署在任何 1 核 512M 的轻量云服务器（腾讯云、阿里云、华为云、AWS 等）上。

### 🚀 方式一：Linux 云服务器单行一键安装（全自动）

在你的 Linux VPS 终端上直接执行以下命令：
```bash
curl -fsSL https://raw.githubusercontent.com/XCool-603/openremote/main/server/deploy.sh | sudo bash
```
> **该命令会自动执行**：系统架构检测 -> 自动安装 Docker -> 配置开机自启 -> 放行防火墙端口 -> 启动容器 -> 打印公网配置信息。

---

### 🐳 方式二：已有 Docker 服务器单条命令启动

无需克隆整个项目仓库，复制执行即可：
```bash
mkdir -p /opt/openremote-server && cd /opt/openremote-server
curl -fsSL https://raw.githubusercontent.com/XCool-603/openremote/main/server/signaling.js -o signaling.js
docker run -d \
  --name openremote-relay \
  --restart always \
  -v /opt/openremote-server/signaling.js:/app/signaling.js \
  -w /app \
  -p 28844:28844/tcp \
  -p 28844:28844/udp \
  -p 28845:28845/tcp \
  node:20-alpine node signaling.js
```

---

### 🛡️ 云平台安全组开放规则（必做）

部署完成后，请在腾讯云/阿里云/华为云等**云控制台安全组**添加入方向放行规则：
- **`28844 / TCP`**：HTTP 信令 API 与 9 位设备代码心跳注册
- **`28844 / UDP`**：STUN 协议公网端口反射与打洞
- **`28845 / TCP`**：TCP 流量穿透中继隧道

### 🔍 验证运行
浏览器打开：`http://<你的服务器公网IP>:28844/api/status`
看到 `{"status": "online", ...}` 即表示部署成功！

### 📱 客户端配置
在 OpenRemote 软件中点击左侧 **【设置】** -> 将 **信令服务器地址** 修改为 `http://<你的服务器公网IP>:28844` 并保存，即可享受无视内网限制的全球设备穿透连接！详细文档请参阅 [server/README.md](./server/README.md)。

---

## 🛠️ 网络与公网连接指引

OpenRemote 支持两种无公网 IPv4 限制的连接方式：

| 连接方案 | 适用网络环境 | 配置要求 | 延迟与性能 |
| :--- | :--- | :--- | :--- |
| **方案 A：IPv6 全球直连** | 双方网络均支持 IPv6（国内绝大部分宽带及 5G 手机热点） | 零配置，主界面会自动显示本机的 IPv6 地址，控制端直接输入该 IPv6 即可直连 | **极低延迟（P2P 直连，无服务器带宽瓶颈）** |
| **方案 B：中继穿透 (Relay)** | 纯 IPv4 内网、公司防火墙限制等无法直连的环境 | 在设置中填写中继服务器 IP，被控端生成 9 位设备码，控制端输入设备码连接 | **稳定可靠（通过自建中继转发）** |

---

## 📚 详细设计文档

- [系统详细架构与协议设计 (ARCHITECTURE.md)](./ARCHITECTURE.md)
- [信令、中继与 Tauri IPC 接口规范 (API.md)](./API.md)

---

## 📄 开源许可证

本项目基于 [MIT License](./LICENSE) 协议开源。
