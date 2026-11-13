<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref } from "vue";
import { ElMessage } from "element-plus";
import { api, listenEvent } from "../../api";
import { fmtUptime, type LlamaStatus, type ModelRow, type NodeView } from "../../types";

const models = ref<ModelRow[]>([]);
const nodes = ref<NodeView[]>([]);
const selectedModel = ref("");
const ctxSize = ref(4096);
const ngl = ref(99);
const port = ref(8080);
const status = ref<LlamaStatus | null>(null);
const logBox = ref<HTMLElement | null>(null);

// 聊天
interface ChatMsg {
  role: "user" | "assistant" | "error";
  content: string;
}
const chat = ref<ChatMsg[]>([]);
const chatInput = ref("");
const chatting = ref(false);

const onlinePairedNodes = computed(() =>
  nodes.value.filter((n) => n.paired && n.online),
);

async function refresh() {
  try {
    models.value = await api.listModels();
    nodes.value = await api.getNodes();
    status.value = await api.llamaStatus();
    if (!selectedModel.value) {
      const ok = models.value.find((m) => m.status === "ok");
      if (ok) selectedModel.value = ok.id;
    }
  } catch (e) {
    ElMessage.error(`刷新失败：${e}`);
  }
}

async function start() {
  if (!selectedModel.value) {
    ElMessage.warning("请先选择模型（需状态为「可用」）");
    return;
  }
  try {
    status.value = await api.llamaStart(selectedModel.value, ctxSize.value, ngl.value, port.value);
    chat.value = [];
    ElMessage.success("推理服务已启动，正在加载模型…");
  } catch (e) {
    ElMessage.error(String(e));
  }
}

async function stop() {
  try {
    status.value = await api.llamaStop();
    ElMessage.success("已停止");
  } catch (e) {
    ElMessage.error(String(e));
  }
}

async function sendChat() {
  const text = chatInput.value.trim();
  if (!text || chatting.value || !status.value?.running) return;
  chatInput.value = "";
  chat.value.push({ role: "user", content: text });
  chatting.value = true;
  await nextTick(scrollBottom);

  try {
    const resp = await fetch(`${status.value.base_url}/v1/chat/completions`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        messages: [{ role: "user", content: text }],
        max_tokens: 512,
        temperature: 0.7,
      }),
    });
    if (!resp.ok) {
      const t = await resp.text();
      throw new Error(`HTTP ${resp.status}：${t.slice(0, 300)}`);
    }
    const data = await resp.json();
    const content = data?.choices?.[0]?.message?.content ?? "(空响应)";
    chat.value.push({ role: "assistant", content });
  } catch (e) {
    const msg = String(e);
    chat.value.push({
      role: "error",
      content: msg.includes("Failed to fetch") || msg.includes("fetch")
        ? "无法连接推理服务：可能模型仍在加载中，请等日志出现 listening 后重试。"
        : msg,
    });
  } finally {
    chatting.value = false;
    await nextTick(scrollBottom);
  }
}

function scrollBottom() {
  if (logBox.value) logBox.value.scrollTop = logBox.value.scrollHeight;
}

let pollTimer: number | undefined;
let unlisteners: Array<() => void> = [];

onMounted(async () => {
  await refresh();
  pollTimer = window.setInterval(async () => {
    try {
      status.value = await api.llamaStatus();
    } catch {
      /* ignore */
    }
    await nextTick(() => {
      const box = document.getElementById("llama-log-panel");
      if (box) box.scrollTop = box.scrollHeight;
    });
  }, 1500);
  unlisteners.push(await listenEvent<string>("llama-log", () => {}));
});

onUnmounted(() => {
  if (pollTimer) window.clearInterval(pollTimer);
  unlisteners.forEach((u) => u());
});
</script>

<template>
  <div class="page">
    <el-row :gutter="16">
      <el-col :span="14">
        <el-card shadow="never">
          <template #header>
            <div class="card-head">
              <span>推理服务控制</span>
              <el-tag :type="status?.running ? 'success' : 'info'">
                {{ status?.running ? "运行中" : "未运行" }}
              </el-tag>
            </div>
          </template>

          <el-form label-width="110px" label-position="left">
            <el-form-item label="模型">
              <el-select v-model="selectedModel" placeholder="选择可用模型" style="width: 100%">
                <el-option
                  v-for="m in models"
                  :key="m.id"
                  :value="m.id"
                  :label="`${m.name}（${m.status === 'ok' ? '可用' : '不可用'}）`"
                  :disabled="m.status !== 'ok'"
                />
              </el-select>
            </el-form-item>
            <el-form-item label="上下文大小">
              <el-input-number v-model="ctxSize" :min="512" :max="131072" :step="1024" />
            </el-form-item>
            <el-form-item label="GPU 层数">
              <el-slider v-model="ngl" :min="0" :max="999" show-input style="width: 100%" />
              <span class="hint">-ngl 99 表示尽量全部放 GPU；分布式模式下各节点按显存自动分配</span>
            </el-form-item>
            <el-form-item label="服务端口">
              <el-input-number v-model="port" :min="1024" :max="65535" />
            </el-form-item>
          </el-form>

          <div class="actions">
            <el-button type="primary" :disabled="!!status?.running" @click="start">
              启动服务
            </el-button>
            <el-button type="danger" :disabled="!status?.running" @click="stop">
              停止服务
            </el-button>
          </div>

          <el-descriptions
            v-if="status"
            :column="2"
            size="small"
            border
            style="margin-top: 14px"
          >
            <el-descriptions-item label="服务地址">
              {{ status.running ? `${status.base_url}（本机+局域网）` : "-" }}
            </el-descriptions-item>
            <el-descriptions-item label="运行时长">
              {{ status.running ? fmtUptime(status.uptime_s) : "-" }}
            </el-descriptions-item>
            <el-descriptions-item label="当前模型">
              {{ status.model_name || "-" }}
            </el-descriptions-item>
            <el-descriptions-item label="RPC 节点">
              {{ status.rpc_targets.length ? status.rpc_targets.join(", ") : "仅本机" }}
            </el-descriptions-item>
          </el-descriptions>
        </el-card>
      </el-col>

      <el-col :span="10">
        <el-card shadow="never">
          <template #header>
            <div class="card-head">
              <span>测试对话</span>
              <span class="hint">调用本机 {{ status?.base_url }}/v1 验证可用</span>
            </div>
          </template>
          <div class="chat-box" ref="chatBox">
            <div v-if="!chat.length" class="chat-msg assistant">
              启动推理服务后，在这里输入内容测试模型是否能正常出字。
            </div>
            <div v-for="(m, i) in chat" :key="i" :class="['chat-msg', m.role]">
              {{ m.content }}
            </div>
          </div>
          <div style="display: flex; gap: 8px">
            <el-input
              v-model="chatInput"
              placeholder="输入测试内容…"
              :disabled="!status?.running || chatting"
              @keyup.enter="sendChat"
            />
            <el-button type="primary" :loading="chatting" :disabled="!status?.running" @click="sendChat">
              发送
            </el-button>
          </div>
        </el-card>
      </el-col>
    </el-row>

    <el-card shadow="never" style="margin-top: 16px">
      <template #header>
        <div class="card-head">
          <span>llama-server 实时日志</span>
          <span class="hint">分布式层分配信息会出现在这里（ndevices / tensor 分配行）</span>
        </div>
      </template>
      <div id="llama-log-panel" class="log-panel" style="height: 300px">
        {{ status?.logs?.join("\n") || "暂无日志" }}
      </div>
    </el-card>
  </div>
</template>

<style scoped>
.card-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
}
.hint {
  color: var(--el-text-color-secondary);
  font-size: 12px;
  font-weight: 400;
}
.actions {
  display: flex;
  gap: 10px;
}
</style>
