//! 待验证 PIN 管理：被控端主动连接并提交 PIN 证明来完成配对，
//! 主控不再主动拨号（由 ctl_server 在连接上验证）。

use crate::master::state::{AppState, PinEntry};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

/// 周期清理过期 PIN 并通知前端
pub async fn run_pin_sweeper(app: AppHandle, state: Arc<AppState>) {
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let mut expired_label: Option<String> = None;
        {
            let mut pins = state.pending_pins.lock().unwrap();
            let before = pins.len();
            pins.retain(|p| !p.expired());
            if pins.len() < before {
                expired_label = Some("PIN 已过期，请重新发起配对或重新导出安装包。".into());
            }
        }
        if let Some(msg) = expired_label {
            app.emit("pair-progress", serde_json::json!({
                "state": "expired",
                "message": msg
            })).ok();
        }
    }
}

/// 新增待验证 PIN（手动配对 5 分钟 / 导出引导 30 分钟）
pub fn add_pending_pin(state: &Arc<AppState>, pin: &str, ttl: Duration, label: &str) {
    state.pending_pins.lock().unwrap().push(PinEntry {
        pin: pin.to_string(),
        label: label.to_string(),
        expires_at: std::time::Instant::now() + ttl,
    });
}
