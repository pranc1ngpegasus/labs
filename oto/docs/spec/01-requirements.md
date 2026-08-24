---
title: 要件定義 — 目的・スコープ・要件の批判的検討
topic: requirements
status: draft
date: 2026-08-24
depends: [00-index]
---

# 01 — 要件定義

## 目的

「oto という Nix package」として、audio-device-rs(キャプチャ)と opus-rs(エンコード)を利用した
**クロスプラットフォームなオフライン録音ツール**を本リポジトリ `labs` に追加する。
まず設計資料を oto/docs に用意するのが今回のフェーズである(本ドキュメント群)。

**oto の長期的な方向性は「クロスプラットフォームなオフライン転写」を含む**(録音 + 転写が
macOS / Linux / Windows で動くプラットフォーム)。リポジトリ内の koe は現時点で macOS 専用の
録音・転写ツールであり、oto とは「将来収束する」関係にある(詳細は「oto の将来像と koe との関係」を参照)。

想定ユーザーストーリー:

- 「会議やメモの録音を、OS を問わず同じコマンドでローカルファイルに残したい」
- 「マイク入力の圧縮録音をスクリプト/CI 的に一定時間だけ取りたい」(例: `oto record out.ogg --duration 60`)
- (将来)「録音した内容を、どの OS でもオフラインで文字起こししたい」

## 要件の批判的検討(5 ステップ)

適用方針として、要求をそのまま受け入れずに以下で再検討する。

### Step 1: 要件を疑う

| 与えられた要求 | 検討 | 結論 |
|---|---|---|
| 「oto という Nix package を切る」 | 本リポジトリは flake に `packages.<system>.{koe,sui,ren}` を持つ。**「Nix package を切る」= flake の `packages.<system>.oto` として追加する**、と解釈する(nixpkgs への上流提案ではない)。 | リポジトリ内 flake に 1 パッケージ追加 |
| 「クロスプラットフォーム」 | 本 flake の対象 system は `x86_64-linux` / `aarch64-linux` / `aarch64-darwin`。Windows は flake のビルド対象外なので、**plain `cargo build` で WASAPI バックエンドをビルドできること**を「クロスプラットフォーム」の定義に含める(Nix での Windows ビルドは追わない)。 | Linux/macOS は Nix、Windows は cargo で対応 |
| 「audio-device-rs と opus-rs を利用」 | 両クレートは必須。audio-device-rs のキャプチャ API はデバイス列挙もカバーするため追加のオーディオクレートは不要。 | 2 クレートで完結させる |
| 「オフライン」 | 録音中・再生中にネットワークを使わない。ファイルはローカルに完結。 | 設計上の前提(非機能要件) |
| 「録音ツール」の暗黙の範囲 | 再生・音量調整・エフェクト・転写・GUI は**要求にない**ので作らない。 | スコープ外 |

### Step 2: 削除する

- **GUI / TUI**: 不要(ratatui は workspace にあるが採用しない)。単一コマンドの CLI だけ。
- **再生機能**: 録音ツールに再生は含めない。
- **転写機能(MVP)**: MVP では作らない。ただし oto のロードマップには**クロスプラットフォーム転写**を含む
  (「oto の将来像と koe との関係」参照)。将来、koe の macOS on-device ASR を oto の macOS エンジンとして
  再利用する経路を確保する。
- **設定ファイル**: フラグだけで足りる。`~/.config/oto/` は作らない。
- **PipeWire バックエンド**: 初期は PulseAudio のみ。PipeWire 環境は互換レイヤー `pipewire-pulse` で動く。
  後述リスクで対処手順のみ記す。feature はコメントで残す。
- **オフライン再エンコードコマンド**(`oto encode` で wav→ogg 変換): MVP では削除。後で足せる。
- **デバイスごとの詳細情報コマンド**: `oto info` は作らない。`oto list` に必要な情報を集約。
- **FFI / GUI のための分割はしない**: oto には Swift FFI も GUI も無い。クレート分割は
  **トピック単位**(capture / encode / pipeline / cli、02 参照)で行う。それ以外の過剰な
  抽象化(ジェネリックなデバイス抽象、config 構造体など)は作らない。

### Step 3: シンプル化

- 出力形式は**拡張子で決定**(`.wav` / `.ogg` / `.opus`)。`--format` での上書きのみ用意し、フラグ重複を避ける。
- キャプチャは 48 kHz を要求し、実デバイスの「実際のサンプルレート/チャンネル数」をそのまま使う
  (WASAPI 共有モード等では要求と異なる値になるため)。リサンプルは Opus 経路のみ・必要なときだけ行う。
- バックプレッシャーは**バウンドチャネル + drop-oldest**。オーディオスレッドをブロックしない。
- 依存 crate は最小化: キャプチャ / エンコードは指定の 2 クレート。追加は
  `usage`(jdx/usage の usage-rs、workspace 既存)、`tokio`(既存)、`ogg`(コンテナ)、
  `rubato`(リサンプル)、`serde_json`(既存、`list --json`)、`jiff`(既存、出力ファイル名タイムスタンプ)、`thiserror`(既存)。

### Step 4 / Step 5: 加速・自動化は後で

方針が固まった後のみ、CI(`nix flake check` を既存 workflow が実行)とヘッドレステストで反復を加速する。
「自動化」に該当する追加物(リリース用ハンドブック等)は MVP 後。

## ゴール / 非ゴール

### ゴール

- `oto record <out.wav|out.ogg>` でマイク入力をファイルに録音できる(macOS / Linux / Windows)。
- `oto list` で入力デバイスを列挙できる(人間向けテキストと `--json`)。
- 録音は Ctrl-C / `--duration` で終了し、ファイルを**正常に確定**できる(ヘッダ書き戻し、Ogg 最終ページ)。
- audio-device-rs と shiguredo_opus の両方が実際に使われる。
- `nix flake check`(CI)が通る。macOS / Linux は `nix build .#oto` でビルド可能。

### 非ゴール(MVP では作らない)

- デバイス再生、ボリューム制御、音声効果
- 転写・音声認識・字幕生成(※将来トピック。ロードマップ参照)
- GUI / TUI
- 設定ファイル、デーモン、システム統合(ホットキー等)
- エンコード済みファイルの再エンコード(`oto encode`)
- WebRTC / 配信 / ネットワーク転送
- Nix による Windows ビルド

## oto の将来像と koe との関係

### 前提: 収束するロードマップ

- **oto** = クロスプラットフォーム(macOS / Linux / Windows)の「録音 + オフライン転写」を目指す。
- **koe** = 現時点で macOS 専用の録音・転写ツール(エンジンに Apple 製 on-device SpeechAnalyzer、
  キャプチャに CoreAudio Process Tap / ScreenCaptureKit、Swift + uniffi のネイティブ層)。

両者には将来同じゴール(録音 + 転写)が重なるため、**長期的には収束させる**。

### 当面の方針

1. **今はマージしない**: クロスプラットフォーム ASR エンジン(whisper.cpp 系 / sherpa-onnx / Vosk 等の
   ローカルエンジン)が未選定のまま、動いている koe を大改造するのは Step 1(要求の具体化)より先回り。
   oto の MVP は録音のみなので、統合メリットもまだ顕在化しない。
2. **収束を安くする設計は今やる**: エンコーダ・パイプラインの概念は koe-core を踏襲する。
   さらに **トランスクリプト出力形式は koe の既存スキーマ(koe-core/transcript の JSON cues 等)と
   整合させる**ことを設計方針とする(形式だけ今合わせておく。実装は後)。
3. **統合判断ポイント = ASR エンジン選定時**: ローカル転写エンジンが決まった時点で、以下から選択する。
   - (a) エンジン抽象を oto に追加し、koe の macOS ASR を oto の macOS エンジンとして取り込む
     (koe-core / koe-ffi / koe-native の対象部分を oto 側に移管 or 依存)
   - (b) 共通コア(パイプライン・コーデック・トランスクリプト)を 1 クレートに切り出し、両 CLI が使う
   この決定は設計文書に記録し、決定までの「共有クレート切り出し」は行わない(Step 2: 戻せるものは切らない)。

### koe に統合しない理由(記録)

- koe の看板機能(転写)は macOS 専用エンジン依存のため、他 OS では「録音だけの koe」になり統合メリットが半減する。
- koe への統合は libpulse / bindgen / libopus(FOD・パッチ)という重いビルド依存を koe 全体に載せ、
  また koe-core は koe-ffi に結合しており(例: `codec/ogg.rs` が `koe_ffi::AudioSourceConfig` を使用)
  流用にはリファクタが要る。
- よって oto は独立クレート群として構築し、収束はエンジン選定時に判断する。

## 技術選定サマリ

| 用途 | 選定 | 理由 |
|---|---|---|
| キャプチャ・デバイス列挙 | `shiguredo_audio_device` (=2026.3) | CoreAudio / PulseAudio / WASAPI を 1 API で吸収。ランタイム依存クレートなし |
| Opus エンコード | `shiguredo_opus` (=2026.2.0, default-features 無効) | libopus を静的リンク。Decoder 付きでテストに使える。シンボル衝突回避済み |
| Ogg ページ組立 | `ogg` | 純 Rust・実績あり。Opus ヘッダ/granule 計算は自前で行う |
| リサンプル(Opus 経路のみ) | `rubato` | 純 Rust。44.1 kHz 等のデバイスを 48 kHz に揃える |
| CLI フレームワーク | jdx/usage の Rust 実装 `usage-rs`(`usage = { package = "usage-rs", version = "6", features = ["validation"] }`、workspace 既存) | koe-cli と同一パターン。KDL 仕様 1 つから解析・help・completions を導出できる |
| シグナル処理 | `tokio::signal` | koe-cli と同一パターン(二度押しで強制終了) |
| エラー | `thiserror`(workspace 既存) | koe/sui と同じ流儀。終了コードで分類 |

## MVP スコープ(確定)

1. `oto list` / `oto version` — デバイス列挙(入力のみ、JSON オプション付き)
2. `oto record out.wav` — WAV 録音(S16 なら PCM16、F32 なら IEEE float32、ネイティブレート・チャンネル)
3. `oto record out.ogg` — Opus 録音(i16 変換、必要なら downmix / resample、Ogg/Opus コンテナ)
4. Ctrl-C / SIGTERM / `--duration` での正常終了とファイル確定、二度押し強制終了
5. Nix パッケージ `packages.<system>.oto` と CI 通過、ヘッドレステスト一式

## 参照

- audio-device-rs 仕様: <https://github.com/shiguredo/audio-device-rs>
- opus-rs 仕様: <https://github.com/shiguredo/opus-rs>
- usage (jdx/usage) — CLI 仕様・CLI ツール・Rust フレームワーク: <https://github.com/jdx/usage> / <https://usage.jdx.dev/>
- RFC 7845(Ogg Opus): <https://www.rfc-editor.org/rfc/rfc7845>
- リポジトリ内 koe(パターン参照): [`koe/docs/spec/`](../../koe/docs/spec/00-index.md)