<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { ElMessage, ElMessageBox } from "element-plus";
import { api } from "../../api";
import { fmtBytes, type AgentStatusView } from "../../types";

const st = ref<AgentStatusView | null>(null);
const pin = ref("");
const submitting = ref(false);

const paired = computed(() => !!st.value?.paired);

async function refresh() {
  try {
    st.value = await api.agentStatus();
  } catch {
    /* 忽略瞬时错误 */
  }
}

async function submitPin() {
  const p = pin.value.trim();
  if (p.length !== 6) {
    ElMessage.warning("请输入主控显示的 6 位 PIN");
    return;
  }
  submitting.value = true;
  try {
    const msg = await api.submitPairPin(p);
    ElMessage.success(msg);
    pin.value = "";
  } catch (e) {
    ElMessage.error(String(e));
  } finally {
    submitting.value = false;
  }
}

async function unpair() {
  try {
    await ElMessageBox.confirm(
      "解除配对后本机将退出主控的节点列表，需重新输入 PIN 才能入网。确定？",
      "解除配对",
      { type: "warning" },
    );
    const msg = await api.unpair();
    ElMessage.success(msg);
    refresh();
  } catch {
    /* 取消 */
  }
}

async function restartRpc() {
  try {
    const msg = await api.rpcRestart();
    ElMessage.success(msg);
    refresh();
  } catch (e) {
    ElMessage.error(String(e));
  }
}

async function stopRpc() {
  try {
    const msg = await api.rpcStop();
    ElMessage.success(msg);
    refresh();
  } catch (e) {
    ElMessage.error(String(e));
  }
}

async function clearCache() {
  try {
    await ElMessageBox.confirm(
      "清理本机 rpc 权重缓存？清理后下次加入推理时需重新传输。",
      "清理缓存",
      { type: "warning" },
    );
    const msg = await api.clearAgentCache();
    ElMessage.success(msg);
    refresh();
  } catch {
    /* 取消 */
  }
}

function openCache() {
  api.openAgentCacheDir().catch((e) => ElMessage.error(String(e)));
}

let timer: number | undefined;
onMounted(() => {
  refresh();
  timer = window.setInterval(refresh, 2000);
});
onUnmounted(() => {
  if (timer) window.clearInterval(timer);
});
</script>

<template>
  <div class="page">
    <div class="head">
      <div>
        <div class="title">LLM 被控端</div>
        <div class="sub">
          {{ st?.hostname }} · {{ st?.version }}
          <el-tag v-if="paired" type="success" style="margin-left: 8px">已配对</el-tag>
          <el-tag v-else type="warning" style="margin-left: 8px">未配对</el-tag>
        </div>
      </div>
      <span :class="['node-dot', st?.rpc_running ? 'online' : 'offline']" style="transform: scale(1.6)" />
    </div>

    <!-- 未配对：PIN 输入 -->
    <el-card v-if="!paired" shadow="never" style="margin-top: 14px">
      <template #header>加入主控</template>
      <p class="desc">
        1. 在主控端「节点管理」页点击本机对应的「配对」按钮，会显示 6 位 PIN。<br />
        2. 把 PIN 输入到下面，配对会在几秒内自动完成。
      </p>
      <div style="display: flex; gap: 10px; margin-top: 12px">
        <el-input
          v-model="pin"
          maxlength="6"
          placeholder="6 位数字 PIN"
          style="width: 200px; font-size: 20px; text-align: center; letter-spacing: 8px"
          @keyup.enter="submitPin"
        />
        <el-button type="primary" :loading="submitting" @click="submitPin">开始配对</el-button>
      </div>
      <p class="hint">PIN 5 分钟内有效。本机信息（主机名/GPU/显存）会通过局域网广播供主控发现。</p>
      <el-alert
        v-if="st?.bootstrap_pending"
        type="success"
        :closable="false"
        style="margin-top: 10px"
        title="检测到安装包内置的引导码：正在自动连接主控，无需手动输入 PIN。若长时间未入网，请确认主控端已启动且与本机在同一局域网。"
      />
    </el-card>

    <!-- 已配对：状态 -->
    <template v-if="paired && st">
      <el-card shadow="never" style="margin-top: 14px">
        <template #header>运行状态</template>
        <el-descriptions :column="2" size="small" border>
          <el-descriptions-item label="主控">{{ st.master_name }}（{{ st.master_ip }}）</el-descriptions-item>
          <el-descriptions-item label="GPU">{{ st.gpu || "未检测到" }}</el-descriptions-item>
          <el-descriptions-item label="显存">
            {{ st.vram_total_mb ? `${st.vram_total_mb} MB` : "-" }}
            {{ st.vram_free_mb != null ? `（空闲 ${st.vram_free_mb} MB）` : "" }}
          </el-descriptions-item>
          <el-descriptions-item label="本机 IP">{{ st.ips.join(" / ") }}</el-descriptions-item>
          <el-descriptions-item label="rpc-server">
            <el-tag :type="st.rpc_running ? 'success' : 'danger'" size="small">
              {{ st.rpc_running ? `运行中（端口 ${st.rpc_port}）` : "已停止" }}
            </el-tag>
            <span v-if="st.rpc_running" class="hint" style="margin-left: 6px">
              pid {{ st.rpc_pid }} · 已运行 {{ st.rpc_uptime_s }} 秒
            </span>
          </el-descriptions-item>
          <el-descriptions-item label="权重缓存">
            {{ fmtBytes(st.cache_bytes) }}
          </el-descriptions-item>
        </el-descriptions>
        <div style="margin-top: 14px; display: flex; gap: 10px; flex-wrap: wrap">
          <el-button size="small" @click="restartRpc">重启 rpc-server</el-button>
          <el-button size="small" @click="stopRpc">停止</el-button>
          <el-button size="small" @click="clearCache">清理缓存</el-button>
          <el-button size="small" @click="openCache">打开缓存目录</el-button>
          <el-button size="small" type="danger" plain @click="unpair">解除配对</el-button>
        </div>
      </el-card>

      <el-alert
        v-if="!st.firewall_ok"
        type="warning"
        :closable="false"
        style="margin-top: 14px"
        title="检测到防火墙规则尚未生效：主控可能无法连接本机 50052 端口。可在主控端重新发起配对，或本机手动放行。"
      />
      <el-alert
        v-if="!st.runtime_ok"
        type="error"
        :closable="false"
        style="margin-top: 14px"
        :title="`llama.cpp 运行时不可用：${st.runtime_message || '未检测到 runtime 目录'}`"
        description="通常缺 VC++ 运行库。安装 Microsoft Visual C++ 2015-2022 Redistributable (x64) 后重启本程序即可；或重新安装最新版被控端（已内置）。"
        show-icon
      />
    </template>

    <el-card shadow="never" style="margin-top: 14px">
      <template #header>事件日志</template>
      <div class="log-panel" style="max-height: 200px">
        {{ st?.logs?.length ? st.logs.slice().reverse().join("\n") : "暂无日志" }}
      </div>
    </el-card>

    <el-alert
      class="security-note"
      type="info"
      :closable="false"
      title="本程序仅响应配对主控的指令；rpc 端口已通过防火墙限制为仅主控 IP 可访问。请勿用于公网环境。"
    />
  </div>
</template>

<style scoped>
.head {
  display: flex;
  justify-content: space-between;
  align-items: center;
}
.title {
  font-size: 18px;
  font-weight: 600;
}
.sub {
  color: var(--el-text-color-secondary);
  font-size: 13px;
  margin-top: 2px;
}
.desc {
  color: var(--el-text-color-regular);
  font-size: 13px;
  line-height: 2;
  margin: 0;
}
.hint {
  color: var(--el-text-color-secondary);
  font-size: 12px;
}
</style>
