<script setup lang="ts">
import { ref } from "vue";
import NodesView from "./NodesView.vue";
import ModelsView from "./ModelsView.vue";
import ServeView from "./ServeView.vue";
import CacheView from "./CacheView.vue";
import SettingsView from "./SettingsView.vue";

const active = ref("nodes");

const menus = [
  { key: "nodes", label: "节点管理", icon: "🖥" },
  { key: "models", label: "模型管理", icon: "📦" },
  { key: "serve", label: "推理服务", icon: "🚀" },
  { key: "cache", label: "缓存与维护", icon: "🧹" },
  { key: "settings", label: "设置", icon: "⚙" },
];

const comps: Record<string, unknown> = {
  nodes: NodesView,
  models: ModelsView,
  serve: ServeView,
  cache: CacheView,
  settings: SettingsView,
};
</script>

<template>
  <el-container style="height: 100%">
    <el-aside width="190px" style="border-right: 1px solid var(--el-border-color-lighter)">
      <div class="brand">
        <div class="brand-title">LLM 局域网推理</div>
        <div class="brand-sub">主控台</div>
      </div>
      <el-menu :default-active="active" @select="(k: string) => (active = k)">
        <el-menu-item v-for="m in menus" :key="m.key" :index="m.key">
          <span style="margin-right: 8px">{{ m.icon }}</span>{{ m.label }}
        </el-menu-item>
      </el-menu>
    </el-aside>
    <el-main style="padding: 0; overflow: hidden">
      <component :is="comps[active]" />
    </el-main>
  </el-container>
</template>

<style scoped>
.brand {
  padding: 18px 16px 10px;
}
.brand-title {
  font-size: 16px;
  font-weight: 600;
}
.brand-sub {
  font-size: 12px;
  color: var(--el-text-color-secondary);
  margin-top: 2px;
}
</style>
