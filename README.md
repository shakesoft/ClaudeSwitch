# Claude 账号切换器

用 Rust + [Slint](https://github.com/slint-ui/slint) 写的 Windows 小工具：在多个 Claude 桌面版账号之间切换，并附带通用的 VPN 断线保护和网络泄漏自查。

## 功能

**账号切换**
- 每个账号是一份 Claude 桌面版数据目录（`%APPDATA%\Claude`）的快照，切换时自动关闭 Claude 桌面版，再换入目标账号的数据
- 添加、重命名、删除账号；删除需点两次确认，快照会移入回收站
- "登录"按钮：在新终端里执行 `claude auth logout && claude auth login`，重新登录 Claude Code

**网络自检**
- 泄漏自查：对比 HTTP、UDP（STUN，即 WebRTC 会暴露的地址）、IPv6 的出口地址，并列出 DNS 服务器
- 断线保护：指定 VPN 网卡名关键字和进程名，网卡断开时立即结束这些进程；设置和开关状态会保存

**托盘与自启**
- 关闭窗口时收到托盘，右键托盘图标选"退出"才真正退出
- 可设置登录 Windows 后自动启动，启动后直接进托盘

## 环境要求

- Windows 10 / 11
- Rust 1.89 或更高
- Visual Studio Build Tools（"使用 C++ 的桌面开发"工作负载）

```powershell
winget install Microsoft.VisualStudio.2022.BuildTools --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
```

## 编译与运行

在 **PowerShell** 中执行（Git Bash 自带的 `link` 命令会挡住 MSVC 链接器，导致编译失败）：

```powershell
cargo run                  # 调试版
cargo build --release      # 正式版：target\release\claude-switcher.exe，不带控制台窗口
cargo test                 # 单元测试
```

首次编译需要几分钟。带 `--tray` 参数启动时不显示窗口，只放托盘图标。

## 数据位置

| 内容 | 位置 |
|---|---|
| 账号快照 | `%USERPROFILE%\.claude-switcher\accounts\<账号名>\` |
| 当前账号名 | `%USERPROFILE%\.claude-switcher\current.txt` |
| 断线保护设置 | `%USERPROFILE%\.claude-switcher\guard.txt` |
| 开机自启 | 注册表 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 中的 `ClaudeSwitcher` |

## 注意事项

- 本工具仅用于切换本人名下的多个账号，请遵守 [Anthropic 的使用条款](https://www.anthropic.com/legal)
- 第一次切换账号前，建议先手动备份 `%APPDATA%\Claude`
- 账号切换只换 Claude 桌面版的数据；Claude Code 的登录信息在 `%USERPROFILE%\.claude` 中，不随之切换
- "登录"按钮会让本机所有 Claude Code 会话退出登录
- 断线保护的进程名不要填 `claude.exe`，否则 Claude Code 也会被结束
- 断线保护只在程序运行时有效（可配合开机自启使用）
- 开机自启记录的是程序的完整路径，移动程序后需重新勾选一次

## 项目结构

```
src/
├── main.rs        界面、托盘、各页面的交互
├── accounts.rs    账号快照：切换 / 添加 / 重命名 / 删除，关闭 Claude 桌面版，重新登录
├── netcheck.rs    泄漏自查（HTTP / STUN / IPv6 / DNS）与断线保护
└── autostart.rs   开机自启（注册表 Run 项）
assets/tray.png    托盘与窗口图标
```
