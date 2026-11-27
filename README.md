# LLM 局域网分布式推理（主控 + 被控）

基于 llama.cpp RPC 后端的图形化客户端：把局域网内多台电脑的 GPU/内存拼起来运行单机装不下的大模型。
同一套代码构建两种角色（Tauri 2 + Vue 3 + Element Plus + SQLite）。普通用户全程图形化操作，零命令行。

## 发布物

**只发布主控台安装包（NSIS 单 exe）**。被控端不单独发布——在主控台
「缓存与维护 → 导出被控端安装包」生成自带主控地址与长期引导码的被控端安装包，
拷到其他电脑双击安装，装完启动即自动入网。

主控安装包内置：

- llama.cpp 运行时（llama-server / ggml-rpc-server + 全部依赖 DLL）
- **VC++ 运行库**（MSVCP140 / VCRUNTIME140 / VCRUNTIME140_1）——干净系统无需另行安装
- **WebView2 引导程序**（embedBootstrapper）——离线机器也能装
- 中文安装界面；安装时自动配置防火墙（全部网络类型，入站限本网段）

## 架构（连接方向）

```
被控 ──UDP 47890 心跳广播（多网段定向）──▶ 主控
被控 ◀─UDP 单播 presence 应答───────────── 主控
被控 ──TCP 47891 主动连接（控制会话）─────▶ 主控
主控 ──TCP 50052 RPC 拨号（权重分发/推理）─▶ 被控
```

被控端除 RPC 端口外**零入站依赖**（出站广播/连接不需要防火墙配置）；
RPC 50052 是 llama.cpp 硬约束（主控必须拨号被控），由安装器自动写
「仅允许主控 IP、全部网络类型」的防火墙规则兜底。

## 开发

```bash
npm install
npm run dev:web          # 前端 5173（主控与被控 dev 共用）
npm run dev:master       # tauri dev 主控
npm run dev:agent        # tauri dev 被控（端口 5174）
npm run build:master     # NSIS 主控安装包
npm run build:agent      # NSIS 被控安装包
```

- 运行时二进制（gitignore）：`src-tauri/binaries/runtime-master/`（llama-server 集 +
  VC DLL）、`src-tauri/binaries/runtime-agent/`（rpc-server 集 + VC DLL）；
  CI 自动从 llama.cpp b12318 预编译包裁剪组装，本地开发可手工放置
  （参考 `.github/workflows/release.yml` 的组装步骤）。
- CI（GitHub Actions，tag `v*` 触发）跑双角色单测 + **真实安装器 E2E**
  （注入引导配置 → /S 静默安装 → 验证 NSIS 钩子写出的 config.json → 卸载清理）
  → 构建主控安装包（内嵌被控安装器模板）→ 发布 Release（仅主控安装包一个资产）。

## 关键机制

| 机制 | 说明 |
|---|---|
| 引导码注入 v3 | 配置 JSON + 主控 IP + 定长 18 字节尾（`json_len(6)ip_len(4)magic(8)`，magic 在最末）追加到安装器尾部；NSIS 钩子从末尾倒读，无偏移推算。单测 `bootstrap::tests::tail_layout_matches_nsis_contract` 钉死契约 |
| 引导码生命周期 | 128 位随机数，SQLite 持久化，**长期有效**（主控重启不失效），可在「导出记录」作废；使用次数/最近使用时间有记录 |
| 配对凭证 | 引导码（安装包注入）或 6 位 PIN（手动）走同一 AEAD 证明通道：X25519 ECDH + 凭证混入 HKDF → AES-256-GCM，错凭证解不开证明帧 |
| 会话密钥 | 每连接临时 ECDH（前向保密）+ 长期 node_key 认证；方向分离密钥 + 序列号 nonce 防重放；node_key 仅存被控端（DPAPI 用户作用域） |
| RPC 目标 | 主控启动推理前对每个节点发 `rpc.targets` 询问，用控制会话**真实对端 IP** 拨号（多网卡机器上比心跳 IP 列表可靠） |
| 运行时自检 | 启动推理前跑 `llama-server --version`；缺 VC 运行库等失败给出明确中文原因 + 恢复建议 |
| 子进程管理 | Job Object：主程序退出整树终止，不留僵尸 rpc-server/llama-server |

## 端口约定

| 端口 | 用途 | 防火墙 |
|---|---|---|
| UDP 47890 | 被控→主控 心跳广播 | 主控放行入站（全部网络类型，限本网段） |
| TCP 47891 | 被控→主控 控制通道（配对+指令） | 主控放行入站（全部网络类型，限本网段） |
| TCP 50052 | 主控→被控 llama.cpp RPC | 被控放行入站，**仅允许主控 IP**（安装器自动） |
| TCP 8080 | llama-server OpenAI 兼容 API | 仅本机，不放行 |

## 安全设计

- RPC 50052 无鉴权无加密（llama.cpp 事实），只能靠防火墙白名单兜底 → UI 显著提示禁止公网
- 心跳 HMAC 防伪造（内置产品级密钥，防噪声非强认证）
- 控制会话 AEAD；引导码可作废；卸载清理防火墙规则与配置

## 已知事项

1. 事件推送偶发不达前端：前端已全量轮询兜底（不影响功能）
2. UAC 弹窗需要用户在场（主控首次启动自动放行防火墙、被控运行时补规则）
3. 防火墙状态判定依赖 netsh 查询输出格式（Windows 大版本变化时可能需要适配）
