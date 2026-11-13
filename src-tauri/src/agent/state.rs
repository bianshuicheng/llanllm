//! 被控端运行时状态：身份、配对信息、RPC 看护、缓存。

use crate::agent::rpc_runner::RpcRunner;
use crate::common::db::Db;
use crate::common::paths;
use crate::common::sysinfo::{gpu_info, GpuInfo};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct AgentState {
    pub db: Db,
    pub id: String,
    pub gpu: GpuInfo,
    pub rpc: std::sync::Arc<RpcRunner>,
    node_key: Mutex<Option<[u8; 32]>>,
    master_id: Mutex<Option<String>>,
    master_name: Mutex<Option<String>>,
    master_ip: Mutex<Option<String>>,
    pending_pin: Mutex<Option<(String, Instant)>>,
    rpc_port_setting: AtomicU16,
    ui_logs: Mutex<Vec<String>>,
    app: Mutex<Option<tauri::AppHandle>>,
    /// 广播发现到的主控：master_id → (ip, ctl_port)
    discovered: Mutex<HashMap<String, (String, u16)>>,
    /// 安装包注入的主控地址提示（优先级最高）
    addr_hint: Mutex<Option<(String, u16)>>,
}

const PIN_TTL: Duration = Duration::from_secs(300);
const UI_LOG_CAP: usize = 200;

impl AgentState {
    pub fn load(db: Db) -> Result<Self, String> {
        let id = db.get_setting("agent_id").unwrap_or_else(|| {
            let id = uuid::Uuid::new_v4().to_string();
            let _ = db.set_setting("agent_id", &id);
            id
        });
        let node_key = load_node_key();
        let master_id = db.get_setting("master_id");
        let master_name = db.get_setting("master_name");
        let master_ip = db.get_setting("master_ip");
        let rpc_port = db
            .get_setting("rpc_port")
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(crate::common::protocol::RPC_PORT_DEFAULT);

        Ok(AgentState {
            db,
            id,
            gpu: gpu_info(),
            rpc: std::sync::Arc::new(RpcRunner::new()),
            node_key: Mutex::new(node_key),
            master_id: Mutex::new(master_id),
            master_name: Mutex::new(master_name),
            master_ip: Mutex::new(master_ip),
            pending_pin: Mutex::new(None),
            rpc_port_setting: AtomicU16::new(rpc_port),
            ui_logs: Mutex::new(Vec::new()),
            app: Mutex::new(None),
            discovered: Mutex::new(HashMap::new()),
            addr_hint: Mutex::new(None),
        })
    }

    pub fn set_app(&self, app: tauri::AppHandle) {
        *self.app.lock().unwrap() = Some(app);
        self.emit_state();
    }

    pub fn emit_state(&self) {
        if let Some(app) = self.app.lock().unwrap().as_ref() {
            use tauri::Emitter;
            let _ = app.emit("agent-state", self.status_view());
        }
    }

    pub fn push_ui_log(&self, line: String) {
        let mut logs = self.ui_logs.lock().unwrap();
        logs.push(line);
        if logs.len() > UI_LOG_CAP {
            let overflow = logs.len() - UI_LOG_CAP;
            logs.drain(..overflow);
        }
        drop(logs);
        self.emit_state();
    }

    // —— 配对 ——

    pub fn submit_pending_pin(&self, pin: &str) -> Result<(), String> {
        if pin.len() != 6 || !pin.bytes().all(|b| b.is_ascii_digit()) {
            return Err("PIN 必须是 6 位数字".into());
        }
        if self.is_paired() {
            return Err("本机已配对，请先在下方「解除配对」".into());
        }
        *self.pending_pin.lock().unwrap() =
            Some((pin.to_string(), Instant::now() + PIN_TTL));
        self.push_ui_log("已记录 PIN，等待主控发起配对…（5 分钟内有效）".into());
        Ok(())
    }

    pub fn take_pending_pin(&self) -> Option<String> {
        let mut p = self.pending_pin.lock().unwrap();
        match p.as_ref() {
            Some((pin, exp)) if *exp > Instant::now() => Some(pin.clone()),
            Some(_) => {
                *p = None;
                None
            }
            None => None,
        }
    }

    /// 查看（不消费）待配对 PIN
    pub fn peek_pending_pin(&self) -> Option<String> {
        let p = self.pending_pin.lock().unwrap();
        match p.as_ref() {
            Some((pin, exp)) if *exp > Instant::now() => Some(pin.clone()),
            _ => None,
        }
    }

    /// 主控存在应答到达：记录 master_id → (ip, ctl_port)
    pub fn note_presence(&self, master_id: &str, ip: &str, port: u16) {
        self.discovered
            .lock()
            .unwrap()
            .insert(master_id.to_string(), (ip.to_string(), port));
    }

    /// 安装包注入的主控地址（优先级最高）
    pub fn set_addr_hint(&self, ip: &str, port: u16) {
        *self.addr_hint.lock().unwrap() = Some((ip.to_string(), port));
    }

    /// 选出要连接的主控地址：已配对 → 匹配 master_id 的发现项或已存 IP；
    /// 未配对 → 注入提示优先，其次任意发现项。
    pub fn pick_master_addr(&self) -> Option<(String, u16)> {
        if let Some(hint) = self.addr_hint.lock().unwrap().clone() {
            return Some(hint);
        }
        let discovered = self.discovered.lock().unwrap();
        if let Some(mid) = self.master_id.lock().unwrap().clone() {
            if let Some(addr) = discovered.get(&mid) {
                return Some(addr.clone());
            }
            // 发现表里还没有，用配对时记录的 IP
            if let Some(ip) = self.master_ip.lock().unwrap().clone() {
                return Some((ip, crate::common::protocol::CONTROL_PORT));
            }
            return None;
        }
        discovered.values().next().cloned()
    }

    pub fn clear_pending_pin(&self) {
        *self.pending_pin.lock().unwrap() = None;
    }

    pub fn is_paired(&self) -> bool {
        self.node_key().is_some()
    }

    pub fn node_key(&self) -> Option<[u8; 32]> {
        *self.node_key.lock().unwrap()
    }

    pub fn master_id(&self) -> Option<String> {
        self.master_id.lock().unwrap().clone()
    }

    pub fn master_name(&self) -> Option<String> {
        self.master_name.lock().unwrap().clone()
    }

    pub fn master_ip(&self) -> Option<String> {
        self.master_ip.lock().unwrap().clone()
    }

    /// 配对成功：保存长期密钥（DPAPI）与主控信息
    pub fn complete_pairing(
        &self,
        node_key: &[u8; 32],
        master_id: &str,
        master_name: &str,
        master_ip: &str,
    ) -> Result<(), String> {
        save_node_key(node_key)?;
        *self.node_key.lock().unwrap() = Some(*node_key);
        *self.master_id.lock().unwrap() = Some(master_id.into());
        *self.master_name.lock().unwrap() = Some(master_name.into());
        *self.master_ip.lock().unwrap() = Some(master_ip.into());
        let _ = self.db.set_setting("master_id", master_id);
        let _ = self.db.set_setting("master_name", master_name);
        let _ = self.db.set_setting("master_ip", master_ip);
        self.emit_state();
        Ok(())
    }

    pub fn unpair(&self) -> Result<(), String> {
        // 停 rpc → 清规则 → 清密钥
        let _ = self.rpc.stop();
        let fw = crate::agent::firewall::remove_rules().err();
        let _ = std::fs::remove_file(node_key_path());
        *self.node_key.lock().unwrap() = None;
        *self.master_id.lock().unwrap() = None;
        *self.master_name.lock().unwrap() = None;
        *self.master_ip.lock().unwrap() = None;
        let _ = self.db.set_setting("master_id", "");
        let _ = self.db.set_setting("master_name", "");
        let _ = self.db.set_setting("master_ip", "");
        self.push_ui_log("已解除配对（密钥与主控信息已清除）".into());
        if let Some(e) = fw {
            return Err(e);
        }
        Ok(())
    }

    // —— RPC / 缓存 ——

    pub fn rpc_port_setting(&self) -> u16 {
        self.rpc_port_setting.load(Ordering::Relaxed)
    }

    pub fn set_rpc_port(&self, port: u16) {
        self.rpc_port_setting.store(port, Ordering::Relaxed);
        let _ = self.db.set_setting("rpc_port", &port.to_string());
    }

    pub fn cache_dir(&self) -> PathBuf {
        paths::cache_dir()
    }

    pub fn cache_bytes(&self) -> u64 {
        dir_size(&paths::cache_dir())
    }

    pub fn clear_cache(&self) -> Result<String, String> {
        let dir = paths::cache_dir();
        if !dir.exists() {
            return Ok("缓存目录不存在，无需清理".into());
        }
        let before = dir_size(&dir);
        std::fs::remove_dir_all(&dir)
            .map_err(|e| format!("清理失败：{e}（rpc-server 可能正在占用，请先停止）"))?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("重建缓存目录失败: {e}"))?;
        Ok(format!(
            "已清理本机缓存，释放 {}",
            human_bytes(before)
        ))
    }

    pub fn open_cache_dir(&self) -> Result<(), String> {
        let dir = paths::cache_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("创建缓存目录失败: {e}"))?;
        #[cfg(windows)]
        std::process::Command::new("explorer")
            .arg(&dir)
            .spawn()
            .map_err(|e| format!("打开目录失败: {e}"))?;
        Ok(())
    }

    /// 刷新显存空闲值（NVIDIA 机器上每心跳调用）
    pub fn refresh_vram_free(&self) -> Option<u64> {
        if self.gpu.name.as_deref().map(|n| n.contains("NVIDIA")).unwrap_or(false) {
            let out = std::process::Command::new("nvidia-smi")
                .args(["--query-gpu=memory.free", "--format=csv,noheader,nounits"])
                .creation_flags_hide()
                .output()
                .ok()?;
            let text = String::from_utf8_lossy(&out.stdout);
            let v = text.lines().next()?.trim().parse::<u64>().ok();
            if v.is_some() {
                // 更新缓存值（gpu 结构不可变，改用内部可变性意义不大；返回即可）
                return v;
            }
            None
        } else {
            self.gpu.vram_free_mb
        }
    }

    pub fn status_view(&self) -> serde_json::Value {
        let (rpc_running, rpc_pid, rpc_uptime, rpc_port) = self.rpc.status();
        serde_json::json!({
            "id": self.id,
            "hostname": crate::common::sysinfo::hostname(),
            "ips": crate::common::sysinfo::local_ips(),
            "gpu": self.gpu.name,
            "vram_total_mb": self.gpu.vram_total_mb,
            "vram_free_mb": self.gpu.vram_free_mb,
            "paired": self.is_paired(),
            "master_id": self.master_id(),
            "master_name": self.master_name(),
            "master_ip": self.master_ip(),
            "rpc_running": rpc_running,
            "rpc_pid": rpc_pid,
            "rpc_uptime_s": rpc_uptime,
            "rpc_port": rpc_port,
            "cache_bytes": self.cache_bytes(),
            "cache_dir": self.cache_dir().to_string_lossy(),
            "logs": self.ui_logs.lock().unwrap().clone(),
            "rpc_logs": self.rpc.tail_logs(50),
            "version": crate::common::sysinfo::app_version(),
            "firewall_ok": crate::agent::firewall::rules_present(),
        })
    }
}

pub fn human_bytes(n: u64) -> String {
    if n >= 1024 * 1024 * 1024 {
        format!("{:.2} GB", n as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    } else {
        format!("{n} B")
    }
}

pub fn dir_size(p: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            match e.file_type() {
                Ok(t) if t.is_dir() => total += dir_size(&e.path()),
                _ => total += e.metadata().map(|m| m.len()).unwrap_or(0),
            }
        }
    }
    total
}

fn node_key_path() -> PathBuf {
    paths::data_dir().join("node.key")
}

fn load_node_key() -> Option<[u8; 32]> {
    let p = node_key_path();
    let enc = std::fs::read(&p).ok()?;
    let plain = crate::common::crypto::dpapi_unprotect(&enc).ok()?;
    if plain.len() == 32 {
        let mut k = [0u8; 32];
        k.copy_from_slice(&plain);
        Some(k)
    } else {
        None
    }
}

fn save_node_key(key: &[u8; 32]) -> Result<(), String> {
    let enc = crate::common::crypto::dpapi_protect(key)?;
    std::fs::write(node_key_path(), enc).map_err(|e| format!("保存密钥失败: {e}"))
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
