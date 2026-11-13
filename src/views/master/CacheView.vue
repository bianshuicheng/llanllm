<script setup lang="ts">
import { onMounted, ref } from "vue";
import { ElMessage, ElMessageBox } from "element-plus";
import { api } from "../../api";
import type { NodeView } from "../../types";

const masterCache = ref<{ bytes: number; path: string; human: string } | null>(null);
const nodes = ref<NodeView[]>([]);
const nodeCache = ref<Record<string, string>>({});
const busy = ref<Record<string, boolean>>({});

async function refresh() {
  try {
    masterCache.value = await api.masterCacheInfo();
    nodes.value = (await api.getNodes()).filter((n) => n.paired);
  } catch (e) {
    ElMessage.error(`刷新失败：${e}`);
  }
}

async function refreshNodeCache(n: NodeView) {
  if (!n.online) {
    ElMessage.warning(`节点「${n.hostname}」离线，无法查询`);
    return;
  }
  busy.value = { ...busy.value, [n.id]: true };
  try {
    const res = (await api.nodeRequest(n.id, "cache.info", {})) as { bytes: number };
    nodeCache.value = {
      ...nodeCache.value,
      [n.id]: fmt(res.bytes),
    };
  } catch (e) {
    ElMessage.error(`查询「${n.hostname}」缓存失败：${e}`);
  } finally {
    busy.value = { ...busy.value, [n.id]: false };
  }
}

function fmt(n: number): string {
  if (n >= 1024 ** 3) return (n / 1024 ** 3).toFixed(2) + " GB";
  if (n >= 1024 ** 2) return (n / 1024 ** 2).toFixed(1) + " MB";
  return n + " B";
}

async function clearMaster() {
  try {
    await ElMessageBox.confirm(
      "清理主控本地 rpc 缓存目录？清理后下次启动模型需重新向被控端传输权重。",
      "清理主控缓存",
      { type: "warning" },
    );
    const msg = await api.clearMasterCache();
    ElMessage.success(msg);
    refresh();
  } catch (e) {
    if (e !== "cancel" && String(e) !== "cancel") ElMessage.error(String(e));
  }
}

async function clearNode(n: NodeView) {
  try {
    await ElMessageBox.confirm(
      `清理被控端「${n.hostname}」的 rpc 缓存？清理后下次启动需重新传输模型层。`,
      "清理被控缓存",
      { type: "warning" },
    );
    busy.value = { ...busy.value, [n.id]: true };
    const msg = await api.clearNodeCache(n.id);
    ElMessage.success(`「${n.hostname}」：${msg}`);
    nodeCache.value = { ...nodeCache.value, [n.id]: "0 B" };
  } catch (e) {
    if (String(e) !== "cancel") ElMessage.error(String(e));
  } finally {
    busy.value = { ...busy.value, [n.id]: false };
  }
}

async function clearAllNodes() {
  try {
    await ElMessageBox.confirm(
      "清理全部已配对被控端的 rpc 缓存？",
      "清理全部被控缓存",
      { type: "warning" },
    );
    for (const n of nodes.value.filter((x) => x.online)) {
      try {
        await api.clearNodeCache(n.id);
      } catch (e) {
        ElMessage.error(`「${n.hostname}」清理失败：${e}`);
      }
    }
    ElMessage.success("全部在线被控端缓存已清理");
    refresh();
  } catch {
    /* 取消 */
  }
}

async function exportAgent() {
  try {
    const msg = await api.exportAgentInstaller();
    ElMessageBox.alert(msg, "导出成功", { confirmButtonText: "好的" }).catch(() => {});
  } catch (e) {
    if (String(e) !== "已取消") ElMessage.error(String(e));
  }
}

onMounted(refresh);
</script>

<template>
  <div class="page">
    <el-alert
      type="warning"
      :closable="false"
      title="清理缓存后，下次启动模型时权重需重新通过网络传输到各节点（首次启动会明显变慢）。"
      style="margin-bottom: 14px"
    />

    <el-row :gutter="16">
      <el-col :span="10">
        <el-card shadow="never">
          <template #header>主控本地缓存</template>
          <div class="cache-path">{{ masterCache?.path || "-" }}</div>
          <div class="cache-size">占用：{{ masterCache?.human || "-" }}</div>
          <div style="margin-top: 12px">
            <el-button type="danger" plain @click="clearMaster">清理主控缓存</el-button>
            <el-button @click="refresh">刷新</el-button>
          </div>
        </el-card>
      </el-col>
      <el-col :span="14">
        <el-card shadow="never">
          <template #header>
            <div class="head">
              <span>被控端缓存</span>
              <el-button size="small" @click="clearAllNodes" :disabled="!nodes.some((n) => n.online)">
                清理全部在线被控
              </el-button>
            </div>
          </template>
          <el-table :data="nodes" size="small">
            <el-table-column prop="hostname" label="节点" min-width="120" />
            <el-table-column label="状态" width="80">
              <template #default="{ row }">
                <span :class="['node-dot', row.online ? 'online' : 'offline']" />
                {{ row.online ? "在线" : "离线" }}
              </template>
            </el-table-column>
            <el-table-column label="缓存占用" min-width="110">
              <template #default="{ row }">
                {{ nodeCache[row.id] || "未查询" }}
              </template>
            </el-table-column>
            <el-table-column label="操作" width="170">
              <template #default="{ row }">
                <el-button
                  size="small"
                  :loading="busy[row.id]"
                  :disabled="!row.online"
                  @click="refreshNodeCache(row)"
                >
                  查询
                </el-button>
                <el-button
                  size="small"
                  type="danger"
                  plain
                  :disabled="!row.online || busy[row.id]"
                  @click="clearNode(row)"
                >
                  清理
                </el-button>
              </template>
            </el-table-column>
          </el-table>
        </el-card>
      </el-col>
    </el-row>

    <el-card shadow="never" style="margin-top: 16px">
      <template #header>导出被控端</template>
      <p class="desc">
        生成一个「被控端安装包」，拷贝到其他电脑双击安装即可自动入网（预注入主控地址与引导密钥）。
      </p>
      <el-button type="primary" @click="exportAgent">导出被控端安装包</el-button>
    </el-card>
  </div>
</template>

<style scoped>
.cache-path {
  font-family: Consolas, monospace;
  font-size: 12px;
  color: var(--el-text-color-secondary);
  word-break: break-all;
}
.cache-size {
  font-size: 15px;
  margin-top: 6px;
}
.head {
  display: flex;
  justify-content: space-between;
  align-items: center;
}
.desc {
  color: var(--el-text-color-secondary);
  font-size: 13px;
}
</style>
