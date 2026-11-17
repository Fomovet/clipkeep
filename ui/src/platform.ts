/** True inside the macOS app (WKWebView). Used only for copy and shortcut labels. */
export const isMac =
  /Mac|iPhone|iPad/.test(navigator.platform) || /Mac|iPhone|iPad/.test(navigator.userAgent);

/** In-panel shortcut label. The key handler still accepts both Ctrl and ⌘. */
export function modShortcut(key: string): string {
  return `${isMac ? "⌘" : "Ctrl+"}${key}`;
}
