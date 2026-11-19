<script setup lang="ts">
import { nextTick, onMounted, ref, watch } from "vue";
import { api } from "../api";
import ClipIcon from "./ClipIcon.vue";

const props = defineProps<{
  mode: "first-run" | "recovery";
  trusted: boolean;
  hint: string;
  busy: boolean;
}>();

defineEmits<{
  openSettings: [];
  confirm: [];
  dismiss: [];
  finish: [];
}>();

const primary = ref<HTMLButtonElement | null>(null);

function focusPrimary() {
  nextTick(() => primary.value?.focus());
}

onMounted(focusPrimary);
watch(() => props.trusted, (trusted) => {
  if (trusted) focusPrimary();
});
</script>

<template>
  <section class="guide" role="dialog" aria-modal="true" aria-labelledby="accessibility-guide-title">
    <header class="head" @mousedown.left="api.startDrag()">
      <ClipIcon :name="trusted ? 'check' : 'info'" />
      <h2 id="accessibility-guide-title">{{ trusted ? "辅助功能已开启" : "自动粘贴需要辅助功能" }}</h2>
    </header>

    <div class="body">
      <template v-if="trusted">
        <p>点击一条记录，clipkeep 会把它粘贴回你刚才使用的应用。</p>
      </template>
      <template v-else>
        <p v-if="mode === 'recovery'">这次的内容已经复制，可以先按 ⌘V 粘贴。要恢复点击即粘贴，需要重新允许辅助功能。</p>
        <p v-else>点击一条记录时，clipkeep 会模拟按下 ⌘V，把内容粘贴回你刚才使用的应用。macOS 将此视为辅助功能。</p>
        <p>记录、搜索和复制不需要这个权限。未授权时，内容仍会复制到剪贴板，可以自己按 ⌘V。</p>
        <ol>
          <li>如果系统弹出「clipkeep 想要控制这台电脑」，点「打开系统设置」。</li>
          <li>进入「系统设置 → 隐私与安全性 → 辅助功能」，找到 clipkeep，打开旁边的开关。</li>
          <li>回到 clipkeep，点「我已授权」。</li>
        </ol>
        <p v-if="hint" class="hint" role="status">{{ hint }}</p>
        <p class="note">选择「稍后再说」后，记录和搜索可以照常使用。下次点击记录粘贴时，会再次出现这个说明。</p>
        <p class="note">安装包未经 Apple 公证。如果以后某个版本在启动前被拦住，先双击一次「应用程序」里的 clipkeep，再打开「系统设置 → 隐私与安全性」，点「仍要打开」，并在确认框里再确认一次。</p>
      </template>
    </div>

    <footer class="foot">
      <button v-if="trusted" ref="primary" type="button" class="primary" @click="$emit('finish')">开始使用</button>
      <template v-else>
        <button ref="primary" type="button" class="primary" @click="$emit('openSettings')">打开系统设置</button>
        <button type="button" :disabled="busy" @click="$emit('confirm')">我已授权</button>
        <button type="button" class="quiet" :disabled="busy" @click="$emit('dismiss')">稍后再说</button>
      </template>
    </footer>
  </section>
</template>

<style scoped>
.guide {
  position: absolute;
  inset: 0;
  z-index: 60;
  display: flex;
  flex-direction: column;
  background: var(--surface);
}
.head {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 12px 14px 6px;
  cursor: grab;
}
.head h2 {
  margin: 0;
  font-size: var(--fs-lg);
  font-weight: 650;
  line-height: 1.35;
  user-select: none;
}
.head svg { width: 18px; height: 18px; color: var(--accent); }
.body {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: 6px 16px 8px;
  color: var(--text-2);
  font-size: var(--fs-sm);
  line-height: 1.55;
  user-select: text;
}
.body p { margin: 0 0 10px; }
ol {
  margin: 0 0 12px;
  padding-left: 1.25em;
}
li + li { margin-top: 6px; }
.hint {
  padding: 8px 10px;
  border-radius: var(--r-sm);
  background: var(--surface-2);
  color: var(--text);
}
.note {
  color: var(--muted);
  font-size: var(--fs-xs);
  line-height: 1.55;
}
.foot {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  padding: 10px 14px 14px;
  border-top: 1px solid var(--border);
}
button {
  min-height: 30px;
  padding: 5px 10px;
  border: 1px solid var(--border-strong);
  border-radius: var(--r-sm);
  background: var(--surface-2);
  color: var(--text);
  font-size: var(--fs-sm);
  font-weight: 600;
}
button:hover:not(:disabled) { background: var(--hover); }
button:disabled { opacity: 0.55; cursor: default; }
.primary {
  border-color: var(--accent);
  background: var(--accent-soft);
  color: var(--accent);
}
.primary:hover:not(:disabled) { background: var(--accent-soft); filter: brightness(0.97); }
.quiet { color: var(--text-2); font-weight: 500; }
</style>
