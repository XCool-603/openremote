use crate::transport::{ConnectionChannel, QuicClient, QuicServer};
use anyhow::{anyhow, Context, Result};
use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncUdpSocket, Endpoint, EndpointConfig, TransportConfig, UdpPoller};
use std::fmt;
use std::future::Future;
use std::io::{self, IoSliceMut};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::task::{ready, Context as TaskContext, Poll};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, Interest};
use tokio::sync::{mpsc, Mutex as AsyncMutex, Notify};

type BoxedUnitFut = Pin<Box<dyn Future<Output = io::Result<()>> + Send>>;

struct FnUdpPoller<F> {
    make_fut: F,
    in_flight: StdMutex<Option<BoxedUnitFut>>,
}

impl<F> fmt::Debug for FnUdpPoller<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FnUdpPoller").finish_non_exhaustive()
    }
}

impl<F, Fut> UdpPoller for FnUdpPoller<F>
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = io::Result<()>> + Send + 'static,
{
    fn poll_writable(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        let mut guard = self.in_flight.lock().unwrap();
        let fut = guard.get_or_insert_with(|| Box::pin((self.make_fut)()));
        let res = fut.as_mut().poll(cx);
        if res.is_ready() {
            *guard = None;
        }
        res
    }
}

fn quic_relay_transport_config() -> Arc<TransportConfig> {
    let mut cfg = TransportConfig::default();
    cfg.max_idle_timeout(Some(Duration::from_secs(15).try_into().unwrap()));
    cfg.keep_alive_interval(Some(Duration::from_secs(2)));
    cfg.initial_rtt(Duration::from_millis(35));
    // Strictly avoid IP fragmentation across PPPoE/VPN + 13-byte ORLY relay wrapper
    cfg.mtu_discovery_config(None);
    cfg.initial_mtu(1200);
    let _ = cfg.min_mtu(1200);
    cfg.stream_receive_window((8 * 1024 * 1024u32).into());
    cfg.receive_window((16 * 1024 * 1024u32).into());
    cfg.send_window(8 * 1024 * 1024);
    Arc::new(cfg)
}

fn code_to_9bytes(code: &str) -> Result<[u8; 9]> {
    let clean = crate::signaling::clean_code(code);
    let bytes = clean.as_bytes();
    if bytes.len() != 9 {
        return Err(anyhow!("Invalid 9-digit device code '{}'", code));
    }
    let mut out = [0u8; 9];
    out.copy_from_slice(bytes);
    Ok(out)
}

// ============================================================================
// Controller Side: Direct UDP Socket -> `dxcool.cn:28844/udp` (`ORLY` mux)
// ============================================================================

struct UdpRelayClientSocket {
    io: tokio::net::UdpSocket,
    relay_udp_addr: SocketAddr,
    code9: [u8; 9],
    local_addr: SocketAddr,
}

impl fmt::Debug for UdpRelayClientSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UdpRelayClientSocket")
            .field("relay_udp_addr", &self.relay_udp_addr)
            .field("local_addr", &self.local_addr)
            .finish()
    }
}

impl AsyncUdpSocket for UdpRelayClientSocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Box::pin(FnUdpPoller {
            make_fut: move || {
                let s = Arc::clone(&self);
                async move { s.io.writable().await }
            },
            in_flight: StdMutex::new(None),
        })
    }

    fn try_send(&self, transmit: &Transmit) -> io::Result<()> {
        let len = transmit.contents.len();
        if len > 1900 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "QUIC datagram exceeds ORLY UDP relay buffer",
            ));
        }
        let mut pkt = [0u8; 2048];
        pkt[0..4].copy_from_slice(b"ORLY");
        pkt[4] = 0x03;
        pkt[5..14].copy_from_slice(&self.code9);
        pkt[14..14 + len].copy_from_slice(transmit.contents);
        self.io
            .try_io(Interest::WRITABLE, || {
                self.io.try_send_to(&pkt[..14 + len], self.relay_udp_addr)
            })
            .map(|_| ())
    }

    fn poll_recv(
        &self,
        cx: &mut TaskContext<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        if bufs.is_empty() || meta.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let mut tmp = [0u8; 2048];
        loop {
            ready!(self.io.poll_recv_ready(cx))?;
            match self.io.try_io(Interest::READABLE, || self.io.try_recv_from(&mut tmp)) {
                Ok((n, _from)) => {
                    if n > 5 && &tmp[0..5] == b"ORLY\x04" {
                        let payload_len = (n - 5).min(bufs[0].len());
                        bufs[0][..payload_len].copy_from_slice(&tmp[5..5 + payload_len]);
                        meta[0] = RecvMeta {
                            addr: self.relay_udp_addr,
                            len: payload_len,
                            stride: payload_len,
                            ecn: None,
                            dst_ip: None,
                        };
                        return Poll::Ready(Ok(1));
                    }
                    // Ignore non-data ORLY control echoes / stale STUN packets and continue
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(e) => return Poll::Ready(Err(e)),
            }
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.local_addr)
    }

    fn may_fragment(&self) -> bool {
        false
    }
}

/// Connect to target Host via UDP-QUIC Relay (`dxcool.cn:28844/udp`).
/// Eliminates TCP Head-of-Line blocking across lossy cross-ISP links!
pub async fn connect_quic_via_udp_relay(
    signal_url: &str,
    device_code: &str,
    auth_token: Option<&str>,
) -> Result<ConnectionChannel> {
    let relay_udp_addr = crate::nat::resolve_stun_addr_direct(signal_url)
        .await
        .ok_or_else(|| anyhow!("无法解析 UDP 中继地址: {}", signal_url))?;
    connect_quic_via_udp_relay_addr(relay_udp_addr, device_code, auth_token).await
}

pub async fn connect_quic_via_udp_relay_addr(
    relay_udp_addr: SocketAddr,
    device_code: &str,
    auth_token: Option<&str>,
) -> Result<ConnectionChannel> {
    let token = auth_token
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| anyhow!("请先登录账号后再使用中继连接"))?;
    let tok_bytes = token.as_bytes();
    if tok_bytes.len() > 240 {
        return Err(anyhow!("Auth token too long for UDP relay handshake"));
    }

    let code9 = code_to_9bytes(device_code)?;

    let std_sock = if relay_udp_addr.ip().is_loopback() {
        std::net::UdpSocket::bind("127.0.0.1:0")
            .map_err(|e| anyhow!("绑定本地回环 UDP 失败: {}", e))?
    } else {
        crate::nat::bind_udp_direct(0)
            .map_err(|e| anyhow!("绑定本地直连 UDP 失败: {}", e))?
    };
    std_sock.set_nonblocking(true)?;
    let io_sock = tokio::net::UdpSocket::from_std(std_sock)?;
    let local_addr = io_sock.local_addr()?;

    // Handshake: ORLY\x02 <9B code> <1B token_len> <token>
    let mut hello = Vec::with_capacity(15 + tok_bytes.len());
    hello.extend_from_slice(b"ORLY\x02");
    hello.extend_from_slice(&code9);
    hello.push(tok_bytes.len() as u8);
    hello.extend_from_slice(tok_bytes);

    let mut ack_buf = [0u8; 64];
    let mut joined = false;
    for attempt in 0..3 {
        let _ = io_sock.send_to(&hello, relay_udp_addr).await;
        let wait_ms = if attempt == 0 { 220 } else { 250 };
        if let Ok(Ok((n, _))) = tokio::time::timeout(
            Duration::from_millis(wait_ms),
            io_sock.recv_from(&mut ack_buf),
        )
        .await
        {
            if n >= 7 && &ack_buf[..7] == b"ORLY\x00OK" {
                joined = true;
                break;
            }
            if n >= 9 && &ack_buf[..9] == b"ORLY\x7fAUTH" {
                return Err(anyhow!("中继鉴权失败：请重新登录账号"));
            }
            if n >= 11 && &ack_buf[..11] == b"ORLY\x7fNOHOST" {
                return Err(anyhow!("被控端尚未开启 UDP-QUIC 中继（可能仍是旧版客户端）"));
            }
        }
    }

    if !joined {
        return Err(anyhow!(
            "UDP 中继握手超时（{}，回退 TCP 中继）",
            relay_udp_addr
        ));
    }

    let abstract_sock = Arc::new(UdpRelayClientSocket {
        io: io_sock,
        relay_udp_addr,
        code9,
        local_addr,
    });

    let runtime = quinn::default_runtime()
        .ok_or_else(|| anyhow!("No tokio runtime for UDP-QUIC relay"))?;
    let mut endpoint = Endpoint::new_with_abstract_socket(
        EndpointConfig::default(),
        None,
        abstract_sock,
        runtime,
    )
    .context("Failed creating UDP-QUIC relay client endpoint")?;

    QuicClient::apply_client_config_with_transport(&mut endpoint, quic_relay_transport_config())?;
    let client = QuicClient { endpoint };
    tokio::time::timeout(Duration::from_millis(2600), client.connect(relay_udp_addr))
        .await
        .map_err(|_| anyhow!("UDP-QUIC relay handshake timed out"))?
}

// ============================================================================
// Host Side: Persistent `OPRM_QUIC_HOST` bridge (`dxcool.cn:28845/tcp` <-> `QuicServer`)
// ============================================================================

struct RelayQuicHostSocket {
    recv_rx: StdMutex<mpsc::Receiver<(u16, Vec<u8>)>>,
    send_tx: mpsc::Sender<(u16, Vec<u8>)>,
    writable_notify: Arc<Notify>,
    local_addr: SocketAddr,
}

impl fmt::Debug for RelayQuicHostSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelayQuicHostSocket")
            .field("local_addr", &self.local_addr)
            .finish()
    }
}

struct HostRelayPoller {
    send_tx: mpsc::Sender<(u16, Vec<u8>)>,
    notify: Arc<Notify>,
    in_flight: StdMutex<Option<Pin<Box<dyn Future<Output = ()> + Send>>>>,
}

impl fmt::Debug for HostRelayPoller {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostRelayPoller").finish_non_exhaustive()
    }
}

impl UdpPoller for HostRelayPoller {
    fn poll_writable(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        if self.send_tx.capacity() > 0 {
            *self.in_flight.lock().unwrap() = None;
            return Poll::Ready(Ok(()));
        }
        let mut guard = self.in_flight.lock().unwrap();
        let fut = guard.get_or_insert_with(|| {
            let n = Arc::clone(&self.notify);
            Box::pin(async move { n.notified().await })
        });
        let res = fut.as_mut().poll(cx);
        if res.is_ready() {
            *guard = None;
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }
}

impl AsyncUdpSocket for RelayQuicHostSocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Box::pin(HostRelayPoller {
            send_tx: self.send_tx.clone(),
            notify: Arc::clone(&self.writable_notify),
            in_flight: StdMutex::new(None),
        })
    }

    fn try_send(&self, transmit: &Transmit) -> io::Result<()> {
        let peer_port = transmit.destination.port();
        match self
            .send_tx
            .try_send((peer_port, transmit.contents.to_vec()))
        {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => Err(io::ErrorKind::WouldBlock.into()),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(io::ErrorKind::BrokenPipe.into()),
        }
    }

    fn poll_recv(
        &self,
        cx: &mut TaskContext<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        if bufs.is_empty() || meta.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let mut rx = self.recv_rx.lock().unwrap();
        match rx.poll_recv(cx) {
            Poll::Ready(Some((peer_port, pkt))) => {
                let n = pkt.len().min(bufs[0].len());
                bufs[0][..n].copy_from_slice(&pkt[..n]);
                meta[0] = RecvMeta {
                    addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), peer_port),
                    len: n,
                    stride: n,
                    ecn: None,
                    dst_ip: None,
                };
                Poll::Ready(Ok(1))
            }
            Poll::Ready(None) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Host QUIC relay bridge closed",
            ))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.local_addr)
    }

    fn may_fragment(&self) -> bool {
        false
    }
}

fn append_relay_frame(dst: &mut Vec<u8>, peer_port: u16, pkt: &[u8]) {
    let total_len = (4 + pkt.len()) as u16;
    dst.extend_from_slice(&total_len.to_be_bytes());
    dst.extend_from_slice(&(peer_port as u32).to_be_bytes());
    dst.extend_from_slice(pkt);
}

/// Bind a persistent `QuicServer` backed by `dxcool.cn`'s UDP-QUIC relay bridge
/// and return `(Arc<QuicServer>, background_bridge_task)`.
pub fn create_host_quic_relay_server(
    relay_addr: String,
    device_code: String,
    stop_flag: Arc<AtomicBool>,
) -> Result<(Arc<QuicServer>, tokio::task::JoinHandle<()>)> {
    let (recv_tx, recv_rx) = mpsc::channel::<(u16, Vec<u8>)>(1024);
    let (send_tx, send_rx) = mpsc::channel::<(u16, Vec<u8>)>(1024);
    let send_rx_shared = Arc::new(AsyncMutex::new(send_rx));
    let writable_notify = Arc::new(Notify::new());
    let local_addr: SocketAddr = "127.0.0.1:28846".parse().unwrap();

    let sock = Arc::new(RelayQuicHostSocket {
        recv_rx: StdMutex::new(recv_rx),
        send_tx,
        writable_notify: Arc::clone(&writable_notify),
        local_addr,
    });

    let server = Arc::new(QuicServer::bind_with_abstract_socket(
        sock,
        quic_relay_transport_config(),
    )?);

    let clean_code = crate::signaling::clean_code(&device_code);
    let task = tokio::spawn(async move {
        let (host, port) = if let Some((h, p)) = relay_addr.rsplit_once(':') {
            (h.to_string(), p.parse::<u16>().unwrap_or(28845))
        } else {
            (relay_addr.clone(), 28845)
        };

        while !stop_flag.load(Ordering::Acquire) {
            let conn_res = if host == "127.0.0.1" || host == "localhost" {
                tokio::net::TcpStream::connect((host.as_str(), port))
                    .await
                    .map_err(|e| anyhow!("{}", e))
            } else {
                crate::signaling::connect_tcp_direct(&host, port, Duration::from_secs(4)).await
            };
            match conn_res {
                Ok(mut stream) => {
                    let cmd = format!("OPRM_QUIC_HOST {}\n", clean_code);
                    if stream.write_all(cmd.as_bytes()).await.is_err()
                        || stream.flush().await.is_err()
                    {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                    crate::diag_log(&format!(
                        "Host UDP-QUIC Relay datagram bridge connected ({})",
                        relay_addr
                    ));

                    let (mut reader, mut writer) = tokio::io::split(stream);
                    let send_rx_w = Arc::clone(&send_rx_shared);
                    let notify_w = Arc::clone(&writable_notify);
                    let stop_w = Arc::clone(&stop_flag);

                    let writer_task = tokio::spawn(async move {
                        let mut rx = send_rx_w.lock().await;
                        let mut batch_buf = Vec::with_capacity(4096);
                        while !stop_w.load(Ordering::Acquire) {
                            match rx.recv().await {
                                Some((peer_port, pkt)) => {
                                    notify_w.notify_one();
                                    batch_buf.clear();
                                    append_relay_frame(&mut batch_buf, peer_port, &pkt);
                                    // Drain any immediately queued datagrams into one TCP write
                                    while batch_buf.len() < 16384 {
                                        match rx.try_recv() {
                                            Ok((pp2, p2)) => {
                                                notify_w.notify_one();
                                                append_relay_frame(&mut batch_buf, pp2, &p2);
                                            }
                                            Err(_) => break,
                                        }
                                    }
                                    if writer.write_all(&batch_buf).await.is_err() {
                                        break;
                                    }
                                }
                                None => break,
                            }
                        }
                    });

                    // Reader loop: [u16 BE totalLen][u32 BE peerId][QUIC datagram]
                    while !stop_flag.load(Ordering::Acquire) {
                        let mut len_buf = [0u8; 2];
                        if reader.read_exact(&mut len_buf).await.is_err() {
                            break;
                        }
                        let total_len = u16::from_be_bytes(len_buf) as usize;
                        if total_len < 4 || total_len > 4096 {
                            break;
                        }
                        let mut frame = vec![0u8; total_len];
                        if reader.read_exact(&mut frame).await.is_err() {
                            break;
                        }
                        let peer_id = u32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]);
                        let payload = frame[4..].to_vec();
                        let _ = recv_tx.try_send((peer_id as u16, payload));
                    }

                    writer_task.abort();
                }
                Err(_) => {}
            }
            tokio::time::sleep(Duration::from_millis(1500)).await;
        }
    });

    Ok((server, task))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Message;
    use tokio::net::{TcpListener, UdpSocket};

    #[tokio::test]
    async fn test_udp_quic_relay_e2e_roundtrip() {
        let tcp_listener = TcpListener::bind("127.0.0.1:0").await.expect("bind tcp");
        let tcp_addr = tcp_listener.local_addr().unwrap();
        let udp_srv = Arc::new(UdpSocket::bind("127.0.0.1:0").await.expect("bind udp"));
        let udp_addr = udp_srv.local_addr().unwrap();

        let (c2h_tx, mut c2h_rx) = mpsc::channel::<Vec<u8>>(256);
        let ctrl_ep: Arc<StdMutex<Option<SocketAddr>>> = Arc::new(StdMutex::new(None));
        let ctrl_ep_tcp = Arc::clone(&ctrl_ep);
        let udp_for_tcp = Arc::clone(&udp_srv);

        // Local mock relay server mirroring server/signaling.js ORLY <-> OPRM_QUIC_HOST bridge
        let srv_tcp_task = tokio::spawn(async move {
            let (stream, _) = tcp_listener.accept().await.unwrap();
            let (mut rd, mut wr) = tokio::io::split(stream);
            // Read "OPRM_QUIC_HOST 123456789\n"
            let mut hdr = Vec::new();
            let mut b = [0u8; 1];
            while rd.read_exact(&mut b).await.is_ok() {
                hdr.push(b[0]);
                if b[0] == b'\n' {
                    break;
                }
            }
            let writer_task = tokio::spawn(async move {
                while let Some(pkt) = c2h_rx.recv().await {
                    let mut out = Vec::with_capacity(6 + pkt.len());
                    out.extend_from_slice(&((4 + pkt.len()) as u16).to_be_bytes());
                    out.extend_from_slice(&(10001u32).to_be_bytes());
                    out.extend_from_slice(&pkt);
                    if wr.write_all(&out).await.is_err() {
                        break;
                    }
                }
            });
            loop {
                let mut lbuf = [0u8; 2];
                if rd.read_exact(&mut lbuf).await.is_err() {
                    break;
                }
                let tlen = u16::from_be_bytes(lbuf) as usize;
                let mut frame = vec![0u8; tlen];
                if rd.read_exact(&mut frame).await.is_err() {
                    break;
                }
                let caddr_opt = *ctrl_ep_tcp.lock().unwrap();
                if let Some(caddr) = caddr_opt {
                    let mut out = Vec::with_capacity(5 + frame.len() - 4);
                    out.extend_from_slice(b"ORLY\x04");
                    out.extend_from_slice(&frame[4..]);
                    let _ = udp_for_tcp.send_to(&out, caddr).await;
                }
            }
            writer_task.abort();
        });

        let udp_for_rx = Arc::clone(&udp_srv);
        let ctrl_ep_udp = Arc::clone(&ctrl_ep);
        let srv_udp_task = tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            while let Ok((n, from)) = udp_for_rx.recv_from(&mut buf).await {
                if n >= 5 && &buf[..5] == b"ORLY\x02" {
                    *ctrl_ep_udp.lock().unwrap() = Some(from);
                    let _ = udp_for_rx.send_to(b"ORLY\x00OK", from).await;
                } else if n > 14 && &buf[..5] == b"ORLY\x03" {
                    let _ = c2h_tx.send(buf[14..n].to_vec()).await;
                }
            }
        });

        let stop = Arc::new(AtomicBool::new(false));
        let (host_srv, bridge_task) =
            create_host_quic_relay_server(tcp_addr.to_string(), "123-456-789".to_string(), Arc::clone(&stop))
                .expect("create host quic relay server");

        let host_accept_task = tokio::spawn(async move {
            let inc = host_srv.accept_incoming().await.expect("host accept_incoming");
            let ch = QuicServer::finish_incoming(inc).await.expect("host finish_incoming");
            let msg = ch.recv().await.expect("host recv Ping");
            if let Message::Ping { timestamp } = msg {
                ch.send(&Message::Pong { timestamp }).await.expect("host send Pong");
            }
            let frame_msg = Message::VideoFrameH264 {
                data: vec![0xAB; 65536],
                width: 1280,
                height: 800,
                is_keyframe: true,
                timestamp: 42,
            };
            ch.send(&frame_msg).await.expect("host send 64KB frame");
            let ack = ch.recv().await.expect("host recv VideoFrameAck");
            assert!(matches!(ack, Message::VideoFrameAck { timestamp: 42 }));
        });

        tokio::time::sleep(Duration::from_millis(80)).await;
        let ctrl_chan = connect_quic_via_udp_relay_addr(udp_addr, "123-456-789", Some("test_token"))
            .await
            .expect("controller connect_quic_via_udp_relay_addr");

        ctrl_chan.send(&Message::Ping { timestamp: 777 }).await.unwrap();
        let pong = ctrl_chan.recv().await.unwrap();
        match pong {
            Message::Pong { timestamp } => assert_eq!(timestamp, 777),
            other => panic!("Expected Pong, got {:?}", other),
        }
        let big = ctrl_chan.recv().await.unwrap();
        match big {
            Message::VideoFrameH264 { data, width, height, .. } => {
                assert_eq!(data.len(), 65536);
                assert_eq!((width, height), (1280, 800));
            }
            other => panic!("Expected VideoFrameH264, got {:?}", other),
        }
        ctrl_chan.send(&Message::VideoFrameAck { timestamp: 42 }).await.unwrap();
        let _ = host_accept_task.await;
        ctrl_chan.close().await;

        stop.store(true, Ordering::Release);
        bridge_task.abort();
        srv_tcp_task.abort();
        srv_udp_task.abort();
    }
}
