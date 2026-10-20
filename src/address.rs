//! デバイスアドレスの文法（[`DeviceType`]・[`Address`]）。
//!
//! 送る前にここで文法を検査し、通らないものは [`AddressError`]（→
//! [`crate::KvError::InvalidRequest`]）で断る。PLC に「たぶん通らない」文字列を
//! 送って E0 を待つより、送らずに断るほうが原因が分かりやすく、接続にも触れない。
//!
//! 解析は大文字小文字を問わず、表示（＝送信する文字列）は大文字の正規形にする。
//! 文法（接頭辞・ビット番号の表記・ゼロサプレス）とデバイスごとの番号範囲は
//! docs/protocol.md P-13・P-22〜P-24 の表が正。範囲外の番号を持つ [`Address`] は
//! 作れない（構築・解析・[`Address::offset`] のどれもが範囲を検査する）ので、
//! 「範囲内の `Address`」は型で保証される。
//!
//! `R` の上限は KV-8000 などの `R199915` を採っている。KV-7000 シリーズで CPU
//! 機能バージョンが 2.2 以下の機種は実際には `R99915` までで、その間の番号は
//! 送ってから PLC の `E0` で断られる（P-24）。

use std::fmt;
use std::str::FromStr;

use crate::error::KvError;
use crate::protocol;

/// v0 で扱うデバイス種別。
///
/// `#[non_exhaustive]` を **付けていない** のは意図的（[`crate::KvError`] と同じ理由）:
/// 利用側（banto のアダプター）の `match` を網羅にしておけば、デバイス（T・C・W・B など）を
/// 足したときにコンパイルが壊れて気づける。variant の追加は破壊的変更として版を上げて
/// タグを打つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceType {
    /// `DM` データメモリ（ワード）。
    Dm,
    /// `EM` 拡張データメモリ（ワード）。
    Em,
    /// `R` リレー（ビット）。
    R,
    /// `MR` 内部補助リレー（ビット）。
    Mr,
    /// `LR` ラッチリレー（ビット）。
    Lr,
}

impl DeviceType {
    /// すべての種別。**長い接頭辞が先**（`MR`・`LR` を `R` より先に照合するため）。
    pub const ALL: [DeviceType; 5] = [
        DeviceType::Dm,
        DeviceType::Em,
        DeviceType::Mr,
        DeviceType::Lr,
        DeviceType::R,
    ];

    /// 正規形の接頭辞（大文字）。
    pub const fn prefix(self) -> &'static str {
        match self {
            DeviceType::Dm => protocol::PREFIX_DM,
            DeviceType::Em => protocol::PREFIX_EM,
            DeviceType::R => protocol::PREFIX_R,
            DeviceType::Mr => protocol::PREFIX_MR,
            DeviceType::Lr => protocol::PREFIX_LR,
        }
    }

    /// ビットデバイスか（`false` ならワードデバイス）。
    pub const fn is_bit(self) -> bool {
        matches!(self, DeviceType::R | DeviceType::Mr | DeviceType::Lr)
    }

    /// 最大デバイス番号（ワイヤ上の番号。ビットデバイスはチャネル × 100 +
    /// ビットの形で、`R` なら `199915`）。最小はどれも 0。`[P-24]`
    pub const fn max_number(self) -> u32 {
        match self {
            DeviceType::Dm => protocol::MAX_NUMBER_DM,
            DeviceType::Em => protocol::MAX_NUMBER_EM,
            DeviceType::R => protocol::MAX_NUMBER_R,
            DeviceType::Mr => protocol::MAX_NUMBER_MR,
            DeviceType::Lr => protocol::MAX_NUMBER_LR,
        }
    }
}

impl fmt::Display for DeviceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.prefix())
    }
}

/// アドレスの文法違反。[`KvError::InvalidRequest`] に変換される。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid address {input:?}: {reason}")]
pub struct AddressError {
    input: String,
    reason: String,
}

impl AddressError {
    fn new(input: impl Into<String>, reason: impl Into<String>) -> Self {
        AddressError {
            input: input.into(),
            reason: reason.into(),
        }
    }

    fn out_of_range(input: impl Into<String>, device: DeviceType) -> Self {
        AddressError::new(
            input,
            format!(
                "device number out of range for {device} (0..={})",
                device.max_number()
            ),
        )
    }

    /// 断られた入力文字列。
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl From<AddressError> for KvError {
    fn from(e: AddressError) -> Self {
        KvError::InvalidRequest(e.to_string())
    }
}

/// 1 点のデバイスアドレス（`DM100`・`R1015` など）。
///
/// ビットデバイスの番号はワイヤ上の表記どおり「チャネル × 100 + ビット」で
/// 保持する（`R1015` なら `number() == 1015`、`channel() == Some(10)`、
/// `bit() == Some(15)`）。ビット部が 16 以上（`R1016`）や、デバイスの範囲
/// （[`DeviceType::max_number`]）を超える番号は作れない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Address {
    device: DeviceType,
    number: u32,
}

impl Address {
    /// ワードデバイスのアドレスを作る。`device` がビットデバイス、または
    /// 番号が範囲外ならエラー。
    pub fn word(device: DeviceType, number: u32) -> Result<Address, AddressError> {
        if device.is_bit() {
            return Err(AddressError::new(
                format!("{device}{number}"),
                "Address::word needs a word device",
            ));
        }
        if number > device.max_number() {
            return Err(AddressError::out_of_range(
                format!("{device}{number}"),
                device,
            ));
        }
        Ok(Address { device, number })
    }

    /// ビットデバイスのアドレスを作る。`device` がワードデバイス、`bit` が
    /// 16 以上、または番号が範囲外ならエラー。
    pub fn bit(device: DeviceType, channel: u32, bit: u32) -> Result<Address, AddressError> {
        let shown = || format!("{device}{channel}{bit:0w$}", w = protocol::BIT_FIELD_DIGITS);
        if !device.is_bit() {
            return Err(AddressError::new(
                shown(),
                "Address::bit needs a bit device",
            ));
        }
        if bit >= protocol::BITS_PER_CHANNEL {
            return Err(AddressError::new(shown(), "bit part must be 00..=15"));
        }
        let number = channel
            .checked_mul(protocol::BIT_CHANNEL_MULTIPLIER)
            .and_then(|n| n.checked_add(bit))
            .ok_or_else(|| AddressError::out_of_range(shown(), device))?;
        if number > device.max_number() {
            return Err(AddressError::out_of_range(shown(), device));
        }
        Ok(Address { device, number })
    }

    /// デバイス種別。
    pub fn device(&self) -> DeviceType {
        self.device
    }

    /// ワイヤ上の番号（ビットデバイスはチャネル × 100 + ビット）。
    pub fn number(&self) -> u32 {
        self.number
    }

    /// ビットデバイスのチャネル。ワードデバイスは `None`。
    pub fn channel(&self) -> Option<u32> {
        self.device
            .is_bit()
            .then_some(self.number / protocol::BIT_CHANNEL_MULTIPLIER)
    }

    /// ビットデバイスのビット（0..=15）。ワードデバイスは `None`。
    pub fn bit_index(&self) -> Option<u32> {
        self.device
            .is_bit()
            .then_some(self.number % protocol::BIT_CHANNEL_MULTIPLIER)
    }

    /// `n` 点先のアドレス（ワードは +n ワード、ビットはビット 15 の次が
    /// 次チャネルのビット 0）。デバイスの範囲を超えれば `None`。
    /// 連続アクセスが範囲に収まるかの検査とモックの連続読み書きで使う。
    ///
    /// ビットの繰り上がり規則はマニュアルの番号表記（チャネル + ビット
    /// `00`〜`15`）からの推論（docs/protocol.md P-13）。
    pub fn offset(&self, n: u32) -> Option<Address> {
        if self.device.is_bit() {
            let ch = self.number / protocol::BIT_CHANNEL_MULTIPLIER;
            let bit = self.number % protocol::BIT_CHANNEL_MULTIPLIER;
            let linear = u64::from(ch) * u64::from(protocol::BITS_PER_CHANNEL)
                + u64::from(bit)
                + u64::from(n);
            let ch = u32::try_from(linear / u64::from(protocol::BITS_PER_CHANNEL)).ok()?;
            let bit = (linear % u64::from(protocol::BITS_PER_CHANNEL)) as u32;
            Address::bit(self.device, ch, bit).ok()
        } else {
            Address::word(self.device, self.number.checked_add(n)?).ok()
        }
    }

    /// 文字列から解析する（[`FromStr`] と同じ）。
    pub fn parse(s: &str) -> Result<Address, AddressError> {
        s.parse()
    }
}

impl FromStr for Address {
    type Err = AddressError;

    fn from_str(s: &str) -> Result<Address, AddressError> {
        if s.is_empty() {
            return Err(AddressError::new(s, "empty"));
        }
        let device = DeviceType::ALL
            .into_iter()
            .find(|d| {
                s.get(..d.prefix().len())
                    .is_some_and(|p| p.eq_ignore_ascii_case(d.prefix()))
            })
            .ok_or_else(|| AddressError::new(s, "unknown device type (v0: DM, EM, R, MR, LR)"))?;
        let digits = &s[device.prefix().len()..];
        if digits.is_empty() {
            return Err(AddressError::new(s, "missing device number"));
        }
        // `u32::from_str` は先頭の `+` を受け付けてしまうので、数字だけかを先に見る。
        if !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(AddressError::new(
                s,
                "device number must be decimal digits only",
            ));
        }
        // ゼロサプレスしてよい（P-13・P-22）ので先頭の 0 は受け付ける。
        let number: u32 = digits
            .parse()
            .map_err(|_| AddressError::out_of_range(s, device))?;
        if device.is_bit() {
            Address::bit(
                device,
                number / protocol::BIT_CHANNEL_MULTIPLIER,
                number % protocol::BIT_CHANNEL_MULTIPLIER,
            )
            .map_err(|e| AddressError::new(s, e.reason))
        } else {
            Address::word(device, number).map_err(|e| AddressError::new(s, e.reason))
        }
    }
}

impl fmt::Display for Address {
    /// 送信に使う正規形（大文字。ビットデバイスはビット部を 2 桁で書く:
    /// `R005`・`R1015`）。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.channel(), self.bit_index()) {
            (Some(ch), Some(bit)) => write!(
                f,
                "{}{}{:0w$}",
                self.device.prefix(),
                ch,
                bit,
                w = protocol::BIT_FIELD_DIGITS
            ),
            _ => write!(f, "{}{}", self.device.prefix(), self.number),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(s: &str) -> Address {
        s.parse()
            .unwrap_or_else(|e| panic!("{s:?} should parse: {e}"))
    }

    fn bad(s: &str) {
        assert!(s.parse::<Address>().is_err(), "{s:?} should be rejected");
    }

    #[test]
    fn bit_part_boundary() {
        let a = ok("R1015");
        assert_eq!(a.device(), DeviceType::R);
        assert_eq!(a.channel(), Some(10));
        assert_eq!(a.bit_index(), Some(15));
        assert_eq!(a.to_string(), "R1015");
        bad("R1016");
        bad("R1099");
        ok("R1000");
        ok("R0");
        bad("MR16");
        ok("MR15");
    }

    #[test]
    fn word_devices() {
        let a = ok("DM100");
        assert_eq!(a.device(), DeviceType::Dm);
        assert_eq!(a.number(), 100);
        assert_eq!(a.channel(), None);
        assert_eq!(a.to_string(), "DM100");
        assert_eq!(ok("EM0").to_string(), "EM0");
        bad("DM4294967295");
        bad("DM4294967296");
    }

    #[test]
    fn case_insensitive_and_canonical_upper() {
        assert_eq!(ok("dm100").to_string(), "DM100");
        assert_eq!(ok("Em7").to_string(), "EM7");
        assert_eq!(ok("mr1015").to_string(), "MR1015");
        assert_eq!(ok("lR3").to_string(), "LR003");
        assert_eq!(ok("r5").to_string(), "R005");
        assert_eq!(ok("DM0100").to_string(), "DM100");
    }

    #[test]
    fn longest_prefix_wins() {
        assert_eq!(ok("MR100").device(), DeviceType::Mr);
        assert_eq!(ok("LR100").device(), DeviceType::Lr);
        assert_eq!(ok("R100").device(), DeviceType::R);
    }

    #[test]
    fn garbage_and_empty() {
        for s in [
            "", "DM", "R", "MR", "X100", "D100", "DM-1", "DM+1", "DM 1", " DM1", "DM1 ", "DM1.U",
            "DM1a", "RD100", "ＤＭ1", "DM١",
        ] {
            bad(s);
        }
    }

    #[test]
    fn constructors_check_device_kind() {
        assert!(Address::word(DeviceType::R, 1).is_err());
        assert!(Address::bit(DeviceType::Dm, 1, 0).is_err());
        assert!(Address::bit(DeviceType::R, 10, 16).is_err());
        assert_eq!(Address::bit(DeviceType::R, 10, 15).unwrap(), ok("R1015"));
        assert!(Address::bit(DeviceType::R, u32::MAX, 0).is_err());
    }

    #[test]
    fn offset_carries_bits_into_next_channel() {
        assert_eq!(ok("R1015").offset(1), Some(ok("R1100")));
        assert_eq!(ok("R1014").offset(3), Some(ok("R1101")));
        assert_eq!(ok("R0").offset(16), Some(ok("R100")));
        assert_eq!(ok("DM10").offset(2), Some(ok("DM12")));
        // 範囲の端を超える offset は None。
        assert_eq!(ok("DM65534").offset(0), Some(ok("DM65534")));
        assert_eq!(ok("DM65534").offset(1), None);
        assert_eq!(ok("DM0").offset(u32::MAX), None);
        assert_eq!(ok("R199914").offset(1), Some(ok("R199915")));
        assert_eq!(ok("R199915").offset(1), None);
        assert_eq!(ok("LR99915").offset(1), None);
        assert_eq!(ok("MR399900").offset(15), Some(ok("MR399915")));
        assert_eq!(ok("MR399900").offset(16), None);
    }

    #[test]
    fn device_number_ranges() {
        // DM・EM: 0..=65534
        assert_eq!(ok("DM0").number(), 0);
        assert_eq!(ok("DM65534").number(), 65534);
        bad("DM65535");
        assert_eq!(ok("EM65534").number(), 65534);
        bad("EM65535");
        assert!(Address::word(DeviceType::Dm, 65534).is_ok());
        assert!(Address::word(DeviceType::Em, 65535).is_err());
        // R: 00000..=199915
        assert_eq!(ok("R199915").to_string(), "R199915");
        bad("R199916"); // ビット部 16
        bad("R200000");
        assert!(Address::bit(DeviceType::R, 1999, 15).is_ok());
        assert!(Address::bit(DeviceType::R, 2000, 0).is_err());
        // MR: 00000..=399915
        assert_eq!(ok("MR399915").to_string(), "MR399915");
        bad("MR400000");
        // LR: 00000..=99915
        assert_eq!(ok("LR99915").to_string(), "LR99915");
        bad("LR100000");
        // ゼロサプレスしない（桁を詰めた）表記も同じ番号。
        assert_eq!(ok("DM00200"), ok("DM200"));
        assert_eq!(ok("R00005"), ok("R5"));
        // 範囲外のメッセージは種別と上限を含む。
        let e = "DM65535".parse::<Address>().unwrap_err().to_string();
        assert!(e.contains("0..=65534"), "{e}");
    }
}
