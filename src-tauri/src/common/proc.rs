//! 子进程管理：Windows Job Object 保证主程序退出时子进程（rpc-server /
//! llama-server）整树终止，不产生僵尸进程。

/// 生命周期内持有 Job 句柄；Drop 时关闭句柄 → 系统级杀掉整个进程树。
#[cfg(windows)]
pub struct JobHandle {
    handle: windows::Win32::Foundation::HANDLE,
}

// HANDLE 是裸指针所以默认 !Send；本句柄仅用于 CloseHandle/AssignProcessToJobObject，
// 跨线程移动使用是安全的（Windows 内核对象句柄本身与线程无关）。
#[cfg(windows)]
unsafe impl Send for JobHandle {}

#[cfg(windows)]
impl JobHandle {
    pub fn create() -> Result<Self, String> {
        use windows::Win32::System::JobObjects::{
            CreateJobObjectW, SetInformationJobObject, JobObjectExtendedLimitInformation,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        unsafe {
            let handle = CreateJobObjectW(None, None)
                .map_err(|e| format!("创建 Job Object 失败: {e}"))?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .map_err(|e| {
                let _ = windows::Win32::Foundation::CloseHandle(handle);
                format!("配置 Job Object 失败: {e}")
            })?;
            Ok(JobHandle { handle })
        }
    }

    pub fn assign(&self, child: &tokio::process::Child) -> Result<(), String> {
        use windows::Win32::System::JobObjects::AssignProcessToJobObject;
        let raw = child.raw_handle().ok_or("无法获取子进程句柄")?;
        let h = windows::Win32::Foundation::HANDLE(raw as *mut _);
        unsafe {
            AssignProcessToJobObject(self.handle, h)
                .map_err(|e| format!("加入 Job Object 失败: {e}"))
        }
    }
}

#[cfg(windows)]
impl Drop for JobHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

/// spawn 一个不弹控制台窗口、已挂入 Job 的子进程。
/// 返回 (child, ManagedChild)。ManagedChild Drop 时整树终止。
pub fn spawn_managed(
    mut cmd: tokio::process::Command,
) -> Result<(tokio::process::Child, ManagedChild), String> {
    #[cfg(windows)]
    {
        // tokio::process::Command 在 Windows 上自带 creation_flags 方法
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.kill_on_drop(true);
    let child = cmd
        .spawn()
        .map_err(|e| format!("启动子进程失败: {e}"))?;
    #[cfg(windows)]
    {
        let job = JobHandle::create()?;
        job.assign(&child)?;
        Ok((child, ManagedChild { _job: job }))
    }
    #[cfg(not(windows))]
    {
        Ok((child, ManagedChild {}))
    }
}

/// 保存 Job 句柄，Drop 时杀树（Windows）。
#[cfg(windows)]
pub struct ManagedChild {
    _job: JobHandle,
}

#[cfg(not(windows))]
pub struct ManagedChild {}

/// 构造 tokio Command 的便捷封装（接管 stdio）。
pub fn base_command(program: &std::path::Path) -> tokio::process::Command {
    use std::process::Stdio;
    let mut cmd = tokio::process::Command::new(program);
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    cmd
}
