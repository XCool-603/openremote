import http from 'node:http';
import dgram from 'node:dgram';
import net from 'node:net';

const HTTP_PORT = parseInt(process.env.PORT || '28844', 10);
const UDP_PORT = parseInt(process.env.UDP_PORT || '28844', 10);
const RELAY_PORT = parseInt(process.env.RELAY_PORT || '28845', 10);

// In-memory registry of online peer devices
// Key: normalized device_code (e.g. "739281904")
const peerRegistry = new Map();

// Relay sessions awaiting or bridged
// Key: session_id -> { hostSocket, ctrlSocket, timer }
const relaySessions = new Map();

// Helper to normalize device codes
function normalizeCode(code) {
  return String(code || '').replace(/[-\s]/g, '').trim();
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

// --- HTTP Signaling API ---
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

  // 1. Device Registration / Heartbeat (Dual-stack IPv4 + IPv6)
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
          deviceId: data.device_id || 'unknown',
          deviceCode: data.device_code,
          normalizedCode: code,
          deviceName: data.device_name || 'OpenRemote Peer',
          localIp: data.local_ip || clientIp,
          listenPort: data.listen_port || 28843,
          publicIp: clientIp,
          publicPort: data.public_port || data.listen_port || 28843,
          ipv6: data.ipv6 || null,
          lastSeen: Date.now(),
        };

        peerRegistry.set(code, peerInfo);
        console.log(`[Signaling] Registered device ${data.device_code} (${peerInfo.deviceName}) IPv4: ${clientIp}, IPv6: ${peerInfo.ipv6 || 'none'}`);

        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ success: true, publicIp: clientIp, registered: peerInfo }));
      } catch (err) {
        res.writeHead(400, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ error: 'Invalid JSON payload' }));
      }
    });
    return;
  }

  // 2. Lookup Peer by Device Code (Candidates: IPv6 Global, Local LAN, Public NAT Reflexive)
  if (req.method === 'GET' && url.pathname.startsWith('/api/lookup/')) {
    const rawCode = url.pathname.replace('/api/lookup/', '');
    const code = normalizeCode(rawCode);
    const peer = peerRegistry.get(code);

    if (peer) {
      const candidates = [];
      // High priority: Direct Public IPv6 (bypasses all NATs and firewalls!)
      if (peer.ipv6) {
        candidates.push(`[${peer.ipv6}]:${peer.listenPort}`);
      }
      // Local LAN address (for peers on same subnet)
      candidates.push(`${peer.localIp}:${peer.listenPort}`);
      // Public IPv4 NAT reflexive address
      candidates.push(`${peer.publicIp}:${peer.publicPort}`);

      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(
        JSON.stringify({
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
        })
      );
    } else {
      res.writeHead(404, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ found: false, message: `Device '${rawCode}' is offline or not found` }));
    }
    return;
  }

  // 3. Request Relay Session (Fallback for strict NAT/no IPv6)
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

  // 4. Status & Healthcheck
  if (url.pathname === '/api/status' || url.pathname === '/health' || url.pathname === '/') {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(
      JSON.stringify({
        status: 'online',
        server: 'OpenRemote Dual-Stack Signaling & Relay Server',
        onlinePeers: peerRegistry.size,
        activeRelays: relaySessions.size,
        features: ['IPv6-Direct', 'STUN-Reflective', 'TCP-Relay-Tunnel'],
        timestamp: new Date().toISOString(),
      })
    );
    return;
  }
    );
    return;
  }

  res.writeHead(404, { 'Content-Type': 'application/json' });
  res.end(JSON.stringify({ error: 'Endpoint not found' }));
});

// --- TCP Relay Tunneling Server (100% Connectivity Fallback) ---
const relayServer = net.createServer((socket) => {
  let headerBuffer = '';
  let identified = false;

  const onData = (chunk) => {
    if (identified) return;
    headerBuffer += chunk.toString('utf8');
    const newlineIndex = headerBuffer.indexOf('\n');
    if (newlineIndex !== -1) {
      identified = true;
      const line = headerBuffer.slice(0, newlineIndex).trim();
      const remainder = chunk.slice(Buffer.byteLength(line, 'utf8') + 1);

      // Protocol: "OPRM_RELAY <sessionId> <HOST|CTRL>"
      const parts = line.split(' ');
      if (parts[0] !== 'OPRM_RELAY' || parts.length < 3) {
        socket.destroy();
        return;
      }

      const sessionId = parts[1];
      const role = parts[2].toUpperCase();
      let session = relaySessions.get(sessionId);

      if (!session) {
        session = { host: null, ctrl: null, createdAt: Date.now() };
        relaySessions.set(sessionId, session);
      }

      socket.removeListener('data', onData);

      if (role === 'HOST') {
        session.host = socket;
        console.log(`[Relay] Host connected for session ${sessionId}`);
      } else {
        session.ctrl = socket;
        console.log(`[Relay] Controller connected for session ${sessionId}`);
      }

      // If both sides connected, bridge the sockets directly!
      if (session.host && session.ctrl) {
        console.log(`[Relay] Bridging data pipeline for session ${sessionId}`);
        session.host.pipe(session.ctrl);
        session.ctrl.pipe(session.host);

        const cleanup = () => {
          session.host?.destroy();
          session.ctrl?.destroy();
          relaySessions.delete(sessionId);
        };
        session.host.on('close', cleanup);
        session.ctrl.on('close', cleanup);
        session.host.on('error', cleanup);
        session.ctrl.on('error', cleanup);
      }

      if (remainder.length > 0) {
        socket.unshift(remainder);
      }
    }
  };

  socket.on('data', onData);
});

// --- UDP STUN / NAT Reflexive Port Reflection ---
const udpSocket = dgram.createSocket('udp4');

udpSocket.on('message', (msg, rinfo) => {
  const response = Buffer.from(
    JSON.stringify({
      type: 'stun-reflexive',
      ip: rinfo.address,
      port: rinfo.port,
    })
  );
  udpSocket.send(response, rinfo.port, rinfo.address);
});

// Start Servers
server.listen(HTTP_PORT, '0.0.0.0', () => {
  console.log(`[Signaling] Dual-stack HTTP Signaling Server listening on :${HTTP_PORT}`);
});

relayServer.listen(RELAY_PORT, '0.0.0.0', () => {
  console.log(`[Relay] TCP Relay Tunnel Server listening on :${RELAY_PORT}`);
});

udpSocket.bind(UDP_PORT, '0.0.0.0');
