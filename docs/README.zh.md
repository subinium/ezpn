<p align="center">
  <img src="../assets/hero.png" width="720" alt="ezpn 演示">
</p>

<h1 align="center">ezpn</h1>

<p align="center">
  <strong>终端面板，即刻呈现。</strong><br>
  面向 macOS 和 Linux 的终端复用器，支持鼠标操作、持久会话和熟悉的前缀键。
</p>

<p align="center">
  <a href="https://crates.io/crates/ezpn"><img src="https://img.shields.io/crates/v/ezpn?style=flat-square&color=orange" alt="crates.io"></a>
  <a href="../LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="MIT License"></a>
  <a href="https://github.com/subinium/ezpn/actions"><img src="https://img.shields.io/github/actions/workflow/status/subinium/ezpn/ci.yml?style=flat-square&label=CI" alt="CI"></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey?style=flat-square" alt="Platform">
</p>

<p align="center">
  <a href="../README.md">English</a> | <a href="README.ko.md">한국어</a> | <a href="README.ja.md">日本語</a> | <b>中文</b> | <a href="README.es.md">Español</a> | <a href="README.fr.md">Français</a>
</p>

---

## 开始工作

```sh
cargo install ezpn --locked
ezpn                 # two shells
ezpn 2 3             # a 2-by-3 grid
ezpn -S work         # create or reattach to a named session
```

构建需要 Rust 1.88 或更新版本。[GitHub Releases](https://github.com/subinium/ezpn/releases)
提供 macOS 和 Linux 二进制文件；如果附有校验和，请一并验证。
ezpn 是可执行的终端复用器，不是可嵌入 Rust 应用的 GUI 库。

## 会话与 SSH

```sh
ezpn a work
ezpn a work --shared
ezpn a work --readonly
ezpn ls
ezpn kill work
```

按 `Ctrl+B`，再按 `d`，只会分离当前客户端。Shell 进程仍会继续运行，
包括非活动标签页中的任务。重新连接时，连接的是这些现有进程。
只读客户端不能输入，也不能调整可写客户端的工作区大小。

请在远程主机上安装 ezpn，并确保它位于 PATH 中：

```sh
ssh -t host 'ezpn -S work'
ssh -t host 'ezpn a work'
ssh -J bastion -t host 'ezpn a work'
```

SSH 必须分配 PTY。SSH 客户端断开连接不会结束远程守护进程。
加密、身份认证、主机密钥验证和转发由 OpenSSH 负责。
不要将 ezpn 的本地 Unix 套接字暴露到未经身份验证的网络。

## 鼠标与键盘

| 操作 | 效果 |
| --- | --- |
| 点击窗格内容 | 聚焦该窗格 |
| 拖动分隔线 | 调整分割大小 |
| 标题栏分割按钮 | 分割选中的窗格 |
| 标题栏关闭按钮 | 先确认再关闭 |
| 点击标签页 | 切换标签页 |
| 滚动 | 滚动历史记录，或转发给支持鼠标的应用 |
| 拖动文本 | 选择并复制 |
| Shift + 拖动 | 选择 ezpn 文本，而不是向应用发送鼠标输入 |
| 双击不处理鼠标的应用内容 | 切换缩放 |
| F1 / F2 | 设置 / 均分大小 |
| Alt + 方向键 | 切换窗格；macOS 上需将 Option 配置为 Meta |

向应用发送的点击、移动、滚轮和按钮释放事件使用应用请求的鼠标编码。
除非显式重新绑定，`Ctrl+D`、`Ctrl+E`、`Ctrl+W` 等按键会传给 Shell。
它们不再用于分割窗格或请求退出。

按 `Ctrl+B` 后，可使用以下按键：

| 按键 | 操作 |
| --- | --- |
| `%` / `"` | 按列 / 按行分割 |
| `o` / 方向键 | 切换窗格 |
| `x` | 确认关闭窗格 |
| `z` | 切换缩放 |
| `R` | 调整大小模式 |
| `Space` / `E` | 均分大小 |
| `c` / `n` / `p` | 新建 / 下一个 / 上一个标签页 |
| `0`–`9` | 按从 0 开始的索引选择标签页 |
| `,` / `&` | 重命名 / 确认关闭标签页 |
| `[` | 复制模式 |
| `:` | 命令面板 |
| `r` | 重新加载全局配置 |
| `B` | 切换广播输入 |
| `d` | 分离当前客户端 |
| `?` | 帮助 |
| `Ctrl+B` | 将前缀键发送给应用 |

复制模式支持 vi 移动、`v`/`V` 选择、`y` 或 Enter 复制、
`/`/`?` 搜索、`n`/`N` 跳转到下一个/上一个匹配项，以及 `q`/Escape 退出。
支持部分常用 tmux 按键，并不意味着完全兼容 tmux 命令。

## 保留工作进程的布局调整

```sh
ezpn -l dev       # 7:3
ezpn -l ide       # 7:3/1:1
ezpn -l quad      # 2-by-2
ezpn -l '7:3/5:5'
ezpn -b none
```

命令面板中的 `select-layout` 会重新排列现有进程。
如果布局的窗格数不同，操作会被拒绝；请显式分割或关闭窗格。
分割或加载快照失败时，不会破坏当前工作区。

## 受信任的项目工作区

允许自动启动前，请先检查仓库中的命令：

```toml
# .ezpn.toml
[workspace]
layout = "7:3"

[[pane]]
name = "shell"
cwd = "."

[[pane]]
name = "worker"
command = "printf 'ready\\n'; exec sh"
restart = "on_failure"
```

```sh
ezpn init
ezpn doctor
ezpn --trust-project
```

`--trust-project` 授权自动执行 `.ezpn.toml` / Procfile。
若要启动普通 Shell 而不加载仓库命令，请显式指定网格，例如 `ezpn 1 2`。
`doctor` 以只读方式检查语法，不执行命令，也不解析机密值。

项目环境变量插值支持环境变量、文件和机密值引用。
诊断信息不会输出外部值。配置中读取了外部值的窗格，会从用于执行的快照元数据和历史记录中排除；
恢复时，这些窗格会打开干净的 Shell。
这是有意优先保护隐私，避免在用户不知情时保存已解析的凭据。
详见[配置](../docs/configuration.md)和[安全](../docs/security.md)。

## 配置与恢复

```toml
# ~/.config/ezpn/config.toml
[global]
border = "rounded"
scrollback = 10000
persist_scrollback = false

[keys]
prefix = "b"

[theme]
name = "ezpn-dark"
```

主题：`ezpn-dark`、`ezpn-light`、`nord`、`gruvbox-dark`、`solarized-dark`。
用户按键映射位于 `[keymap.normal]`、`[keymap.prefix]` 和 `[keymap.copy_mode]`。
`Ctrl+B r` 基于一次读取并验证的文件内容，重新加载支持的配置项。
设置面板保存失败时会报告错误，不会声称保存成功。

磁盘快照与分离后仍在运行的会话不同。`ezpn --restore FILE` 会
**启动新进程**。选择保存的历史记录只能恢复为文本，无法恢复运行中的编辑器、
进程内存、终端图形或精确的备用屏幕状态。
快照文件受大小和解压限制，并使用私有访问权限。

## 兼容性与验证依据

- 支持范围是使用 UTF-8 ANSI 终端和 Unix PTY 的 macOS、Linux。
  不支持原生 Windows。
- 子应用的键盘协议协商与宿主终端能力是两回事。传统应用接收传统序列；
  受支持的 Kitty 扩展需显式启用。
- 应用写入剪贴板时受配置的 OSC 52 策略约束。
  通过 SSH 复制时，优先使用连接到会话的终端，而不是远程桌面的剪贴板。
- 渲染范围受限，小视口会裁剪显示。解析器限制、支持的序列及未经测试的
  GUI 终端模拟器组合，见[终端兼容性](../docs/terminal-protocol.md)。
- `--features render-diff` 启用可选且有边界限制的 ANSI 差异输出路径；
  不支持的帧会回退到原始输出。这并不保证在所有场景下提速。
- 真实 PTY 测试覆盖连接/分离、调整大小、共享/只读客户端以及传输中断。
  另有隔离的本机回环 SSH 测试，用于区分真实 SSH 与模拟测试。
- 长时间稳定性测试和与 tmux/Zellij 的性能比较属于独立的验证工作。
  不声称 ezpn 始终比这两个项目更快或更省内存。

[发布审计](../docs/audits/v0.14.0.md)记录结果和剩余限制。
[预检脚本](../scripts/preflight.py)记录 PASS/FAIL/SKIP 和真实退出码；
不会把失败的测试藏在被忽略的占位测试后面。

## 文档

[入门](../docs/getting-started.md) · [配置](../docs/configuration.md) ·
[SSH 与终端协议](../docs/terminal-protocol.md) · [剪贴板](../docs/clipboard.md) ·
[安全](../docs/security.md) · [脚本功能限制](../docs/scripting.md) ·
[参与贡献](../CONTRIBUTING.md) · [更新日志](../CHANGELOG.md)

## 许可证

[MIT](../LICENSE)
