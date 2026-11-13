<script setup lang="ts">
import { onMounted, onUnmounted, ref } from "vue";
import { ElMessage, ElMessageBox } from "element-plus";
import { api } from "../../api";
import { fmtBytes, type ModelRow } from "../../types";

const models = ref<ModelRow[]>([]);
const modelsDir = ref("");
const loading = ref(false);
const pathInput = ref("");
const importing = ref(false);

// —— HF 下载 ——
interface HfFile {
  path: string;
  size: number;
}
interface HfProgress {
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
}
const repoInput = ref("");
const hfFiles = ref<HfFile[]>([]);
const hfSelected = ref<string[]>([]);
const hfParsing = ref(false);
const hfProgress = ref<HfProgress | null>(null);
let hfTimer: number | undefined;

async function parseRepo() {
  const repo = repoInput.value.trim();
  if (!repo) {
    ElMessage.warning("请输入仓库 ID（如 Qwen/Qwen3-4B-GGUF）");
    return;
  }
  hfParsing.value = true;
  hfFiles.value = [];
  hfSelected.value = [];
  try {
    hfFiles.value = await api.hfListFiles(repo);
    hfSelected.value = hfFiles.value.map((f) => f.path);
  } catch (e) {
    ElMessage.error(String(e));
  } finally {
    hfParsing.value = false;
  }
}

async function startDownload() {
  if (!hfSelected.value.length) {
    ElMessage.warning("请选择要下载的文件");
    return;
  }
  const files = hfFiles.value.filter((f) => hfSelected.value.includes(f.path));
  try {
    await ElMessageBox.confirm(
      `将从 HF 下载 ${files.length} 个文件（共 ${fmtBytes(files.reduce((s, f) => s + f.size, 0))}）到模型目录 ${modelsDir.value}`,
      "开始下载",
      { confirmButtonText: "开始", cancelButtonText: "取消" },
    );
    await api.hfDownload(repoInput.value.trim(), files);
    ElMessage.success("下载已开始");
  } catch (e) {
    if (e !== "cancel" && String(e) !== "cancel") ElMessage.error(String(e));
  }
}

async function pollHf() {
  try {
    hfProgress.value = await api.hfStatus();
  } catch {
    /* ignore */
  }
}

async function cancelDownload() {
  api.hfCancel().catch(() => {});
}

function hfStateText(s: string): string {
  const map: Record<string, string> = {
    downloading: "下载中",
    retrying: "重试中",
    done: "已完成",
    failed: "失败",
    cancelled: "已取消",
    preparing: "准备中",
  };
  return map[s] || s;
}

async function importByPath() {
  const p = pathInput.value.trim().replace(/^"|"$/g, "");
  if (!p) {
    ElMessage.warning("请粘贴 .gguf 文件的完整路径");
    return;
  }
  importing.value = true;
  try {
    const row = await api.importModel(p);
    ElMessage.success(`已导入：${row.name}`);
    pathInput.value = "";
    refresh();
  } catch (e) {
    ElMessage.error(`导入失败：${e}`);
  } finally {
    importing.value = false;
  }
}

async function refresh() {
  loading.value = true;
  try {
    models.value = await api.listModels();
    modelsDir.value = await api.getModelsDir();
  } catch (e) {
    ElMessage.error(`获取模型列表失败：${e}`);
  } finally {
    loading.value = false;
  }
}

async function importModel() {
  try {
    const row = await api.importModelDialog();
    ElMessage.success(`已导入：${row.name}`);
    refresh();
  } catch (e) {
    if (!String(e).includes("已取消")) ElMessage.error(`导入失败：${e}`);
  }
}

async function relocate(row: ModelRow) {
  try {
    await api.relocateModelDialog(row.id);
    ElMessage.success("已更新路径");
    refresh();
  } catch (e) {
    if (!String(e).includes("已取消")) ElMessage.error(`重新定位失败：${e}`);
  }
}

async function removeModel(row: ModelRow) {
  try {
    // confirm=同时删除文件 / cancel=仅删记录 / close=放弃
    const action = await ElMessageBox.confirm(
      `删除条目「${row.name}」？选择按钮决定是否同时删除磁盘文件。`,
      "删除模型",
      {
        type: "warning",
        confirmButtonText: "同时删除文件",
        cancelButtonText: "仅删除记录",
        distinguishCancelAndClose: true,
      },
    )
      .then(() => "confirm")
      .catch((act) => act as string);
    if (action === "close") return;
    const msg = await api.removeModel(row.id, action === "confirm");
    ElMessage.success(msg);
    refresh();
  } catch (e) {
    ElMessage.error(`删除失败：${e}`);
  }
}

async function openDir() {
  try {
    await api.openDir(modelsDir.value);
  } catch (e) {
    ElMessage.error(String(e));
  }
}

onMounted(() => {
  refresh();
  hfTimer = window.setInterval(pollHf, 1000);
});

onUnmounted(() => {
  if (hfTimer) window.clearInterval(hfTimer);
});
</script>

<template>
  <div class="page">
    <div class="toolbar">
      <el-button type="primary" @click="importModel">导入模型（原地使用）</el-button>
      <el-button @click="refresh" :loading="loading">刷新</el-button>
      <div class="dir-info">
        当前模型目录：
        <span class="dir-path">{{ modelsDir || "-" }}</span>
        <el-button size="small" text type="primary" @click="openDir">打开目录</el-button>
        <span class="hint">（更改目录位置请到「设置」页）</span>
      </div>
    </div>
    <div class="toolbar">
      <el-input
        v-model="pathInput"
        placeholder="或粘贴 .gguf 文件完整路径导入（如 E:\models\xxx.gguf）"
        style="flex: 1; min-width: 380px"
        @keyup.enter="importByPath"
      />
      <el-button :loading="importing" @click="importByPath">路径导入</el-button>
    </div>

    <el-table :data="models" style="width: 100%" v-loading="loading">
      <el-table-column prop="name" label="名称" min-width="200" />
      <el-table-column label="来源" width="100">
        <template #default="{ row }">
          <el-tag :type="row.source === 'import' ? 'primary' : 'success'">
            {{ row.source === "import" ? "本地导入" : "下载" }}
          </el-tag>
        </template>
      </el-table-column>
      <el-table-column label="大小" width="110">
        <template #default="{ row }">{{ fmtBytes(row.size) }}</template>
      </el-table-column>
      <el-table-column prop="path" label="完整路径" min-width="300" show-overflow-tooltip />
      <el-table-column label="状态" width="110">
        <template #default="{ row }">
          <el-tag v-if="row.status === 'ok'" type="success">可用</el-tag>
          <el-tag v-else-if="row.status === 'missing'" type="danger">文件不可用</el-tag>
          <el-tag v-else-if="row.status === 'size_mismatch'" type="warning">文件已变化</el-tag>
          <el-tag v-else type="info">{{ row.status }}</el-tag>
        </template>
      </el-table-column>
      <el-table-column label="操作" width="170" fixed="right">
        <template #default="{ row }">
          <el-button
            v-if="row.status !== 'ok'"
            size="small"
            type="warning"
            plain
            @click="relocate(row)"
          >
            重新定位
          </el-button>
          <el-button size="small" type="danger" plain @click="removeModel(row)">删除</el-button>
        </template>
      </el-table-column>
    </el-table>

    <el-alert
      class="security-note"
      type="info"
      :closable="false"
      title="导入的模型保留在原位置（含移动硬盘/网络盘），不会复制搬移；启动推理前会自动校验文件是否存在。"
    />

    <el-card shadow="never" style="margin-top: 16px">
      <template #header>
        <div class="card-head">
          <span>从 HuggingFace 下载</span>
          <span class="hint">默认保存到当前模型目录；国内网络建议在「设置」切换 hf-mirror.com 镜像</span>
        </div>
      </template>

      <div style="display: flex; gap: 8px; margin-bottom: 10px">
        <el-input
          v-model="repoInput"
          placeholder="仓库 ID 或完整网址，如 Qwen/Qwen3-4B-GGUF"
          style="flex: 1"
          :disabled="!!hfProgress && ['downloading', 'retrying'].includes(hfProgress.state)"
          @keyup.enter="parseRepo"
        />
        <el-button :loading="hfParsing" @click="parseRepo">解析文件列表</el-button>
      </div>

      <el-table v-if="hfFiles.length" :data="hfFiles" size="small" max-height="220">
        <el-table-column width="42">
          <template #header>
            <el-checkbox
              :model-value="hfSelected.length === hfFiles.length"
              @change="hfSelected = $event ? hfFiles.map((f) => f.path) : []"
            />
          </template>
          <template #default="{ row }">
            <el-checkbox-group v-model="hfSelected">
              <el-checkbox :value="row.path" />
            </el-checkbox-group>
          </template>
        </el-table-column>
        <el-table-column prop="path" label="文件" min-width="320" show-overflow-tooltip />
        <el-table-column label="大小" width="110">
          <template #default="{ row }">{{ fmtBytes(row.size) }}</template>
        </el-table-column>
      </el-table>

      <div
        v-if="hfFiles.length"
        style="display: flex; gap: 10px; align-items: center; margin-top: 10px"
      >
        <el-button
          type="primary"
          :disabled="!!hfProgress && ['downloading', 'retrying'].includes(hfProgress.state)"
          @click="startDownload"
        >
          下载所选
        </el-button>
        <span class="hint">已选 {{ hfSelected.length }} / {{ hfFiles.length }} 个文件</span>
      </div>

      <div v-if="hfProgress" style="margin-top: 12px">
        <div class="hint" style="margin-bottom: 6px">
          {{ hfStateText(hfProgress.state) }} · {{ hfProgress.file }}（第
          {{ hfProgress.file_index }}/{{ hfProgress.file_total }} 个）
          <span v-if="hfProgress.speed_bps">
            · {{ fmtBytes(hfProgress.speed_bps) }}/s
            · 已下载 {{ fmtBytes(hfProgress.bytes_done) }} / {{ fmtBytes(hfProgress.bytes_total) }}
            <template v-if="hfProgress.eta_s">· 剩余约 {{ hfProgress.eta_s }}s</template>
          </span>
          <span v-if="hfProgress.message"> · {{ hfProgress.message }}</span>
        </div>
        <el-progress
          :percentage="
            hfProgress.bytes_total > 0
              ? Math.min(100, Math.round((hfProgress.bytes_done / hfProgress.bytes_total) * 100))
              : 0
          "
          :status="hfProgress.state === 'failed' ? 'exception' : hfProgress.state === 'done' ? 'success' : undefined"
        />
        <el-button
          v-if="['downloading', 'retrying'].includes(hfProgress.state)"
          size="small"
          type="danger"
          plain
          style="margin-top: 8px"
          @click="cancelDownload"
        >
          取消下载
        </el-button>
      </div>
    </el-card>
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
.dir-info {
  color: var(--el-text-color-secondary);
  font-size: 13px;
}
.dir-path {
  color: var(--el-text-color-primary);
  font-family: Consolas, monospace;
  font-size: 12px;
}
.hint {
  font-size: 12px;
}
</style>
