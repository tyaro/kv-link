//! デバイスアドレスの文法（[`DeviceType`]・[`Address`]）。
//!
//! 送る前にここで文法を検査し、通らないものは [`AddressError`]（→
//! [`crate::KvError::InvalidRequest`]）で断る。PLC に「たぶん通らない」文字列を
//! 送って E0 を待つより、送らずに断るほうが原因が分かりやすく、接続にも触れない。
//!
//! 解析は大文字小文字を問わず、表示（＝送信する文字列）は大文字の正規形にする。
//! 文法そのもの（接頭辞・ビット番号の表記）は未照合で、docs/protocol.md P-13・
//! P-22・P-23 の表が正。デバイスごとの番号範囲は未照合なので検査しない
//! （P-24、`protocol.rs` の TODO）。

use std::fmt;
use std::str::FromStr;

use crate::error::KvError;
use crate::protocol;

/// v0 で扱うデバイス種別。
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
    reason: &'static str,
}

impl AddressError {
    fn new(input: impl Into<String>, reason: &'static str) -> Self {
        AddressError {
            input: input.into(),
            reason,
        }
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
/// `bit() == Some(15)`）。ビット部が 16 以上（`R1016`）は作れない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Address {
    device: DeviceType,
    number: u32,
}

impl Address {
    /// ワードデバイスのアドレスを作る。`device` がビットデバイスならエラー。
    pub fn word(device: DeviceType, number: u32) -> Result<Address, AddressError> {
        if device.is_bit() {
            return Err(AddressError::new(
                format!("{device}{number}"),
                "Address::word needs a word device",
            ));
        }
        Ok(Address { device, number })
    }

    /// ビットデバイスのアドレスを作る。`device` がワードデバイス、または
    /// `bit` が 16 以上ならエラー。
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
            .ok_or_else(|| AddressError::new(shown(), "device number too large"))?;
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
    /// 次チャネルのビット 0）。モックの連続読み書きで使う。桁あふれは `None`。
    ///
    /// ビットの繰り上がり規則は未照合（docs/protocol.md P-13）。
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
            Some(Address {
                device: self.device,
                number: self.number.checked_add(n)?,
            })
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
        let number: u32 = digits
            .parse()
            .map_err(|_| AddressError::new(s, "device number too large"))?;
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
        // 範囲検査はしない（docs/protocol.md P-24）。
        assert_eq!(ok("DM4294967295").number(), u32::MAX);
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
        assert_eq!(ok("DM4294967295").offset(1), None);
    }
}
