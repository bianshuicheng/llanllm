//! ggml-rpc-server 进程看护：启动、崩溃自动拉起、停止、状态。

use crate::common::paths;
use crate::common::proc::{base_command, spawn_managed};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct RpcProc {
    pub child: tokio::process::Child,
    pub _managed: crate::common::proc::ManagedChild,
    pub started_at: Instant,
    pub port: u16,
}

#[derive(Default)]
struct RpcInner {
    proc: Option<RpcProc>,
    restarts: u32,
    last_exit_note: Option<String>,
}

pub struct RpcRunner {
    inner: Mutex<RpcInner>,
    port: AtomicU16,
    stopping: AtomicBool,
    logs: Mutex<Vec<String>>,
}

const LOG_CAP: usize = 300;
const MAX_RESTARTS: u32 = 50;

impl RpcRunner {
    pub fn new() -> Self {
        RpcRunner {
            inner: Mutex::new(RpcInner::default()),
            port: AtomicU16::new(crate::common::protocol::RPC_PORT_DEFAULT),
            stopping: AtomicBool::new(false),
            logs: Mutex::new(Vec::new()),
        }
    }

    pub fn port(&self) -> u16 {
        self.port.load(Ordering::Relaxed)
    }

    fn push_log(&self, line: String) {
        let mut logs = self.logs.lock().unwrap();
        logs.push(line);
        if logs.len() > LOG_CAP {
            let overflow = logs.len() - LOG_CAP;
            logs.drain(..overflow);
        }
    }

    pub fn tail_logs(&self, n: usize) -> Vec<String> {
        let logs = self.logs.lock().unwrap();
        if logs.len() <= n {
            logs.clone()
        } else {
            logs[logs.len() - n..].to_vec()
        }
    }

    /// 启动（若已运行则直接返回 Ok(false)）
    pub fn ensure_running(self: &Arc<Self>, port: u16) -> Result<bool, String> {
        {
            let mut inner = self.inner.lock().unwrap();
            if let Some(p) = inner.proc.as_mut() {
                if p.child.try_wait().map(|s| s.is_none()).unwrap_or(false) {
                    return Ok(false); // 已在运行
                }
            }
        }
        self.stopping.store(false, Ordering::Relaxed);
        self.port.store(port, Ordering::Relaxed);

        let exe = paths::runtime_dir()
            .ok_or("未找到 llama.cpp 运行时（runtime/ggml-rpc-server.exe）")?
            .join("ggml-rpc-server.exe");
        if !exe.exists() {
            return Err(format!("未找到 ggml-rpc-server：{}", exe.display()));
        }

        let mut cmd = base_command(&exe);
        cmd.arg("-H").arg("0.0.0.0").arg("-p").arg(port.to_string()).arg("-c");
        cmd.env("LLAMA_CACHE", paths::cache_dir());
        cmd.current_dir(exe.parent().unwrap_or(std::path::Path::new(".")));

        let (child, managed) = spawn_managed(cmd)?;
        self.push_log(format!(
            "[启动] ggml-rpc-server -H 0.0.0.0 -p {port} -c pid={}",
            child.id().unwrap_or(0)
        ));

        let started_at = Instant::now();
        let (stdout, stderr) = {
            let mut inner = self.inner.lock().unwrap();
            inner.proc = Some(RpcProc { child, _managed: managed, started_at, port });
            let p = inner.proc.as_mut().unwrap();
            (p.child.stdout.take(), p.child.stderr.take())
        };

        self.spawn_pipe_readers(stdout, stderr);
        self.spawn_supervisor();
        Ok(true)
    }

    fn spawn_pipe_readers(
        self: &Arc<Self>,
        stdout: Option<tokio::process::ChildStdout>,
        stderr: Option<tokio::process::ChildStderr>,
    ) {
        if let Some(out) = stdout {
            let this = Arc::clone(self);
            tauri::async_runtime::spawn(async move {
                read_lines(out, &this).await;
            });
        }
        if let Some(err) = stderr {
            let this = Arc::clone(self);
            tauri::async_runtime::spawn(async move {
                read_lines_err(err, &this).await;
            });
        }
    }

    /// 看护：进程意外退出时 3 秒后自动拉起（主动停止除外）
    fn spawn_supervisor(self: &Arc<Self>) {
        let this = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(800)).await;
                let mut exited = false;
                {
                    let mut inner = this.inner.lock().unwrap();
                    if let Some(p) = inner.proc.as_mut() {
                        match p.child.try_wait() {
                            Ok(Some(status)) => {
                                inner.last_exit_note = Some(format!("进程退出：{status}"));
                                this.push_log(format!("[退出] ggml-rpc-server 退出（{status}）"));
                                exited = true;
                            }
                            Ok(None) => {}
                            Err(e) => {
                                inner.last_exit_note = Some(format!("等待失败：{e}"));
                                exited = true;
                            }
                        }
                    }
                }
                if !exited {
                    continue;
                }
                if this.stopping.load(Ordering::Relaxed) {
                    this.inner.lock().unwrap().proc = None;
                    this.stopping.store(false, Ordering::Relaxed);
                    break;
                }
                // 崩溃自动拉起
                let restart: Option<u16> = {
                    let mut inner = this.inner.lock().unwrap();
                    if inner.restarts >= MAX_RESTARTS {
                        this.push_log("[看护] 重启次数过多，暂停自动重启".into());
                        None
                    } else {
                        inner.restarts += 1;
                        let port = inner
                            .proc
                            .as_ref()
                            .map(|p| p.port)
                            .unwrap_or_else(|| this.port());
                        inner.proc = None;
                        this.push_log(format!("[看护] 3 秒后自动重启 rpc-server（端口 {port}）"));
                        Some(port)
                    }
                };
                match restart {
                    Some(port) => {
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        if let Err(e) = this.ensure_running(port) {
                            this.push_log(format!("[看护] 重启失败：{e}"));
                            break;
                        }
                        break; // 新的 supervisor 已随新进程启动
                    }
                    None => break,
                }
            }
        });
    }

    pub fn stop(&self) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(p) = inner.proc.as_mut() {
            self.stopping.store(true, Ordering::Relaxed);
            p.child.start_kill().map_err(|e| format!("停止失败: {e}"))?;
            self.push_log("[停止] ggml-rpc-server 已请求停止".into());
        }
        Ok(())
    }

    pub fn status(&self) -> (bool, Option<u32>, u64, u16) {
        let mut inner = self.inner.lock().unwrap();
        match inner.proc.as_mut() {
            Some(p) => {
                let running = p.child.try_wait().map(|s| s.is_none()).unwrap_or(false);
                (running, p.child.id(), p.started_at.elapsed().as_secs(), p.port)
            }
            None => (false, None, 0, self.port()),
        }
    }
}

async fn read_lines(out: tokio::process::ChildStdout, runner: &RpcRunner) {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut lines = BufReader::new(out).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(l)) => runner.push_log(format!("[rpc] {l}")),
            _ => break,
        }
    }
}

async fn read_lines_err(err: tokio::process::ChildStderr, runner: &RpcRunner) {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut lines = BufReader::new(err).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(l)) => runner.push_log(format!("[rpc] {l}")),
            _ => break,
        }
    }
}
