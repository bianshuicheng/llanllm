<script setup lang="ts">
import { onMounted, ref } from "vue";
import { api } from "./api";
import MasterLayout from "./views/master/MasterLayout.vue";
import AgentHome from "./views/agent/AgentHome.vue";

const role = ref<string>("");
const error = ref("");

onMounted(async () => {
  try {
    role.value = await api.getRole();
  } catch (e) {
    error.value = String(e);
  }
});
</script>

<template>
  <div v-if="error" class="page">
    <el-alert type="error" :title="`后端通信失败：${error}`" :closable="false" />
  </div>
  <MasterLayout v-else-if="role === 'master'" />
  <AgentHome v-else-if="role === 'agent'" />
  <div v-else class="page">
    <el-skeleton :rows="6" animated />
  </div>
</template>
