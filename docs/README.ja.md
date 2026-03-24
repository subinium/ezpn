<p align="center">
  <img src="../assets/hero.png" width="720" alt="ezpn デモ">
</p>

# ezpn

コマンド一つでターミナルを分割。クリック、ドラッグ、完了。

[![License](https://img.shields.io/badge/license-MIT-blue)](../LICENSE)
[![Crate](https://img.shields.io/badge/crates.io-v0.2.0-orange)](https://crates.io/crates/ezpn)
[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey)]()

[English](../README.md) | [한국어](README.ko.md) | **日本語** | [中文](README.zh.md) | [Español](README.es.md) | [Français](README.fr.md)

## インストール

```bash
cargo install ezpn
```

## 使い方

```bash
ezpn              # 2ペイン（横並び）
ezpn 4            # 4ペイン（横方向）
ezpn 3 -d v       # 3ペイン（縦方向）
ezpn 2 3          # 2×3グリッド
ezpn --layout '7:3/1:1'   # 比率指定レイアウト
ezpn -e 'make watch' -e 'npm dev'   # ペインごとのコマンド
```

## 操作方法

**マウス:** クリックで選択 / `×`で閉じる / ボーダーをドラッグでリサイズ / ダブルクリックでズーム切替 / スクロール対応

**キーボード:**

| キー | 操作 |
|---|---|
| `Ctrl+D` | 左右分割 |
| `Ctrl+E` | 上下分割 |
| `Ctrl+N` | 次のペイン |
| `Ctrl+G` | 設定パネル |
| `Ctrl+W` | 終了 |

**tmux互換キー (`Ctrl+B` の後):**

| キー | 操作 |
|---|---|
| `%` | 左右分割 |
| `"` | 上下分割 |
| `o` | 次のペイン |
| `Arrow` | 方向移動 |
| `x` | ペインを閉じる |
| `z` | ズーム切替 |
| `R` | リサイズモード |
| `q` | ペイン番号表示＋1-9でジャンプ（0は10番目） |
| `{ }` | ペイン入替 |
| `?` | ヘルプ |
| `[` | スクロールモード (j/k/g/G、qで終了) |
| `d` | 終了（確認あり） |

## 主な機能

- **自由なレイアウト** — グリッド、比率指定、個別分割、ドラッグリサイズ
- **ペインごとのコマンド** — `-e`フラグで個別コマンド起動
- **タイトルバー** — `[━] [┃] [×]` ボタン + 実行中コマンド表示
- **ズームモード** — `Ctrl+B z` またはダブルクリックで全画面
- **キーボードリサイズ** — `Ctrl+B R` → 矢印キー/hjklでサイズ調整
- **ペイン入替** — `Ctrl+B {` / `}` でペイン位置を交換
- **クイックジャンプ** — `Ctrl+B q` → 番号表示、1-9キーでジャンプ
- **tmuxプリフィクスキー** — `Ctrl+B`の後にtmuxキーが使用可能
- **設定ファイル** — `~/.config/ezpn/config.toml` 対応
- **IPC自動化** — `ezpn-ctl`による外部制御
- **ワークスペースの保存・復元** — `ezpn-ctl save/load`

## 比較

|  | tmux | Zellij | ezpn |
|---|---|---|---|
| 設定 | `.tmux.conf` | KDLファイル | CLIフラグ |
| 分割 | `Ctrl+B %` | モード切替 | `Ctrl+D` / クリック |
| Detach | 可能 | 可能 | 不可 |

## ライセンス

[MIT](../LICENSE)
