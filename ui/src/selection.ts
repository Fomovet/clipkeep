/** Selection identity survives hover and list reordering; focus is deliberately separate. */
export interface SelectionState {
  ids: number[];
  anchor: number | null;
}

type Row = { id: number };

export function clearSelection(state: SelectionState, current?: Row) {
  state.ids = [];
  state.anchor = current?.id ?? null;
}

export function reconcileSelection(state: SelectionState, rows: Row[], current?: Row) {
  const live = new Set(rows.map((row) => row.id));
  state.ids = state.ids.filter((id) => live.has(id));
  if (state.anchor == null || !live.has(state.anchor)) state.anchor = current?.id ?? null;
}

/** Keyboard navigation establishes a new anchor outside an active multi-selection. */
export function keyboardSelectionFocus(state: SelectionState, current?: Row) {
  if (!state.ids.length) state.anchor = current?.id ?? null;
}

/** Called BEFORE hover changes current focus. Never move an established anchor on hover. */
export function hoverSelectionFocus(state: SelectionState, previous?: Row) {
  if (state.anchor == null) state.anchor = previous?.id ?? null;
}

export function toggleSelection(state: SelectionState, row: Row) {
  const pos = state.ids.indexOf(row.id);
  if (pos >= 0) state.ids.splice(pos, 1);
  else state.ids.push(row.id);
  state.anchor = row.id;
}

export function rangeSelection(state: SelectionState, rows: Row[], index: number) {
  const target = rows[index];
  if (!target) return;
  const found = rows.findIndex((row) => row.id === state.anchor);
  const anchor = found < 0 ? index : found;
  const ids = new Set(state.ids);
  for (let i = Math.min(anchor, index); i <= Math.max(anchor, index); i++) ids.add(rows[i].id);
  state.ids = [...ids];
  state.anchor = rows[anchor].id;
}
