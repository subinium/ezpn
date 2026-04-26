<p align="center">
  <img src="../assets/hero.png" width="720" alt="ezpn 演示">
</p>

<h1 align="center">ezpn</h1>

<p align="center">
  <strong>终端面板，即刻呈现。</strong><br>
  零配置终端复用器，支持会话持久化和 tmux 兼容按键。
</p>

<p align="center">
  <a href="https://crates.io/crates/ezpn"><img src="https://img.shields.io/crates/v/ezpn?style=flat-square&color=orange" alt="crates.io"></a>
  <a href="../LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="MIT License"></a>
  <a href="https://github.com/subinium/ezpn/actions"><img src="https://img.shields.io/github/actions/workflow/status/subinium/ezpn/ci.yml?style=flat-square&label=CI" alt="CI"></a>
  <a href="https://github.com/subinium/ezpn/actions/workflows/gitleaks.yml"><img src="https://img.shields.io/github/actions/workflow/status/subinium/ezpn/gitleaks.yml?style=flat-square&label=gitleaks" alt="gitleaks"></a>
  <a href="https://github.com/subinium/ezpn/actions/workflows/supply-chain.yml"><img src="https://img.shields.io/github/actions/workflow/status/subinium/ezpn/supply-chain.yml?style=flat-square&label=audit" alt="audit"></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey?style=flat-square" alt="Platform">
</p>

<p align="center">
  <a href="../README.md">English</a> | <a href="README.ko.md">한국어</a> | <a href="README.ja.md">日本語</a> | <b>中文</b> | <a href="README.es.md">Español</a> | <a href="README.fr.md">Français</a>
</p>

---

## 为什么选择 ezpn？

```bash
$ ezpn                # 终端即刻分屏
$ ezpn 2 3            # 2x3 Shell 网格
$ ezpn -l dev         # 预设布局
```

无需配置文件，无需设置，无需学习成本。会话在后台持久运行 — `Ctrl+B d` 分离，`ezpn a` 回来。

**在项目中**，将 `.ezpn.toml` 放入仓库并运行 `ezpn` — 所有人共享相同的工作空间：

```toml
[session]
name = "myproject"           # 固定会话名（冲突时变为 myproject-1、-2...）

[workspace]
layout = "7:3/1:1"
persist_scrollback = true    # 滚动缓冲区在分离/重连后仍然保留

[[pane]]
name = "editor"
command = "nvim ."

[[pane]]
name = "server"
command = "npm run dev"
restart = "on_failure"
env = { NODE_ENV = "${env:NODE_ENV}", DB_URL = "${file:.env.local}" }

[[pane]]
name = "tests"
command = "npm test -- --watch"

[[pane]]
name = "logs"
command = "tail -f logs/app.log"
```

```bash
$ ezpn         # 读取 .ezpn.toml，启动一切
$ ezpn doctor  # 在运行前校验环境变量插值与密钥引用
```

不需要 tmuxinator。不需要 YAML。仓库里放一个 TOML 文件就行。

## 安装

```bash
cargo install ezpn
```

也可以从 [最新 release](https://github.com/subinium/ezpn/releases/latest) 下载预编译二进制 — `ezpn-x86_64-unknown-linux-gnu.tar.gz`、`ezpn-x86_64-apple-darwin.tar.gz` 或 `ezpn-aarch64-apple-darwin.tar.gz`。

<details>
<summary>从源码构建</summary>

```bash
git clone https://github.com/subinium/ezpn
cd ezpn && cargo install --path .
```

</details>

## 快速开始

```bash
ezpn                  # 2 个面板（或加载 .ezpn.toml）
ezpn 2 3              # 2x3 网格
ezpn -l dev           # 布局预设 (dev, monitor, quad, stack, trio...)
ezpn -e 'cmd1' -e 'cmd2'   # 每个面板的命令
```

### 会话

```bash
Ctrl+B d               # 分离（会话继续运行）
ezpn a                 # 重新连接最近的会话
ezpn a myproject       # 按名称重新连接
ezpn ls                # 列出活动会话
ezpn kill myproject    # 终止会话
ezpn --new             # 即使 $PWD 已有会话也强制创建新会话
```

会话名默认为 `basename($PWD)`。冲突按确定性规则解析 — `repo` → `repo-1` → `repo-2`（扫描时会回收已失效的 socket）。可在 `.ezpn.toml` 中通过 `[session].name = "..."` 固定名称。

### 标签页

```bash
Ctrl+B c               # 新标签页
Ctrl+B n / p           # 下一个 / 上一个标签页
Ctrl+B 0-9             # 按编号跳转标签页
```

所有 tmux 按键都可用 — `Ctrl+B %` 分割，`Ctrl+B x` 关闭，`Ctrl+B [` 进入复制模式。

## 主要功能

| | |
|---|---|
| **零配置** | 开箱即用，无需 rc 文件。 |
| **布局预设** | `dev`、`ide`、`monitor`、`quad`、`stack`、`main`、`trio` |
| **会话持久化** | 像 tmux 一样分离/连接。后台守护进程保持进程运行。冷连接低于 50 ms。 |
| **滚动缓冲区持久化** | 选项 `persist_scrollback` 让缓冲区在分离/重连后仍然保留（v3 快照采用 gzip+bincode）。 |
| **标签页** | tmux 风格窗口，支持标签栏和鼠标点击切换。 |
| **鼠标优先** | 点击聚焦、拖拽调整大小、滚轮浏览历史、拖拽选择和复制。 |
| **复制模式** | Vi 按键、可视选择、按显示宽度的增量搜索、OSC 52 剪贴板。 |
| **命令面板** | `Ctrl+B :` tmux 兼容命令。 |
| **广播模式** | 同时向所有面板输入。 |
| **项目配置** | 每个项目一个 `.ezpn.toml` — 布局、命令、环境变量、自动重启。 |
| **环境变量插值** | 在面板 env 中使用 `${HOME}`、`${env:VAR}`、`${file:.env.local}`、`${secret:keychain:KEY}`。 |
| **主题** | TOML 调色板 + 4 套内置主题（`tokyo-night`、`gruvbox-dark`、`solarized-dark`/`-light`）。 |
| **热重载** | `Ctrl+B r` 不分离即可重新加载 `~/.config/ezpn/config.toml`。 |
| **无边框模式** | `ezpn -b none` 最大化屏幕空间。 |
| **Kitty 键盘** | `Shift+Enter`、`Ctrl+Arrow`、Alt+Char（CSI u / RFC 3665）— 修饰键正确工作。 |
| **CJK/Unicode** | 中文、日文、韩文和 emoji 的精确宽度计算。 |
| **崩溃隔离** | 单个面板崩溃不会拖垮守护进程（信号安全的 SIGTERM/SIGCHLD 处理）。 |

## 布局预设

```bash
ezpn -l dev       # 7:3 — 主区 + 侧边
ezpn -l ide       # 7:3/1:1 — 编辑器 + 侧边栏 + 底部两个
ezpn -l monitor   # 1:1:1 — 三列均等
ezpn -l quad      # 2x2 网格
ezpn -l stack     # 1/1/1 — 三行堆叠
ezpn -l main      # 6:4/1 — 上方宽对 + 下方全宽
ezpn -l trio      # 1/1:1 — 上方全宽 + 下方两个
```

自定义比例：`ezpn -l '7:3/5:5'`

## 项目配置

在项目根目录放置 `.ezpn.toml` 然后运行 `ezpn`。完毕。

**面板选项：** `command`、`cwd`、`name`、`env`、`restart`（`never`/`on_failure`/`always`）、`shell`

```bash
ezpn init              # 生成 .ezpn.toml 模板
ezpn from Procfile     # 从 Procfile 导入
ezpn doctor            # 校验配置 + 环境变量插值，缺失引用时以非零退出
```

### 环境变量插值

面板 env 值支持四种引用形式：

```toml
[[pane]]
command = "npm run dev"
env = {
  HOME       = "${HOME}",                    # 进程环境变量
  NODE_ENV   = "${env:NODE_ENV}",            # 显式指定环境变量
  DB_URL     = "${file:.env.local}",         # dotenv 风格的文件查找
  GH_TOKEN   = "${secret:keychain:GH_TOKEN}",# macOS Keychain（Linux：secret-tool）
}
```

与 `.ezpn.toml` 同目录的 `.env.local` 会被自动合并并覆盖 `[env]`。当系统钥匙串不可用时，`${secret:keychain:KEY}` 会回退到 `${env:KEY}` 并发出警告。递归深度上限为 8，用于检测循环引用。

### 主题

```toml
# .ezpn.toml 或 ~/.config/ezpn/config.toml
theme = "tokyo-night"   # default | tokyo-night | gruvbox-dark | solarized-dark | solarized-light
```

用户主题从 `~/.config/ezpn/themes/<name>.toml` 加载。ezpn 会自动检测 `$COLORTERM` / `$TERM`，在不支持真彩色时降级为 256 色或 16 色。

<details>
<summary>全局配置 (~/.config/ezpn/config.toml)</summary>

```toml
border = rounded            # single | rounded | heavy | double | none
shell = /bin/zsh
scrollback = 10000
status_bar = true
tab_bar = true
prefix = b                  # 前缀键 (Ctrl+<key>)
theme = default             # default | tokyo-night | gruvbox-dark | solarized-dark | solarized-light
persist_scrollback = false  # 是否将滚动缓冲区写入自动快照（默认关闭）
```

设置面板（`Ctrl+B Shift+,`）的更改会以原子方式持久化。使用 `Ctrl+B r` 从磁盘重新加载。

</details>

## 键绑定

**直接快捷键：**

| 按键 | 操作 |
|---|---|
| `Ctrl+D` | 左右分割 |
| `Ctrl+E` | 上下分割 |
| `Ctrl+N` | 下一个面板 |
| `F2` | 均等化大小 |

**前缀模式**（`Ctrl+B` 之后）：

| 按键 | 操作 |
|---|---|
| `%` / `"` | 左右 / 上下分割 |
| `o` / Arrow | 面板导航 |
| `x` | 关闭面板 |
| `z` | 缩放切换 |
| `R` | 调整大小模式 |
| `[` | 复制模式 |
| `B` | 广播 |
| `:` | 命令面板 |
| `r` | 重新加载配置 |
| `d` | 分离 |
| `?` | 帮助 |

<details>
<summary>完整键绑定参考</summary>

**标签页：**

| 按键 | 操作 |
|---|---|
| `Ctrl+B c` | 新标签页 |
| `Ctrl+B n` / `p` | 下一个 / 上一个标签页 |
| `Ctrl+B 0-9` | 按编号跳转标签页 |
| `Ctrl+B ,` | 重命名标签页 |
| `Ctrl+B &` | 关闭标签页 |

**面板：**

| 按键 | 操作 |
|---|---|
| `Ctrl+B {` / `}` | 与前 / 后面板交换 |
| `Ctrl+B E` / `Space` | 均等化 |
| `Ctrl+B s` | 切换状态栏 |
| `Ctrl+B q` | 面板编号 + 快速跳转 |

**复制模式**（`Ctrl+B [`）：

| 按键 | 操作 |
|---|---|
| `h` `j` `k` `l` | 移动光标 |
| `w` / `b` | 下一个 / 上一个单词 |
| `0` / `$` / `^` | 行首 / 行尾 / 第一个非空白字符 |
| `g` / `G` | 滚动缓冲区顶部 / 底部 |
| `Ctrl+U` / `Ctrl+D` | 半页上 / 下 |
| `v` | 字符选择 |
| `V` | 行选择 |
| `y` / `Enter` | 复制并退出 |
| `/` / `?` | 向前 / 向后搜索 |
| `n` / `N` | 下一个 / 上一个匹配 |
| `q` / `Esc` | 退出 |

**鼠标：**

| 操作 | 效果 |
|---|---|
| 点击面板 | 聚焦 |
| 双击 | 缩放切换 |
| 点击标签 | 切换标签页 |
| 点击 `[x]` | 关闭面板 |
| 拖拽边框 | 调整大小 |
| 拖拽文本 | 选择 + 复制 |
| 滚轮 | 滚动缓冲区历史 |

**macOS 注意：** Alt+Arrow 方向导航需要将 Option 设置为 Meta（iTerm2：Preferences > Profiles > Keys > `Esc+`）。

</details>

<details>
<summary>命令面板命令</summary>

`Ctrl+B :` 打开命令提示符。支持所有 tmux 别名。

```
split / split-window         左右分割
split -v                     上下分割
new-tab / new-window         新标签页
next-tab / prev-tab          切换标签页
close-pane / kill-pane       关闭面板
close-tab / kill-window      关闭标签页
rename-tab <name>            重命名标签页
layout <spec>                更改布局
equalize / even              均等化大小
zoom                         缩放切换
broadcast                    广播切换
```

</details>

## ezpn vs. tmux vs. Zellij

| | tmux | Zellij | **ezpn** |
|---|---|---|---|
| 配置 | 需要 `.tmux.conf` | KDL 配置 | **零配置** |
| 首次使用 | 空白屏幕 | 教程模式 | **`ezpn`** |
| 会话 | `tmux a` | `zellij a` | **`ezpn a`** |
| 项目配置 | tmuxinator (gem) | — | **`.ezpn.toml` 内置** |
| 广播 | `:setw synchronize-panes` | — | **`Ctrl+B B`** |
| 自动重启 | — | — | **`restart = "always"`** |
| Kitty 键盘 | 不支持 | 支持 | **支持** |
| 插件 | — | WASM | — |
| 生态系统 | 庞大（30年） | 成长中 | 新兴 |

**ezpn** — 零配置即用的终端分屏。
**tmux** — 需要深度脚本和插件生态系统时。
**Zellij** — 想要现代 UI 和 WASM 插件时。

## CLI 参考

```
ezpn [ROWS COLS]         以网格布局启动
ezpn -l <PRESET>         以布局预设启动
ezpn -e <CMD> [-e ...]   每个面板的命令
ezpn -S <NAME>           命名会话
ezpn -b <STYLE>          边框样式 (single/rounded/heavy/double/none)
ezpn --new               强制创建新会话（跳过对已有会话的自动连接）
ezpn a [NAME]            连接会话
ezpn ls                  列出会话
ezpn kill [NAME]         终止会话
ezpn rename OLD NEW      重命名会话
ezpn init                生成 .ezpn.toml 模板
ezpn from <FILE>         从 Procfile 导入
ezpn doctor              校验 .ezpn.toml + 环境变量插值
```

## 许可证

[MIT](../LICENSE)
