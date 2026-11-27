//! 本机信息：主机名、IP 列表、GPU 概要（nvidia-smi 优先，CIM 兜底）、运行时自检。

use serde::Serialize;
use std::net::Ipv4Addr;

#[derive(Debug, Clone, Serialize, Default)]
pub struct GpuInfo {
    pub name: Option<String>,
    pub vram_total_mb: Option<u64>,
    pub vram_free_mb: Option<u64>,
}

/// llama.cpp 运行时自检结果
#[derive(Debug, Clone, Serialize)]
pub struct RuntimeCheck {
    pub ok: bool,
    pub message: String,
    pub llama_server: bool,
    pub rpc_server: bool,
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

/// 推断“主用网卡”的 IP：与默认路由同网段的私网地址。
///
/// if-addrs 没有默认路由信息，用启发式排序：
/// 192.168/10.172/172.16 之类常见家用/办公网段优先，虚拟网段
/// （192.168.56.x VirtualBox / 172.17+ Docker / 26.x WSL）靠后。
/// 广播心跳按这里的每个网段各自发一份，主用 IP 用于「导出被控端」注入。
pub fn primary_ip() -> Option<String> {
    let ips = local_ips();
    ips.iter()
        .map(|s| (s, ip_score(s)))
        .max_by_key(|(_, sc)| *sc)
        .filter(|(_, sc)| *sc >= 0)
        .map(|(s, _)| s.clone())
}

fn ip_score(s: &str) -> i32 {
    let v4: Ipv4Addr = match s.parse() {
        Ok(v) => v,
        Err(_) => return -100,
    };
    let o = v4.octets();
    match (o[0], o[1]) {
        (10, 0..=1) => 90,                    // 10.0/10.1 常见主网
        (192, 168) if o[2] != 56 => 90,       // 192.168.x（排除 VirtualBox 56）
        (172, 16..=31) if !(17..=31).contains(&o[1]) || o[1] == 16 => 85, // 172.16 主网（排除 Docker）
        (192, 168) => 40,                     // VirtualBox 等虚拟网段
        (172, 17..=31) => 40,                 // Docker/WSL 网段
        (169, 254) => 10,                     // APIPA（几乎一定是断网）
        _ => 30,
    }
}

/// 心跳应使用的广播地址列表：本机每个私网 IPv4 网段的定向广播 + 全局广播。
/// 多网卡机器（虚拟机/VPN/WSL）上 255.255.255.255 常走默认网卡，
/// 定向广播确保每个真实局域网网段都能收到心跳。
pub fn broadcast_targets() -> Vec<std::net::SocketAddr> {
    let mut out = vec![(
        std::net::IpAddr::V4(Ipv4Addr::BROADCAST),
        crate::common::protocol::DISCOVERY_PORT,
    )];
    for iface in if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(v4) => Some(v4),
            _ => None,
        })
        .filter(|v4| !v4.ip.is_loopback())
    {
        out.push((std::net::IpAddr::V4(iface.broadcast), crate::common::protocol::DISCOVERY_PORT));
    }
    out.sort();
    out.dedup();
    out.into_iter()
        .map(|(ip, port)| std::net::SocketAddr::new(ip, port))
        .collect()
}

pub fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// llama.cpp 运行时自检：检查 llama-server / ggml-rpc-server 是否在位，
/// 并用 `llama-server --version`（退出码 0，不需要 VC 弹窗）验证可执行。
/// 干净机器缺 VC++ 运行库时 Windows 加载器启动失败，会在这里被明确指出。
pub fn runtime_selfcheck() -> RuntimeCheck {
    let dir = crate::common::paths::runtime_dir();
    let (ls_ok, ls_err) = match &dir {
        Some(d) => {
            let exe = d.join("llama-server.exe");
            if !exe.exists() {
                (false, Some(format!("缺少 llama-server.exe（{}）", d.display())))
            } else {
                match std::process::Command::new(&exe)
                    .arg("--version")
                    .creation_flags_hide_window()
                    .output()
                {
                    Ok(out) if out.status.success() => (true, None),
                    Ok(out) => (
                        false,
                        Some(format!(
                            "llama-server 启动失败（{}）。通常缺 VC++ 运行库，可安装 \
                             Microsoft Visual C++ 2015-2022 Redistributable 后重试。",
                            out.status.code().unwrap_or(-1)
                        )),
                    ),
                    Err(e) => (false, Some(format!("llama-server 无法启动：{e}"))),
                }
            }
        }
        None => (false, Some("未找到 llama.cpp 运行时目录（runtime/）".into())),
    };
    let rpc_ok = dir
        .map(|d| d.join("ggml-rpc-server.exe").exists())
        .unwrap_or(false);

    let ok = ls_ok && rpc_ok;
    let message = if ok {
        "运行时就绪".into()
    } else {
        ls_err.unwrap_or_else(|| "缺少 ggml-rpc-server.exe".into())
    };
    RuntimeCheck { ok, message, llama_server: ls_ok, rpc_server: rpc_ok }
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
        println!("primary: {:?}", primary_ip());
        println!("broadcasts: {:?}", broadcast_targets());
        println!("gpu: {:?}", gpu_info());
    }

    #[test]
    fn ip_scoring_prefers_lan_over_virtual() {
        assert!(ip_score("192.168.1.5") > ip_score("192.168.56.1"));
        assert!(ip_score("10.0.0.227") > ip_score("172.17.0.1"));
        assert!(ip_score("169.254.10.2") < 0 || ip_score("169.254.10.2") <= 10);
    }
}
