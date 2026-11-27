//! E2E：注入引导配置到被控端安装器 → 静默安装 → 验证 NSIS 钩子正确写出 config.json。
//!
//! 这是「导出被控端 → 新电脑安装 → 自动入网」链路的最关键一环：v0.1.0 里
//! NSIS 钩子的偏移量 bug 只有真实安装才能暴露，单测抓不住。
//!
//! 运行（CI 中设置 LLAN_AGENT_INSTALLER 指向构建好的被控端安装器）：
//!   cargo test --features master --test exported_installer

use llanllm_lib::common::bootstrap::{build_tail, parse_tail, BootstrapConfig};
use std::path::PathBuf;
use std::process::Command;

fn sample_cfg() -> BootstrapConfig {
    BootstrapConfig {
        v: 3,
        master_id: "ci-master-id".into(),
        master_name: "CI-主控-测试".into(),
        master_ip: "192.168.1.99".into(),
        token_id: "0123456789abcdef".into(),
        token: "00112233445566778899aabbccddeeff".into(),
        discovery_port: 47890,
        ctl_port: 47891,
        rpc_port: 50052,
        expires_at: 0,
    }
}

#[test]
fn installer_hook_writes_config() {
    let Ok(installer) = std::env::var("LLAN_AGENT_INSTALLER") else {
        eprintln!("跳过：未设置 LLAN_AGENT_INSTALLER（本机单测）");
        return;
    };
    let installer = PathBuf::from(installer);
    assert!(installer.exists(), "安装器模板不存在：{}", installer.display());

    // 1) 注入（与主控导出功能同一份 build_tail 代码）
    let blob = std::fs::read(&installer).expect("读取安装器");
    let cfg = sample_cfg();
    let blob = build_tail(blob, &cfg).expect("注入配置");
    let injected = std::env::temp_dir().join("llan-agent-injected-e2e.exe");
    std::fs::write(&injected, &blob).expect("写出注入后的安装器");
    // 自校验：注入后的 exe 尾部必须能被被控端的 parse_tail 读回
    assert_eq!(parse_tail(&std::fs::read(&injected).unwrap()).unwrap(), cfg);

    // 2) 静默安装（runner 是管理员；NSIS perMachine /S 免交互）
    let status = Command::new(&injected)
        .arg("/S")
        .status()
        .expect("运行安装器");
    assert!(status.success(), "静默安装失败：{:?}", status.code());

    // 3) 验证安装钩子把配置写了出来
    let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
    let inst_dir = PathBuf::from(&pf).join("LLM被控端");
    let config = inst_dir.join("config.json");
    let data = std::fs::read(&config)
        .unwrap_or_else(|e| panic!("安装钩子未写出 config.json（{}）：{e}", config.display()));
    let parsed: BootstrapConfig = match serde_json::from_slice(&data) {
        Ok(p) => p,
        Err(e) => {
            // 诊断转储：钩子写出内容的前 300 字节、注入文件尾部的 hex、钩子调试文件
            eprintln!("config.json 解析失败：{e}");
            eprintln!("config.json 前 300 字节：{:02x?}", &data[..data.len().min(300)]);
            eprintln!("config.json 原文：{:?}", String::from_utf8_lossy(&data[..data.len().min(300)]));
            if let Ok(blob) = std::fs::read(&injected) {
                let n = blob.len();
                eprintln!("注入文件尾部 64 字节：{:02x?}", &blob[n.saturating_sub(64)..]);
            }
            if let Ok(d) = std::fs::read_to_string(inst_dir.join("hook-debug.txt")) {
                eprintln!("hook-debug.txt：{d}");
            }
            if let Ok(d) = std::fs::read_to_string(inst_dir.join("config-ip.txt")) {
                eprintln!("config-ip.txt：{d:?}");
            }
            panic!("config.json 内容与预期不符");
        }
    };
    assert_eq!(parsed, cfg, "config.json 内容与注入配置不一致");
    let ip_file = std::fs::read_to_string(inst_dir.join("config-ip.txt")).expect("config-ip.txt");
    assert_eq!(ip_file.trim(), "192.168.1.99");
    // 运行时精简校验：rpc-server 在位、llama-server 不应存在
    assert!(inst_dir.join("runtime").join("ggml-rpc-server.exe").exists());
    assert!(!inst_dir.join("runtime").join("llama-server.exe").exists());
    // VC 运行库随包
    assert!(inst_dir.join("runtime").join("vcruntime140.dll").exists());

    // 4) 清理：静默卸载（PREUNINSTALL 钩子应删掉 config.json）。
    // NSIS 卸载器 /S 会重启自身（Au_.exe）并让父进程立即返回——删除是异步完成的，
    // 必须轮询等待，否则断言会偶发失败。
    let un = inst_dir.join("uninstall.exe");
    if un.exists() {
        let _ = Command::new(&un).arg("/S").status();
    }
    let mut gone = false;
    for _ in 0..40 {
        if !config.exists() {
            gone = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    assert!(
        gone,
        "卸载后 config.json 应被 PREUNINSTALL 钩子删除"
    );
    let _ = std::fs::remove_file(&injected);
}
