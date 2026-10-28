<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, reactive, ref, watch } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { api, type LanState, type Settings, type SettingsView, type StorageUsage, type Theme } from "../api";
import { formatSize, useUpdate } from "../update";
import ClipIcon from "./ClipIcon.vue";

defineEmits<{ back: [] }>();

const themeOptions: { id: Theme; label: string }[] = [
  { id: "system", label: "跟随系统" },
  { id: "light", label: "浅色" },
  { id: "dark", label: "深色" },
];

const view = ref<SettingsView | null>(null);
const s = computed(() => view.value?.settings);
// Unknown until the task lookup returns, which is slower than the settings: rendering the switch
// early would animate it from off to its real state every time the page opens.
const autostart = ref<boolean | null>(null);
const lan = ref<LanState | null>(null);
const peerName = ref("");
const peerCode = ref("");
const lanBusy = ref(false);
const error = ref("");
const notice = ref("");
const storageDraft = ref(500);
const storageUsage = ref<StorageUsage | null>(null);
const storageError = ref("");
const busy = ref(false);

const clearOpen = ref(false);
const clearFlags = reactive({ pinned: false, grouped: false, all: false });
const clearArmed = ref(false);

const { version: appVersion, state: update, newer } = useUpdate();
const aboutEl = ref<HTMLElement | null>(null);
const updateDesc = computed(() => {
  const s = update.value;
  switch (s.kind) {
    case "checking": return "正在检查最新版本…";
    case "upToDate": return "已是最新版";
    case "available":
      return s.portable
        ? `发现新版本 v${s.version}。单文件版请下载后替换当前的 exe`
        : `发现新版本 v${s.version}${s.size ? `（${formatSize(s.size)}）` : ""}，下载完成后由你确认安装`;
    case "downloading": return `正在下载 v${s.version}`;
    case "ready": return `v${s.version} 已下载并通过校验。安装时 clipkeep 会自动关闭，完成后重新启动，历史记录不受影响`;
    case "error": return s.message;
    default: return "同时查询 GitHub 和 jsDelivr，国内网络也能检查";
  }
});
const updatePercent = computed(() => {
  const s = update.value;
  return s.kind === "downloading" && s.total > 0 ? Math.min(100, (s.received / s.total) * 100) : 0;
});
type UpdateAction = { label: string; icon: "refresh" | "download" | "close" | "upload"; run: () => unknown; accent?: boolean; busy?: boolean };
const updateActions = computed<UpdateAction[]>(() => {
  const s = update.value;
  const check: UpdateAction = { label: "检查更新", icon: "refresh", run: api.checkUpdate };
  const browser: UpdateAction = { label: "浏览器下载", icon: "download", run: api.openReleasePage };
  switch (s.kind) {
    case "checking": return [{ label: "检查中…", icon: "refresh", run: () => {}, busy: true }];
    case "available":
      return s.portable
        ? [{ ...browser, label: "前往下载", accent: true }]
        : [{ label: "下载并安装", icon: "download", run: api.downloadUpdate, accent: true }];
    case "downloading": return [{ label: "取消", icon: "close", run: api.cancelUpdate }];
    case "ready": return [{ label: "立即安装", icon: "upload", run: install, accent: true }];
    case "error": return s.version ? [browser, { label: "重试", icon: "refresh", run: api.downloadUpdate }] : [{ ...check, label: "重试" }];
    default: return [check];
  }
});

async function install() {
  try {
    await api.installUpdate();
  } catch (e) {
    error.value = `安装失败：${e}`;
  }
}

// Opened from the tray's "new version" row: bring this card into view once the page has loaded.
const props = defineProps<{ focusAbout?: boolean }>();
watch(
  () => [props.focusAbout, aboutEl.value] as const,
  ([focus, el]) => {
    if (focus && el) nextTick(() => el.scrollIntoView({ block: "center" }));
  },
  { immediate: true },
);
let clearTimer: number | undefined;
let noticeTimer: number | undefined;
let storageTimer: number | undefined;
let storageRequest = 0;
let disposed = false;
const unlisteners: UnlistenFn[] = [];

const storageLimitBytes = computed(() => (s.value?.maxStorageMb ?? 0) * 1024 * 1024);
const storagePercent = computed(() => storageUsage.value && storageLimitBytes.value > 0
  ? Math.min(100, (storageUsage.value.ordinaryBytes / storageLimitBytes.value) * 100)
  : 0);
const storageRemainingBytes = computed(() => Math.max(0, storageLimitBytes.value - (storageUsage.value?.ordinaryBytes ?? 0)));
const storagePercentLabel = computed(() => {
  if (storagePercent.value > 0 && storagePercent.value < 0.1) return "<0.1%";
  const digits = storagePercent.value < 10 || storagePercent.value > 99 ? 1 : 0;
  return `${Number(storagePercent.value.toFixed(digits))}%`;
});

function formatBytes(bytes: number) {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = -1;
  do { value /= 1024; unit++; } while (value >= 1024 && unit < units.length - 1);
  return `${new Intl.NumberFormat("zh-CN", { maximumFractionDigits: value < 10 ? 2 : 1 }).format(value)} ${units[unit]}`;
}

async function refreshStorageUsage() {
  const request = ++storageRequest;
  try {
    const usage = await api.getStorageUsage();
    if (disposed || request !== storageRequest) return;
    storageUsage.value = usage;
    storageError.value = "";
  } catch (e) {
    if (disposed || request !== storageRequest) return;
    storageUsage.value = null;
    storageError.value = `读取存储用量失败：${String(e)}`;
  }
}

function queueStorageRefresh() {
  window.clearTimeout(storageTimer);
  storageTimer = window.setTimeout(() => { void refreshStorageUsage(); }, 120);
}

async function load() {
  const v = await api.getSettings();
  if (disposed) return;
  view.value = v;
  storageDraft.value = v.settings.maxStorageMb;
}

async function save(patch: Partial<Settings>) {
  if (!view.value) return;
  error.value = "";
  try {
    view.value.settings = await api.updateSettings({ ...view.value.settings, ...patch });
    storageDraft.value = view.value.settings.maxStorageMb;
  } catch (e) {
    error.value = String(e);
    try {
      await load();
    } catch (reloadError) {
      error.value += `；重新读取设置失败：${String(reloadError)}`;
    }
  }
}

function saveStorage() {
  save({ maxStorageMb: Math.max(0, Math.floor(Number(storageDraft.value) || 0)) });
}

async function toggleAutostart() {
  error.value = "";
  try {
    autostart.value = await api.setAutostart(!autostart.value);
  } catch (e) {
    error.value = String(e);
  }
}

async function lanAction(action: () => Promise<LanState>, success?: string) {
  lanBusy.value = true;
  error.value = "";
  try {
    lan.value = await action();
    if (success) flash(success);
  } catch (e) {
    error.value = String(e);
  } finally {
    lanBusy.value = false;
  }
}

async function addPeer() {
  await lanAction(() => api.addLanPeer(peerName.value, peerCode.value), "设备已添加");
  if (!error.value) { peerName.value = ""; peerCode.value = ""; }
}

function toggleLanReceive() {
  if (lan.value) lanAction(() => api.setLanReceive(!lan.value!.receiveEnabled));
}

function removePeer(address: string) {
  lanAction(() => api.removeLanPeer(address), "设备已移除");
}

async function copyCode(address: string) {
  error.value = "";
  try {
    await api.copyLanCode(address);
    flash("配对码已复制，请通过可信方式交给另一台设备");
  } catch (e) {
    error.value = String(e);
  }
}

function flash(msg: string) {
  notice.value = msg;
  window.clearTimeout(noticeTimer);
  noticeTimer = window.setTimeout(() => (notice.value = ""), 6000);
}

async function run(fn: () => Promise<string | null>) {
  busy.value = true;
  error.value = "";
  try {
    const msg = await fn();
    if (msg) flash(msg);
  } catch (e) {
    error.value = String(e);
  } finally {
    busy.value = false;
  }
}

const clearSummary = computed(() => {
  if (clearFlags.all) return "将删除全部数据，包括收藏、置顶和分组内容。";
  const extra = [clearFlags.pinned ? "置顶" : "", clearFlags.grouped ? "分组内容" : ""].filter(Boolean);
  return extra.length ? `将删除未收藏的历史，以及${extra.join("、")}（收藏保留）。` : "将删除未收藏的历史（收藏、置顶、分组保留）。";
});

function armClear() {
  clearArmed.value = true;
  clearTimer = window.setTimeout(() => (clearArmed.value = false), 4000);
}
async function confirmClear() {
  window.clearTimeout(clearTimer);
  clearArmed.value = false;
  try {
    await api.clearData(clearFlags.pinned, clearFlags.grouped, clearFlags.all);
    clearOpen.value = false;
    clearFlags.pinned = clearFlags.grouped = clearFlags.all = false;
    flash("已清空");
  } catch (e) {
    error.value = `清空失败：${String(e)}`;
  }
}

const hotkeyLabel = computed(() => view.value?.hotkeys.find((h) => h.id === s.value?.hotkey)?.label ?? "");

async function refreshLan() {
  try {
    const state = await api.getLanState();
    if (!disposed) lan.value = state;
  } catch (e) {
    if (!disposed) error.value = `读取局域网状态失败：${String(e)}`;
  }
}

onMounted(() => {
  void refreshStorageUsage();
  void (async () => {
    const [settingsResult, autostartResult, lanResult] = await Promise.allSettled([
      load(), api.getAutostart(), api.getLanState(),
    ]);
    if (disposed) return;
    if (settingsResult.status === "rejected") error.value = `读取设置失败：${String(settingsResult.reason)}`;
    if (autostartResult.status === "fulfilled") autostart.value = autostartResult.value;
    else error.value = `读取开机自启状态失败：${String(autostartResult.reason)}`;
    if (lanResult.status === "fulfilled") lan.value = lanResult.value;
    else error.value = `读取局域网状态失败：${String(lanResult.reason)}`;
    try {
      const onAutostart = await listen<boolean>("autostart-changed", (e) => (autostart.value = e.payload));
      if (disposed) onAutostart(); else unlisteners.push(onAutostart);
      const onLan = await listen("lan-state-changed", () => { void refreshLan(); });
      if (disposed) onLan(); else unlisteners.push(onLan);
      const onClips = await listen("clips-changed", queueStorageRefresh);
      if (disposed) onClips(); else unlisteners.push(onClips);
    } catch (e) {
      if (!disposed) error.value = `监听设置变化失败：${String(e)}`;
    }
  })();
});

onUnmounted(() => {
  disposed = true;
  window.clearTimeout(clearTimer);
  window.clearTimeout(noticeTimer);
  window.clearTimeout(storageTimer);
  storageRequest++;
  unlisteners.forEach((u) => u());
});
</script>

<template>
  <div class="settings">
    <header class="topbar" @mousedown.self.left="api.startDrag()">
      <button class="icon-btn" title="返回 (Esc)" @click="$emit('back')"><ClipIcon name="back" /></button>
      <ClipIcon name="settings" class="title-icon" /><span class="title" @mousedown.left="api.startDrag()">设置</span>
      <div class="spacer" @mousedown.left="api.startDrag()"></div>
    </header>

    <p v-if="error && !view" class="banner error">{{ error }}</p>

    <div v-if="s && view" class="scroll">
      <transition name="fade">
        <p v-if="error" class="banner error">{{ error }}</p>
      </transition>
      <transition name="fade">
        <p v-if="notice" class="banner notice"><ClipIcon name="download" class="tiny" />{{ notice }}</p>
      </transition>

      <h3><ClipIcon name="general" />常规</h3>
      <div class="card">
        <div class="row">
          <div class="label">
            <div class="name">外观</div>
            <div class="desc">跟随系统时随 Windows 浅色 / 深色自动切换。</div>
          </div>
          <div class="segmented" role="group" aria-label="外观">
            <button v-for="option in themeOptions" :key="option.id" type="button" :class="{ active: s.theme === option.id }" :aria-pressed="s.theme === option.id" @click="save({ theme: option.id })">{{ option.label }}</button>
          </div>
        </div>
        <label class="row">
          <div class="label">
            <div class="name">开机自启</div>
            <div class="desc">创建当前用户登录时以管理员权限运行的计划任务，启动后静默到托盘；关闭时删除任务。</div>
          </div>
          <input v-if="autostart !== null" type="checkbox" class="switch" :checked="autostart" @change="toggleAutostart" />
          <span v-else class="switch-placeholder" aria-hidden="true"></span>
        </label>
        <label class="row">
          <div class="label">
            <div class="name">固定窗口（失焦不隐藏）</div>
            <div class="desc">点击别处窗口不自动隐藏，方便连续操作；也可点标题栏图钉快速切换。</div>
          </div>
          <input type="checkbox" class="switch" :checked="s.keepOnBlur" @change="save({ keepOnBlur: !s.keepOnBlur })" />
        </label>
        <label class="row">
          <div class="label">
            <div class="name">记住窗口位置</div>
            <div class="desc">开：总在上次拖到的位置打开；关：在鼠标旁弹出。按住标题栏空白处拖动。</div>
          </div>
          <input type="checkbox" class="switch" :checked="s.rememberPosition" @change="save({ rememberPosition: !s.rememberPosition })" />
        </label>
        <label class="row">
          <div class="label">
            <div class="name">记录图片</div>
            <div class="desc">复制截图/图片时保存为 PNG，列表显示缩略图。</div>
          </div>
          <input type="checkbox" class="switch" :checked="s.captureImages" @change="save({ captureImages: !s.captureImages })" />
        </label>
        <label class="row">
          <div class="label">
            <div class="name">粘贴图片时附带文件名</div>
            <div class="desc">粘贴到网页上传时用原文件名或粘贴时间命名，避免多张图都叫 image.png。个别软件可能把图片当作文件发送，遇到时可关闭。</div>
          </div>
          <input type="checkbox" class="switch" :checked="s.pasteImageAsFile" @change="save({ pasteImageAsFile: !s.pasteImageAsFile })" />
        </label>
      </div>

      <h3><ClipIcon name="storage" />存储</h3>
      <div class="card">
        <div class="row storage-overview">
          <template v-if="storageUsage">
            <div class="storage-header">
              <div class="storage-primary">
                <span class="storage-label">普通历史已用</span>
                <strong class="storage-value">{{ formatBytes(storageUsage.ordinaryBytes) }}</strong>
              </div>
              <div v-if="s.maxStorageMb > 0" class="storage-limit">
                <span>清理上限</span>
                <strong>{{ formatBytes(storageLimitBytes) }}</strong>
              </div>
              <span v-else class="storage-unlimited">不限容量</span>
            </div>
            <template v-if="s.maxStorageMb > 0">
              <div class="storage-track" role="progressbar" aria-label="普通历史容量"
                :aria-valuenow="storagePercent" aria-valuemin="0" aria-valuemax="100"
                :aria-valuetext="`${formatBytes(storageUsage.ordinaryBytes)}，上限 ${formatBytes(storageLimitBytes)}`">
                <div class="storage-fill" :style="{ width: `${storagePercent}%` }"></div>
              </div>
              <div class="storage-caption"><span>{{ storagePercentLabel }} 已用</span><span>剩余 {{ formatBytes(storageRemainingBytes) }}</span></div>
            </template>
            <p v-else class="storage-caption">普通历史不会按容量自动清理。</p>
            <div class="storage-details">
              <span>数据库文件 <strong>{{ formatBytes(storageUsage.databaseBytes) }}</strong></span>
              <span>收藏等保留内容 <strong>{{ formatBytes(storageUsage.protectedBytes) }}</strong></span>
            </div>
            <p class="storage-note">进度按普通历史内容计算；数据库文件含写入日志，两者大小不必相同。</p>
          </template>
          <div v-else-if="storageError" class="storage-unavailable" role="alert">
            <span>{{ storageError }}</span>
            <button class="btn" @click="refreshStorageUsage">重试</button>
          </div>
          <p v-else class="storage-loading" role="status">正在读取存储用量…</p>
        </div>
        <div class="row">
          <div class="label">
            <div class="name">自动清理阈值</div>
            <div class="desc">普通历史超过此容量时删除最旧的。<b>收藏、置顶、分组不计入也不清理。</b><code>0</code> = 不限制。</div>
          </div>
          <div class="num">
            <input v-model.number="storageDraft" type="number" min="0" step="50" @change="saveStorage" />
            <span>MB</span>
          </div>
        </div>
      </div>

      <h3><ClipIcon name="network" />局域网传送</h3>
      <div v-if="lan" class="card">
        <label class="row">
          <div class="label">
            <div class="name">接收其他设备的文本</div>
            <div class="desc">默认关闭。开启后，同一局域网内持有配对码的设备可手动发送文本，收到后只存入本机历史，不覆盖当前剪贴板。</div>
          </div>
          <input type="checkbox" class="switch" :checked="lan.receiveEnabled" :disabled="lanBusy" @change="toggleLanReceive" />
        </label>
        <div v-if="lan.receiveEnabled" class="row lan-code-row">
          <div class="label">
            <div class="name">本机配对码</div>
            <div class="desc">在另一台设备的 clipkeep 中添加；请通过可信方式传递。端口 45788，首次接收可能需要允许 Windows 防火墙访问。</div>
          </div>
          <div v-if="lan.localCodes.length" class="lan-addresses">
            <button v-for="item in lan.localCodes" :key="item.address" class="btn" @click="copyCode(item.address)">复制 {{ item.address }}</button>
          </div>
          <p v-else class="desc">未找到局域网 IPv4 地址，请检查网络连接。</p>
        </div>
        <div class="row lan-form">
          <div class="label">
            <div class="name">添加接收设备</div>
            <div class="desc">从另一台设备复制配对码，填写一个易辨认的名称。</div>
          </div>
          <label class="lan-field"><span>设备名称</span><input v-model="peerName" class="lan-input" type="text" maxlength="40" autocomplete="off" placeholder="例如：笔记本" /></label>
          <label class="lan-field"><span>配对码</span><input v-model="peerCode" class="lan-input" type="text" spellcheck="false" autocomplete="off" autocapitalize="off" placeholder="粘贴 cklan1:// 配对码" /></label>
          <button class="btn lan-add" :disabled="lanBusy || !peerName.trim() || !peerCode.trim()" @click="addPeer">添加设备</button>
        </div>
        <div v-for="peer in lan.peers" :key="peer.address" class="row">
          <div class="label"><div class="name lan-peer-name" :title="peer.name">{{ peer.name }}</div><div class="desc lan-peer-address">{{ peer.address }}</div></div>
          <button class="btn" :disabled="lanBusy" :aria-label="`移除 ${peer.name}`" @click="removePeer(peer.address)">移除</button>
        </div>
        <div v-if="lan.receiveEnabled" class="row">
          <div class="label"><div class="name">重置本机配对码</div><div class="desc">旧配对码立即失效，已配对的发送设备需要重新添加本机。</div></div>
          <button class="btn" :disabled="lanBusy" @click="lanAction(api.rotateLanCode, '配对码已重置')">重置</button>
        </div>
      </div>

      <h3><ClipIcon name="keyboard" />快捷键</h3>
      <div class="card">
        <label class="row">
          <div class="label">
            <div class="name">全局快捷键{{ s.hotkeyEnabled ? `（${hotkeyLabel}）` : "" }}</div>
            <div class="desc">Win+V 是 Windows 保留快捷键，可能与系统面板冲突；遇到冲突可选 Ctrl+Shift+V。clipkeep 不修改系统剪贴板设置；关闭快捷键后可从托盘打开。</div>
          </div>
          <input type="checkbox" class="switch" :checked="s.hotkeyEnabled" @change="save({ hotkeyEnabled: !s.hotkeyEnabled })" />
        </label>
        <div class="row">
          <div class="label"><div class="name">组合键</div></div>
          <select :value="s.hotkey" :disabled="!s.hotkeyEnabled" @change="save({ hotkey: ($event.target as HTMLSelectElement).value })">
            <option v-for="h in view.hotkeys" :key="h.id" :value="h.id">{{ h.label }}</option>
          </select>
        </div>
        <p class="hint">
          <kbd>↑</kbd><kbd>↓</kbd> 选择 · <kbd>Enter</kbd> 粘贴 · <kbd>Shift+Enter</kbd> 仅复制 · <kbd>Ctrl+1–9</kbd> 快速粘贴 ·
          <kbd>Ctrl+P</kbd> 置顶 · <kbd>Ctrl+S</kbd> 收藏 · <kbd>Ctrl+D</kbd> 删除 · <kbd>Tab</kbd> 切分组 · <kbd>Esc</kbd> 关闭
        </p>
      </div>

      <h3><ClipIcon name="database" />数据</h3>
      <div class="card">
        <div class="row">
          <div class="label"><div class="name">导出备份</div><div class="desc">记录、收藏、分组、图片打包成一个 .ckbak 文件。</div></div>
          <button class="btn" :disabled="busy" @click="run(api.exportData)"><ClipIcon name="download" class="tiny" />导出</button>
        </div>
        <div class="row">
          <div class="label"><div class="name">导入备份</div><div class="desc">从 .ckbak 合并导入，已存在的不重复。</div></div>
          <button class="btn" :disabled="busy" @click="run(api.importData)"><ClipIcon name="upload" class="tiny" />导入</button>
        </div>
        <div class="row">
          <div class="label"><div class="name">数据目录</div><div class="desc sel">{{ view.dataDir }}</div></div>
        </div>
      </div>

      <h3><ClipIcon name="info" />关于</h3>
      <div ref="aboutEl" class="card">
        <div class="row update-row">
          <div class="label">
            <div class="name">版本 {{ appVersion }}<span v-if="newer" class="update-dot" aria-label="有新版本"></span></div>
            <div class="desc update-desc" :class="update.kind" aria-live="polite">{{ updateDesc }}</div>
            <template v-if="update.kind === 'downloading'">
              <div class="storage-track" role="progressbar" aria-label="下载进度" :aria-valuenow="Math.round(updatePercent)" aria-valuemin="0" aria-valuemax="100">
                <div class="storage-fill" :style="{ width: `${updatePercent}%` }"></div>
              </div>
              <div class="storage-caption">
                <span>{{ formatSize(update.received) }}{{ update.total ? ` / ${formatSize(update.total)}` : "" }}</span>
                <span>{{ Math.floor(updatePercent) }}%</span>
              </div>
            </template>
          </div>
          <div class="update-actions">
            <button
              v-for="a in updateActions"
              :key="a.label"
              class="btn"
              :class="{ accent: a.accent }"
              :disabled="a.busy"
              @click="a.run()"
            >
              <ClipIcon :name="a.icon" class="tiny" :class="{ spin: a.busy }" />{{ a.label }}
            </button>
          </div>
        </div>
        <label class="row">
          <div class="label">
            <div class="name">启动时检查更新</div>
            <div class="desc">启动约 15 秒后检查一次。有新版本时托盘图标显示红点，不会自动下载或安装。</div>
          </div>
          <input type="checkbox" class="switch" :checked="s.checkUpdateOnStart" @change="save({ checkUpdateOnStart: !s.checkUpdateOnStart })" />
        </label>
      </div>

      <h3 class="danger-heading"><ClipIcon name="trash" />清空记录</h3>
      <div class="card danger-card">
        <div v-if="!clearOpen" class="row">
          <div class="label"><div class="name">清空历史</div><div class="desc">默认只清未收藏的历史；可勾选扩大范围。</div></div>
          <button class="btn danger" @click="clearOpen = true"><ClipIcon name="trash" class="tiny" />清空</button>
        </div>
        <template v-else>
          <label class="row check"><input type="checkbox" v-model="clearFlags.pinned" :disabled="clearFlags.all" /><span>同时删除置顶数据</span></label>
          <label class="row check"><input type="checkbox" v-model="clearFlags.grouped" :disabled="clearFlags.all" /><span>同时删除分组数据</span></label>
          <label class="row check danger-check"><input type="checkbox" v-model="clearFlags.all" /><span>删除所有数据（含收藏）</span></label>
          <div class="row confirm">
            <p class="desc">{{ clearSummary }}</p>
            <div class="confirm-btns">
              <button class="btn" @click="clearOpen = false">取消</button>
              <button v-if="!clearArmed" class="btn danger" @click="armClear">清空</button>
              <button v-else class="btn danger solid" @click="confirmClear">确认删除？再点一次</button>
            </div>
          </div>
        </template>
      </div>

      <p class="foot">clipkeep · 数据默认只保存在本机 · 局域网文本传送需手动启用</p>
    </div>
  </div>
</template>

<style scoped>
.settings {
  display: flex;
  flex-direction: column;
  height: 100%;
}
.topbar {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 9px 10px 8px;
  border-bottom: 1px solid var(--border);
}
.title {
  font-weight: 600;
  font-size: var(--fs-lg);
}
.title-icon { width: 16px; height: 16px; color: var(--accent); }
.spacer {
  flex: 1;
  align-self: stretch;
  cursor: grab;
}
.scroll {
  flex: 1;
  overflow-y: auto;
  padding: 2px 12px 16px;
}
h3 {
  display: flex;
  align-items: center;
  gap: 7px;
  margin: 16px 2px 7px;
  font-size: var(--fs-xs);
  font-weight: 600;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--muted);
}
h3 svg { width: 14px; height: 14px; }
h3.danger-heading { color: var(--danger); }
.card {
  background: var(--surface-2);
  border: 1px solid var(--border);
  border-radius: var(--r-md);
  overflow: hidden;
}
.danger-card {
  border-color: var(--danger-soft);
}
.row {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 11px 13px;
}
.row + .row,
.check + .check,
.check + .confirm {
  border-top: 1px solid var(--border);
}
.storage-overview {
  display: block;
  padding-top: 14px;
  padding-bottom: 12px;
}
.storage-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  flex-wrap: wrap;
  gap: 8px 16px;
}
.storage-primary,
.storage-limit {
  display: flex;
  flex-direction: column;
  gap: 3px;
}
.storage-limit { text-align: right; }
.storage-label,
.storage-limit span {
  color: var(--muted);
  font-size: var(--fs-xs);
}
.storage-value {
  color: var(--text);
  font-size: 20px;
  line-height: 1.15;
  font-weight: 650;
  font-variant-numeric: tabular-nums;
}
.storage-limit strong {
  color: var(--text-2);
  font-size: var(--fs-md);
  font-weight: 600;
  font-variant-numeric: tabular-nums;
}
.storage-unlimited {
  align-self: center;
  padding: 4px 9px;
  border-radius: var(--r-pill);
  background: var(--accent-soft);
  color: var(--accent);
  font-size: var(--fs-xs);
  font-weight: 600;
  white-space: nowrap;
}
.storage-track {
  height: 8px;
  margin-top: 12px;
  overflow: hidden;
  border-radius: var(--r-pill);
  background: var(--border-strong);
}
.storage-fill {
  height: 100%;
  min-width: 0;
  border-radius: inherit;
  background: var(--accent);
}
.update-row { align-items: flex-start; }
.update-row .label { flex: 1; min-width: 0; }
.update-row .storage-track { margin-top: 8px; }
.update-actions { display: flex; gap: 6px; flex-shrink: 0; }
.update-dot {
  display: inline-block;
  width: 7px;
  height: 7px;
  margin-left: 6px;
  vertical-align: middle;
  border-radius: 50%;
  background: #e5484d;
}
.storage-caption {
  display: flex;
  justify-content: space-between;
  gap: 8px;
  margin: 6px 0 0;
  color: var(--muted);
  font-size: var(--fs-xs);
  line-height: 1.4;
}
.storage-details {
  display: flex;
  flex-wrap: wrap;
  gap: 5px 16px;
  margin-top: 11px;
  padding-top: 9px;
  border-top: 1px solid var(--border);
  color: var(--muted);
  font-size: var(--fs-xs);
}
.storage-details strong {
  color: var(--text-2);
  font-weight: 600;
  font-variant-numeric: tabular-nums;
}
.storage-note {
  margin: 7px 0 0;
  color: var(--muted);
  font-size: var(--fs-xs);
  line-height: 1.5;
}
.storage-unavailable {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  color: var(--danger);
  font-size: var(--fs-sm);
}
.storage-loading {
  margin: 0;
  color: var(--muted);
  font-size: var(--fs-sm);
}
label.row {
  cursor: pointer;
}
.label {
  flex: 1;
  min-width: 0;
}
.name {
  font-weight: 500;
}
.desc {
  margin: 3px 0 0;
  color: var(--muted);
  font-size: var(--fs-sm);
  line-height: 1.55;
}
.sel {
  user-select: text;
  word-break: break-all;
}
.desc b {
  color: var(--text-2);
  font-weight: 600;
}
code {
  font-family: "Cascadia Mono", Consolas, monospace;
  font-size: 10.5px;
  padding: 0 3px;
  border-radius: 4px;
  background: var(--hover);
}
.hint {
  margin: 0;
  padding: 11px 13px;
  border-top: 1px solid var(--border);
  color: var(--muted);
  font-size: var(--fs-sm);
  line-height: 2.1;
}
.banner {
  display: flex;
  align-items: center;
  gap: 6px;
  margin: 10px 0 0;
  padding: 9px 11px;
  border-radius: var(--r-sm);
  font-size: var(--fs-sm);
}
.error {
  background: var(--danger-soft);
  color: var(--danger);
}
.notice {
  background: color-mix(in srgb, var(--ok) 15%, transparent);
  color: var(--ok);
}
.tiny {
  width: 13px;
  height: 13px;
}

select,
.num input {
  color: var(--text);
  background: var(--surface);
  border: 1px solid var(--border-strong);
  border-radius: var(--r-sm);
  padding: 5px 9px;
  outline: none;
}
.lan-code-row,
.lan-form { flex-direction: column; align-items: stretch; }
.lan-addresses { display: flex; flex-wrap: wrap; gap: 6px; }
.lan-field { display: flex; flex-direction: column; gap: 4px; color: var(--text-2); font-size: var(--fs-xs); }
.lan-peer-name, .lan-peer-address { overflow-wrap: anywhere; }
.lan-input {
  width: 100%;
  padding: 7px 9px;
  border: 1px solid var(--border-strong);
  border-radius: var(--r-sm);
  outline: none;
  background: var(--surface);
  color: var(--text);
}
.lan-input:focus { border-color: var(--accent); }
.lan-add { align-self: flex-end; }
select:focus,
.num input:focus {
  border-color: var(--accent);
}
select:disabled {
  opacity: 0.5;
}
.num {
  display: flex;
  align-items: center;
  gap: 6px;
  color: var(--muted);
}
.num input {
  width: 84px;
  text-align: right;
}

.btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 6px 13px;
  border-radius: var(--r-sm);
  border: 1px solid var(--border-strong);
  color: var(--text);
  white-space: nowrap;
  transition: background 0.14s var(--ease);
}
.btn:hover:not(:disabled) {
  background: var(--hover);
}
.segmented {
  display: inline-flex;
  flex-shrink: 0;
  padding: 2px;
  gap: 2px;
  border-radius: var(--r-sm);
  background: var(--hover);
  border: 1px solid var(--border);
}
.segmented button {
  padding: 4px 9px;
  border-radius: 4px;
  color: var(--text-2);
  font-size: var(--fs-sm);
  white-space: nowrap;
  transition: background 0.14s var(--ease), color 0.14s var(--ease);
}
.segmented button:hover:not(.active) {
  color: var(--text);
}
.segmented button.active {
  background: var(--elevated);
  color: var(--text);
  box-shadow: var(--shadow-sm), 0 0 0 1px var(--border-strong);
}
.btn:disabled {
  opacity: 0.5;
  cursor: default;
}
.btn.danger {
  color: var(--danger);
  border-color: var(--danger-soft);
}
.btn.danger:hover {
  background: var(--danger-soft);
}
.btn.danger.solid {
  background: var(--danger);
  color: #fff;
  border-color: var(--danger);
}
.check {
  gap: 9px;
}
.check span {
  flex: 1;
}
.check input {
  width: 16px;
  height: 16px;
  accent-color: var(--accent);
}
.danger-check span {
  color: var(--danger);
}
.confirm {
  flex-direction: column;
  align-items: stretch;
  gap: 9px;
}
.confirm .desc {
  margin: 0;
}
.confirm-btns {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}
.foot {
  margin: 20px 0 4px;
  text-align: center;
  color: var(--muted);
  font-size: var(--fs-xs);
}
.update-desc.upToDate { color: var(--ok); }
.update-desc.available { color: var(--accent); }
.update-desc.error { color: var(--danger); }
.btn.accent {
  color: var(--accent);
  border-color: var(--accent);
  background: var(--accent-soft);
}
.btn.accent:hover:not(:disabled) { background: var(--accent-soft); filter: brightness(0.97); }
.btn:disabled:has(.spin) { cursor: progress; }
.spin { animation: settings-spin 0.9s linear infinite; }
@keyframes settings-spin { to { transform: rotate(360deg); } }
@media (prefers-reduced-motion: reduce) { .spin { animation: none; } }

.fade-enter-active,
.fade-leave-active {
  transition: opacity 0.2s var(--ease);
}
.fade-enter-from,
.fade-leave-to {
  opacity: 0;
}
</style>
