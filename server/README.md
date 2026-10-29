# OpenRemote 中继与信令服务 (Docker 部署指南)

本服务提供 OpenRemote 的**信令交换、STUN 端口探测与 TCP 流量穿透中继**，零外部数据库依赖，内存占用仅约 10MB。

---

## ⚡ 极简 3 步部署 (推荐)

在你的 Linux 云服务器上依次执行以下命令：

```bash
# 1. 克隆代码仓库
git clone https://github.com/XCool-603/openremote.git

# 2. 进入中继服务目录
cd openremote/server

# 3. 启动容器 (二选一)

# 方式 A: 纯 Docker 启动 (Build & Run)
docker build -t openremote-relay .
docker run -d \
  --name openremote-relay \
  --restart always \
  -p 28844:28844/tcp \
  -p 28844:28844/udp \
  -p 28845:28845/tcp \
  openremote-relay

# 方式 B: Docker Compose 一键启动
docker compose up -d
```

---

## 🛡️ 云服务器安全组放行 (必须)

请登录云厂商控制台（腾讯云、阿里云、华为云等），在服务器的**安全组/防火墙**添加入方向放行：

| 端口 | 协议 | 用途说明 |
| :--- | :--- | :--- |
| **`28844`** | **TCP** | HTTP 信令 API 与 9 位设备码在线心跳 |
| **`28844`** | **UDP** | STUN 公网端口反射与打洞 |
| **`28845`** | **TCP** | 对称 NAT 穿透时的 TCP 流量中继隧道 |

---

## 🔍 验证部署

浏览器访问：
```
http://<你的服务器公网IP>:28844/api/status
```
返回 `{"status": "online", ...}` 即表示服务部署成功。

---

## 📱 客户端配置

1. 打开 **OpenRemote 客户端**；
2. 进入左侧 **【设置】**（Settings）；
3. 将 **信令服务器地址** 改为：`http://<你的服务器公网IP>:28844` 并保存；
4. 返回主页，即可在任意网络环境下通过 9 位设备代码一键穿透连接！

---

## 🛠️ 常用运维命令

- **查看日志**：`docker logs -f openremote-relay`
- **重启服务**：`docker restart openremote-relay`
- **停止服务**：`docker stop openremote-relay`
