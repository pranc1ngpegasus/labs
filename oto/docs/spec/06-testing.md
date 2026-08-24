---
title: テストと検証戦略
topic: testing
status: draft
date: 2026-08-24
depends: [04-encoding, 05-nix-packaging]
---

# 06 — テストと検証

## 方針

録音ツールのうち **デバイスと無関係な層(変換・エンコーダ・コンテナ・フレームバッファ)を
ヘッドレスで網羅**し、デバイス依存の層(キャプチャ開始)は実機で最小限検証する。
CI(`nix flake check` → `cargo test --workspace`)はオーディオデバイスなしで通ることを必須とする。

```
デバイス依存:  capture 開始 / デバイス列挙 … 実機 or 手動
デバイス非依存: convert / wav / ogg_opus / pipeline(チャネル) … ヘッドレステスト
```

テストの置き場所はクレート分割(02)に合わせ、トピックごとに閉じる:

- 変換・エンコーダ・コンテナのテスト → `oto-encode`(`wav.rs` / `ogg_opus.rs` / `convert.rs` のユニットテスト + 往復テスト)
- チャネル・ドロップ・セッション制御 → `oto-core`(`pipeline.rs` / `recorder.rs`)
- デバイス列挙・キャプチャ → 実機(`#[ignore]` の integration test)

## ヘッドレステスト(CI で実行)

### convert.rs

- S16 → i16: 変換なし(reinterpret)が正しいこと(バイト列 → サンプル列)。
- F32 → i16: `NaN → 0`、`+Inf/-Inf → clamp`、`±1.0 → ±32767`、境界値。
- ダウンミックス: 3ch→1ch(平均)、3ch→2ch(ペア平均)、2ch→1ch、1ch はそのまま。
- レート判定: 対応レート集合の判定ロジック。

### wav.rs

- 既知バイト列との一致: PCM16・float32 それぞれのヘッダ 44 バイト(フィクスチャ)。
- `finalize()` でサイズフィールドが正しく書き戻されること(書いて閉じて読み直し)。
- 4 GiB 超過時のエラー。

### ogg_opus.rs

- **Opus 往復テスト**: 正弦波 PCM を `Encoder::encode` → `Encoder` の入力に直結、
  `Decoder`(shiguredo_opus 同梱)で戻し、非無音・RMS が閾値以上であることを確認。
- フレームバッファ: 分割チャンク(任意サイズ)を流しても 1 パケット =
  `frame_samples` サンプルになること。残余の持ち越し。
- 端数フレーム: 停止時にゼロパディングされて最終フレームが出ること。
- granulepos: 全ページの granulepos が 48 kHz 単位で単調増加し、
  `pre_skip + n × frame_samples` の系列になること(パーサをテスト内に書いて検証)。
- ヘッダ: `OpusHead` / `OpusTags` のバイト列(フィクスチャ)と一致、EOS/BOS フラグ。

### pipeline.rs

- バウンドチャネルの drop-oldest: 満杯時に最新が残り、drop カウントが増えること
  (デバイスなしで `sync_channel` を直接駆動してテスト)。

### CLI

- usage-rs のパース: 引数エラー → `MainError::InvalidArgs`(exit 2)の単体テスト。

## 実機検証(自動化しない)

CI にオーディオデバイスは無いため、capture 経路は手動で確認する。
`OTO_DEVICE_TESTS=1` 環境変数のときだけ実行される integration test を用意し、
実機で `cargo test -- --ignored` 相当にできるようにする(デバイス試験は `#[ignore]`)。

マニュアルチェックリスト:

| OS | 確認項目 |
|---|---|
| macOS | `oto list` で内蔵マイク/外部 USB が見える。`oto record a.wav` → 音声が入る。権限プロンプトが出ること。`oto record a.ogg` が ffplay / VLC で再生できる |
| Linux (PulseAudio / pipewire-pulse) | `oto list`、44.1 kHz デバイスでの `oto record a.ogg`(リサンプル経路)、`aplay` で再生確認 |
| Windows | WASAPI デバイスで録音、メディアプレイヤーで再生確認(実レート/チャンネルが要求と異なる場合の挙動) |
| 全 OS | Ctrl-C 正常停止→ファイル確定、Ctrl-C 2 回→exit 5、`--duration` 停止、`--device` 選択、ドロップ警告表示 |

## CI で担保されること

- `checks.fmt` / `checks.clippy`(`--all-targets --all-features -- --deny warnings`):
  oto のコードも厳格 lint(pedantic + nursery、`unsafe_code`/`unwrap_used`/`panic` deny)が必須。
- `checks.test`: 上記ヘッドレステスト一式。
- `checks.hakari`: 依存 feature の統合に漏れがないこと(workspace-hack 再生成を忘れると fail)。
- 実機経路(録音そのもの)は CI では未検証である点を README に明記する。

## 検証ツール

- 生成物の外部視点での確認に `ffmpeg`/`ffprobe`(nix develop 上)を使う:
  `ffprobe -show_streams out.ogg` で Opus ストリームの sample rate / channels / pre-skip を確認し、
  `ffplay out.ogg` で実際に再生して無音録音でないことを確かめる
  (手動スクリプト `oto/scripts/verify-recording.sh` を任意で用意)。