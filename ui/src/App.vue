<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, reactive, ref, watch } from "vue";
import { convertFileSrc } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { api, applyTheme, loadUi, saveUi, type Clip, type Group, type LanPeer, type Scope } from "./api";
import SettingsView from "./components/SettingsView.vue";
import ClipIcon from "./components/ClipIcon.vue";
import { useUpdate } from "./update";

const page = ref<"list" | "settings">("list");
const { newer: newerVersion } = useUpdate();
// Set when Settings is opened from the tray's "new version" row, to scroll to that card.
const focusAbout = ref(false);
const scope = ref<Scope>("all");
const activeGroup = ref<number | null>(null);
const query = ref("");
const clips = ref<Clip[]>([]);
// Rows are rendered in steps: all 1000 cost the renderer ~40MB while a screenful is visible.
const RENDER_STEP = 100;
// Shortest row (one line of text) plus its gap, for working out how many rows a scroll offset spans.
const MIN_ROW_HEIGHT = 74;
const renderLimit = ref(RENDER_STEP);
const visibleClips = computed(() => clips.value.slice(0, renderLimit.value));
const groups = ref<Group[]>([]);
const selected = ref(0);
const pinnedWindow = ref(false);
const searchEl = ref<HTMLInputElement | null>(null);
const listEl = ref<HTMLElement | null>(null);
const listScrolled = ref(false);
const groupPicker = reactive({ open: false, x: 0, y: 0 });
const groupPickerSearchEl = ref<HTMLInputElement | null>(null);
const groupFilter = ref("");
const moveFilter = ref("");

const groupInput = ref<HTMLInputElement | null>(null);
const groupEditor = reactive({ open: false, mode: "create" as "create" | "rename", groupId: 0, name: "", error: "", busy: false, x: 0, y: 0 });
const groupMenu = reactive({ open: false, groupId: 0, confirming: false, x: 0, y: 0 });
const moveMenu = reactive({ open: false, clipId: 0, x: 0, y: 0 });
const sendMenu = reactive({ open: false, clipId: 0, x: 0, y: 0 });
const clipMenu = reactive({ open: false, clipId: 0, x: 0, y: 0 });
let menuGeneration = 0;
let windowShown = false;
const menuClip = computed(() => clips.value.find((c) => c.id === clipMenu.clipId));
const moveClip = computed(() => clips.value.find((c) => c.id === moveMenu.clipId));
const menuGroup = computed(() => groups.value.find((g) => g.id === groupMenu.groupId));
const visibleGroups = computed(() => {
  const filter = groupFilter.value.trim().toLocaleLowerCase();
  return groups.value.map((group, index) => ({ group, index })).filter(({ group }) => !filter || group.name.toLocaleLowerCase().includes(filter));
});
const visibleMoveGroups = computed(() => {
  const filter = moveFilter.value.trim().toLocaleLowerCase();
  return groups.value.filter((group) => !filter || group.name.toLocaleLowerCase().includes(filter));
});
const lanPeers = ref<LanPeer[]>([]);
const sendLoading = ref(false);
const sendBusy = ref(false);
const sendTarget = ref("");
const sendError = ref("");
const sendLoadError = ref(false);
const paused = ref(false);
const notice = ref("");
let noticeTimer: number | undefined;
const previewClip = ref<Clip | null>(null);
const fullText = ref("");
const draftText = ref("");
const editing = ref(false);
const previewBusy = ref(false);
const previewError = ref("");
const editorEl = ref<HTMLTextAreaElement | null>(null);
let previewGeneration = 0;
let dragIndex = -1;
let searchTimer: number | undefined;
let scrollTimer: number | undefined;
let restoreScroll = 0;
const unlisteners: UnlistenFn[] = [];

const groupName = (id: number | null) => groups.value.find((g) => g.id === id)?.name ?? null;

let loadSeq = 0;
async function reload(keepScroll = false) {
  const seq = ++loadSeq;
  const prev = keepScroll ? (listEl.value?.scrollTop ?? 0) : 0;
  const rows = await api.listClips(scope.value, activeGroup.value, query.value);
  if (seq !== loadSeq) return;
  // A refresh keeps the rows already rendered; a fresh load renders enough to reach the
  // remembered scroll position.
  if (!keepScroll) {
    const viewport = listEl.value?.clientHeight ?? 0;
    renderLimit.value = Math.max(RENDER_STEP, Math.ceil((restoreScroll + viewport) / MIN_ROW_HEIGHT) + RENDER_STEP);
  }
  clips.value = rows;
  if (selected.value >= rows.length) selected.value = Math.max(0, rows.length - 1);
  nextTick(() => {
    if (listEl.value) listEl.value.scrollTop = keepScroll ? prev : restoreScroll;
    listScrolled.value = (listEl.value?.scrollTop ?? 0) > 0;
    restoreScroll = 0;
  });
}

async function reloadGroups() {
  groups.value = await api.listGroups();
  if (scope.value === "group" && !groups.value.some((g) => g.id === activeGroup.value)) selectScope("all");
}

function selectScope(s: Scope, groupId: number | null = null) {
  scope.value = s;
  activeGroup.value = groupId;
  saveUi({ scope: s, groupId });
}

watch([scope, activeGroup], () => {
  selected.value = 0;
  reload();
});
// Debounced so a full-text LIKE scan doesn't run on every keystroke.
watch(query, () => {
  selected.value = 0;
  window.clearTimeout(searchTimer);
  searchTimer = window.setTimeout(reload, 180);
});

const paste = (c?: Clip) => c && api.pasteClip(c.id);
const copyOnly = (c?: Clip) => c && api.copyClip(c.id);
const remove = (c?: Clip) => c && api.deleteClip(c.id);
const toggleFav = (c?: Clip) => c && api.toggleFavorite(c.id);
const togglePin = (c?: Clip) => c && api.togglePinned(c.id);

function flash(message: string) {
  notice.value = message;
  window.clearTimeout(noticeTimer);
  noticeTimer = window.setTimeout(() => (notice.value = ""), 4500);
}

async function openPreview(c: Clip) {
  if (c.kind !== "text") return;
  const generation = ++previewGeneration;
  previewClip.value = c;
  fullText.value = "";
  previewError.value = "";
  editing.value = false;
  previewBusy.value = true;
  try {
    const text = await api.getClipText(c.id);
    if (generation !== previewGeneration) return;
    fullText.value = text;
    draftText.value = text;
  } catch (e) {
    if (generation === previewGeneration) previewError.value = String(e);
  } finally {
    if (generation === previewGeneration) previewBusy.value = false;
  }
}

function closePreview() {
  if (editing.value) {
    editing.value = false;
    draftText.value = fullText.value;
  } else {
    previewGeneration++;
    previewClip.value = null;
    nextTick(() => searchEl.value?.focus());
  }
}

function startEditing() {
  draftText.value = fullText.value;
  editing.value = true;
  nextTick(() => editorEl.value?.focus());
}

async function savePreview() {
  if (!previewClip.value || previewBusy.value) return;
  const generation = previewGeneration;
  const id = previewClip.value.id;
  const text = draftText.value;
  previewBusy.value = true;
  previewError.value = "";
  try {
    await api.updateClipText(id, text);
    if (generation !== previewGeneration) return;
    fullText.value = text;
    editing.value = false;
    flash("文本已更新");
  } catch (e) {
    if (generation === previewGeneration) previewError.value = String(e);
  } finally {
    if (generation === previewGeneration) previewBusy.value = false;
  }
}

// `focus` moves into the menu once loaded; a menu opened by hovering leaves focus on its parent.
async function loadSendPeers(generation: number, focus: boolean) {
  sendLoading.value = true;
  sendLoadError.value = false;
  sendError.value = "";
  try {
    const peers = (await api.getLanState()).peers;
    if (generation !== menuGeneration) return;
    lanPeers.value = peers;
  } catch (error) {
    if (generation !== menuGeneration) return;
    sendLoadError.value = true;
    sendError.value = `读取设备失败：${error}`;
  } finally {
    if (generation === menuGeneration) {
      sendLoading.value = false;
      fitMenu(sendMenu, ".send-menu");
      nextTick(() => {
        if (focus && generation === menuGeneration && sendMenu.open)
          (document.querySelector(".send-menu .menu-row:not(:disabled)") as HTMLButtonElement | null)?.focus();
      });
    }
  }
}

function openSendMenu(c: Clip, row: HTMLElement, focus: boolean) {
  closeSubmenus();
  const generation = menuGeneration;
  sendMenu.clipId = c.id;
  lanPeers.value = [];
  sendLoading.value = true;
  sendError.value = "";
  sendLoadError.value = false;
  sendMenu.open = true;
  placeSubmenu(sendMenu, row, ".send-menu");
  if (focus) {
    nextTick(() => {
      if (generation === menuGeneration) (document.querySelector(".send-menu") as HTMLElement | null)?.focus();
    });
  }
  void loadSendPeers(generation, focus);
}

async function sendTo(peer: LanPeer) {
  if (sendBusy.value || sendLoading.value) return;
  const generation = menuGeneration;
  const id = sendMenu.clipId;
  sendBusy.value = true;
  sendTarget.value = peer.name;
  sendError.value = "";
  fitMenu(sendMenu, ".send-menu");
  try {
    await api.sendClipToPeer(id, peer.address);
    if (generation === menuGeneration) closeMenus();
    if (windowShown && page.value === "list") flash(`已发送到 ${peer.name}`);
  } catch (e) {
    if (generation === menuGeneration) {
      sendError.value = `发送失败：${e}`;
      fitMenu(sendMenu, ".send-menu");
    } else if (windowShown && page.value === "list") flash(`发送失败：${e}`);
  } finally {
    sendBusy.value = false;
    sendTarget.value = "";
  }
}

function openLanSettings() {
  closeMenus();
  openSettings();
}

async function togglePinnedWindow() {
  pinnedWindow.value = await api.toggleKeepOnBlur();
}

function showGroupEditor(mode: "create" | "rename", group: Group | null, x: number, y: number) {
  closeMenus();
  groupEditor.mode = mode;
  groupEditor.groupId = group?.id ?? 0;
  groupEditor.name = group?.name ?? "";
  groupEditor.error = "";
  groupEditor.busy = false;
  groupEditor.open = true;
  placeMenu(groupEditor, x, y, ".group-editor");
  nextTick(() => groupInput.value?.select());
}

function openNewGroup(e: MouseEvent) {
  e.stopPropagation();
  const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
  showGroupEditor("create", null, r.right - 228, r.bottom + 5);
}

function openGroupPicker(e: MouseEvent) {
  e.stopPropagation();
  if (groupPicker.open) {
    groupPicker.open = false;
    return;
  }
  closeMenus();
  groupFilter.value = "";
  const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
  groupPicker.open = true;
  placeMenu(groupPicker, r.left, r.bottom + 5, ".group-picker");
  nextTick(() => {
    if (groups.value.length >= 7) groupPickerSearchEl.value?.focus();
    else (document.querySelector(".group-picker .group-picker-row.active .group-picker-name, .group-picker .group-picker-name, .group-picker .group-picker-add") as HTMLButtonElement | null)?.focus();
  });
}

function chooseGroup(id: number) {
  closeMenus();
  selectScope("group", id);
}

function cancelGroupEditor() {
  groupEditor.open = false;
  nextTick(() => searchEl.value?.focus());
}

function renameFromGroupMenu() {
  const group = menuGroup.value;
  if (group) showGroupEditor("rename", group, groupMenu.x, groupMenu.y);
}

async function saveGroup() {
  if (groupEditor.busy) return;
  const name = groupEditor.name.trim();
  if (!name) {
    groupEditor.error = "请输入分组名称";
    groupInput.value?.focus();
    return;
  }
  if (groups.value.some((g) => g.id !== groupEditor.groupId && g.name.trim().toLocaleLowerCase() === name.toLocaleLowerCase())) {
    groupEditor.error = "已有同名分组";
    groupInput.value?.focus();
    return;
  }
  groupEditor.busy = true;
  groupEditor.error = "";
  try {
    if (groupEditor.mode === "create") {
      const group = await api.createGroup(name);
      await reloadGroups();
      selectScope("group", group.id);
    } else {
      await api.renameGroup(groupEditor.groupId, name);
      await reloadGroups();
    }
    groupEditor.open = false;
  } catch (error) {
    groupEditor.error = String(error);
  } finally {
    groupEditor.busy = false;
  }
}

function openGroupMenu(g: Group, e: MouseEvent) {
  e.preventDefault();
  e.stopPropagation();
  closeMenus();
  groupMenu.groupId = g.id;
  groupMenu.confirming = false;
  groupMenu.open = true;
  if (e.type === "contextmenu") {
    placeMenu(groupMenu, e.clientX + 4, e.clientY + 4, ".group-menu", true);
  } else {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    placeMenu(groupMenu, r.right - 184, r.bottom + 5, ".group-menu");
  }
  nextTick(() => (document.querySelector(".group-menu .menu-row") as HTMLButtonElement | null)?.focus());
}

function requestDeleteGroup() {
  groupMenu.confirming = true;
  fitMenu(groupMenu, ".group-menu");
  nextTick(() => (document.querySelector(".group-menu .group-delete-confirm") as HTMLButtonElement | null)?.focus());
}

async function deleteGroup() {
  const group = menuGroup.value;
  if (!group) return;
  groupMenu.open = false;
  try {
    await api.deleteGroup(group.id, false);
    await reloadGroups();
    flash(`已删除分组「${group.name}」，条目仍保留`);
  } catch (error) {
    flash(`删除分组失败：${error}`);
  }
}

function onDragStart(i: number, e: DragEvent) {
  dragIndex = i;
  e.dataTransfer!.effectAllowed = "move";
}
async function onDrop(i: number) {
  if (dragIndex < 0 || dragIndex === i) return;
  const arr = [...groups.value];
  const [moved] = arr.splice(dragIndex, 1);
  arr.splice(i, 0, moved);
  groups.value = arr;
  dragIndex = -1;
  await api.reorderGroups(arr.map((g) => g.id));
}

function openMoveMenu(c: Clip, row: HTMLElement, focus: boolean) {
  closeSubmenus();
  moveFilter.value = "";
  moveMenu.open = true;
  moveMenu.clipId = c.id;
  placeSubmenu(moveMenu, row, ".move-menu");
  if (focus) nextTick(() => (document.querySelector(".move-menu .menu-search input, .move-menu .menu-row:not(:disabled)") as HTMLElement | null)?.focus());
}

// The clip menu's submenus cascade beside their row like native menus: the parent stays open,
// hovering a row opens its submenu after a short pause, and a click or → opens it at once.
type Submenu = "send" | "move";
const SUBMENU_HOVER_MS = 150;
let submenuTimer: number | undefined;
const openSubmenu = computed<Submenu | null>(() => (sendMenu.open ? "send" : moveMenu.open ? "move" : null));

function showSubmenu(kind: Submenu, row: HTMLElement, focus: boolean) {
  window.clearTimeout(submenuTimer);
  const c = menuClip.value;
  if (!c) return;
  if (kind === "send") openSendMenu(c, row, focus);
  else openMoveMenu(c, row, focus);
}

function closeSubmenus() {
  window.clearTimeout(submenuTimer);
  menuGeneration++;
  moveMenu.open = false;
  sendMenu.open = false;
  sendLoading.value = false;
  sendError.value = "";
  sendLoadError.value = false;
}

function onClipMenuHover(e: MouseEvent) {
  const row = (e.target as HTMLElement).closest(".menu-row") as HTMLElement | null;
  if (!row) return;
  const kind = (row.dataset.submenu as Submenu | undefined) ?? null;
  window.clearTimeout(submenuTimer);
  if (kind === openSubmenu.value) return;
  submenuTimer = window.setTimeout(() => {
    if (!clipMenu.open) return;
    if (kind) showSubmenu(kind, row, false);
    else closeSubmenus();
  }, SUBMENU_HOVER_MS);
}

// Reaching the submenu cancels a close scheduled by rows crossed on the way.
function keepSubmenu() {
  window.clearTimeout(submenuTimer);
}

function focusSubmenuRow(kind: Submenu) {
  nextTick(() => (document.querySelector(`.clip-menu [data-submenu="${kind}"]`) as HTMLElement | null)?.focus());
}

// Widest submenu (.move-menu), so the clip menu can keep a side free for it.
const SUBMENU_WIDTH = 196;
// Submenus tuck 4px under their parent's edge, as native menus do.
const SUBMENU_OVERLAP = 4;

function placeSubmenu(menu: { x: number; y: number }, row: HTMLElement, selector: string) {
  const parent = (row.closest(".menu") as HTMLElement).getBoundingClientRect();
  menu.x = parent.right - SUBMENU_OVERLAP;
  menu.y = row.getBoundingClientRect().top - 5;
  nextTick(() => {
    const el = document.querySelector(selector) as HTMLElement | null;
    if (!el) return;
    // Right of the parent when it fits, else left. Only a window narrower than both menus
    // together leaves the clamp below to overlap them.
    if (parent.right - SUBMENU_OVERLAP + el.offsetWidth > window.innerWidth - 8 && parent.left + SUBMENU_OVERLAP - el.offsetWidth >= 8) {
      menu.x = parent.left + SUBMENU_OVERLAP - el.offsetWidth;
    }
    menu.x = Math.max(8, Math.min(menu.x, window.innerWidth - el.offsetWidth - 8));
    menu.y = Math.max(8, Math.min(menu.y, window.innerHeight - el.offsetHeight - 8));
  });
}

// Shift the clip menu just enough that a submenu fits beside it on one side. Doing it when the
// menu opens keeps it from jumping once a submenu is hovered.
function leaveRoomForSubmenus(anchorX: number) {
  nextTick(() => {
    const el = document.querySelector(".clip-menu") as HTMLElement | null;
    if (!el) return;
    const width = el.offsetWidth;
    const maxRight = window.innerWidth - 8;
    const needRight = clipMenu.x + width - SUBMENU_OVERLAP + SUBMENU_WIDTH - maxRight;
    const needLeft = 8 - (clipMenu.x + SUBMENU_OVERLAP - SUBMENU_WIDTH);
    if (needRight <= 0 || needLeft <= 0) return;
    // Both menus side by side nearly fill this narrow window. Keep the menu under where it was
    // opened: submenus to the right when that was in the left half, else to the left.
    const towardRight = anchorX <= window.innerWidth / 2;
    clipMenu.x = towardRight ? Math.max(8, clipMenu.x - needRight) : Math.min(maxRight - width, clipMenu.x + needLeft);
  });
}

function dismissMenuBeforeItemClick(e: MouseEvent) {
  if (!clipMenu.open && !moveMenu.open && !sendMenu.open && !groupMenu.open && !groupEditor.open && !groupPicker.open) return;
  e.preventDefault();
  e.stopImmediatePropagation();
  closeMenus();
}

function closeMenus() {
  closeSubmenus();
  groupPicker.open = false;
  clipMenu.open = false;
  groupMenu.open = false;
  groupEditor.open = false;
}

function startWindowDrag() {
  // Native dragging can consume the click event used by the outside-click handler.
  closeMenus();
  void api.startDrag();
}

function fitMenu(menu: { x: number; y: number }, selector: string, fromPointer = false) {
  nextTick(() => {
    const el = document.querySelector(selector) as HTMLElement | null;
    if (!el) return;
    let x = menu.x;
    let y = menu.y;
    if (fromPointer) {
      // Keep the menu's nearest corner beside the pointer when the bottom or
      // right edge cannot fit the preferred placement.
      if (x + el.offsetWidth > window.innerWidth - 8) x -= el.offsetWidth + 8;
      if (y + el.offsetHeight > window.innerHeight - 8) y -= el.offsetHeight + 8;
    }
    menu.x = Math.max(8, Math.min(x, window.innerWidth - el.offsetWidth - 8));
    menu.y = Math.max(8, Math.min(y, window.innerHeight - el.offsetHeight - 8));
  });
}

function placeMenu(menu: { x: number; y: number }, x: number, y: number, selector: string, fromPointer = false) {
  menu.x = x;
  menu.y = y;
  fitMenu(menu, selector, fromPointer);
}

function openClipMenu(c: Clip, e: MouseEvent) {
  e.preventDefault();
  e.stopPropagation();
  closeMenus();
  clipMenu.clipId = c.id;
  clipMenu.open = true;
  selected.value = Math.max(0, clips.value.findIndex((row) => row.id === c.id));
  if (e.type === "contextmenu") {
    placeMenu(clipMenu, e.clientX + 4, e.clientY + 4, ".clip-menu", true);
    leaveRoomForSubmenus(e.clientX);
  } else {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    placeMenu(clipMenu, r.right - 188, r.bottom + 4, ".clip-menu");
    leaveRoomForSubmenus(r.left + r.width / 2);
  }
  nextTick(() => (document.querySelector(".clip-menu .menu-row") as HTMLButtonElement | null)?.focus());
}

function menuAction(action: "copy" | "preview" | "favorite" | "pin" | "delete") {
  const c = menuClip.value;
  closeMenus();
  if (!c) return;
  if (action === "copy") copyOnly(c);
  else if (action === "preview") openPreview(c);
  else if (action === "favorite") toggleFav(c);
  else if (action === "pin") togglePin(c);
  else remove(c);
}
async function moveTo(groupId: number | null) {
  const id = moveMenu.clipId;
  closeMenus();
  try {
    await api.moveClip(id, groupId);
  } catch (e) {
    flash(`移动失败：${e}`);
  }
}

function move(delta: number) {
  if (!clips.value.length) return;
  selected.value = Math.min(clips.value.length - 1, Math.max(0, selected.value + delta));
  nextTick(() => listEl.value?.children[selected.value]?.scrollIntoView({ block: "nearest" }));
}

const tabKeys = computed<Array<Scope | number>>(() => ["all", "fav", ...groups.value.map((g) => g.id)]);
function cycleTab(dir: number) {
  const keys = tabKeys.value;
  const cur = scope.value === "group" ? activeGroup.value! : scope.value;
  const next = keys[(keys.indexOf(cur) + dir + keys.length) % keys.length];
  if (next === "all" || next === "fav") selectScope(next);
  else selectScope("group", next as number);
}

function onKey(e: KeyboardEvent) {
  if (previewClip.value) {
    if (e.key === "Escape") {
      closePreview();
      e.preventDefault();
    } else if (editing.value && e.ctrlKey && e.key === "Enter") {
      savePreview();
      e.preventDefault();
    }
    return;
  }
  const submenu = openSubmenu.value;
  const active = document.activeElement as HTMLElement | null;
  // A hovered submenu can be open while focus is still on the parent menu.
  if (submenu && !active?.closest(".clip-menu")) {
    // Back out of the submenu to its row; the arrow is left alone while typing in its search box.
    if (e.key === "Escape" || (e.key === "ArrowLeft" && !(e.target instanceof HTMLInputElement))) {
      closeSubmenus();
      e.preventDefault();
      focusSubmenuRow(submenu);
    }
    return;
  }
  if (clipMenu.open) {
    const row = active;
    const kind = row?.dataset.submenu as Submenu | undefined;
    if (e.key === "Escape") {
      e.preventDefault();
      if (submenu) closeSubmenus();
      else {
        clipMenu.open = false;
        nextTick(() => searchEl.value?.focus());
      }
    } else if (kind && e.key === "ArrowRight") {
      showSubmenu(kind, row!, true);
      e.preventDefault();
    }
    return;
  }
  if (groupMenu.open) {
    if (e.key === "Escape") {
      groupMenu.open = false;
      e.preventDefault();
      nextTick(() => searchEl.value?.focus());
    }
    return;
  }
  if (groupEditor.open) {
    if (e.key === "Escape") {
      cancelGroupEditor();
      e.preventDefault();
    }
    return;
  }
  if (groupPicker.open) {
    if (e.key === "Escape") {
      groupPicker.open = false;
      e.preventDefault();
      nextTick(() => searchEl.value?.focus());
    }
    return;
  }
  if (page.value === "settings") {
    if (e.key === "Escape") {
      e.preventDefault();
      backToList();
    }
    return;
  }
  const current = clips.value[selected.value];
  const ctrl = e.ctrlKey || e.metaKey;
  const key = e.key.toLowerCase();
  let handled = true;
  if (e.key === "Escape") query.value ? (query.value = "") : hideWindow();
  else if (e.key === "ArrowDown") move(1);
  else if (e.key === "ArrowUp") move(-1);
  else if (e.key === "PageDown") move(8);
  else if (e.key === "PageUp") move(-8);
  else if (e.key === "Enter") e.shiftKey ? copyOnly(current) : paste(current);
  else if (e.key === "Tab") {
    e.preventDefault();
    cycleTab(e.shiftKey ? -1 : 1);
    return;
  } else if (ctrl && key === "p") togglePin(current);
  else if (ctrl && key === "s") toggleFav(current);
  else if (ctrl && key === "d") remove(current);
  else if (ctrl && /^[1-9]$/.test(e.key)) paste(clips.value[Number(e.key) - 1]);
  else handled = false;
  if (handled) e.preventDefault();
}

// Scrolling moves rows under a still cursor and Chromium reports that as mousemove. Only a real
// pointer move should take the selection, so the wheel doesn't drag it along row by row.
let lastPointer = { x: -1, y: -1 };
function onItemHover(i: number, e: MouseEvent) {
  if (e.screenX === lastPointer.x && e.screenY === lastPointer.y) return;
  lastPointer = { x: e.screenX, y: e.screenY };
  selected.value = i;
}

// Keyboard moves, previews and the like may select a row that isn't rendered yet.
watch(selected, (index) => {
  if (index >= renderLimit.value) renderLimit.value = index + RENDER_STEP;
});

function onScroll() {
  // Menus are fixed in place; once their row scrolls away they would point at nothing.
  if (clipMenu.open) closeMenus();
  const el = listEl.value;
  if (el && renderLimit.value < clips.value.length && el.scrollTop + el.clientHeight > el.scrollHeight - 600) {
    renderLimit.value += RENDER_STEP;
  }
  listScrolled.value = (listEl.value?.scrollTop ?? 0) > 0;
  window.clearTimeout(scrollTimer);
  scrollTimer = window.setTimeout(() => saveUi({ scrollTop: listEl.value?.scrollTop ?? 0 }), 200);
}

function backToTop() {
  if (!listEl.value) return;
  listEl.value.scrollTop = 0;
  selected.value = 0;
  listScrolled.value = false;
  window.clearTimeout(scrollTimer);
  saveUi({ scrollTop: 0 });
  nextTick(() => searchEl.value?.focus());
}

function onHidden() {
  windowShown = false;
  // Reset while hidden so the next show doesn't open on the old page or search.
  page.value = "list";
  query.value = "";
  listScrolled.value = false;
  closeMenus();
  previewGeneration++;
  previewClip.value = null;
  editing.value = false;
  previewBusy.value = false;
  previewError.value = "";
  fullText.value = "";
  draftText.value = "";
  groupFilter.value = "";
  moveFilter.value = "";
  notice.value = "";
  window.clearTimeout(noticeTimer);
}

function hideWindow() {
  onHidden();
  void api.hideWindow();
}

function timeAgo(ms: number) {
  const s = (Date.now() - ms) / 1000;
  if (s < 60) return "刚刚";
  if (s < 3600) return `${Math.floor(s / 60)} 分钟前`;
  if (s < 86400) return `${Math.floor(s / 3600)} 小时前`;
  if (s < 86400 * 7) return `${Math.floor(s / 86400)} 天前`;
  const d = new Date(ms);
  const sameYear = d.getFullYear() === new Date().getFullYear();
  return sameYear ? `${d.getMonth() + 1}月${d.getDate()}日` : `${d.getFullYear()}/${d.getMonth() + 1}/${d.getDate()}`;
}

function itemHint(c: Clip, index: number) {
  return [
    `记录于 ${new Date(c.updatedAt).toLocaleString("zh-CN")}`,
    c.kind === "image" ? `${c.width}×${c.height}` : "",
    c.source ? `来自 ${c.source}` : "",
    index < 9 ? `Ctrl+${index + 1} 快速粘贴` : "",
  ].filter(Boolean).join(" · ");
}

function openSettings(about = false) {
  focusAbout.value = about;
  page.value = "settings";
}
function backToList() {
  focusAbout.value = false;
  page.value = "list";
  nextTick(() => searchEl.value?.focus());
}

async function onShown() {
  onHidden();
  windowShown = true;
  // The reload below covers a search cleared on hide whose debounced reload is still pending.
  window.clearTimeout(searchTimer);
  // Restore keyboard control before the database refresh finishes.
  nextTick(() => searchEl.value?.focus());
  const ui = loadUi();
  restoreScroll = ui.scrollTop;
  selected.value = 0;
  // Groups stay current through groups-changed, so the remembered tab is restored right away
  // (falling back to All if that group is gone) and the requests run together.
  if (ui.scope === "group" && groups.value.some((g) => g.id === ui.groupId)) {
    scope.value = "group";
    activeGroup.value = ui.groupId;
  } else {
    scope.value = ui.scope === "fav" ? "fav" : "all";
    activeGroup.value = null;
  }
  const [, , isPaused] = await Promise.all([reloadGroups(), reload(), api.getCapturePaused()]);
  paused.value = isPaused;
}

function preventNativeContextMenu(event: MouseEvent) {
  event.preventDefault();
}

onMounted(async () => {
  window.addEventListener("keydown", onKey);
  window.addEventListener("click", closeMenus);
  if (import.meta.env.PROD) document.addEventListener("contextmenu", preventNativeContextMenu);
  unlisteners.push(
    await listen("clips-changed", () => reload(true)),
    await listen("groups-changed", reloadGroups),
    await listen("window-shown", onShown),
    await listen("window-hidden", onHidden),
    // Arrives after window-shown, whose reset to the list page it then overrides.
    await listen("open-about", () => openSettings(true)),
    await listen("settings-changed", syncSettings),
    await listen<boolean>("capture-paused-changed", (e) => (paused.value = e.payload)),
  );
  // onShown restores the remembered group tab from the already loaded groups.
  await reloadGroups();
  await onShown();
  await syncSettings();
});

async function syncSettings() {
  const { settings } = await api.getSettings();
  pinnedWindow.value = settings.keepOnBlur;
  applyTheme(settings.theme);
}

onUnmounted(() => {
  window.removeEventListener("keydown", onKey);
  window.removeEventListener("click", closeMenus);
  if (import.meta.env.PROD) document.removeEventListener("contextmenu", preventNativeContextMenu);
  window.clearTimeout(noticeTimer);
  window.clearTimeout(searchTimer);
  window.clearTimeout(scrollTimer);
  unlisteners.forEach((u) => u());
});
</script>

<template>
  <div class="shell">
    <SettingsView v-if="page === 'settings'" :focus-about="focusAbout" @back="backToList" />

    <template v-else>
      <header class="topbar" @mousedown.self.left="startWindowDrag">
        <nav class="tabs" aria-label="剪贴板视图">
          <button class="tab primary-tab" :class="{ active: scope === 'all' }" aria-label="剪贴板" title="剪贴板" @click="selectScope('all')"><ClipIcon name="clipboard" /><span>剪贴板</span></button>
          <button class="tab primary-tab" :class="{ active: scope === 'fav' }" aria-label="收藏" title="收藏" @click="selectScope('fav')"><ClipIcon name="star" /><span>收藏</span></button>
          <span class="tab-sep"></span>
          <button class="tab group-trigger" :class="{ active: scope === 'group', open: groupPicker.open }" :title="scope === 'group' ? groupName(activeGroup) ?? '分组' : `查看分组（${groups.length}）`" aria-label="选择分组" :aria-expanded="groupPicker.open" @click="openGroupPicker">
            <ClipIcon name="folder" /><span class="group-trigger-name">{{ scope === 'group' ? groupName(activeGroup) ?? '分组' : '分组' }}</span><ClipIcon name="chevron-down" class="group-trigger-chevron" />
          </button>
        </nav>
        <div class="spacer" @mousedown.left="startWindowDrag"></div>
        <div class="actions">
          <button
            class="icon-btn"
            :class="{ on: pinnedWindow }"
            :title="pinnedWindow ? '已固定（失焦不隐藏）' : '固定窗口'"
            @click="togglePinnedWindow"
          >
            <ClipIcon name="pin" />
          </button>
          <button class="icon-btn settings-btn" :title="newerVersion ? `设置 · 有新版本 v${newerVersion}` : '设置'" @click="openSettings()">
            <ClipIcon name="settings" /><span v-if="newerVersion" class="update-dot" aria-label="有新版本"></span>
          </button>
          <button class="icon-btn" title="隐藏 (Esc)" @click="hideWindow"><ClipIcon name="close" /></button>
        </div>
      </header>

      <div class="search-area" :class="{ scrolled: listScrolled }">
        <div class="search">
          <ClipIcon name="search" />
          <input ref="searchEl" v-model="query" placeholder="搜索文本…" spellcheck="false" />
          <button v-if="query" class="icon-btn clear" title="清除" @click="query = ''"><ClipIcon name="close" /></button>
        </div>

        <button v-if="paused" class="pause-banner" @click="api.toggleCapturePaused()">
          <ClipIcon name="pause" /> 记录已暂停 · 点击继续
        </button>
        <div v-if="notice" class="notice-banner">{{ notice }}</div>
      </div>

      <ul ref="listEl" class="list" @scroll="onScroll">
        <!-- Clips are replaced, never mutated, so a row only re-renders when its inputs change
             instead of all of them on every selection move. -->
        <li
          v-for="(c, i) in visibleClips"
          :key="c.id"
          v-memo="[c, i, i === selected, scope, groupName(c.groupId)]"
          class="item"
          :class="{ selected: i === selected, pinned: c.pinned }"
          :title="itemHint(c, i)"
          @mousemove="onItemHover(i, $event)"
          @click.capture="dismissMenuBeforeItemClick"
          @click="paste(c)"
          @contextmenu="openClipMenu(c, $event)"
        >
          <div class="body" :class="{ 'image-body': c.kind === 'image' && c.thumb }">
            <img v-if="c.kind === 'image' && c.thumb" class="thumb" :src="convertFileSrc(c.thumb)" :alt="c.source ? `${c.source} 的缩略图` : '剪贴板图片缩略图'" width="76" height="76" loading="lazy" decoding="async" draggable="false" />
            <div v-if="c.kind === 'image' && c.thumb" class="image-summary">
              <span class="image-name" :title="c.source || '剪贴板图片'">{{ c.source || '剪贴板图片' }}</span>
              <span v-if="c.width && c.height" class="image-dimensions">{{ c.width }} × {{ c.height }}</span>
            </div>
            <div v-else class="text">{{ c.preview }}<span v-if="c.truncated" class="more">…</span></div>
            <div class="item-footer">
              <div class="meta">
                <span class="meta-time">{{ timeAgo(c.updatedAt) }}</span>
                <span class="meta-dot" aria-hidden="true">·</span>
                <span class="meta-type"><ClipIcon :name="c.kind === 'image' ? 'image' : 'file-text'" class="tiny" />{{ c.kind === 'image' ? '图片' : '文本' }}</span>
                <span v-if="c.pinned" class="meta-flag pin-flag"><ClipIcon name="pin" class="tiny" />置顶</span>
                <span v-if="scope !== 'group' && groupName(c.groupId)" class="meta-flag group-flag" :title="groupName(c.groupId) ?? ''"><ClipIcon name="folder" class="tiny" /><span class="group-label">{{ groupName(c.groupId) }}</span></span>
              </div>
              <div class="ops">
                <button v-if="c.kind === 'text'" class="icon-btn sm" title="查看全文 / 编辑" @click.stop="openPreview(c)"><ClipIcon name="eye" /></button>
                <button
                  class="icon-btn sm star-op"
                  :class="{ active: c.favorite }"
                  :title="c.favorite ? '取消收藏 (Ctrl+S)' : '收藏 (Ctrl+S)'"
                  :aria-label="c.favorite ? '取消收藏' : '收藏'"
                  :aria-pressed="c.favorite"
                  @click.stop="toggleFav(c)"
                >
                  <ClipIcon :name="c.favorite ? 'star-fill' : 'star'" />
                </button>
                <button class="icon-btn sm more-op" title="更多操作 · 也可右键条目" aria-label="更多操作" @click.stop="openClipMenu(c, $event)"><ClipIcon name="more" /></button>
              </div>
            </div>
          </div>
        </li>
        <li v-if="!clips.length" class="empty">
          <ClipIcon :name="scope === 'fav' ? 'star' : scope === 'group' ? 'folder' : 'clipboard'" class="empty-icon" />
          <p>
            {{
              query
                ? "没有匹配的内容"
                : scope === "fav"
                  ? "还没有收藏，划到条目上点星标"
                  : scope === "group"
                    ? "这个分组是空的，右键条目可移入"
                    : "复制点什么，它就会出现在这里"
            }}
          </p>
        </li>
      </ul>
      <button
        v-if="listScrolled && clips.length"
        class="back-to-top"
        type="button"
        title="回到顶部"
        aria-label="回到列表顶部"
        @click="backToTop"
      ><ClipIcon name="back-to-top" /></button>
    </template>

    <div v-if="groupPicker.open" class="menu menu-surface group-picker" :style="{ left: groupPicker.x + 'px', top: groupPicker.y + 'px' }" @click.stop>
      <div class="menu-label group-picker-head">分组 <span>{{ groups.length }}</span></div>
      <div v-if="groups.length >= 7" class="menu-search"><ClipIcon name="search" /><input ref="groupPickerSearchEl" v-model="groupFilter" aria-label="搜索分组" placeholder="查找分组…" spellcheck="false" /></div>
      <div class="group-picker-scroll" @dragover.prevent>
        <div v-for="{ group: g, index: i } in visibleGroups" :key="g.id" class="group-picker-row" :class="{ active: scope === 'group' && activeGroup === g.id }" :draggable="!groupFilter" :title="groupFilter ? g.name : `${g.name} · 拖动排序`" @dragstart="onDragStart(i, $event)" @dragend="dragIndex = -1" @drop.prevent="onDrop(i)" @contextmenu="openGroupMenu(g, $event)">
          <button class="group-picker-name" :aria-label="`打开分组 ${g.name}`" :title="g.name" @click.stop="chooseGroup(g.id)"><ClipIcon name="folder" /><span>{{ g.name }}</span></button>
          <button class="group-picker-more" :aria-label="`管理分组 ${g.name}`" :title="`管理 ${g.name}`" @click.stop="openGroupMenu(g, $event)"><ClipIcon name="more" /></button>
        </div>
        <p v-if="!groups.length" class="group-picker-empty">还没有分组</p>
        <p v-else-if="!visibleGroups.length" class="group-picker-empty">没有匹配的分组</p>
      </div>
      <div class="menu-divider"></div>
      <button class="menu-row group-picker-add" @click.stop="openNewGroup($event)"><ClipIcon name="folder-plus" /><span class="menu-text">新建分组</span></button>
    </div>

    <div v-if="clipMenu.open && menuClip" class="menu menu-surface clip-menu" :style="{ left: clipMenu.x + 'px', top: clipMenu.y + 'px' }" @click.stop @mouseover="onClipMenuHover">
      <button class="menu-row" @click="menuAction('copy')"><ClipIcon name="copy" /><span class="menu-text">仅复制</span><span class="menu-shortcut">Shift+Enter</span></button>
      <button v-if="menuClip.kind === 'text'" class="menu-row" @click="menuAction('preview')"><ClipIcon name="eye" /><span class="menu-text">查看 / 编辑</span></button>
      <div class="menu-divider"></div>
      <button v-if="menuClip.kind === 'text'" class="menu-row" :class="{ open: openSubmenu === 'send' }" data-submenu="send" aria-haspopup="menu" :aria-expanded="openSubmenu === 'send'" @click="showSubmenu('send', $event.currentTarget as HTMLElement, true)"><ClipIcon name="send" /><span class="menu-text">发送到设备</span><ClipIcon name="chevron" class="menu-chevron" /></button>
      <button class="menu-row" :class="{ open: openSubmenu === 'move' }" data-submenu="move" aria-haspopup="menu" :aria-expanded="openSubmenu === 'move'" @click="showSubmenu('move', $event.currentTarget as HTMLElement, true)"><ClipIcon name="folder" /><span class="menu-text">移动到分组</span><ClipIcon name="chevron" class="menu-chevron" /></button>
      <div class="menu-divider"></div>
      <button class="menu-row" @click="menuAction('favorite')"><ClipIcon :name="menuClip.favorite ? 'star-fill' : 'star'" /><span class="menu-text">{{ menuClip.favorite ? '取消收藏' : '收藏' }}</span></button>
      <button class="menu-row" @click="menuAction('pin')"><ClipIcon name="pin" /><span class="menu-text">{{ menuClip.pinned ? '取消置顶' : '置顶' }}</span></button>
      <div class="menu-divider"></div>
      <button class="menu-row danger" @click="menuAction('delete')"><ClipIcon name="trash" /><span class="menu-text">删除条目</span></button>
    </div>
    <div v-if="moveMenu.open" class="menu menu-surface move-menu" :style="{ left: moveMenu.x + 'px', top: moveMenu.y + 'px' }" @click.stop @mouseenter="keepSubmenu">
      <div class="menu-label">移动到</div>
      <div v-if="groups.length >= 7" class="menu-search"><ClipIcon name="search" /><input v-model="moveFilter" aria-label="搜索目标分组" placeholder="查找分组…" spellcheck="false" /></div>
      <div class="move-menu-scroll">
        <button v-for="g in visibleMoveGroups" :key="g.id" class="menu-row" :title="g.name" @click="moveTo(g.id)"><ClipIcon name="folder" /><span class="menu-text">{{ g.name }}</span><ClipIcon v-if="moveClip?.groupId === g.id" name="check" class="menu-chevron" /></button>
        <div v-if="!groups.length" class="menu-label">先在分组菜单中新建分组</div>
        <div v-else-if="!visibleMoveGroups.length" class="menu-label">没有匹配的分组</div>
      </div>
      <template v-if="moveClip?.groupId != null">
        <div class="menu-divider"></div>
        <button class="menu-row" @click="moveTo(null)"><ClipIcon name="close" /><span class="menu-text">移出分组</span></button>
      </template>
    </div>
    <div v-if="sendMenu.open" class="menu menu-surface send-menu" :style="{ left: sendMenu.x + 'px', top: sendMenu.y + 'px' }" role="group" aria-label="发送到设备" tabindex="-1" @click.stop @mouseenter="keepSubmenu">
      <div class="menu-label">发送文本到</div>
      <div v-if="sendLoading" class="send-menu-message" role="status">正在读取设备…</div>
      <template v-else>
        <button v-for="peer in lanPeers" :key="peer.address" class="menu-row" :disabled="sendBusy" :title="`${peer.name} · ${peer.address}`" :aria-label="`发送到 ${peer.name}（${peer.address}）`" @click="sendTo(peer)"><ClipIcon name="send" /><span class="menu-text">{{ peer.name }}</span></button>
        <div v-if="!lanPeers.length && !sendLoadError" class="send-menu-message">还没有接收设备</div>
        <div v-if="sendBusy" class="send-menu-message" role="status">正在发送到 {{ sendTarget }}…</div>
        <div v-if="sendError" class="send-menu-message send-menu-error" role="alert">{{ sendError }}</div>
        <template v-if="!lanPeers.length">
          <div class="menu-divider"></div>
          <button v-if="sendLoadError" class="menu-row" @click="loadSendPeers(menuGeneration, true)"><ClipIcon name="refresh" /><span class="menu-text">重试读取</span></button>
          <button v-else class="menu-row" @click="openLanSettings"><ClipIcon name="plus" /><span class="menu-text">添加设备</span></button>
        </template>
      </template>
    </div>
    <div v-if="groupMenu.open && menuGroup" class="menu menu-surface group-menu" :style="{ left: groupMenu.x + 'px', top: groupMenu.y + 'px' }" @click.stop>
      <div class="menu-label group-menu-title" :title="menuGroup.name">{{ menuGroup.name }}</div>
      <template v-if="!groupMenu.confirming">
        <button class="menu-row" @click="renameFromGroupMenu"><ClipIcon name="edit" /><span class="menu-text">重命名</span></button>
        <div class="menu-divider"></div>
        <button class="menu-row danger" @click="requestDeleteGroup"><ClipIcon name="trash" /><span class="menu-text">删除分组</span></button>
      </template>
      <div v-else class="group-confirm">
        <p>删除分组后，其中的条目会保留在剪贴板。</p>
        <div class="group-confirm-actions">
          <button class="group-cancel" @click="groupMenu.confirming = false">取消</button>
          <button class="group-delete-confirm" @click="deleteGroup">确认删除</button>
        </div>
      </div>
    </div>
    <form v-if="groupEditor.open" class="menu menu-surface group-editor" :style="{ left: groupEditor.x + 'px', top: groupEditor.y + 'px' }" @click.stop @submit.prevent="saveGroup">
      <label class="group-editor-label" for="group-name-input">{{ groupEditor.mode === 'create' ? '新建分组' : '重命名分组' }}</label>
      <input id="group-name-input" ref="groupInput" v-model="groupEditor.name" class="group-editor-input" placeholder="输入分组名称" maxlength="60" autocomplete="off" spellcheck="false" :aria-invalid="Boolean(groupEditor.error)" :aria-describedby="groupEditor.error ? 'group-editor-error' : undefined" @input="groupEditor.error = ''" />
      <p v-if="groupEditor.error" id="group-editor-error" class="group-editor-error" role="alert">{{ groupEditor.error }}</p>
      <div class="group-editor-actions">
        <button type="button" class="group-cancel" :disabled="groupEditor.busy" @click="cancelGroupEditor">取消</button>
        <button type="submit" class="group-submit" :disabled="groupEditor.busy">{{ groupEditor.mode === 'create' ? '创建' : '保存' }}</button>
      </div>
    </form>

    <section v-if="previewClip" class="preview-panel" aria-label="文本全文预览">
      <header class="preview-head">
        <button class="icon-btn" title="返回 (Esc)" @click="closePreview"><ClipIcon name="back" /></button>
        <div class="preview-title"><ClipIcon :name="editing ? 'edit' : 'file-text'" />{{ editing ? '编辑文本' : '文本全文' }}</div>
        <span class="preview-size">{{ (editing ? draftText.length : fullText.length).toLocaleString() }} 字符</span>
        <button class="icon-btn" :title="editing ? '取消编辑' : '关闭'" @click="closePreview"><ClipIcon name="close" /></button>
      </header>
      <div v-if="previewError" class="preview-error">{{ previewError }}</div>
      <div v-if="previewBusy && !fullText" class="preview-loading">正在读取全文…</div>
      <textarea v-else-if="editing" ref="editorEl" v-model="draftText" class="preview-editor" spellcheck="false"></textarea>
      <pre v-else class="preview-content">{{ fullText }}</pre>
      <footer class="preview-foot">
        <span>{{ editing ? 'Ctrl+Enter 保存 · Esc 取消' : '可选择并复制任意段落' }}</span>
        <button v-if="editing" class="preview-action" :disabled="previewBusy" @click="savePreview">保存</button>
        <button v-else class="preview-action" :disabled="previewBusy || !!previewError" @click="startEditing">编辑</button>
      </footer>
    </section>
  </div>
</template>

<style scoped>
.topbar {
  display: flex;
  align-items: center;
  gap: 5px;
  padding: 9px 8px 7px;
  border-bottom: 1px solid var(--divider);
  background: var(--surface-2);
}
.spacer {
  flex: 1;
  align-self: stretch;
  min-width: 4px;
  cursor: grab;
}
.tabs {
  display: flex;
  align-items: center;
  flex: 0 1 auto;
  min-width: 0;
  gap: 2px;
}
.tab {
  flex-shrink: 0;
  white-space: nowrap;
  padding: 5px 6px;
  border-radius: var(--r-sm);
  color: var(--text-2);
  font-weight: 500;
  transition: background 0.14s var(--ease), color 0.14s var(--ease);
}
.tab:hover {
  background: var(--hover);
  color: var(--text);
}
.tab.active {
  background: var(--accent-soft);
  color: var(--accent);
  font-weight: 600;
}
.primary-tab,
.group-trigger {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 4px;
}
.primary-tab svg,
.group-trigger svg { width: 14px; height: 14px; }
.primary-tab.active svg { stroke-width: 2.1; }
.tab-sep {
  width: 1px;
  height: 16px;
  margin: 0 3px;
  background: var(--border);
  flex-shrink: 0;
}
.group-trigger { width: auto; max-width: 120px; min-width: 0; }
.group-trigger-name { min-width: 0; flex: 0 1 auto; overflow: hidden; text-overflow: ellipsis; text-align: left; }
.group-trigger .group-trigger-chevron { width: 12px; height: 12px; }
.group-trigger.open { background: var(--hover); }
.actions {
  display: flex;
  gap: 1px;
}
.settings-btn { position: relative; }
/* Same red as the dot painted on the tray icon. */
.settings-btn .update-dot {
  position: absolute;
  top: 5px;
  right: 5px;
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: #e5484d;
  box-shadow: 0 0 0 1.5px var(--surface-2);
}
@media (max-width: 370px) {
  .primary-tab { width: 30px; padding: 5px 7px; }
  .primary-tab span { display: none; }
  .group-trigger { max-width: 96px; }
}

/* Rows slide under this header; once the list is scrolled it gets an edge so they don't
   look cut off mid-air. */
.search-area {
  position: relative;
  z-index: 2;
  padding: 10px 10px 6px;
  background: var(--surface);
  transition: box-shadow 0.14s var(--ease);
}
.search-area.scrolled {
  box-shadow: 0 1px 0 var(--border), 0 6px 10px -8px rgba(0, 0, 0, 0.2);
}
.search {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 0 6px 0 10px;
  height: 36px;
  background: var(--surface-2);
  border: 1px solid var(--border-strong);
  border-radius: var(--r-md);
  color: var(--muted);
  transition: border-color 0.14s var(--ease), background 0.14s var(--ease);
}
.search:focus-within {
  border-color: var(--accent);
  background: var(--surface);
}
.search input {
  flex: 1;
  min-width: 0;
  border: none;
  outline: none;
  background: none;
  color: var(--text);
}
.search .clear {
  width: 24px;
  height: 24px;
}
.pause-banner,
.notice-banner {
  display: flex;
  align-items: center;
  gap: 6px;
  width: 100%;
  margin-top: 8px;
  padding: 6px 10px;
  border-radius: var(--r-sm);
  font-size: var(--fs-sm);
  text-align: left;
}
.pause-banner {
  background: var(--accent-soft);
  color: var(--accent);
}
.pause-banner svg { width: 12px; height: 12px; }
.notice-banner { background: var(--surface-2); color: var(--text-2); }

.list {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  list-style: none;
  margin: 0;
  padding: 6px 9px 56px;
  scroll-padding-bottom: 56px;
}
.back-to-top {
  position: absolute;
  right: 18px;
  bottom: 16px;
  z-index: 4;
  display: grid;
  place-items: center;
  width: 36px;
  height: 36px;
  border: 1px solid var(--border-strong);
  border-radius: var(--r-md);
  background: var(--elevated);
  box-shadow: var(--shadow-sm), 0 4px 12px rgba(0, 0, 0, 0.12);
  color: var(--text-2);
}
.back-to-top:hover {
  background: var(--accent-soft);
  color: var(--accent);
}
.back-to-top svg {
  width: 17px;
  height: 17px;
}
.item {
  position: relative;
  display: flex;
  align-items: flex-start;
  gap: 8px;
  margin-bottom: 6px;
  padding: 9px 10px 9px 12px;
  border: 1px solid var(--border);
  border-radius: var(--r-md);
  background: var(--surface-2);
  box-shadow: 0 1px 2px rgba(0, 0, 0, .025);
  cursor: default;
  /* Up to 1000 rows: skip rendering the off-screen ones. 82px is the measured average row
     height; `auto` keeps a row's real height once it has been shown. */
  content-visibility: auto;
  contain-intrinsic-size: auto 82px;
}
.item.selected {
  background: var(--selected);
  border-color: var(--accent);
  box-shadow: 0 0 0 1px var(--accent-soft);
}
.item::before {
  content: "";
  position: absolute;
  left: -1px;
  top: 10px;
  bottom: 10px;
  width: 3px;
  border-radius: var(--r-pill);
  background: var(--accent);
  opacity: 0;
  transition: opacity 0.14s var(--ease);
}
.item.selected::before {
  opacity: 1;
}
.item.pinned::before {
  opacity: 1;
  background: var(--pin);
}
.body {
  flex: 1;
  min-width: 0;
}
.text {
  white-space: pre-wrap;
  word-break: break-word;
  line-height: 1.5;
  color: var(--text);
  display: -webkit-box;
  -webkit-line-clamp: 3;
  line-clamp: 3;
  -webkit-box-orient: vertical;
  overflow: hidden;
}
.more {
  color: var(--muted);
}
.thumb {
  display: block;
  width: 76px;
  height: 76px;
  object-fit: contain;
  border-radius: var(--r-sm);
  border: 1px solid var(--border);
  background: repeating-conic-gradient(var(--hover) 0% 25%, transparent 0% 50%) 0 0 / 14px 14px;
}
.image-body {
  display: grid;
  grid-template-columns: 76px minmax(0, 1fr);
  grid-template-rows: minmax(0, 1fr) auto;
  column-gap: 10px;
  min-height: 76px;
}
.image-body .thumb { grid-row: 1 / 3; }
.image-summary {
  display: flex;
  flex-direction: column;
  justify-content: center;
  gap: 4px;
  grid-column: 2;
  min-width: 0;
}
.image-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text);
  font-weight: 500;
}
.image-dimensions { color: var(--muted); font-size: var(--fs-xs); }
.image-body .item-footer { grid-column: 2; margin-top: 0; }
.item-footer {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 4px;
  min-height: 24px;
  margin-top: 5px;
}
.meta {
  display: flex;
  align-items: center;
  gap: 5px;
  min-width: 0;
  flex: 1;
  overflow: hidden;
  color: var(--text-2);
  font-size: var(--fs-sm);
  white-space: nowrap;
}
.meta-time,
.meta-dot,
.meta-type,
.pin-flag { flex: none; }
.meta-dot { color: var(--muted); }
.meta-type,
.meta-flag {
  display: inline-flex;
  align-items: center;
  gap: 3px;
}
.pin-flag { margin-left: 5px; color: var(--pin); }
.group-flag { min-width: 0; margin-left: 5px; color: var(--muted); }
.group-label { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.tiny { width: 12px; height: 12px; flex: none; }

.ops {
  display: flex;
  gap: 2px;
  flex-shrink: 0;
}
.ops .icon-btn { opacity: 0; pointer-events: none; }
.item:hover .ops .icon-btn,
.item:focus-within .ops .icon-btn {
  opacity: 1;
  pointer-events: auto;
}
.icon-btn.sm {
  width: 24px;
  height: 24px;
}
.icon-btn.sm svg { width: 14px; height: 14px; }
.star-op.active {
  color: var(--star);
}
.more-op { color: var(--text-2); }

.empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 12px;
  padding: 56px 24px;
  text-align: center;
  color: var(--muted);
}
.empty-icon {
  width: 34px;
  height: 34px;
  opacity: 0.4;
}
.empty p {
  margin: 0;
  line-height: 1.6;
}

.menu {
  position: fixed;
  z-index: 20;
  width: 188px;
  max-height: calc(100vh - 16px);
  overflow-y: auto;
}
.clip-menu .menu-row { min-height: 26px; padding: 3px 7px; }
/* The row whose submenu is showing stays highlighted, as in native menus. */
.clip-menu .menu-row.open { background: var(--accent-soft); color: var(--accent); }
.clip-menu .menu-row.open > svg { color: var(--accent); }
.clip-menu .menu-divider { margin: 2px 6px; }
.move-menu { width: 196px; } /* SUBMENU_WIDTH */
.send-menu { width: 184px; }
.send-menu-message { padding: 7px 9px; color: var(--muted); font-size: var(--fs-xs); line-height: 1.45; overflow-wrap: anywhere; }
.send-menu-error { color: var(--danger); }
.group-menu { width: 184px; }
.group-picker { width: 248px; overflow: hidden; }
.group-picker-head { justify-content: space-between; }
.group-picker-head span { font-variant-numeric: tabular-nums; font-weight: 500; }
.group-picker-scroll,
.move-menu-scroll { max-height: min(240px, calc(100vh - 210px)); overflow-y: auto; }
.menu-search { display: flex; align-items: center; gap: 6px; height: 30px; margin: 3px 4px 6px; padding: 0 7px; border: 1px solid var(--border-strong); border-radius: var(--r-sm); background: var(--surface); color: var(--muted); }
.menu-search:focus-within { border-color: var(--accent); }
.menu-search svg { width: 13px; height: 13px; }
.menu-search input { min-width: 0; width: 100%; border: 0; outline: 0; background: transparent; color: var(--text); font-size: var(--fs-sm); }
.group-picker-row { display: flex; align-items: center; min-width: 0; gap: 2px; border-radius: var(--r-sm); }
.group-picker-row:hover,
.group-picker-row.active { background: var(--accent-soft); }
.group-picker-name { display: flex; align-items: center; min-width: 0; flex: 1; gap: 8px; padding: 6px 8px; color: var(--text); text-align: left; }
.group-picker-row.active .group-picker-name { color: var(--accent); font-weight: 600; }
.group-picker-name svg { width: 14px; height: 14px; flex: none; color: var(--text-2); }
.group-picker-row.active .group-picker-name svg { color: var(--accent); }
.group-picker-name span { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.group-picker-more { display: grid; place-items: center; width: 26px; height: 26px; margin-right: 3px; border-radius: var(--r-sm); color: var(--muted); flex: none; }
.group-picker-more svg { width: 14px; height: 14px; }
.group-picker-more:hover { color: var(--accent); background: var(--surface); }
.group-picker-empty { margin: 0; padding: 13px 8px; color: var(--muted); font-size: var(--fs-sm); }
.group-picker-add { color: var(--accent); }
.group-picker-add > svg { color: var(--accent); }
.group-menu-title { display: block; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.group-confirm { padding: 3px 7px 7px; }
.group-confirm p { margin: 2px 0 10px; color: var(--text-2); font-size: var(--fs-xs); line-height: 1.5; }
.group-confirm-actions, .group-editor-actions { display: flex; justify-content: flex-end; gap: 6px; }
.group-cancel, .group-submit, .group-delete-confirm {
  min-height: 28px;
  padding: 5px 10px;
  border: 1px solid var(--border-strong);
  border-radius: var(--r-sm);
  background: var(--surface-2);
  color: var(--text-2);
  font-size: var(--fs-sm);
  font-weight: 600;
}
.group-cancel:hover { background: var(--hover); color: var(--text); }
.group-submit { border-color: var(--accent); background: var(--accent-soft); color: var(--accent); }
.group-submit:hover { background: var(--selected); }
.group-delete-confirm { border-color: var(--danger); background: var(--danger-soft); color: var(--danger); }
.group-delete-confirm:hover { filter: brightness(.95); }
.group-editor { width: 228px; padding: 11px; }
.group-editor-label { display: block; margin-bottom: 8px; color: var(--text); font-size: var(--fs-sm); font-weight: 600; }
.group-editor-input {
  display: block;
  width: 100%;
  height: 32px;
  padding: 0 9px;
  border: 1px solid var(--border-strong);
  border-radius: var(--r-sm);
  outline: none;
  background: var(--surface);
  color: var(--text);
}
.group-editor-input:focus { border-color: var(--accent); box-shadow: 0 0 0 2px var(--accent-soft); }
.group-editor-error { margin: 6px 0 0; color: var(--danger); font-size: var(--fs-xs); }
.group-editor-actions { margin-top: 10px; }
.preview-panel {
  position: absolute;
  inset: 0;
  z-index: 40;
  display: flex;
  flex-direction: column;
  background: var(--surface);
}
.preview-head {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 9px 10px;
  border-bottom: 1px solid var(--border);
}
.preview-title { display: flex; align-items: center; gap: 7px; flex: 1; font-weight: 600; font-size: var(--fs-lg); }
.preview-title svg { width: 16px; height: 16px; color: var(--accent); }
.preview-size { color: var(--muted); font-size: var(--fs-xs); white-space: nowrap; }
.preview-content,
.preview-editor {
  flex: 1;
  min-height: 0;
  margin: 10px;
  padding: 13px;
  overflow: auto;
  border: 1px solid var(--border);
  border-radius: var(--r-md);
  background: var(--surface-2);
  color: var(--text);
  font: inherit;
  line-height: 1.65;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  user-select: text;
}
.preview-editor { resize: none; outline: none; }
.preview-editor:focus { border-color: var(--accent); }
.preview-foot {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 0 10px 10px;
  color: var(--muted);
  font-size: var(--fs-xs);
}
.preview-foot span { flex: 1; }
.preview-action {
  padding: 7px 15px;
  border-radius: var(--r-sm);
  background: var(--accent-soft);
  color: var(--accent);
  font-weight: 600;
}
.preview-action:disabled { opacity: .5; }
.preview-error,
.preview-loading {
  margin: 10px 10px 0;
  color: var(--danger);
  font-size: var(--fs-sm);
}
.preview-loading { color: var(--muted); }
</style>
