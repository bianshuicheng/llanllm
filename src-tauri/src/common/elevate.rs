//! 提权执行脚本（UAC 弹窗），用于防火墙规则等系统级操作。主控/被控共用。

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

/// 把 Windows 的退出码翻译成中文原因。
/// 常见值来自 PowerShell/consent.exe 的惯例（1/1223 = 用户取消 UAC）。
pub fn explain_exit_code(code: i32) -> String {
    match code {
        0 => "成功".into(),
        1 => "操作被用户取消（UAC 弹窗点了「否」或直接关闭）".into(),
        1223 => "操作被用户取消（UAC 弹窗点了「否」）".into(),
        -1 => "提权请求被拒绝（UAC 被取消）".into(),
        2 => "系统找不到指定的文件".into(),
        5 => "拒绝访问（权限不足）".into(),
        1602 => "操作被用户取消".into(),
        1603 => "安装过程中发生致命错误".into(),
        3010 => "成功，但需要重启才能完全生效".into(),
        _ => format!("退出码 {code}"),
    }
}

/// Start-Process RunAs 提权运行某个程序并等待退出码。
/// `arguments` 会原样拼在 -ArgumentList 里（调用方负责引号）。
fn run_as(program: &str, arguments: &str, window_title: &str) -> Result<String, String> {
    let inner = format!(
        "$p = Start-Process {program} -Verb RunAs -PassThru -Wait -WindowStyle Hidden \
         -ArgumentList {arguments}; \
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
    if code == 0 {
        return Ok(stdout);
    }
    Err(format!(
        "{window_title}失败：{}\n{}",
        explain_exit_code(code),
        if stderr.is_empty() { stdout } else { stderr }
    ))
}

/// 清理路径里可能引入的引号（防注入/防参数拆分）
fn sanitize_path(p: &Path) -> Result<String, String> {
    Ok(p.to_str()
        .ok_or("脚本路径含非法字符")?
        .replace(['"', '\''], ""))
}

/// 以管理员身份运行 .cmd 批处理脚本（防火墙 netsh 用）。
/// 路径带引号：用户名/安装目录含空格时（如 C:\Users\Li Na\...）不加引号必然失败。
pub fn run_elevated_cmd(script_path: &Path, window_title: &str) -> Result<String, String> {
    let script = sanitize_path(script_path)?;
    run_as(
        "cmd.exe",
        &format!("'/c','\"{script}\"'"),
        window_title,
    )
}

/// 以管理员身份运行 PowerShell 脚本（复杂逻辑用）。
pub fn run_elevated_script(script_path: &Path, window_title: &str) -> Result<String, String> {
    let script = sanitize_path(script_path)?;
    run_as(
        "powershell",
        &format!(
            "'-NoProfile','-ExecutionPolicy','Bypass','-File','\"{script}\"'"
        ),
        window_title,
    )
}

/// 运行一段非提权 PowerShell 并捕获 stdout
pub fn run_ps(script: &str) -> Result<String, String> {
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .hide_window()
        .output()
        .map_err(|e| format!("PowerShell 执行失败: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "PowerShell 执行失败（{}）",
            explain_exit_code(out.status.code().unwrap_or(-1))
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_explanations() {
        assert_eq!(explain_exit_code(0), "成功");
        assert!(explain_exit_code(1).contains("取消"));
        assert!(explain_exit_code(1223).contains("取消"));
        assert!(explain_exit_code(999).contains("999"));
    }

    #[test]
    fn sanitize_strips_quotes() {
        let p = Path::new("C:\\Users\\Li Na\\fw.cmd");
        assert_eq!(sanitize_path(p).unwrap(), "C:\\Users\\Li Na\\fw.cmd");
        let bad = Path::new("C:\\We\"ird'path.cmd");
        assert_eq!(sanitize_path(bad).unwrap(), "C:\\Weirdpath.cmd");
    }
}
