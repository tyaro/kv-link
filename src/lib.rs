//! # kv-link
//!
//! KEYENCE KV シリーズ PLC の **上位リンク通信**（Ethernet / TCP）クライアント。
//! tokio の非同期 I/O で動く。
//!
//! banto-industrial の banto-hub から KV シリーズに接続するためのワイヤ層として
//! 作った（tyaro/banto-industrial#438）。banto には依存しない単独のライブラリで、
//! banto 側は `banto-plc` / `banto-plc-write` / `banto-broker` の薄いアダプターから
//! これを使う（SLMP を `tyaro/slmp` の git タグで使っているのと同じ形）。
//!
//! > **プロトコルの事実は公式マニュアルと照合済み、実機では未確認。** コマンドの
//! > 書式・終端・応答の形・エラーコード・ビット番号の文法・デバイス範囲・点数上限は
//! > 「KV-XLE02 ユーザーズマニュアル」（AS_166260_KV-XLE02_UM_084A11_KJ_JP_2036_1）
//! > 13 章と照合し、[`protocol`] モジュールと `docs/protocol.md` の表に集めてある。
//! > 実機での確認はまだ 1 つもしていない（`docs/protocol.md`「実機確認」）。
//! > マニュアルは KV-XLE02 のもので、KV-8000 の内蔵ポートや KV-LE21V などでも
//! > 同じと仮定しているが未照合。
//!
//! ## 境界（何を持ち、何を持たないか）
//!
//! 境界は banto-industrial の `docs/tag-server-design.md` §3.4.1 のオーナー決定
//! （2026-10-18）で決まっている。
//!
//! | このクレートが持つ | banto 側が持つ（ここには入れない） |
//! | --- | --- |
//! | コマンドの組み立てと応答の解析（[`protocol`]・`codec`） | 単一セッションの直列化（複数の利用者からの要求の順番待ち） |
//! | アドレス文法の解析（[`Address`]） | 再接続のループとバックオフ |
//! | 型変換（[`KvValue`]: `bool`・`u16`・`i16`・`u32`・`i32`・`f32`） | 品質フラグ（Good/Bad）、タグとの対応付け |
//! | 非同期 TCP クライアント（[`KvClient`]: 分割受信・終端・タイムアウト・毒化） | シミュレーション拒否・全シミュレーション運転時の防御 |
//! | 構造化エラー（[`KvError`]） | エラーを broker の契約へ写すアダプター |
//! | テスト用モック TCP サーバー（feature `mock`、[`mock`]） | Hub の設定・UI・DB |
//!
//! ## エラーの分類（broker の「`Err` = 接続致命」との対応）
//!
//! SLMP では最初エラーを `std::io::Error` 一本で返していたため、「PLC がこの要求を
//! 断った（続けてよい）」と「フレームが壊れた（接続を捨てるべき）」をメッセージ
//! 文字列のパースで見分けるしかなく、後から構造化し直す手戻りになった
//! （banto-industrial `docs/h9-slmp-structured-error-spec.md`）。そのため
//! [`KvError`] は最初の版から構造化してあり、[`KvError::is_connection_fatal`] で
//! 判定できる。判断の軸は **「バイト列がまだ要求の境界に揃っているか」**:
//!
//! | variant | 致命 | 根拠 | banto broker での扱い（想定） |
//! | --- | --- | --- | --- |
//! | [`KvError::Device`] | いいえ | `E` + 数字の完全な 1 行を終端まで受信済み | その要求だけ Bad、セッション継続 |
//! | [`KvError::InvalidRequest`] | いいえ | 送信していない（範囲・点数・奇数番地の 32 bit など） | その要求だけ失敗、セッション継続 |
//! | [`KvError::Framing`] | はい | 応答の境界・中身が信用できない | `Err`（接続致命）→ 再接続 |
//! | [`KvError::Timeout`] | はい | 遅れた応答が後から届きうる | `Err`（接続致命）→ 再接続 |
//! | [`KvError::NotConnected`] | はい | 毒化済み・未接続 | `Err`（接続致命）→ 再接続 |
//! | [`KvError::Io`] | はい | 接続が壊れた | `Err`（接続致命）→ 再接続 |
//!
//! [`KvError`] に `#[non_exhaustive]` を付けていないのは、アダプターの `match` を
//! 網羅的にして、variant が増えたらコンパイルエラーで気付けるようにするため。
//!
//! ## 1 要求ずつ・毒化
//!
//! 上位リンク通信の応答には要求との対応を示す番号（Modbus TCP の transaction id
//! のようなもの）が無く、「次に届いた 1 行がいま送った要求の応答」という順序
//! だけが頼りになる。したがって:
//!
//! - **同時に飛ぶ要求は常に 1 つ。** 要求系メソッドはすべて `&mut self` を取るので、
//!   1 つの [`KvClient`] で要求が重なることは型で起きない。複数の利用者で 1 本の
//!   接続を共有するための直列化は banto 側の責務。
//! - **致命エラーの後は毒化する。** タイムアウトした要求の応答は後から届きうる。
//!   それを次の要求の応答として読むと、別のアドレスの値を正しい値として返して
//!   しまう（産業用途で最も避けたい種類の誤り）。そこで `Timeout`・`Framing`・`Io`
//!   の後はソケットを捨て、以後の呼び出しはネットワークに触れずに
//!   [`KvError::NotConnected`] を返す。続けるには新しいクライアントを作る。
//!   タイムアウトで読み出しの途中の future が捨てられても、そのストリームを
//!   二度と使わないので安全。
//! - **余分なバイトはずれの証拠。** 終端 `\r\n` の後ろに同じ読み出しで続くバイト、
//!   および要求を送る直前に届いている頼んでいないバイトは `Framing`（毒化）。
//! - **`Device` は毒化しない。** 完全な 1 行を受け取っているので、ストリームは
//!   揃ったまま。
//!
//! 応答は終端まで最大 [`client::MAX_RESPONSE_BYTES`] バイト読む（理由はその定数の
//! 文書）。TCP は 1 つの応答を複数のセグメントに分けて届けうるので、終端が
//! 来るまで読み足す。
//!
//! ## 使い方
//!
//! ```no_run
//! use kv_link::{Address, KvClient, KvConfig, KvError};
//!
//! # async fn demo() -> Result<(), KvError> {
//! let mut plc = KvClient::connect("192.168.0.10:8501", KvConfig::default()).await?;
//!
//! let dm100: Address = "DM100".parse()?;
//! let value: u16 = plc.read(&dm100).await?;
//! let block: Vec<i32> = plc.read_many(&"DM200".parse()?, 4).await?;
//! plc.write::<bool>(&"MR1015".parse()?, true).await?;
//!
//! // 範囲外の番号（"DM65535"）はそもそも Address にできない。範囲内でも、
//! // 機種によっては PLC が E0 で断ることがある（古い KV-7000 の R100000 以降など）。
//! match plc.read::<bool>(&"R150000".parse()?).await {
//!     Ok(v) => println!("{v}"),
//!     // PLC が断った（または送る前に断った）: この要求だけの失敗。plc は使い続けてよい。
//!     Err(e) if !e.is_connection_fatal() => eprintln!("rejected: {e}"),
//!     // 接続致命: plc は毒化済み。作り直す（再接続の方針は呼び出し側が決める）。
//!     Err(e) => return Err(e),
//! }
//! # let _ = (value, block);
//! # Ok(())
//! # }
//! ```
//!
//! ポート番号 8501 はユニットの初期設定（変更可能、docs/protocol.md P-27）。
//! ライブラリは既定ポートを持たず、呼び出し側が渡す。
//!
//! ## 32 bit 値と奇数番地
//!
//! `u32`・`i32`・`f32` は `.D`/`.L` で 2 ワードを 1 要求で読み書きする（指定番号が
//! 下位ワード、次の番号が上位ワード）。マニュアル（13-33）は上下 2 ワードの
//! 同時性を **偶数番地の場合にしか保証しない** ので、v0 は奇数番地から始まる
//! 32 bit アクセスを送らずに [`KvError::InvalidRequest`] で断る
//! （[`codec::REJECT_NON_SIMULTANEOUS_32BIT`]）。
//!
//! ## まだ無いもの
//!
//! - `DM`・`EM`・`R`・`MR`・`LR` 以外のデバイス、XYM 表記。
//! - ビットデバイスをワード形式（`.U` など）で読み書きすること（プロトコル上は可、
//!   v0 はライブラリの制限として扱わない）。
//! - 奇数番地の 32 bit アクセスのオプトイン。

#![warn(missing_docs)]

pub mod address;
pub mod client;
pub mod codec;
pub mod error;
#[cfg(any(test, feature = "mock"))]
pub mod mock;
pub mod protocol;

pub use address::{Address, AddressError, DeviceType};
pub use client::{KvClient, KvConfig};
pub use codec::KvValue;
pub use error::{DeviceErrorCode, KvError};
pub use protocol::DataFormat;
