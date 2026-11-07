"""Actual Tauri/WebView2 + native hotkey/SendInput integration (no IPC mock).

Start an isolated debug build with identifier com.clipkeep.clipboard.focus-qa and
main.additionalBrowserArgs=--remote-debugging-port=9224. Its hotkey must be Ctrl+Alt+V.
Requires the Vite dev server. Only that isolated data directory may be used.
"""
import argparse
import json
import os
import time
from pathlib import Path
from playwright.sync_api import sync_playwright, expect
from focus_regression import Probe, wait_text


def run(probe_path, out):
    fixture = Probe(probe_path, "--target")
    rename_fixture = Probe(probe_path, "--explorer-target")
    clipboard = Probe(probe_path)
    marker = "【完整程序焦点验收】"
    checks = []
    try:
        with sync_playwright() as p:
            browser = p.chromium.connect_over_cdp(
                os.environ.get("CLIPKEEP_QA_CDP", "http://127.0.0.1:9224")
            )
            page = next(page for context in browser.contexts for page in context.pages if page.url == "http://localhost:1420/")
            settings = page.evaluate('async () => (await import("/src/api.ts")).api.getSettings()')
            expected_data_suffix = os.environ.get(
                "CLIPKEEP_QA_DATA_DIR_SUFFIX", "com.clipkeep.clipboard.focus-qa"
            )
            assert settings["dataDir"].endswith(expected_data_suffix), "Refusing to use unexpected app data"
            assert settings["settings"]["hotkey"] == "ctrl+alt+v"
            page.evaluate('async () => { try { await (await import("/src/api.ts")).api.hideWindow(); } catch {} }')
            page.evaluate('''async () => {
                const {listen}=await import('/node_modules/@tauri-apps/api/event.js');
                window.qaShows=0; window.qaHides=0;
                window.qaModes=[];
                await listen('window-shown',event=>{window.qaShows++;window.qaModes.push(event.payload)});
                await listen('window-hidden',()=>window.qaHides++);
            }''')
            clipboard.call("clipboard", text=marker)
            time.sleep(.2)  # Allow the real clipboard monitor to capture this new record.

            def show(index):
                fixture.call("focus", index=index)
                shown = page.evaluate("qaShows")
                assert fixture.call("hotkey")
                page.wait_for_function("n=>qaShows>n", arg=shown, timeout=4000)
                expect(page.get_by_placeholder("搜索文本…")).to_be_focused()

            def restored(index):
                deadline = time.monotonic() + 2
                while time.monotonic() < deadline:
                    status = fixture.call("status")
                    if status == {"foreground": True, "field": index}:
                        return
                    time.sleep(.01)
                raise AssertionError(status)

            for method in ("mouse", "enter", "ctrl-number", "copy-only", "escape", "close", "hotkey"):
                for index in (0, 1):
                    show(index)
                    hidden = page.evaluate("qaHides")
                    if method in ("mouse", "enter", "ctrl-number", "copy-only"):
                        page.get_by_placeholder("搜索文本…").fill(marker)
                        expect(page.get_by_text(marker, exact=True)).to_be_visible()
                        if method == "mouse":
                            page.get_by_text(marker, exact=True).click()
                        else:
                            page.keyboard.press({"enter": "Enter", "ctrl-number": "Control+1", "copy-only": "Shift+Enter"}[method])
                    elif method == "escape":
                        page.keyboard.press("Escape")
                    elif method == "close":
                        page.get_by_role("button", name="隐藏 (Esc)", exact=True).click()
                    else:
                        assert fixture.call("hotkey")
                    page.wait_for_function("n=>qaHides>n", arg=hidden, timeout=4000)
                    restored(index)
                    if method in ("mouse", "enter", "ctrl-number"):
                        wait_text(fixture, index, "左" + marker + "右")
                    else:
                        assert fixture.call("text")[index] == "左右"
                checks.append(method + ":both-native-controls-pass")
                print(checks[-1], flush=True)

            # A real external activation hides the popup without taking focus back.
            show(0)
            hidden = page.evaluate("qaHides")
            fixture.call("focus", index=1)
            page.wait_for_function("n=>qaHides>n", arg=hidden, timeout=4000)
            restored(1)
            checks.append("external-click-hide-keeps-new-control")

            # A 4096×4096 clipboard image exercises real image preparation on the worker.
            clipboard.call("image")
            deadline = time.monotonic() + 15
            image_id = None
            while time.monotonic() < deadline:
                clips = page.evaluate('''async () => (await import('/src/api.ts')).api.listClips('all', null, '')''')
                image_id = next((clip['id'] for clip in clips if clip['kind'] == 'image' and clip['width'] == 4096), None)
                if image_id is not None:
                    break
                time.sleep(.05)
            assert image_id is not None, 'Clipboard monitor did not capture the fixture image'
            show(0)
            hidden = page.evaluate("qaHides")
            started = time.monotonic()
            page.locator('.item').filter(has=page.locator('.image-summary')).first.click()
            page.wait_for_function("n=>qaHides>n", arg=hidden, timeout=15000)
            restored(0)
            image_ms = round((time.monotonic() - started) * 1000)
            checks.append(f"4096x4096-image-preparation-restores-control:{image_ms}ms")

            show(0)
            page.evaluate('''id => {
                window.qaOldImage = (async () => {
                    try { await (await import('/src/api.ts')).api.copyClip(id); return 'completed'; }
                    catch(error) { return String(error); }
                })();
            }''', image_id)
            page.evaluate('async () => { try { await (await import("/src/api.ts")).api.hideWindow(); } catch {} }')
            restored(0)
            show(1)
            old_result = page.evaluate("qaOldImage")
            assert old_result == "操作已取消", old_result
            expect(page.get_by_placeholder("搜索文本…")).to_be_focused()
            page.keyboard.press("Escape")
            restored(1)
            assert fixture.call("text") == ["左右", "左右"]
            checks.append("slow-image-cancelled-before-new-session-without-moving-new-target")

            # A CabinetWClass fixture with a focused Edit reproduces Explorer's inline rename
            # focus contract. The app must show above it without activating, then paste by mouse.
            rename_fixture.call("focus", index=0)
            shown = page.evaluate("qaShows")
            assert rename_fixture.call("hotkey")
            page.wait_for_function("n=>qaShows>n", arg=shown, timeout=4000)
            expect(page.get_by_placeholder("重命名时请用鼠标选择记录")).to_be_disabled()
            assert page.evaluate("qaModes.at(-1)") is True
            assert rename_fixture.call("status") == {"foreground": True, "field": 0}
            hidden = page.evaluate("qaHides")
            page.get_by_text(marker, exact=True).first.click()
            page.wait_for_function("n=>qaHides>n", arg=hidden, timeout=4000)
            wait_text(rename_fixture, 0, "左" + marker + "右")
            assert rename_fixture.call("status") == {"foreground": True, "field": 0}
            checks.append("explorer-rename-mode-keeps-edit-focused-and-pastes-by-mouse")

            rename_fixture.call("focus", index=1)
            shown = page.evaluate("qaShows")
            assert rename_fixture.call("hotkey")
            page.wait_for_function("n=>qaShows>n", arg=shown, timeout=4000)
            expect(page.get_by_placeholder("重命名时请用鼠标选择记录")).to_be_disabled()
            hidden = page.evaluate("qaHides")
            assert rename_fixture.call("hotkey")
            page.wait_for_function("n=>qaHides>n", arg=hidden, timeout=4000)
            assert rename_fixture.call("status") == {"foreground": True, "field": 1}
            checks.append("explorer-rename-hotkey-dismiss-preserves-both-controls")

            rename_fixture.call("focus", index=0)
            shown = page.evaluate("qaShows")
            assert rename_fixture.call("hotkey")
            page.wait_for_function("n=>qaShows>n", arg=shown, timeout=4000)
            hidden = page.evaluate("qaHides")
            fixture.call("focus", index=1)
            page.wait_for_function("n=>qaHides>n", arg=hidden, timeout=4000)
            assert fixture.call("status") == {"foreground": True, "field": 1}
            checks.append("explorer-rename-external-activation-hides-without-stealing-focus")

            show(0)
            page.screenshot(path=str(out / "actual-tauri-focus.png"))
            page.keyboard.press("Escape")
            restored(0)
            # Do not browser.close(): the app owns this WebView2 instance.
    finally:
        try:
            clipboard.call("restore-clipboard")
        finally:
            fixture.close()
            rename_fixture.close()
            clipboard.close()
    report = {"result": "PASS", "realTauri": True, "mockIPC": False, "checks": checks}
    (out / "focus-app-regression.json").write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps(report, ensure_ascii=False))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--probe", required=True, type=Path)
    parser.add_argument("--output", type=Path, default=Path("docs/focus-qa"))
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    run(args.probe.resolve(), args.output.resolve())
