# kv-link

KEYENCE KV シリーズ PLC の上位リンク通信（Ethernet / TCP）クライアントライブラリ（Rust、tokio 非同期）。

banto-industrial の banto-hub から KV シリーズに接続するためのワイヤ層として作る
（[tyaro/banto-industrial#438](https://github.com/tyaro/banto-industrial/issues/438)、
置き場所の決定は banto-industrial の `docs/tag-server-design.md` §3.4.1）。
banto には依存しない、単独で使えるライブラリにする。

状態: **v0.1.0 実装中（プロトコル未照合）**（2026-10-18）。クライアント・構造化エラー・
モックサーバーの最初の版はあるが、コマンド・応答の形・エラーコード・ビット番号の文法は
すべて記憶ベースで、キーエンス公式の通信マニュアルとはまだ照合していない
（[docs/protocol.md](docs/protocol.md) の照合表が正）。照合前のものは確定扱いにしない。

## 使い方

```toml
[dependencies]
kv-link = { git = "https://github.com/tyaro/kv-link", tag = "v0.1.0" }

[dev-dependencies]
kv-link = { git = "https://github.com/tyaro/kv-link", tag = "v0.1.0", features = ["mock"] }
```

```rust
use kv_link::{Address, KvClient, KvConfig};

let mut plc = KvClient::connect("192.168.0.10:8501", KvConfig::default()).await?;
let dm100: Address = "DM100".parse()?;
let v: u16 = plc.read(&dm100).await?;
plc.write_many::<i32>(&"DM200".parse()?, &[1, -1]).await?;
plc.write::<bool>(&"MR1015".parse()?, true).await?;
```

- 型: `bool`（`R`・`MR`・`LR`）、`u16`・`i16`・`u32`・`i32`（`DM`・`EM`）。単点（`read`/`write`）と
  連続（`read_many`/`write_many`）。生のコマンドは `raw_command`。
- エラーは `KvError`。`is_connection_fatal()` が `true`（`Framing`・`Timeout`・`NotConnected`・`Io`）
  なら、そのクライアントは毒化済みで以後 `NotConnected` しか返さない。作り直すこと。
  `Device`（PLC の E コード）と `InvalidRequest`（送る前に断った）は非致命。
- 1 つのクライアントで同時に飛ぶ要求は 1 つ（`&mut self`）。複数の利用者での共有・再接続は
  呼び出し側の責務。
- feature `mock` でテスト用のモック TCP サーバー（`kv_link::mock::MockServer`、障害注入・
  要求の記録つき）を使える。
- `f32`・ワード順・デバイス範囲と最大点数の検査はマニュアル照合後に足す。

設計の詳細は `src/lib.rs` のモジュール文書（`cargo doc --open --all-features`）。

## ライセンス

MIT
