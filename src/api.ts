import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AgentStatusView,
  BootstrapToken,
  LlamaStatus,
  MasterInfo,
  ModelRow,
  NodeView,
  PinInfo,
} from "./types";

export const api = {
  getRole: () => invoke<string>("get_role"),

  // —— 主控 ——
  getMasterInfo: () => invoke<MasterInfo>("get_master_info"),
  getNodes: () => invoke<NodeView[]>("get_nodes"),
  createPairPin: (nodeId: string) =>
    invoke<PinInfo>("create_pair_pin", { nodeId }),
  cancelPairPin: () => invoke<void>("cancel_pair_pin"),
  removeNode: (nodeId: string) => invoke<string>("remove_node", { nodeId }),
  importModelDialog: () => invoke<ModelRow>("import_model_dialog"),
  importModel: (path: string) => invoke<ModelRow>("import_model", { path }),
  listModels: () => invoke<ModelRow[]>("list_models"),
  relocateModelDialog: (id: string) => invoke<ModelRow>("relocate_model_dialog", { id }),
  relocateModel: (id: string, newPath: string) =>
    invoke<ModelRow>("relocate_model", { id, newPath }),
  removeModel: (id: string, deleteFile: boolean) =>
    invoke<string>("remove_model", { id, deleteFile }),
  getModelsDir: () => invoke<string>("get_models_dir"),
  setModelsDir: (newDir: string, moveFiles: boolean) =>
    invoke<{ moved: number; dir: string; message: string }>("set_models_dir", {
      newDir,
      moveFiles,
    }),
  openDir: (path: string) => invoke<void>("open_dir", { path }),
  llamaStart: (modelId: string, ctxSize: number, ngl: number, port: number) =>
    invoke<LlamaStatus>("llama_start", { modelId, ctxSize, ngl, port }),
  llamaStop: () => invoke<LlamaStatus>("llama_stop"),
  llamaStatus: () => invoke<LlamaStatus>("llama_status"),
  nodeRequest: (nodeId: string, method: string, params: object) =>
    invoke<unknown>("node_request", { nodeId, method, params }),
  clearNodeCache: (nodeId: string) => invoke<string>("clear_node_cache", { nodeId }),
  clearMasterCache: () => invoke<string>("clear_master_cache"),
  masterCacheInfo: () => invoke<{ bytes: number; path: string; human: string }>("master_cache_info"),
  allowDiscoveryFirewall: () => invoke<string>("allow_discovery_firewall"),
  exportAgentInstaller: (label?: string) =>
    invoke<string>("export_agent_installer", { label: label ?? null }),
  listBootstrapTokens: () => invoke<BootstrapToken[]>("list_bootstrap_tokens"),
  revokeBootstrapToken: (id: string) => invoke<string>("revoke_bootstrap_token", { id }),

  // —— HF 下载 ——
  hfListFiles: (repoId: string) =>
    invoke<{ path: string; size: number }[]>("hf_list_files", { repoId }),
  hfDownload: (repoId: string, files: { path: string; size: number }[]) =>
    invoke<string>("hf_download", { repoId, files }),
  hfCancel: () => invoke<void>("hf_cancel"),
  hfStatus: () =>
    invoke<{
      repo: string;
      file: string;
      file_index: number;
      file_total: number;
      bytes_done: number;
      bytes_total: number;
      speed_bps: number;
      eta_s: number | null;
      state: string;
      message: string | null;
    } | null>("hf_status"),
  getHfMirror: () => invoke<string>("get_hf_mirror"),
  setHfMirror: (mirror: string) => invoke<string>("set_hf_mirror", { mirror }),

  // —— 被控 ——
  agentStatus: () => invoke<AgentStatusView>("agent_status"),
  submitPairPin: (pin: string) => invoke<string>("submit_pair_pin", { pin }),
  unpair: () => invoke<string>("unpair"),
  rpcRestart: () => invoke<string>("rpc_restart"),
  rpcStop: () => invoke<string>("rpc_stop"),
  clearAgentCache: () => invoke<string>("clear_agent_cache"),
  openAgentCacheDir: () => invoke<void>("open_agent_cache_dir"),
  setAgentRpcPort: (port: number) => invoke<string>("set_agent_rpc_port", { port }),
};

export function listenEvent<T>(name: string, cb: (payload: T) => void): Promise<UnlistenFn> {
  return listen<T>(name, (e) => cb(e.payload));
}
