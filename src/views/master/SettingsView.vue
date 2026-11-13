<script setup lang="ts">
import { onMounted, ref } from "vue";
import { ElMessage, ElMessageBox } from "element-plus";
import { api } from "../../api";

const info = ref<{
  master_id: string;
  master_name: string;
  models_dir: string;
  cache_dir: string;
  cache_bytes: number;
  version: string;
} | null>(null);
const dark = ref(false);
const hfMirror = ref("https://huggingface.co");

const MIRROR_PRESETS = [
  { label: "官方 huggingface.co", value: "https://huggingface.co" },
  { label: "国内镜像 hf-mirror.com（推荐）", value: "https://hf-mirror.com" },
];

async function saveMirror(v: string) {
  try {
    const msg = await api.setHfMirror(v);
    hfMirror.value = v;
    ElMessage.success(msg);
  } catch (e) {
    ElMessage.error(String(e));
  }
}

async function refresh() {
  try {
    info.value = await api.getMasterInfo();
  } catch (e) {
    ElMessage.error(`获取设置失败：${e}`);
  }
}

async function changeModelsDir() {
  if (!info.value) return;
  try {
    const { value } = await ElMessageBox.prompt(
      "输入新的模型目录绝对路径（例如 D:\\LLM\\models）。稍后会询问是否迁移现有文件。",
      "更改模型目录",
      { inputValue: info.value.models_dir, inputPattern: /\S+/, inputErrorMessage: "路径不能为空" },
    );
    const newDir = value.trim();
    if (newDir === info.value.models_dir) {
      ElMessage.info("路径未变化");
      return;
    }
    const moveChoice = await ElMessageBox.confirm(
      `是否把旧目录（${info.value.models_dir}）内的模型文件移动到新位置？\n\n「移动文件」= 文件物理迁移 + 记录路径全部更新；「取消/关闭」= 仅更改默认目录，不动现有文件。`,
      "迁移选项",
      {
        confirmButtonText: "移动文件",
        cancelButtonText: "仅改目录不移动",
        distinguishCancelAndClose: true,
        type: "warning",
      },
    )
      .then(() => true)
      .catch((act) => (act === "cancel" ? false : null));
    if (moveChoice === null) return;
    const res = await api.setModelsDir(newDir, moveChoice);
    ElMessage.success(res.message + (moveChoice ? "，服务可正常使用新路径" : ""));
    refresh();
  } catch (e) {
    if (String(e) !== "cancel" && String(e) !== "close") ElMessage.error(String(e));
  }
}

function toggleDark(v: boolean) {
  dark.value = v;
  document.documentElement.classList.toggle("dark", v);
  localStorage.setItem("llanllm-dark", v ? "1" : "0");
}

onMounted(async () => {
  refresh();
  dark.value = localStorage.getItem("llanllm-dark") === "1";
  document.documentElement.classList.toggle("dark", dark.value);
  try {
    hfMirror.value = await api.getHfMirror();
  } catch {
    /* ignore */
  }
});
</script>

<template>
  <div class="page" style="max-width: 860px">
    <el-card shadow="never" style="margin-bottom: 16px">
      <template #header>本机信息</template>
      <el-descriptions :column="2" size="small" border>
        <el-descriptions-item label="主控 ID">{{ info?.master_id || "-" }}</el-descriptions-item>
        <el-descriptions-item label="主机名">{{ info?.master_name || "-" }}</el-descriptions-item>
        <el-descriptions-item label="版本">{{ info?.version || "-" }}</el-descriptions-item>
        <el-descriptions-item label="缓存目录">{{ info?.cache_dir || "-" }}</el-descriptions-item>
      </el-descriptions>
    </el-card>

    <el-card shadow="never" style="margin-bottom: 16px">
      <template #header>模型目录</template>
      <div class="row">
        <span class="path">{{ info?.models_dir || "-" }}</span>
        <el-button type="primary" plain @click="changeModelsDir">更改模型目录…</el-button>
      </div>
      <p class="hint">
        下载的模型默认保存到这里；「本地导入」的模型始终原地使用，不受此设置影响。
        更改目录时可选是否迁移现有文件（会自动更新模型列表路径）。
      </p>
    </el-card>

    <el-card shadow="never" style="margin-bottom: 16px">
      <template #header>外观</template>
      <div class="row">
        <span>深色模式</span>
        <el-switch :model-value="dark" @change="(v: boolean) => toggleDark(v)" />
      </div>
    </el-card>

    <el-card shadow="never" style="margin-bottom: 16px">
      <template #header>网络安全</template>
      <el-alert
        type="error"
        :closable="false"
        title="llama.cpp 的 RPC 协议（端口 50052）无鉴权无加密，只能靠防火墙白名单保护。请勿将本工具的任何端口暴露到公网，仅限可信局域网使用。"
      />
      <div class="row" style="margin-top: 10px">
        <span>放行 UDP 47890 发现端口（供被控端广播心跳到达本机）</span>
        <el-button @click="api.allowDiscoveryFirewall().then((m) => ElMessage.success(m)).catch((e) => ElMessage.error(String(e)))">
          放行（需管理员）
        </el-button>
      </div>
    </el-card>

    <el-card shadow="never">
      <template #header>HuggingFace 下载源</template>
      <div class="row">
        <span>模型下载使用的站点</span>
        <el-select
          :model-value="hfMirror"
          style="width: 320px"
          @change="(v: string) => saveMirror(v)"
        >
          <el-option
            v-for="m in MIRROR_PRESETS"
            :key="m.value"
            :label="m.label"
            :value="m.value"
          />
        </el-select>
      </div>
      <p class="hint">国内网络访问官方站点经常失败，推荐 hf-mirror.com；下载失败会自动重试并保留断点。</p>
    </el-card>

    <el-card shadow="never" style="margin-top: 16px">
      <template #header>即将推出</template>
      <p class="hint">开机自启 · PIN 有效期设置 · 诊断日志导出</p>
    </el-card>
  </div>
</template>

<style scoped>
.row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}
.path {
  font-family: Consolas, monospace;
  font-size: 12.5px;
  word-break: break-all;
}
.hint {
  color: var(--el-text-color-secondary);
  font-size: 12.5px;
  line-height: 1.7;
}
</style>
