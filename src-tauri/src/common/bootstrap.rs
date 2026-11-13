//! 引导配置注入格式（主控与被控共用）。
//!
//! 注入格式 v2（追加在安装器 exe 之后）：
//! ```text
//! [原始安装器字节][config JSON(utf8)][master_ip ascii][magic "LLANCFG2"]
//! [u32 LE ip 长度][u32 LE json 长度]
//! ```
//! - NSIS 安装器（perMachine 提权）读取自身尾部，识别 magic 后把
//!   config 写到安装目录 config.json、IP 写到 config-ip.txt，
//!   并据 IP 自动添加防火墙规则（仅允许主控访问 RPC 端口）。
//! - 被控端同时支持直接读取自身 exe 尾部（直接运行导出的 exe 的场景）。
//! - 若尾部无标记（原始未注入模板），安装后走普通 PIN 输入流程。

pub const CFG_MAGIC: &[u8; 8] = b"LLANCFG2";

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct BootstrapConfig {
    pub v: u32,
    pub master_id: String,
    pub master_name: String,
    /// 主控局域网 IP（被控主动连接用）
    pub master_ip: String,
    /// 一次性引导 PIN（配对完成后作废）
    pub pin: String,
    pub discovery_port: u16,
    pub ctl_port: u16,
    pub rpc_port: u16,
    /// 引导 PIN 过期时间（unix 秒）
    pub expires_at: i64,
}

/// 把配置追加到安装器字节尾部（v2：json + ip 两段）
pub fn build_tail(mut blob: Vec<u8>, cfg: &BootstrapConfig) -> Result<Vec<u8>, String> {
    let json = serde_json::to_vec(cfg).map_err(|e| e.to_string())?;
    let ip = cfg.master_ip.trim().as_bytes().to_vec();
    let json_len = json.len() as u32;
    let ip_len = ip.len() as u32;
    blob.extend_from_slice(&json);
    blob.extend_from_slice(&ip);
    blob.extend_from_slice(CFG_MAGIC);
    blob.extend_from_slice(&ip_len.to_le_bytes());
    blob.extend_from_slice(&json_len.to_le_bytes());
    Ok(blob)
}

fn read_lengths(blob: &[u8]) -> Option<(usize, usize, usize, usize)> {
    // 尾部布局：[json][ip][magic 8][ip_len 4][json_len 4]
    if blob.len() < 24 {
        return None;
    }
    let json_len_pos = blob.len() - 4;
    let ip_len_pos = json_len_pos - 4;
    let magic_pos = ip_len_pos - 8;
    if &blob[magic_pos..ip_len_pos] != CFG_MAGIC {
        return None;
    }
    let json_len = u32::from_le_bytes([
        blob[json_len_pos],
        blob[json_len_pos + 1],
        blob[json_len_pos + 2],
        blob[json_len_pos + 3],
    ]) as usize;
    let ip_len = u32::from_le_bytes([
        blob[ip_len_pos],
        blob[ip_len_pos + 1],
        blob[ip_len_pos + 2],
        blob[ip_len_pos + 3],
    ]) as usize;
    let ip_pos = magic_pos.checked_sub(ip_len)?;
    let json_pos = ip_pos.checked_sub(json_len)?;
    Some((json_pos, json_len, ip_pos, ip_len))
}

/// 从 exe 尾部解析注入的配置；无标记返回 None。
pub fn parse_tail(blob: &[u8]) -> Option<BootstrapConfig> {
    let (json_pos, json_len, _, _) = read_lengths(blob)?;
    serde_json::from_slice(&blob[json_pos..json_pos + json_len]).ok()
}

/// 从 exe 尾部解析注入的主控 IP（可能为空）。
pub fn parse_tail_ip(blob: &[u8]) -> Option<String> {
    let (_, _, ip_pos, ip_len) = read_lengths(blob)?;
    let s = String::from_utf8_lossy(&blob[ip_pos..ip_pos + ip_len]).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> BootstrapConfig {
        BootstrapConfig {
            v: 1,
            master_id: "m-1".into(),
            master_name: "CSB".into(),
            master_ip: "10.0.0.227".into(),
            pin: "123456".into(),
            discovery_port: 47890,
            ctl_port: 47891,
            rpc_port: 50052,
            expires_at: 1794500000,
        }
    }

    #[test]
    fn tail_roundtrip() {
        let mut blob = vec![0u8; 1000];
        blob.extend_from_slice(b"FAKE-EXE-PADDING");
        let cfg = sample();
        let blob = build_tail(blob, &cfg).unwrap();
        let parsed = parse_tail(&blob).expect("应能解析出配置");
        assert_eq!(parsed.pin, "123456");
        assert_eq!(parsed.master_id, "m-1");
        assert_eq!(parsed.ctl_port, 47891);
        assert_eq!(parse_tail_ip(&blob).as_deref(), Some("10.0.0.227"));
    }

    #[test]
    fn tail_empty_ip() {
        let mut cfg = sample();
        cfg.master_ip = String::new();
        let blob = build_tail(Vec::new(), &cfg).unwrap();
        assert_eq!(parse_tail_ip(&blob), None);
        assert_eq!(parse_tail(&blob).unwrap().pin, "123456");
    }

    #[test]
    fn tail_no_marker() {
        let blob = b"plain installer without injection".to_vec();
        assert!(parse_tail(&blob).is_none());
    }
}
