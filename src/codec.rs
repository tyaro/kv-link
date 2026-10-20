//! コマンドの組み立てと応答の解析（ネットワークに触れない純粋関数）。
//!
//! 型付き API の型（`bool`・`u16`・`i16`・`u32`・`i32`・`f32`）は [`KvValue`] で
//! 表す。どの型がどのデータ形式（`.U` など）に対応し、応答の 1 トークンを
//! どう解釈するかはここに閉じる。トレイトは封印してあり（sealed）、型を
//! 足すのはこのクレートだけ。
//!
//! ## 送る前の検査（[`KvError::InvalidRequest`]、何も送らない）
//!
//! - 型とデバイスの組み合わせ（`bool` はビットデバイス、数値型はワードデバイス）。
//!   プロトコル上はビットデバイスも `.U`・`.S`・`.H` でワードとして読み書きできる
//!   が、v0 はライブラリの制限としてこの組み合わせに絞る（docs/protocol.md P-14）。
//! - 点数: ビット・16 bit 形式は 1〜1000、32 bit 形式は 1〜500（P-25）。
//! - 範囲: アクセスする最後のワード／ビットまでがデバイスの範囲に収まること
//!   （32 bit は 1 点 2 ワード、ビットはチャネルをまたいで数える。P-24）。
//! - 32 bit 形式（`u32`・`i32`・`f32`）の先頭番号は偶数であること
//!   （[`REJECT_NON_SIMULTANEOUS_32BIT`]、P-32）。
//!
//! ## 応答の解析は厳密に
//!
//! 値の個数違い・空トークン（空白の連続や前後の空白）・形式の範囲外・
//! 形式の固定幅より長いトークンは、どれも「応答がこの要求のものか信用できない」
//! として [`KvError::Framing`]（接続致命）にする。
//!
//! 幅について: マニュアル（P-08）では PLC は各形式を固定幅（ビット 1・`.U` 5・
//! `.S` 6・`.D` 10・`.L` 11・`.H` 4 バイト、符号付きは常に `+`/`-` 付き）で返す。
//! 解析は **固定幅より長いトークンを拒み**、**短いトークン（ゼロサプレスされた
//! 形）と符号付き形式での `+` の省略は受け付ける**。PLC が送るのは固定幅だけの
//! はずだが、短い形を拒んでも安全性は上がらず（値の意味は一意に決まる）、
//! KV-XLE02 以外のユニットや将来の版で詰め方が違った場合に接続を失うだけなので、
//! 寛容にしておく。長いトークンは別の形式・別の要求の応答である兆候なので拒む。

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
        /// 応答 1 トークンの最大長（PLC が返す固定幅）。これより長いトークンは
        /// `Framing`。
        const MAX_TOKEN_LEN: usize = match Self::FORMAT {
            Some(f) => f.token_width(),
            None => crate::protocol::BIT_TOKEN_WIDTH,
        };
        fn parse_token(token: &str) -> Option<Self>;
        fn encode(self) -> String;
    }
}

/// 型付き API で読み書きできる値の型（`bool`・`u16`・`i16`・`u32`・`i32`・`f32`）。
///
/// `bool` はビットデバイス（`R`・`MR`・`LR`）専用、数値型はワードデバイス
/// （`DM`・`EM`）専用。組み合わせが違えば送る前に
/// [`KvError::InvalidRequest`] になる。
///
/// | 型 | 形式 | 1 点のワード数 |
/// | --- | --- | --- |
/// | `bool` | なし（ビット） | — |
/// | `u16` | `.U` | 1 |
/// | `i16` | `.S` | 1 |
/// | `u32` | `.D` | 2（偶数番地から） |
/// | `i32` | `.L` | 2（偶数番地から） |
/// | `f32` | `.D`（ビット列） | 2（偶数番地から） |
///
/// `f32` は `.D` で 32 bit をそのまま読み、`f32::from_bits` で解釈する（書き込みは
/// `f32::to_bits` を `.D` の 10 進で送る）。下位ワードが指定したデバイス、上位
/// ワードが次のデバイス（マニュアル 13-33）。PLC がその 2 ワードに IEEE 754
/// 単精度を置いている（KV STUDIO の実数の標準形式）ことは仮定で、未照合
/// （docs/protocol.md P-26）。書き込みは NaN を含むどのビット列も断らない
/// （解釈するのはラダー側で、このクレートは値の意味に立ち入らない）。
pub trait KvValue: sealed::Codec {}

impl<T: sealed::Codec> KvValue for T {}

/// 32 bit 形式（`u32`・`i32`・`f32`）を奇数番地から読み書きする要求を、送る前に
/// 断るか（ライブラリの方針、v0 は `true`）。
///
/// マニュアル（13-33、docs/protocol.md P-32）は `.D`・`.L` の上位・下位ワードの
/// 同時性をデバイス番号が偶数の場合にしか保証しない。奇数番地からの 32 bit 値は
/// 上下で別の瞬間の値が混ざりうり、産業用途ではもっとも見つけにくい種類の誤りに
/// なるので、v0 は一律に断る。奇数番地が必要な利用者が出たら、この定数を
/// 変えるのではなく明示的なオプトイン（設定項目）を足して緩める。
pub const REJECT_NON_SIMULTANEOUS_32BIT: bool = true;

/// 型付き API の読み出し応答 1 行（終端込み）の最大バイト数。点数上限（P-25）と
/// 固定幅（P-08）から導く。`client::MAX_RESPONSE_BYTES` がこれ以上であることを
/// コンパイル時に検査している。
pub(crate) const MAX_TYPED_RESPONSE_BYTES: usize = {
    let mut worst = protocol::MAX_POINTS_16BIT * (protocol::BIT_TOKEN_WIDTH + 1);
    let mut i = 0;
    while i < DataFormat::ALL.len() {
        let f = DataFormat::ALL[i];
        let n = f.max_points() * (f.token_width() + 1);
        if n > worst {
            worst = n;
        }
        i += 1;
    }
    // n 個の値の間の空白は n - 1 個。
    worst - 1 + protocol::RESPONSE_TERMINATOR.len()
};

/// 符号なし 10 進（数字のみ、`max_len` 桁まで）。Rust の `from_str` は先頭 `+` を
/// 許すので先に見る。
fn parse_unsigned<T: std::str::FromStr>(token: &str, max_len: usize) -> Option<T> {
    if token.is_empty() || token.len() > max_len || !token.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    token.parse().ok()
}

/// 符号付き 10 進（任意の `+`/`-` の後に数字 1 桁以上、符号込みで `max_len` まで）。
fn parse_signed<T: std::str::FromStr>(token: &str, max_len: usize) -> Option<T> {
    if token.len() > max_len {
        return None;
    }
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
    fn parse_token(token: &str) -> Option<bool> {
        match token {
            protocol::BIT_OFF => Some(false),
            protocol::BIT_ON => Some(true),
            _ => None,
        }
    }
    fn encode(self) -> String {
        if self {
            protocol::BIT_ON
        } else {
            protocol::BIT_OFF
        }
        .to_owned()
    }
}

impl sealed::Codec for u16 {
    const NAME: &'static str = "u16";
    const FORMAT: Option<DataFormat> = Some(DataFormat::U16);
    fn parse_token(token: &str) -> Option<u16> {
        parse_unsigned(token, Self::MAX_TOKEN_LEN)
    }
    fn encode(self) -> String {
        self.to_string()
    }
}

impl sealed::Codec for i16 {
    const NAME: &'static str = "i16";
    const FORMAT: Option<DataFormat> = Some(DataFormat::S16);
    fn parse_token(token: &str) -> Option<i16> {
        parse_signed(token, Self::MAX_TOKEN_LEN)
    }
    fn encode(self) -> String {
        self.to_string()
    }
}

impl sealed::Codec for u32 {
    const NAME: &'static str = "u32";
    const FORMAT: Option<DataFormat> = Some(DataFormat::U32);
    fn parse_token(token: &str) -> Option<u32> {
        parse_unsigned(token, Self::MAX_TOKEN_LEN)
    }
    fn encode(self) -> String {
        self.to_string()
    }
}

impl sealed::Codec for i32 {
    const NAME: &'static str = "i32";
    const FORMAT: Option<DataFormat> = Some(DataFormat::S32);
    fn parse_token(token: &str) -> Option<i32> {
        parse_signed(token, Self::MAX_TOKEN_LEN)
    }
    fn encode(self) -> String {
        self.to_string()
    }
}

/// `f32` は `.D`（符号なし 32 bit）のビット列として運ぶ（[`KvValue`] の文書）。
impl sealed::Codec for f32 {
    const NAME: &'static str = "f32";
    const FORMAT: Option<DataFormat> = Some(DataFormat::U32);
    fn parse_token(token: &str) -> Option<f32> {
        parse_unsigned::<u32>(token, Self::MAX_TOKEN_LEN).map(f32::from_bits)
    }
    fn encode(self) -> String {
        self.to_bits().to_string()
    }
}

/// `.H` の 1 トークン（16 進 1〜4 桁）。型付き API では使わないが、モックと
/// `raw_command` の利用者向けに置く。
pub fn parse_hex16(token: &str) -> Option<u16> {
    if token.is_empty()
        || token.len() > DataFormat::Hex16.token_width()
        || !token.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    u16::from_str_radix(token, 16).ok()
}

/// 型とデバイスの組み合わせを検査し、コマンドに書くデバイス部
/// （`DM100.U`・`R1015`）を返す。
fn device_operand<T: KvValue>(addr: &Address) -> Result<String, KvError> {
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

/// 1 点が占めるワード数（ビットは 1 ビット = 1 として数える）。
fn units_per_point<T: KvValue>() -> u32 {
    T::FORMAT.map_or(1, DataFormat::words)
}

/// 1 要求あたりの最大点数。`[P-25]`
fn max_points<T: KvValue>() -> usize {
    T::FORMAT.map_or(protocol::MAX_POINTS_16BIT, DataFormat::max_points)
}

/// `addr` から `count` 点の `T` へのアクセスを送る前に検査し、コマンドに書く
/// デバイス部を返す（モジュール文書「送る前の検査」）。
pub(crate) fn check_access<T: KvValue>(addr: &Address, count: usize) -> Result<String, KvError> {
    let operand = device_operand::<T>(addr)?;
    let max = max_points::<T>();
    if count == 0 || count > max {
        return Err(KvError::InvalidRequest(format!(
            "{} access needs 1..={max} point(s) per request, got {count}; split the request",
            T::NAME
        )));
    }
    let units = units_per_point::<T>();
    if REJECT_NON_SIMULTANEOUS_32BIT
        && units == 2
        && !protocol::is_32bit_simultaneous(addr.number())
    {
        return Err(KvError::InvalidRequest(format!(
            "{} access must start at an even device number, got {addr}: the KEYENCE manual \
             (KV-XLE02 UM 13-33) guarantees that the low and high words of a 32-bit value \
             are taken at the same instant only for even device numbers",
            T::NAME
        )));
    }
    // count <= 1000、units <= 2 なので u32 に収まる。
    let span = count as u32 * units;
    if addr.offset(span - 1).is_none() {
        return Err(KvError::InvalidRequest(format!(
            "{count} x {} from {addr} runs past the last {} device number ({})",
            T::NAME,
            addr.device(),
            addr.device().max_number()
        )));
    }
    Ok(operand)
}

/// `RD <dev>[.fmt]`
pub(crate) fn build_read<T: KvValue>(addr: &Address) -> Result<String, KvError> {
    Ok(format!(
        "{}{}{}",
        protocol::CMD_READ,
        protocol::SEPARATOR,
        check_access::<T>(addr, 1)?
    ))
}

/// `RDS <dev>[.fmt] <n>`
pub(crate) fn build_read_many<T: KvValue>(addr: &Address, count: usize) -> Result<String, KvError> {
    let sep = protocol::SEPARATOR;
    Ok(format!(
        "{}{sep}{}{sep}{count}",
        protocol::CMD_READ_CONSECUTIVE,
        check_access::<T>(addr, count)?
    ))
}

/// `WR <dev>[.fmt] <v>`
pub(crate) fn build_write<T: KvValue>(addr: &Address, value: T) -> Result<String, KvError> {
    let sep = protocol::SEPARATOR;
    Ok(format!(
        "{}{sep}{}{sep}{}",
        protocol::CMD_WRITE,
        check_access::<T>(addr, 1)?,
        value.encode()
    ))
}

/// `WRS <dev>[.fmt] <n> <v1> ... <vn>`
pub(crate) fn build_write_many<T: KvValue>(
    addr: &Address,
    values: &[T],
) -> Result<String, KvError> {
    let sep = protocol::SEPARATOR;
    let mut cmd = format!(
        "{}{sep}{}{sep}{}",
        protocol::CMD_WRITE_CONSECUTIVE,
        check_access::<T>(addr, values.len())?,
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
    use crate::codec::sealed::Codec;

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

    fn invalid<T>(r: Result<T, KvError>) -> String {
        match r {
            Err(KvError::InvalidRequest(m)) => m,
            Err(e) => panic!("expected InvalidRequest, got {e:?}"),
            Ok(_) => panic!("expected InvalidRequest, got Ok"),
        }
    }

    #[test]
    fn max_token_len_matches_manual_widths() {
        assert_eq!(<bool as Codec>::MAX_TOKEN_LEN, 1);
        assert_eq!(<u16 as Codec>::MAX_TOKEN_LEN, 5);
        assert_eq!(<i16 as Codec>::MAX_TOKEN_LEN, 6);
        assert_eq!(<u32 as Codec>::MAX_TOKEN_LEN, 10);
        assert_eq!(<i32 as Codec>::MAX_TOKEN_LEN, 11);
        assert_eq!(<f32 as Codec>::MAX_TOKEN_LEN, 10);
        assert_eq!(DataFormat::Hex16.token_width(), 4);
        // 1000 x .S（6 + 空白）が最大: 7000 - 1 + 終端 2。
        assert_eq!(MAX_TYPED_RESPONSE_BYTES, 7001);
    }

    #[test]
    fn point_count_limits() {
        // ビット・16 bit: 1..=1000
        assert!(build_read_many::<u16>(&addr("DM0"), 1000).is_ok());
        invalid(build_read_many::<u16>(&addr("DM0"), 1001));
        assert!(build_read_many::<i16>(&addr("DM0"), 1000).is_ok());
        invalid(build_read_many::<i16>(&addr("DM0"), 1001));
        assert!(build_read_many::<bool>(&addr("R0"), 1000).is_ok());
        invalid(build_read_many::<bool>(&addr("R0"), 1001));
        // 32 bit: 1..=500
        assert!(build_read_many::<u32>(&addr("DM0"), 500).is_ok());
        invalid(build_read_many::<u32>(&addr("DM0"), 501));
        assert!(build_read_many::<i32>(&addr("EM0"), 500).is_ok());
        invalid(build_read_many::<i32>(&addr("EM0"), 501));
        assert!(build_read_many::<f32>(&addr("EM0"), 500).is_ok());
        invalid(build_read_many::<f32>(&addr("EM0"), 501));
        // 書き込みも同じ上限。
        assert!(build_write_many::<u16>(&addr("DM0"), &[0; 1000]).is_ok());
        invalid(build_write_many::<u16>(&addr("DM0"), &[0; 1001]));
        assert!(build_write_many::<u32>(&addr("DM0"), &[0; 500]).is_ok());
        invalid(build_write_many::<u32>(&addr("DM0"), &[0; 501]));
        assert!(build_write_many::<bool>(&addr("MR0"), &[false; 1000]).is_ok());
        invalid(build_write_many::<bool>(&addr("MR0"), &[false; 1001]));
        // 巨大な点数もパニックせず断る。
        invalid(build_read_many::<bool>(&addr("R0"), usize::MAX));
        // 点数はゼロサプレスした 10 進で送る（`0003` ではなく `3`）。
        assert_eq!(
            build_read_many::<u16>(&addr("DM0"), 1000).unwrap(),
            "RDS DM0.U 1000"
        );
    }

    #[test]
    fn accessed_span_must_fit_the_device_range() {
        // ワード: 先頭 + 点数 × ワード数 - 1 <= 上限
        assert!(build_read::<u16>(&addr("DM65534")).is_ok());
        let m = invalid(build_read::<u32>(&addr("DM65534")));
        assert!(m.contains("65534"), "{m}");
        invalid(build_write::<f32>(&addr("EM65534"), 1.0));
        assert!(build_read::<u32>(&addr("DM65532")).is_ok());
        assert!(build_read_many::<u16>(&addr("DM65000"), 535).is_ok());
        invalid(build_read_many::<u16>(&addr("DM65000"), 536));
        assert!(build_read_many::<u32>(&addr("DM64534"), 500).is_ok());
        invalid(build_read_many::<u32>(&addr("DM64536"), 500));
        assert!(build_write_many::<i16>(&addr("EM65533"), &[1, 2]).is_ok());
        invalid(build_write_many::<i16>(&addr("EM65533"), &[1, 2, 3]));
        // ビット: チャネルの繰り上がりを含めた最後のビット <= 上限
        assert!(build_read::<bool>(&addr("R199915")).is_ok());
        assert!(build_read_many::<bool>(&addr("R199900"), 16).is_ok());
        invalid(build_read_many::<bool>(&addr("R199900"), 17));
        assert!(build_read_many::<bool>(&addr("R199815"), 17).is_ok());
        invalid(build_read_many::<bool>(&addr("R199815"), 18));
        assert!(build_read::<bool>(&addr("LR99915")).is_ok());
        invalid(build_write_many::<bool>(&addr("LR99915"), &[true, true]));
        assert!(build_read::<bool>(&addr("MR399915")).is_ok());
        invalid(build_read_many::<bool>(&addr("MR399915"), 2));
    }

    #[test]
    fn odd_start_for_32bit_types_is_rejected() {
        const { assert!(REJECT_NON_SIMULTANEOUS_32BIT) };
        for m in [
            invalid(build_read::<u32>(&addr("DM1"))),
            invalid(build_read::<i32>(&addr("EM3"))),
            invalid(build_read::<f32>(&addr("DM65533"))),
            invalid(build_read_many::<u32>(&addr("DM101"), 2)),
            invalid(build_write::<i32>(&addr("DM7"), 1)),
            invalid(build_write::<f32>(&addr("DM9"), 1.0)),
            invalid(build_write_many::<u32>(&addr("EM11"), &[1, 2])),
        ] {
            assert!(m.contains("even"), "{m}");
            assert!(m.contains("13-33"), "{m}");
        }
        // 16 bit 形式は奇数番地でよい。
        assert!(build_read::<u16>(&addr("DM1")).is_ok());
        assert!(build_write_many::<i16>(&addr("DM1"), &[1, 2]).is_ok());
        // 偶数番地からの RDS/WRS は 2 ワードずつ進むので全点が偶数番地。
        assert!(build_read_many::<i32>(&addr("DM100"), 3).is_ok());
    }

    #[test]
    fn f32_is_carried_as_d_bits() {
        assert_eq!(
            build_write::<f32>(&addr("DM0"), 1.0).unwrap(),
            "WR DM0.D 1065353216"
        );
        assert_eq!(
            build_write::<f32>(&addr("DM0"), -0.0).unwrap(),
            "WR DM0.D 2147483648"
        );
        assert_eq!(build_read::<f32>(&addr("EM2")).unwrap(), "RD EM2.D");
        assert_eq!(
            parse_read_response::<f32>("3212836864 0000000000", 2).unwrap(),
            [-1.0, 0.0]
        );
        let v = parse_read_response::<f32>("2139095040", 1).unwrap();
        assert_eq!(v[0], f32::INFINITY);
        // NaN もビット列のまま往復する（断らない）。
        let nan = f32::from_bits(0x7FC0_0001);
        let cmd = build_write::<f32>(&addr("DM0"), nan).unwrap();
        let token = cmd.rsplit(' ').next().unwrap();
        let back = parse_read_response::<f32>(token, 1).unwrap();
        assert_eq!(back[0].to_bits(), 0x7FC0_0001);
        framing::<f32>("4294967296", 1);
        framing::<f32>("1.5", 1);
        framing::<f32>("+1", 1);
    }

    #[test]
    fn overlong_tokens_are_framing_but_zero_suppressed_ones_parse() {
        framing::<u16>("000001", 1);
        framing::<i16>("+000001", 1);
        framing::<i16>("0000001", 1);
        framing::<u32>("00000000001", 1);
        framing::<i32>("+00000000001", 1);
        framing::<bool>("01", 1);
        // 短い形（ゼロサプレス）と符号付きの `+` 省略は受け付ける。
        assert_eq!(parse_read_response::<u16>("1", 1).unwrap(), [1]);
        assert_eq!(
            parse_read_response::<i16>("+1 -1 1", 3).unwrap(),
            [1, -1, 1]
        );
        assert_eq!(parse_read_response::<u32>("70000", 1).unwrap(), [70000]);
        assert_eq!(parse_read_response::<i32>("-5", 1).unwrap(), [-5]);
    }

    #[test]
    fn manual_examples_parse() {
        // RDS DM200.S 3 → +15025 -25400 +00000（13-17〜13-20）
        assert_eq!(
            parse_read_response::<i16>("+15025 -25400 +00000", 3).unwrap(),
            [15025, -25400, 0]
        );
        // RDS R100 4 → 1 0 1 0
        assert_eq!(
            parse_read_response::<bool>("1 0 1 0", 4).unwrap(),
            [true, false, true, false]
        );
        assert_eq!(
            build_read_many::<i16>(&addr("DM200"), 3).unwrap(),
            "RDS DM200.S 3"
        );
        assert_eq!(
            build_read_many::<bool>(&addr("R100"), 4).unwrap(),
            "RDS R100 4"
        );
        // WRS DM200.S 3 +15025 -05400 200 の書き込み側。このクレートは
        // ゼロサプレス・正の数は `+` 省略で送る（13-21〜13-23 で可）。
        assert_eq!(
            build_write_many::<i16>(&addr("DM200"), &[15025, -5400, 200]).unwrap(),
            "WRS DM200.S 3 15025 -5400 200"
        );
        assert_eq!(
            build_write_many::<bool>(&addr("R100"), &[true, false, true, false]).unwrap(),
            "WRS R100 4 1 0 1 0"
        );
    }

    #[test]
    fn multi_digit_e_lines_are_not_device_errors() {
        // エラーコードは 1 桁だけ。`E10` は値として解釈できず Framing。
        framing::<u16>("E10", 1);
        framing::<i16>("E10", 1);
        framing::<bool>("E10", 1);
        assert!(matches!(
            parse_write_response("E10"),
            Err(KvError::Framing(_))
        ));
        match parse_read_response::<u16>("E5", 1) {
            Err(KvError::Device { code }) => assert_eq!(code, DeviceErrorCode::CpuError),
            other => panic!("{other:?}"),
        }
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
        assert_eq!(parse_read_response::<i16>("+00000", 1).unwrap(), [0]);
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
        framing::<u32>("9999999999", 1);
        framing::<u32>("-1", 1);
    }

    #[test]
    fn i32_values() {
        assert_eq!(
            parse_read_response::<i32>("-2147483648 +2147483647", 2).unwrap(),
            [i32::MIN, i32::MAX]
        );
        assert_eq!(parse_read_response::<i32>("+0000000000", 1).unwrap(), [0]);
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
