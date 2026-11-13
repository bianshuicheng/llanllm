//! HuggingFace 下载：仓库解析、镜像切换、断点续传、失败重试、进度上报。
//!
//! - 默认站 https://huggingface.co，可切 https://hf-mirror.com（设置项 hf_mirror）。
//! - 下载落盘 .part 文件 + Range 续传；完成后改名并注册进模型库（source=download）。
//! - 失败自动重试（5 次，指数退避）；全程通过 hf-progress 事件推给前端（前端同时可轮询 hf_status）。

use crate::common::db::ModelRow;
use crate::master::state::AppState;
use futures_util::StreamExt;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

pub const DEFAULT_MIRROR: &str = "https://huggingface.co";
pub const CN_MIRROR: &str = "https://hf-mirror.com";
const MAX_RETRIES: u32 = 5;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct HfFile {
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct HfProgress {
    pub repo: String,
    pub file: String,
    pub file_index: usize,
    pub file_total: usize,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// 字节/秒（最近窗口）
    pub speed_bps: u64,
    /// 剩余秒数（估算，可能为 null）
    pub eta_s: Option<u64>,
    /// preparing / downloading / retrying / done / failed / cancelled
    pub state: String,
    pub message: Option<String>,
}

pub struct HfState {
    /// 当前批次取消标志（None = 空闲）
    pub cancel: Arc<AtomicBool>,
    pub busy: AtomicBool,
    /// 前端轮询用的最近进度快照
    pub last: Mutex<Option<HfProgress>>,
}

impl HfState {
    pub fn new() -> Self {
        HfState {
            cancel: Arc::new(AtomicBool::new(false)),
            busy: AtomicBool::new(false),
            last: Mutex::new(None),
        }
    }

    fn set_progress(&self, p: HfProgress) {
        *self.last.lock().unwrap() = Some(p.clone());
    }
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("llanllm/0.1")
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("构建 HTTP 客户端失败: {e}"))
}

/// 规范化仓库 ID：允许完整网址 / 带网页路径尾巴，输出 "owner/name"。
pub fn normalize_repo(repo: &str) -> Result<String, String> {
    let repo = repo.trim().trim_end_matches('/').to_string();
    let repo = if let Some(idx) = repo.find("://") {
        let after = &repo[idx + 3..];
        match after.find('/') {
            Some(p) => after[p + 1..].to_string(),
            None => String::new(),
        }
    } else {
        repo.clone()
    };
    let repo = repo
        .split("/tree/")
        .next()
        .unwrap_or(&repo)
        .trim_end_matches('/')
        .to_string();
    let segs: Vec<&str> = repo.split('/').filter(|s| !s.is_empty()).collect();
    if segs.len() != 2 {
        return Err("仓库 ID 格式不正确（示例：Qwen/Qwen3-4B-GGUF）".into());
    }
    Ok(format!("{}/{}", segs[0], segs[1]))
}

/// 解析仓库里的 .gguf 文件列表
pub async fn list_repo_files(base: &str, repo: &str) -> Result<Vec<HfFile>, String> {
    let repo = normalize_repo(repo)?;
    let url = format!("{}/api/models/{}/tree/main?recursive=true", base, repo);
    let resp = client()?
        .get(&url)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| format!("访问 HF 失败（{url}）：{e}。国内网络请切换镜像站（设置 → HF 镜像）。"))?;
    if !resp.status().is_success() {
        return Err(format!("HF 返回 {}（仓库不存在或网络受限）", resp.status()));
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| format!("解析仓库列表失败: {e}"))?;
    let arr = v.as_array().ok_or("仓库列表格式异常")?;
    let mut files = Vec::new();
    for item in arr {
        let is_file = item.get("type").and_then(|t| t.as_str()) == Some("file");
        let path = item.get("path").and_then(|p| p.as_str()).unwrap_or("");
        let size = item.get("size").and_then(|s| s.as_u64()).unwrap_or(0);
        if is_file && path.to_ascii_lowercase().ends_with(".gguf") {
            files.push(HfFile { path: path.to_string(), size });
        }
    }
    if files.is_empty() {
        return Err("该仓库下没有 .gguf 模型文件".into());
    }
    Ok(files)
}

fn quant_of(name: &str) -> Option<String> {
    // 从文件名提取量化档位：...-Q4_K_M.gguf / Q4_K_M.gguf
    let stem = Path::new(name).file_stem()?.to_string_lossy().into_owned();
    for seg in stem.split(['-', '.', '_']).rev() {
        let s = seg.to_ascii_uppercase();
        if (s.starts_with('I') && s.len() >= 5 && s[1..].chars().all(|c| c.is_ascii_digit()))
            || (s.starts_with('Q')
                && s.len() >= 3
                && s.as_bytes()[1].is_ascii_digit())
        {
            return Some(s);
        }
    }
    None
}

/// 下载一批文件（顺序），断点续传 + 重试；完成后注册模型。
pub async fn download_batch(
    app: AppHandle,
    app_state: Arc<AppState>,
    hf: Arc<HfState>,
    base: String,
    repo: String,
    files: Vec<HfFile>,
    dest_dir: PathBuf,
) -> Result<(), String> {
    if hf.busy.swap(true, Ordering::SeqCst) {
        return Err("已有下载任务进行中".into());
    }
    hf.cancel.store(false, Ordering::SeqCst);
    std::fs::create_dir_all(&dest_dir).map_err(|e| format!("创建下载目录失败: {e}"))?;

    let http = client()?;
    let repo = normalize_repo(&repo)?;
    let mut results: Vec<Result<ModelRow, String>> = Vec::new();

    for (idx, f) in files.iter().enumerate() {
        if hf.cancel.load(Ordering::SeqCst) {
            hf.set_progress(HfProgress {
                repo: repo.clone(),
                file: f.path.clone(),
                file_index: idx,
                file_total: files.len(),
                bytes_done: 0,
                bytes_total: 0,
                speed_bps: 0,
                eta_s: None,
                state: "cancelled".into(),
                message: Some("已取消".into()),
            });
            break;
        }
        let res = download_one(&app, &hf, &http, &base, &repo, f, idx, files.len(), &dest_dir).await;
        match res {
            Ok((path, size, mtime)) => {
                let name = Path::new(&f.path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| f.path.clone());
                let row = ModelRow {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: name.clone(),
                    path: path.to_string_lossy().into_owned(),
                    size,
                    mtime,
                    source: "download".into(),
                    quant: quant_of(&name),
                    hf_repo: Some(repo.clone()),
                    added_at: chrono::Utc::now().timestamp(),
                    status: "ok".into(),
                };
                if let Err(e) = app_state.db.add_model(&row) {
                    results.push(Err(e));
                } else {
                    results.push(Ok(row));
                }
            }
            Err(e) => {
                if hf.cancel.load(Ordering::SeqCst) {
                    break;
                }
                results.push(Err(e));
                break; // 某文件彻底失败则中止批次
            }
        }
    }

    hf.busy.store(false, Ordering::SeqCst);
    let ok = results.iter().filter(|r| r.is_ok()).count();
    let fail_msg: Option<String> =
        results.iter().find_map(|r| r.as_ref().err().cloned());
    let was_cancelled = hf.cancel.load(Ordering::SeqCst);
    hf.set_progress(HfProgress {
        repo: repo.clone(),
        file: files.last().map(|f| f.path.clone()).unwrap_or_default(),
        file_index: files.len(),
        file_total: files.len(),
        bytes_done: 0,
        bytes_total: 0,
        speed_bps: 0,
        eta_s: None,
        state: if was_cancelled {
            "cancelled".into()
        } else if fail_msg.is_some() {
            "failed".into()
        } else {
            "done".into()
        },
        message: Some(match (was_cancelled, fail_msg) {
            (true, _) => "下载已取消".to_string(),
            (false, Some(e)) => e,
            _ => format!("完成：{ok} 个文件已入库"),
        }),
    });
    app.emit("hf-progress", hf.last.lock().unwrap().clone()).ok();
    Ok(())
}

async fn download_one(
    app: &AppHandle,
    hf: &Arc<HfState>,
    http: &reqwest::Client,
    base: &str,
    repo: &str,
    f: &HfFile,
    idx: usize,
    total: usize,
    dest_dir: &Path,
) -> Result<(PathBuf, u64, i64), String> {
    let url = format!("{}/{}/resolve/main/{}", base, repo, f.path);
    let name = Path::new(&f.path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| f.path.clone());
    let final_path = dest_dir.join(&name);
    let part_path = dest_dir.join(format!("{name}.part"));

    let mut attempt: u32 = 0;
    loop {
        if hf.cancel.load(Ordering::SeqCst) {
            return Err("已取消".into());
        }
        attempt += 1;
        let start_from = std::fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0);

        hf.set_progress(HfProgress {
            repo: repo.into(),
            file: f.path.clone(),
            file_index: idx,
            file_total: total,
            bytes_done: start_from,
            bytes_total: f.size,
            speed_bps: 0,
            eta_s: None,
            state: if attempt == 1 {
                "downloading".into()
            } else {
                format!("retrying")
            },
            message: Some(if attempt == 1 {
                String::new()
            } else {
                format!("第 {attempt}/{MAX_RETRIES} 次重试")
            }),
        });

        let mut req = http.get(&url);
        if start_from > 0 {
            req = req.header("Range", format!("bytes={start_from}-"));
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                if attempt >= MAX_RETRIES {
                    return Err(format!("下载失败（重试 {MAX_RETRIES} 次）：{e}。国内网络请切换 HF 镜像站。"));
                }
                tokio::time::sleep(Duration::from_secs(2u64.pow(attempt - 1).min(30))).await;
                continue;
            }
        };
        if !resp.status().is_success() {
            let code = resp.status();
            if attempt >= MAX_RETRIES {
                return Err(format!("下载失败：HF 返回 {code}（重试 {MAX_RETRIES} 次）"));
            }
            tokio::time::sleep(Duration::from_secs(2u64.pow(attempt - 1).min(30))).await;
            continue;
        }

        // 流式落盘
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&part_path)
            .await
            .map_err(|e| format!("打开 .part 文件失败: {e}"))?;
        let mut writer = tokio::io::BufWriter::new(file);
        let mut stream = resp.bytes_stream();
        let mut done = start_from;
        let mut last_report = std::time::Instant::now();
        let mut last_bytes = start_from;

        let mut io_err: Option<String> = None;
        loop {
            // 30 秒无新数据视为停滞（reqwest 总超时会切断大文件下载，故用分块空闲超时）
            let chunk = match tokio::time::timeout(Duration::from_secs(30), stream.next()).await {
                Ok(Some(c)) => c,
                Ok(None) => break, // 流结束
                Err(_) => {
                    io_err = Some("连接停滞超过 30 秒".into());
                    break;
                }
            };
            if hf.cancel.load(Ordering::SeqCst) {
                return Err("已取消".into());
            }
            match chunk {
                Ok(bytes) => {
                    use tokio::io::AsyncWriteExt;
                    if let Err(e) = writer.write_all(&bytes).await {
                        io_err = Some(format!("写盘失败: {e}"));
                        break;
                    }
                    done += bytes.len() as u64;
                    if last_report.elapsed() >= PROGRESS_INTERVAL {
                        let elapsed = last_report.elapsed().as_secs_f64().max(0.001);
                        let speed = ((done - last_bytes) as f64 / elapsed) as u64;
                        last_report = std::time::Instant::now();
                        last_bytes = done;
                        let eta = f
                            .size
                            .checked_sub(done)
                            .map(|rem| if speed > 0 { rem / speed.max(1) } else { 0 });
                        hf.set_progress(HfProgress {
                            repo: repo.into(),
                            file: f.path.clone(),
                            file_index: idx,
                            file_total: total,
                            bytes_done: done,
                            bytes_total: f.size,
                            speed_bps: speed,
                            eta_s: eta,
                            state: "downloading".into(),
                            message: None,
                        });
                        app.emit("hf-progress", hf.last.lock().unwrap().clone()).ok();
                    }
                }
                Err(e) => {
                    io_err = Some(format!("网络中断: {e}"));
                    break;
                }
            }
            if last_report.elapsed() > Duration::from_secs(60) {
                io_err = Some("连接停滞超过 60 秒".into());
                break;
            }
        }

        if let Some(e) = io_err {
            use tokio::io::AsyncWriteExt;
            let _ = writer.flush().await;
            if attempt >= MAX_RETRIES {
                return Err(format!("{e}（重试 {MAX_RETRIES} 次后仍失败）。已保留 .part 进度，可稍后重试。"));
            }
            tokio::time::sleep(Duration::from_secs(2u64.pow(attempt - 1).min(30))).await;
            continue;
        }

        use tokio::io::AsyncWriteExt;
        writer.flush().await.map_err(|e| format!("flush 失败: {e}"))?;
        drop(writer);

        // 校验大小
        let got = std::fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0);
        if f.size > 0 && got != f.size {
            if attempt >= MAX_RETRIES {
                return Err(format!(
                    "文件大小校验失败：得到 {got}，期望 {}。请重试或切换镜像。",
                    f.size
                ));
            }
            continue; // 保留 .part 断点续传
        }

        // 完成：.part → 正式文件
        if final_path.exists() {
            std::fs::remove_file(&final_path).map_err(|e| format!("清理旧文件失败: {e}"))?;
        }
        std::fs::rename(&part_path, &final_path).map_err(|e| format!("重命名失败: {e}"))?;
        let mtime = std::fs::metadata(&final_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        hf.set_progress(HfProgress {
            repo: repo.into(),
            file: f.path.clone(),
            file_index: idx + 1,
            file_total: total,
            bytes_done: got,
            bytes_total: f.size,
            speed_bps: 0,
            eta_s: None,
            state: if idx + 1 == total { "done".into() } else { "downloading".into() },
            message: Some(format!("「{name}」下载完成")),
        });
        app.emit("hf-progress", hf.last.lock().unwrap().clone()).ok();
        return Ok((final_path, got, mtime));
    }
}

pub fn human_speed(bps: u64) -> String {
    if bps >= 1024 * 1024 {
        format!("{:.1} MB/s", bps as f64 / 1048576.0)
    } else {
        format!("{} KB/s", bps / 1024)
    }
}
