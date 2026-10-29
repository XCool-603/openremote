# OpenRemote 信令与数据中继服务器 (Docker 部署指南)

本服务提供：
1. **HTTP 信令与设备寻址**（根据 9 位设备代码查询局域网与公网 IPv6/IPv4 候选）
2. **UDP STUN 端口反射**（NAT 映射探测）
3. **TCP Relay 数据中继隧道**（为严苛对称型 NAT / 纯内网环境提供 100% 连接保障）

---

## 🚀 方式一：Docker Compose 一键启动（推荐）

将 `server/` 目录上传至具有独立公网 IP 的云服务器（腾讯云、阿里云、华为云、AWS、VPS 等）：

```bash
cd server
docker compose up -d --build
```

查看运行状态与日志：
```bash
docker compose logs -f
```

---

## 🐳 方式二：纯 Docker 单行命令启动

如果无需 Compose，直接构建并启动：

```bash
# 1. 构建镜像 (体积仅 ~40MB)
docker build -t openremote-relay:latest .

# 2. 运行容器
docker run -d \
  --name openremote-relay \
  --restart unless-stopped \
  -p 28844:28844/tcp \
  -p 28844:28844/udp \
  -p 28845:28845/tcp \
  openremote-relay:latest
```

---

## 🔒 云服务器安全组 / 防火墙开放端口

请务必在云服务器控制台（安全组规则）中放行以下 3 个端口：

| 端口 | 协议 | 用途 |
| :--- | :---: | :--- |
| **`28844`** | **TCP** | 设备心跳注册、设备代码在线查询 API |
| **`28844`** | **UDP** | STUN 公网 NAT 端口反射探测 |
| **`28845`** | **TCP** | 跨内网数据中继管道 (Relay Tunnel) |

> **提示（UFW / iptables）：**
> ```bash
> sudo ufw allow 28844/tcp
> sudo ufw allow 28844/udp
> sudo ufw allow 28845/tcp
> sudo ufw reload
> ```

---

## 📱 客户端配置与连接

服务器启动后：
1. 在浏览器或终端中访问验证：
   ```bash
   curl http://<你的云服务器公网IP>:28844/api/status
   ```
   返回 `{"status":"online", ...}` 即表示部署成功！

2. 打开控制端与被控端的 **OpenRemote 客户端**：
   - 进入 **“系统设置”** -> **“信令服务器地址”**；
   - 填入你的服务器公网地址：
     `http://<你的云服务器公网IP>:28844`
   - 点击 **“保存配置”**。

两端即可通过 9 位设备代码在任意网络、任意跨网环境下随时互联！
