//! 被控端入口：Tauri 应用、命令注册、心跳/控制服务/RPC 看护后台任务。

pub mod ctl_client;
pub mod firewall;
pub mod heartbeat;
pub mod rpc_runner;
pub mod state;

use crate::common::db::Db;
use crate::common::paths;
use state::AgentState;
use std::sync::Arc;
use tauri::{Manager, State};

pub fn run() {
    paths::ensure_dirs().expect("初始化数据目录失败");

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let db = Db::open(&paths::data_dir().join("agent.db"))?;
            let state = Arc::new(AgentState::load(db)?);
            state.set_app(handle.clone());
            app.manage(state.clone());

            // 读取安装包注入的引导配置，自动完成配对（无需手动输入 PIN）。
            // 两个来源：1) 安装目录 config.json（NSIS 安装器写入）
            //          2) 自身 exe 尾部的注入标记（直接运行导出的 exe 时）
            // 引导码长期有效（可被主控作废），过期只在界面上提示但不阻断。
            if !state.is_paired() {
                let cfg_from_json = std::env::current_exe().ok().and_then(|exe| {
                    let dir = exe.parent()?.to_path_buf();
                    let data = std::fs::read(dir.join("config.json")).ok()?;
                    serde_json::from_slice::<crate::common::bootstrap::BootstrapConfig>(&data).ok()
                });
                let cfg_from_tail = std::env::current_exe().ok().and_then(|exe| {
                    let blob = std::fs::read(&exe).ok()?;
                    crate::common::bootstrap::parse_tail(&blob)
                });
                let cfg = cfg_from_json.or(cfg_from_tail);
                if let Some(cfg) = cfg {
                    let master_ip = cfg.master_ip.trim().to_string();
                    if !master_ip.is_empty() {
                        state.set_addr_hint(&master_ip, cfg.ctl_port);
                    }
                    state.set_bootstrap(&cfg.token_id, &cfg.token);
                    if cfg.expired(chrono::Utc::now().timestamp()) {
                        state.push_ui_log(
                            "安装包中的引导码已过期，请到主控重新导出安装包，或手动输入 PIN。".into(),
                        );
                    } else {
                        state.push_ui_log(format!(
                            "检测到安装包注入的引导配置（主控「{}」@ {master_ip}），自动配对中…",
                            cfg.master_name
                        ));
                    }
                }
            }

            // 心跳广播（含监听主控存在应答）
            tauri::async_runtime::spawn(heartbeat::run_heartbeat(state.clone()));
            // 主动连接主控：配对 + 持久控制会话
            tauri::async_runtime::spawn(ctl_client::run_ctl_client(state.clone()));
            // 已配对则自动启动 rpc-server
            if state.is_paired() {
                let st = state.clone();
                tauri::async_runtime::spawn(async move {
                    let port = st.rpc_port_setting();
                    match st.rpc.ensure_running(port) {
                        Ok(true) => st.push_ui_log(format!("rpc-server 已自动启动（端口 {port}）")),
                        Ok(false) => {}
                        Err(e) => st.push_ui_log(format!("rpc-server 自动启动失败：{e}")),
                    }
                    st.emit_state();
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_role,
            agent_status,
            submit_pair_pin,
            cancel_pair_pin,
            unpair,
            rpc_restart,
            rpc_stop,
            clear_agent_cache,
            open_agent_cache_dir,
            set_agent_rpc_port,
        ])
        .run(tauri::generate_context!())
        .expect("被控端启动失败");
}

#[tauri::command]
pub fn get_role() -> String {
    "agent".into()
}

#[tauri::command]
pub fn agent_status(state: State<'_, Arc<AgentState>>) -> serde_json::Value {
    state.status_view()
}

#[tauri::command]
pub fn submit_pair_pin(
    state: State<'_, Arc<AgentState>>,
    pin: String,
) -> Result<String, String> {
    state.submit_pending_pin(&pin)?;
    Ok("已记录 PIN，等待主控发起配对。请保持本窗口开启。".into())
}

#[tauri::command]
pub fn cancel_pair_pin(state: State<'_, Arc<AgentState>>) {
    state.clear_pending_pin();
}

#[tauri::command]
pub fn unpair(state: State<'_, Arc<AgentState>>) -> Result<String, String> {
    state.unpair()?;
    Ok("已解除配对".into())
}

#[tauri::command]
pub fn rpc_restart(state: State<'_, Arc<AgentState>>) -> Result<String, String> {
    let port = state.rpc_port_setting();
    state.rpc.stop().ok();
    std::thread::sleep(std::time::Duration::from_millis(500));
    match state.rpc.ensure_running(port) {
        Ok(true) => {
            state.push_ui_log(format!("rpc-server 已重启（端口 {port}）"));
            Ok(format!("rpc-server 已重启（端口 {port}）"))
        }
        Ok(false) => Ok("rpc-server 已在运行".into()),
        Err(e) => Err(e),
    }
}

#[tauri::command]
pub fn rpc_stop(state: State<'_, Arc<AgentState>>) -> Result<String, String> {
    state.rpc.stop()?;
    state.push_ui_log("rpc-server 已停止".into());
    Ok("rpc-server 已停止".into())
}

#[tauri::command]
pub fn clear_agent_cache(state: State<'_, Arc<AgentState>>) -> Result<String, String> {
    state.clear_cache()
}

#[tauri::command]
pub fn open_agent_cache_dir(state: State<'_, Arc<AgentState>>) -> Result<(), String> {
    state.open_cache_dir()
}

#[tauri::command]
pub fn set_agent_rpc_port(
    state: State<'_, Arc<AgentState>>,
    port: u16,
) -> Result<String, String> {
    state.set_rpc_port(port);
    Ok(format!("端口已保存为 {port}，重启 rpc-server 后生效"))
}
