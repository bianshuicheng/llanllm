//! 主控端入口：Tauri 应用、命令注册、后台任务。

pub mod ctl_server;
pub mod discovery;
pub mod export;
pub mod hf;
pub mod llama;
pub mod models;
pub mod pairing;
pub mod state;

use crate::common::db::Db;
use crate::common::paths;
use crate::common::protocol::CONTROL_PORT;
use state::{AppState, NodeView};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;

pub fn run() {
    paths::ensure_dirs().expect("初始化数据目录失败");

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let db_path = paths::data_dir().join("master.db");
            let db = Db::open(&db_path)?;
            let state = Arc::new(AppState::new(db));

            // 启动时把 DB 里的已配对节点读进内存（标记离线，等心跳）
            {
                let mut nodes = state.nodes.lock().unwrap();
                for row in state.db.list_nodes() {
                    let now = std::time::Instant::now() - std::time::Duration::from_secs(9999);
                    nodes.insert(
                        row.id.clone(),
                        state::LiveNode {
                            info: state::NodeInfo {
                                id: row.id,
                                hostname: row.hostname,
                                ips: vec![row.last_ip.clone()],
                                gpu: row.gpu.clone(),
                                vram_total_mb: row.vram_total_mb,
                                vram_free_mb: row.vram_free_mb,
                                version: row.version.unwrap_or_default(),
                                rpc_port: crate::common::protocol::RPC_PORT_DEFAULT,
                                ctl_port: CONTROL_PORT,
                            },
                            last_seen: now,
                            first_seen: now,
                            paired: true,
                        },
                    );
                }
            }

            app.manage(state.clone());

            // 后台任务
            tauri::async_runtime::spawn(discovery::run_discovery(
                handle.clone(),
                state.clone(),
            ));
            // 控制服务器：接受被控端主动连接（配对 + 持久会话）
            tauri::async_runtime::spawn(ctl_server::run_ctl_server(
                handle.clone(),
                state.clone(),
            ));
            // 过期 PIN 清理
            tauri::async_runtime::spawn(pairing::run_pin_sweeper(
                handle.clone(),
                state.clone(),
            ));
            // 自动放行主控端口（UDP 47890 发现 + TCP 47891 控制），规则缺失才提权
            tauri::async_runtime::spawn(async move {
                auto_open_firewall(handle, state).await;
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_role,
            get_master_info,
            get_nodes,
            create_pair_pin,
            cancel_pair_pin,
            remove_node,
            import_model_dialog,
            import_model,
            list_models,
            relocate_model_dialog,
            relocate_model,
            remove_model,
            get_models_dir,
            set_models_dir,
            open_dir,
            llama_start,
            llama_stop,
            llama_status,
            node_request,
            clear_node_cache,
            clear_master_cache,
            master_cache_info,
            allow_discovery_firewall,
            export_agent_installer,
            list_bootstrap_tokens,
            revoke_bootstrap_token,
            hf_list_files,
            hf_download,
            hf_cancel,
            hf_status,
            get_hf_mirror,
            set_hf_mirror,
        ])
        .run(tauri::generate_context!())
        .expect("主控端启动失败");
}

// ———————— 命令 ————————

#[tauri::command]
pub fn get_role() -> String {
    "master".into()
}

#[derive(serde::Serialize)]
pub struct MasterInfo {
    pub master_id: String,
    pub master_name: String,
    pub models_dir: String,
    pub cache_dir: String,
    pub cache_bytes: u64,
    pub version: String,
    /// llama.cpp 运行时自检（缺 VC++ 运行库等问题的显式提示）
    pub runtime_ok: bool,
    pub runtime_message: String,
    /// 本机各网卡 IP（导出被控端时注入的是主用 IP）
    pub local_ips: Vec<String>,
}

#[tauri::command]
pub fn get_master_info(state: State<'_, Arc<AppState>>) -> MasterInfo {
    let cache_dir = paths::cache_dir();
    let check = crate::common::sysinfo::runtime_selfcheck();
    MasterInfo {
        master_id: state.master_id.clone(),
        master_name: state.master_name.clone(),
        models_dir: state.models_dir().to_string_lossy().into_owned(),
        cache_dir: cache_dir.to_string_lossy().into_owned(),
        cache_bytes: models::dir_size(&cache_dir),
        version: crate::common::sysinfo::app_version(),
        runtime_ok: check.ok,
        runtime_message: check.message,
        local_ips: crate::common::sysinfo::local_ips(),
    }
}

#[tauri::command]
pub fn get_nodes(state: State<'_, Arc<AppState>>) -> Vec<NodeView> {
    state.nodes_snapshot()
}

#[derive(serde::Serialize)]
pub struct PinInfo {
    pub pin: String,
    pub expires_in_s: u64,
}

/// 生成配对 PIN：被控端在发现主控后主动连接并提交 PIN 证明
#[tauri::command]
pub fn create_pair_pin(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    node_id: String,
) -> Result<PinInfo, String> {
    let _ = node_id; // 新架构下无需目标地址：被控通过广播应答发现主控后主动连入
    let pin = crate::common::crypto::gen_pin6();
    pairing::add_pending_pin(&state, &pin, std::time::Duration::from_secs(300), "手动配对");
    app.emit("pair-progress", serde_json::json!({
        "state": "waiting", "message": "请在被控端输入 PIN，被控端将主动连接完成配对…"
    })).ok();
    Ok(PinInfo { pin, expires_in_s: 300 })
}

#[tauri::command]
pub fn cancel_pair_pin(state: State<'_, Arc<AppState>>) {
    state.pending_pins.lock().unwrap().clear();
}

#[tauri::command]
pub fn remove_node(
    state: State<'_, Arc<AppState>>,
    node_id: String,
) -> Result<String, String> {
    state.db.delete_node(&node_id)?;
    state.nodes.lock().unwrap().remove(&node_id);
    // 断开该节点的控制会话（被控发现连接失效后会自动重试，直到解除配对）
    state.ctl_sessions.lock().unwrap().remove(&node_id);
    Ok("已删除配对记录（被控端不会自动解除，如需彻底解绑请在被控端点「解除配对」）".into())
}

// ———————— 模型 ————————

#[tauri::command]
pub fn import_model_dialog(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<crate::common::db::ModelRow, String> {
    let picked = app
        .dialog()
        .file()
        .add_filter("GGUF 模型", &["gguf"])
        .add_filter("所有文件", &["*"])
        .blocking_pick_file();
    let path = match picked {
        Some(f) => file_path_to_string(f),
        None => return Err("已取消".into()),
    };
    models::import_model(&state, &path)
}

#[tauri::command]
pub fn import_model(
    state: State<'_, Arc<AppState>>,
    path: String,
) -> Result<crate::common::db::ModelRow, String> {
    models::import_model(&state, &path)
}

#[tauri::command]
pub fn list_models(state: State<'_, Arc<AppState>>) -> Vec<crate::common::db::ModelRow> {
    models::list_models(&state)
}

fn file_path_to_string(f: tauri_plugin_dialog::FilePath) -> String {
    match f {
        tauri_plugin_dialog::FilePath::Url(u) => u.to_string(),
        tauri_plugin_dialog::FilePath::Path(p) => p.to_string_lossy().into_owned(),
    }
}

#[tauri::command]
pub fn relocate_model_dialog(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<crate::common::db::ModelRow, String> {
    let _row = state.db.get_model(&id).ok_or("条目不存在")?;
    let picked = app
        .dialog()
        .file()
        .add_filter("GGUF 模型", &["gguf"])
        .add_filter("所有文件", &["*"])
        .blocking_pick_file();
    let path = match picked {
        Some(f) => file_path_to_string(f),
        None => return Err("已取消".into()),
    };
    models::relocate_model(&state, &id, &path)
}

#[tauri::command]
pub fn relocate_model(
    state: State<'_, Arc<AppState>>,
    id: String,
    new_path: String,
) -> Result<crate::common::db::ModelRow, String> {
    models::relocate_model(&state, &id, &new_path)
}

#[tauri::command]
pub fn remove_model(
    state: State<'_, Arc<AppState>>,
    id: String,
    delete_file: bool,
) -> Result<String, String> {
    models::remove_model(&state, &id, delete_file)?;
    Ok(if delete_file {
        "已删除记录与文件".into()
    } else {
        "已删除记录（文件保留在原位置）".into()
    })
}

#[tauri::command]
pub fn get_models_dir(state: State<'_, Arc<AppState>>) -> String {
    state.models_dir().to_string_lossy().into_owned()
}

#[tauri::command]
pub fn set_models_dir(
    state: State<'_, Arc<AppState>>,
    new_dir: String,
    move_files: bool,
) -> Result<serde_json::Value, String> {
    let moved = models::change_models_dir(&state, &new_dir, move_files)?;
    Ok(serde_json::json!({
        "moved": moved,
        "dir": new_dir,
        "message": if move_files {
            format!("模型目录已更改，迁移了 {moved} 个文件")
        } else {
            "模型目录已更改（未迁移文件）".to_string()
        }
    }))
}

#[tauri::command]
pub fn open_dir(path: String) -> Result<(), String> {
    let p = PathBuf::from(&path);
    if !p.exists() {
        std::fs::create_dir_all(&p).map_err(|e| format!("目录不存在且无法创建：{e}"))?;
    }
    #[cfg(windows)]
    {
        std::process::Command::new("explorer")
            .arg(&p)
            .spawn()
            .map_err(|e| format!("打开目录失败: {e}"))?;
    }
    #[cfg(not(windows))]
    {
        let _ = p;
        return Err("当前平台未实现".into());
    }
    Ok(())
}

// ———————— 推理服务 ————————

/// 收集 RPC 拨号目标：对每个在线已配对节点发起一次 rpc.targets 询问，
/// 优先用控制会话的真实对端 IP（多网卡机器上比心跳 IP 列表可靠），
/// 问不通（节点恰好断线等）回退到会话对端 IP。同时刷新节点的显存占用。
async fn collect_rpc_targets(state: &Arc<AppState>) -> Vec<String> {
    let sessions: Vec<(String, Arc<crate::master::ctl_server::CtlSession>)> = {
        let nodes = state.nodes.lock().unwrap();
        let sessions = state.ctl_sessions.lock().unwrap();
        nodes
            .values()
            .filter(|n| n.paired && n.last_seen.elapsed() < std::time::Duration::from_secs(7))
            .filter_map(|n| {
                sessions
                    .get(&n.info.id)
                    .map(|s| (n.info.id.clone(), Arc::clone(s)))
            })
            .collect()
    };
    let mut out = Vec::new();
    for (node_id, session) in sessions {
        let fallback = format!(
            "{}:{}",
            session.peer_ip,
            crate::common::protocol::RPC_PORT_DEFAULT
        );
        let target = match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            session.request("rpc.targets", serde_json::json!({})),
        )
        .await
        {
            Ok(Ok(v)) => {
                if let Some(free) = v.get("vram_free_mb").and_then(|x| x.as_u64()) {
                    let mut nodes = state.nodes.lock().unwrap();
                    if let Some(live) = nodes.get_mut(&node_id) {
                        live.info.vram_free_mb = Some(free);
                    }
                }
                let ip = v
                    .get("ips")
                    .and_then(|x| x.as_array())
                    .and_then(|a| a.first())
                    .and_then(|x| x.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| session.peer_ip.clone());
                let port = v
                    .get("rpc_port")
                    .and_then(|x| x.as_u64())
                    .map(|x| x as u16)
                    .unwrap_or(crate::common::protocol::RPC_PORT_DEFAULT);
                format!("{ip}:{port}")
            }
            _ => fallback,
        };
        if !target.starts_with(':') {
            out.push(target);
        }
    }
    out
}

#[tauri::command]
pub async fn llama_start(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    model_id: String,
    ctx_size: u32,
    ngl: i32,
    port: u16,
) -> Result<state::LlamaStatusView, String> {
    // 运行时自检先行：干净机器缺 VC++ 运行库时给出明确中文原因，而不是黑盒失败
    let check = crate::common::sysinfo::runtime_selfcheck();
    if !check.ok {
        return Err(format!("llama.cpp 运行时不可用：{}", check.message));
    }

    let model = state.db.get_model(&model_id).ok_or("模型条目不存在")?;
    let status = state.validate_model(&model);
    if status != "ok" {
        return Err(match status.as_str() {
            "missing" => format!(
                "模型文件不可用：{}（文件被移动或删除）。请到「模型管理」点击「重新定位」。",
                model.path
            ),
            "size_mismatch" => format!(
                "模型文件内容已变化：{}（大小与导入时不一致）。请重新定位或移除。",
                model.path
            ),
            _ => format!("模型状态异常：{status}"),
        });
    }

    // 自动收集所有在线已配对节点（真实连接 IP 优先）
    let targets: Vec<String> = collect_rpc_targets(state.inner()).await;

    llama::start_llama(
        app,
        state.inner().clone(),
        model.path.clone(),
        model.name.clone(),
        ctx_size,
        ngl,
        port,
        targets,
    )?;
    Ok(state.llama_status())
}

#[tauri::command]
pub fn llama_stop(state: State<'_, Arc<AppState>>) -> Result<state::LlamaStatusView, String> {
    llama::stop_llama(&state)?;
    Ok(state.llama_status())
}

#[tauri::command]
pub fn llama_status(state: State<'_, Arc<AppState>>) -> state::LlamaStatusView {
    state.llama_status()
}

// ———————— 节点控制（缓存等，经持久会话下发） ————————

#[tauri::command]
pub async fn node_request(
    state: State<'_, Arc<AppState>>,
    node_id: String,
    method: String,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let session = state
        .ctl_sessions
        .lock()
        .unwrap()
        .get(&node_id)
        .map(Arc::clone)
        .ok_or_else(|| "节点未连接（等待被控端主动连接主控）".to_string())?;
    session.request(&method, params).await
}

#[tauri::command]
pub async fn clear_node_cache(
    state: State<'_, Arc<AppState>>,
    node_id: String,
) -> Result<String, String> {
    let session = state
        .ctl_sessions
        .lock()
        .unwrap()
        .get(&node_id)
        .map(Arc::clone)
        .ok_or_else(|| "节点未连接（等待被控端主动连接主控）".to_string())?;
    let v = session
        .request("cache.clear", serde_json::json!({}))
        .await
        .map_err(|e| format!("清理被控缓存失败：{e}"))?;
    Ok(v.as_str().unwrap_or("已完成").to_string())
}

#[tauri::command]
pub fn clear_master_cache(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    models::clear_master_cache(&state)
}

#[derive(serde::Serialize)]
pub struct CacheInfo {
    pub bytes: u64,
    pub path: String,
    pub human: String,
}

#[tauri::command]
pub fn master_cache_info(_state: State<'_, Arc<AppState>>) -> CacheInfo {
    let dir = paths::cache_dir();
    let bytes = models::dir_size(&dir);
    CacheInfo {
        bytes,
        path: dir.to_string_lossy().into_owned(),
        human: llama::format_bytes(bytes),
    }
}

// ———————— 防火墙（主控自身，自动放行） ————————

const FW_RULE_DISCOVERY: &str = "LLanLLM Master Discovery";
const FW_RULE_CTL: &str = "LLanLLM Master CTL";

fn master_firewall_rules_present() -> bool {
    let out = crate::common::elevate::run_ps(
        "netsh advfirewall firewall show rule name=\"LLanLLM Master Discovery\"; netsh advfirewall firewall show rule name=\"LLanLLM Master CTL\"",
    );
    out.map(|s| {
        s.contains(FW_RULE_DISCOVERY) && s.contains(FW_RULE_CTL)
    })
    .unwrap_or(false)
}

/// 主控防火墙脚本：UDP 47890（心跳入站）+ TCP 47891（被控控制连接入站）。
/// profile=any：Windows 把家庭/办公网判成「公用」时（v0.1.0 的实际故障）规则也生效；
/// 入站来源用 localsubnet 限制到本网段，不因 profile=any 扩大暴露面。
fn master_firewall_script() -> String {
    format!(
        r#"netsh advfirewall firewall delete rule name="{FW_RULE_DISCOVERY}"
netsh advfirewall firewall delete rule name="{FW_RULE_CTL}"
netsh advfirewall firewall add rule name="{FW_RULE_DISCOVERY}" dir=in action=allow protocol=UDP localport=47890 remoteip=localsubnet profile=any
netsh advfirewall firewall add rule name="{FW_RULE_CTL}" dir=in action=allow protocol=TCP localport=47891 remoteip=localsubnet profile=any
"#
    )
}

/// 启动时自动放行主控端口（UDP 47890 心跳入站 + TCP 47891 被控控制连接入站）。
/// 规则已存在则跳过（不重复弹 UAC）。
async fn auto_open_firewall(app: AppHandle, _state: Arc<AppState>) {
    if master_firewall_rules_present() {
        app.emit("firewall-status", serde_json::json!({
            "ok": true, "message": "防火墙规则已就绪（UDP 47890 / TCP 47891）"
        })).ok();
        return;
    }
    let script = paths::data_dir().join("fw-master-auto.cmd");
    let content = master_firewall_script();
    if std::fs::write(&script, content).is_err() {
        app.emit("firewall-status", serde_json::json!({
            "ok": false, "message": "无法写入防火墙脚本（数据目录不可写）"
        })).ok();
        return;
    }
    app.emit("firewall-status", serde_json::json!({
        "ok": false,
        "message": "正在自动放行主控端口（UDP 47890 / TCP 47891），请在弹出的管理员确认框中点「是」…"
    })).ok();
    match crate::common::elevate::run_elevated_cmd(&script, "放行主控端口") {
        Ok(_) => {
            app.emit("firewall-status", serde_json::json!({
                "ok": true, "message": "已自动放行主控端口（UDP 47890 / TCP 47891）"
            })).ok();
        }
        Err(e) => {
            app.emit("firewall-status", serde_json::json!({
                "ok": false,
                "message": format!("主控端口自动放行失败：{e}（被控端将无法发现/连接本机，可稍后在「节点管理」重试）")
            })).ok();
        }
    }
}

/// 手动重试放行（设置页/节点页按钮）
#[tauri::command]
pub fn allow_discovery_firewall(app: AppHandle) -> Result<String, String> {
    let script = paths::data_dir().join("fw-master-manual.cmd");
    std::fs::write(&script, master_firewall_script()).map_err(|e| format!("写脚本失败: {e}"))?;
    crate::common::elevate::run_elevated_cmd(&script, "放行主控端口")
        .map(|_| "已放行 UDP 47890（发现）与 TCP 47891（被控控制连接），全部网络类型、限本网段".to_string())
        .map_err(|e| {
            app.emit("firewall-error", e.clone()).ok();
            e
        })
}

// ———————— 导出被控端 ————————

#[tauri::command]
pub fn export_agent_installer(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    label: Option<String>,
) -> Result<String, String> {
    let template = export::locate_template().ok_or_else(|| {
        "未找到被控端安装器模板。请先构建被控端安装包（agent-installer.exe）并放到主控程序目录。"
            .to_string()
    })?;

    let master_name = &state.master_name;
    let picked = app
        .dialog()
        .file()
        .set_file_name(format!("LLM被控端-引导安装-{master_name}.exe").as_str())
        .add_filter("可执行文件", &["exe"])
        .blocking_save_file();
    let out = match picked {
        Some(f) => file_path_to_string(f),
        None => return Err("已取消".into()),
    };

    // 每次导出生成新的长期引导码并落库（主控重启后依然有效，可在导出记录里作废）
    let cfg = export::create_bootstrap(state.inner(), &label.unwrap_or_else(|| "导出安装包".into()))?;
    let size = export::inject_and_write(Path::new(&template), &cfg, Path::new(&out))?;

    Ok(format!(
        "已导出被控端安装包：{}\n大小 {:.1} MB。\n引导码长期有效（可在「导出记录」中作废）；\
         把该文件拷到新电脑双击安装，装完启动即自动入网，无需输入 PIN。",
        out,
        size as f64 / 1048576.0,
    ))
}

// ———————— 导出记录（引导码管理） ————————

#[derive(serde::Serialize)]
pub struct BootstrapTokenView {
    pub id: String,
    pub label: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub use_count: i64,
    pub last_used_at: Option<i64>,
}

#[tauri::command]
pub fn list_bootstrap_tokens(state: State<'_, Arc<AppState>>) -> Vec<BootstrapTokenView> {
    state
        .db
        .list_tokens()
        .into_iter()
        .map(|t| BootstrapTokenView {
            id: t.id,
            label: t.label,
            created_at: t.created_at,
            expires_at: t.expires_at,
            use_count: t.use_count,
            last_used_at: t.last_used_at,
        })
        .collect()
}

#[tauri::command]
pub fn revoke_bootstrap_token(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<String, String> {
    state.db.delete_token(&id)?;
    Ok("已作废该引导码。用它导出的安装包将无法自动入网（已装好并配对的节点不受影响）。".into())
}

// ———————— HuggingFace 下载 ————————

fn hf_base(state: &State<'_, Arc<AppState>>) -> String {
    state
        .db
        .get_setting("hf_mirror")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| hf::DEFAULT_MIRROR.into())
}

#[tauri::command]
pub async fn hf_list_files(
    state: State<'_, Arc<AppState>>,
    repo_id: String,
) -> Result<Vec<hf::HfFile>, String> {
    let base = hf_base(&state);
    hf::list_repo_files(&base, &repo_id).await
}

#[tauri::command]
pub fn hf_download(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    repo_id: String,
    files: Vec<hf::HfFile>,
) -> Result<String, String> {
    if files.is_empty() {
        return Err("请至少选择一个文件".into());
    }
    let base = hf_base(&state);
    let dest = state.models_dir();
    let st = state.inner().clone();
    let hf = st.hf.clone();
    let repo = repo_id.trim().to_string();
    tauri::async_runtime::spawn(async move {
        let _ = hf::download_batch(app, st, hf, base, repo, files, dest).await;
    });
    Ok("下载已开始".into())
}

#[tauri::command]
pub fn hf_cancel(state: State<'_, Arc<AppState>>) {
    state.hf.cancel.store(true, Ordering::SeqCst);
}

#[tauri::command]
pub fn hf_status(state: State<'_, Arc<AppState>>) -> Option<hf::HfProgress> {
    state.hf.last.lock().unwrap().clone()
}

#[tauri::command]
pub fn get_hf_mirror(state: State<'_, Arc<AppState>>) -> String {
    hf_base(&state)
}

#[tauri::command]
pub fn set_hf_mirror(
    state: State<'_, Arc<AppState>>,
    mirror: String,
) -> Result<String, String> {
    let m = mirror.trim().trim_end_matches('/').to_string();
    if !m.is_empty() && !m.starts_with("http://") && !m.starts_with("https://") {
        return Err("镜像地址必须以 http(s):// 开头".into());
    }
    state.db.set_setting("hf_mirror", &m)?;
    let shown = if m.is_empty() { hf::DEFAULT_MIRROR.to_string() } else { m };
    Ok(format!("HF 下载源已设为 {shown}"))
}
