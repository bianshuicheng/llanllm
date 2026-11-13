#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// 主控端可执行入口（bin: llanllm-master，required-features = master）

fn main() {
    llanllm_lib::master::run();
}
