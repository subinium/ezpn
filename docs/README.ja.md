<p align="center">
  <img src="../assets/hero.png" width="720" alt="ezpn デモ">
</p>

<h1 align="center">ezpn</h1>

<p align="center">
  <strong>ターミナルペイン、瞬時に。</strong><br>
  ゼロ設定でセッション永続化とtmux互換キーを提供するターミナルマルチプレクサ。
</p>

<p align="center">
  <a href="https://crates.io/crates/ezpn"><img src="https://img.shields.io/crates/v/ezpn?style=flat-square&color=orange" alt="crates.io"></a>
  <a href="../LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="MIT License"></a>
  <a href="https://github.com/subinium/ezpn/actions"><img src="https://img.shields.io/github/actions/workflow/status/subinium/ezpn/ci.yml?style=flat-square&label=CI" alt="CI"></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey?style=flat-square" alt="Platform">
</p>

<p align="center">
  <a href="../README.md">English</a> | <a href="README.ko.md">한국어</a> | <b>日本語</b> | <a href="README.zh.md">中文</a> | <a href="README.es.md">Español</a> | <a href="README.fr.md">Français</a>
</p>

---

## なぜ ezpn？

```bash
$ ezpn                # ターミナルを即座に分割
$ ezpn 2 3            # 2x3 シェルグリッド
$ ezpn -l dev         # プリセットレイアウト
```

設定ファイルも、セットアップも、学習コストもなし。セッションはバックグラウンドで永続化 — `Ctrl+B d` でデタッチ、`ezpn a` で復帰。

**プロジェクトで**、`.ezpn.toml` をリポジトリに入れて `ezpn` を実行 — 全員が同じワークスペースを使えます：

```toml
[workspace]
layout = "7:3/1:1"

[[pane]]
name = "editor"
command = "nvim ."

[[pane]]
name = "server"
command = "npm run dev"
restart = "on_failure"

[[pane]]
name = "tests"
command = "npm test -- --watch"

[[pane]]
name = "logs"
command = "tail -f logs/app.log"
```

```bash
$ ezpn   # .ezpn.toml を読んですべて起動
```

tmuxinatorも不要。YAMLも不要。リポジトリにTOMLファイル1つだけ。

## インストール

```bash
cargo install ezpn
```

<details>
<summary>ソースからビルド</summary>

```bash
git clone https://github.com/subinium/ezpn
cd ezpn && cargo install --path .
```

</details>

## クイックスタート

```bash
ezpn                  # 2ペイン（または .ezpn.toml を読み込み）
ezpn 2 3              # 2x3 グリッド
ezpn -l dev           # レイアウトプリセット (dev, monitor, quad, stack, trio...)
ezpn -e 'cmd1' -e 'cmd2'   # ペインごとのコマンド
```

### セッション

```bash
Ctrl+B d               # デタッチ（セッションは実行継続）
ezpn a                 # 最新のセッションに再接続
ezpn a myproject       # 名前で再接続
ezpn ls                # アクティブなセッション一覧
ezpn kill myproject    # セッションを終了
```

### タブ

```bash
Ctrl+B c               # 新しいタブ
Ctrl+B n / p           # 次 / 前のタブ
Ctrl+B 0-9             # 番号でタブに移動
```

すべてのtmuxキーが動作します — `Ctrl+B %` で分割、`Ctrl+B x` で閉じる、`Ctrl+B [` でコピーモード。

## 主な機能

| | |
|---|---|
| **ゼロ設定** | そのまま使える。rcファイル不要。 |
| **レイアウトプリセット** | `dev`, `ide`, `monitor`, `quad`, `stack`, `main`, `trio` |
| **セッション永続化** | tmuxのようにデタッチ/アタッチ。バックグラウンドデーモンがプロセスを維持。 |
| **タブ** | tmuxスタイルのウィンドウ。タブバーとマウスクリック切り替え対応。 |
| **マウスファースト** | クリックでフォーカス、ドラッグでリサイズ、スクロールで履歴、ドラッグで選択&コピー。 |
| **コピーモード** | Viキー、ビジュアル選択、インクリメンタル検索、OSC 52クリップボード。 |
| **コマンドパレット** | `Ctrl+B :` tmux互換コマンド。 |
| **ブロードキャストモード** | 全ペインに同時入力。 |
| **プロジェクト設定** | `.ezpn.toml` — レイアウト、コマンド、環境変数、自動再起動。 |
| **ボーダーレスモード** | `ezpn -b none` で画面スペースを最大化。 |
| **Kittyキーボード** | `Shift+Enter`、`Ctrl+Arrow`などの修飾キーが正常動作。 |
| **CJK/Unicode** | 日本語、中国語、韓国語、絵文字の正確な幅計算。 |

## レイアウトプリセット

```bash
ezpn -l dev       # 7:3 — メイン + サイド
ezpn -l ide       # 7:3/1:1 — エディタ + サイドバー + 下部2つ
ezpn -l monitor   # 1:1:1 — 3列均等
ezpn -l quad      # 2x2 グリッド
ezpn -l stack     # 1/1/1 — 3行スタック
ezpn -l main      # 6:4/1 — 上部ワイドペア + 下部フル
ezpn -l trio      # 1/1:1 — 上部フル + 下部2つ
```

カスタム比率: `ezpn -l '7:3/5:5'`

## プロジェクト設定

プロジェクトルートに `.ezpn.toml` を配置して `ezpn` を実行。以上。

**ペインごとのオプション:** `command`, `cwd`, `name`, `env`, `restart` (`never`/`on_failure`/`always`), `shell`

```bash
ezpn init              # .ezpn.toml テンプレート生成
ezpn from Procfile     # Procfileからインポート
```

<details>
<summary>グローバル設定</summary>

`~/.config/ezpn/config.toml`:

```toml
border = rounded        # single | rounded | heavy | double | none
shell = /bin/zsh
scrollback = 10000
status_bar = true
tab_bar = true
prefix = b              # プリフィクスキー (Ctrl+<key>)
```

</details>

## キーバインド

**直接ショートカット:**

| キー | 操作 |
|---|---|
| `Ctrl+D` | 左右分割 |
| `Ctrl+E` | 上下分割 |
| `Ctrl+N` | 次のペイン |
| `F2` | サイズ均等化 |

**プリフィクスモード** (`Ctrl+B` の後):

| キー | 操作 |
|---|---|
| `%` / `"` | 左右 / 上下分割 |
| `o` / Arrow | ペイン移動 |
| `x` | ペインを閉じる |
| `z` | ズーム切替 |
| `R` | リサイズモード |
| `[` | コピーモード |
| `B` | ブロードキャスト |
| `:` | コマンドパレット |
| `d` | デタッチ |
| `?` | ヘルプ |

<details>
<summary>全キーバインド一覧</summary>

**タブ:**

| キー | 操作 |
|---|---|
| `Ctrl+B c` | 新しいタブ |
| `Ctrl+B n` / `p` | 次 / 前のタブ |
| `Ctrl+B 0-9` | 番号でタブに移動 |
| `Ctrl+B ,` | タブ名変更 |
| `Ctrl+B &` | タブを閉じる |

**ペイン:**

| キー | 操作 |
|---|---|
| `Ctrl+B {` / `}` | 前 / 次のペインと交換 |
| `Ctrl+B E` / `Space` | サイズ均等化 |
| `Ctrl+B s` | ステータスバー切替 |
| `Ctrl+B q` | ペイン番号 + クイックジャンプ |

**コピーモード** (`Ctrl+B [`):

| キー | 操作 |
|---|---|
| `h` `j` `k` `l` | カーソル移動 |
| `w` / `b` | 次 / 前の単語 |
| `0` / `$` / `^` | 行頭 / 行末 / 最初の非空白文字 |
| `g` / `G` | スクロールバック先頭 / 末尾 |
| `Ctrl+U` / `Ctrl+D` | 半ページ上 / 下 |
| `v` | 文字選択 |
| `V` | 行選択 |
| `y` / `Enter` | コピーして終了 |
| `/` / `?` | 前方 / 後方検索 |
| `n` / `N` | 次 / 前のマッチ |
| `q` / `Esc` | 終了 |

**マウス:**

| 操作 | 効果 |
|---|---|
| ペインクリック | フォーカス |
| ダブルクリック | ズーム切替 |
| タブクリック | タブ切替 |
| `[x]` クリック | ペインを閉じる |
| ボーダードラッグ | リサイズ |
| テキストドラッグ | 選択 + コピー |
| スクロールホイール | スクロールバック履歴 |

**macOS注意:** Alt+Arrowの方向移動にはOptionをMetaに設定する必要があります（iTerm2: Preferences > Profiles > Keys > `Esc+`）。

</details>

<details>
<summary>コマンドパレットのコマンド</summary>

`Ctrl+B :` でコマンドプロンプトを開きます。tmuxエイリアスすべて対応。

```
split / split-window         左右分割
split -v                     上下分割
new-tab / new-window         新しいタブ
next-tab / prev-tab          タブ切替
close-pane / kill-pane       ペインを閉じる
close-tab / kill-window      タブを閉じる
rename-tab <name>            タブ名変更
layout <spec>                レイアウト変更
equalize / even              サイズ均等化
zoom                         ズーム切替
broadcast                    ブロードキャスト切替
```

</details>

## なぜ ezpn か（vs. tmux）

3つの測定可能な主張。自分のワークロードで検証してから信頼してください。

| 軸 | tmux 3.4 | **ezpn 0.12** | 測定方法 |
| --- | --- | --- | --- |
| アイドル時 RSS (16 ペイン、50 MB スクロールバック合計、Linux 6.6) | ~180 MB | **~28 MB** | 16 分割後 1 分アイドルで `ps -o rss= -p $(pgrep -d, tmux\|ezpn)`。 |
| `send-keys` の信頼性 | fire-and-forget; 終了シグナルなし | **`--await-prompt` で OSC 133 D までブロック** | `ezpn-ctl send-keys --await-prompt --timeout 60s -- 'cargo test\n'` — [scripting.md](scripting.md) 参照。 |
| DECSET 2026 (同期出力) | ホストエミュレータへパススルー | **インターセプト + バッファ**; クライアントへ単一原子フレーム | 2 クライアント同時接続中に `printf '\e[?2026h…\e[?2026l'` — 両方が同じ原子的再描画を見る。 |

数値以外：

- **ゼロ設定のデフォルト。** 新規インストールで全ての tmux キーが動作。`.tmux.conf` 不要、プラグインマネージャ不要。
- **TOML、YAML サテライトではない。** `.ezpn.toml` はリポジトリに置かれ、`gem install tmuxinator` なしで全員が同じワークスペースを共有。
- **OSC 52 ペースト注入ガード。** `cat hostile.log` がクリップボードを暗黙に上書きできない（[clipboard.md](clipboard.md)、[security.md](security.md)）。
- **凍結されたワイヤープロトコル。** [`docs/protocol/v1.md`](protocol/v1.md) が IPC 表面に SemVer をコミット — マイナーバンプでスクリプトが壊れない。

切り替え前に考慮すべきトレードオフ：

- プラグインシステムなし。tmux のプラグインエコシステムは 10 年以上、ezpn は空。
- `pipe-pane`、`command-alias`、`if-shell` なし。代わりに `[[hooks]]` とイベントバスを使用。
- Linux と macOS のみ。Windows 非対応。

フル移行ガイド：[docs/migration-from-tmux.md](migration-from-tmux.md)。

## ezpn vs. tmux vs. Zellij

| | tmux | Zellij | **ezpn** |
|---|---|---|---|
| 設定 | `.tmux.conf` 必要 | KDL設定 | **ゼロ設定** |
| 初回使用 | 空の画面 | チュートリアルモード | **`ezpn`** |
| セッション | `tmux a` | `zellij a` | **`ezpn a`** |
| プロジェクト設定 | tmuxinator (gem) | — | **`.ezpn.toml` 内蔵** |
| ブロードキャスト | `:setw synchronize-panes` | — | **`Ctrl+B B`** |
| 自動再起動 | — | — | **`restart = "always"`** |
| Kittyキーボード | 非対応 | 対応 | **対応** |
| プラグイン | — | WASM | — |
| エコシステム | 巨大（30年） | 成長中 | 新規 |

**ezpn** — 設定不要で即使えるターミナル分割。
**tmux** — 深いスクリプティングとプラグインエコシステムが必要な場合。
**Zellij** — モダンUIとWASMプラグインが欲しい場合。

## CLIリファレンス

```
ezpn [ROWS COLS]         グリッドレイアウトで開始
ezpn -l <PRESET>         レイアウトプリセットで開始
ezpn -e <CMD> [-e ...]   ペインごとのコマンド
ezpn -S <NAME>           名前付きセッション
ezpn -b <STYLE>          ボーダースタイル (single/rounded/heavy/double/none)
ezpn a [NAME]            セッションに接続
ezpn ls                  セッション一覧
ezpn kill [NAME]         セッション終了
ezpn rename OLD NEW      セッション名変更
ezpn init                .ezpn.toml テンプレート生成
ezpn from <FILE>         Procfileからインポート
```

## ドキュメント

- [はじめに](getting-started.md) — 5分ツアー
- [tmux からの移行](migration-from-tmux.md) — キー別、コマンド別
- [設定](configuration.md) — `config.toml` + `.ezpn.toml` フルリファレンス
- [スクリプティング](scripting.md) — `ezpn-ctl`、イベント、`ls --json`
- [クリップボード](clipboard.md) — OSC 52、フォールバックチェーン、SSH 落とし穴
- [ターミナルプロトコル](terminal-protocol.md) — ezpn がパススルー / インターセプト / 改変するもの
- [セキュリティ](security.md) — 脅威モデルとデフォルト
- [IPC ワイヤープロトコル v1](protocol/v1.md) — v1.0 で凍結

## ライセンス

[MIT](../LICENSE)
