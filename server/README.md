# OpenRemote 中继与信令服务器 (Docker 一键部署完整指南)

本目录包含 OpenRemote 专用的**轻量级双栈信令与 TCP 流量中继服务**。
单机仅需 **10MB 内存**，零外部数据库依赖，可部署在任何 1 核 512M 以上的轻量云服务器（腾讯云、阿里云、华为云、AWS、自建 VPS 等）上。

---

## 🚀 方式一：Linux 服务器单行脚本全自动部署（最推荐）

如果你有一台全新的 Linux 云服务器（支持 Ubuntu / Debian / CentOS / Rocky / AlmaLinux / Alpine）：
登录服务器后，**直接复制并运行以下这一行命令**：

```bash
curl -fsSL https://raw.githubusercontent.com/XCool-603/openremote/main/server/deploy.sh | sudo bash
```

> **该脚本会自动完成：**
> 1. 检测系统架构并获取公网 IPv4 / IPv6 地址；
> 2. 自动检测并安装最新版 Docker 环境；
> 3. 初始化 `/opt/openremote-server` 目录并配置 Node.js 20 容器；
> 4. 自动启动容器并开启开机自启（`restart: always`）；
> 5. 自动放行 Linux 本机防火墙端口（UFW / firewalld）；
> 6. 执行本地健康检查并打印客户端配置说明。

---

## 🐳 方式二：已有 Docker 服务器的单条命令部署

如果你的服务器上已经安装了 Docker，**无需克隆整个仓库**，直接在终端执行以下命令：

```bash
# 1. 创建工作目录
mkdir -p /opt/openremote-server && cd /opt/openremote-server

# 2. 下载单文件源码
curl -fsSL https://raw.githubusercontent.com/XCool-603/openremote/main/server/signaling.js -o signaling.js

# 3. 直接通过 Node.js 镜像单行启动（无需编译镜像）
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

## 📦 方式三：使用 Docker Compose 部署

适合习惯使用 `docker-compose` 管理容器的用户：

### 1. 准备目录与配置文件
在服务器上执行：
```bash
mkdir -p /opt/openremote-server && cd /opt/openremote-server
```

创建 `docker-compose.yml`：
```yaml
version: '3.8'

services:
  openremote-relay:
    image: node:20-alpine
    container_name: openremote-relay
    restart: always
    working_dir: /app
    volumes:
      - ./signaling.js:/app/signaling.js
    command: node signaling.js
    ports:
      - "28844:28844/tcp"   # HTTP 信令 API
      - "28844:28844/udp"   # STUN NAT 端口探测
      - "28845:28845/tcp"   # TCP 数据穿透中继隧道
    environment:
      - PORT=28844
      - UDP_PORT=28844
      - RELAY_PORT=28845
```

下载 `signaling.js` 并启动：
```bash
curl -fsSL https://raw.githubusercontent.com/XCool-603/openremote/main/server/signaling.js -o signaling.js
docker compose up -d
```

---

## 🛡️ 【关键步骤】云厂商防火墙 / 安全组放行

无论采用上述哪种部署方式，**必须在你的云服务器控制台（安全组规则）中放行以下 3 个端口**，否则控制端和被控端无法连通：

| 端口号 | 协议 | 用途说明 | 必选/可选 |
| :--- | :--- | :--- | :--- |
| **`28844`** | **TCP** | HTTP 信令交互、9 位设备码在线注册与心跳查询 | **必须开放** |
| **`28844`** | **UDP** | STUN 协议端口反射、公网 NAT 探测与打洞 | **必须开放** |
| **`28845`** | **TCP** | 对称 NAT 穿透时的 TCP 流量中继隧道 | **必须开放** |

### 主流云厂商安全组配置方法：
1. **腾讯云**：控制台 -> 轻量应用服务器 / 云服务器 CVM -> 安全组 / 防火墙 -> 添加规则 -> 协议填 `TCP` 端口填 `28844,28845`，再添加一条协议填 `UDP` 端口填 `28844`，策略均设为 `允许`。
2. **阿里云**：控制台 -> ECS 实例 -> 安全组 -> 入方向规则 -> 增加上述 TCP 与 UDP 规则。
3. **华为云**：安全组配置同上。

---

## 🔍 验证部署是否成功

### 1. 服务端自测
在服务器终端运行：
```bash
curl http://127.0.0.1:28844/api/status
```
若返回如下 JSON，说明服务已完美运行：
```json
{
  "status": "online",
  "server": "OpenRemote Dual-Stack Signaling & Relay Server",
  "onlinePeers": 0,
  "activeRelays": 0,
  "features": ["IPv6-Direct", "STUN-Reflective", "TCP-Relay-Tunnel"],
  "timestamp": "2026-10-29T03:15:00.000Z"
}
```

### 2. 远程电脑浏览器验证
在任意可以上网的电脑/手机浏览器中访问：
```
http://<你的服务器公网IP>:28844/api/status
```
如果能正常显示 JSON，证明**公网防火墙和安全组已彻底通畅**！

---

## 📱 客户端配置与使用

当中继服务器部署并验证通过后：

1. 打开 **OpenRemote 客户端**；
2. 点击左侧导航栏的 **【设置】(Settings)**；
3. 将 **信令服务器地址 (Signaling Server)** 输入框修改为：
   ```
   http://<你的服务器公网IP>:28844
   ```
4. 点击下方 **【保存设置】**；
5. 返回 **【主页】**，软件会自动向该中继服务器注册你的本机 9 位设备代码。其他任意内网电脑只要配置同一个中继服务器，输入该 9 位设备代码即可一键公网秒级穿透连接！

---

## 🛠️ 运维与排障指南

### 1. 查看容器实时日志
```bash
docker logs -f openremote-relay
```

### 2. 重启服务
```bash
docker restart openremote-relay
```

### 3. 停止并销毁服务
```bash
docker stop openremote-relay && docker rm openremote-relay
```

### 4. 常见问题排查：
- **浏览器打不开 `http://IP:28844/api/status`**：
  - 检查云平台控制台（安全组）是否放行了 TCP 28844；
  - 检查 Linux 系统内置防火墙（`sudo ufw status` 或 `sudo systemctl status firewalld`）；
  - 检查容器是否存活：`docker ps | grep openremote-relay`。
- **设备码连不上**：
  - 确保主控端与被控端填写的信令服务器 IP 相同且能连通；
  - 检查 TCP 28845 端口是否开放。
