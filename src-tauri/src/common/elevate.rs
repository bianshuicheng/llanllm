//! 提权执行 PowerShell 脚本（UAC 弹窗），用于防火墙规则等系统级操作。
//! 主控/被控共用。

use std::path::Path;
use std::process::Command;

trait HideWindow {
    fn hide_window(&mut self) -> &mut Self;
}

impl HideWindow for Command {
    fn hide_window(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            self.creation_flags(CREATE_NO_WINDOW);
        }
        self
    }
}

/// 写脚本文件并以管理员身份运行，等待完成并返回是否成功。
/// 用户取消 UAC → Err。脚本自身的输出会透传回来辅助排错。
pub fn run_elevated_script(script_path: &Path, window_title: &str) -> Result<String, String> {
    let script = script_path
        .to_str()
        .ok_or("脚本路径含非法字符")?
        .replace('\'', "''");

    // 内层：以管理员身份运行目标脚本；外层：等待并取回退出码
    let inner = format!(
        "$p = Start-Process powershell -Verb RunAs -PassThru -Wait -WindowStyle Hidden \
         -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','{script}'; \
         exit $p.ExitCode"
    );
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &inner])
        .hide_window()
        .output()
        .map_err(|e| format!("无法启动提权流程: {e}"))?;

    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    let code = out.status.code().unwrap_or(-1);
    if code == -1 {
        return Err(format!(
            "提权请求被拒绝或失败（UAC 被取消？）\n{window_title}\n{stderr}"
        ));
    }
    if code != 0 {
        return Err(format!(
            "管理员操作失败（退出码 {code}）\n{stdout}\n{stderr}"
        ));
    }
    Ok(stdout)
}

/// 运行一段非提权 PowerShell 并捕获 stdout
pub fn run_ps(script: &str) -> Result<String, String> {
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .hide_window()
        .output()
        .map_err(|e| format!("PowerShell 执行失败: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
