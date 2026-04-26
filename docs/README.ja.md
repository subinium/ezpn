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
  <a href="https://github.com/subinium/ezpn/actions/workflows/gitleaks.yml"><img src="https://img.shields.io/github/actions/workflow/status/subinium/ezpn/gitleaks.yml?style=flat-square&label=gitleaks" alt="gitleaks"></a>
  <a href="https://github.com/subinium/ezpn/actions/workflows/supply-chain.yml"><img src="https://img.shields.io/github/actions/workflow/status/subinium/ezpn/supply-chain.yml?style=flat-square&label=audit" alt="audit"></a>
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
[session]
name = "myproject"           # セッション名を固定（衝突時は myproject-1, -2... となる）

[workspace]
layout = "7:3/1:1"
persist_scrollback = true    # スクロールバックがデタッチ/再アタッチ後も残る

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
$ ezpn         # .ezpn.toml を読んですべて起動
$ ezpn doctor  # 実行前に環境変数の補間とシークレット参照を検証
```

tmuxinatorも不要。YAMLも不要。リポジトリにTOMLファイル1つだけ。

## インストール

```bash
cargo install ezpn
```

[最新リリース](https://github.com/subinium/ezpn/releases/latest) からビルド済みバイナリを入手することもできます — `ezpn-x86_64-unknown-linux-gnu.tar.gz`、`ezpn-x86_64-apple-darwin.tar.gz`、または `ezpn-aarch64-apple-darwin.tar.gz`。

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
ezpn --new             # $PWD のセッションが既にあっても強制的に新規作成
```

セッション名はデフォルトで `basename($PWD)`。衝突は決定的に解決されます — `repo` → `repo-1` → `repo-2`（スキャン中に死んだソケットは回収されます）。`.ezpn.toml` の `[session].name = "..."` で名前を固定できます。

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
| **セッション永続化** | tmuxのようにデタッチ/アタッチ。バックグラウンドデーモンがプロセスを維持。コールドアタッチ50ms未満。 |
| **スクロールバック永続化** | `persist_scrollback` をオプトインするとデタッチ/再アタッチ後も残る（v3スナップショットでgzip+bincode）。 |
| **タブ** | tmuxスタイルのウィンドウ。タブバーとマウスクリック切り替え対応。 |
| **マウスファースト** | クリックでフォーカス、ドラッグでリサイズ、スクロールで履歴、ドラッグで選択&コピー。 |
| **コピーモード** | Viキー、ビジュアル選択、表示幅対応のインクリメンタル検索、OSC 52クリップボード。 |
| **コマンドパレット** | `Ctrl+B :` tmux互換コマンド。 |
| **ブロードキャストモード** | 全ペインに同時入力。 |
| **プロジェクト設定** | `.ezpn.toml` — レイアウト、コマンド、環境変数、自動再起動。 |
| **環境変数の補間** | ペインの env で `${HOME}`、`${env:VAR}`、`${file:.env.local}`、`${secret:keychain:KEY}` をサポート。 |
| **テーマ** | TOMLパレット + 4種類の組み込み（`tokyo-night`、`gruvbox-dark`、`solarized-dark`/`-light`）。 |
| **ホットリロード** | `Ctrl+B r` でデタッチせずに `~/.config/ezpn/config.toml` を再読み込み。 |
| **ボーダーレスモード** | `ezpn -b none` で画面スペースを最大化。 |
| **Kittyキーボード** | `Shift+Enter`、`Ctrl+Arrow`、Alt+Char（CSI u / RFC 3665）— 修飾キーが正常動作。 |
| **CJK/Unicode** | 日本語、中国語、韓国語、絵文字の正確な幅計算。 |
| **クラッシュ分離** | パニックを起こしたペイン1つでデーモン全体は落ちない（シグナルセーフな SIGTERM/SIGCHLD 処理）。 |
| **スクリプト可能な入力** | `ezpn-ctl send-keys --pane N -- 'cmd' Enter` — エディタ、AIエージェント、CIスクリプト向け。 |
| **イベントストリーム** | バイナリプロトコル上の長寿命 `S_EVENT` サブスクリプション（`-CC` スタイル統合）。 |
| **フック** | 宣言的 `[[hooks]]` 設定：デーモンイベントごとにシェル実行、ワーカープール + フックごとのタイムアウト。 |
| **正規表現検索** | `[copy_mode] search = "regex"` で copy モード検索を POSIX パターン + smart-case に切り替え。 |
| **ペインごとの履歴** | `ezpn-ctl clear-history --pane N` / `set-scrollback --pane N --lines L` で実行時制御。 |

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
ezpn doctor            # 設定 + 環境変数の補間を検証、参照が欠けていれば非ゼロで終了
```

### フック

デーモンイベントごとにシェルコマンドを実行。4スレッドのワーカープール、フックごとの `timeout_ms` あり。各子プロセスは独自のプロセスグループで spawn されるので、SIGTERM → SIGKILL のエスカレーションがツリー全体に届きます。

```toml
# ~/.config/ezpn/config.toml または .ezpn.toml

[[hooks]]
event = "client-attached"
command = "notify-send 'pane {client_id} attached'"
shell = true
timeout_ms = 2000

[[hooks]]
event = "tab-created"
command = ["/usr/local/bin/ezpn-tab-init", "{name}", "{tab_index}"]
```

v0.11 では `client-attached`、`client-detached`、`tab-created`、`tab-closed`、`session-renamed` の 5 イベントを配線。変数展開（`{session}`、`{client_id}`、`{pane_id}`、…）は exec 前に `command` 文字列へイベントごとの値を差し込みます。

### 環境変数の補間

ペインの env 値は4種類の参照形式をサポートします：

```toml
[[pane]]
command = "npm run dev"
env = {
  HOME       = "${HOME}",                    # プロセス環境変数
  NODE_ENV   = "${env:NODE_ENV}",            # 明示的な env 参照
  DB_URL     = "${file:.env.local}",         # dotenv 形式のファイル参照
  GH_TOKEN   = "${secret:keychain:GH_TOKEN}",# macOS Keychain（Linux: secret-tool）
}
```

`.ezpn.toml` の隣にある `.env.local` は自動マージされ、`[env]` を上書きします。`${secret:keychain:KEY}` は OS のキーチェーンが利用できない場合、警告を出して `${env:KEY}` にフォールバックします。再帰は循環検知のために深さ8で打ち切られます。

### テーマ

```toml
# .ezpn.toml または ~/.config/ezpn/config.toml
theme = "tokyo-night"   # default | tokyo-night | gruvbox-dark | solarized-dark | solarized-light
```

ユーザーテーマは `~/.config/ezpn/themes/<name>.toml` から読み込まれます。ezpn は `$COLORTERM` / `$TERM` を自動検出し、truecolor 非対応の場合は 256色または16色にダウングレードします。

<details>
<summary>グローバル設定 (~/.config/ezpn/config.toml)</summary>

```toml
border = rounded            # single | rounded | heavy | double | none
shell = /bin/zsh
scrollback = 10000
status_bar = true
tab_bar = true
prefix = b                  # プリフィクスキー (Ctrl+<key>)
theme = default             # default | tokyo-night | gruvbox-dark | solarized-dark | solarized-light
persist_scrollback = false  # 自動スナップショットにスクロールバックを保存（既定はオフ）
```

設定パネル（`Ctrl+B Shift+,`）での変更はアトミックに永続化されます。`Ctrl+B r` でディスクから再読み込みできます。

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
| `r` | 設定を再読み込み |
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

**ezpn** — 設定不要で即使えるターミナル分割 + `ezpn-ctl send-keys` / イベントストリーム / フックのスクリプト面。
**tmux** — 深いプラグインエコシステム（TPM など）が必要な場合。
**Zellij** — WASMプラグインが欲しい場合。

## CLIリファレンス

```
ezpn [ROWS COLS]         グリッドレイアウトで開始
ezpn -l <PRESET>         レイアウトプリセットで開始
ezpn -e <CMD> [-e ...]   ペインごとのコマンド
ezpn -S <NAME>           名前付きセッション
ezpn -b <STYLE>          ボーダースタイル (single/rounded/heavy/double/none)
ezpn --new               強制的に新規セッション（既存セッションへの自動アタッチをスキップ）
ezpn a [NAME]            セッションに接続
ezpn ls                  セッション一覧
ezpn kill [NAME]         セッション終了
ezpn rename OLD NEW      セッション名変更
ezpn init                .ezpn.toml テンプレート生成
ezpn from <FILE>         Procfileからインポート
ezpn doctor              .ezpn.toml と環境変数の補間を検証
```

### `ezpn-ctl`（スクリプティング）

```
ezpn-ctl list                                ペイン一覧
ezpn-ctl split [horizontal|vertical] [PANE]  ペイン分割
ezpn-ctl close PANE                          ペインを閉じる
ezpn-ctl focus PANE                          ペインにフォーカス
ezpn-ctl save <PATH>                         ワークスペーススナップショット保存
ezpn-ctl load <PATH>                         ワークスペース復元
ezpn-ctl exec PANE <CMD>                     ペインを新しいコマンドで置き換え

ezpn-ctl send-keys [--pane N | --target current] [--literal] -- <key>...
                                             コードトークンまたは生バイトをペインの PTY へ送信。
                                             例:
                                               ezpn-ctl send-keys --pane 0 -- 'echo hi' Enter
                                               ezpn-ctl send-keys --target current -- C-c
                                               ezpn-ctl send-keys --pane 0 --literal -- $'#!/bin/sh\nexit 0\n'

ezpn-ctl clear-history --pane N              可視画面より上のスクロールバックを破棄
ezpn-ctl set-scrollback --pane N --lines L   スクロールバックのリングサイズ変更（scrollback_max_lines が上限）
```

## ライセンス

[MIT](../LICENSE)
