"""Real Windows focus/SendInput regression using disposable native and Chromium windows.

Build: cargo build --example focus_probe --features focus-probe
Run: python native/tests/focus_regression.py --probe <focus_probe.exe>
Requires playwright (`python -m playwright install chromium`). No user browser profile is used.
"""
import argparse
import json
import os
import queue
import subprocess
import threading
import time
from pathlib import Path
from playwright.sync_api import sync_playwright


class Probe:
    def __init__(self, executable, *args):
        self.process = subprocess.Popen(
            [str(executable), *args], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, text=True, encoding="utf-8", bufsize=1,
            creationflags=subprocess.CREATE_NO_WINDOW,
        )
        self.lines = queue.Queue()
        def read():
            for line in self.process.stdout:
                if line.startswith("QA:"):
                    self.lines.put(json.loads(line[3:]))
        threading.Thread(target=read, daemon=True).start()
        assert self.lines.get(timeout=15)["ready"]

    def request(self, command, **values):
        self.process.stdin.write(json.dumps({"cmd": command, **values}, ensure_ascii=False) + "\n")
        self.process.stdin.flush()
        return self.lines.get(timeout=30)

    def call(self, command, **values):
        result = self.request(command, **values)
        assert result["ok"], result
        if command in ("focus", "show"):
            assert result["value"], "Fixture failed to gain foreground; refusing to send input"
        return result["value"]

    def close(self):
        if self.process.poll() is None:
            try:
                self.call("quit")
                self.process.wait(timeout=5)
            finally:
                if self.process.poll() is None:
                    self.process.kill()


def wait_text(target, index, expected):
    deadline = time.monotonic() + 2
    while time.monotonic() < deadline:
        values = target.call("text")
        if values[index] == expected:
            return values
        time.sleep(.01)
    raise AssertionError({"expected": expected, "actual": values, "field": index})


def run(probe_path, out):
    fixture = Probe(probe_path, "--target")
    probe = Probe(probe_path)
    passed = []
    marker = "【焦点验收】"
    try:
        probe.call("clipboard", text=marker)
        # Native child HWNDs: original caret, both controls, and 20 activations each.
        for index in (0, 1):
            for cycle in range(20):
                assert fixture.call("focus", index=index)
                before = fixture.call("text")
                probe.call("capture")
                assert probe.call("busy")
                assert probe.call("show")
                probe.call("return", paste=True)
                after = wait_text(fixture, index, "左" + marker + "右")
                assert after[1-index] == before[1-index]
            passed.append(f"native-field-{index}:20/20-pastes-at-original-caret")
            print(passed[-1], flush=True)
        for index in (0, 1):
            fixture.call("focus", index=index)
            probe.call("capture")
            probe.call("show")
            probe.call("return", paste=False)
            assert fixture.call("text")[index] == "左右"
        passed.append("dismiss/copy-only-restores-native-control-without-paste")

        fixture.call("focus", index=0)
        probe.call("capture")
        probe.call("show")
        try:
            fixture.call("modifier", down=True)
            started = time.monotonic()
            result = probe.request("return", paste=True)
            elapsed = time.monotonic() - started
            assert not result["ok"] and "未能及时恢复" in result["error"], result
            assert .49 <= elapsed < 2, elapsed
            assert fixture.call("text")[0] == "左右"
        finally:
            fixture.call("modifier", down=False)
        passed.append("held-modifier-times-out-at-500ms-without-paste")

        fixture.call("focus", index=0)
        probe.call("capture")
        probe.call("show")
        fixture.call("modifier", down=True)
        release = threading.Timer(.08, lambda: fixture.call("modifier", down=False))
        release.start()
        try:
            probe.call("return", paste=True)
            wait_text(fixture, 0, "左" + marker + "右")
        finally:
            release.join()
            fixture.call("modifier", down=False)
        passed.append("paste-waits-for-modifier-release")

        # Clicking a different window must not be overridden by a delayed action.
        fixture.call("focus", index=0)
        probe.call("capture")
        probe.call("show")
        other = Probe(probe_path, "--target")
        try:
            other.call("focus", index=1)
            result = probe.request("return", paste=True)
            assert not result["ok"] and "其他窗口" in result["error"], result
            assert other.call("text") == ["左右", "左右"]
        finally:
            other.close()
        passed.append("external-window-switch-cancels-paste")
        fixture.call("focus", index=0)
        probe.call("capture")
        probe.call("show")
        assert probe.call("stale")
        assert not probe.request("return", paste=True)["ok"]
        passed.append("new-session-invalidates-old-action")
        probe.call("cancel")

        # A real browser GUI, with its render-host HWND and DOM selection preserved.
        with sync_playwright() as p:
            browser = p.chromium.launch(headless=False, executable_path=os.environ.get("CLIPKEEP_QA_BROWSER_EXE"),
                args=["--no-first-run", "--disable-background-networking"])
            page = browser.new_page(viewport={"width": 900, "height": 650})
            page.set_content('''<!doctype html><html lang="zh-CN"><meta charset="utf-8">
              <title>Clipkeep focus regression — local fixture</title>
              <style>body{font:20px system-ui;padding:40px}input,textarea,[contenteditable]{display:block;
              margin:16px 0;padding:12px;width:600px;border:1px solid #555;font:inherit}</style>
              <label>input<input id="input" value="左右"></label>
              <label>textarea<textarea id="textarea">左右</textarea></label>
              <label>contenteditable<div id="editable" contenteditable="true">左右</div></label>
              <p>Disposable local-only fixture. No requests or form submissions.</p></html>''')
            for name in ("input", "textarea", "editable"):
                for cycle in range(20):
                    page.bring_to_front()
                    page.evaluate('''name => {
                        const el=document.getElementById(name);
                        if(name==='editable') {
                          el.textContent='左右'; el.focus();
                          const range=document.createRange(); range.setStart(el.firstChild,1); range.collapse(true);
                          const sel=getSelection(); sel.removeAllRanges(); sel.addRange(range);
                        } else { el.value='左右'; el.focus(); el.setSelectionRange(1,1); }
                    }''', name)
                    assert page.evaluate("document.hasFocus()"), "Browser fixture is not foreground"
                    probe.call("capture")
                    assert probe.call("show"), (name, cycle, "probe did not gain foreground")
                    probe.call("return", paste=True)
                    page.wait_for_function('''([name,expected]) => {
                        const el=document.getElementById(name);
                        return (name==='editable'?el.textContent:el.value)===expected && document.activeElement===el;
                    }''', arg=[name, "左" + marker + "右"], timeout=2000)
                passed.append(f"browser-{name}:20/20-pastes-at-original-caret")
                print(passed[-1], flush=True)
            page.screenshot(path=str(out / "browser-focus.png"))
            browser.close()

        fixture.call("focus", index=0)
        probe.call("capture")
        probe.call("show")
        fixture.call("destroy-field", index=0)
        result = probe.request("return", paste=True)
        assert not result["ok"] and "输入框已失效" in result["error"], result
        passed.append("destroyed-control-rejected")
        probe.call("cancel")
        fixture.call("focus", index=1)
        probe.call("capture")
        probe.call("show")
        fixture.close()
        result = probe.request("return", paste=True)
        assert not result["ok"] and "窗口已关闭" in result["error"], result
        passed.append("closed-window-rejected")

        # Keep the panel visible and follow child focus changes within the same foreground
        # window. Each return must use the latest control, without starting a new session.
        fixture = Probe(probe_path, "--target")
        probe.call("track")
        fixture.call("focus", index=0)
        probe.call("capture")
        probe.call("show")
        probe.call("return", paste=True, keep_open=True)
        wait_text(fixture, 0, "左" + marker + "右")
        for index in (1, 0, 1):
            fixture.call("focus", index=index)
            before = fixture.call("text")
            deadline = time.monotonic() + 2
            while not probe.call("tracked-control"):
                assert time.monotonic() < deadline, "Child focus change was not tracked"
                time.sleep(.01)
            probe.call("show")
            probe.call("claim")
            probe.call("return", paste=True, keep_open=True)
            after = wait_text(fixture, index, "左" + marker + "右")
            assert after[1-index] == before[1-index], "Paste changed the previous control"
        passed.append("pinned-panel-follows-controls-within-same-foreground-window")
    finally:
        try:
            probe.call("restore-clipboard")
        finally:
            fixture.close()
            probe.close()
    report = {"result": "PASS", "nativePasteCycles": 44, "browserPasteCycles": 60, "checks": passed}
    (out / "focus-regression.json").write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps(report, ensure_ascii=False))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--probe", required=True, type=Path)
    parser.add_argument("--output", type=Path, default=Path("docs/focus-qa"))
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    run(args.probe.resolve(), args.output.resolve())
