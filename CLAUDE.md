# kv-link プロジェクト規約（Claude 向け）

KEYENCE KV シリーズ PLC の上位リンク通信（Ethernet / TCP）クライアントライブラリ（Rust、tokio 非同期）。
banto-industrial の banto-hub から KV シリーズへつなぐワイヤ層（tyaro/banto-industrial#438）。

## 境界

境界は banto-industrial の
[docs/tag-server-design.md §3.4.1](https://github.com/tyaro/banto-industrial/blob/main/docs/tag-server-design.md)
のオーナー決定（2026-10-18）で決まっている。

- **このクレートが持つ**: フレームの組み立てと応答解析、アドレス文法、型変換、非同期 TCP クライアント
  （分割受信・終端・タイムアウト・遅延応答を次の要求に流用しない＝毒化）、構造化エラー、
  テスト用モック TCP サーバー（feature `mock`）。
- **持たない（banto 側の責務）**: 単一セッションの直列化、再接続ループ、品質フラグ、
  シミュレーション拒否。
- **banto のどのクレートにも依存しない。**
- エラーは構造化したまま保つ（`KvError` の致命/非致命の分類を文字列パースに頼らせない）。
  分類の根拠は `src/lib.rs` のモジュール文書。

## プロトコルの事実

- コマンド書式・終端・応答の形・エラーコード・ビット番号の文法などは **公式マニュアルとの照合が必要**。
  **[docs/protocol.md](docs/protocol.md) が唯一の正**で、コード側の定数は `src/protocol.rs` に集める
  （他のモジュールにプロトコルのリテラルを書かない）。
- 照合前の事実を「対応済み」と書かない。事実を足す・変えるときは docs/protocol.md の表に行を足し、
  `src/protocol.rs` の該当定数に `[P-xx]` を付ける。
- 範囲・最大点数など未確認の値でハードな検査を足さない（照合後に足す）。

## 開発規約

- 検証: `cargo fmt --check` / `cargo clippy --all-targets --all-features -- -D warnings` /
  `cargo test --all-features`（doctest を含む） / `cargo build`（feature なし） / `cargo build --features mock` /
  `RUSTDOCFLAGS="-D warnings" cargo doc --all-features --no-deps`。
- コミットメッセージは日本語・Conventional Commits 風（例: `feat: RDS の応答解析を追加`）。
- 設計意図はクレートの `lib.rs` 冒頭やモジュール文書に書く（banto-industrial と同じ流儀）。
  コメントは「なぜ」を書く。
- banto-industrial は git タグでこのクレートを参照する（`deny.toml` の `required-git-spec = "tag"`）。
  公開 API の破壊的変更（`KvError` の variant 追加を含む）は版を上げてタグを打つ。
  `KvError`・`DeviceType`・`DeviceErrorCode` は意図的に `#[non_exhaustive]` にしていない
  （banto のアダプターの `match` が網羅であり続け、variant を足すとコンパイルで壊れて
  気づけるように）。デバイス（T・C・W・B など）やエラーコードの追加も同じく破壊的変更で、
  版を上げてタグを打つ。
