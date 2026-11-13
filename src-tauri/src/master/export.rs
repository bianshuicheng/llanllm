//! 导出被控端安装包：把引导配置（PIN 等）注入到被控端安装器模板 exe 的尾部。

use crate::common::bootstrap::{build_tail, BootstrapConfig};
use crate::common::protocol::{CONTROL_PORT, DISCOVERY_PORT, RPC_PORT_DEFAULT};
use crate::master::state::AppState;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// 定位被控端安装器模板。
/// 优先级：主控 exe 同目录 agent-installer.exe → runtime/agent-installer.exe
/// （打包时随 runtime 资源一起分发）→ 开发构建产物 llanllm-agent.exe。
pub fn locate_template() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("agent-installer.exe");
            if p.exists() {
                return Some(p);
            }
            let p2 = dir.join("runtime").join("agent-installer.exe");
            if p2.exists() {
                return Some(p2);
            }
            // 开发联调：同 target 目录里就有 agent 构建
            let dev = dir.join("llanllm-agent.exe");
            if dev.exists() {
                return Some(dev);
            }
        }
    }
    if let Some(manifest) = option_env!("CARGO_MANIFEST_DIR") {
        for profile in ["debug", "release"] {
            let p = Path::new(manifest)
                .join("target")
                .join(profile)
                .join("llanllm-agent.exe");
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

/// 生成引导 PIN（30 分钟有效）并返回配置。
pub fn create_bootstrap(state: &Arc<AppState>) -> Result<BootstrapConfig, String> {
    let pin = crate::common::crypto::gen_pin6();
    let expires_at = chrono::Utc::now().timestamp() + 1800; // 30 分钟
    crate::master::pairing::add_pending_pin(state, &pin, Duration::from_secs(1800), "导出引导");
    Ok(BootstrapConfig {
        v: 1,
        master_id: state.master_id.clone(),
        master_name: state.master_name.clone(),
        master_ip: crate::common::sysinfo::local_ips()
            .first()
            .cloned()
            .unwrap_or_default(),
        pin,
        discovery_port: DISCOVERY_PORT,
        ctl_port: CONTROL_PORT,
        rpc_port: RPC_PORT_DEFAULT,
        expires_at,
    })
}

/// 把配置注入模板并写出单 exe。
pub fn inject_and_write(template: &Path, cfg: &BootstrapConfig, out: &Path) -> Result<u64, String> {
    let blob = std::fs::read(template)
        .map_err(|e| format!("读取模板安装器失败（{}）：{e}", template.display()))?;
    let blob = build_tail(blob, cfg)?;
    if out.exists() {
        std::fs::remove_file(out).map_err(|e| format!("目标文件被占用或无法覆盖：{e}"))?;
    }
    std::fs::write(out, &blob).map_err(|e| format!("写出安装包失败：{e}"))?;
    Ok(blob.len() as u64)
}

pub use crate::common::bootstrap::parse_tail;
