//! llama-server 进程管理：拼装 --rpc 参数、启动/停止、日志捕获。

use crate::common::paths;
use crate::common::proc::{base_command, spawn_managed};
use crate::common::protocol::LLAMA_PORT_DEFAULT;
use crate::master::state::{AppState, LlamaProc};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

pub const LOG_CAP: usize = 800;

pub fn push_log(state: &Arc<AppState>, app: &AppHandle, line: String) {
    let mut logs = state.llama_logs.lock().unwrap();
    logs.push(line.clone());
    if logs.len() > LOG_CAP {
        let overflow = logs.len() - LOG_CAP;
        logs.drain(..overflow);
    }
    drop(logs);
    app.emit("llama-log", line).ok();
}

/// 启动 llama-server。
/// - `model_path`：gguf 绝对路径（任意位置）
/// - `rpc_targets`：在线已配对节点 "ip:50052" 列表
pub fn start_llama(
    app: AppHandle,
    state: Arc<AppState>,
    model_path: String,
    model_name: String,
    ctx_size: u32,
    ngl: i32,
    port: u16,
    rpc_targets: Vec<String>,
) -> Result<(), String> {
    let mut guard = state.llama.lock().unwrap();
    if guard.is_some() {
        return Err("推理服务已在运行，请先停止".into());
    }

    let exe = paths::runtime_dir()
        .ok_or("未找到 llama.cpp 运行时（runtime/llama-server.exe）")?
        .join("llama-server.exe");
    if !exe.exists() {
        return Err(format!("未找到 llama-server：{}", exe.display()));
    }
    let mpath = PathBuf::from(&model_path);
    let md = std::fs::metadata(&mpath)
        .map_err(|e| format!("模型文件不可用：{}（{e}）。请到「模型管理」重新定位。", model_path))?;
    if md.is_dir() {
        return Err("所选路径是目录，不是 .gguf 文件".into());
    }

    let mut cmd = base_command(&exe);
    cmd.arg("-m").arg(&mpath);
    if !rpc_targets.is_empty() {
        cmd.arg("--rpc").arg(rpc_targets.join(","));
    }
    cmd.arg("-ngl").arg(ngl.to_string());
    cmd.arg("--ctx-size").arg(ctx_size.to_string());
    cmd.arg("--host").arg("0.0.0.0");
    cmd.arg("--port").arg(port.to_string());
    cmd.env("LLAMA_CACHE", paths::cache_dir());
    cmd.current_dir(exe.parent().unwrap_or(std::path::Path::new(".")));

    let (child, managed) = spawn_managed(cmd)?;
    let started_at = std::time::Instant::now();

    *state.llama_logs.lock().unwrap() = Vec::new();
    push_log(
        &state,
        &app,
        format!(
            "[启动] llama-server 模型={} rpc=[{}] ctx={} ngl={} port={} size={}",
            model_path,
            if rpc_targets.is_empty() { "仅本机".to_string() } else { rpc_targets.join(",") },
            ctx_size,
            ngl,
            port,
            format_bytes(md.len())
        ),
    );

    let proc = LlamaProc {
        child,
        _managed: managed,
        started_at,
        model_path: model_path.clone(),
        model_name,
        port,
        ctx_size,
        ngl,
        rpc_targets,
    };
    *guard = Some(proc);
    drop(guard);

    // 捕获 stdout/stderr → 日志
    read_pipes(app.clone(), state.clone());

    // 监视进程退出
    let app2 = app.clone();
    let state2 = state.clone();
    tauri::async_runtime::spawn(async move {
        let exit_info = loop {
            let done: Option<String> = {
                let mut guard = state2.llama.lock().unwrap();
                match guard.as_mut() {
                    Some(p) => match p.child.try_wait() {
                        Ok(Some(status)) => {
                            let info = format!("[退出] llama-server 已退出（{status}）");
                            guard.take();
                            Some(info)
                        }
                        Ok(None) => None,
                        Err(e) => {
                            let info = format!("[退出] 等待进程状态失败：{e}");
                            guard.take();
                            Some(info)
                        }
                    },
                    None => Some("[退出] 进程已被停止".to_string()),
                }
            }; // guard 在此离开作用域，await 之前释放
            if let Some(info) = done {
                break info;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        push_log(&state2, &app2, exit_info);
        app2.emit("llama-status", state2.llama_status()).ok();
    });

    app.emit("llama-status", state.llama_status()).ok();
    Ok(())
}

pub fn stop_llama(state: &Arc<AppState>) -> Result<(), String> {
    let mut guard = state.llama.lock().unwrap();
    if let Some(p) = guard.as_mut() {
        p.child.start_kill().map_err(|e| format!("停止失败: {e}"))?;
    }
    // ManagedChild 随 take() 释放 → Job 句柄关闭 → 整树终止
    guard.take();
    Ok(())
}

/// 日志读取：进入时把 stdout/stderr 从 state 中的 child 取走，
/// 两个后台任务逐行推送到前端。
fn read_pipes(app: AppHandle, state: Arc<AppState>) {
    let (stdout, stderr) = {
        let mut guard = state.llama.lock().unwrap();
        match guard.as_mut() {
            Some(p) => (p.child.stdout.take(), p.child.stderr.take()),
            None => (None, None),
        }
    };
    if let Some(out) = stdout {
        let app = app.clone();
        let st = state.clone();
        tauri::async_runtime::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut lines = BufReader::new(out).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(l)) => {
                        let line = format!("[stdout] {l}");
                        push_log(&st, &app, line);
                    }
                    _ => break,
                }
            }
        });
    }
    if let Some(err) = stderr {
        let app = app.clone();
        let st = state.clone();
        tauri::async_runtime::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut lines = BufReader::new(err).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(l)) => {
                        let line = format!("[llama] {l}");
                        push_log(&st, &app, line);
                    }
                    _ => break,
                }
            }
        });
    }
}

pub fn format_bytes(n: u64) -> String {
    if n >= 1024 * 1024 * 1024 {
        format!("{:.2} GB", n as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    } else {
        format!("{n} B")
    }
}

/// 默认端口给 UI 展示用
pub fn default_port() -> u16 {
    LLAMA_PORT_DEFAULT
}
