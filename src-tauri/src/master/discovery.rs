//! UDP 发现：监听 47890，校验 HMAC 心跳，维护节点在线状态并推送给前端。
//! 收到心跳后单播「存在应答」给被控（被控据此主动连接主控）。

use crate::common::protocol::{
    verify_heartbeat, HeartbeatSigned, PresenceBase, PresenceSigned, DISCOVERY_PORT, PROTO_VERSION,
};
use crate::master::state::{AppState, NodeInfo};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::net::UdpSocket;

/// 心跳超时（倍数于 2s 发送周期）
const OFFLINE_AFTER: Duration = Duration::from_secs(7);
/// 未配对节点消失多久后从列表移除
const UNPAIRED_FORGET: Duration = Duration::from_secs(30);

pub async fn run_discovery(app: AppHandle, state: Arc<AppState>) {
    let sock = match UdpSocket::bind(("0.0.0.0", DISCOVERY_PORT)).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("绑定发现端口 {DISCOVERY_PORT} 失败: {e}（可能被占用）");
            app.emit("discovery-error", format!("监听 UDP {DISCOVERY_PORT} 失败：{e}。请检查端口是否被占用，或防火墙是否放行。")).ok();
            return;
        }
    };

    eprintln!("[discovery] listening on UDP {DISCOVERY_PORT}");

    let mut buf = vec![0u8; 8192];
    let mut last_emit = Instant::now() - Duration::from_secs(10);
    // 存在应答限频：每个被控 id 每 2 秒最多回一次
    let mut last_presence: HashMap<String, Instant> = HashMap::new();

    loop {
        match tokio::time::timeout(OFFLINE_AFTER, sock.recv_from(&mut buf)).await {
            Ok(Ok((n, src))) => {
                if let Some(reply) = handle_packet(&app, &state, &buf[..n], src, &mut last_presence) {
                    // UDP 回包走有状态放行，被控无需防火墙规则；必须真正 await 发送
                    if let Err(e) = sock.send_to(&reply, src).await {
                        eprintln!("[discovery] presence reply to {src} failed: {e}");
                    } else {
                        eprintln!("[discovery] presence reply sent to {src} ({} bytes)", reply.len());
                    }
                }
                if last_emit.elapsed() > Duration::from_millis(500) {
                    emit_nodes(&app, &state);
                    last_emit = Instant::now();
                }
            }
            Ok(Err(e)) => {
                eprintln!("UDP 接收错误: {e}");
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
            Err(_) => { /* 超时：走下面的清扫 */ }
        }
        sweep(&app, &state);
        if last_emit.elapsed() > Duration::from_millis(500) {
            emit_nodes(&app, &state);
            last_emit = Instant::now();
        }
    }
}

fn handle_packet(
    app: &AppHandle,
    state: &Arc<AppState>,
    data: &[u8],
    src: SocketAddr,
    last_presence: &mut HashMap<String, Instant>,
) -> Option<Vec<u8>> {
    let signed: HeartbeatSigned = match serde_json::from_slice(data) {
        Ok(v) => v,
        Err(_) => return None, // 非 JSON 噪声，忽略
    };
    if signed.base.kind != "hb" || signed.base.v != PROTO_VERSION {
        return None;
    }
    if !verify_heartbeat(&signed) {
        eprintln!("[discovery] HMAC verify fail from {src} (id={})", signed.base.id);
        return None; // 签名不符：伪造/噪声，丢弃
    }
    // 防重放：时间戳偏差容忍 2 分钟
    let now_ts = chrono::Utc::now().timestamp();
    if (signed.base.ts - now_ts).abs() > 120 {
        return None;
    }

    let hb = signed.base;
    let info = NodeInfo {
        id: hb.id.clone(),
        hostname: hb.hostname.clone(),
        ips: hb.ips.clone(),
        gpu: hb.gpu.clone(),
        vram_total_mb: hb.vram_total_mb,
        vram_free_mb: hb.vram_free_mb,
        version: hb.version.clone(),
        rpc_port: hb.rpc_port,
        ctl_port: hb.ctl_port,
    };

    let is_paired_in_db = state.db.list_nodes().iter().any(|n| n.id == hb.id);
    {
        let mut nodes = state.nodes.lock().unwrap();
        let entry = nodes
            .entry(hb.id.clone())
            .or_insert_with(|| crate::master::state::LiveNode {
                info: info.clone(),
                last_seen: Instant::now(),
                paired: is_paired_in_db,
                first_seen: Instant::now(),
            });
        entry.info = info;
        entry.last_seen = Instant::now();
        if is_paired_in_db {
            entry.paired = true;
        }
    }

    // 已配对本主控的节点：更新 DB 里的 IP / 显存
    if hb.master_id.as_deref() == Some(state.master_id.as_str()) {
        state.refresh_paired_node(&hb.id);
    }

    // 单播存在应答（限频 2s/节点）：被控收到后主动连接主控 47891
    let throttle_ok = last_presence
        .get(&hb.id)
        .map(|t| t.elapsed() > Duration::from_secs(2))
        .unwrap_or(true);
    if throttle_ok {
        last_presence.insert(hb.id.clone(), Instant::now());
        let presence = PresenceBase {
            v: PROTO_VERSION,
            kind: "presence".into(),
            master_id: state.master_id.clone(),
            master_name: state.master_name.clone(),
            ctl_port: crate::common::protocol::CONTROL_PORT,
            ts: now_ts,
        };
        let signed_p = PresenceSigned {
            sig: crate::common::protocol::sign_presence_base(&presence),
            base: presence,
        };
        if let Ok(buf) = serde_json::to_vec(&signed_p) {
            return Some(buf); // 由 async 循环真正发送
        }
        let _ = app;
    }
    None
}

fn sweep(app: &AppHandle, state: &Arc<AppState>) {
    let mut changed = false;
    let mut nodes = state.nodes.lock().unwrap();
    let ids: Vec<String> = nodes.keys().cloned().collect();
    for id in ids {
        let offline = nodes
            .get(&id)
            .map(|n| n.last_seen.elapsed() > OFFLINE_AFTER)
            .unwrap_or(false);
        if !offline {
            continue;
        }
        let paired = nodes.get(&id).map(|n| n.paired).unwrap_or(false);
        let gone_for = nodes
            .get(&id)
            .map(|n| n.last_seen.elapsed())
            .unwrap_or(Duration::ZERO);
        if !paired && gone_for > UNPAIRED_FORGET {
            nodes.remove(&id);
            changed = true;
        }
    }
    drop(nodes);
    if changed {
        emit_nodes(app, state);
    }
}

pub fn emit_nodes(app: &AppHandle, state: &Arc<AppState>) {
    let snap = state.nodes_snapshot();
    app.emit("nodes-changed", &snap).ok();
}
