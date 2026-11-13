//! 被控端防火墙：配对完成时下发仅允许主控 IP 访问的规则（需管理员/UAC）。

use crate::common::{elevate, paths};
use std::path::Path;

pub const RULE_RPC: &str = "LLanLLM Agent RPC";
pub const RULE_CTL: &str = "LLanLLM Agent CTL";

/// 配对成功后调用：为 RPC 端口与控制端口添加 remoteip=主控IP 的入站规则。
pub fn apply_scoped_rules(master_ip: &str) -> Result<(), String> {
    let script = paths::data_dir().join("fw-agent-scoped.ps1");
    let content = format!(
        r#"$ErrorActionPreference = 'Stop'
netsh advfirewall firewall delete rule name="{RULE_RPC}" | Out-Null
netsh advfirewall firewall delete rule name="{RULE_CTL}" | Out-Null
netsh advfirewall firewall add rule name="{RULE_RPC}" dir=in action=allow protocol=TCP localport=50052 remoteip={master_ip} profile=private,domain | Out-Null
netsh advfirewall firewall add rule name="{RULE_CTL}" dir=in action=allow protocol=TCP localport=47891 remoteip={master_ip} profile=private,domain | Out-Null
Write-Output "OK"
"#
    );
    std::fs::write(&script, content).map_err(|e| format!("写防火墙脚本失败: {e}"))?;
    elevate::run_elevated_script(&script, "添加防火墙规则（仅允许主控访问）")?;
    Ok(())
}

/// 解除配对/卸载时清理规则。
pub fn remove_rules() -> Result<(), String> {
    let script = paths::data_dir().join("fw-agent-remove.ps1");
    let content = format!(
        r#"$ErrorActionPreference = 'Stop'
netsh advfirewall firewall delete rule name="{RULE_RPC}" | Out-Null
netsh advfirewall firewall delete rule name="{RULE_CTL}" | Out-Null
Write-Output "OK"
"#
    );
    std::fs::write(&script, content).map_err(|e| format!("写防火墙脚本失败: {e}"))?;
    elevate::run_elevated_script(&script, "清理防火墙规则")?;
    Ok(())
}

/// 检查规则是否已存在（非提权，netsh show 允许普通用户查询）
pub fn rules_present() -> bool {
    let out = elevate::run_ps(&format!(
        "netsh advfirewall firewall show rule name=\"{RULE_RPC}\" | Select-String 'Rule Name'"
    ));
    out.map(|s| !s.is_empty()).unwrap_or(false)
}

/// 安装期已放行（钩子写入）则跳过运行期提权
pub fn rpc_rule_scoped_ok(_master_ip: &str) -> bool {
    rules_present()
}

/// 探测本程序 exe 的安装路径规则（预留：安装器阶段可创建 program 规则）
pub fn rule_for_program_exists(_exe: &Path) -> bool {
    false
}
