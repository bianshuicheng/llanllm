#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// 被控端可执行入口（bin: llanllm-agent，required-features = agent）

fn main() {
    llanllm_lib::agent::run();
}
