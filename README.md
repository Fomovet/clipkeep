<div align="center">

<img src="assets/brand/clipkeep-512.png" width="96" alt="clipkeep 图标">

# clipkeep

**按 Win+V，找回你复制过的所有内容。**

轻量的剪贴板历史工具：记录不丢，随手能搜，一键粘贴回原处，还能把文字发到局域网里的另一台电脑。

[![Release](https://img.shields.io/github/v/release/Fomovet/clipkeep)](https://github.com/Fomovet/clipkeep/releases/latest)
[![Downloads](https://img.shields.io/github/downloads/Fomovet/clipkeep/total)](https://github.com/Fomovet/clipkeep/releases)
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS-blue)](#下载安装)
[![License](https://img.shields.io/github/license/Fomovet/clipkeep)](LICENSE)
[![LINUX DO](https://img.shields.io/badge/LINUX-DO-FFB003.svg?logo=data:image/svg%2bxml;base64,DQo8c3ZnIHhtbG5zPSJodHRwOi8vd3d3LnczLm9yZy8yMDAwL3N2ZyIgd2lkdGg9IjEwMCIgaGVpZ2h0PSIxMDAiPjxwYXRoIGQ9Ik00Ni44Mi0uMDU1aDYuMjVxMjMuOTY5IDIuMDYyIDM4IDIxLjQyNmM1LjI1OCA3LjY3NiA4LjIxNSAxNi4xNTYgOC44NzUgMjUuNDV2Ni4yNXEtMi4wNjQgMjMuOTY4LTIxLjQzIDM4LTExLjUxMiA3Ljg4NS0yNS40NDUgOC44NzRoLTYuMjVxLTIzLjk3LTIuMDY0LTM4LjAwNC0yMS40M1EuOTcxIDY3LjA1Ni0uMDU0IDUzLjE4di02LjQ3M0MxLjM2MiAzMC43ODEgOC41MDMgMTguMTQ4IDIxLjM3IDguODE3IDI5LjA0NyAzLjU2MiAzNy41MjcuNjA0IDQ2LjgyMS0uMDU2IiBzdHlsZT0ic3Ryb2tlOm5vbmU7ZmlsbC1ydWxlOmV2ZW5vZGQ7ZmlsbDojZWNlY2VjO2ZpbGwtb3BhY2l0eToxIi8+PHBhdGggZD0iTTQ3LjI2NiAyLjk1N3EyMi41My0uNjUgMzcuNzc3IDE1LjczOGE0OS43IDQ5LjcgMCAwIDEgNi44NjcgMTAuMTU3cS00MS45NjQuMjIyLTgzLjkzIDAgOS43NS0xOC42MTYgMzAuMDI0LTI0LjM4N2E2MSA2MSAwIDAgMSA5LjI2Mi0xLjUwOCIgc3R5bGU9InN0cm9rZTpub25lO2ZpbGwtcnVsZTpldmVub2RkO2ZpbGw6IzE5MTkxOTtmaWxsLW9wYWNpdHk6MSIvPjxwYXRoIGQ9Ik03Ljk4IDcwLjkyNmMyNy45NzctLjAzNSA1NS45NTQgMCA4My45My4xMTNRODMuNDI2IDg3LjQ3MyA2Ni4xMyA5NC4wODZxLTE4LjgxIDYuNTQ0LTM2LjgzMi0xLjg5OC0xNC4yMDMtNy4wOS0yMS4zMTctMjEuMjYyIiBzdHlsZT0ic3Ryb2tlOm5vbmU7ZmlsbC1ydWxlOmV2ZW5vZGQ7ZmlsbDojZjlhZjAwO2ZpbGwtb3BhY2l0eToxIi8+PC9zdmc+)](https://linux.do/t/topic/3043910/62)

**[下载最新版](https://github.com/Fomovet/clipkeep/releases/latest)** · [安装说明](#下载安装) · [快捷键](#快捷键) · [常见问题](#常见问题)

</div>

![浅色和深色主界面](assets/screenshots/hero.png)

## 为什么用 clipkeep

Windows 自带的剪贴板历史只存 25 条，没固定的记录重启就没了，也不能搜索。clipkeep 用同一个 Win+V 呼出，把这些都补上：

- **记录不会丢**：不限条数，重启后还在。只有超过容量上限（默认 500 MB）时才清理最旧的记录，收藏、置顶和分组里的内容一直保留。
- **找得到**：输入几个字，就能搜出几天前复制的那段文字。
- **贴得回去**：选中一条，直接粘贴到你刚才打字的地方，管理员窗口里也行。
- **两台电脑互传**：配对一次，右键就能把文字发到另一台电脑。加密直连，不用账号，不经过服务器。
- **够轻**：安装包约 3 MB，常驻托盘，不打扰。

## 功能一览

### 复制过的都在

- 记录文字和图片；在资源管理器里复制图片文件，也会把图片记下来
- 同一内容重复复制只保留一条，并把它顶到最前面
- 复制颜色代码（如 `#2563eb`）时，直接显示颜色色块
- 密码管理器标记为敏感的内容不会被记录

![搜索、右键菜单、全文预览和编辑](assets/screenshots/main.png)

### 整理和查找

- 输入即搜，结果实时出现
- 收藏、置顶、自定义分组；分组可以拖动排序，按 `Tab` 切换
- 长文本可以查看全文，也能直接修改

![收藏、分组和托盘菜单](assets/screenshots/organize.png)

### 粘贴回原处，连续粘贴也顺手

- `Enter` 粘贴到呼出面板前的输入位置，`Shift+Enter` 只复制不粘贴
- 点亮右上角的图钉固定面板后，可以依次点击多条连续粘贴，填表单、录数据很方便
- 在资源管理器里重命名文件时也能粘贴，不会打断文件名编辑
- 粘贴图片到网页时带上原文件名，不会每张都叫 `image.png`

### 局域网互传

- 两台电脑配对后，右键任意一条文字就能发过去
- 使用 ChaCha20-Poly1305 加密，两台电脑直接连接，不经过服务器
- 对方收到的内容只存进历史记录，不会覆盖他正在用的剪贴板

![发送端、接收端和配对设置](assets/screenshots/lan.png)

### 其他细节

- 浅色、深色、跟随系统三种外观
- 托盘常驻、开机自启、一键暂停记录
- 导出和导入备份，导入时自动跳过重复内容
- 应用内检查更新；Windows 版下载的安装包会先核对 SHA-256，再由你确认安装

![常规、存储、快捷键和数据设置](assets/screenshots/settings.png)

## 下载安装

到 [Releases](https://github.com/Fomovet/clipkeep/releases/latest) 下载最新版。

### Windows

| 文件 | 说明 |
|---|---|
| `clipkeep_x.y.z_x64-setup.exe`（推荐） | 安装到当前用户目录，全新安装后首次运行会自动开启开机自启，支持应用内更新 |
| `clipkeep_x.y.z_x64.exe` | 单文件版，放在任意目录直接运行；开机自启需在设置里手动开启 |

装好后按 **Win+V** 即可使用。

- 需要 Windows 10 / 11（64 位）和 WebView2 运行时（系统一般自带，安装包会自动补装）
- clipkeep 以管理员身份运行，手动打开时会弹出一次 UAC 确认，开机自启时不弹，原因见[常见问题](#为什么需要管理员权限)
- 卸载：安装版在系统「设置 → 应用」里卸载；单文件版先在设置里关闭开机自启，再删除 exe

### macOS（Apple Silicon）

下载 `clipkeep_x.y.z_aarch64.dmg`，把 `clipkeep.app` 拖进「应用程序」。默认呼出键是 **⌘⇧V**。

- 需要 macOS 11 及以上、M 系列芯片
- 安装包未经 Apple 公证，首次打开如果被拦截，到「系统设置 → 隐私与安全性」里允许打开
- 自动粘贴需要在「系统设置 → 隐私与安全性 → 辅助功能」里允许 clipkeep；记录、搜索和复制不需要这个权限
- 设置里可以隐藏程序坞图标，只保留菜单栏图标
- 暂不支持应用内安装更新，有新版本时会提示前往下载页

### 数据保存在哪

- Windows：`%APPDATA%\com.clipkeep.clipboard\`
- macOS：`~/Library/Application Support/com.clipkeep.clipboard/`

升级和卸载都不会删除这些数据；想彻底清除，手动删掉这个目录即可。

## 快捷键

| 按键 | 作用 |
|---|---|
| `Win+V` | 呼出 / 隐藏面板（可在设置里改成 `Ctrl+Shift+V`、`Alt+V`、`Ctrl+Alt+V` 或 ``Ctrl+` ``） |
| `↑` `↓` / `PgUp` `PgDn` | 选择条目 |
| `Enter` | 粘贴到原来的窗口 |
| `Shift+Enter` | 只复制，不粘贴 |
| `Ctrl+1` ~ `Ctrl+9` | 直接粘贴第 1～9 条 |
| `Ctrl+P` / `Ctrl+S` / `Ctrl+D` | 置顶 / 收藏 / 删除 |
| `Tab` / `Shift+Tab` | 在剪贴板、收藏和各个分组之间切换 |
| `Esc` | 清空搜索；搜索框为空时隐藏面板 |

macOS 上默认呼出键是 `⌘⇧V`，面板里的 `Ctrl` 组合键也可以用 `⌘`。

## 配置局域网互传

1. 接收方：设置 → 局域网传送 → 打开接收 → 复制配对码
2. 发送方：在同一页面点「添加设备」，粘贴配对码
3. 右键文本条目，在「发送到设备」里选择对方电脑

配对是单向的，想互相发送就两边各添加一次。互传使用 `45788/TCP` 端口，首次开启时如果防火墙弹窗，请点「允许」。

## 和其他工具对比

|  | clipkeep | Ditto | CopyQ | EcoPaste | PasteBar |
|---|---|---|---|---|---|
| 技术栈 | Rust + Tauri 2 + Vue 3 | C++ / MFC | C++ / Qt | Rust + Tauri 2 + React | Rust + Tauri + React |
| 平台 | Windows / macOS（Apple Silicon） | Windows | Windows / macOS / Linux | Windows / macOS / Linux | Windows / macOS |
| 分发方式 | 安装包 / 单文件版 / dmg | 安装包 / 便携版 | 安装包 / 便携版 | 安装包 | 安装包 |
| 局域网互传 | 内置，加密直连，免账号 | 支持，需手动配置 IP | — | — | — |
| 往管理员窗口粘贴 | 默认支持 | 支持，按需弹出 UAC | 需以管理员运行 | 需在设置中开启 | 需以管理员运行 |
| 开源协议 | MIT | GPL-3.0 | GPL-3.0 | Apache-2.0 | CC BY-NC（有限商用例外） |

clipkeep 专注文字和图片。如果需要记录富文本或任意文件，可以考虑 CopyQ 和 EcoPaste。

## 常见问题

### 为什么需要管理员权限？

Windows 不允许普通程序向管理员窗口发送按键，也不会把管理员窗口里的按键交给普通程序。clipkeep 以管理员身份运行，所以在管理员窗口里也能用 Win+V 呼出并粘贴。macOS 版不需要提权，但自动粘贴要在「辅助功能」里授权，系统会在第一次粘贴时提示。

### 按 Win+V 弹出的是系统剪贴板？

少数环境下 Win+V 会被系统抢先处理，可以在设置里把快捷键改成 `Ctrl+Shift+V`。

### 选中记录后没有自动粘贴？

clipkeep 会粘贴到呼出面板前的输入位置。如果原窗口已经关闭，或者焦点没法安全恢复，它会停止自动粘贴并提示你；这时内容已经复制好了，手动按 `Ctrl+V` 即可。

### 重命名文件时为什么不能搜索？

在资源管理器重命名文件时呼出面板，面板不会抢走焦点，这样文件名编辑框才不会关掉。这时请用鼠标点选要粘贴的记录，或再按一次快捷键收起；完成重命名后就能正常搜索了。

### 截图工具复制的图片没有被记录？

PixPin 等只提供位图格式的截图工具已经支持。如果还有软件复制的内容记录不到，请[提 Issue](https://github.com/Fomovet/clipkeep/issues)。

### 数据安全吗？

数据只保存在本机，只有你主动用局域网发送时，内容才会传到另一台电脑。本地数据库和备份文件目前没有加密，复制敏感内容时请注意。

## 参与开发

<details>
<summary>技术栈和构建方法</summary>

| 模块 | 实现 |
|---|---|
| 应用框架 | Tauri 2，Rust 2024 edition |
| 前端 | Vue 3.5，TypeScript 6，Vite 8 |
| 数据存储 | SQLite（rusqlite 内置编译，WAL 模式） |
| 剪贴板 | clipboard-master 监听 + arboard 读写 |
| 全局快捷键 | Windows：低级键盘钩子 + SendInput；macOS：Carbon RegisterEventHotKey + CGEvent |
| 图片处理 | image crate，支持 PNG / JPEG / GIF / WebP / BMP / TIFF |
| 内容去重 | blake3 |
| 传输加密 | ChaCha20-Poly1305 |

Windows：

```powershell
npm --prefix ui ci
.\ui\node_modules\.bin\tauri.cmd dev                 # 开发模式
.\ui\node_modules\.bin\tauri.cmd build --no-bundle   # 单文件版：native/target/release/clipkeep.exe
.\ui\node_modules\.bin\tauri.cmd build               # 同时生成安装包：native/target/release/bundle/nsis/
```

macOS：

```bash
npm --prefix ui ci
npm --prefix ui run build
cd native
../ui/node_modules/.bin/tauri build --bundles app,dmg \
  --config '{"build":{"beforeBuildCommand":null}}'
```

产物在 `native/target/release/bundle/macos/`（`.app`）和 `bundle/dmg/`（`.dmg`）。

CI 会在 Windows 和 macOS（Apple Silicon）上分别运行 `cargo fmt`、`cargo test` 和 `clippy -D warnings`，并在 macOS 上打出 `.dmg`，作为构建产物保留 30 天。Windows 焦点回归测试的运行方式见[焦点回归测试](native/tests/README.md)。

</details>

## 反馈

遇到问题或有建议，欢迎[提 Issue](https://github.com/Fomovet/clipkeep/issues)。

## 许可证

[MIT](LICENSE)
