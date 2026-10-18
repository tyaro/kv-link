//! コマンドの組み立てと応答の解析（ネットワークに触れない純粋関数）。
//!
//! 型付き API の型（`bool`・`u16`・`i16`・`u32`・`i32`）は [`KvValue`] で
//! 表す。どの型がどのデータ形式（`.U` など）に対応し、応答の 1 トークンを
//! どう解釈するかはここに閉じる。トレイトは封印してあり（sealed）、型を
//! 足すのはこのクレートだけ（f32 とワード順はマニュアル照合まで保留、
//! docs/protocol.md P-26）。
//!
//! 応答の解析は **厳密** にする: 値の個数違い・空トークン（空白の連続や
//! 前後の空白）・形式の範囲外は、どれも「応答がこの要求のものか信用できない」
//! として [`KvError::Framing`]（接続致命）にする。

use crate::address::Address;
use crate::error::{DeviceErrorCode, KvError};
use crate::protocol::{self, DataFormat};

mod sealed {
    use crate::protocol::DataFormat;

    /// [`super::KvValue`] の実体。外から実装・呼び出しできないよう非公開
    /// モジュールに置く。
    pub trait Codec: Sized + Copy + Send + Sync + 'static {
        /// 型名（エラーメッセージ用）。
        const NAME: &'static str;
        /// `None` = ビットデバイス用（形式を付けない）。
        const FORMAT: Option<DataFormat>;
        /// 応答 1 トークンの最大長（符号・ゼロ詰め込み）。応答が上限バイト数に
        /// 収まるかの事前検査に使う。
        const MAX_TOKEN_LEN: usize;
        fn parse_token(token: &str) -> Option<Self>;
        fn encode(self) -> String;
    }
}

/// 型付き API で読み書きできる値の型（`bool`・`u16`・`i16`・`u32`・`i32`）。
///
/// `bool` はビットデバイス（`R`・`MR`・`LR`）専用、数値型はワードデバイス
/// （`DM`・`EM`）専用。組み合わせが違えば送る前に
/// [`KvError::InvalidRequest`] になる。
pub trait KvValue: sealed::Codec {}

impl<T: sealed::Codec> KvValue for T {}

/// 符号なし 10 進（数字のみ）。Rust の `from_str` は先頭 `+` を許すので先に見る。
fn parse_unsigned<T: std::str::FromStr>(token: &str) -> Option<T> {
    if token.is_empty() || !token.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    token.parse().ok()
}

/// 符号付き 10 進（任意の `+`/`-` の後に数字 1 桁以上）。
fn parse_signed<T: std::str::FromStr>(token: &str) -> Option<T> {
    let digits = token
        .strip_prefix('+')
        .or_else(|| token.strip_prefix('-'))
        .unwrap_or(token);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    token.parse().ok()
}

impl sealed::Codec for bool {
    const NAME: &'static str = "bool";
    const FORMAT: Option<DataFormat> = None;
    const MAX_TOKEN_LEN: usize = 1;
    fn parse_token(token: &str) -> Option<bool> {
        match token {
            "0" => Some(false),
            "1" => Some(true),
            _ => None,
        }
    }
    fn encode(self) -> String {
        if self { "1" } else { "0" }.to_owned()
    }
}

impl sealed::Codec for u16 {
    const NAME: &'static str = "u16";
    const FORMAT: Option<DataFormat> = Some(DataFormat::U16);
    const MAX_TOKEN_LEN: usize = 5;
    fn parse_token(token: &str) -> Option<u16> {
        parse_unsigned(token)
    }
    fn encode(self) -> String {
        self.to_string()
    }
}

impl sealed::Codec for i16 {
    const NAME: &'static str = "i16";
    const FORMAT: Option<DataFormat> = Some(DataFormat::S16);
    const MAX_TOKEN_LEN: usize = 6;
    fn parse_token(token: &str) -> Option<i16> {
        parse_signed(token)
    }
    fn encode(self) -> String {
        self.to_string()
    }
}

impl sealed::Codec for u32 {
    const NAME: &'static str = "u32";
    const FORMAT: Option<DataFormat> = Some(DataFormat::U32);
    const MAX_TOKEN_LEN: usize = 10;
    fn parse_token(token: &str) -> Option<u32> {
        parse_unsigned(token)
    }
    fn encode(self) -> String {
        self.to_string()
    }
}

impl sealed::Codec for i32 {
    const NAME: &'static str = "i32";
    const FORMAT: Option<DataFormat> = Some(DataFormat::S32);
    const MAX_TOKEN_LEN: usize = 11;
    fn parse_token(token: &str) -> Option<i32> {
        parse_signed(token)
    }
    fn encode(self) -> String {
        self.to_string()
    }
}

/// `.H` の 1 トークン（16 進 1〜4 桁）。型付き API では使わないが、モックと
/// `raw_command` の利用者向けに置く。
pub fn parse_hex16(token: &str) -> Option<u16> {
    if token.is_empty() || token.len() > 4 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u16::from_str_radix(token, 16).ok()
}

/// 型とデバイスの組み合わせを検査し、コマンドに書くデバイス部
/// （`DM100.U`・`R1015`）を返す。
pub(crate) fn device_operand<T: KvValue>(addr: &Address) -> Result<String, KvError> {
    match (T::FORMAT, addr.device().is_bit()) {
        (None, true) => Ok(addr.to_string()),
        (Some(fmt), false) => Ok(format!(
            "{addr}{}{}",
            protocol::FORMAT_DELIMITER,
            fmt.suffix()
        )),
        (None, false) => Err(KvError::InvalidRequest(format!(
            "{} can only be used with bit devices (R, MR, LR), got {addr}",
            T::NAME
        ))),
        (Some(_), true) => Err(KvError::InvalidRequest(format!(
            "{} can only be used with word devices (DM, EM), got {addr}",
            T::NAME
        ))),
    }
}

/// `count` 点の読み出し応答が `max_line_len` バイト（終端込み）に収まるか。
/// 収まらない要求を送ると、正しい応答でも上限で `Framing` になり接続を
/// 失うので、送る前に `InvalidRequest` で断る。
pub(crate) fn check_response_fits<T: KvValue>(
    count: usize,
    max_line_len: usize,
) -> Result<(), KvError> {
    let worst = count
        .checked_mul(T::MAX_TOKEN_LEN + 1)
        .map(|n| n + protocol::RESPONSE_TERMINATOR.len());
    match worst {
        Some(n) if n <= max_line_len => Ok(()),
        _ => Err(KvError::InvalidRequest(format!(
            "reading {count} x {} may exceed the {max_line_len}-byte response limit; split the request",
            T::NAME
        ))),
    }
}

/// `RD <dev>[.fmt]`
pub(crate) fn build_read<T: KvValue>(addr: &Address) -> Result<String, KvError> {
    Ok(format!(
        "{}{}{}",
        protocol::CMD_READ,
        protocol::SEPARATOR,
        device_operand::<T>(addr)?
    ))
}

/// `RDS <dev>[.fmt] <n>`
pub(crate) fn build_read_many<T: KvValue>(addr: &Address, count: usize) -> Result<String, KvError> {
    if count == 0 {
        return Err(KvError::InvalidRequest("count must be at least 1".into()));
    }
    let sep = protocol::SEPARATOR;
    Ok(format!(
        "{}{sep}{}{sep}{count}",
        protocol::CMD_READ_CONSECUTIVE,
        device_operand::<T>(addr)?
    ))
}

/// `WR <dev>[.fmt] <v>`
pub(crate) fn build_write<T: KvValue>(addr: &Address, value: T) -> Result<String, KvError> {
    let sep = protocol::SEPARATOR;
    Ok(format!(
        "{}{sep}{}{sep}{}",
        protocol::CMD_WRITE,
        device_operand::<T>(addr)?,
        value.encode()
    ))
}

/// `WRS <dev>[.fmt] <n> <v1> ... <vn>`
pub(crate) fn build_write_many<T: KvValue>(
    addr: &Address,
    values: &[T],
) -> Result<String, KvError> {
    if values.is_empty() {
        return Err(KvError::InvalidRequest(
            "at least one value is required".into(),
        ));
    }
    let sep = protocol::SEPARATOR;
    let mut cmd = format!(
        "{}{sep}{}{sep}{}",
        protocol::CMD_WRITE_CONSECUTIVE,
        device_operand::<T>(addr)?,
        values.len()
    );
    for v in values {
        cmd.push(sep);
        cmd.push_str(&v.encode());
    }
    Ok(cmd)
}

/// 応答 1 行がエラー応答なら [`KvError::Device`] にする。
pub(crate) fn check_device_error(line: &str) -> Result<(), KvError> {
    match DeviceErrorCode::parse(line) {
        Some(code) => Err(KvError::Device { code }),
        None => Ok(()),
    }
}

/// 読み出し応答（終端を除いた 1 行）を `expected` 個の値として解析する。
/// エラー応答は `Device`、それ以外の不一致はすべて `Framing`。
pub fn parse_read_response<T: KvValue>(line: &str, expected: usize) -> Result<Vec<T>, KvError> {
    check_device_error(line)?;
    let mut values = Vec::with_capacity(expected);
    for token in line.split(protocol::SEPARATOR) {
        if values.len() == expected {
            return Err(KvError::Framing(format!(
                "expected {expected} value(s), got more: {line:?}"
            )));
        }
        let v = T::parse_token(token).ok_or_else(|| {
            KvError::Framing(format!(
                "{token:?} is not a valid {} value (response {line:?})",
                T::NAME
            ))
        })?;
        values.push(v);
    }
    if values.len() != expected {
        return Err(KvError::Framing(format!(
            "expected {expected} value(s), got {}: {line:?}",
            values.len()
        )));
    }
    Ok(values)
}

/// 書き込み応答（終端を除いた 1 行）を検査する。`OK` 以外でエラー応答でも
/// ないものは `Framing`。
pub fn parse_write_response(line: &str) -> Result<(), KvError> {
    check_device_error(line)?;
    if line == protocol::RESPONSE_OK {
        Ok(())
    } else {
        Err(KvError::Framing(format!(
            "expected {:?} for a write, got {line:?}",
            protocol::RESPONSE_OK
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> Address {
        s.parse().unwrap()
    }

    fn framing<T: KvValue + std::fmt::Debug>(line: &str, n: usize) {
        match parse_read_response::<T>(line, n) {
            Err(KvError::Framing(_)) => {}
            other => panic!("{line:?} as {}: expected Framing, got {other:?}", T::NAME),
        }
    }

    #[test]
    fn commands() {
        assert_eq!(build_read::<u16>(&addr("dm100")).unwrap(), "RD DM100.U");
        assert_eq!(build_read::<i16>(&addr("DM1")).unwrap(), "RD DM1.S");
        assert_eq!(build_read::<u32>(&addr("EM2")).unwrap(), "RD EM2.D");
        assert_eq!(build_read::<i32>(&addr("EM2")).unwrap(), "RD EM2.L");
        assert_eq!(build_read::<bool>(&addr("mr1015")).unwrap(), "RD MR1015");
        assert_eq!(
            build_read_many::<u16>(&addr("DM0"), 3).unwrap(),
            "RDS DM0.U 3"
        );
        assert_eq!(build_write::<i16>(&addr("DM0"), -5).unwrap(), "WR DM0.S -5");
        assert_eq!(build_write::<bool>(&addr("R5"), true).unwrap(), "WR R005 1");
        assert_eq!(
            build_write_many::<i32>(&addr("DM10"), &[1, -1]).unwrap(),
            "WRS DM10.L 2 1 -1"
        );
        assert_eq!(
            build_write_many::<bool>(&addr("LR0"), &[true, false, true]).unwrap(),
            "WRS LR000 3 1 0 1"
        );
    }

    #[test]
    fn type_device_mismatch_and_empty_counts_are_invalid_requests() {
        assert!(matches!(
            build_read::<u16>(&addr("R100")),
            Err(KvError::InvalidRequest(_))
        ));
        assert!(matches!(
            build_read::<bool>(&addr("DM100")),
            Err(KvError::InvalidRequest(_))
        ));
        assert!(matches!(
            build_read_many::<u16>(&addr("DM0"), 0),
            Err(KvError::InvalidRequest(_))
        ));
        assert!(matches!(
            build_write_many::<u16>(&addr("DM0"), &[]),
            Err(KvError::InvalidRequest(_))
        ));
    }

    #[test]
    fn response_size_precheck() {
        assert!(check_response_fits::<i32>(10, 16 * 1024).is_ok());
        assert!(check_response_fits::<i32>(100_000, 16 * 1024).is_err());
        assert!(check_response_fits::<u16>(usize::MAX, 16 * 1024).is_err());
        // 境界: 11 桁 + 空白 = 12 バイト/点、終端 2 バイト。
        assert!(check_response_fits::<i32>(10, 122).is_ok());
        assert!(check_response_fits::<i32>(10, 121).is_err());
    }

    #[test]
    fn u16_values() {
        assert_eq!(parse_read_response::<u16>("00000", 1).unwrap(), [0]);
        assert_eq!(parse_read_response::<u16>("65535", 1).unwrap(), [u16::MAX]);
        assert_eq!(
            parse_read_response::<u16>("00001 00002 65535", 3).unwrap(),
            [1, 2, 65535]
        );
        framing::<u16>("65536", 1);
        framing::<u16>("+1", 1);
        framing::<u16>("-1", 1);
        framing::<u16>("1A", 1);
    }

    #[test]
    fn i16_values() {
        assert_eq!(parse_read_response::<i16>("-32768", 1).unwrap(), [i16::MIN]);
        assert_eq!(parse_read_response::<i16>("+32767", 1).unwrap(), [i16::MAX]);
        assert_eq!(parse_read_response::<i16>("32767", 1).unwrap(), [i16::MAX]);
        assert_eq!(parse_read_response::<i16>("-00001", 1).unwrap(), [-1]);
        framing::<i16>("32768", 1);
        framing::<i16>("-32769", 1);
        framing::<i16>("+", 1);
        framing::<i16>("--1", 1);
        framing::<i16>("+-1", 1);
    }

    #[test]
    fn u32_values() {
        assert_eq!(
            parse_read_response::<u32>("4294967295", 1).unwrap(),
            [u32::MAX]
        );
        assert_eq!(parse_read_response::<u32>("0000000000", 1).unwrap(), [0]);
        framing::<u32>("4294967296", 1);
        framing::<u32>("-1", 1);
    }

    #[test]
    fn i32_values() {
        assert_eq!(
            parse_read_response::<i32>("-2147483648 +2147483647", 2).unwrap(),
            [i32::MIN, i32::MAX]
        );
        framing::<i32>("2147483648", 1);
        framing::<i32>("-2147483649", 1);
    }

    #[test]
    fn bool_values() {
        assert_eq!(
            parse_read_response::<bool>("1 0 1", 3).unwrap(),
            [true, false, true]
        );
        framing::<bool>("2", 1);
        framing::<bool>("00", 1);
        framing::<bool>("true", 1);
    }

    #[test]
    fn value_count_and_whitespace_are_strict() {
        framing::<u16>("1 2", 1);
        framing::<u16>("1", 2);
        framing::<u16>("", 1);
        framing::<u16>("1  2", 2);
        framing::<u16>(" 1", 1);
        framing::<u16>("1 ", 1);
        framing::<u16>("OK", 1);
    }

    #[test]
    fn error_responses_become_device_errors() {
        match parse_read_response::<u16>("E0", 1) {
            Err(KvError::Device { code }) => assert_eq!(code, DeviceErrorCode::DeviceNumber),
            other => panic!("{other:?}"),
        }
        match parse_write_response("E4") {
            Err(KvError::Device { code }) => assert_eq!(code, DeviceErrorCode::WriteProhibited),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn write_response() {
        assert!(parse_write_response("OK").is_ok());
        for line in ["", "ok", "OK ", "00001", "OKOK"] {
            assert!(
                matches!(parse_write_response(line), Err(KvError::Framing(_))),
                "{line:?}"
            );
        }
    }

    #[test]
    fn hex16() {
        assert_eq!(parse_hex16("FFFF"), Some(0xFFFF));
        assert_eq!(parse_hex16("00a0"), Some(0xA0));
        assert_eq!(parse_hex16("10000"), None);
        assert_eq!(parse_hex16(""), None);
        assert_eq!(parse_hex16("+1"), None);
    }
}
