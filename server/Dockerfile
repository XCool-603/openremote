# OpenRemote Dual-Stack Signaling & Relay Server
FROM node:20-alpine

WORKDIR /app

# Copy server code (zero external dependencies, using Node.js built-ins)
COPY signaling.js .

# Expose ports:
# 28844/tcp: HTTP Signaling & Device Registry API
# 28844/udp: STUN NAT Reflexive Port Reflection
# 28845/tcp: TCP Data Relay Tunnel
EXPOSE 28844/tcp
EXPOSE 28844/udp
EXPOSE 28845/tcp

ENV PORT=28844 \
    UDP_PORT=28844 \
    RELAY_PORT=28845 \
    NODE_ENV=production

# Healthcheck
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
  CMD wget -qO- http://127.0.0.1:28844/api/status || exit 1

CMD ["node", "signaling.js"]
