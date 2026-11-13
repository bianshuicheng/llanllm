//! 模型管理：本地导入（原地使用）、校验、重新定位、删除、目录迁移。

use crate::common::db::ModelRow;
use crate::common::paths;
use crate::master::llama::format_bytes;
use crate::master::state::AppState;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub fn import_model(state: &Arc<AppState>, path: &str) -> Result<ModelRow, String> {
    let p = PathBuf::from(path);
    if !p.is_absolute() {
        return Err("必须选择绝对路径".into());
    }
    if p.extension().map(|e| e.to_ascii_lowercase() != "gguf").unwrap_or(true) {
        return Err("仅支持 .gguf 模型文件".into());
    }
    let md = std::fs::metadata(&p)
        .map_err(|e| format!("无法读取文件信息：{path}（{e}）"))?;
    if md.is_dir() {
        return Err("所选路径是目录".into());
    }
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let row = ModelRow {
        id: uuid::Uuid::new_v4().to_string(),
        name: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        path: p.to_string_lossy().into_owned(),
        size: md.len(),
        mtime,
        source: "import".into(),
        quant: None,
        hf_repo: None,
        added_at: chrono::Utc::now().timestamp(),
        status: "ok".into(),
    };
    state.db.add_model(&row)?;
    Ok(row)
}

pub fn list_models(state: &Arc<AppState>) -> Vec<ModelRow> {
    let mut rows = state.db.list_models();
    for r in rows.iter_mut() {
        r.status = state.validate_model(r);
    }
    rows
}

/// 重新定位：把条目指向新的实际文件
pub fn relocate_model(state: &Arc<AppState>, id: &str, new_path: &str) -> Result<ModelRow, String> {
    let mut row = state
        .db
        .get_model(id)
        .ok_or_else(|| "条目不存在".to_string())?;
    let p = PathBuf::from(new_path);
    let md = std::fs::metadata(&p).map_err(|e| format!("新路径不可读：{new_path}（{e}）"))?;
    row.path = p.to_string_lossy().into_owned();
    row.name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| row.name.clone());
    row.size = md.len();
    row.mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(row.mtime);
    state.db.update_model_path(id, &row.path, row.size, row.mtime)?;
    Ok(row)
}

pub fn remove_model(state: &Arc<AppState>, id: &str, delete_file: bool) -> Result<(), String> {
    let row = state.db.get_model(id).ok_or("条目不存在")?;
    if delete_file {
        std::fs::remove_file(&row.path)
            .map_err(|e| format!("删除文件失败（记录已保留）：{}", e))?;
    }
    state.db.delete_model(id)
}

/// 当前默认模型目录（可能被设置覆盖）
pub fn models_dir(state: &Arc<AppState>) -> PathBuf {
    state.models_dir()
}

/// 更改默认模型目录。
/// - `move_files`=true：把位于旧目录内的模型文件迁移到新目录，并更新 DB 路径。
/// - `move_files`=false：仅切换目录，DB 记录原样保留（导入的模型不受影响）。
/// 返回迁移的文件数。
pub fn change_models_dir(
    state: &Arc<AppState>,
    new_dir: &str,
    move_files: bool,
) -> Result<usize, String> {
    let new_path = PathBuf::from(new_dir);
    if !new_path.is_absolute() {
        return Err("模型目录必须是绝对路径".into());
    }
    std::fs::create_dir_all(&new_path)
        .map_err(|e| format!("无法创建目录 {new_dir}：{e}（请检查权限或选择其他位置）"))?;
    // 写探测：确认目录可写
    let probe = new_path.join(".llanllm-write-test");
    std::fs::write(&probe, b"ok").map_err(|e| format!("目录不可写：{new_dir}（{e}）"))?;
    let _ = std::fs::remove_file(&probe);

    let old_dir = state.models_dir();
    let mut moved = 0usize;

    if move_files {
        let rows = state.db.list_models();
        for row in rows {
            let old_p = PathBuf::from(&row.path);
            // 只迁移位于旧默认目录内的文件
            if !starts_with_ci(&old_p, &old_dir) {
                continue;
            }
            let target = new_path.join(&row.name);
            if move_one(&old_p, &target)? {
                moved += 1;
            }
            state
                .db
                .update_model_path(&row.id, &target.to_string_lossy(), row.size, row.mtime)?;
        }
        // 迁移后尝试清掉旧目录（若为默认位置且已空）
        if old_dir != paths::default_models_dir() {
            let _ = std::fs::remove_dir(&old_dir);
        }
    }

    state.db.set_setting("models_dir", new_dir)?;
    *state.models_dir_override.lock().unwrap() = Some(new_path);
    Ok(moved)
}

fn move_one(from: &Path, to: &Path) -> Result<bool, String> {
    if !from.exists() {
        return Ok(false); // 文件本来就没了，DB 路径会在下次校验时标 missing
    }
    if from == to {
        return Ok(false);
    }
    if to.exists() {
        return Err(format!("目标已存在同名文件：{}", to.display()));
    }
    // 同盘 rename；跨盘 fallback 复制+删除
    if std::fs::rename(from, to).is_ok() {
        return Ok(true);
    }
    std::fs::copy(from, to)
        .map_err(|e| format!("迁移文件失败 {} → {}：{e}", from.display(), to.display()))?;
    std::fs::remove_file(from)
        .map_err(|e| format!("复制成功但删除源文件失败：{e}（请手动清理 {}）", from.display()))?;
    Ok(true)
}

/// Windows 大小写不敏感的前缀判断
fn starts_with_ci(p: &Path, dir: &Path) -> bool {
    let ps = p.to_string_lossy().to_lowercase();
    let ds = dir.to_string_lossy().to_lowercase();
    let ds = ds.trim_end_matches(['\\', '/']);
    ps.starts_with(&format!("{ds}\\")) || ps.starts_with(&format!("{ds}/"))
}

pub fn dir_free_bytes(p: &Path) -> Option<u64> {
    // Windows: GetDiskFreeSpaceExW
    #[cfg(windows)]
    {
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let wide: Vec<u16> = p
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            let mut free: u64 = 0;
            GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&mut free), None, None).ok()?;
            Some(free)
        }
    }
    #[cfg(not(windows))]
    {
        let _ = p;
        None
    }
}

/// 主控本地缓存目录占用
pub fn cache_dir_size() -> u64 {
    dir_size(&paths::cache_dir())
}

pub fn dir_size(p: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let ft = match e.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if ft.is_dir() {
                total += dir_size(&e.path());
            } else {
                total += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total
}

pub fn clear_master_cache(_state: &Arc<AppState>) -> Result<String, String> {
    let dir = paths::cache_dir();
    if !dir.exists() {
        return Ok("缓存目录不存在，无需清理".into());
    }
    let before = dir_size(&dir);
    std::fs::remove_dir_all(&dir)
        .map_err(|e| format!("清理失败：{e}（可能有进程占用缓存文件，请先停止推理服务）"))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("重建缓存目录失败: {e}"))?;
    Ok(format!(
        "已清理主控本地缓存，释放 {}",
        format_bytes(before)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_ci() {
        assert!(starts_with_ci(
            Path::new(r"E:\models\foo.gguf"),
            Path::new(r"e:\models")
        ));
        assert!(starts_with_ci(
            Path::new(r"E:\MODELS\sub\foo.gguf"),
            Path::new(r"E:\Models")
        ));
        assert!(!starts_with_ci(
            Path::new(r"E:\models2\foo.gguf"),
            Path::new(r"E:\models")
        ));
    }
}
