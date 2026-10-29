use anyhow::Result;
use openremote::protocol::Message;
use openremote::signaling;
use openremote::transport::ConnectionChannel;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[tokio::main]
async fn main() -> Result<()> {
    let _ = env_logger::builder()
        .filter_level(log::LevelFilter::Debug)
        .try_init();

    let rand_id = rand::random::<u32>() % 1000000;
    let test_device_code = format!("999-{:03}-{:03}", rand_id / 1000, rand_id % 1000);
    let signal_url = "http://dxcool.cn:28844";
    let relay_addr = signaling::parse_relay_address(signal_url);

    println!("=======================================================");
    println!("🔬 OpenRemote 中继全双工穿透端到端双向严格自检程序");
    println!("=======================================================");
    println!("📍 测试中继地址: {}", relay_addr);
    println!("📍 设备代码: {}", test_device_code);

    let is_running = Arc::new(AtomicBool::new(true));
    let is_running_host = is_running.clone();

    // 1. 模拟被控端（Host）连接中继并待命
    println!("\n[1/4] 启动被控端 (Host)，正在连接中继并注册...");
    let host_stream = signaling::register_relay_tunnel(&relay_addr, &test_device_code).await?;
    println!("✅ 被控端 (Host) 注册成功，正在等待控制端连入...");

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // 2. 模拟控制端（Ctrl）连接中继并连入
    println!("\n[2/4] 启动控制端 (Ctrl)，正在通过中继加入该设备...");
    let ctrl_stream = signaling::join_relay_tunnel(&relay_addr, &test_device_code).await?;
    println!("✅ 控制端 (Ctrl) 握手成功!");

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let host_channel = openremote::transport::ConnectionChannel::new_tcp(host_stream);
    let ctrl_channel = openremote::transport::ConnectionChannel::new_tcp(ctrl_stream);

    // 被控端开异步任务等待第一包消息
    let host_chan_rx = host_channel.clone();
    let host_rx_task = tokio::spawn(async move {
        println!("  [Host Task] 开始等待接收消息 (channel.recv())...");
        match host_chan_rx.recv().await {
            Ok(msg) => {
                println!("  [Host Task] 🎉 成功接收消息: {:?}", msg);
                Ok(())
            }
            Err(e) => {
                println!("  [Host Task] ❌ 接收失败: {}", e);
                Err(e)
            }
        }
    });

    // 3. 控制端向被控端发送 Handshake 报文
    println!("\n[3/4] 控制端发送 Handshake 握手协议...");
    let handshake = Message::Handshake {
        client_id: "e2e-controller".to_string(),
        token: None,
    };
    ctrl_channel.send(&handshake).await?;
    println!("  [Ctrl Task] Handshake 已通过 ctrl_channel.send 发出");

    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), host_rx_task).await
        .map_err(|_| anyhow::anyhow!("Host rx task 超时"))??;

    // 4. 被控端向控制端回传模拟屏幕数据帧 (连续 5 帧)
    println!("\n[4/4] 被控端向控制端推流屏幕帧...");
    let host_chan_clone = host_channel.clone();
    let host_task = tokio::spawn(async move {
        for i in 1..=5 {
            let fake_data = vec![128u8; 1024 * 30]; // 30KB 帧
            let frame = Message::ScreenFrame {
                data: fake_data,
                width: 1920,
                height: 1080,
                timestamp: chrono::Utc::now().timestamp_millis(),
            };
            if let Err(e) = host_chan_clone.send(&frame).await {
                eprintln!("❌ 被控端发送帧失败: {}", e);
                break;
            }
            println!("  📤 被控端已发出帧 #{} (30KB)", i);
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    });

    let ctrl_chan_clone = ctrl_channel.clone();
    let mut received_frames = 0;
    while received_frames < 5 {
        match ctrl_chan_clone.recv().await {
            Ok(Message::ScreenFrame { width, height, data, timestamp }) => {
                received_frames += 1;
                println!("  📥 控制端成功收到帧 #{}: {}x{}, 数据大小: {} 字节, 时间戳: {}",
                    received_frames, width, height, data.len(), timestamp
                );
            }
            Ok(other) => {
                println!("  ℹ️ 控制端收到其他消息: {:?}", other);
            }
            Err(e) => {
                eprintln!("❌ 控制端接收失败: {}", e);
                break;
            }
        }
    }

    let _ = host_task.await;

    println!("\n=======================================================");
    if received_frames == 5 {
        println!("🏆 完美！中继服务器 dxcool.cn:28845 双向管道全双工推流 100% 成功！");
    } else {
        println!("💥 测试未完全通过，仅收到 {} 帧", received_frames);
    }
    println!("=======================================================");

    Ok(())
}
