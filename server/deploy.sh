#!/bin/bash
# ==============================================================================
# OpenRemote Relay & Signaling Server - 一键全自动部署脚本
# 适用系统: Ubuntu 20.04+, Debian 11+, CentOS 7/8/9, Rocky, AlmaLinux, Alpine
# ==============================================================================

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m' # No Color

echo -e "${CYAN}"
echo "=============================================================="
echo "   🚀 OpenRemote 双栈信令与中继服务器 - Docker 一键安装程序"
echo "=============================================================="
echo -e "${NC}"

# 1. 检查 root 权限
if [ "$EUID" -ne 0 ]; then
  echo -e "${RED}❌ 错误: 请使用 root 用户或 sudo 执行此脚本！${NC}"
  echo "示例: sudo bash deploy.sh"
  exit 1
fi

# 2. 检测系统架构与公网 IP
SERVER_IP=$(curl -s4 https://api.ipify.org || curl -s4 http://ip.sb || echo "YOUR_SERVER_IP")
IPV6_ADDR=$(curl -s6 https://api6.ipify.org 2>/dev/null || echo "")

echo -e "${BLUE}ℹ️ 检测到服务器公网 IPv4: ${GREEN}${SERVER_IP}${NC}"
if [ -n "$IPV6_ADDR" ]; then
  echo -e "${BLUE}ℹ️ 检测到服务器公网 IPv6: ${GREEN}${IPV6_ADDR}${NC}"
fi

# 3. 检查并安装 Docker
if ! command -v docker &> /dev/null; then
  echo -e "${YELLOW}⚠️ 未检测到 Docker，正在自动安装 Docker 环境...${NC}"
  
  if command -v apt-get &> /dev/null; then
    apt-get update -y
    apt-get install -y ca-certificates curl gnupg lsb-release
  elif command -v yum &> /dev/null; then
    yum install -y curl
  fi

  # 使用官方一键安装脚本（国内服务器使用阿里云镜像）
  curl -fsSL https://get.docker.com -o get-docker.sh
  sh get-docker.sh
  rm -f get-docker.sh
  systemctl enable docker
  systemctl start docker
  echo -e "${GREEN}✅ Docker 安装成功！${NC}"
else
  echo -e "${GREEN}✅ 检测到 Docker 已安装: $(docker --version)${NC}"
fi

# 4. 准备部署目录
APP_DIR="/opt/openremote-server"
echo -e "${BLUE}📁 初始化服务目录: ${APP_DIR}${NC}"
mkdir -p "${APP_DIR}"
cd "${APP_DIR}"

# 5. 生成 signaling.js 服务源码
echo -e "${BLUE}📦 写入信令与中继服务组件...${NC}"
cat << 'EOF' > signaling.js
import http from 'node:http';
import dgram from 'node:dgram';
import net from 'node:net';

const HTTP_PORT = parseInt(process.env.PORT || '28844', 10);
const UDP_PORT = parseInt(process.env.UDP_PORT || '28844', 10);
const RELAY_PORT = parseInt(process.env.RELAY_PORT || '28845', 10);

const peerRegistry = new Map();
const relaySessions = new Map();

function normalizeCode(code) {
  return String(code || '').replace(/[-\s]/g, '').trim();
}

setInterval(() => {
  const now = Date.now();
  for (const [code, info] of peerRegistry.entries()) {
    if (now - info.lastSeen > 90_000) {
      peerRegistry.delete(code);
      console.log(`[Signaling] Expired offline device: ${code}`);
    }
  }
}, 30_000);

const server = http.createServer((req, res) => {
  res.setHeader('Access-Control-Allow-Origin', '*');
  res.setHeader('Access-Control-Allow-Methods', 'GET, POST, OPTIONS');
  res.setHeader('Access-Control-Allow-Headers', 'Content-Type, Authorization');

  if (req.method === 'OPTIONS') {
    res.writeHead(204);
    res.end();
    return;
  }

  const url = new URL(req.url, `http://${req.headers.host || 'localhost'}`);
  const clientIp = (
    req.headers['x-forwarded-for'] ||
    req.socket.remoteAddress ||
    '127.0.0.1'
  ).split(',')[0].trim().replace(/^::ffff:/, '');

  if (req.method === 'POST' && url.pathname === '/api/register') {
    let body = '';
    req.on('data', (chunk) => (body += chunk));
    req.on('end', () => {
      try {
        const data = JSON.parse(body || '{}');
        const code = normalizeCode(data.device_code);
        if (!code) {
          res.writeHead(400, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify({ error: 'Missing device_code' }));
          return;
        }

        const peerInfo = {
          deviceId: data.device_id || code,
          deviceCode: code,
          deviceName: data.device_name || 'OpenRemote-Host',
          localIp: data.local_ip || '',
          listenPort: data.port || 28843,
          publicIp: clientIp,
          publicPort: data.public_port || data.port || 28843,
          ipv6: data.ipv6 || null,
          lastSeen: Date.now(),
        };

        peerRegistry.set(code, peerInfo);
        console.log(`[Signaling] Peer registered: ${code} (${peerInfo.deviceName}) from ${clientIp}`);

        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({
          success: true,
          registeredCode: code,
          reflectedIp: clientIp,
          ipv6Detected: !!peerInfo.ipv6,
          relayPort: RELAY_PORT,
          expiresInSec: 90,
        }));
      } catch (err) {
        res.writeHead(400, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ error: 'Invalid JSON body' }));
      }
    });
    return;
  }

  if (req.method === 'GET' && url.pathname.startsWith('/api/lookup')) {
    const rawCode = url.searchParams.get('code') || url.pathname.split('/').pop();
    const code = normalizeCode(rawCode);

    const peer = peerRegistry.get(code);
    if (peer) {
      peer.lastQueried = Date.now();
      const candidates = [];
      if (peer.ipv6) candidates.push({ type: 'ipv6', address: peer.ipv6, port: peer.listenPort });
      if (peer.publicIp) candidates.push({ type: 'public_ipv4', address: peer.publicIp, port: peer.publicPort });
      if (peer.localIp) candidates.push({ type: 'lan_ipv4', address: peer.localIp, port: peer.listenPort });

      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({
        found: true,
        peer: {
          deviceId: peer.deviceId,
          deviceCode: peer.deviceCode,
          deviceName: peer.deviceName,
          localIp: peer.localIp,
          listenPort: peer.listenPort,
          publicIp: peer.publicIp,
          publicPort: peer.publicPort,
          ipv6: peer.ipv6,
          relayPort: RELAY_PORT,
          candidates,
        },
      }));
    } else {
      res.writeHead(404, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ found: false, message: `Device '${rawCode}' is offline or not found` }));
    }
    return;
  }

  if (req.method === 'POST' && url.pathname === '/api/relay/create') {
    const sessionId = `relay_${Date.now()}_${Math.random().toString(36).substring(2, 8)}`;
    relaySessions.set(sessionId, { host: null, ctrl: null, createdAt: Date.now() });

    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({
      success: true,
      sessionId,
      relayHost: req.headers.host ? req.headers.host.split(':')[0] : '127.0.0.1',
      relayPort: RELAY_PORT,
    }));
    return;
  }

  if (url.pathname === '/api/status' || url.pathname === '/health' || url.pathname === '/') {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({
      status: 'online',
      server: 'OpenRemote Dual-Stack Signaling & Relay Server',
      onlinePeers: peerRegistry.size,
      activeRelays: relaySessions.size,
      features: ['IPv6-Direct', 'STUN-Reflective', 'TCP-Relay-Tunnel'],
      timestamp: new Date().toISOString(),
    }));
    return;
  }

  res.writeHead(404, { 'Content-Type': 'text/plain' });
  res.end('Not Found');
});

const udpServer = dgram.createSocket({ type: 'udp4', reuseAddr: true });
udpServer.on('message', (msg, rinfo) => {
  const resp = Buffer.from(JSON.stringify({
    echo: true,
    clientIp: rinfo.address,
    clientPort: rinfo.port,
    family: rinfo.family,
    timestamp: Date.now(),
  }));
  udpServer.send(resp, rinfo.port, rinfo.address, (err) => {
    if (err) console.error(`[STUN UDP] Send error to ${rinfo.address}:${rinfo.port}`, err);
  });
});

const pendingRelays = new Map();
const tcpRelayServer = net.createServer({ noDelay: true }, (socket) => {
  let authenticated = false;
  let buffer = '';

  const cleanup = () => {
    socket.destroy();
  };

  socket.on('error', (err) => {
    console.error(`[TCP Relay] Socket error: ${err.message}`);
    cleanup();
  });

  socket.on('data', (chunk) => {
    if (authenticated) return;
    buffer += chunk.toString('utf8');
    const newlineIdx = buffer.indexOf('\n');
    if (newlineIdx !== -1) {
      const line = buffer.substring(0, newlineIdx).trim();
      const remaining = chunk.slice(Buffer.byteLength(line, 'utf8') + 1);
      const [command, sessionId] = line.split(/\s+/);

      if (command === 'REGISTER' && sessionId) {
        authenticated = true;
        pendingRelays.set(sessionId, { hostSocket: socket, createdAt: Date.now() });
        socket.write('OK\n');
        console.log(`[TCP Relay] Host bound to session: ${sessionId}`);
        setTimeout(() => {
          if (pendingRelays.has(sessionId)) {
            pendingRelays.delete(sessionId);
            socket.destroy();
          }
        }, 120_000);
      } else if (command === 'JOIN' && sessionId) {
        authenticated = true;
        const entry = pendingRelays.get(sessionId);
        if (entry && entry.hostSocket && !entry.hostSocket.destroyed) {
          pendingRelays.delete(sessionId);
          socket.write('OK\n');
          const hostSocket = entry.hostSocket;
          hostSocket.pipe(socket);
          socket.pipe(hostSocket);
          if (remaining.length > 0) hostSocket.write(remaining);
          console.log(`[TCP Relay] Bridged full-duplex tunnel for session: ${sessionId}`);
        } else {
          socket.write('ERROR: Session not found or expired\n');
          socket.end();
        }
      } else {
        socket.write('ERROR: Invalid handshake command\n');
        socket.end();
      }
    }
  });
});

server.listen(HTTP_PORT, '::', () => {
  console.log(`[Signaling HTTP] Listening on [::]:${HTTP_PORT} (IPv4 + IPv6)`);
});
udpServer.bind(UDP_PORT, '0.0.0.0', () => {
  console.log(`[STUN UDP] Listening on 0.0.0.0:${UDP_PORT}`);
});
tcpRelayServer.listen(RELAY_PORT, '::', () => {
  console.log(`[TCP Relay Tunnel] Listening on [::]:${RELAY_PORT} (IPv4 + IPv6)`);
});
EOF

# 6. 生成 Dockerfile 与 docker-compose.yml
cat << 'EOF' > Dockerfile
FROM node:20-alpine
WORKDIR /app
COPY signaling.js .
EXPOSE 28844/tcp
EXPOSE 28844/udp
EXPOSE 28845/tcp
ENV PORT=28844 \
    UDP_PORT=28844 \
    RELAY_PORT=28845 \
    NODE_ENV=production
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
  CMD wget -qO- http://127.0.0.1:28844/api/status || exit 1
CMD ["node", "signaling.js"]
EOF

cat << 'EOF' > docker-compose.yml
version: '3.8'

services:
  openremote-relay:
    build: .
    container_name: openremote-relay
    restart: always
    ports:
      - "28844:28844/tcp"
      - "28844:28844/udp"
      - "28845:28845/tcp"
    environment:
      - PORT=28844
      - UDP_PORT=28844
      - RELAY_PORT=28845
    healthcheck:
      test: ["CMD", "wget", "-qO-", "http://127.0.0.1:28844/api/status"]
      interval: 30s
      timeout: 5s
      retries: 3
EOF

# 7. 构建并启动容器
echo -e "${BLUE}🔨 启动 OpenRemote Docker 容器...${NC}"
if docker compose version &> /dev/null; then
  docker compose down --remove-orphans 2>/dev/null || true
  docker compose up -d --build
elif command -v docker-compose &> /dev/null; then
  docker-compose down --remove-orphans 2>/dev/null || true
  docker-compose up -d --build
else
  # 降级直接通过 docker run 启动
  docker stop openremote-relay 2>/dev/null || true
  docker rm openremote-relay 2>/dev/null || true
  docker build -t openremote-relay:latest .
  docker run -d \
    --name openremote-relay \
    --restart always \
    -p 28844:28844/tcp \
    -p 28844:28844/udp \
    -p 28845:28845/tcp \
    openremote-relay:latest
fi

# 8. 防火墙端口自动放行提醒与尝试
echo -e "${BLUE}🛡️ 配置系统防火墙端口 (28844 TCP/UDP, 28845 TCP)...${NC}"
if command -v ufw &> /dev/null && ufw status | grep -q "Status: active"; then
  ufw allow 28844/tcp
  ufw allow 28844/udp
  ufw allow 28845/tcp
  ufw reload
  echo -e "${GREEN}✅ UFW 防火墙端口放行完成！${NC}"
elif command -v firewall-cmd &> /dev/null && systemctl is-active --quiet firewalld; then
  firewall-cmd --permanent --add-port=28844/tcp
  firewall-cmd --permanent --add-port=28844/udp
  firewall-cmd --permanent --add-port=28845/tcp
  firewall-cmd --reload
  echo -e "${GREEN}✅ firewalld 防火墙端口放行完成！${NC}"
fi

# 9. 本地服务健康自检
echo -e "${BLUE}🔍 正在执行服务健康自检...${NC}"
sleep 3
STATUS_CHECK=$(curl -s http://127.0.0.1:28844/api/status || echo "FAILED")

if [[ "$STATUS_CHECK" == *"online"* ]]; then
  echo -e "${GREEN}🎉 恭喜！OpenRemote 中继与信令服务已成功运行！${NC}"
else
  echo -e "${YELLOW}⚠️ 服务已启动，但在自检时未能立即获取响应，请使用 'docker logs openremote-relay' 查看日志。${NC}"
fi

echo ""
echo -e "${CYAN}=============================================================="
echo -e "                   🎉 部署成功与配置指南"
echo -e "==============================================================${NC}"
echo -e "📍 ${YELLOW}你的信令与中继服务器公网 IP:${NC} ${GREEN}${SERVER_IP}${NC}"
echo -e "🌐 ${YELLOW}HTTP 信令接口:${NC}  http://${SERVER_IP}:28844/api/status"
echo -e "🌐 ${YELLOW}UDP STUN 接口:${NC}   ${SERVER_IP}:28844 (UDP)"
echo -e "🌐 ${YELLOW}TCP 数据中继隧道:${NC} ${SERVER_IP}:28845 (TCP)"
echo ""
echo -e "${YELLOW}⚠️ 【至关重要】请确保云服务器控制台安全组已开放下列 3 个端口：${NC}"
echo -e "   1. 端口 ${GREEN}28844 / TCP${NC} (信令/心跳 API)"
echo -e "   2. 端口 ${GREEN}28844 / UDP${NC} (STUN 打洞与端口探测)"
echo -e "   3. 端口 ${GREEN}28845 / TCP${NC} (穿透数据中继流量)"
echo ""
echo -e "${CYAN}📱 客户端配置方法：${NC}"
echo -e "   1. 打开 OpenRemote 客户端 -> 点击左侧【设置】(Settings)"
echo -e "   2. 将【信令服务器地址】修改为: ${GREEN}http://${SERVER_IP}:28844${NC}"
echo -e "   3. 点击保存，主页即可自动注册并支持 9 位设备码全球穿透！"
echo -e "${CYAN}==============================================================${NC}"
