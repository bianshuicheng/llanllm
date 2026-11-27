//! 引导配置注入格式（主控与被控共用）。
//!
//! 注入格式 v3（追加在安装器 exe 之后）：
//! ```text
//! [原始安装器字节][config JSON(utf8)][master_ip ascii]
//! [json_len: 6 位十进制 ASCII][ip_len: 4 位十进制 ASCII][magic "LLANCFG3"]
//! ```
//! 设计要点：
//! - 定长尾部 18 字节，magic 放在**最末**：NSIS 钩子从文件末尾倒读 18 字节即可
//!   同时完成识别与定位，不依赖任何偏移量推算（v2 的 magic 不在末尾，钩子的
//!   倒读偏移写错导致识别永远失败）。
//! - 长度用十进制 ASCII 而不是小端整数：避免在 NSIS 里逐字节拼装整数。
//! - NSIS 安装器（perMachine 提权）读取自身尾部，识别 magic 后把 config 写到
//!   安装目录 config.json，并据 master_ip 自动添加防火墙规则（仅允许主控访问 RPC 端口）。
//! - 被控端同时支持直接读取自身 exe 尾部（开发联调：直接运行导出的 exe）。
//! - 若尾部无标记（原始未注入模板），安装后走普通 PIN 输入流程。

use serde::{Deserialize, Serialize};

pub const CFG_MAGIC: &[u8; 8] = b"LLANCFG3";
/// 定长尾部：json_len(6) + ip_len(4) + magic(8)
pub const TAIL_FIXED: usize = 18;
const JSON_LEN_DIGITS: usize = 6;
const IP_LEN_DIGITS: usize = 4;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BootstrapConfig {
    /// 格式版本（3）
    pub v: u32,
    pub master_id: String,
    pub master_name: String,
    /// 主控局域网 IP：被控据此直接连接，安装器也据此写本机防火墙的来源白名单
    pub master_ip: String,
    /// 引导码指纹（sha256 前 16 位十六进制，非机密）：主控用它定位库里的记录
    pub token_id: String,
    /// 引导码本体（128 位随机数的十六进制，机密）
    pub token: String,
    pub discovery_port: u16,
    pub ctl_port: u16,
    pub rpc_port: u16,
    /// 引导码过期时间（unix 秒）；0 = 长期有效，直到在主控界面作废
    pub expires_at: i64,
}

impl BootstrapConfig {
    pub fn expired(&self, now: i64) -> bool {
        self.expires_at > 0 && self.expires_at <= now
    }
}

/// 把配置追加到安装器字节尾部。
pub fn build_tail(mut blob: Vec<u8>, cfg: &BootstrapConfig) -> Result<Vec<u8>, String> {
    let json = serde_json::to_vec(cfg).map_err(|e| e.to_string())?;
    let ip = cfg.master_ip.trim().as_bytes().to_vec();
    let json_max = 10usize.pow(JSON_LEN_DIGITS as u32) - 1;
    let ip_max = 10usize.pow(IP_LEN_DIGITS as u32) - 1;
    if json.len() > json_max || ip.len() > ip_max {
        return Err("引导配置过长，无法注入".into());
    }
    blob.extend_from_slice(&json);
    blob.extend_from_slice(&ip);
    blob.extend_from_slice(format!("{:0width$}", json.len(), width = JSON_LEN_DIGITS).as_bytes());
    blob.extend_from_slice(format!("{:0width$}", ip.len(), width = IP_LEN_DIGITS).as_bytes());
    blob.extend_from_slice(CFG_MAGIC);
    Ok(blob)
}

/// 返回 (json_pos, json_len, ip_pos, ip_len)
fn read_lengths(blob: &[u8]) -> Option<(usize, usize, usize, usize)> {
    // JSON 最短为 "{}"
    if blob.len() < TAIL_FIXED + 2 {
        return None;
    }
    let end = blob.len();
    if &blob[end - CFG_MAGIC.len()..end] != CFG_MAGIC {
        return None;
    }
    let digits = &blob[end - TAIL_FIXED..end - CFG_MAGIC.len()];
    if !digits.iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let json_len = ascii_dec(&digits[..JSON_LEN_DIGITS])?;
    let ip_len = ascii_dec(&digits[JSON_LEN_DIGITS..JSON_LEN_DIGITS + IP_LEN_DIGITS])?;
    if json_len < 2 {
        return None;
    }
    let ip_pos = end - TAIL_FIXED - ip_len;
    let json_pos = ip_pos.checked_sub(json_len)?;
    Some((json_pos, json_len, ip_pos, ip_len))
}

fn ascii_dec(b: &[u8]) -> Option<usize> {
    std::str::from_utf8(b).ok()?.parse::<usize>().ok()
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
            v: 3,
            master_id: "m-1".into(),
            master_name: "CSB".into(),
            master_ip: "10.0.0.227".into(),
            token_id: "0123456789abcdef".into(),
            token: "0123456789abcdef0123456789abcdef".into(),
            discovery_port: 47890,
            ctl_port: 47891,
            rpc_port: 50052,
            expires_at: 0,
        }
    }

    #[test]
    fn tail_roundtrip() {
        let mut blob = vec![0u8; 1000];
        blob.extend_from_slice(b"FAKE-EXE-PADDING");
        let cfg = sample();
        let blob = build_tail(blob, &cfg).unwrap();
        let parsed = parse_tail(&blob).expect("应能解析出配置");
        assert_eq!(parsed, cfg);
        assert_eq!(parse_tail_ip(&blob).as_deref(), Some("10.0.0.227"));
    }

    /// 钉死字节布局：NSIS 钩子按「末尾 18 字节 = json_len(6) ip_len(4) magic(8)」倒读，
    /// 这里的断言就是两端的契约。
    #[test]
    fn tail_layout_matches_nsis_contract() {
        let cfg = sample();
        let json = serde_json::to_vec(&cfg).unwrap();
        let blob = build_tail(Vec::new(), &cfg).unwrap();
        let n = blob.len();
        assert_eq!(&blob[n - 8..], b"LLANCFG3");
        let mut want_tail = format!("{:06}{:04}", json.len(), "10.0.0.227".len()).into_bytes();
        want_tail.extend_from_slice(b"LLANCFG3");
        assert_eq!(&blob[n - TAIL_FIXED..], &want_tail[..]);
        // magic 之前是明文 IP，再之前是原样 JSON
        assert_eq!(&blob[n - TAIL_FIXED - 10..n - TAIL_FIXED], b"10.0.0.227");
        assert_eq!(&blob[..json.len()], &json[..]);
        assert_eq!(n, json.len() + 10 + TAIL_FIXED);
    }

    #[test]
    fn tail_empty_ip() {
        let mut cfg = sample();
        cfg.master_ip = String::new();
        let blob = build_tail(Vec::new(), &cfg).unwrap();
        assert_eq!(parse_tail_ip(&blob), None);
        assert_eq!(parse_tail(&blob).unwrap().token, cfg.token);
    }

    #[test]
    fn tail_chinese_hostname_roundtrip() {
        let mut cfg = sample();
        cfg.master_name = "办公室-电脑-01".into();
        let blob = build_tail(vec![1u8; 7], &cfg).unwrap();
        assert_eq!(parse_tail(&blob).unwrap().master_name, "办公室-电脑-01");
    }

    #[test]
    fn tail_no_marker() {
        let blob = b"plain installer without injection".to_vec();
        assert!(parse_tail(&blob).is_none());
        // 旧版 v2 magic 不再被识别
        let mut old = vec![0u8; 40];
        old.extend_from_slice(b"LLANCFG2");
        assert!(parse_tail(&old).is_none());
    }

    #[test]
    fn tail_rejects_corruption() {
        let cfg = sample();
        let good = build_tail(vec![0u8; 16], &cfg).unwrap();
        // 截断
        assert!(parse_tail(&good[..good.len() - 1]).is_none());
        // 长度字段出现非数字
        let mut bad = good.clone();
        let n = bad.len();
        bad[n - TAIL_FIXED] = b'x';
        assert!(parse_tail(&bad).is_none());
        // 长度超出文件
        let mut bad = good.clone();
        for i in 0..6 {
            bad[n - TAIL_FIXED + i] = b'9';
        }
        assert!(parse_tail(&bad).is_none());
    }

    #[test]
    fn expiry_semantics() {
        let mut cfg = sample();
        assert!(!cfg.expired(1_000), "0 表示长期有效");
        cfg.expires_at = 500;
        assert!(!cfg.expired(499));
        assert!(cfg.expired(500));
    }
}
