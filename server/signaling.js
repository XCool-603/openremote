import http from 'node:http';
import dgram from 'node:dgram';
import net from 'node:net';
import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';

const SERVER_VERSION = '2.1.0-auth';
const HTTP_PORT = parseInt(process.env.PORT || '28844', 10);
const UDP_PORT = parseInt(process.env.UDP_PORT || '28844', 10);
const RELAY_PORT = parseInt(process.env.RELAY_PORT || '28845', 10);
const REQUIRE_RELAY_AUTH = process.env.REQUIRE_RELAY_AUTH !== 'false'; // Default: true
const AUTH_SECRET = process.env.AUTH_SECRET || 'openremote-enterprise-secret-key-2026';
const DATA_DIR = process.env.DATA_DIR || path.join(process.cwd(), 'data');
const USERS_FILE = path.join(DATA_DIR, 'users.json');

// Ensure data directory exists
try {
  if (!fs.existsSync(DATA_DIR)) {
    fs.mkdirSync(DATA_DIR, { recursive: true });
  }
} catch (err) {
  console.warn(`[Auth] Could not create data directory ${DATA_DIR}:`, err.message);
}

// --- User Account Store ---
// Map: username (lowercase) -> { username, passwordHash, salt, createdAt }
const users = new Map();

function hashPassword(password, salt) {
  return crypto.pbkdf2Sync(String(password), salt, 10000, 32, 'sha256').toString('hex');
}

function saveUsers() {
  try {
    const list = Array.from(users.values());
    fs.writeFileSync(USERS_FILE, JSON.stringify(list, null, 2), 'utf8');
  } catch (err) {
    console.warn('[Auth] Failed to persist users.json:', err.message);
  }
}

function loadUsers() {
  try {
    if (fs.existsSync(USERS_FILE)) {
      const raw = fs.readFileSync(USERS_FILE, 'utf8');
      const list = JSON.parse(raw);
      if (Array.isArray(list)) {
        for (const u of list) {
          if (u && u.username) {
            users.set(u.username.toLowerCase(), u);
          }
        }
      }
    }
  } catch (err) {
    console.warn('[Auth] Failed to load users.json:', err.message);
  }

  // Seed default account (admin / 123456) so user can test immediately out-of-the-box
  if (!users.has('admin')) {
    const salt = 'openremote_default_salt';
    users.set('admin', {
      username: 'admin',
      salt,
      passwordHash: hashPassword('123456', salt),
      createdAt: new Date().toISOString(),
    });
    saveUsers();
    console.log('[Auth] Initialized default account: admin / 123456');
  }
}

loadUsers();

// --- Stateless HMAC Token Generator & Verifier ---
function createToken(username) {
  const payload = Buffer.from(
    JSON.stringify({
      sub: username,
      iat: Date.now(),
      exp: Date.now() + 30 * 24 * 3600 * 1000, // 30 days
    })
  ).toString('base64url');
  const sig = crypto.createHmac('sha256', AUTH_SECRET).update(payload).digest('base64url');
  return `${payload}.${sig}`;
}

function verifyToken(token) {
  if (!token || typeof token !== 'string') return null;
  const clean = token.replace(/^Bearer\s+/i, '').trim();
  const parts = clean.split('.');
  if (parts.length !== 2) return null;
  const [payloadB64, sig] = parts;
  const expectedSig = crypto
    .createHmac('sha256', AUTH_SECRET)
    .update(payloadB64)
    .digest('base64url');
  if (sig !== expectedSig) return null;

  try {
    const data = JSON.parse(Buffer.from(payloadB64, 'base64url').toString('utf8'));
    if (data.exp && Date.now() > data.exp) return null;
    return data.sub || null;
  } catch {
    return null;
  }
}

// --- Device Registry & Relay Pools ---
// Key: normalized 9-digit device_code (e.g. "849201948")
const peerRegistry = new Map();

// Waiting relay sessions (not yet bridged)
// Key: sessionId -> { host: Socket|null, ctrl: Socket|null, hostRemainder: Buffer, ctrlRemainder: Buffer, hostUser: string|null, createdAt: number }
const relaySessions = new Map();

// Active bridged sessions
// Key: bridgeId -> { sessionId, host, ctrl, user, startedAt }
const activeBridges = new Map();

// Pending UDP hole-punching requests queued for target hosts
// Key: normalized target device_code -> Array<{ ctrlIp, ctrlPort, createdAt }>
const pendingPunches = new Map();

function normalizeCode(code) {
  return String(code || '').replace(/[-\s]/g, '').trim();
}

function sendJson(res, statusCode, data) {
  const payload = JSON.stringify(data);
  res.writeHead(statusCode, {
    'Content-Type': 'application/json; charset=utf-8',
    'Content-Length': Buffer.byteLength(payload),
  });
  res.end(payload);
}

function readJsonBody(req) {
  return new Promise((resolve, reject) => {
    let body = '';
    req.on('data', (chunk) => {
      body += chunk;
      if (body.length > 1024 * 64) {
        reject(new Error('Payload too large'));
      }
    });
    req.on('end', () => {
      try {
        resolve(JSON.parse(body || '{}'));
      } catch (e) {
        reject(e);
      }
    });
    req.on('error', reject);
  });
}

// Prune stale device registrations every 30 seconds (TTL: 90s)
setInterval(() => {
  const now = Date.now();
  for (const [code, info] of peerRegistry.entries()) {
    if (now - info.lastSeen > 90_000) {
      peerRegistry.delete(code);
      console.log(`[Signaling] Expired offline device: ${code}`);
    }
  }
}, 30_000);

// --- HTTP Signaling & Account API ---
const server = http.createServer(async (req, res) => {
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
  )
    .split(',')[0]
    .trim()
    .replace(/^::ffff:/, '');

  // 1. Account Registration: POST /api/auth/register
  if (req.method === 'POST' && url.pathname === '/api/auth/register') {
    try {
      const data = await readJsonBody(req);
      const username = String(data.username || '').trim();
      const password = String(data.password || '');

      if (username.length < 2 || username.length > 32) {
        return sendJson(res, 400, { success: false, error: '用户名长度需在 2~32 个字符之间' });
      }
      if (password.length < 4) {
        return sendJson(res, 400, { success: false, error: '密码长度至少为 4 位' });
      }

      const key = username.toLowerCase();
      if (users.has(key)) {
        return sendJson(res, 409, { success: false, error: '该用户名已被注册，请直接登录' });
      }

      const salt = crypto.randomBytes(16).toString('hex');
      const passwordHash = hashPassword(password, salt);
      const userObj = {
        username,
        salt,
        passwordHash,
        createdAt: new Date().toISOString(),
      };
      users.set(key, userObj);
      saveUsers();

      const token = createToken(username);
      console.log(`[Auth] Registered new user: ${username} from ${clientIp}`);
      return sendJson(res, 200, {
        success: true,
        username,
        token,
      });
    } catch {
      return sendJson(res, 400, { success: false, error: '无效的请求格式' });
    }
  }

  // 2. Account Login: POST /api/auth/login
  if (req.method === 'POST' && url.pathname === '/api/auth/login') {
    try {
      const data = await readJsonBody(req);
      const username = String(data.username || '').trim();
      const password = String(data.password || '');
      const key = username.toLowerCase();

      const userObj = users.get(key);
      if (!userObj) {
        return sendJson(res, 401, { success: false, error: '账号不存在，请先注册' });
      }

      const hash = hashPassword(password, userObj.salt);
      if (hash !== userObj.passwordHash) {
        return sendJson(res, 401, { success: false, error: '密码错误，请重试' });
      }

      const token = createToken(userObj.username);
      console.log(`[Auth] User logged in: ${userObj.username} from ${clientIp}`);
      return sendJson(res, 200, {
        success: true,
        username: userObj.username,
        token,
      });
    } catch {
      return sendJson(res, 400, { success: false, error: '无效的请求格式' });
    }
  }

  // 3. Verify Token: GET /api/auth/me
  if (req.method === 'GET' && url.pathname === '/api/auth/me') {
    const authHeader = req.headers['authorization'] || url.searchParams.get('token') || '';
    const username = verifyToken(authHeader);
    if (!username) {
      return sendJson(res, 401, { authenticated: false, error: '登录已过期或未登录' });
    }
    return sendJson(res, 200, { authenticated: true, username });
  }

  // 4. Device Registration / Heartbeat: POST /api/register
  if (req.method === 'POST' && url.pathname === '/api/register') {
    try {
      const data = await readJsonBody(req);
      const code = normalizeCode(data.device_code);
      if (!code) {
        return sendJson(res, 400, { error: 'Missing device_code' });
      }

      const tokenUser = verifyToken(data.auth_token || req.headers['authorization'] || '');
      const effectivePublicIp = data.stun_ip || clientIp;
      const effectivePublicPort = data.stun_port || data.public_port || data.listen_port || 28843;

      const peerInfo = {
        deviceId: data.device_id || 'unknown',
        deviceCode: data.device_code,
        normalizedCode: code,
        deviceName: data.device_name || 'OpenRemote Peer',
        localIp: data.local_ip || clientIp,
        listenPort: data.listen_port || 28843,
        publicIp: effectivePublicIp,
        publicPort: effectivePublicPort,
        ipv6: data.ipv6 || null,
        owner: tokenUser || null,
        lastSeen: Date.now(),
      };

      peerRegistry.set(code, peerInfo);
      console.log(
        `[Signaling] Registered device ${data.device_code} (${peerInfo.deviceName}) Public: ${effectivePublicIp}:${effectivePublicPort}, IPv6: ${peerInfo.ipv6 || 'none'}, User: ${tokenUser || 'guest'}`
      );

      // Drain any pending UDP hole-punching requests for this host
      const punches = pendingPunches.get(code) || [];
      if (punches.length > 0) {
        pendingPunches.delete(code);
      }

      return sendJson(res, 200, {
        success: true,
        publicIp: effectivePublicIp,
        registered: peerInfo,
        punches,
      });
    } catch {
      return sendJson(res, 400, { error: 'Invalid JSON payload' });
    }
  }

  // 5. Lookup Peer by Device Code: GET /api/lookup/:code
  if (req.method === 'GET' && url.pathname.startsWith('/api/lookup')) {
    const rawCode =
      url.searchParams.get('code') || url.pathname.replace(/^\/api\/lookup\/?/, '');
    const code = normalizeCode(rawCode);
    const peer = peerRegistry.get(code);

    if (peer) {
      const candidates = [];
      if (peer.ipv6) {
        candidates.push(`[${peer.ipv6}]:${peer.listenPort}`);
      }
      if (peer.localIp) {
        candidates.push(`${peer.localIp}:${peer.listenPort}`);
      }
      if (peer.publicIp) {
        candidates.push(`${peer.publicIp}:${peer.publicPort}`);
      }

      const waitingSession = relaySessions.get(code);
      const hostReadyOnRelay = Boolean(
        waitingSession && waitingSession.host && !waitingSession.host.destroyed
      );

      return sendJson(res, 200, {
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
          hostReadyOnRelay,
          candidates,
        },
      });
    } else {
      return sendJson(res, 404, {
        found: false,
        message: `Device '${rawCode}' is offline or not found`,
      });
    }
  }

  // 5.5 Coordinated UDP Hole Punching: POST /api/punch & GET /api/punch/poll/:code
  if (req.method === 'POST' && url.pathname === '/api/punch') {
    try {
      const data = await readJsonBody(req);
      const targetCode = normalizeCode(data.target_code);
      const ctrlIp = String(data.ctrl_ip || clientIp).trim();
      const ctrlPort = parseInt(data.ctrl_port || '0', 10);

      if (!targetCode || !ctrlIp || !ctrlPort) {
        return sendJson(res, 400, { success: false, error: 'Missing target_code, ctrl_ip, or ctrl_port' });
      }

      const list = pendingPunches.get(targetCode) || [];
      list.push({ ctrlIp, ctrlPort, createdAt: Date.now() });
      pendingPunches.set(targetCode, list.slice(-5));
      console.log(`[Punch] Queued UDP hole punch -> Host ${targetCode} to reach Ctrl ${ctrlIp}:${ctrlPort}`);
      return sendJson(res, 200, { success: true });
    } catch {
      return sendJson(res, 400, { success: false, error: 'Invalid JSON payload' });
    }
  }

  if (req.method === 'GET' && url.pathname.startsWith('/api/punch/poll/')) {
    const rawCode = url.pathname.replace(/^\/api\/punch\/poll\/?/, '');
    const code = normalizeCode(rawCode);
    const list = pendingPunches.get(code) || [];
    const fresh = list.filter((p) => Date.now() - p.createdAt < 15_000);
    if (list.length > 0) {
      pendingPunches.delete(code);
    }
    return sendJson(res, 200, { punches: fresh });
  }

  // 6. Status & Healthcheck
  if (url.pathname === '/api/status' || url.pathname === '/health' || url.pathname === '/') {
    const onlineDevices = Array.from(peerRegistry.values()).map((p) => ({
      deviceCode: p.deviceCode,
      deviceName: p.deviceName,
      owner: p.owner,
      hasIpv6: Boolean(p.ipv6),
      relayStandby: Boolean(
        relaySessions.get(p.normalizedCode)?.host &&
          !relaySessions.get(p.normalizedCode).host.destroyed
      ),
      lastSeenAgoSec: Math.round((Date.now() - p.lastSeen) / 1000),
    }));

    return sendJson(res, 200, {
      status: 'online',
      version: SERVER_VERSION,
      server: 'OpenRemote Dual-Stack Signaling & Authenticated Relay Server',
      relayAuthRequired: REQUIRE_RELAY_AUTH,
      registeredUsers: users.size,
      onlinePeers: peerRegistry.size,
      standbyRelayHosts: relaySessions.size,
      activeRelayBridges: activeBridges.size,
      devices: onlineDevices,
      features: [
        'Account-Auth-HMAC',
        'Authenticated-TCP-Relay',
        'Zero-Loss-Packet-Bridge',
        'IPv6-Direct',
        'STUN-Reflective',
      ],
      timestamp: new Date().toISOString(),
    });
  }

  return sendJson(res, 404, { error: 'Endpoint not found' });
});

// --- TCP Relay Tunneling Server (Port 28845) ---
function tryBridgeSession(sessionId) {
  const session = relaySessions.get(sessionId);
  if (!session || !session.host || !session.ctrl) return;
  if (session.host.destroyed || session.ctrl.destroyed) return;

  // Move out of waiting pool immediately so new standby connections don't collide
  relaySessions.delete(sessionId);

  const hostSocket = session.host;
  const ctrlSocket = session.ctrl;
  const bridgeId = `${sessionId}_${Date.now()}`;

  activeBridges.set(bridgeId, {
    sessionId,
    host: hostSocket,
    ctrl: ctrlSocket,
    user: session.ctrlUser || session.hostUser || 'guest',
    startedAt: Date.now(),
  });

  console.log(
    `[Relay] Bridging full-duplex data pipeline for session ${sessionId} (user: ${session.ctrlUser || 'guest'}, ctrlRemainder: ${session.ctrlRemainder.length}B, hostRemainder: ${session.hostRemainder.length}B)`
  );

  // Flush any coalesced binary payload bytes that arrived in the same TCP packet as the handshake line
  if (session.ctrlRemainder && session.ctrlRemainder.length > 0) {
    hostSocket.write(session.ctrlRemainder);
    session.ctrlRemainder = Buffer.alloc(0);
  }
  if (session.hostRemainder && session.hostRemainder.length > 0) {
    ctrlSocket.write(session.hostRemainder);
    session.hostRemainder = Buffer.alloc(0);
  }

  // Pipe bidirectional raw binary streams without injecting any extra ASCII characters
  hostSocket.pipe(ctrlSocket);
  ctrlSocket.pipe(hostSocket);

  hostSocket.resume();
  ctrlSocket.resume();

  let cleaned = false;
  const cleanup = () => {
    if (cleaned) return;
    cleaned = true;
    activeBridges.delete(bridgeId);
    if (!hostSocket.destroyed) hostSocket.destroy();
    if (!ctrlSocket.destroyed) ctrlSocket.destroy();
    console.log(`[Relay] Closed bridged session ${sessionId} (${bridgeId})`);
  };

  hostSocket.on('close', cleanup);
  ctrlSocket.on('close', cleanup);
  hostSocket.on('error', cleanup);
  ctrlSocket.on('error', cleanup);
}

const relayServer = net.createServer({ noDelay: true }, (socket) => {
  socket.setNoDelay(true);
  socket.setKeepAlive(true, 15_000);

  let rawHeaderBuf = Buffer.alloc(0);
  let identified = false;

  // Timeout if client connects but never sends handshake line within 10s
  const handshakeTimer = setTimeout(() => {
    if (!identified && !socket.destroyed) {
      socket.destroy();
    }
  }, 10_000);

  const onData = (chunk) => {
    if (identified) return;
    rawHeaderBuf = Buffer.concat([rawHeaderBuf, chunk]);

    const newlineIndex = rawHeaderBuf.indexOf(0x0a); // '\n'
    if (newlineIndex === -1) {
      if (rawHeaderBuf.length > 1024) {
        // Handshake line too long / invalid protocol
        clearTimeout(handshakeTimer);
        socket.destroy();
      }
      return;
    }

    identified = true;
    clearTimeout(handshakeTimer);
    socket.removeListener('data', onData);

    // Pause socket reading until both peers are paired so no subsequent packets are lost
    socket.pause();

    const line = rawHeaderBuf.subarray(0, newlineIndex).toString('utf8').trim();
    const remainder = Buffer.from(rawHeaderBuf.subarray(newlineIndex + 1));
    rawHeaderBuf = Buffer.alloc(0);

    // Protocol: "OPRM_RELAY <sessionId> <HOST|CTRL> [authToken]"
    const parts = line.split(/\s+/);
    let sessionId = '';
    let role = '';
    let token = '';

    if (parts[0] === 'OPRM_RELAY' && parts.length >= 3) {
      sessionId = normalizeCode(parts[1]);
      role = parts[2].toUpperCase();
      token = parts[3] || '';
    } else if (parts[0] === 'REGISTER' && parts.length >= 2) {
      sessionId = normalizeCode(parts[1]);
      role = 'HOST';
      token = parts[2] || '';
    } else if (parts[0] === 'JOIN' && parts.length >= 2) {
      sessionId = normalizeCode(parts[1]);
      role = 'CTRL';
      token = parts[2] || '';
    } else {
      console.warn(`[Relay] Invalid handshake line: "${line}"`);
      socket.destroy();
      return;
    }

    if (!sessionId || (role !== 'HOST' && role !== 'CTRL')) {
      socket.destroy();
      return;
    }

    const authUser = verifyToken(token);

    // Enforce account authentication for Controller relay requests
    if (role === 'CTRL' && REQUIRE_RELAY_AUTH && !authUser) {
      console.warn(
        `[Relay] Rejected unauthenticated CTRL connection for session ${sessionId} (missing or invalid token)`
      );
      socket.write('ERROR_AUTH_REQUIRED 请先登录账号后再使用中继服务器\n');
      socket.destroy();
      return;
    }

    let session = relaySessions.get(sessionId);
    if (!session) {
      session = {
        host: null,
        ctrl: null,
        hostRemainder: Buffer.alloc(0),
        ctrlRemainder: Buffer.alloc(0),
        hostUser: null,
        ctrlUser: null,
        createdAt: Date.now(),
      };
      relaySessions.set(sessionId, session);
    }

    if (role === 'HOST') {
      // If an older unbridged host socket exists, replace it cleanly
      if (session.host && session.host !== socket && !session.host.destroyed) {
        session.host._replaced = true;
        session.host.destroy();
      }
      session.host = socket;
      session.hostRemainder = remainder;
      session.hostUser = authUser;
      console.log(
        `[Relay] Host standby ready for session ${sessionId} (user: ${authUser || 'guest'})`
      );

      socket.on('close', () => {
        if (socket._replaced) return;
        const cur = relaySessions.get(sessionId);
        if (cur && cur.host === socket) {
          cur.host = null;
          cur.hostRemainder = Buffer.alloc(0);
          if (!cur.ctrl) relaySessions.delete(sessionId);
        }
      });
      socket.on('error', () => {});
    } else {
      if (session.ctrl && session.ctrl !== socket && !session.ctrl.destroyed) {
        session.ctrl._replaced = true;
        session.ctrl.destroy();
      }
      session.ctrl = socket;
      session.ctrlRemainder = remainder;
      session.ctrlUser = authUser;
      console.log(
        `[Relay] Controller joined session ${sessionId} (user: ${authUser || 'guest'}, initial payload: ${remainder.length}B)`
      );

      socket.on('close', () => {
        if (socket._replaced) return;
        const cur = relaySessions.get(sessionId);
        if (cur && cur.ctrl === socket) {
          cur.ctrl = null;
          cur.ctrlRemainder = Buffer.alloc(0);
          if (!cur.host) relaySessions.delete(sessionId);
        }
      });
      socket.on('error', () => {});
    }

    tryBridgeSession(sessionId);
  };

  socket.on('data', onData);
  socket.on('error', () => {});
});

// --- UDP STUN / NAT Reflexive Port Reflection ---
const udpSocket = dgram.createSocket('udp4');

udpSocket.on('message', (msg, rinfo) => {
  if (msg.toString('utf8').trim() === 'KEEPALIVE') {
    return;
  }
  const response = Buffer.from(
    JSON.stringify({
      type: 'stun-reflexive',
      ip: rinfo.address,
      port: rinfo.port,
    })
  );
  udpSocket.send(response, rinfo.port, rinfo.address);
});

udpSocket.on('error', (err) => {
  console.warn('[UDP STUN] Error:', err.message);
});

// Start Servers
server.listen(HTTP_PORT, '0.0.0.0', () => {
  console.log(
    `[Signaling] HTTP Signaling & Auth Server (${SERVER_VERSION}) listening on :${HTTP_PORT}`
  );
});

relayServer.listen(RELAY_PORT, '0.0.0.0', () => {
  console.log(
    `[Relay] Authenticated TCP Relay Tunnel Server listening on :${RELAY_PORT} (Auth required: ${REQUIRE_RELAY_AUTH})`
  );
});

udpSocket.bind(UDP_PORT, '0.0.0.0');
