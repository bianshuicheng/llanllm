//! 被控端控制客户端：主动连接主控 47891（出站，被控零入站防火墙）。
//!
//! - 未配对 + 有凭证（安装包注入的引导码 或 手动输入的 6 位 PIN）+ 已发现主控
//!   → 发起配对（PairHello → PairAck → AEAD PairProof → AEAD PairDone）
//!   凭证随 PairHello 明文携带指纹、密文帧证明持有本体，主控在库中核对
//! - 已配对 → 用 node_key 建立持久控制会话，接收主控指令并应答
//! - 断线自动重连（指数退避，上限 10s）

use crate::agent::state::AgentState;
use crate::common::crypto::{
    aead_open, aead_seal, derive_node_key, derive_session_keys, ecdh, random_bytes,
    random_ephemeral, Aead,
};
use crate::common::protocol::{
    read_frame, read_json_frame, write_frame, write_json_frame, CtlRequest, CtlResponse,
    HandshakeMsg,
};
use std::sync::Arc;
use tokio::net::TcpStream;
use x25519_dalek::PublicKey;

const DIR_I2R: u8 = 0x01; // 发起方（被控）→ 应答方（主控）
const DIR_R2I: u8 = 0x02; // 应答方（主控）→ 发起方（被控）

pub async fn run_ctl_client(state: Arc<AgentState>) {
    let mut backoff = 2u64;
    loop {
        let paired = state.is_paired();
        let pin = if paired { None } else { state.peek_pending_pin() };
        let bootstrap = if paired { None } else { state.peek_bootstrap() };
        let addr = state.pick_master_addr();

        match (paired, &addr) {
            // 已配对：维持控制会话
            (true, Some(addr)) => {
                if let Err(e) = ctl_session(&state, addr).await {
                    state.push_ui_log(format!("控制会话断开：{e}，稍后自动重连"));
                } else {
                    backoff = 2;
                }
            }
            // 未配对 + 凭证 + 已发现主控：主动配对
            (false, Some(addr)) => {
                let attempt = match (&bootstrap, &pin) {
                    (Some((tid, tok)), _) => try_pair(&state, addr, tid.clone(), Some(tok.clone())).await,
                    (_, Some(p)) => try_pair(&state, addr, String::new(), Some(p.clone())).await,
                    _ => Ok(()),
                };
                match attempt {
                    Ok(()) => backoff = 2,
                    Err(e) => state.push_ui_log(format!(
                        "配对尝试失败（{}:{}）：{e}",
                        addr.0, addr.1
                    )),
                }
            }
            // 未配对且还没发现主控：安静等待心跳/存在应答
            _ => {}
        }
        tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(10);
    }
}

/// 主动配对：`secret` 为 Some 时放进 PIN 证明帧（引导码或 6 位 PIN）。
/// 配对成功后同一连接直接转为控制会话（keys 延续）。
async fn try_pair(
    state: &Arc<AgentState>,
    addr: &(String, u16),
    token_id: String,
    secret: Option<String>,
) -> Result<(), String> {
    let addr_str = format!("{}:{}", addr.0, addr.1);
    let mut stream = tokio::time::timeout(std::time::Duration::from_secs(5), TcpStream::connect(&addr_str))
        .await
        .map_err(|_| "连接主控超时".to_string())?
        .map_err(|e| format!("连接失败: {e}"))?;

    let (sk, pk) = random_ephemeral();
    let salt = random_bytes(16);
    let gpu = state.gpu.clone();
    write_json_frame(
        &mut stream,
        &HandshakeMsg::PairHello {
            id: state.id.clone(),
            epk: hex::encode(pk.as_bytes()),
            salt: hex::encode(&salt),
            hostname: crate::common::sysinfo::hostname(),
            gpu: gpu.name.clone(),
            vram_total_mb: gpu.vram_total_mb,
            version: crate::common::sysinfo::app_version(),
            token_id,
        },
    )
    .await
    .map_err(|e| format!("发送 PairHello 失败: {e}"))?;

    let ack = read_json_frame::<HandshakeMsg>(&mut stream)
        .await
        .map_err(|e| format!("读取 PairAck 失败: {e}"))?;
    let (epk, _master_name, master_id) = match ack {
        HandshakeMsg::PairAck { epk, master_name, master_id } => (epk, master_name, master_id),
        HandshakeMsg::PairFail { reason } => return Err(reason),
        _ => return Err("主控应答异常".into()),
    };

    // 若已配对到其他主控则拒绝
    if let Some(cur) = state.master_id() {
        if cur != master_id {
            return Err("本机已配对到其他主控".into());
        }
    }

    let epk_bytes: [u8; 32] = hex::decode(&epk)
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or("主控临时公钥格式错误")?;
    // salt 即本端生成的原始字节（线上是它的 hex 编码），无需再解码
    let peer = PublicKey::from(epk_bytes);
    let shared = ecdh(&sk, &peer);
    let mut salt_secret = salt;
    let secret = secret.unwrap_or_default();
    salt_secret.extend_from_slice(secret.as_bytes());
    let (k_i2r, k_r2i) = derive_session_keys(&shared, &salt_secret);

    // 凭证证明（AEAD，主控逐个核对凭证）
    let proof = HandshakeMsg::PairProof;
    let sealed = aead_seal(&k_i2r, 0, DIR_I2R, &serde_json::to_vec(&proof).unwrap(), b"pair");
    write_frame(&mut stream, &sealed)
        .await
        .map_err(|e| format!("发送凭证证明失败: {e}"))?;

    // 等待 PairDone（主控持有正确凭证才能产生）
    let sealed2 = read_frame(&mut stream)
        .await
        .map_err(|e| format!("等待配对确认失败: {e}"))?;
    let plain2 = aead_open(&k_r2i, 0, DIR_R2I, &sealed2, b"pair")?;
    let done: HandshakeMsg = serde_json::from_slice(&plain2)
        .map_err(|e| format!("配对确认解析失败: {e}"))?;
    let (done_master_id, done_master_name) = match done {
        HandshakeMsg::PairDone { master_id, master_name } => (master_id, master_name),
        _ => return Err("主控配对确认类型异常".into()),
    };

    let node_key = derive_node_key(&k_i2r);
    let ip = addr.0.clone();
    state.complete_pairing(&node_key, &done_master_id, &done_master_name, &ip)?;
    state.take_pending_pin();
    state.clear_bootstrap();
    state.push_ui_log(format!(
        "配对成功：主控「{done_master_name}」({ip})，node_key 已加密保存"
    ));

    // 配对成功后立即做两件事（不阻塞会话）：启动 rpc-server + 防火墙收紧
    {
        let st = Arc::clone(state);
        tauri::async_runtime::spawn(async move {
            let port = st.rpc_port_setting();
            match st.rpc.ensure_running(port) {
                Ok(true) => st.push_ui_log(format!("rpc-server 已启动（端口 {port}）")),
                Ok(false) => {}
                Err(e) => st.push_ui_log(format!("rpc-server 启动失败：{e}")),
            }
            st.emit_state();
        });
    }
    {
        let st = Arc::clone(state);
        let master_ips = vec![ip.clone()];
        tauri::async_runtime::spawn(async move {
            let first = master_ips.first().cloned().unwrap_or_default();
            if crate::agent::firewall::rpc_rule_scoped_ok(&first) {
                st.push_ui_log("防火墙规则已就绪（50052 仅允许主控访问）".to_string());
                return;
            }
            match crate::agent::firewall::apply_scoped_rules(&master_ips) {
                Ok(()) => st.push_ui_log(format!(
                    "防火墙规则已更新：50052 仅允许主控（{}）访问",
                    master_ips.join(", ")
                )),
                Err(e) => st.push_ui_log(format!(
                    "防火墙规则设置失败：{e}（若安装时已放行可忽略；主控可能无法连接本机 RPC）"
                )),
            }
            st.emit_state();
        });
    }

    // 同一连接转为控制会话（seq 从 1 继续）
    serve_session(state, stream, Aead::new(k_i2r), Aead::new(k_r2i), 1, 1).await
}

/// 已配对：用 node_key 建立持久控制会话
async fn ctl_session(state: &Arc<AgentState>, addr: &(String, u16)) -> Result<(), String> {
    let node_key = state.node_key().ok_or("未配对")?;
    let expect_master = state.master_id().ok_or("未配对")?;
    let addr_str = format!("{}:{}", addr.0, addr.1);

    let mut stream = tokio::time::timeout(std::time::Duration::from_secs(5), TcpStream::connect(&addr_str))
        .await
        .map_err(|_| "连接主控超时".to_string())?
        .map_err(|e| format!("连接失败: {e}"))?;

    let (sk, pk) = random_ephemeral();
    let salt = random_bytes(16);
    write_json_frame(
        &mut stream,
        &HandshakeMsg::CtlHello {
            id: state.id.clone(),
            epk: hex::encode(pk.as_bytes()),
            salt: hex::encode(&salt),
        },
    )
    .await
    .map_err(|e| format!("发送 CtlHello 失败: {e}"))?;

    let ready = read_json_frame::<HandshakeMsg>(&mut stream)
        .await
        .map_err(|e| {
            eprintln!("[ctl-client] read CtlReady failed: {e}");
            format!("读取 CtlReady 失败: {e}")
        })?;
    let (epk, master_id) = match ready {
        HandshakeMsg::CtlReady { epk, master_id } => (epk, master_id),
        HandshakeMsg::CtlFail { reason } => {
            eprintln!("[ctl-client] master rejected session: {reason}");
            return Err(reason);
        }
        _ => {
            eprintln!("[ctl-client] unexpected handshake reply type");
            return Err("主控会话应答异常".into());
        }
    };
    if master_id != expect_master {
        eprintln!(
            "[ctl-client] master id mismatch: got {master_id}, expect {expect_master}"
        );
        return Err("主控身份不匹配".into());
    }

    let epk_bytes: [u8; 32] = hex::decode(&epk)
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or("主控临时公钥格式错误")?;
    // salt 即本端生成的原始字节（线上是它的 hex 编码），无需再解码
    let peer = PublicKey::from(epk_bytes);
    let shared = ecdh(&sk, &peer);
    let mut salt2 = salt;
    salt2.extend_from_slice(&node_key);
    let (k_i2r, k_r2i) = derive_session_keys(&shared, &salt2);

    // 首条指令兼做双向认证（两端都必须持有 node_key）
    state.push_ui_log(format!("已连接主控 {}:{}", addr.0, addr.1));
    serve_session(state, stream, Aead::new(k_i2r), Aead::new(k_r2i), 0, 0).await
}

/// 会话服务循环：收主控指令（R2I）→ dispatch → 应答（I2R）
async fn serve_session(
    state: &Arc<AgentState>,
    mut stream: TcpStream,
    send_aead: Aead,
    recv_aead: Aead,
    mut seq_tx: u64,
    mut seq_rx: u64,
) -> Result<(), String> {
    state.emit_state();
    loop {
        let frame = match read_frame(&mut stream).await {
            Ok(f) => f,
            Err(e) => {
                eprintln!("[ctl-client] read_frame error: {e}");
                return Err("连接已断开".into());
            }
        };
        let plain = recv_aead
            .open(seq_rx, DIR_R2I, &frame, b"ctl")
            .map_err(|e| format!("解密指令失败: {e}"))?;
        seq_rx += 1;
        let req: CtlRequest =
            serde_json::from_slice(&plain).map_err(|e| format!("指令解析失败: {e}"))?;
        let resp = dispatch(state, &req).await;
        let buf = serde_json::to_vec(&resp).map_err(|e| e.to_string())?;
        let sealed = send_aead.seal(seq_tx, DIR_I2R, &buf, b"ctl");
        seq_tx += 1;
        write_frame(&mut stream, &sealed)
            .await
            .map_err(|e| format!("发送应答失败: {e}"))?;
    }
}

async fn dispatch(state: &Arc<AgentState>, req: &CtlRequest) -> CtlResponse {
    let id = req.id.as_str();
    match req.method.as_str() {
        "ping" => CtlResponse::ok(id, serde_json::json!({
            "pong": true, "ts": chrono::Utc::now().timestamp()
        })),
        "whoami" => CtlResponse::ok(id, serde_json::json!({ "id": state.id })),
        "status" => {
            let (rpc_running, rpc_pid, rpc_uptime, rpc_port) = state.rpc.status();
            let free = state.refresh_vram_free();
            let st = crate::common::protocol::AgentStatus {
                rpc_running,
                rpc_pid,
                rpc_uptime_s: rpc_uptime,
                rpc_port,
                cache_bytes: state.cache_bytes(),
                gpu: state.gpu.name.clone(),
                vram_free_mb: free.or(state.gpu.vram_free_mb),
                vram_total_mb: state.gpu.vram_total_mb,
                version: crate::common::sysinfo::app_version(),
            };
            CtlResponse::ok(id, serde_json::to_value(st).unwrap_or_default())
        }
        "rpc.start" => {
            let port = req
                .params
                .get("port")
                .and_then(|v| v.as_u64())
                .map(|v| v as u16)
                .unwrap_or_else(|| state.rpc_port_setting());
            match state.rpc.ensure_running(port) {
                Ok(true) => CtlResponse::ok(id, serde_json::json!({"started": true, "port": port})),
                Ok(false) => CtlResponse::ok(id, serde_json::json!({"started": false, "port": port})),
                Err(e) => CtlResponse::err(id, 500, e),
            }
        }
        "rpc.stop" => match state.rpc.stop() {
            Ok(()) => CtlResponse::ok(id, serde_json::json!({"stopped": true})),
            Err(e) => CtlResponse::err(id, 500, e),
        },
        // 主控启动推理前调用：返回主控应拨号的本机地址与会话对端信息。
        // 主控优先使用真实连接 IP，避免心跳里多网卡 IP 顺序不稳定导致连错网卡。
        "rpc.targets" => {
            let free = state.refresh_vram_free();
            CtlResponse::ok(id, serde_json::json!({
                "rpc_port": state.rpc.port(),
                "ips": crate::common::sysinfo::local_ips(),
                "vram_free_mb": free.or(state.gpu.vram_free_mb),
            }))
        }
        "cache.info" => CtlResponse::ok(id, serde_json::json!({ "bytes": state.cache_bytes() })),
        "cache.clear" => match state.clear_cache() {
            Ok(msg) => CtlResponse::ok(id, serde_json::json!(msg)),
            Err(e) => CtlResponse::err(id, 500, e),
        },
        "firewall.setup" => {
            let master_ips: Vec<String> = req
                .params
                .get("master_ips")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let legacy: Option<String> = req
                .params
                .get("master_ip")
                .and_then(|v| v.as_str().map(String::from));
            let mut ips = master_ips;
            if ips.is_empty() {
                if let Some(ip) = legacy {
                    if !ip.is_empty() {
                        ips.push(ip);
                    }
                }
            }
            if ips.is_empty() {
                return CtlResponse::err(id, 400, "缺少 master_ips");
            }
            let st = Arc::clone(state);
            tauri::async_runtime::spawn(async move {
                match crate::agent::firewall::apply_scoped_rules(&ips) {
                    Ok(()) => st.push_ui_log("防火墙规则已更新（仅主控可访问 50052）".to_string()),
                    Err(e) => st.push_ui_log(format!("防火墙规则失败：{e}")),
                }
                st.emit_state();
            });
            CtlResponse::ok(id, serde_json::json!({"started": true}))
        }
        other => CtlResponse::err(id, 404, format!("未知指令: {other}")),
    }
}
