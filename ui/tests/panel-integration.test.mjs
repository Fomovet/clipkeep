import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import test from "node:test";
import ts from "typescript";

const app = readFileSync(new URL("../src/App.vue", import.meta.url), "utf8");
const script = app.match(/<script setup lang="ts">([\s\S]*?)<\/script>/)[1];
const ast = ts.createSourceFile("App.ts", script, ts.ScriptTarget.Latest, true);

function panel() {
  const names = new Set(["openPreview", "saveClipAsFile", "onKey"]);
  const source = ast.statements.filter((n) => ts.isFunctionDeclaration(n) && names.has(n.name?.text))
    .map((n) => n.getText(ast)).join("\n");
  const ref = (value) => ({ value });
  const calls = [];
  const context = vm.createContext({
    previewGeneration: 0, previewClip: ref(null), fullText: ref(""), draftText: ref(""),
    previewError: ref(""), imageSaveError: ref(""), editing: ref(false), previewBusy: ref(false), accessibilityGuide: ref(false),
    api: { async getClipText() { calls.push("text"); return "full text"; } },
    closePreview() { calls.push("close"); },
  });
  vm.runInContext(ts.transpileModule(source, {
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS },
  }).outputText, context);
  return { context, calls };
}

test("image preview remains available after integrating the permission guide", async () => {
  const { context: p, calls } = panel();
  const image = { id: 4, kind: "image", image: "/app/images/example.png" };
  await p.openPreview(image);
  assert.equal(p.previewClip.value, image);
  assert.equal(p.previewError.value, "");
  assert.equal(p.previewBusy.value, false);
  assert.deepEqual(calls, []);
});

test("a failed image export keeps the preview visible and allows a retry", async () => {
  const { context: p } = panel();
  const image = { id: 4, kind: "image", image: "/app/images/example.png" };
  p.api.saveClipImage = async () => { throw new Error("write failed"); };
  const notices = [];
  p.flash = (message) => notices.push(message);

  await p.openPreview(image);
  await p.saveClipAsFile(image);
  assert.match(p.imageSaveError.value, /write failed/);
  assert.equal(p.previewError.value, "");
  assert.equal(p.previewBusy.value, false);
  assert.equal(p.previewClip.value, image);

  p.api.saveClipImage = async () => "/exports/example.png";
  await p.saveClipAsFile(image);
  assert.equal(p.imageSaveError.value, "");
  assert.deepEqual(notices, ["已保存到 /exports/example.png"]);
});

test("text preview still loads full editable text", async () => {
  const { context: p, calls } = panel();
  await p.openPreview({ id: 5, kind: "text" });
  assert.equal(p.fullText.value, "full text");
  assert.equal(p.draftText.value, "full text");
  assert.deepEqual(calls, ["text"]);
});

test("permission guide blocks underlying preview and paste shortcuts", () => {
  const { context: p, calls } = panel();
  p.accessibilityGuide.value = true;
  p.previewClip.value = { kind: "image" };
  let prevented = false;
  p.onKey({ key: "Escape", preventDefault() { prevented = true; } });
  p.onKey({ key: "Enter", preventDefault() {} });
  assert.equal(prevented, true);
  assert.deepEqual(calls, []);
});

test("Space still closes the preview when the guide is not open", () => {
  const { context: p, calls } = panel();
  p.previewClip.value = { kind: "image" };
  let prevented = false;
  p.onKey({ key: " ", preventDefault() { prevented = true; } });
  assert.equal(prevented, true);
  assert.deepEqual(calls, ["close"]);
});

test("merge keeps both API types, the guide, and the image-aware preview", () => {
  const apiImport = ast.statements.find((n) => ts.isImportDeclaration(n) && n.moduleSpecifier.text === "./api");
  const imported = new Set(apiImport.importClause.namedBindings.elements.map((n) => n.name.text));
  assert.ok(imported.has("KindFilter"));
  assert.ok(imported.has("MacosSetupState"));
  assert.match(app, /<AccessibilityGuide[\s\S]*?@finish="finishAccessibility"/);
  assert.match(app, /:aria-label="previewClip.kind === 'image' \? '图片预览' : '文本全文预览'"/);
  assert.ok(!app.includes("<<<<<<<"));
});
