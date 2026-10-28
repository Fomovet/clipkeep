import { computed, onMounted, onUnmounted, ref } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { api, getVersion, type UpdateState } from "./api";

/** Live view of the backend update state, shared by the settings page, tray menu and topbar. */
export function useUpdate() {
  const version = ref("");
  const state = ref<UpdateState>({ kind: "idle" });
  let unlisten: UnlistenFn | undefined;
  let disposed = false;

  onMounted(async () => {
    const off = await listen<UpdateState>("update-state", (e) => (state.value = e.payload));
    if (disposed) return off();
    unlisten = off;
    const [v, s] = await Promise.all([getVersion().catch(() => ""), api.getUpdateState()]);
    version.value = v;
    state.value = s;
  });
  onUnmounted(() => {
    disposed = true;
    unlisten?.();
  });

  /** A newer release is known (announced, downloading, downloaded, or its download failed). */
  const newer = computed(() => {
    const s = state.value;
    return s.kind === "available" || s.kind === "downloading" || s.kind === "ready"
      ? s.version
      : s.kind === "error"
        ? s.version
        : null;
  });

  return { version, state, newer };
}

export function formatSize(bytes: number) {
  return bytes >= 1024 * 1024 ? `${(bytes / 1024 / 1024).toFixed(1)} MB` : `${Math.round(bytes / 1024)} KB`;
}
