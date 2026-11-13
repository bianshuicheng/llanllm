//! 本机信息：主机名、IP 列表、GPU 概要（nvidia-smi 优先，CIM 兜底）。

use serde::Serialize;

#[derive(Debug, Clone, Serialize, Default)]
pub struct GpuInfo {
    pub name: Option<String>,
    pub vram_total_mb: Option<u64>,
    pub vram_free_mb: Option<u64>,
}

pub fn hostname() -> String {
    hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown-host".into())
}

pub fn local_ips() -> Vec<String> {
    let mut ips: Vec<String> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| match i.addr.ip() {
            std::net::IpAddr::V4(v4) if !v4.is_loopback() => Some(v4.to_string()),
            _ => None,
        })
        .collect();
    ips.sort();
    ips.dedup();
    ips
}

pub fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// GPU 信息：优先 nvidia-smi（快），否则 PowerShell CIM（慢，数秒）。
pub fn gpu_info() -> GpuInfo {
    if let Some(g) = gpu_info_nvidia_smi() {
        return g;
    }
    gpu_info_cim().unwrap_or_default()
}

fn gpu_info_nvidia_smi() -> Option<GpuInfo> {
    let out = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.total,memory.free",
            "--format=csv,noheader,nounits",
        ])
        .creation_flags_hide_window()
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text.lines().next()?.trim().to_string();
    // "NVIDIA GeForce RTX 4070, 12288, 11000"
    let mut parts = first.split(',').map(|s| s.trim().to_string());
    let name = parts.next().filter(|s| !s.is_empty());
    let total = parts.next().and_then(|s| s.parse::<u64>().ok());
    let free = parts.next().and_then(|s| s.parse::<u64>().ok());
    Some(GpuInfo {
        name,
        vram_total_mb: total,
        vram_free_mb: free,
    })
}

/// 非 NVIDIA 机器兜底：PowerShell 查 Win32_VideoController（只取第一个有名字的）。
fn gpu_info_cim() -> Option<GpuInfo> {
    let ps = "Get-CimInstance Win32_VideoController | Where-Object { $_.Name -and $_.Name -notmatch 'Basic|Hyper-V' } | Select-Object -First 1 Name,@{n='MB';e={[math]::Round($_.AdapterRAM/1MB)}} | ConvertTo-Json -Compress";
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", ps])
        .creation_flags_hide_window()
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let name = v
        .get("Name")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    let mb = v.get("MB").and_then(|x| x.as_f64()).map(|f| f as u64);
    Some(GpuInfo {
        name,
        vram_total_mb: mb,
        vram_free_mb: None,
    })
}

/// 隐藏控制台窗口的小扩展（仅 Windows 生效）。
trait HideWindow {
    fn creation_flags_hide_window(&mut self) -> &mut Self;
}

impl HideWindow for std::process::Command {
    fn creation_flags_hide_window(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            self.creation_flags(CREATE_NO_WINDOW);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn basic_sysinfo() {
        assert!(!hostname().is_empty());
        println!("ips: {:?}", local_ips());
        println!("gpu: {:?}", gpu_info());
    }
}
