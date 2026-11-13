//! 主控运行时状态：节点表（内存+DB）、llama-server 进程、配对 PIN。

use crate::common::db::{Db, ModelRow, NodeRow};
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize)]
pub struct NodeInfo {
    pub id: String,
    pub hostname: String,
    pub ips: Vec<String>,
    pub gpu: Option<String>,
    pub vram_total_mb: Option<u64>,
    pub vram_free_mb: Option<u64>,
    pub version: String,
    pub rpc_port: u16,
    pub ctl_port: u16,
}

#[derive(Debug, Clone)]
pub struct LiveNode {
    pub info: NodeInfo,
    pub last_seen: Instant,
    pub first_seen: Instant,
    pub paired: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeView {
    pub id: String,
    pub hostname: String,
    pub ip: String,
    pub ips: Vec<String>,
    pub gpu: Option<String>,
    pub vram_total_mb: Option<u64>,
    pub vram_free_mb: Option<u64>,
    pub version: String,
    pub online: bool,
    pub paired: bool,
    pub ctl_port: u16,
    pub rpc_port: u16,
}

#[derive(Debug, Clone)]
pub struct PinEntry {
    pub pin: String,
    /// 说明（手动配对 / 导出引导）
    pub label: String,
    pub expires_at: Instant,
}

impl PinEntry {
    pub fn expired(&self) -> bool {
        self.expires_at <= Instant::now()
    }
}

/// llama-server 进程
pub struct LlamaProc {
    pub child: tokio::process::Child,
    pub _managed: crate::common::proc::ManagedChild,
    pub started_at: Instant,
    pub model_path: String,
    pub model_name: String,
    pub port: u16,
    pub ctx_size: u32,
    pub ngl: i32,
    pub rpc_targets: Vec<String>,
}

#[derive(Serialize, Clone)]
pub struct LlamaStatusView {
    pub running: bool,
    pub port: u16,
    pub model_name: Option<String>,
    pub model_path: Option<String>,
    pub uptime_s: u64,
    pub ctx_size: Option<u32>,
    pub ngl: Option<i32>,
    pub rpc_targets: Vec<String>,
    pub logs: Vec<String>,
    /// 前端聊天/健康探测地址
    pub base_url: String,
}

pub struct AppState {
    pub db: Db,
    pub master_id: String,
    pub master_name: String,
    pub nodes: Mutex<HashMap<String, LiveNode>>,
    /// 待验证的 PIN（被控主动连接提交证明）
    pub pending_pins: Mutex<Vec<PinEntry>>,
    /// 被控主动连入的持久控制会话：node_id → 句柄
    pub ctl_sessions: Mutex<HashMap<String, Arc<crate::master::ctl_server::CtlSession>>>,
    pub llama: Mutex<Option<LlamaProc>>,
    pub llama_logs: Mutex<Vec<String>>,
    pub models_dir_override: Mutex<Option<PathBuf>>,
    pub hf: Arc<crate::master::hf::HfState>,
}

impl AppState {
    pub fn new(db: Db) -> Self {
        let master_id = db
            .get_setting("master_id")
            .unwrap_or_else(|| {
                let id = uuid::Uuid::new_v4().to_string();
                let _ = db.set_setting("master_id", &id);
                id
            });
        let master_name = crate::common::sysinfo::hostname();
        let models_dir = db.get_setting("models_dir");
        AppState {
            db,
            master_id,
            master_name,
            nodes: Mutex::new(HashMap::new()),
            pending_pins: Mutex::new(Vec::new()),
            ctl_sessions: Mutex::new(HashMap::new()),
            llama: Mutex::new(None),
            llama_logs: Mutex::new(Vec::new()),
            models_dir_override: Mutex::new(models_dir.map(PathBuf::from)),
            hf: Arc::new(crate::master::hf::HfState::new()),
        }
    }

    pub fn models_dir(&self) -> PathBuf {
        if let Some(p) = self.models_dir_override.lock().unwrap().clone() {
            return p;
        }
        crate::common::paths::default_models_dir()
    }

    /// 心跳到达后把已配对节点的最新 IP/显存写回 DB（幂等）
    pub fn refresh_paired_node(&self, node_id: &str) {
        let nodes = self.nodes.lock().unwrap();
        if let Some(live) = nodes.get(node_id) {
            if let Some(mut row) = self.db.list_nodes().into_iter().find(|n| n.id == node_id) {
                let ip = live.info.ips.first().cloned().unwrap_or_default();
                if row.last_ip != ip
                    || row.gpu != live.info.gpu
                    || row.vram_free_mb != live.info.vram_free_mb
                {
                    row.last_ip = ip;
                    row.gpu = live.info.gpu.clone();
                    row.vram_total_mb = live.info.vram_total_mb;
                    row.vram_free_mb = live.info.vram_free_mb;
                    row.version = Some(live.info.version.clone());
                    let _ = self.db.upsert_node(&row);
                }
            }
        }
    }

    pub fn nodes_snapshot(&self) -> Vec<NodeView> {
        let nodes = self.nodes.lock().unwrap();
        let mut out: Vec<NodeView> = nodes
            .values()
            .map(|n| NodeView {
                id: n.info.id.clone(),
                hostname: n.info.hostname.clone(),
                ip: n.info.ips.first().cloned().unwrap_or_default(),
                ips: n.info.ips.clone(),
                gpu: n.info.gpu.clone(),
                vram_total_mb: n.info.vram_total_mb,
                vram_free_mb: n.info.vram_free_mb,
                version: n.info.version.clone(),
                online: n.last_seen.elapsed() < Duration::from_secs(7),
                paired: n.paired,
                ctl_port: n.info.ctl_port,
                rpc_port: n.info.rpc_port,
            })
            .collect();
        out.sort_by(|a, b| {
            b.paired
                .cmp(&a.paired)
                .then_with(|| a.hostname.cmp(&b.hostname))
        });
        out
    }

    pub fn llama_status(&self) -> LlamaStatusView {
        let guard = self.llama.lock().unwrap();
        let logs = self.llama_logs.lock().unwrap().clone();
        match guard.as_ref() {
            Some(p) => LlamaStatusView {
                running: true,
                port: p.port,
                model_name: Some(p.model_name.clone()),
                model_path: Some(p.model_path.clone()),
                uptime_s: p.started_at.elapsed().as_secs(),
                ctx_size: Some(p.ctx_size),
                ngl: Some(p.ngl),
                rpc_targets: p.rpc_targets.clone(),
                logs,
                base_url: format!("http://127.0.0.1:{}", p.port),
            },
            None => LlamaStatusView {
                running: false,
                port: crate::common::protocol::LLAMA_PORT_DEFAULT,
                model_name: None,
                model_path: None,
                uptime_s: 0,
                ctx_size: None,
                ngl: None,
                rpc_targets: vec![],
                logs,
                base_url: format!("http://127.0.0.1:{}", crate::common::protocol::LLAMA_PORT_DEFAULT),
            },
        }
    }

    /// 启动前校验模型行
    pub fn validate_model(&self, m: &ModelRow) -> String {
        let path = PathBuf::from(&m.path);
        match std::fs::metadata(&path) {
            Ok(md) => {
                if md.len() == m.size {
                    "ok".into()
                } else {
                    "size_mismatch".into()
                }
            }
            Err(_) => "missing".into(),
        }
    }

    pub fn node_row(&self, node_id: &str) -> Option<NodeRow> {
        self.db.list_nodes().into_iter().find(|n| n.id == node_id)
    }
}
