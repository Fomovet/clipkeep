<script setup lang="ts">
import {
  ArrowLeft,
  ArrowUpToLine,
  Check,
  ChevronDown,
  ChevronRight,
  Clipboard,
  Copy,
  Download,
  Database,
  Ellipsis,
  Eye,
  FileText,
  FolderClosed,
  FolderPlus,
  HardDrive,
  Image as ImageIcon,
  Info,
  Keyboard,
  Network,
  Pause,
  Palette,
  Pencil,
  Pin,
  Play,
  Plus,
  Power,
  RefreshCw,
  Search,
  Send,
  Settings2,
  SlidersHorizontal,
  Star,
  Trash2,
  Upload,
  X,
} from "@lucide/vue";

const icons = {
  plus: Plus,
  "folder-plus": FolderPlus,
  pin: Pin,
  settings: Settings2,
  close: X,
  back: ArrowLeft,
  "back-to-top": ArrowUpToLine,
  search: Search,
  folder: FolderClosed,
  star: Star,
  "star-fill": Star,
  trash: Trash2,
  clipboard: Clipboard,
  "file-text": FileText,
  image: ImageIcon,
  palette: Palette,
  eye: Eye,
  edit: Pencil,
  send: Send,
  pause: Pause,
  play: Play,
  check: Check,
  power: Power,
  refresh: RefreshCw,
  more: Ellipsis,
  copy: Copy,
  chevron: ChevronRight,
  "chevron-down": ChevronDown,
  general: SlidersHorizontal,
  storage: HardDrive,
  network: Network,
  keyboard: Keyboard,
  database: Database,
  download: Download,
  upload: Upload,
  info: Info,
} as const;

defineProps<{ name: keyof typeof icons }>();
</script>

<template>
  <component :is="icons[name]" :stroke-width="1.8" :class="{ 'favorite-icon': name === 'star-fill' }" aria-hidden="true" />
</template>

<style scoped>
.favorite-icon { fill: currentColor; }
</style>
