---
title: Oto — 設計仕様 Index
topic: index
status: draft
date: 2026-08-24
---

# Oto — クロスプラットフォーム・オフライン録音ツール(将来: 転写)

**Oto** は、[shiguredo/audio-device-rs](https://github.com/shiguredo/audio-device-rs) と
[shiguredo/opus-rs](https://github.com/shiguredo/opus-rs) を利用した、マイク入力のオフライン録音 CLI ツールです。
macOS / Linux / Windows で同じ操作・同じファイル形式で録音できます。

- 録音はすべてローカルで完結する。ネットワークアクセスは不要(オフライン)。
- 音声データはデバイスを出ない。テレメトリ・クラウド送信なし。
- 出力形式は WAV(可逆・そのまま)と Ogg/Opus(圧縮)の 2 種。拡張子で切り替える。
- Nix flake で `packages.<system>.oto` としてパッケージ化する(本リポジトリ `labs` の既存パターン踏襲)。
- **ロードマップ**: 長期的にはクロスプラットフォームなオフライン転写に拡張する。
  macOS 専用の録音・転写ツール koe とは「将来収束する」関係(01-requirements 参照)。

## 設計原則

1. **オフライン・ファースト。** 録音パイプラインにネットワークは一切現れない。
2. **ミニマム。** MVP は「録音してファイルを残す」だけ。再生・GUI・設定ファイルは持たない。
   転写は MVP では持たないが、**クロスプラットフォーム転写をロードマップに含む**(koe は現時点で
   macOS 専用の録音・転写ツール。両者の関係は 01 に定義)。
3. **プラットフォーム共通 API 一本。** audio-device-rs の `AudioCapture` だけで入力デバイスを扱う。
   プラットフォーム固有コードは持たない(デバイス列挙・キャプチャはすべて同クレートに委ねる)。
4. **デバイス非依存コードの分離。** エンコーダ・コンテナ書き出し・変換ロジックは実デバイスなしで
   テスト可能な純 Rust の層に置く。CI はここをヘッドレスで検証する。
5. **リポジトリ規約の踏襲。** 単一 Cargo workspace + crane による Nix ビルド、厳格 lint
   (`unsafe_code` / `unwrap_used` / `panic` deny、clippy `pedantic` + `nursery` deny)、
   jdx/usage (`usage-rs`) ベースの CLI、`tokio::signal` による Ctrl-C 処理(koe-cli と同型)。

## ドキュメント Index

| # | ドキュメント | 概要 |
|---|--------------|------|
| 01 | [要件定義](./01-requirements.md) | 目的、要件の批判的検討(5 ステップ)、ゴール/非ゴール、MVP スコープ |
| 02 | [アーキテクチャ](./02-architecture.md) | クレート構成、データフロー、スレッドモデル、エラー設計 |
| 03 | [CLI インターフェース](./03-cli-interface.md) | サブコマンド、フラグ、出力、終了コード |
| 04 | [エンコードとコンテナ](./04-encoding.md) | サンプル形式変換、WAV、Opus + Ogg(RFC 7845)の詳細 |
| 05 | [Nix パッケージング](./05-nix-packaging.md) | flake 統合、システム依存、libopus prebuilt の扱い、CI、Windows |
| 06 | [テストと検証](./06-testing.md) | ヘッドレステスト戦略、デバイス試験、CI での検証 |
| 07 | [実装計画](./07-implementation-plan.md) | PR 分割、リスクと対策、将来拡張 |

## クレート構成(計画)

トピック単位で 4 クレートに分割(依存は一方向: `oto-cli → oto-core → {oto-capture, oto-encode}`)。

```
oto/
├── oto-capture/          # デバイス列挙・選択・キャプチャ(shiguredo_audio_device を隔離)
├── oto-encode/           # 変換(S16/F32→i16、ダウンミックス、リサンプル)+ エンコーダ(WAV / Ogg+Opus)。ヘッドレステスト対象
├── oto-core/             # 録音パイプライン(バウンドチャネル、コンシューマ、録音セッション制御・統計)
├── oto-cli/              # バイナリ(bin 名: oto)。usage-rs CLI、list/record、シグナル処理、進行表示
├── package.nix           # flake 用パッケージ定義(koe/sui パターン)
├── nix/patches/
│   └── shiguredo_opus-prebuilt.patch
└── docs/spec/
```

詳細は [02-architecture](./02-architecture.md) を参照。

## 実装者向け Quick-Start

1. [01-requirements](./01-requirements.md) でスコープと「切ったもの」を確認する。
2. [02-architecture](./02-architecture.md) でデータフローとスレッドモデルを把握する。
3. [04-encoding](./04-encoding.md) で WAV / Ogg(Opus)のバイト列仕様を確認する。
4. [05-nix-packaging](./05-nix-packaging.md) のパッチ適用方針はビルドを触る前に必ず読む。
5. 実装順は [07-implementation-plan](./07-implementation-plan.md) の PR 分割に従う。