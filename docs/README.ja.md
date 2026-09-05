<p align="center">
  <img src="../assets/hero.png" width="720" alt="ezpn デモ">
</p>

<h1 align="center">ezpn</h1>

<p align="center">
  <strong>ターミナルペイン、瞬時に。</strong><br>
  マウス操作、持続するセッション、使い慣れたプレフィックスキーを備えた macOS・Linux 向けターミナルマルチプレクサ。
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

## 作業を始める

```sh
cargo install ezpn --locked
ezpn                 # two shells
ezpn 2 3             # a 2-by-3 grid
ezpn -S work         # create or reattach to a named session
```

ビルドには Rust 1.88 以降が必要です。[GitHub Releases](https://github.com/subinium/ezpn/releases)で
macOS・Linux 向けバイナリを配布しています。チェックサムが付属する場合は検証してください。
ezpn は実行型のターミナルマルチプレクサであり、Rust に組み込む GUI ライブラリではありません。

## セッションと SSH

```sh
ezpn a work
ezpn a work --shared
ezpn a work --readonly
ezpn ls
ezpn kill work
```

`Ctrl+B` の後に `d` を押すと、現在のクライアントだけをデタッチします。非アクティブなタブの
ジョブも含め、シェルプロセスは動き続けます。再アタッチすると、そのプロセスに再接続します。
読み取り専用クライアントは入力できず、書き込み可能なクライアントの作業領域もリサイズできません。

リモートホストに ezpn をインストールし、PATH から実行できるようにしてください。

```sh
ssh -t host 'ezpn -S work'
ssh -t host 'ezpn a work'
ssh -J bastion -t host 'ezpn a work'
```

SSH では PTY の割り当てが必要です。SSH クライアントが切断されても、リモートのデーモンは終了しません。
暗号化、認証、ホスト鍵の検証、転送は OpenSSH が担当します。
ezpn のローカル Unix ソケットを、認証のないネットワークに公開しないでください。

## マウスとキーボード

| 操作 | 動作 |
| --- | --- |
| ペインの内容をクリック | ペインにフォーカス |
| 区切り線をドラッグ | 分割サイズを変更 |
| タイトルバーの分割ボタン | 選択したペインを分割 |
| タイトルバーの閉じるボタン | 確認してから閉じる |
| タブをクリック | タブを切り替え |
| スクロール | 履歴をスクロール、またはマウス対応アプリへ転送 |
| テキストをドラッグ | 選択してコピー |
| Shift + ドラッグ | アプリへのマウス入力ではなく ezpn のテキストを選択 |
| マウス非対応アプリの内容をダブルクリック | ズームを切り替え |
| F1 / F2 | 設定 / サイズの均等化 |
| Alt + 矢印キー | ペイン間を移動。macOS では Option を Meta に設定 |

アプリに送るクリック、移動、ホイール、ボタン解放イベントには、アプリが要求したマウスエンコーディングを使います。
`Ctrl+D`、`Ctrl+E`、`Ctrl+W` などは、明示的に再割り当てしない限りシェルに送られます。
これらのキーでペインを分割したり、終了を要求したりすることはありません。

`Ctrl+B` に続けて、次のキーを使用します。

| キー | 動作 |
| --- | --- |
| `%` / `"` | 列 / 行を分割 |
| `o` / 矢印キー | ペイン間を移動 |
| `x` | ペインを閉じる確認 |
| `z` | ズームを切り替え |
| `R` | リサイズモード |
| `Space` / `E` | サイズを均等化 |
| `c` / `n` / `p` | 新しいタブ / 次のタブ / 前のタブ |
| `0`–`9` | 0 始まりのインデックスでタブを選択 |
| `,` / `&` | タブ名を変更 / 閉じる確認 |
| `[` | コピーモード |
| `:` | コマンドパレット |
| `r` | グローバル設定を再読み込み |
| `B` | ブロードキャスト入力を切り替え |
| `d` | 現在のクライアントをデタッチ |
| `?` | ヘルプ |
| `Ctrl+B` | プレフィックスキーをアプリに送信 |

コピーモードでは vi 形式の移動、`v`/`V` による選択、`y` または Enter でコピー、
`/`/`?` で検索、`n`/`N` で次/前の一致へ移動、`q`/Escape で終了できます。
一般的な tmux キーバインドの一部に対応していますが、tmux コマンド全体との互換性はありません。

## 作業を維持したままレイアウトを変更

```sh
ezpn -l dev       # 7:3
ezpn -l ide       # 7:3/1:1
ezpn -l quad      # 2-by-2
ezpn -l '7:3/5:5'
ezpn -b none
```

コマンドパレットの `select-layout` は、既存のプロセスを維持したまま再配置します。
ペイン数が異なるレイアウトは拒否するため、ペインの分割や終了は明示的に行ってください。
分割やスナップショットの読み込みに失敗しても、現在の作業領域は壊れません。

## 信頼するプロジェクトの作業領域

自動起動を許可する前に、リポジトリ内のコマンドを確認してください。

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

`--trust-project` は `.ezpn.toml` / Procfile の自動実行を許可します。
リポジトリのコマンドを読み込まずに通常のシェルを起動するには、`ezpn 1 2` のようにグリッドを明示します。
`doctor` は読み取り専用で構文を検査し、コマンドの実行やシークレットの解決は行いません。

プロジェクトの環境変数展開では、環境変数・ファイル・シークレットを参照できます。
外部の値を診断メッセージに出力することはありません。設定で外部の値を読み取ったペインは、
実行に使うスナップショットのメタデータと履歴から除外され、復元時にはクリーンなシェルとして開きます。
解決済みの認証情報を知らないうちに保存することを避け、プライバシーを優先する意図的な方針です。
[設定](../docs/configuration.md)と[セキュリティ](../docs/security.md)を参照してください。

## 設定と復旧

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

テーマ: `ezpn-dark`、`ezpn-light`、`nord`、`gruvbox-dark`、`solarized-dark`。
ユーザーのキーマップは `[keymap.normal]`、`[keymap.prefix]`、`[keymap.copy_mode]` に記述します。
`Ctrl+B r` は、一度読み取って検証したファイル内容から、対応する設定を再読み込みします。
設定パネルで保存に失敗した場合は、成功と表示せずエラーを通知します。

ディスク上のスナップショットと、デタッチして稼働中のセッションは別物です。`ezpn --restore FILE` は
**新しいプロセスを起動します**。明示的に保存を有効にした履歴はテキストとして復元されます。
実行中のエディター、プロセスメモリ、ターミナル画像、厳密な代替画面の状態は復元しません。
スナップショットにはサイズ・展開量の上限があり、アクセス権限も限定されます。

## 互換性と検証根拠

- 対象は、UTF-8 対応 ANSI ターミナルと Unix PTY を使用する macOS・Linux です。
  ネイティブ Windows には対応していません。
- 子アプリのキーボードプロトコル交渉とホストの機能は別です。従来のアプリには従来のシーケンスを送り、
  対応する Kitty 拡張は明示的に有効化した場合に使います。
- アプリからのクリップボード書き込みには、設定した OSC 52 ポリシーを適用します。
  SSH 経由でユーザーがコピーする場合は、リモートのデスクトップクリップボードより接続先のターミナルを優先します。
- 描画範囲には制限があり、小さな表示領域では切り詰めます。パーサーの制限、対応シーケンス、
  未検証の GUI エミュレーターの組み合わせは[ターミナル互換性](../docs/terminal-protocol.md)を参照してください。
- `--features render-diff` は、上限を設けた任意の ANSI 差分出力経路を有効にします。
  非対応のフレームは元の出力に戻ります。常に高速になるという保証ではありません。
- 実際の PTY テストでは、アタッチ/デタッチ、リサイズ、共有/読み取り専用クライアント、転送の中断を検証します。
  別途、隔離したループバック SSH テストで実際の SSH とシミュレーションを区別します。
- 長時間の安定性テストと tmux/Zellij との性能比較は別個の検証事項です。
  ezpn が両プロジェクトより常に高速、または省メモリであるとは主張しません。

[リリース監査](../docs/audits/v0.14.0.md)に結果と残る制約を記録します。
[事前検証スクリプト](../scripts/preflight.py)は PASS/FAIL/SKIP と実際の終了コードを記録します。
失敗したテストを、無視されたプレースホルダーで隠すことはありません。

## ドキュメント

[はじめに](../docs/getting-started.md) · [設定](../docs/configuration.md) ·
[SSH とターミナルプロトコル](../docs/terminal-protocol.md) · [クリップボード](../docs/clipboard.md) ·
[セキュリティ](../docs/security.md) · [スクリプト機能の制限](../docs/scripting.md) ·
[貢献する](../CONTRIBUTING.md) · [変更履歴](../CHANGELOG.md)

## ライセンス

[MIT](../LICENSE)
