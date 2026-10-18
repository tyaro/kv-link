//! 構造化エラー [`KvError`] と、PLC が返したエラーコード [`DeviceErrorCode`]。
//!
//! 分類の根拠は lib.rs のモジュール文書「エラーの分類」を参照。要点は
//! **「バイト列がまだ要求の境界に揃っているか」** で、揃っていれば非致命
//! （`Device` / `InvalidRequest`）、揃っている保証がなければ接続致命
//! （`Framing` / `Timeout` / `NotConnected` / `Io`）。

use std::fmt;

use crate::protocol;

/// PLC がエラー応答（`E` + 数字 1 桁）で返したコード。
///
/// 既知のコード（マニュアル 13-32 の一覧: `E0`・`E1`・`E2`・`E4`・`E5`・`E6`）は
/// 名前付きの variant に、一覧にない 1 桁のコード（`E3`・`E7`〜`E9`）は生の
/// 文字列のまま [`DeviceErrorCode::Unknown`] に入れる（知らないコードが来ても
/// フレーミング異常にはしない: 完全な 1 行が届いている以上、ストリームは
/// 揃ったままだから）。意味は docs/protocol.md P-15〜P-21。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DeviceErrorCode {
    /// `E0` デバイス番号異常。
    DeviceNumber,
    /// `E1` コマンド異常。
    Command,
    /// `E2` プログラム未登録。
    ProgramNotRegistered,
    /// `E4` 書き込み禁止。
    WriteProhibited,
    /// `E5` 本体エラー（CPU ユニットのエラーが解除されていない）。
    CpuError,
    /// `E6` コメントなし（コメントが登録されていない）。
    NoComment,
    /// 上の一覧にない 1 桁のコード。`E` を含む生の文字列（例 `"E9"`）。
    Unknown(String),
}

impl DeviceErrorCode {
    /// 応答 1 行（終端を除いたもの）がエラー応答なら、そのコードを返す。
    ///
    /// 文法は `E` + 数字ちょうど [`protocol::MAX_ERROR_CODE_DIGITS`] 桁（1 桁）、
    /// それ以外の文字を含まないこと（`E10` はエラー応答ではない）。エラー応答でなければ `None`（値や `OK` として
    /// 呼び出し側が続けて解釈する）。
    pub fn parse(line: &str) -> Option<DeviceErrorCode> {
        let digits = line.strip_prefix(protocol::ERROR_PREFIX)?;
        if digits.is_empty()
            || digits.len() > protocol::MAX_ERROR_CODE_DIGITS
            || !digits.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        Some(match line {
            protocol::ERR_DEVICE_NUMBER => DeviceErrorCode::DeviceNumber,
            protocol::ERR_COMMAND => DeviceErrorCode::Command,
            protocol::ERR_PROGRAM_NOT_REGISTERED => DeviceErrorCode::ProgramNotRegistered,
            protocol::ERR_WRITE_PROHIBITED => DeviceErrorCode::WriteProhibited,
            protocol::ERR_CPU_ERROR => DeviceErrorCode::CpuError,
            protocol::ERR_NO_COMMENT => DeviceErrorCode::NoComment,
            other => DeviceErrorCode::Unknown(other.to_owned()),
        })
    }

    /// ワイヤ上の表記（`"E0"` など）。
    pub fn as_str(&self) -> &str {
        match self {
            DeviceErrorCode::DeviceNumber => protocol::ERR_DEVICE_NUMBER,
            DeviceErrorCode::Command => protocol::ERR_COMMAND,
            DeviceErrorCode::ProgramNotRegistered => protocol::ERR_PROGRAM_NOT_REGISTERED,
            DeviceErrorCode::WriteProhibited => protocol::ERR_WRITE_PROHIBITED,
            DeviceErrorCode::CpuError => protocol::ERR_CPU_ERROR,
            DeviceErrorCode::NoComment => protocol::ERR_NO_COMMENT,
            DeviceErrorCode::Unknown(raw) => raw,
        }
    }

    /// 人が読む短い説明（英語。ログ・UI 用）。
    pub fn description(&self) -> &'static str {
        match self {
            DeviceErrorCode::DeviceNumber => "device number error",
            DeviceErrorCode::Command => "command error",
            DeviceErrorCode::ProgramNotRegistered => "program not registered",
            DeviceErrorCode::WriteProhibited => "write prohibited",
            DeviceErrorCode::CpuError => "CPU unit error not cleared",
            DeviceErrorCode::NoComment => "comment not registered",
            DeviceErrorCode::Unknown(_) => "unknown error code",
        }
    }
}

impl fmt::Display for DeviceErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.as_str(), self.description())
    }
}

/// kv-link のすべての失敗。
///
/// `#[non_exhaustive]` を **付けていない** のは意図的: 利用側（banto のアダプター）が
/// 致命/非致命を構造的な `match` で網羅し、variant が増えたらコンパイルエラーで
/// 気付けるようにするため（メッセージ文字列のパースで分類しない、SLMP の H9 の教訓）。
/// variant を足すのは破壊的変更として版を上げる。
#[derive(Debug, thiserror::Error)]
pub enum KvError {
    /// PLC がエラーコードを返した。完全な 1 行（終端まで）を受け取っており
    /// ストリームは揃ったまま → **非致命**。同じクライアントで続けてよい。
    #[error("PLC returned error {code}")]
    Device {
        /// PLC が返したコード。
        code: DeviceErrorCode,
    },

    /// 送る前に断った要求（アドレスの文法違反、型とデバイスの組み合わせ違い、
    /// 点数 0、形式の範囲外の値、応答が上限に収まらない点数など）。
    /// 何も送っていない → **非致命**。
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// 応答の形がおかしい（値の個数違い、数値でない、終端が上限内に来ない、
    /// 終端の後ろに余分なバイト、値を期待したのに `OK` など）。どこまでが
    /// この要求への応答か信用できない → **接続致命**。クライアントは毒化する。
    #[error("malformed response: {0}")]
    Framing(String),

    /// 応答が時間内に揃わなかった（接続時は接続タイムアウト）。遅れて届く応答を
    /// 次の要求の応答と取り違えないよう → **接続致命**。クライアントは毒化する。
    ///
    /// 書き込み（`WR`・`WRS`）で、要求を送った後に接続致命エラー（これ、
    /// [`KvError::Io`]、[`KvError::Framing`]。例: 壊れた応答・余分なバイト・
    /// 終端なし）が返ったときは **結果不明**: PLC が書き込んだかどうかは
    /// 分からない。成功扱いにせず、このクレートは再送もしない（再送するかは利用側が判断する。
    /// tyaro/banto-industrial#438 の「結果不明の書き込みを無条件に再送しない」）。
    /// なお [`KvError::InvalidRequest`] と [`KvError::NotConnected`] は
    /// 何も送っていないので、書き込みは行われていない。
    #[error("timed out waiting for the PLC")]
    Timeout,

    /// 接続していない、または以前の致命エラーで毒化済み。ネットワークには
    /// 触れていない → **接続致命**（新しいクライアントを作る必要がある）。
    #[error("not connected (the client was closed or poisoned by an earlier fatal error)")]
    NotConnected,

    /// それ以外の I/O 失敗（接続拒否・リセット・相手の切断など）→ **接続致命**。
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl KvError {
    /// この失敗の後、同じ接続（同じ [`crate::KvClient`]）を使い続けられないか。
    ///
    /// banto broker の「`Err` = 接続致命」の契約にそのまま対応させるための関数:
    /// `true` なら broker はセッションを捨てて再接続する、`false` なら
    /// その要求だけを失敗（品質 Bad など）にして続ける。
    ///
    /// | variant          | 致命 | 理由                                   |
    /// | ---------------- | ---- | -------------------------------------- |
    /// | `Device`         | いいえ | 完全な 1 行を受信済み、ストリームは揃ったまま |
    /// | `InvalidRequest` | いいえ | 送信していない                         |
    /// | `Framing`        | はい | 応答の境界が信用できない               |
    /// | `Timeout`        | はい | 遅れた応答が後から届きうる             |
    /// | `NotConnected`   | はい | 接続がない（毒化済み）                 |
    /// | `Io`             | はい | 接続が壊れた                           |
    pub fn is_connection_fatal(&self) -> bool {
        match self {
            KvError::Device { .. } | KvError::InvalidRequest(_) => false,
            KvError::Framing(_) | KvError::Timeout | KvError::NotConnected | KvError::Io(_) => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_error_codes_parse_to_named_variants() {
        assert_eq!(
            DeviceErrorCode::parse("E0"),
            Some(DeviceErrorCode::DeviceNumber)
        );
        assert_eq!(DeviceErrorCode::parse("E1"), Some(DeviceErrorCode::Command));
        assert_eq!(
            DeviceErrorCode::parse("E2"),
            Some(DeviceErrorCode::ProgramNotRegistered)
        );
        assert_eq!(
            DeviceErrorCode::parse("E4"),
            Some(DeviceErrorCode::WriteProhibited)
        );
        assert_eq!(
            DeviceErrorCode::parse("E5"),
            Some(DeviceErrorCode::CpuError)
        );
        assert_eq!(
            DeviceErrorCode::parse("E6"),
            Some(DeviceErrorCode::NoComment)
        );
    }

    #[test]
    fn unknown_error_codes_keep_the_raw_code() {
        for raw in ["E3", "E7", "E8", "E9"] {
            assert_eq!(
                DeviceErrorCode::parse(raw),
                Some(DeviceErrorCode::Unknown(raw.into()))
            );
            assert_eq!(DeviceErrorCode::parse(raw).unwrap().as_str(), raw);
        }
    }

    #[test]
    fn non_error_lines_are_not_error_codes() {
        for line in [
            "", "E", "OK", "E1A", "e1", " E1", "E1 ", "E10", "E99", "E00", "E100", "00001", "E-1",
            "E+1",
        ] {
            assert_eq!(DeviceErrorCode::parse(line), None, "{line:?}");
        }
    }

    #[test]
    fn round_trip_as_str() {
        for code in ["E0", "E1", "E2", "E4", "E5", "E6", "E7"] {
            assert_eq!(DeviceErrorCode::parse(code).unwrap().as_str(), code);
        }
    }

    #[test]
    fn fatality_classification() {
        assert!(!KvError::Device {
            code: DeviceErrorCode::Command
        }
        .is_connection_fatal());
        assert!(!KvError::InvalidRequest("x".into()).is_connection_fatal());
        assert!(KvError::Framing("x".into()).is_connection_fatal());
        assert!(KvError::Timeout.is_connection_fatal());
        assert!(KvError::NotConnected.is_connection_fatal());
        assert!(KvError::Io(std::io::Error::other("x")).is_connection_fatal());
    }
}
