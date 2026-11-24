# OpenRemote 中继与信令服务 (`v2.1.0-auth` 独立服务端)

本目录 (`server/`) 为 **OpenRemote 独立服务器端**，包含：
1. **HTTP 信令与设备查找服务 (`28844/TCP`)**
2. **账号注册/登录与 Token 鉴权体系 (`/api/auth/register`, `/api/auth/login`)**
3. **带登录鉴权与零丢包粘包处理的 TCP 中继隧道 (`28845/TCP`)**
4. **UDP STUN 端口反射服务 (`28844/UDP`)**

---

## 🔄 已有服务器一键更新方法 (推荐)

登录你的 Linux 云服务器 (`dxcool.cn`)，进入仓库目录执行：

```bash
cd ~/openremote
git pull origin main
docker rm -f openremote-relay 2>/dev/null || true
docker compose up -d --build
```

或者直接执行一键更新脚本：

```bash
cd ~/openremote/server
bash update.sh
```

---

## 🔍 验证服务器是否更新成功

在服务器上或浏览器访问：

```bash
curl http://127.0.0.1:28844/api/status
```

如果返回包含 `"version": "2.1.0-auth"` 与 `"relayAuthRequired": true`，即代表新服务器更新成功！

- **内置默认测试账号**：
  - 用户名：`admin`
  - 密码：`123456`
  - *(同时支持在客户端左下角【登录 / 注册账号】中自由注册新账号，账号数据持久化保存在 `server/data/users.json`)*
