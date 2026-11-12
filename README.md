<div align="center">

<img src="assets/brand/clipkeep-512.png" width="96" alt="clipkeep 图标">

# clipkeep

Windows / macOS 剪贴板历史工具

[![Release](https://img.shields.io/github/v/release/Fomovet/clipkeep)](https://github.com/Fomovet/clipkeep/releases/latest)
[![LINUX DO](https://img.shields.io/badge/LINUX-DO-FFB003.svg?logo=data:image/svg%2bxml;base64,DQo8c3ZnIHhtbG5zPSJodHRwOi8vd3d3LnczLm9yZy8yMDAwL3N2ZyIgd2lkdGg9IjEwMCIgaGVpZ2h0PSIxMDAiPjxwYXRoIGQ9Ik00Ni44Mi0uMDU1aDYuMjVxMjMuOTY5IDIuMDYyIDM4IDIxLjQyNmM1LjI1OCA3LjY3NiA4LjIxNSAxNi4xNTYgOC44NzUgMjUuNDV2Ni4yNXEtMi4wNjQgMjMuOTY4LTIxLjQzIDM4LTExLjUxMiA3Ljg4NS0yNS40NDUgOC44NzRoLTYuMjVxLTIzLjk3LTIuMDY0LTM4LjAwNC0yMS40M1EuOTcxIDY3LjA1Ni0uMDU0IDUzLjE4di02LjQ3M0MxLjM2MiAzMC43ODEgOC41MDMgMTguMTQ4IDIxLjM3IDguODE3IDI5LjA0NyAzLjU2MiAzNy41MjcuNjA0IDQ2LjgyMS0uMDU2IiBzdHlsZT0ic3Ryb2tlOm5vbmU7ZmlsbC1ydWxlOmV2ZW5vZGQ7ZmlsbDojZWNlY2VjO2ZpbGwtb3BhY2l0eToxIi8+PHBhdGggZD0iTTQ3LjI2NiAyLjk1N3EyMi41My0uNjUgMzcuNzc3IDE1LjczOGE0OS43IDQ5LjcgMCAwIDEgNi44NjcgMTAuMTU3cS00MS45NjQuMjIyLTgzLjkzIDAgOS43NS0xOC42MTYgMzAuMDI0LTI0LjM4N2E2MSA2MSAwIDAgMSA5LjI2Mi0xLjUwOCIgc3R5bGU9InN0cm9rZTpub25lO2ZpbGwtcnVsZTpldmVub2RkO2ZpbGw6IzE5MTkxOTtmaWxsLW9wYWNpdHk6MSIvPjxwYXRoIGQ9Ik03Ljk4IDcwLjkyNmMyNy45NzctLjAzNSA1NS45NTQgMCA4My45My4xMTNRODMuNDI2IDg3LjQ3MyA2Ni4xMyA5NC4wODZxLTE4LjgxIDYuNTQ0LTM2LjgzMi0xLjg5OC0xNC4yMDMtNy4wOS0yMS4zMTctMjEuMjYyIiBzdHlsZT0ic3Ryb2tlOm5vbmU7ZmlsbC1ydWxlOmV2ZW5vZGQ7ZmlsbDojZjlhZjAwO2ZpbGwtb3BhY2l0eToxIi8+PC9zdmc+)](https://linux.do/t/topic/3043910/62)

</div>

![浅色和深色主界面](assets/screenshots/hero.png)

Windows 自带的 Win+V 只存 25 条，未固定的记录重启就清空，也没法搜索。clipkeep 同样用 Win+V 呼出，记录会一直保留，可以搜索和分组，也能把文字发到局域网里的另一台电脑。

## 下载安装

从 [Releases](https://github.com/Fomovet/clipkeep/releases/latest) 下载。安装包和单文件版功能相同，区别如下：

|  | 安装包（推荐） | 单文件版 |
|---|---|---|
| 文件 | `clipkeep_x.y.z_x64-setup.exe` | `clipkeep_x.y.z_x64.exe` |
| 安装 | 装到当前用户目录，不需要管理员权限 | 放到任意目录直接运行 |
| 开机自启 | 首次运行后自动开启 | 在设置里手动开启 |
| 升级 | 运行新版安装包覆盖 | 替换 exe 文件 |
| 卸载 | 在系统「设置 → 应用」里卸载 | 先在设置里关闭开机自启，再删除 exe |

- 系统要求：Windows 10 / 11（64 位），依赖 WebView2 运行时（系统通常已自带，安装包会自动补装）
- 历史记录和设置保存在 `%APPDATA%\com.clipkeep.clipboard\`，升级和卸载都不会删除；想彻底清除，手动删掉这个目录即可
- 程序以管理员身份运行，手动打开时会弹出一次 UAC 确认，开机自启不弹，原因见[常见问题](#常见问题)

装好后按 Win+V 即可使用。

### macOS（Apple Silicon）

本 fork 额外提供 macOS 原生（arm64）版本，功能与交互和 Windows 版一致，只是把平台相关的实现换成 AppKit / Carbon / WKWebView 对应能力。

- 从本仓库的 Releases 下载 `.dmg`，把 `clipkeep.app` 拖进「应用程序」即可；首次打开若被 Gatekeeper 拦截，右键选择「打开」。
- 首次使用「粘贴到原窗口」时，macOS 会要求授予 **辅助功能** 权限（系统设置 → 隐私与安全性 → 辅助功能）。全局快捷键、剪贴板记录和复制不需要该权限。
- 默认快捷键为 `⌘⇧V`（设置里的其它组合用 `⌃` / `⌥` / `⇧` 标记）。macOS 版没有 in-app 自动升级，更新时从发布页下载新版 `.dmg` 覆盖即可。
- 数据目录：`~/Library/Application Support/com.clipkeep.clipboard/`；开机自启由 LaunchAgent 管理，在设置里开关。
- 「设置 → 常规 → 在程序坞中隐藏图标」可立即隐藏或恢复 Dock 图标，并保留到下次启动；隐藏时仍可通过菜单栏白色图标或快捷键打开。
- 开机自启、托盘、局域网互传、图片记录、导入导出等行为与 Windows 版相同。

从源码构建：

```bash
npm --prefix ui ci
npm --prefix ui run build
cd native
cargo test --locked
../ui/node_modules/.bin/tauri build --bundles app,dmg \
  --config '{"build":{"beforeBuildCommand":null}}'
```

产物在 `native/target/release/bundle/macos/`（`.app`）和 `bundle/dmg/`（`.dmg`）。

## 功能

### 记录
- 历史不限条数，只按容量清理（默认 500 MB，可设为不限）
- 收藏、置顶和分组里的内容不会被自动清理
- 记录文字和图片，在资源管理器里复制图片文件也能记下来
- 重复复制同样的内容只保留一条

![搜索、右键菜单、全文预览和编辑](assets/screenshots/main.png)

### 查找和整理
- 实时搜索
- 收藏、置顶、自定义分组，分组可拖动排序
- 长文本可以查看全文，也可以直接修改

![收藏、分组和托盘菜单](assets/screenshots/organize.png)

### 局域网互传
- 两台电脑配对后，可以把任意一条文字发给对方
- 内容用 ChaCha20-Poly1305 加密，两台电脑直接连接，不经过服务器
- 对方收到的内容只存入历史记录，不会覆盖他当前的剪贴板

![发送端、接收端和配对设置](assets/screenshots/lan.png)

### 其他
- 浅色、深色、跟随系统三种外观
- 托盘常驻、开机自启、暂停记录
- 导出和导入备份，导入时会跳过重复内容

![常规、存储、快捷键和数据设置](assets/screenshots/settings.png)

## 快捷键

| 按键 | 作用 |
|---|---|
| `Win+V` | 呼出 / 隐藏（可在设置里改成 `Ctrl+Shift+V` 等） |
| `↑` `↓` / `PgUp` `PgDn` | 选择条目 |
| `Enter` | 粘贴到原来的窗口 |
| `Shift+Enter` | 只复制，不粘贴 |
| `Ctrl+1` ~ `Ctrl+9` | 直接粘贴第 1～9 条 |
| `Ctrl+P` / `Ctrl+S` / `Ctrl+D` | 置顶 / 收藏 / 删除 |
| `Tab` / `Shift+Tab` | 切换剪贴板、收藏和各个分组 |
| `Esc` | 清空搜索；搜索框为空时隐藏窗口 |

macOS 上把表中的 `Win` 换成 `⌘`、`Ctrl` 换成 `⌃`（默认呼出键为 `⌘⇧V`）；窗口内快捷键的参数键同样兼容 Command 键。

## 配置局域网互传

1. 接收方：设置 → 局域网传送 → 打开接收 → 复制配对码
2. 发送方：在同一页面点「添加设备」，粘贴配对码
3. 右键文本条目，在「发送到设备」里选择对方电脑

配对是单向的，想互相发送就两边各添加一次。使用 `45788/TCP` 端口，首次开启时如果防火墙弹窗，请点「允许」。

## 和其他工具对比

|  | clipkeep | Ditto | CopyQ | EcoPaste | PasteBar |
|---|---|---|---|---|---|
| 技术栈 | Rust + Tauri 2 + Vue 3 | C++ / MFC | C++ / Qt | Rust + Tauri 2 + React | Rust + Tauri + React |
| 平台 | Windows / macOS（Apple Silicon） | Windows | Windows / macOS / Linux | Windows / macOS / Linux | Windows / macOS |
| 分发方式 | 安装包 / 单文件版 | 安装包 / 便携版 | 安装包 / 便携版 | 安装包 | 安装包 |
| 局域网互传 | 内置，加密直连，免账号 | 支持，需手动配置 IP | — | — | — |
| 往管理员窗口粘贴 | 默认支持 | 支持，按需弹出 UAC | 需以管理员运行 | 需在设置中开启 | 需以管理员运行 |
| 开源协议 | MIT | GPL-3.0 | GPL-3.0 | Apache-2.0 | CC BY-NC（有限商用例外） |

clipkeep 只记录文字和图片；macOS 版目前仅支持 Apple Silicon（arm64）。如果需要记录富文本和任意文件，可以考虑 CopyQ 和 EcoPaste。

## 常见问题

### 为什么需要管理员权限？
Windows 不允许普通程序向管理员窗口发送按键，也不会把管理员窗口里的按键交给普通程序。clipkeep 以管理员身份运行，所以在管理员窗口里也能用 Win+V 呼出并粘贴。macOS 不需要提权，但合成 `⌘V` 按键需要在「辅助功能」里授权（系统会在首次粘贴时提示）。

### 按 Win+V 弹出的是系统剪贴板？
少数环境下 Win+V 会被系统抢先处理，可以在设置里把快捷键改成 `Ctrl+Shift+V`。

### 截图工具复制的图片没有被记录？
v0.2.0 已修复 PixPin 等只提供位图格式的截图工具。如果还有软件复制的内容记录不到，请[提 Issue](https://github.com/Fomovet/clipkeep/issues)。

### 选择记录或关闭面板后，如何返回原输入位置？

面板唤起前会保存原窗口和输入控件。选择记录、Shift+Enter 仅复制、Esc 最终关闭、关闭按钮或快捷键收起时，先恢复原输入位置；自动粘贴还会等待修饰键释放并确认焦点。点击其他软件时自动隐藏不会抢回焦点。原窗口失效或恢复超时会停止自动粘贴，已复制的内容可手动粘贴；面板隐藏后的错误会在下次打开时提示。

在资源管理器重命名文件时呼出面板，会以不激活窗口的方式显示，保留文件名编辑框和光标。此时请用鼠标选择要粘贴的记录，或再次按快捷键收起；搜索和键盘选中暂不可用。点击面板外会自动收起，完成重命名后即可正常搜索。

### 数据安全吗？
数据只保存在本机，只有你主动用局域网发送时，内容才会传到另一台电脑。本地数据库和备份文件目前没有加密，复制敏感内容时请注意。

## 开发

| 模块 | 实现 |
|---|---|
| 应用框架 | Tauri 2，Rust 2024 edition |
| 前端 | Vue 3.5，TypeScript 6，Vite 8 |
| 数据存储 | SQLite（rusqlite 内置编译，WAL 模式） |
| 剪贴板 | clipboard-master 监听 + arboard 读写 |
| 全局快捷键 | Windows：windows-sys 低级键盘钩子 + SendInput；macOS：Carbon RegisterEventHotKey + CGEvent |
| 图片处理 | image crate，支持 PNG / JPEG / GIF / WebP / BMP / TIFF |
| 内容去重 | blake3 |
| 传输加密 | ChaCha20-Poly1305 |

```powershell
npm --prefix ui ci
.\ui\node_modules\.bin\tauri.cmd dev                 # 开发模式
.\ui\node_modules\.bin\tauri.cmd build --no-bundle   # 单文件版：native/target/release/clipkeep.exe
.\ui\node_modules\.bin\tauri.cmd build               # 同时生成安装包：native/target/release/bundle/nsis/
```

CI 会在 Windows 和 macOS（Apple Silicon）两个 runner 上分别运行 `cargo fmt`、`cargo test` 和 `clippy -D warnings`，并在 macOS 上构建 `.app` / `.dmg`。

Windows 焦点回归使用真实原生输入框、Chromium 和独立 Tauri 测试实例，运行方式见 [焦点回归测试](native/tests/README.md)。前端 IPC 模拟仅验证重复提交和错误提示。

## 反馈

遇到问题或有建议，请[提 Issue](https://github.com/Fomovet/clipkeep/issues)。

## 许可证

[MIT](LICENSE)
