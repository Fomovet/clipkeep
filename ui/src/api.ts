import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";

export interface Clip {
  id: number;
  kind: "text" | "image";
  preview: string;
  size: number;
  truncated: boolean;
  width: number | null;
  height: number | null;
  favorite: boolean;
  pinned: boolean;
  updatedAt: number;
  source: string | null;
  groupId: number | null;
  thumb: string | null;
}

export interface Group {
  id: number;
  name: string;
  sortOrder: number;
}

export interface Settings {
  hotkeyEnabled: boolean;
  hotkey: string;
  captureImages: boolean;
  rememberPosition: boolean;
  keepOnBlur: boolean;
  windowX: number | null;
  windowY: number | null;
  maxStorageMb: number;
  theme: Theme;
  checkUpdateOnStart: boolean;
  pasteImageAsFile: boolean;
}

export type Theme = "system" | "light" | "dark";

export interface SettingsView {
  settings: Settings;
  hotkeys: { id: string; label: string }[];
  dataDir: string;
}

export interface StorageUsage {
  ordinaryBytes: number;
  protectedBytes: number;
  databaseBytes: number;
}

export interface LanPeer {
  name: string;
  address: string;
}

export interface LanState {
  receiveEnabled: boolean;
  localCodes: { address: string; code: string }[];
  peers: LanPeer[];
}

export type Scope = "all" | "fav" | "group";

export const api = {
  listClips: (scope: Scope, groupId: number | null, query: string) =>
    invoke<Clip[]>("list_clips", { scope, groupId, query }),
  getClipText: (id: number) => invoke<string>("get_clip_text", { id }),
  updateClipText: (id: number, text: string) => invoke<void>("update_clip_text", { id, text }),
  getCapturePaused: () => invoke<boolean>("get_capture_paused"),
  toggleCapturePaused: () => invoke<boolean>("toggle_capture_paused"),
  getLanState: () => invoke<LanState>("get_lan_state"),
  setLanReceive: (enabled: boolean) => invoke<LanState>("set_lan_receive", { enabled }),
  addLanPeer: (name: string, code: string) => invoke<LanState>("add_lan_peer", { name, code }),
  removeLanPeer: (address: string) => invoke<LanState>("remove_lan_peer", { address }),
  rotateLanCode: () => invoke<LanState>("rotate_lan_code"),
  copyLanCode: (address: string) => invoke<void>("copy_lan_code", { address }),
  sendClipToPeer: (id: number, address: string) => invoke<void>("send_clip_to_peer", { id, address }),
  toggleFavorite: (id: number) => invoke<boolean>("toggle_favorite", { id }),
  togglePinned: (id: number) => invoke<boolean>("toggle_pinned", { id }),
  moveClip: (id: number, groupId: number | null) => invoke<void>("move_clip", { id, groupId }),
  deleteClip: (id: number) => invoke<void>("delete_clip", { id }),
  clearData: (includePinned: boolean, includeGrouped: boolean, includeAll: boolean) =>
    invoke<void>("clear_data", { includePinned, includeGrouped, includeAll }),
  listGroups: () => invoke<Group[]>("list_groups"),
  createGroup: (name: string) => invoke<Group>("create_group", { name }),
  renameGroup: (id: number, name: string) => invoke<void>("rename_group", { id, name }),
  deleteGroup: (id: number, deleteItems: boolean) => invoke<void>("delete_group", { id, deleteItems }),
  reorderGroups: (ids: number[]) => invoke<void>("reorder_groups", { ids }),
  pasteClip: (id: number) => invoke<void>("paste_clip", { id }),
  copyClip: (id: number) => invoke<void>("copy_clip", { id }),
  hideWindow: () => invoke<void>("hide_window"),
  startDrag: () => invoke<void>("start_drag"),
  toggleKeepOnBlur: () => invoke<boolean>("toggle_keep_on_blur"),
  getAutostart: () => invoke<boolean>("get_autostart"),
  setAutostart: (enabled: boolean) => invoke<boolean>("set_autostart", { enabled }),
  getSettings: () => invoke<SettingsView>("get_settings"),
  getStorageUsage: () => invoke<StorageUsage>("get_storage_usage"),
  updateSettings: (settings: Settings) => invoke<Settings>("update_settings", { settings }),
  exportData: () => invoke<string | null>("export_data"),
  importData: () => invoke<string | null>("import_data"),
  // In the browser, never inside our WebView: following a link there replaces the app's page.
  openReleasePage: () => invoke<void>("open_release_page"),
  // Checking, downloading and installing run in the backend and report through `update-state`.
  getUpdateState: () => invoke<UpdateState>("get_update_state"),
  checkUpdate: () => invoke<void>("check_update"),
  downloadUpdate: () => invoke<void>("download_update"),
  cancelUpdate: () => invoke<void>("cancel_update"),
  installUpdate: () => invoke<void>("install_update"),
};

export { getVersion };

/** Mirrors `update::State` in the backend. */
export type UpdateState =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "upToDate" }
  | { kind: "available"; version: string; size: number; portable: boolean }
  | { kind: "downloading"; version: string; received: number; total: number }
  | { kind: "ready"; version: string }
  | { kind: "error"; message: string; version: string | null };

// The chosen theme is cached so the next window paints in it before settings load.
const THEME_KEY = "clipkeep.theme";
const systemDark = window.matchMedia("(prefers-color-scheme: dark)");
let currentTheme: Theme = "system";

function paintTheme() {
  const dark = currentTheme === "dark" || (currentTheme === "system" && systemDark.matches);
  document.documentElement.dataset.theme = dark ? "dark" : "light";
}

export function applyTheme(theme: Theme) {
  currentTheme = theme;
  paintTheme();
  try {
    localStorage.setItem(THEME_KEY, theme);
  } catch {
    // The cache only avoids a flash on open.
  }
}

export function initTheme() {
  try {
    const cached = localStorage.getItem(THEME_KEY);
    if (cached === "light" || cached === "dark" || cached === "system") currentTheme = cached;
  } catch {
    // Fall back to following the system.
  }
  paintTheme();
  systemDark.addEventListener("change", paintTheme);
}

// Navigation state belongs to the frontend, not backend settings.
const UI_KEY = "clipkeep.ui";

export interface UiState {
  scope: Scope;
  groupId: number | null;
  scrollTop: number;
}

export function loadUi(): UiState {
  try {
    const raw = localStorage.getItem(UI_KEY);
    if (raw) return { scope: "all", groupId: null, scrollTop: 0, ...JSON.parse(raw) };
  } catch {
    // Corrupt or unavailable browser storage should not prevent the window from opening.
  }
  return { scope: "all", groupId: null, scrollTop: 0 };
}

export function saveUi(state: Partial<UiState>) {
  try {
    localStorage.setItem(UI_KEY, JSON.stringify({ ...loadUi(), ...state }));
  } catch {
    // UI preferences are best effort.
  }
}
