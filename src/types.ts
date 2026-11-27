export interface NodeView {
  id: string;
  hostname: string;
  ip: string;
  ips: string[];
  gpu: string | null;
  vram_total_mb: number | null;
  vram_free_mb: number | null;
  version: string;
  online: boolean;
  paired: boolean;
  ctl_port: number;
  rpc_port: number;
}

export interface ModelRow {
  id: string;
  name: string;
  path: string;
  size: number;
  mtime: number;
  source: "import" | "download" | string;
  quant: string | null;
  hf_repo: string | null;
  added_at: number;
  status: string; // ok | missing | size_mismatch | unknown
}

export interface LlamaStatus {
  running: boolean;
  port: number;
  model_name: string | null;
  model_path: string | null;
  uptime_s: number;
  ctx_size: number | null;
  ngl: number | null;
  rpc_targets: string[];
  logs: string[];
  base_url: string;
}

export interface MasterInfo {
  master_id: string;
  master_name: string;
  models_dir: string;
  cache_dir: string;
  cache_bytes: number;
  version: string;
  runtime_ok: boolean;
  runtime_message: string;
  local_ips: string[];
}

export interface AgentStatusView {
  id: string;
  hostname: string;
  ips: string[];
  gpu: string | null;
  vram_total_mb: number | null;
  vram_free_mb: number | null;
  paired: boolean;
  master_id: string | null;
  master_name: string | null;
  master_ip: string | null;
  rpc_running: boolean;
  rpc_pid: number | null;
  rpc_uptime_s: number;
  rpc_port: number;
  cache_bytes: number;
  cache_dir: string;
  logs: string[];
  rpc_logs: string[];
  version: string;
  firewall_ok: boolean;
  bootstrap_pending: boolean;
  runtime_ok: boolean;
  runtime_message: string;
}

export interface BootstrapToken {
  id: string;
  label: string;
  created_at: number;
  expires_at: number; // 0 = 长期有效
  use_count: number;
  last_used_at: number | null;
}

export interface PinInfo {
  pin: string;
  expires_in_s: number;
}

export function fmtBytes(n: number | null | undefined): string {
  if (n == null) return "-";
  if (n >= 1024 ** 3) return (n / 1024 ** 3).toFixed(2) + " GB";
  if (n >= 1024 ** 2) return (n / 1024 ** 2).toFixed(1) + " MB";
  if (n >= 1024) return (n / 1024).toFixed(1) + " KB";
  return n + " B";
}

export function fmtUptime(s: number): string {
  if (s < 60) return `${s} 秒`;
  if (s < 3600) return `${Math.floor(s / 60)} 分 ${s % 60} 秒`;
  return `${Math.floor(s / 3600)} 时 ${Math.floor((s % 3600) / 60)} 分`;
}
