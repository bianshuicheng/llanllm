//! 用真实抓包数据复现主控的解析 + 验签路径（回归用）。
//! 运行：cargo test --test hb_verify

use llanllm_lib::common::protocol::{verify_heartbeat, HeartbeatSigned};

#[test]
fn verify_captured_heartbeat() {
    let path = r"C:\Users\bianc\AppData\Local\Temp\llan-setup\hb-raw.json";
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(_) => {
            eprintln!("跳过：抓包文件不存在（{path}）");
            return;
        }
    };
    println!("raw: {}", String::from_utf8_lossy(&data));
    let signed: HeartbeatSigned = serde_json::from_slice(&data).expect("反序列化失败");
    println!("parsed: id={} hostname={}", signed.base.id, signed.base.hostname);
    assert_eq!(signed.base.kind, "hb");
    assert_eq!(signed.base.v, 1);
    assert!(verify_heartbeat(&signed), "HMAC 验签失败！");
    let now = chrono::Utc::now().timestamp();
    let delta = signed.base.ts - now;
    println!("ts delta: {delta}（fixture 为历史抓包，仅打印不作断言）");
    println!("协议检查通过：主控侧解析+验签逻辑应接受此心跳");
}
