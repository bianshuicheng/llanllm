# LLM 局域网分布式推理（主控 + 被控）

基于 llama.cpp RPC 后端的图形化客户端：把局域网内多台电脑的 GPU/内存拼起来运行单机装不下的大模型。
同一套代码构建两种角色（Tauri 2 + Vue 3 + Element Plus + SQLite）。

## 当前状态（2026-11-13）

**已完成并实测通过：**

| 模块 | 状态 | 说明 |
|---|---|---|
| 工程骨架 + 双角色构建 | ✅ | `cargo build`（主控）/ `cargo build --no-default-features --features agent`（被控），产物 `llanllm-master.exe` / `llanllm-agent.exe` |
| UDP 心跳广播/发现 | ✅ 实测 | 被控每 2s 广播（HMAC-SHA256 防伪造），主控 47890 监听并解析展示（主机名/GPU/显存/IP） |
| PIN 配对 | ✅ 实测 | 主控 6 位 PIN（5 分钟有效）→ X25519 ECDH + PIN 混入 HKDF → AES-256-GCM 双向认证 → 长期密钥 node_key DPAPI 加密落盘 |
| SQLite 持久化 | ✅ 实测 | 主控节点表 / 模型表 / 设置表；重启后自动恢复 |
| rpc-server 看护 | ✅ 实测 | 配对成功/重启后自动拉起 `ggml-rpc-server -H 0.0.0.0 -p 50052 -c`，崩溃自动重启，Job Object 保证随主程序退出 |
| llama-server 启停 | ✅ 实测 | 自动收集在线已配对节点拼 `--rpc` 列表，Job Object 管理，stdout/stderr 捕获 |
| 分布式推理 | ✅ 实测 | gemma-4-E4B (5GB, Q4_K_M) 权重按层推送至被控（rpc-cache 落盘 128 分片 2.6GB），`/v1/chat/completions` 出字 ~11.4 tok/s（CPU 回环） |
| 防火墙规则下发 | ✅ 代码完成 | 配对成功自动生成仅允许主控 IP 的 50052/47891 入站规则（UAC 提权，netsh）；主控放行 UDP 47890；UI 有未生效警告 |
| 模型管理 | ✅ 代码完成 | 文件选择器导入 + 路径粘贴导入（原地使用不搬移）、状态校验（missing/size_mismatch）、重新定位、删除（可选删文件） |
| 模型目录更改 | ✅ 代码完成 | 设置页更改 + 可选迁移文件（跨盘 copy+del 回退），DB 路径批量更新，可写性探测 |
| 缓存管理 | ✅ 代码完成 | 主控/被控/全部被控清理，跨端走加密控制通道（cache.info/cache.clear） |
| 控制通道（TCP 47891） | ✅ 协议完成 | 每连接临时 ECDH + node_key 绑定认证，方向分离密钥 + 序列号防重放；JSON-RPC 风格指令（status/rpc.start/rpc.stop/cache.* /firewall.setup） |
| HF 下载 | ⏳ 未开始 | 按计划最后做（镜像/断点续传/重试） |
| 导出被控端单 exe | ⏳ 未开始 | NSIS 注入配置方案已定（尾部标记） |
| 托盘/开机自启 | ⏳ 未开始 | |

**待用户回归的 UI 流程**（自动化测试因锁屏受阻）：路径导入→模型列表、推理服务页启动+日志面板+聊天窗、缓存清理按钮、更改模型目录迁移。

## 开发环境

- Rust 1.99 (stable-msvc)、Node 26、VS Build Tools 2022（本机已装）
- llama.cpp b12318 预编译包（CPU x64）已放入 `src-tauri/binaries/runtime/`（gitignore）

## 常用命令

```bash
npm install
npm run dev:web          # 前端 5173（主控与被控 dev 共用）
npm run dev:master       # tauri dev 主控
npm run dev:agent        # tauri dev 被控（端口 5174）
npm run build:master     # NSIS 主控安装包
npm run build:agent      # NSIS 被控安装包

# 联调（不经 tauri CLI，直接跑双 exe）：
# 1) npm run dev:web
# 2) cargo build && cargo build --no-default-features --features agent
# 3) 运行 target/debug/llanllm-master.exe 与 llanllm-agent.exe（都连 5173）
```

## 端口约定

| 端口 | 用途 | 防火墙 |
|---|---|---|
| UDP 47890 | 被控→主控 心跳广播 | 主控放行入站（专用/域网络） |
| TCP 47891 | 主控→被控 控制通道（配对+指令） | 被控放行入站，限主控 IP |
| TCP 50052 | llama.cpp RPC（权重分发+推理） | 被控放行入站，限主控 IP |
| TCP 8080 | llama-server OpenAI 兼容 API | 仅本机使用，不放行 |

## 安全设计

- RPC 50052 **无鉴权无加密**（llama.cpp 事实），只能靠防火墙白名单兜底 → UI 显著提示禁止公网。
- 心跳 HMAC 防伪造（内置产品级密钥，防噪声非强认证）。
- 配对：PIN 混入 ECDH 会话派生，错误 PIN 无法解密 PairProof。
- 控制会话：临时 ECDH（前向保密）+ 长期 node_key 认证；双向独立密钥 + 递增序列号 nonce 防重放。
- node_key 仅存被控端（DPAPI 用户作用域加密）；主控侧每节点密钥存 SQLite。

## 已知问题

1. **事件推送偶发不达前端**（nodes-changed 等）：原因待查（不影响功能，前端已全量轮询兜底）。
2. dev 模式两角色共用 tauri.conf.json → 被控窗口标题显示"主控台"（仅 dev；打包时各用各的 conf）。
3. UAC 弹窗需要用户在场；防火墙规则未生效时 UI 有持续警告。

## 目录结构

```
llanllm/
├── src/                    # Vue 前端（角色自适应）
│   ├── views/master/       # 节点/模型/推理服务/缓存/设置
│   └── views/agent/        # 被控状态+配对向导
├── src-tauri/
│   ├── src/common/         # crypto/protocol/db/proc(job object)/sysinfo/elevate
│   ├── src/master/         # discovery(UDP)/pairing/ctl(加密会话)/llama/models
│   ├── src/agent/          # heartbeat/control_server/rpc_runner/firewall
│   ├── binaries/runtime/   # llama.cpp 预编译二进制（gitignore）
│   └── tauri.conf.json / tauri.agent.conf.json
└── staging/                # 下载与日志暂存（gitignore）
```
