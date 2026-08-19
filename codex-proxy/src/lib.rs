//! `codex-proxy` — 常駐 OAuth プロキシ。
//!
//! Codex CLI の `ChatGPT` (OAuth) トークンを自動更新しながら、ローカルに
//! OpenAI-compatible な `/v1/responses` エンドポイントを expose する HTTP
//! サーバのライブラリ部分。CLI 本体は `main.rs`。

pub mod auth;
pub mod error;
pub mod proxy;
