---
title: CLI インターフェース
topic: cli
status: draft
date: 2026-08-24
depends: [02-architecture]
---

# 03 — CLI インターフェース

## 概要

CLI は **jdx/usage**(<https://github.com/jdx/usage>)の Rust フレームワーク `usage-rs` で定義する。
workspace の既存依存 `usage = { package = "usage-rs", version = "6", features = ["validation"] }`
を koe-cli と同一の derive スタイルで使う。

usage は「CLI の仕様(KDL)・CLI ツール・Rust フレームワーク」をまとめたプロジェクト(OpenAPI for CLIs)で、
**1 つの Rust 宣言(実体は KDL 仕様)からコマンド解析・help・シェル completions を導出**できる。
oto が使うのは解析(derive + validation)であり、completions / ドキュメント生成(`usage` CLI)は
将来拡張(07 参照)。

```rust
#[derive(Debug, Cli)]
#[usage(
    bin = "oto",
    version = env!("CARGO_PKG_VERSION"),
    about = "Offline audio recorder — real-time mic capture to WAV or Ogg/Opus",
    arg_required_else_help,
)]
struct CliRoot {
    #[usage(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommands)]
enum Command {
    /// List input audio devices.
    List(ListArgs),
    /// Record microphone input to a file.
    Record(RecordArgs),
}
```

`oto help` / `oto --help` は usage-rs が生成する。`version` サブコマンドは
`--version` で代替可能なため持たない(削除対象。必要なら後で足す)。

## サブコマンド

### `oto list`

```console
oto list [--json]
```

- 入力デバイスに限定して列挙する(`AudioDeviceList::enumerate_input()`)。
- テキスト出力(既定):

  ```
  1: MacBook Pro Microphone (ID: 8D93D0E0-... ) mono 48000 Hz
  2: USB Audio Device (ID: ... ) stereo 48000 Hz
  ```

- `--json`: `[{ "name", "unique_id", "channels", "sample_rate" }]`(`serde` + `serde_json`)。
- テキスト出力では最後にシステム音声ソースの利用可否を 1 行表示する:
  `System audio: available (macOS 13+) (`oto record --source system`)`。
  `--json` のスキーマは入力デバイスのみで変更しない(安定契約)。

### `oto record`

```console
oto record [<output>] [--source <mic|system>] [--device <id-or-name>]
           [--channels <1|2>] [--bitrate <kbps>] [--duration <secs>]
           [--format <wav|ogg>] [--quiet]
```

| フラグ | 既定値 | 説明 |
|---|---|---|
| `<output>` | `oto-<timestamp>.ogg` | 出力パス。拡張子で形式を判定(既定は Ogg/Opus) |
| `--source` | `mic` | 録音ソース。`mic` / `microphone`(入力デバイス)と `system` / `loopback`(システム出力ミックス)。`system` は macOS 13+ の ScreenCaptureKit 経由(ドライバ不要)。他プラットフォームでは未実装(exit 3) |
| `--device` | デフォルト入力 | `unique_id` 完全一致 → なければ `name` の部分一致(大文字小文字無視)。一致なしは exit 3。`--source system` では使わない |
| `--channels` | mic:`1` / system:`2` | 要求チャンネル数。実機が別の値を返す場合は実機優先(WASAPI 共有モード等) |
| `--bitrate` | `64` | Opus ビットレート(kbps)。WAV では無視 |
| `--duration` | なし(Ctrl-C まで) | 指定秒数で自動停止。`--duration 90` / `--duration 1.5`(小数可) |
| `--format` | 拡張子判定(既定 Ogg/Opus) | `wav` / `ogg` を明示指定。拡張子と異なる場合はこちらを優先 |
| `--quiet` | false | 進行表示を抑制(ログは stderr にのみ) |

実行中の表示(1 秒間隔で更新、stderr、`--quiet` で抑制):

```
Recording microphone input to recording-20260824-153000.ogg [mono 48000 Hz] — Ctrl-C to stop
Recording system audio to system-20260824-153000.ogg [stereo 48000 Hz] — Ctrl-C to stop
```

停止後のサマリ(stdout):

```
Wrote recording-20260824-153000.ogg (1:23, 680 KB, 3 frames dropped)
```

### 形式の決定

```
output extension:
  .ogg | .opus  → Opus (shiguredo_opus + ogg コンテナ)
  .wav          → WAV
  other         → Opus (既定。拡張子は補正しない。ユーザーの指定を尊重)
--format が与えられた場合、拡張子より優先。
既定の出力ファイル名も .ogg とする(oto-<timestamp>.ogg)。
```

## 終了コード

koe-cli と同一の割り当てを踏襲する:

| コード | 意味 |
|---|---|
| 0 | 正常終了(録音完了、Ctrl-C 1 回目による正常停止含む) |
| 1 | 予約(未使用) |
| 2 | 引数エラー(usage パース失敗) |
| 3 | デバイス列挙・キャプチャ構築/開始エラー(マイク権限不足のヒントを含む) |
| 4 | ファイル I/O エラー |
| 5 | 割り込み(SIGINT 2 回目による強制終了。ファイル未確定の可能性) |
| 6 | 内部エラー |

> 注: koe は `1 = 権限エラー` を設けていますが、oto は `oto permissions` サブコマンドを
> 持たないため、マイク権限まわりの失敗はすべて exit 3(キャプチャエラー)に集約し、
> メッセージ内でシステム設定への導線を案内します。

## 実行例

```console
$ oto list
1: MacBook Pro Microphone (ID: 8D93D0E0-...) mono 48000 Hz

$ oto record memo.ogg --duration 90
Recording to memo.ogg [mono 48000 Hz, 64 kbps] — 1:30, 680 KB — Ctrl-C to stop
Wrote memo.ogg (1:30, 680 KB)

$ oto record backup.wav --device "USB Audio"
...
```