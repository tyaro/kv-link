//! 上位リンク通信のワイヤ上の事実（コマンド名・書式・終端・応答の形・エラーコード・
//! ビット番号の文法）を **この 1 ファイルに集める**。
//!
//! ここにある値はすべて **未照合（記憶ベース）** で、キーエンス公式の通信マニュアルとは
//! まだ突き合わせていない。各定数のコメントにある `[P-xx]` は
//! `docs/protocol.md` の表の行番号で、照合の作業はその表とこのファイルだけを
//! 触れば済むようにしてある（他のモジュールはここの定数を参照するだけで、
//! リテラルを持たない）。照合が済むまで「対応済み」と書かないこと。
//!
//! ライブラリの方針として決めた値（応答の上限バイト数など、プロトコルの事実では
//! ないもの）はここに置かず、使う側のモジュール（`client.rs`）に置く。

/// コマンドの終端。`[P-01]`
pub const COMMAND_TERMINATOR: &[u8] = b"\r";

/// 応答の終端。`[P-02]`
pub const RESPONSE_TERMINATOR: &[u8] = b"\r\n";

/// コマンドの各要素の区切り、および読み出し応答の値と値の区切り（半角空白 1 個）。
/// `[P-03]` `[P-08]`
pub const SEPARATOR: char = ' ';

/// 単点読み出し `RD <dev>[.fmt]`。`[P-04]`
pub const CMD_READ: &str = "RD";
/// 連続読み出し `RDS <dev>[.fmt] <n>`。`[P-05]`
pub const CMD_READ_CONSECUTIVE: &str = "RDS";
/// 単点書き込み `WR <dev>[.fmt] <v>`。`[P-06]`
pub const CMD_WRITE: &str = "WR";
/// 連続書き込み `WRS <dev>[.fmt] <n> <v1> ... <vn>`。`[P-07]`
pub const CMD_WRITE_CONSECUTIVE: &str = "WRS";

/// 書き込み成功の応答。`[P-09]`
pub const RESPONSE_OK: &str = "OK";

/// デバイスとデータ形式の区切り（`DM100.U` の `.`）。`[P-10]`
pub const FORMAT_DELIMITER: char = '.';

/// エラー応答の先頭文字。`[P-15]`
pub const ERROR_PREFIX: char = 'E';

/// エラー応答の `E` に続く数字の最大桁数。`[P-15]`
///
/// 「`E` + 数字 1 桁以上」をそのままエラーとみなすと、`.H`（16 進）の読み出し
/// 応答 `E100` などと区別できない。既知のコードはすべて 1 桁なので、2 桁までに
/// 絞って衝突を減らしている（それでも詰め物なしの 16 進 `E1` とは衝突しうる。
/// 型付き API は `.H` を使わないので影響しない。docs/protocol.md P-15 の注記）。
pub const MAX_ERROR_CODE_DIGITS: usize = 2;

/// `E0` デバイス番号異常。`[P-16]`
pub const ERR_DEVICE_NUMBER: &str = "E0";
/// `E1` コマンド異常。`[P-17]`
pub const ERR_COMMAND: &str = "E1";
/// `E2` プログラム未登録。`[P-18]`
pub const ERR_PROGRAM_NOT_REGISTERED: &str = "E2";
/// `E4` 書き込み禁止。`[P-19]`
pub const ERR_WRITE_PROHIBITED: &str = "E4";
/// `E5` ユニットエラー。`[P-20]`
pub const ERR_UNIT: &str = "E5";
/// `E6` コメントなし。`[P-21]`
pub const ERR_NO_COMMENT: &str = "E6";

/// ワードデバイスのデータ形式（`.U` など）。ビットデバイスは形式を付けない
/// （`[P-14]`）ので、この型はワードデバイスにだけ使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataFormat {
    /// `.U` 符号なし 16 bit 10 進。`[P-11]`
    U16,
    /// `.S` 符号付き 16 bit 10 進。`[P-11]`
    S16,
    /// `.D` 符号なし 32 bit 10 進。連続 2 ワードを PLC が結合する。`[P-11]` `[P-12]`
    U32,
    /// `.L` 符号付き 32 bit 10 進。連続 2 ワードを PLC が結合する。`[P-11]` `[P-12]`
    S32,
    /// `.H` 16 bit 16 進。型付き API では使わない（エラー応答と見分けにくい、
    /// [`MAX_ERROR_CODE_DIGITS`] 参照）。`raw_command` とモック用。`[P-11]`
    Hex16,
}

impl DataFormat {
    /// すべての形式（モックの書式解析と網羅テスト用）。
    pub const ALL: [DataFormat; 5] = [
        DataFormat::U16,
        DataFormat::S16,
        DataFormat::U32,
        DataFormat::S32,
        DataFormat::Hex16,
    ];

    /// `.` の後ろに付ける文字。`[P-11]`
    pub const fn suffix(self) -> &'static str {
        match self {
            DataFormat::U16 => "U",
            DataFormat::S16 => "S",
            DataFormat::U32 => "D",
            DataFormat::S32 => "L",
            DataFormat::Hex16 => "H",
        }
    }

    /// 1 点が占めるワード数。32 bit 形式は 2。`[P-12]`
    pub const fn words(self) -> u32 {
        match self {
            DataFormat::U32 | DataFormat::S32 => 2,
            DataFormat::U16 | DataFormat::S16 | DataFormat::Hex16 => 1,
        }
    }

    /// 接尾辞（大文字小文字は問わない）から形式を引く。
    pub fn from_suffix(s: &str) -> Option<DataFormat> {
        DataFormat::ALL
            .into_iter()
            .find(|f| f.suffix().eq_ignore_ascii_case(s))
    }
}

/// ビットデバイスの番号表記: 番号 = チャネル × `BIT_CHANNEL_MULTIPLIER` + ビット。
/// `R1015` = チャネル 10 のビット 15。`[P-13]`
pub const BIT_CHANNEL_MULTIPLIER: u32 = 100;

/// 1 チャネルのビット数（ビット部は `00..=15`）。`[P-13]`
pub const BITS_PER_CHANNEL: u32 = 16;

/// ビット部を表示するときの桁数（`R005` = チャネル 0 のビット 5）。
/// 先頭チャネル 0 の表記（`R5` か `R005` か `R00005` か）は未照合。`[P-13]`
pub const BIT_FIELD_DIGITS: usize = 2;

/// v0 で扱うデバイス種別の接頭辞。
/// 解析は長い接頭辞から順に照合する（`MR` を `R` より先に）。`[P-22]`
pub const PREFIX_DM: &str = "DM";
/// `[P-22]`
pub const PREFIX_EM: &str = "EM";
/// `[P-23]`
pub const PREFIX_R: &str = "R";
/// `[P-23]`
pub const PREFIX_MR: &str = "MR";
/// `[P-23]`
pub const PREFIX_LR: &str = "LR";

// TODO(docs/protocol.md P-24): デバイスごとの番号範囲（DM の上限など）は機種に
// よって違い、未照合。現時点では範囲の検査をしない（PLC が E0 で断る）。
// 照合後にここへ表を足し、`address.rs` の解析で検査する。

// TODO(docs/protocol.md P-25): RDS/WRS の 1 回あたりの最大点数は未照合。
// 現時点ではライブラリ側で上限を課さない（応答の上限バイト数から導く
// 上限だけを `client.rs` で検査する）。照合後にここへ定数を足す。
