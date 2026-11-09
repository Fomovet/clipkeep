# Windows 焦点回归

需要可交互的 Windows 桌面、Rust MSVC 工具链、Python 和 Playwright。测试会创建临时窗口并切换前台，期间不要操作其他窗口。剪贴板在测试结束时还原；不支持安全快照的格式会在改写前终止测试。

在仓库根目录安装测试依赖并启动 Vite：

```powershell
python -m pip install playwright
python -m playwright install chromium
npm --prefix ui run dev
```

另开终端编译原生测试窗口并运行：

```powershell
cargo build --manifest-path native/Cargo.toml --example focus_probe --locked
python native/tests/focus_regression.py --probe native/target/debug/examples/focus_probe.exe
python ui/tests/focus_ipc_smoke.py
```

`focus_regression.py` 直接引用生产 `focus.rs` 和 `win.rs`：两个 EDIT 各粘贴 20 次，浏览器 input、textarea、contenteditable 各 20 次，另验证固定面板在同一窗口切换输入框后的 4 次连续粘贴；覆盖原光标、控件隔离、500ms 修饰键超时、释放等待、外部切换、旧会话、控件销毁和窗口关闭。`focus_ipc_smoke.py` 使用官方 Tauri mock 验证前端并发与提示，不代表原生验收。

完整 Tauri 验收需要独立标识和数据目录。以下配置只用于测试，不能用于交付：

```powershell
$qaConfig = Get-Content native/tauri.conf.json -Raw | ConvertFrom-Json
$qaConfig.identifier = 'com.clipkeep.clipboard.focus-qa'
$qaConfig.productName = 'clipkeep Focus QA'
$qaConfig.app.windows[0].title = 'clipkeep Focus QA'
$qaConfig.app.windows[0] | Add-Member additionalBrowserArgs '--remote-debugging-port=9224'
$env:TAURI_CONFIG = $qaConfig | ConvertTo-Json -Depth 12 -Compress
cargo build --manifest-path native/Cargo.toml --locked
Remove-Item Env:TAURI_CONFIG
```

首次启动前，在 `%APPDATA%/com.clipkeep.clipboard.focus-qa/settings.json` 写入 UTF-8 JSON `{"hotkey":"ctrl+alt+v","checkUpdateOnStart":false}`，然后启动此 debug EXE（建议复制到单独测试目录）。保持 Vite 运行，执行：

```powershell
python native/tests/focus_app_regression.py --probe native/target/debug/examples/focus_probe.exe
```

该脚本检查独立数据目录后才进行操作，使用真实 IPC、Ctrl+Alt+V 钩子、鼠标、Enter、Ctrl+1、Shift+Enter、Esc 和关闭按钮；还覆盖外部隐藏、4096×4096 图片准备及取消后快速唤起。不要同时运行两组桌面测试。

交付 EXE 启动后，还可运行 `focus_release_smoke.py --exe <交付EXE> --pid <已核实的修复版进程ID> --probe <focus_probe.exe>`。该检查通过真实单实例唤起和原生按键验证 Enter、Ctrl+1、Shift+Enter、Esc，断言窗口确实属于给定进程，且返回两个原生控件各自的光标位置。

默认报告和截图写到 `docs/focus-qa/`（本地忽略目录）。可用 `--output` 指定报告目录；两个浏览器测试均可用 `CLIPKEEP_QA_BROWSER_EXE` 指定独立 Chromium。Tauri 测试使用应用自身的 WebView2。测试结束后退出 QA 实例并停止 Vite，交付构建使用默认标识、不启用调试端口。
