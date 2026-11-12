"""Smoke the delivered release EXE via its real single-instance activation and SendInput.

The given --pid must be the already running release EXE, not a debug/production-old instance.
"""
import argparse
import json
import subprocess
import time
from pathlib import Path
from focus_regression import Probe, wait_text


def run(executable, pid, probe_path, out):
    fixture = Probe(probe_path, "--target")
    clipboard = Probe(probe_path)
    marker = "【修复版EXE验收】"
    passed = []
    try:
        clipboard.call("clipboard", text=marker)
        time.sleep(.2)
        for method in ("enter", "ctrl-number", "copy-only", "escape"):
            for index in (0, 1):
                fixture.call("focus", index=index)
                duplicate = subprocess.Popen([str(executable)], creationflags=subprocess.CREATE_NO_WINDOW)
                duplicate.wait(timeout=5)
                deadline = time.monotonic() + 5
                while clipboard.call("foreground-process") != pid and time.monotonic() < deadline:
                    time.sleep(.01)
                assert clipboard.call("foreground-process") == pid, "Release popup not in foreground"
                # Let the real WebView's window-shown handler restore search focus and records.
                time.sleep(.15)
                assert clipboard.call("submit", method=method)
                deadline = time.monotonic() + 3
                while time.monotonic() < deadline:
                    if fixture.call("status") == {"foreground": True, "field": index}:
                        break
                    time.sleep(.01)
                assert fixture.call("status") == {"foreground": True, "field": index}
                if method in ("enter", "ctrl-number"):
                    wait_text(fixture, index, "左" + marker + "右")
                else:
                    assert fixture.call("text")[index] == "左右"
            passed.append(method + ":both-controls-pass")
            print(passed[-1], flush=True)
    finally:
        try:
            clipboard.call("restore-clipboard")
        finally:
            fixture.close()
            clipboard.close()
    report = {"result":"PASS", "executable":str(executable), "pid":pid, "checks":passed}
    (out / "focus-release-smoke.json").write_text(json.dumps(report,ensure_ascii=False,indent=2),encoding="utf-8")
    print(json.dumps(report,ensure_ascii=False))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", required=True, type=Path)
    parser.add_argument("--pid", required=True, type=int)
    parser.add_argument("--probe", required=True, type=Path)
    parser.add_argument("--output", default=Path("docs/focus-qa"), type=Path)
    args = parser.parse_args()
    args.output.mkdir(parents=True,exist_ok=True)
    run(args.exe.resolve(),args.pid,args.probe.resolve(),args.output.resolve())
