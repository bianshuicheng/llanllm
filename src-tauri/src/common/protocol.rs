//! 发现心跳与控制通道的线上协议定义。
//!
//! - 心跳：UDP 广播 47890，JSON + HMAC(discovery_key)，防伪造。
//! - 控制通道：TCP 47891，长度前缀帧 + 握手（ECDH+PIN/长期密钥）→ AEAD 会话。
//! - 指令：JSON-RPC 风格 {id, method, params} / {id, result} / {id, error}。

use serde::{Deserialize, Serialize};

pub const DISCOVERY_PORT: u16 = 47890;
pub const CONTROL_PORT: u16 = 47891;
pub const RPC_PORT_DEFAULT: u16 = 50052;
pub const LLAMA_PORT_DEFAULT: u16 = 8080;
pub const PROTO_VERSION: u32 = 1;

/// 长度前缀帧的最大负载（1 MiB）
pub const FRAME_MAX: usize = 1024 * 1024;

/// —— UDP 发现心跳（不含签名的主体）——
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatBase {
    pub v: u32,
    #[serde(rename = "type")]
    pub kind: String, // "hb"
    /// 被控端身份（首次启动随机生成，持久化）
    pub id: String,
    pub hostname: String,
    pub ips: Vec<String>,
    pub gpu: Option<String>,
    pub vram_free_mb: Option<u64>,
    pub vram_total_mb: Option<u64>,
    pub version: String,
    /// 已配对的主控 id（未配对为 null）
    pub master_id: Option<String>,
    pub rpc_port: u16,
    pub ctl_port: u16,
    /// 自 1970 的秒数，主控用于防重放（容忍 ±60s）
    pub ts: i64,
}

/// 带签名的完整心跳包
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatSigned {
    #[serde(flatten)]
    pub base: HeartbeatBase,
    /// HMAC-SHA256(discovery_key, base 的紧凑 JSON) hex
    pub sig: String,
}

pub fn sign_heartbeat_base(base: &HeartbeatBase) -> String {
    let json = serde_json::to_vec(base).expect("心跳序列化");
    crate::common::crypto::hmac_sha256_hex(&crate::common::crypto::discovery_key(), &json)
}

pub fn verify_heartbeat(signed: &HeartbeatSigned) -> bool {
    let json = serde_json::to_vec(&signed.base).expect("心跳序列化");
    crate::common::crypto::hmac_sha256_verify(&crate::common::crypto::discovery_key(), &json, &signed.sig)
}

/// —— 主控存在应答（主控收到心跳后，单播回给被控） ——
/// 被控借此得知主控 IP/端口并主动发起控制连接（UDP 回包走有状态放行，被控零防火墙规则）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresenceBase {
    pub v: u32,
    #[serde(rename = "type")]
    pub kind: String, // "presence"
    pub master_id: String,
    pub master_name: String,
    pub ctl_port: u16,
    pub ts: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresenceSigned {
    #[serde(flatten)]
    pub base: PresenceBase,
    pub sig: String,
}

pub fn sign_presence_base(base: &PresenceBase) -> String {
    let json = serde_json::to_vec(base).expect("presence 序列化");
    crate::common::crypto::hmac_sha256_hex(&crate::common::crypto::discovery_key(), &json)
}

pub fn verify_presence(signed: &PresenceSigned) -> bool {
    let json = serde_json::to_vec(&signed.base).expect("presence 序列化");
    crate::common::crypto::hmac_sha256_verify(&crate::common::crypto::discovery_key(), &json, &signed.sig)
}

/// —— 控制通道握手帧（明文 JSON，敏感内容只用密文帧） ——
/// 方向：被控端为连接发起方（主动连接主控 47891），主控为应答方。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum HandshakeMsg {
    /// 被控→主控：发起配对（被控持有凭证：安装包注入的引导码或手动输入的 PIN）
    /// token_id：引导码指纹（非机密）；PIN 配对时为空串
    PairHello {
        id: String,
        epk: String,
        salt: String,
        hostname: String,
        gpu: Option<String>,
        vram_total_mb: Option<u64>,
        version: String,
        #[serde(default)]
        token_id: String,
    },
    /// 主控→被控：应答配对（主控有待验证的 PIN）
    PairAck {
        epk: String,
        master_name: String,
        master_id: String,
    },
    /// 被控→主控：PIN 证明（AEAD 帧，密钥派生自 PIN）
    PairProof,
    /// 主控→被控：配对完成（AEAD 帧）
    PairDone {
        master_id: String,
        master_name: String,
    },
    /// 明文失败应答
    PairFail { reason: String },
    /// 被控→主控：建立控制会话（已配对，用 node_key 认证）
    CtlHello { id: String, epk: String, salt: String },
    /// 主控→被控：应答控制会话
    CtlReady { epk: String, master_id: String },
    /// 明文失败应答（控制通道）
    CtlFail { reason: String },
}

/// —— 控制指令（AEAD 会话内） ——
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CtlRequest {
    pub id: String,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CtlResponse {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<CtlError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CtlError {
    pub code: i32,
    pub message: String,
}

impl CtlResponse {
    pub fn ok(id: &str, result: serde_json::Value) -> Self {
        CtlResponse { id: id.into(), result: Some(result), error: None }
    }
    pub fn err(id: &str, code: i32, message: impl Into<String>) -> Self {
        CtlResponse {
            id: id.into(),
            result: None,
            error: Some(CtlError { code, message: message.into() }),
        }
    }
}

/// 被控端状态上报（status 方法结果）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentStatus {
    pub rpc_running: bool,
    pub rpc_pid: Option<u32>,
    pub rpc_uptime_s: u64,
    pub rpc_port: u16,
    pub cache_bytes: u64,
    pub gpu: Option<String>,
    pub vram_free_mb: Option<u64>,
    pub vram_total_mb: Option<u64>,
    pub version: String,
}

/// —— TCP 帧编解码：[u32 BE 长度][负载] ——
pub async fn read_frame(
    reader: &mut (impl tokio::io::AsyncRead + Unpin),
) -> std::io::Result<Vec<u8>> {
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
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    Ok(buf)
}

pub async fn write_frame(
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    data: &[u8],
) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    writer
        .write_all(&(data.len() as u32).to_be_bytes())
        .await?;
    writer.write_all(data).await?;
    writer.flush().await
}

pub async fn read_json_frame<T: serde::de::DeserializeOwned>(
    reader: &mut (impl tokio::io::AsyncRead + Unpin),
) -> std::io::Result<T> {
    let buf = read_frame(reader).await?;
    serde_json::from_slice(&buf).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, format!("JSON 解析失败: {e}"))
    })
}

pub async fn write_json_frame<T: serde::Serialize>(
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    v: &T,
) -> std::io::Result<()> {
    let buf = serde_json::to_vec(v)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("序列化失败: {e}")))?;
    write_frame(writer, &buf).await
}
