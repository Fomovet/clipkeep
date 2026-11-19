import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import ts from 'typescript';

const app = readFileSync(process.env.CLIPKEEP_APP_SOURCE || new URL('../src/App.vue', import.meta.url), 'utf8');
const selection = readFileSync(new URL('../src/selection.ts', import.meta.url), 'utf8');
const compile = (source) => ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS },
}).outputText;

// Execute the actual App event handlers with lightweight refs, not a duplicate model.
function panel(mac = false) {
  const exports = {};
  vm.runInNewContext(compile(selection), { exports });
  const script = app.match(/<script setup lang="ts">([\s\S]*?)<\/script>/)[1];
  const ast = ts.createSourceFile('App.ts', script, ts.ScriptTarget.Latest, true);
  const names = new Set(['clearMultiSelect', 'toggleMulti', 'rangeMultiSelect', 'onItemClick', 'onItemHover', 'move']);
  const handlers = ast.statements.filter((n) => ts.isFunctionDeclaration(n) && names.has(n.name?.text))
    .map((n) => n.getText(ast)).join('\n');
  assert.equal(handlers.match(/function /g).length, names.size);
  const rows = [10, 20, 30, 40, 50].map((id) => ({ id }));
  const state = { ids: [], anchor: null };
  const pasted = [];
  const context = vm.createContext({
    ...exports, multiSelect: state, selected: { value: 0 }, clips: { value: rows },
    lastPointer: { x: -1, y: -1 }, nextTick() {}, paste: (row) => pasted.push(row.id),
    hasModKey: (e) => mac ? e.metaKey : e.ctrlKey,
  });
  vm.runInContext(compile(handlers), context);
  const click = (i, modifiers = {}) => context.onItemClick(rows[i], i, modifiers);
  const hover = (i) => context.onItemHover(i, { screenX: i + 100, screenY: 10 });
  return { ...context, state, rows, pasted, click, hover };
}
const ids = (p) => Array.from(p.state.ids).sort((a, b) => a - b);

test('keyboard focus -> pointer movement -> first Shift-click selects the range', () => {
  const p = panel();
  p.move(1);
  p.hover(2);
  p.hover(4);
  p.click(4, { shiftKey: true });
  assert.deepEqual(ids(p), [20, 30, 40, 50]);
  assert.equal(p.state.anchor, 20);
  assert.deepEqual(p.pasted, []);
});

test('first hover preserves initial focus without a prior keyboard event', () => {
  const p = panel();
  p.hover(3);
  p.click(3, { shiftKey: true });
  assert.deepEqual(ids(p), [10, 20, 30, 40]);
});

for (const mac of [false, true]) {
  test(`${mac ? 'Cmd' : 'Ctrl'} toggles; Shift extends; plain click still pastes`, () => {
    const p = panel(mac);
    const mod = mac ? { metaKey: true } : { ctrlKey: true };
    p.click(1, mod);
    p.hover(3);
    p.click(3, { shiftKey: true });
    assert.deepEqual(ids(p), [20, 30, 40]);
    p.click(2, mod);
    assert.deepEqual(ids(p), [20, 40]);
    p.click(4);
    assert.deepEqual(ids(p), []);
    assert.deepEqual(p.pasted, [50]);
  });
}

test('cancel and subsequent keyboard navigation establish the next anchor', () => {
  const p = panel();
  p.click(3, { ctrlKey: true });
  p.clearMultiSelect();
  p.move(-2);
  p.hover(4);
  p.click(4, { shiftKey: true });
  assert.deepEqual(ids(p), [20, 30, 40, 50]);
});

test('anchor survives reorder by identity and is replaced when removed', () => {
  const p = panel();
  p.click(1, { ctrlKey: true });
  const reordered = [p.rows[4], p.rows[0], p.rows[2], p.rows[1], p.rows[3]];
  p.reconcileSelection(p.state, reordered, reordered[0]);
  p.rangeSelection(p.state, reordered, 4);
  assert.deepEqual(ids(p), [20, 40]);
  const remaining = reordered.filter((row) => row.id !== 20);
  p.reconcileSelection(p.state, remaining, remaining[0]);
  assert.equal(p.state.anchor, 50);
  assert.deepEqual(ids(p), [40]);
});

test('UI wires events, cancel, reconciliation and query reset', () => {
  assert.match(app, /@mousemove="onItemHover\(row.index, \$event\)"/);
  assert.match(app, /@click="onItemClick\(row.clip, row.index, \$event\)"/);
  assert.match(app, /if \(inMultiSelect.value\) clearMultiSelect\(\)/);
  assert.match(app, /reconcileSelection\(multiSelect, rows, rows\[selected.value\]\)/);
  assert.match(app, /watch\(\[scope, activeGroup, kindFilter\], \(\) => \{\s*clearSelection\(multiSelect\)/);
  assert.match(app, /watch\(query, \(\) => \{\s*clearSelection\(multiSelect\)/);
});
