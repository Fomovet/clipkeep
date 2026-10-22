<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { api, applyTheme } from "../api";
import ClipIcon from "./ClipIcon.vue";

const paused = ref(false);
const autostart = ref(false);
const hotkey = ref("");
const busy = ref(false);
const error = ref("");
// The menu is rendered only once its state is known, so the switches appear in place instead of
// animating from their defaults each time the menu opens.
const loaded = ref(false);
const status = computed(() => paused.value ? "已暂停" : "记录中");
const unlisteners: UnlistenFn[] = [];

async function refresh() {
  try {
    const [isPaused, isAutostart, view] = await Promise.all([
      api.getCapturePaused(), api.getAutostart(), api.getSettings(),
    ]);
    paused.value = isPaused;
    autostart.value = isAutostart;
    applyTheme(view.settings.theme);
    hotkey.value = view.settings.hotkeyEnabled
      ? (view.hotkeys.find((option) => option.id === view.settings.hotkey)?.label ?? "")
      : "";
    error.value = "";
  } catch (e) {
    error.value = String(e);
  } finally {
    loaded.value = true;
  }
}

async function act(action: "show" | "pause" | "autostart" | "quit") {
  if (busy.value) return;
  busy.value = true;
  error.value = "";
  try {
    await invoke("tray_menu_action", { action });
  } catch (e) {
    error.value = String(e);
  } finally {
    if (action === "pause" || action === "autostart") {
      const actionError = error.value;
      await refresh();
      if (actionError) error.value = actionError;
    }
    busy.value = false;
  }
}

function onKey(e: KeyboardEvent) {
  if (e.key === "Escape") {
    e.preventDefault();
    invoke("hide_tray_menu");
  }
}

onMounted(async () => {
  window.addEventListener("keydown", onKey);
  document.addEventListener("contextmenu", preventContextMenu);
  unlisteners.push(
    await listen("tray-menu-shown", refresh),
    await listen("capture-paused-changed", refresh),
    await listen("autostart-changed", refresh),
    await listen("settings-changed", refresh),
  );
  await refresh();
  await nextTick();
  // The window is created on demand and stays hidden until it has something to show.
  await invoke("tray_menu_ready");
});

function preventContextMenu(e: MouseEvent) { e.preventDefault(); }

onUnmounted(() => {
  window.removeEventListener("keydown", onKey);
  document.removeEventListener("contextmenu", preventContextMenu);
  unlisteners.forEach((unlisten) => unlisten());
});
</script>

<template>
  <div v-if="loaded" class="tray-shell menu-surface">
    <header class="menu-label tray-head">
      <strong>clipkeep</strong>
      <span class="tray-status" :class="{ paused }"><i></i>{{ status }}</span>
    </header>
    <nav class="tray-actions" aria-label="clipkeep 托盘菜单">
      <button class="menu-row primary" :disabled="busy" @click="act('show')">
        <ClipIcon name="clipboard" />
        <span class="menu-text">打开主页面</span>
        <span v-if="hotkey" class="menu-shortcut">{{ hotkey }}</span>
      </button>
      <div class="menu-divider"></div>
      <label class="menu-row tray-toggle" :class="{ disabled: busy }">
        <ClipIcon name="clipboard" />
        <span class="menu-text">剪贴板记录</span>
        <input class="switch tray-switch" type="checkbox" role="switch" :checked="!paused" :disabled="busy" @change="act('pause')" />
      </label>
      <label class="menu-row tray-toggle" :class="{ disabled: busy }">
        <ClipIcon name="power" />
        <span class="menu-text">开机自启</span>
        <input class="switch tray-switch" type="checkbox" role="switch" :checked="autostart" :disabled="busy" @change="act('autostart')" />
      </label>
      <div class="menu-divider"></div>
      <button class="menu-row danger" :disabled="busy" @click="act('quit')">
        <ClipIcon name="close" />
        <span class="menu-text">退出 clipkeep</span>
      </button>
    </nav>
    <p v-if="error" class="tray-error" role="alert">{{ error }}</p>
  </div>
</template>

<style scoped>
.tray-shell {
  position: relative;
  height: 100%;
  overflow: hidden;
}
.tray-head { justify-content: space-between; height: 26px; }
.tray-head strong { font-size: var(--fs-xs); color: var(--muted); }
.tray-status { display: inline-flex; align-items: center; gap: 5px; font-weight: 500; }
.tray-status i { width: 6px; height: 6px; border-radius: 50%; background: var(--ok); }
.tray-status.paused i { background: var(--muted); }
.tray-toggle { cursor: pointer; }
.tray-toggle.disabled { opacity: .55; cursor: default; }
.tray-toggle.disabled:hover { background: none; color: var(--text); }
.tray-switch { width: 32px; height: 18px; margin: 0; flex: none; }
.tray-switch::after { width: 14px; height: 14px; }
.tray-switch:checked::after { transform: translateX(14px); }
.tray-switch:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
.tray-error {
  position: absolute;
  inset: auto 5px 5px;
  margin: 0;
  padding: 6px;
  border: 1px solid var(--border-strong);
  border-radius: var(--r-sm);
  background: var(--elevated);
  box-shadow: var(--shadow-sm);
  color: var(--danger);
  font-size: var(--fs-xs);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
</style>
