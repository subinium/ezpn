<p align="center">
  <img src="../assets/hero.png" width="720" alt="ezpn 演示">
</p>

# ezpn

一条命令分割终端。点击、拖拽，搞定。

[![License](https://img.shields.io/badge/license-MIT-blue)](../LICENSE)
[![Crate](https://img.shields.io/badge/crates.io-v0.2.0-orange)](https://crates.io/crates/ezpn)
[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey)]()

[English](../README.md) | [한국어](README.ko.md) | [日本語](README.ja.md) | **中文** | [Español](README.es.md) | [Français](README.fr.md)

## 安装

```bash
cargo install ezpn
```

## 用法

```bash
ezpn              # 左右两个面板
ezpn 4            # 4个水平面板
ezpn 3 -d v       # 3个垂直面板
ezpn 2 3          # 2×3 网格
ezpn --layout '7:3/1:1'   # 比例布局
ezpn -e 'make watch' -e 'npm dev'   # 每个面板运行不同命令
```

## 操作

**鼠标：** 点击选择 / `×`关闭 / 拖拽边框调整大小 / 双击缩放切换 / 滚轮滚动

**键盘：**

| 按键 | 操作 |
|---|---|
| `Ctrl+D` | 左右分割 |
| `Ctrl+E` | 上下分割 |
| `Ctrl+N` | 下一个面板 |
| `Ctrl+G` | 设置面板 |
| `Ctrl+W` | 退出 |

**tmux 兼容键（`Ctrl+B` 之后）：**

| 按键 | 操作 |
|---|---|
| `%` | 左右分割 |
| `"` | 上下分割 |
| `o` | 下一个面板 |
| `Arrow` | 方向导航 |
| `x` | 关闭面板 |
| `z` | 缩放切换 |
| `R` | 调整大小模式 |
| `q` | 面板编号 + 按1-9跳转（0为第10个） |
| `{ }` | 交换面板 |
| `?` | 帮助 |
| `[` | 滚动模式（j/k/g/G，q退出） |
| `d` | 退出（有确认提示） |

## 主要功能

- **灵活布局** — 网格、比例指定、自由分割、拖拽调整
- **面板独立命令** — `-e`标志为每个面板指定命令
- **标题栏** — `[━] [┃] [×]` 按钮 + 运行命令显示
- **缩放模式** — `Ctrl+B z` 或双击全屏放大
- **键盘调整大小** — `Ctrl+B R` → 方向键/hjkl调整
- **面板交换** — `Ctrl+B {` / `}` 交换面板位置
- **快速跳转** — `Ctrl+B q` → 显示编号，1-9键跳转
- **tmux 前缀键** — `Ctrl+B` 后使用 tmux 按键
- **配置文件** — `~/.config/ezpn/config.toml` 支持
- **IPC 自动化** — `ezpn-ctl` 外部控制
- **工作区快照** — `ezpn-ctl save/load` 保存和恢复

## 对比

|  | tmux | Zellij | ezpn |
|---|---|---|---|
| 配置 | `.tmux.conf` | KDL文件 | CLI参数 |
| 分割 | `Ctrl+B %` | 模式切换 | `Ctrl+D` / 点击 |
| 分离 | 支持 | 支持 | 不支持 |

需要会话持久化用 tmux/Zellij，只想快速分屏用 ezpn。

## 许可证

[MIT](../LICENSE)
