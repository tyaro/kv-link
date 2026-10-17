# kv-link

KEYENCE KV シリーズ PLC の上位リンク通信（Ethernet / TCP）クライアントライブラリ（Rust、tokio 非同期）。

banto-industrial の banto-hub から KV シリーズに接続するためのワイヤ層として作る
（[tyaro/banto-industrial#438](https://github.com/tyaro/banto-industrial/issues/438)、
置き場所の決定は banto-industrial の `docs/tag-server-design.md` §3.4.1）。
banto には依存しない、単独で使えるライブラリにする。

状態: **準備中**（コードはまだない）。コマンド・デバイス範囲・エラーコードは
キーエンス公式の通信マニュアルと照合してから確定する。照合前のものは確定扱いにしない。

## ライセンス

MIT
