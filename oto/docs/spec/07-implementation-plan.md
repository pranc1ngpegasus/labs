---
title: 実装計画 — PR 分割・リスク・将来拡張
topic: implementation-plan
status: draft
date: 2026-08-24
depends: [01-requirements, 05-nix-packaging]
---

# 07 — 実装計画

## 進め方

設計資料のレビュー後、以下の PR を順に出す。各 PR は `nix flake check` が通ること
(CI 1 本)を完了条件とする。コミットは Conventional Commits(`feat` / `fix` / `chore` /
`docs` / `test`)、1 論理変更 1 コミット。

## PR 分割

| # | PR(タイトル例) | 内容 | 完了条件 |
|---|---|---|---|
| 1 | `chore: add oto crates to workspace` | `oto/` に 4 クレート(`oto-capture` / `oto-encode` / `oto-core` / `oto-cli`)をスキャフォールド、workspace/Cargo.toml の members + `[workspace.dependencies]` への path 依存追加、計画済み依存の一括追加(`shiguredo_audio_device` / `shiguredo_opus` / `ogg` / `rubato` / `serde_json` / `jiff`)、workspace-hack 再生成、flake に `libpulse`/`libclang`/FOD/`cargoPatches` 追加、`oto/package.nix`、`packages.oto` と default への追加 | `nix flake check` 通過(deps ビルドが Linux で通る) |
| 2 | `feat(oto): device listing` | `oto list`(+ `--json`)。バージョン表示は `--version`(usage-rs 生成)で足りるためサブコマンドは作らない。Capture 未使用 | check 通過 + 実機で列挙 |
| 3 | `feat(oto): wav recording` | capture → channel → WAV ライタ、Ctrl-C/SIGTERM/二度押し、`--duration`、進行表示、サマリ。`wav.rs` ヘッドレステスト | check 通過 + 実機で .wav 録音 |
| 4 | `feat(oto): opus+ogg recording` | convert(i16/ダウンミックス/リサンプル)、`ogg_opus.rs`(RFC 7845)、拡張子ディスパッチ、`ogg_opus.rs` 往復テスト | check 通過 + 実機で .ogg 録音・ffprobe 確認 |
| 5 | `docs(oto): README and licenses` | `oto/README.md`、ルート README への追記、libopus の THIRD-PARTY 注記 | レビュー |
| 6 | `test(oto): pipeline and edge cases`(改善イテレーション) | バウンドチャネル drop 試験、端数・異常系、`#[ignore]` の実機 integration、マニュアルチェックリスト整備 | check 通過 |

> 1 は技術的に最も不確実(パッチ適用・libclang env・FOD)。ここで nix ビルドを先行確立させ、
> 以降の PR を「コード追加だけ」に保つ。

## 実装上の注意(既知の罠)

- **クレート間依存**: トピック分割(02 参照)のため、クレート間は `[workspace.dependencies]` の
  path 依存で接続する(koe/sui と同じ)。**依存の向きは `oto-cli → oto-core → {oto-capture, oto-encode}` の一方向のみ**。循環はエラー。
- **workspace-hack**: 依存を追加したら必ず `cargo hakari generate` して `workspace-hack` を
  再生成し `cargo hakari verify` を通す(CI の `checks.hakari` が fail する)。
- **lockfile**: `Cargo.lock` はコミット済み。`cargo build` で自動更新される差分を必ず含める。
- **ロックダウン lint**: `unwrap_used` / `panic` / `unsafe_code` が deny のため、
  shiguredo 系クレートの `Result` を `?` で安全に伝播させる設計を守る。
- **capture コールバック内でエンコードしない**: チャネル送信のみ(callback は `Send + Sync + 'static`)。
- **`default-*` feature は プラットフォームごとに 1 つ**: ターゲット別 dependency で有効化する
  (05 参照)。2 つ以上有効だと audio-device-rs の build.rs が panic する。

## リスクと対策

| リスク | 影響 | 対策 |
|---|---|---|
| `cargoPatches` が transitive 依存に効かない | PR 1 が遅延 | **解決済み**: crane の `overrideVendorCargoPackage` で vendor ソースに直接 patch(05 参照)。`cargoPatches` は自分の Cargo.toml/patch 用 |
| bindgen + libclang が aarch64-darwin の Nix で通らない | macOS パッケージ不可 | CI 対象外のためローカル検証。`pkgs.llvmPackages.libclang` + env で解決を試み、ダメなら `pkgs.darwin.apple_sdk` 由来の clang を nativeBuildInputs に追加。koe が objc2/フレームワーク系ビルドを aarch64-darwin で通している前例あり |
| FOD の Ubuntu 24.04 prebuilt が nixpkgs の glibc と噛み合わない | Linux ビルド失敗 | 22.04 アセットへ切替(`url`/`sha256` を更新。libopus は静的 lib のため実質リスク低) |
| WASAPI 実レート/チャンネルが要求と異なる | 録音品質・形式の想定外 | 設計上「実値準拠」で吸収済み(04 参照)。Opus 経路はリサンプル/ダウンミックスで対応 |
| macOS マイク権限(TCC) | 初回録音が無音/エラー | exit 3 のメッセージで「システム設定 → プライバシー → マイク」を案内。`oto permissions` は作らない |
| PulseAudio 不在の Linux(PipeWire のみ) | デバイス列挙失敗 | README に `pipewire-pulse` の有効化手順を記載。必要になったら `pulse,default-pulse` に加え `pipewire` feature を足す(設計上後付け可能) |
| libopus prebuilt の更新追従 | バージョン固定の手間 | `=2026.2.0` 固定 + アップグレード手順(05 参照)を手順化 |

## 将来拡張(切ったものの復元ポイント)

- **クロスプラットフォーム転写(ロードマップ)**: ローカル ASR エンジン抽象(whisper.cpp 系 /
  sherpa-onnx / Vosk 等)を oto に追加し、macOS エンジンとして koe の on-device ASR
  (koe-ffi / koe-native の SpeechAnalyzer ブリッジ)を再利用する。
  **統合判断ポイント = ASR エンジン選定時**(01-requirements「oto の将来像と koe との関係」参照)。
- **トランスクリプト形式の整合**: 転写実装時は koe の既存スキーマ(koe-core/transcript の
  JSON cues 等)と出力形式を揃える(形式の設計のみ先に決めておく。実装はエンジン選定後)。
- `oto encode`(既存 wav → ogg 変換): エンコーダは既に分離されているので追加コスト小。
- `usage` CLI(jdx/usage)による Shell completions / Markdown ドキュメント生成
  (usage-rs の derive が出力する KDL 仕様から `usage` ツールで生成できる)。
- PipeWire バックエンド: feature 追加 + `--backend` フラグ。
- 複数パケット/ページの Ogg 書き出し(オーバーヘッド削減)。
- 再生モニタリング(`--monitor`)、レベルメーター。
- macOS ランナーでの CI 追加(`nix flake check --system aarch64-darwin` を macos-14 ジョブで)。
- 権限チェックコマンド(`oto permissions`、koe と同じ枠組み)。