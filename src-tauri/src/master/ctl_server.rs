//! 主控控制服务器：监听 TCP 47891，接受被控端的主动连接。
//!
//! - PairHello：被控持有 PIN 发起配对（AEAD PairProof 证明）→ 配对成功后
//!   同一连接转为控制会话
//! - CtlHello：已配对被控用 node_key 认证建立持久会话
//! - 会话内主控下发 CtlRequest（R2I）、接收 CtlResponse（I2R）

use crate::common::crypto::{
    aead_open, aead_seal, derive_node_key, derive_session_keys, ecdh, random_ephemeral, Aead,
};
use crate::common::db::NodeRow;
use crate::common::protocol::{
    read_frame, read_json_frame, write_frame, write_json_frame, CtlRequest, CtlResponse,
    HandshakeMsg, CONTROL_PORT, FRAME_MAX,
};
use crate::master::discovery::emit_nodes;
use crate::master::state::AppState;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use x25519_dalek::PublicKey;

const DIR_I2R: u8 = 0x01; // 发起方（被控）→ 应答方（主控）
const DIR_R2I: u8 = 0x02; // 应答方（主控）→ 发起方（被控）

type PendingMap = Arc<Mutex<HashMap<String, oneshot::Sender<Result<serde_json::Value, String>>>>>;

/// 一个已认证的持久控制会话（主控视角）
pub struct CtlSession {
    /// 主控→被控 方向密钥（R2I）
    send_key: Arc<Aead>,
    seq_tx: Arc<AtomicU64>,
    pending: PendingMap,
    writer: mpsc::Sender<Vec<u8>>,
    /// 被控的真实连接 IP（TCP 对端）：RPC 拨号、防火墙下发都用它，
    /// 比心跳广播里的 IP 列表更可靠（多网卡机器）
    pub peer_ip: String,
}

impl CtlSession {
    pub async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let id = uuid::Uuid::new_v4().to_string();
        let req = CtlRequest { id: id.clone(), method: method.into(), params };
        let buf = serde_json::to_vec(&req).map_err(|e| e.to_string())?;
        let seq = self.seq_tx.fetch_add(1, Ordering::SeqCst);
        let frame = self.send_key.seal(seq, DIR_R2I, &buf, b"ctl");
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.clone(), tx);
        if self.writer.send(frame).await.is_err() {
            self.pending.lock().unwrap().remove(&id);
            return Err("连接已断开".into());
        }
        match tokio::time::timeout(Duration::from_secs(20), rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => Err("节点响应通道已关闭".into()),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err("节点响应超时".into())
            }
        }
    }
}

pub async fn run_ctl_server(app: AppHandle, state: Arc<AppState>) {
    let listener = match TcpListener::bind(("0.0.0.0", CONTROL_PORT)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("绑定控制端口 {CONTROL_PORT} 失败: {e}");
            app.emit("discovery-error", format!("监听控制端口 {CONTROL_PORT} 失败：{e}（防火墙需放行 TCP {CONTROL_PORT}）")).ok();
            return;
        }
    };
    eprintln!("[ctl-server] listening on TCP {CONTROL_PORT}");
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                let st = Arc::clone(&state);
                let ap = app.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = handle_conn(ap, st, stream, peer).await {
                        eprintln!("连接处理结束({peer}): {e}");
                    }
                });
            }
            Err(e) => {
                eprintln!("accept 失败: {e}");
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
        }
    }
}

async fn handle_conn(
    app: AppHandle,
    state: Arc<AppState>,
    mut stream: TcpStream,
    peer: SocketAddr,
) -> Result<(), String> {
    let hello = read_json_frame::<HandshakeMsg>(&mut stream)
        .await
        .map_err(|e| format!("读取握手失败: {e}"))?;
    match hello {
        HandshakeMsg::PairHello { .. } => do_pairing(app, state, stream, peer, hello).await,
        HandshakeMsg::CtlHello { .. } => do_ctl(app, state, stream, peer, hello).await,
        _ => Err("未知握手类型".into()),
    }
}

/// 被控发起配对：PairHello → PairAck → AEAD PairProof（试每个有效凭证）→ AEAD PairDone
/// 凭证两类：安装包注入的引导码（持久化在 DB，长期有效/可作废）、手动配对 PIN（内存，5 分钟）
async fn do_pairing(
    app: AppHandle,
    state: Arc<AppState>,
    mut stream: TcpStream,
    peer: SocketAddr,
    hello: HandshakeMsg,
) -> Result<(), String> {
    let HandshakeMsg::PairHello { id, epk, salt, hostname, gpu, vram_total_mb, version, token_id } = hello
    else {
        return Err("内部错误".into());
    };

    let epk_bytes: [u8; 32] = hex::decode(&epk)
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or("被控临时公钥格式错误")?;
    let salt_bytes = hex::decode(&salt).map_err(|_| "握手盐格式错误".to_string())?;
    let agent_pk = PublicKey::from(epk_bytes);
    let (sk, my_pk) = random_ephemeral();
    let shared = ecdh(&sk, &agent_pk);

    // 应答（明文）
    write_json_frame(
        &mut stream,
        &HandshakeMsg::PairAck {
            epk: hex::encode(my_pk.as_bytes()),
            master_name: state.master_name.clone(),
            master_id: state.master_id.clone(),
        },
    )
    .await
    .map_err(|e| format!("发送 PairAck 失败: {e}"))?;

    // 读凭证证明
    let proof_frame = read_frame(&mut stream)
        .await
        .map_err(|e| format!("等待凭证证明失败: {e}"))?;

    // 汇总候选凭证：库里的引导码（含过期检查）+ 内存中的待验证 PIN
    enum Credential {
        Token(String), // 引导码 id
        Pin(usize),    // pending_pins 下标
    }
    let now = chrono::Utc::now().timestamp();
    let mut candidates: Vec<(String, Credential)> = Vec::new();
    if !token_id.is_empty() {
        if let Some(row) = state.db.find_token(&token_id) {
            if row.is_active(now) {
                candidates.push((row.token, Credential::Token(row.id)));
            } else {
                eprintln!("[ctl-server] pair attempt with expired token {token_id} from {peer}");
            }
        } else {
            eprintln!("[ctl-server] pair attempt with unknown token {token_id} from {peer}");
        }
    }
    for (idx, e) in state
        .pending_pins
        .lock()
        .unwrap()
        .iter()
        .enumerate()
        .filter(|(_, e)| !e.expired())
    {
        candidates.push((e.pin.clone(), Credential::Pin(idx)));
    }

    // 逐个试 AEAD 证明：能解开即持有正确凭证
    let mut matched: Option<([u8; 32], [u8; 32], Credential)> = None; // (k_i2r, k_r2i, cred)
    for (secret, cred) in &candidates {
        let mut salt_secret = salt_bytes.clone();
        salt_secret.extend_from_slice(secret.as_bytes());
        let (k_i2r, k_r2i) = derive_session_keys(&shared, &salt_secret);
        if aead_open(&k_i2r, 0, DIR_I2R, &proof_frame, b"pair").is_ok() {
            matched = Some((k_i2r, k_r2i, match cred {
                Credential::Token(t) => Credential::Token(t.clone()),
                Credential::Pin(i) => Credential::Pin(*i),
            }));
            break;
        }
    }
    let Some((k_i2r, k_r2i, cred)) = matched else {
        write_json_frame(
            &mut stream,
            &HandshakeMsg::PairFail { reason: "凭证不正确或已失效".into() },
        )
        .await
        .ok();
        return Err("凭证不匹配".into());
    };

    // 凭证命中后的处理：引导码记使用次数；PIN 消费掉
    match &cred {
        Credential::Token(tid) => {
            let _ = state.db.bump_token_use(tid, chrono::Utc::now().timestamp());
            eprintln!("[ctl-server] paired via bootstrap token {tid}");
        }
        Credential::Pin(idx) => {
            let mut pins = state.pending_pins.lock().unwrap();
            if *idx < pins.len() {
                pins.remove(*idx);
            }
        }
    }

    // 配对完成（AEAD）
    let done = HandshakeMsg::PairDone {
        master_id: state.master_id.clone(),
        master_name: state.master_name.clone(),
    };
    let sealed = aead_seal(&k_r2i, 0, DIR_R2I, &serde_json::to_vec(&done).unwrap(), b"pair");
    write_frame(&mut stream, &sealed)
        .await
        .map_err(|e| format!("发送 PairDone 失败: {e}"))?;

    // 持久化节点
    let now = chrono::Utc::now().timestamp();
    let node_key = derive_node_key(&k_i2r);
    let ip = peer.ip().to_string();
    let row = NodeRow {
        id: id.clone(),
        hostname,
        last_ip: ip.clone(),
        gpu,
        vram_total_mb,
        vram_free_mb: None,
        version: Some(version),
        node_key: hex::encode(node_key),
        paired_at: now,
        last_seen: now,
    };
    state.db.upsert_node(&row).map_err(|e| format!("保存节点失败: {e}"))?;
    {
        let mut nodes = state.nodes.lock().unwrap();
        if let Some(live) = nodes.get_mut(&id) {
            live.paired = true;
        }
    }
    app.emit(
        "pair-success",
        serde_json::json!({
            "node_id": id,
            "hostname": row.hostname,
            "ip": ip,
            "message": format!("已与「{}」完成配对", row.hostname)
        }),
    )
    .ok();

    eprintln!("[ctl-server] paired session for {id} ({}) established", row.hostname);
    promote_and_read(app, state, id, stream, k_i2r, k_r2i, 1, 1, ip).await
}

/// 已配对被控的会话重建
async fn do_ctl(
    app: AppHandle,
    state: Arc<AppState>,
    mut stream: TcpStream,
    peer: SocketAddr,
    hello: HandshakeMsg,
) -> Result<(), String> {
    let HandshakeMsg::CtlHello { id, epk, salt } = hello else {
        return Err("内部错误".into());
    };
    let Some(node) = state.node_row(&id) else {
        write_json_frame(&mut stream, &HandshakeMsg::CtlFail { reason: "未配对".into() })
            .await
            .ok();
        return Err("未配对".into());
    };
    let node_key: [u8; 32] = hex::decode(&node.node_key)
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or("节点密钥损坏")?;

    let epk_bytes: [u8; 32] = hex::decode(&epk)
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or("被控临时公钥格式错误")?;
    let salt_bytes = hex::decode(&salt).map_err(|_| "握手盐格式错误".to_string())?;
    let agent_pk = PublicKey::from(epk_bytes);
    let (sk, my_pk) = random_ephemeral();
    let shared = ecdh(&sk, &agent_pk);

    write_json_frame(
        &mut stream,
        &HandshakeMsg::CtlReady {
            epk: hex::encode(my_pk.as_bytes()),
            master_id: state.master_id.clone(),
        },
    )
    .await
    .map_err(|e| format!("发送 CtlReady 失败: {e}"))?;

    let mut salt2 = salt_bytes;
    salt2.extend_from_slice(&node_key);
    let (k_i2r, k_r2i) = derive_session_keys(&shared, &salt2);

    // 更新节点在线 IP
    {
        let mut nodes = state.nodes.lock().unwrap();
        if let Some(live) = nodes.get_mut(&id) {
            live.info.ips = vec![peer.ip().to_string()];
            live.last_seen = std::time::Instant::now();
        }
    }

    eprintln!("[ctl-server] session for {id} ({}) re-established", node.hostname);
    promote_and_read(app, state, id, stream, k_i2r, k_r2i, 0, 0, peer.ip().to_string()).await
}

/// 会话登记 + 读循环。断开时清理会话并失败所有等待者。
#[allow(clippy::too_many_arguments)]
async fn promote_and_read(
    app: AppHandle,
    state: Arc<AppState>,
    node_id: String,
    stream: TcpStream,
    k_i2r: [u8; 32],
    k_r2i: [u8; 32],
    seq_tx0: u64,
    seq_rx0: u64,
    peer_ip: String,
) -> Result<(), String> {
    let (mut read_half, write_half) = stream.into_split();
    let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
    let seq_tx = Arc::new(AtomicU64::new(seq_tx0));
    let seq_rx = Arc::new(AtomicU64::new(seq_rx0));
    let send_key = Arc::new(Aead::new(k_r2i));
    let recv_key = Arc::new(Aead::new(k_i2r));

    // 写任务：从队列取已密封帧并写入
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(64);
    let write_task = tauri::async_runtime::spawn(async move {
        let mut write_half = write_half;
        while let Some(frame) = rx.recv().await {
            let mut buf = (frame.len() as u32).to_be_bytes().to_vec();
            buf.extend_from_slice(&frame);
            if write_half.write_all(&buf).await.is_err() {
                break;
            }
        }
    });

    let session = CtlSession {
        send_key,
        seq_tx,
        pending: Arc::clone(&pending),
        writer: tx,
        peer_ip,
    };
    state.ctl_sessions.lock().unwrap().insert(node_id.clone(), Arc::new(session));
    emit_nodes(&app, &state);

    let mut buf: Vec<u8> = vec![0u8; 65536];
    loop {
        let n = match tokio::time::timeout(
            Duration::from_secs(600),
            read_frame_into(&mut read_half, &mut buf),
        )
        .await
        {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => {
                eprintln!("[ctl-server] read error({node_id}): {e}");
                break;
            }
            Err(_) => continue, // 长空闲：等待下一条（被控会话空闲时无流量）
        };
        let plain = match recv_key.open(seq_rx.load(Ordering::SeqCst), DIR_I2R, &buf[..n], b"ctl")
        {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[ctl-server] 解密应答失败({node_id}): {e}");
                break;
            }
        };
        seq_rx.fetch_add(1, Ordering::SeqCst);
        let resp: CtlResponse = match serde_json::from_slice(&plain) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[ctl-server] 应答解析失败({node_id}): {e}");
                break;
            }
        };
        if let Some(tx) = pending.lock().unwrap().remove(&resp.id) {
            let result = if let Some(err) = resp.error {
                Err(format!("被控端错误 {}: {}", err.code, err.message))
            } else {
                Ok(resp.result.unwrap_or(serde_json::Value::Null))
            };
            let _ = tx.send(result);
        }
    }

    // 清理
    {
        let sessions = state.ctl_sessions.lock().unwrap();
        if let Some(s) = sessions.get(&node_id) {
            let mut p = s.pending.lock().unwrap();
            for (_, tx) in p.drain() {
                let _ = tx.send(Err("连接已断开".into()));
            }
        }
    }
    state.ctl_sessions.lock().unwrap().remove(&node_id);
    write_task.abort();
    emit_nodes(&app, &state);
    eprintln!("[ctl-server] session for {node_id} closed");
    Err("连接断开".into())
}

/// 带长度上限读帧，返回字节数
async fn read_frame_into(
    reader: &mut (impl tokio::io::AsyncRead + Unpin),
    buf: &mut Vec<u8>,
) -> std::io::Result<usize> {
    use tokio::io::AsyncReadExt;
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > FRAME_MAX {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("帧过大: {len}"),
        ));
    }
    if buf.len() < len {
        buf.resize(len, 0);
    }
    reader.read_exact(&mut buf[..len]).await?;
    Ok(len)
}
