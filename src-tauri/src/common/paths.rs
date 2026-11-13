//! 各角色的数据目录、模型目录、缓存目录与 llama.cpp runtime 定位。

use std::path::PathBuf;

#[cfg(feature = "agent")]
pub const ROLE: &str = "agent";
#[cfg(all(not(feature = "agent"), feature = "master"))]
pub const ROLE: &str = "master";
#[cfg(not(any(feature = "agent", feature = "master")))]
compile_error!("必须选择角色 feature：master 或 agent");
#[cfg(all(feature = "agent", feature = "master"))]
compile_error!("master 与 agent feature 互斥");

pub fn app_id() -> &'static str {
    #[cfg(feature = "agent")]
    {
        "com.llanllm.agent"
    }
    #[cfg(not(feature = "agent"))]
    {
        "com.llanllm.master"
    }
}

/// %LOCALAPPDATA%/<app_id>/
pub fn app_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| std::env::temp_dir())
        .join(app_id())
}

/// 数据目录（SQLite、日志、密钥文件）
pub fn data_dir() -> PathBuf {
    app_dir().join("data")
}

/// 默认模型目录（可被设置覆盖；仅主控使用）
pub fn default_models_dir() -> PathBuf {
    app_dir().join("models")
}

/// rpc 权重缓存目录（LLAMA_CACHE 指向这里）
pub fn cache_dir() -> PathBuf {
    app_dir().join("rpc-cache")
}

/// llama.cpp runtime（llama-server / ggml-rpc-server + dll）
/// 优先级：安装目录/runtime（生产） → 源码 binaries/runtime（开发）
pub fn runtime_dir() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let p = parent.join("runtime");
            if p.join("llama-server.exe").exists() {
                return Some(p);
            }
            // 直接和主程序放在一起也接受
            if parent.join("llama-server.exe").exists() {
                return Some(parent.to_path_buf());
            }
        }
    }
    if let Some(manifest) = option_env!("CARGO_MANIFEST_DIR") {
        let p = PathBuf::from(manifest).join("binaries").join("runtime");
        if p.join("llama-server.exe").exists() {
            return Some(p);
        }
    }
    None
}

pub fn ensure_dirs() -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn role_matches_feature() {
        #[cfg(feature = "agent")]
        assert_eq!(ROLE, "agent");
        #[cfg(not(feature = "agent"))]
        assert_eq!(ROLE, "master");
    }
}
