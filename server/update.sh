#!/usr/bin/env bash
set -e

echo "========================================================"
echo "  🚀 OpenRemote 服务器端 (Signaling + Auth + Relay) 更新"
echo "========================================================"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

if [ -d "../.git" ]; then
  echo "[1/3] 正在从 Git 仓库拉取最新服务端代码..."
  git pull origin main
fi

echo "[2/3] 正在重新构建并启动 openremote-relay 容器..."
docker rm -f openremote-relay 2>/dev/null || true

if docker compose version &>/dev/null; then
  docker compose up -d --build
elif command -v docker-compose &>/dev/null; then
  docker-compose up -d --build
else
  docker build -t openremote-relay:latest .
  docker run -d \
    --name openremote-relay \
    --restart always \
    -p 28844:28844/tcp \
    -p 28844:28844/udp \
    -p 28845:28845/tcp \
    -v "$SCRIPT_DIR/data:/app/data" \
    openremote-relay:latest
fi

echo "[3/3] 正在验证服务端状态..."
sleep 2
curl -s http://127.0.0.1:28844/api/status || true
echo ""
echo "========================================================"
echo "✅ 服务器端更新完成！默认测试账号: admin  密码: 123456"
echo "========================================================"
