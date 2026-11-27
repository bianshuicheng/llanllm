//! 被控端防火墙：为 RPC 端口（50052）添加仅允许主控 IP 的入站规则。
//!
//! - 优先直接调用 netsh（安装器已提权场景 / 已有管理员权限时零弹窗）
//! - 不行再走 UAC 提权（PowerShell Start-Process RunAs）
//! - 规则覆盖**全部**网络类型（private/domain/public）：Windows 对家庭/办公
//!   网络的类型判定不可控，只写 private,domain 会让「公用网络」下的机器直接失联
//!   （v0.1.0 的实际故障之一）。rpc 端口本就无鉴权，来源白名单（仅主控 IP）
//!   才是边界，profile=any 不额外扩大暴露面。

use crate::common::elevate;
use std::path::Path;

pub const RULE_RPC: &str = "LLanLLM Agent RPC";

/// netsh 防火墙规则的 remoteip 地址语法上限：单条规则最多 500 个条目。
const MAX_REMOTE_IPS: usize = 500;

fn netsh(args: &str) -> Result<String, String> {
    let out = std::process::Command::new("netsh")
        .args(args.split_whitespace())
        .creation_flags_hide()
        .output()
        .map_err(|e| format!("无法运行 netsh: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !out.status.success() {
        return Err(format!(
            "netsh 失败（{}）：{}{}",
            elevate::explain_exit_code(out.status.code().unwrap_or(-1)),
            stdout,
            stderr
        ));
    }
    Ok(stdout)
}

/// 逐行执行 netsh 脚本（脚本行的格式与 .cmd 文件一致，每行以 netsh 开头）。
fn run_script(script: &str) -> Result<(), String> {
    for line in script.lines().filter(|l| !l.trim().is_empty()) {
        let trimmed = line.trim();
        let rest = trimmed.strip_prefix("netsh").unwrap_or(trimmed).trim();
        netsh(rest)?;
    }
    Ok(())
}

trait HideWin {
    fn creation_flags_hide(&mut self) -> &mut Self;
}

impl HideWin for std::process::Command {
    fn creation_flags_hide(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            self.creation_flags(CREATE_NO_WINDOW);
        }
        self
    }
}

/// 规则内容脚本（netsh 批量）。master_ips 为空 → 兜底放行本地子网。
fn rule_script(master_ips: &[String]) -> String {
    let remote = if master_ips.is_empty() {
        "localsubnet".to_string()
    } else {
        master_ips.join(",")
    };
    format!(
        r#"netsh advfirewall firewall delete rule name="{RULE_RPC}"
netsh advfirewall firewall add rule name="{RULE_RPC}" dir=in action=allow protocol=TCP localport=50052 remoteip={remote} profile=any
"#
    )
}

/// 配对成功/安装时调用：为 RPC 端口添加 remoteip=主控IP 的入站规则（全部网络类型）。
/// 已提权（安装器钩子）时直接成功；普通运行时触发 UAC。
pub fn apply_scoped_rules(master_ips: &[String]) -> Result<(), String> {
    let mut ips: Vec<String> = Vec::new();
    for ip in master_ips {
        let ip = ip.trim();
        if !ip.is_empty() {
            ips.push(ip.to_string());
        }
        if ips.len() >= MAX_REMOTE_IPS {
            break;
        }
    }
    if ips.is_empty() {
        ips.push("localsubnet".into());
    }
    let script = rule_script(&ips);
    match run_script(&script) {
        Ok(()) => Ok(()),
        Err(direct_err) => {
            // 直接 netsh 失败（无管理员权限）→ 提权重跑同样内容
            let path = crate::common::paths::data_dir().join("fw-agent-scoped.cmd");
            std::fs::write(&path, &script).map_err(|e| format!("写防火墙脚本失败: {e}"))?;
            elevate::run_elevated_cmd(&path, "添加防火墙规则").map_err(|e| {
                format!("{e}（直接执行失败原因：{direct_err}）")
            })?;
            Ok(())
        }
    }
}

/// 解除配对/卸载时清理规则。
pub fn remove_rules() -> Result<(), String> {
    let script = format!(
        "netsh advfirewall firewall delete rule name=\"{RULE_RPC}\"\n"
    );
    match run_script(&script) {
        Ok(()) => Ok(()),
        Err(_) => {
            let path = crate::common::paths::data_dir().join("fw-agent-remove.cmd");
            std::fs::write(&path, &script).map_err(|e| format!("写防火墙脚本失败: {e}"))?;
            elevate::run_elevated_cmd(&path, "清理防火墙规则")?;
            Ok(())
        }
    }
}

/// 判断「仅允许 master_ip 访问 50052」的规则是否真的在位。
///
/// 用 netsh show 解析（零依赖、不需要 PowerShell），要求：
/// 1. 规则存在且 Enabled=true
/// 2. Action=Allow 且 Profile 覆盖当前激活的网络类型
/// 3. RemoteIP 列表包含 master_ip（或兜底 localsubnet）
/// 与实现不符 → false，UI 据此提示需要修复。
pub fn rpc_rule_scoped_ok(master_ip: &str) -> bool {
    let show = match netsh(&format!(
        "advfirewall firewall show rule name=\"{RULE_RPC}\" verbose"
    )) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let text = show.to_lowercase();
    if !text.contains("enabled:") || !text.contains("true") {
        return false;
    }
    if !text.contains("action:") || !text.contains("allow") {
        return false;
    }
    if !text.contains("localport:") || !text.contains("50052") {
        return false;
    }
    // profile 覆盖当前网络类型
    let profile_ok = current_profile_allows(&text);
    if !profile_ok {
        return false;
    }
    let ip = master_ip.trim().to_lowercase();
    if ip.is_empty() {
        return true; // 无法判定来源时只要规则在位即认为可用
    }
    text.contains(&format!("remoteip:")) && (text.contains(&ip) || text.contains("localsubnet"))
}

/// 从 netsh show 输出判断规则的 profile 是否覆盖当前激活的网络类型。
fn current_profile_allows(rule_text: &str) -> bool {
    // 规则里的 profile 行（可能为 "Domain, Private" / "Any" 等）
    let profile_line = rule_text
        .lines()
        .find(|l| l.trim_start().starts_with("profile:"))
        .unwrap_or("")
        .to_lowercase();
    let covers = |kw: &str| profile_line.contains(kw) || profile_line.contains("any") || profile_line.is_empty();

    // 当前激活的网络类型
    let state = netsh("advfirewall show allprofiles state").unwrap_or_default().to_lowercase();
    if state.contains("public network state:") && state.contains("on") && !covers("public") {
        return false;
    }
    if state.contains("private network state:") && state.contains("on") && !covers("private") {
        return false;
    }
    if state.contains("domain network state:") && state.contains("on") && !covers("domain") {
        return false;
    }
    true
}

/// 探测本程序 exe 的安装路径规则（预留：安装器阶段可创建 program 规则）
pub fn rule_for_program_exists(_exe: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_script_contents() {
        let s = rule_script(&["10.0.0.227".to_string()]);
        assert!(s.contains("localport=50052"));
        assert!(s.contains("remoteip=10.0.0.227"));
        assert!(s.contains("profile=any"), "必须覆盖全部网络类型");
        assert!(!s.contains("private,domain"), "v0.1.0 的老写法必须消失");
        let empty = rule_script(&[]);
        assert!(empty.contains("remoteip=localsubnet"));
    }

    #[test]
    fn rule_script_caps_entries() {
        let many: Vec<String> = (0..1000).map(|i| format!("10.0.0.{i}")).collect();
        let s = rule_script(&many);
        assert!(!s.contains("10.0.0.999,"), "超过 netsh 500 上限的条目应被截断");
        assert!(s.contains("10.0.0.499"));
    }
}
