//! UDP 心跳广播：每 2 秒向 255.255.255.255:47890 广播本机信息（HMAC 签名），
//! 并监听主控的单播「存在应答」——被控由此得知主控 IP 并主动发起控制连接。
//! （UDP 出站后的回包走有状态放行，被控端无需任何防火墙规则。）

use crate::agent::state::AgentState;
use crate::common::protocol::{
    verify_presence, HeartbeatBase, HeartbeatSigned, PresenceSigned, DISCOVERY_PORT, PROTO_VERSION,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;

pub async fn run_heartbeat(state: Arc<AgentState>) {
    let sock = match UdpSocket::bind(("0.0.0.0", 0)).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("心跳 socket 创建失败: {e}");
            return;
        }
    };
    if sock.set_broadcast(true).is_err() {
        eprintln!("设置广播失败");
    }
    let target = ("255.255.255.255", DISCOVERY_PORT);

    loop {
        let master_id = state.master_id();
        let free = state.refresh_vram_free();
        let hb = HeartbeatBase {
            v: PROTO_VERSION,
            kind: "hb".into(),
            id: state.id.clone(),
            hostname: crate::common::sysinfo::hostname(),
            ips: crate::common::sysinfo::local_ips(),
            gpu: state.gpu.name.clone(),
            vram_free_mb: free.or(state.gpu.vram_free_mb),
            vram_total_mb: state.gpu.vram_total_mb,
            version: crate::common::sysinfo::app_version(),
            master_id: master_id.clone(),
            rpc_port: state.rpc.port(),
            ctl_port: crate::common::protocol::CONTROL_PORT,
            ts: chrono::Utc::now().timestamp(),
        };
        let signed = HeartbeatSigned {
            sig: crate::common::protocol::sign_heartbeat_base(&hb),
            base: hb,
        };
        match serde_json::to_vec(&signed) {
            Ok(buf) => {
                if let Err(e) = sock.send_to(&buf, target).await {
                    eprintln!("心跳发送失败: {e}");
                }
            }
            Err(e) => eprintln!("心跳序列化失败: {e}"),
        }

        // 监听主控的存在应答（UDP 回包，最多等 600ms）
        let mut buf = vec![0u8; 2048];
        while let Ok(Ok((n, src))) =
            tokio::time::timeout(Duration::from_millis(600), sock.recv_from(&mut buf)).await
        {
            if let Ok(p) = serde_json::from_slice::<PresenceSigned>(&buf[..n]) {
                if p.base.kind != "presence" || p.base.v != PROTO_VERSION || !verify_presence(&p) {
                    continue;
                }
                let now = chrono::Utc::now().timestamp();
                if (p.base.ts - now).abs() > 120 {
                    continue;
                }
                // 已配对时只接受已配对主控的应答
                if let Some(cur) = &master_id {
                    if &p.base.master_id != cur {
                        continue;
                    }
                }
                state.note_presence(&p.base.master_id, &src.ip().to_string(), p.base.ctl_port);
            }
        }

        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
