use anyhow::Result;
use openremote::protocol::Message;
use openremote::signaling;
use openremote::transport::ConnectionChannel;

#[tokio::main]
async fn main() -> Result<()> {
    let target_code = "849-201-948";
    let signal_url = "http://dxcool.cn:28844";
    let relay_addr = signaling::parse_relay_address(signal_url);

    println!("=======================================================");
    println!("🧪 OpenRemote 端到端控制链路真实自动化测试程序");
    println!("=======================================================");
    println!("📍 目标设备代码: {}", target_code);
    println!("📍 信令服务器: {}", signal_url);
    println!("📍 中继隧道地址: {}", relay_addr);

    // 1. 信令查询
    println!("\n[1/3] 正在查询信令服务器...");
    match signaling::lookup_peer_candidates(signal_url, target_code).await {
        Ok(candidates) => {
            println!("✅ 成功获取目标设备候选直连地址: {:?}", candidates);
        }
        Err(e) => {
            println!("⚠️ 信令查询返回: {}", e);
        }
    }

    // 2. 连接 TCP 中继隧道
    println!("\n[2/3] 正在连接 TCP Relay 中继隧道 {}...", relay_addr);
    let stream = signaling::join_relay_tunnel(&relay_addr, target_code).await?;
    println!("✅ 成功建立中继隧道连接并发送握手头!");

    let channel = ConnectionChannel::new_tcp(stream);

    // 3. 发送 Handshake
    println!("\n[3/3] 正在向对端发送 Handshake 握手协议...");
    let handshake = Message::Handshake {
        client_id: "test-controller-e2e".to_string(),
        token: None,
    };
    channel.send(&handshake).await?;
    println!("✅ Handshake 发送完成，正在等待被控端回传屏幕流...");

    // 4. 等待并验证前 5 帧屏幕图像
    let start_time = std::time::Instant::now();
    let mut frame_count = 0;

    while frame_count < 5 {
        tokio::select! {
            res = channel.recv() => {
                match res {
                    Ok(Message::ScreenFrame { width, height, timestamp, data }) => {
                        frame_count += 1;
                        println!("🎉 [帧 #{}] 收到真实屏幕画面! 分辨率: {}x{}, 大小: {} 字节, 时间戳: {}",
                            frame_count, width, height, data.len(), timestamp
                        );
                    }
                    Ok(other) => {
                        println!("ℹ️ 收到其他消息: {:?}", other);
                    }
                    Err(e) => {
                        eprintln!("❌ 接收帧发生错误: {}", e);
                        break;
                    }
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {
                eprintln!("❌ 等待屏幕流超时 (10秒未收到任何帧)！");
                break;
            }
        }
    }

    let elapsed = start_time.elapsed();
    println!("\n=======================================================");
    if frame_count > 0 {
        println!("🏆 测试完全通过！在 {:?} 内成功接收 {} 帧真实屏幕画面！", elapsed, frame_count);
    } else {
        println!("💥 测试失败：未能在规定时间内收到屏幕帧！");
    }
    println!("=======================================================");

    Ok(())
}
