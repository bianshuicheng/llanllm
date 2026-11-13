<script setup lang="ts">
import { onMounted, onUnmounted, ref } from "vue";
import { ElMessage, ElMessageBox } from "element-plus";
import { api, listenEvent } from "../../api";
import type { NodeView, PinInfo } from "../../types";

const nodes = ref<NodeView[]>([]);
const loading = ref(false);
const discoveryError = ref("");

// 配对对话框
const pairDialog = ref(false);
const pin = ref("");
const pairMsg = ref("");
const pairingNodeId = ref("");

let unlisteners: Array<() => void> = [];
let pollTimer: number | undefined;

async function refresh(silent = false) {
  if (!silent) loading.value = true;
  try {
    nodes.value = await api.getNodes();
    // 配对弹窗打开时轮询到目标已配对 → 自动收尾（事件不可靠时的兜底）
    if (pairDialog.value && pairingNodeId.value) {
      const t = nodes.value.find((n) => n.id === pairingNodeId.value);
      if (t?.paired) {
        ElMessage.success(`已与「${t.hostname}」完成配对`);
        pairDialog.value = false;
        pairingNodeId.value = "";
      }
    }
  } catch (e) {
    if (!silent) ElMessage.error(`获取节点失败：${e}`);
  } finally {
    if (!silent) loading.value = false;
  }
}

async function startPair(node: NodeView) {
  try {
    const info: PinInfo = await api.createPairPin(node.id);
    pin.value = info.pin;
    pairingNodeId.value = node.id;
    pairMsg.value = `请在「${node.hostname}」的被控端窗口输入此 PIN（5 分钟内有效）`;
    pairDialog.value = true;
  } catch (e) {
    ElMessage.error(`发起配对失败：${e}`);
  }
}

function cancelPair() {
  api.cancelPairPin().catch(() => {});
  pairDialog.value = false;
}

async function removeNode(node: NodeView) {
  try {
    await ElMessageBox.confirm(
      `确定删除「${node.hostname}」的配对记录？被控端将不再信任本主控（可在被控端解除配对）。`,
      "删除节点",
      { type: "warning" },
    );
    const msg = await api.removeNode(node.id);
    ElMessage.success(msg);
    refresh();
  } catch {
    /* 取消 */
  }
}

async function allowFirewall() {
  try {
    const msg = await api.allowDiscoveryFirewall();
    ElMessage.success(msg);
  } catch (e) {
    ElMessage.error(String(e));
  }
}

onMounted(async () => {
  await refresh();
  // 轮询兜底（事件在某些环境下不可靠）
  pollTimer = window.setInterval(() => refresh(true), 2000);
  unlisteners.push(
    await listenEvent<NodeView[]>("nodes-changed", (list) => {
      nodes.value = list;
    }),
  );
  unlisteners.push(
    await listenEvent<{ state: string; message: string }>("pair-progress", (p) => {
      pairMsg.value = p.message;
    }),
  );
  unlisteners.push(
    await listenEvent<{ message: string }>("pair-success", (p) => {
      ElMessage.success(p.message);
      pairDialog.value = false;
      refresh();
    }),
  );
  unlisteners.push(
    await listenEvent<string>("discovery-error", (msg) => {
      discoveryError.value = msg;
    }),
  );
});

onUnmounted(() => {
  if (pollTimer) window.clearInterval(pollTimer);
  unlisteners.forEach((u) => u());
  unlisteners = [];
});

function vramText(n: NodeView): string {
  if (!n.vram_total_mb) return "-";
  const free = n.vram_free_mb != null ? ` / 空闲 ${n.vram_free_mb} MB` : "";
  return `${n.vram_total_mb} MB${free}`;
}
</script>

<template>
  <div class="page">
    <el-alert
      type="warning"
      :closable="false"
      title="安全提示：本工具仅限家庭/办公局域网内使用，严禁将相关端口暴露到公网（无鉴权协议，存在安全风险）。"
      style="margin-bottom: 12px"
    />

    <el-alert
      v-if="discoveryError"
      type="error"
      :closable="false"
      :title="discoveryError"
      style="margin-bottom: 12px"
    />

    <div class="toolbar">
      <el-button type="primary" @click="refresh" :loading="loading">刷新列表</el-button>
      <el-button @click="allowFirewall">重试放行主控端口（防火墙）</el-button>
      <span class="hint">被控端每 2 秒广播心跳，收到主控存在应答后会主动连接本机完成配对/控制。</span>
    </div>

    <el-table :data="nodes" style="width: 100%" v-loading="loading">
      <el-table-column label="状态" width="90">
        <template #default="{ row }">
          <span :class="['node-dot', row.online ? 'online' : 'offline']" />
          {{ row.online ? "在线" : "离线" }}
        </template>
      </el-table-column>
      <el-table-column prop="hostname" label="主机名" min-width="140" />
      <el-table-column prop="ip" label="IP" width="140" />
      <el-table-column label="GPU" min-width="180">
        <template #default="{ row }">{{ row.gpu || "未检测到" }}</template>
      </el-table-column>
      <el-table-column label="显存" min-width="160">
        <template #default="{ row }">{{ vramText(row) }}</template>
      </el-table-column>
      <el-table-column prop="version" label="版本" width="90" />
      <el-table-column label="配对" width="100">
        <template #default="{ row }">
          <el-tag v-if="row.paired" type="success">已配对</el-tag>
          <el-tag v-else type="info">未配对</el-tag>
        </template>
      </el-table-column>
      <el-table-column label="操作" width="160" fixed="right">
        <template #default="{ row }">
          <el-button
            v-if="!row.paired"
            size="small"
            type="primary"
            :disabled="!row.online"
            @click="startPair(row)"
          >
            配对
          </el-button>
          <el-button v-if="row.paired" size="small" type="danger" plain @click="removeNode(row)">
            删除
          </el-button>
        </template>
      </el-table-column>
    </el-table>

    <el-dialog v-model="pairDialog" title="配对进行中" width="440px" :close-on-click-modal="false">
      <div style="text-align: center">
        <div style="font-size: 40px; font-weight: 700; letter-spacing: 12px">{{ pin }}</div>
        <p style="color: var(--el-text-color-secondary); font-size: 13px">{{ pairMsg }}</p>
      </div>
      <template #footer>
        <el-button @click="cancelPair">取消配对</el-button>
      </template>
    </el-dialog>
  </div>
</template>

<style scoped>
.toolbar {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 12px;
  flex-wrap: wrap;
}
.hint {
  color: var(--el-text-color-secondary);
  font-size: 12px;
}
</style>
