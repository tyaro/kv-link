# kv-link

KEYENCE KV シリーズ PLC の上位リンク通信（Ethernet / TCP）クライアントライブラリ（Rust、tokio 非同期）。

banto-industrial の banto-hub から KV シリーズに接続するためのワイヤ層として作る
（[tyaro/banto-industrial#438](https://github.com/tyaro/banto-industrial/issues/438)、
置き場所の決定は banto-industrial の `docs/tag-server-design.md` §3.4.1）。
banto には依存しない、単独で使えるライブラリにする。

状態: **v0.1.0 実装中（マニュアル照合済み・実機未確認）**（2026-10-18）。コマンド・応答の形・
エラーコード・ビット番号の文法・デバイス範囲・点数上限は、キーエンス公式の
「KV-XLE02 ユーザーズマニュアル」13 章（AS_166260_KV-XLE02_UM_084A11_KJ_JP_2036_1）と照合した
（[docs/protocol.md](docs/protocol.md) の照合表が正。マニュアルが直接書いていない部分は推論・未照合と
明記してある）。**実機ではまだ 1 つも確かめていない**ので、「KV シリーズ対応済み」とは書かない。
マニュアルは KV-XLE02 のもので、KV-8000 の内蔵ポートや KV-LE21V などでも同じと仮定しているが未照合。

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

- 型: `bool`（`R`・`MR`・`LR`）、`u16`（`.U`）・`i16`（`.S`）・`u32`（`.D`）・`i32`（`.L`）・`f32`（`.D` の
  ビット列、IEEE 754 単精度と仮定）（`DM`・`EM`）。単点（`read`/`write`）と連続（`read_many`/`write_many`）。
  生のコマンドは `raw_command`。
- 32 bit 型（`u32`・`i32`・`f32`）は指定番号が下位ワード、次の番号が上位ワード。マニュアルは上下 2 ワードの
  同時性を偶数番地でしか保証しないので、**奇数番地から始まる 32 bit アクセスは送らずに `InvalidRequest`**。
- 送る前の検査（違反は `InvalidRequest`、何も送らない）: 番号範囲（`DM`/`EM` 0〜65534、`R` 〜199915、
  `MR` 〜399915、`LR` 〜99915。32 bit の 2 ワード目や連続アクセスの最後の点まで）、1 要求の点数
  （`bool`・16 bit 型 1〜1000、32 bit 型 1〜500）、型とデバイスの組み合わせ。古い KV-7000
  （CPU 機能バージョン 2.2 以下）の `R` は 99915 までで、その先は PLC が `E0` で断る。
- エラーは `KvError`。`is_connection_fatal()` が `true`（`Framing`・`Timeout`・`NotConnected`・`Io`）
  なら、そのクライアントは毒化済みで以後 `NotConnected` しか返さない。作り直すこと。
  `Device`（PLC の E コード）と `InvalidRequest`（送る前に断った）は非致命。
- 1 つのクライアントで同時に飛ぶ要求は 1 つ（`&mut self`）。複数の利用者での共有・再接続は
  呼び出し側の責務。
- feature `mock` でテスト用のモック TCP サーバー（`kv_link::mock::MockServer`、障害注入・
  要求の記録つき）を使える。
- ビットデバイスをワード形式で読み書きすること、XYM 表記、`DM`・`EM`・`R`・`MR`・`LR` 以外のデバイスは
  v0 では扱わない。

設計の詳細は `src/lib.rs` のモジュール文書（`cargo doc --open --all-features`）。

## ライセンス

MIT
