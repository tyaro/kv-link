//! 上位リンク通信のワイヤ上の事実（コマンド名・書式・終端・応答の形・エラーコード・
//! ビット番号の文法・デバイス範囲・点数上限）を **この 1 ファイルに集める**。
//!
//! ここにある値は、キーエンス公式マニュアル「KV-XLE02 ユーザーズマニュアル」
//! （AS_166260_KV-XLE02_UM_084A11_KJ_JP_2036_1）13 章「上位リンク通信機能」と
//! 照合済み（2026-10-18）。各定数のコメントにある `[P-xx]` は `docs/protocol.md` の
//! 表の行番号で、ページ番号と、マニュアルが直接は書いていない推論部分の区別は
//! その表が正。他のモジュールはここの定数を参照するだけで、リテラルを持たない。
//!
//! **実機ではまだ 1 つも確かめていない**（docs/protocol.md「実機確認」）。また
//! マニュアルは KV-XLE02 のもので、KV-8000 の内蔵ポートや KV-LE21V などの他の
//! ユニットでも同じと仮定しているが、それは未照合。
//!
//! ライブラリの方針として決めた値（応答の上限バイト数、奇数番地の 32 bit を
//! 断るかどうかなど、プロトコルの事実ではないもの）はここに置かず、使う側の
//! モジュール（`client.rs`・`codec.rs`）に置く。

/// コマンドの終端（CR）。ユニットは LF を無視するので CR LF で送ってもよいが、
/// このクレートは CR だけを送る。`[P-01]`
pub const COMMAND_TERMINATOR: &[u8] = b"\r";

/// 応答の終端（CR LF）。`[P-02]`
pub const RESPONSE_TERMINATOR: &[u8] = b"\r\n";

/// コマンドの各要素の区切り、および読み出し応答の値と値の区切り（半角空白 1 個、
/// 20H）。`[P-03]` `[P-08]`
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

/// エラー応答の `E` に続く数字の桁数（ちょうど 1 桁）。`[P-15]`
///
/// マニュアルの異常応答の一覧（13-32）はすべて `E` + 数字 1 桁。`E10` のような
/// 2 桁以上はエラー応答とみなさない（型付きの読み出しでは値として解釈できず
/// `Framing` になる）。`.H` の値は常に 4 桁で返る（[`DataFormat::token_width`]）
/// ので、2 文字のエラー応答と衝突しない。
pub const MAX_ERROR_CODE_DIGITS: usize = 1;

/// `E0` デバイス番号異常（デバイス番号・アドレスなどが範囲外）。`[P-16]`
pub const ERR_DEVICE_NUMBER: &str = "E0";
/// `E1` コマンド異常（対応していないコマンド・指定方法の誤り）。`[P-17]`
pub const ERR_COMMAND: &str = "E1";
/// `E2` プログラム未登録。`[P-18]`
pub const ERR_PROGRAM_NOT_REGISTERED: &str = "E2";
/// `E4` 書き込み禁止。`[P-19]`
pub const ERR_WRITE_PROHIBITED: &str = "E4";
/// `E5` 本体エラー（CPU ユニットのエラーを解除しないまま RUN 切り換えを送った）。
/// `[P-20]`
pub const ERR_CPU_ERROR: &str = "E5";
/// `E6` コメントなし（RDC で指定したデバイスにコメントが登録されていない）。`[P-21]`
pub const ERR_NO_COMMENT: &str = "E6";

/// ビット形式（形式指定なしのビットデバイス）の読み出し応答 1 値の幅
/// （`0`/`1` の 1 バイト）。`[P-08]`
pub const BIT_TOKEN_WIDTH: usize = 1;

/// ビット形式・16 bit 形式（`.U`・`.S`・`.H`）の 1 要求あたりの最大点数。`[P-25]`
pub const MAX_POINTS_16BIT: usize = 1000;
/// 32 bit 形式（`.D`・`.L`）の 1 要求あたりの最大点数。`[P-25]`
pub const MAX_POINTS_32BIT: usize = 500;

/// ワードデバイスのデータ形式（`.U` など）。v0 はビットデバイスに形式を付けない
/// （`[P-14]`。プロトコル上はビットデバイスにも `.U`・`.S`・`.H` を付けられるが、
/// ライブラリの制限として使わない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataFormat {
    /// `.U` 符号なし 16 bit 10 進。`[P-11]`
    U16,
    /// `.S` 符号付き 16 bit 10 進。`[P-11]`
    S16,
    /// `.D` 符号なし 32 bit 10 進。指定したデバイスが下位ワード、次のデバイスが
    /// 上位ワード。`[P-11]` `[P-12]`
    U32,
    /// `.L` 符号付き 32 bit 10 進。ワードの並びは `.D` と同じ。`[P-11]` `[P-12]`
    S32,
    /// `.H` 16 bit 16 進（4 桁）。型付き API では使わない。`raw_command` とモック用。
    /// `[P-11]`
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

    /// PLC が返す読み出し応答 1 値の固定幅（バイト）。`.S` は符号込み 6
    /// （0 は `+00000`）、`.L` は符号込み 11（0 は `+0000000000`）。`[P-08]`
    pub const fn token_width(self) -> usize {
        match self {
            DataFormat::U16 => 5,
            DataFormat::S16 => 6,
            DataFormat::U32 => 10,
            DataFormat::S32 => 11,
            DataFormat::Hex16 => 4,
        }
    }

    /// 1 要求あたりの最大点数。`[P-25]`
    pub const fn max_points(self) -> usize {
        match self {
            DataFormat::U32 | DataFormat::S32 => MAX_POINTS_32BIT,
            DataFormat::U16 | DataFormat::S16 | DataFormat::Hex16 => MAX_POINTS_16BIT,
        }
    }

    /// 接尾辞（大文字小文字は問わない）から形式を引く。
    pub fn from_suffix(s: &str) -> Option<DataFormat> {
        DataFormat::ALL
            .into_iter()
            .find(|f| f.suffix().eq_ignore_ascii_case(s))
    }
}

/// 32 bit 形式（`.D`・`.L`）で、先頭のデバイス番号 `first_word` から読み書きした
/// 2 ワードの同時性（上位と下位が同じ瞬間の値であること）が保証されるか。
/// マニュアルは **デバイス番号が偶数の場合のみ** 保証するとしている。`[P-32]`
pub const fn is_32bit_simultaneous(first_word: u32) -> bool {
    first_word.is_multiple_of(2)
}

/// ビットデバイスの番号表記: 番号 = チャネル × `BIT_CHANNEL_MULTIPLIER` + ビット。
/// `R1015` = チャネル 10 のビット 15。`[P-13]`
pub const BIT_CHANNEL_MULTIPLIER: u32 = 100;

/// 1 チャネルのビット数（ビット部は `00..=15`）。`[P-13]`
pub const BITS_PER_CHANNEL: u32 = 16;

/// ビット部を表示するときの桁数（`R005` = チャネル 0 のビット 5）。デバイス番号は
/// ゼロサプレスしてよいので `R5`・`R005`・`R00005` はどれも同じ番号を指す。
/// 送信は下 2 桁をビット部として明示する `R005` 形。`[P-13]`
pub const BIT_FIELD_DIGITS: usize = 2;

/// v0 で扱うデバイス種別の接頭辞。
/// 解析は長い接頭辞から順に照合する（`MR` を `R` より先に）。`[P-22]`
pub const PREFIX_DM: &str = "DM";
/// `[P-22]`
pub const PREFIX_EM: &str = "EM";
/// リレー。マニュアル上は種別記号を省略できるが、このクレートは常に付けて送る。
/// `[P-23]`
pub const PREFIX_R: &str = "R";
/// `[P-23]`
pub const PREFIX_MR: &str = "MR";
/// `[P-23]`
pub const PREFIX_LR: &str = "LR";

/// `DM` の最大デバイス番号（`DM0`〜`DM65534`）。`[P-24]`
pub const MAX_NUMBER_DM: u32 = 65534;
/// `EM` の最大デバイス番号（`EM0`〜`EM65534`）。`[P-24]`
pub const MAX_NUMBER_EM: u32 = 65534;
/// `R` の最大デバイス番号（`R00000`〜`R199915`）。KV-7000 シリーズで CPU 機能
/// バージョンが 2.2 以下なら実際の上限は `R99915` だが、ここでは大きいほうを
/// 採り、古い CPU では PLC の `E0` に任せる。`[P-24]`
pub const MAX_NUMBER_R: u32 = 199_915;
/// `MR` の最大デバイス番号（`MR00000`〜`MR399915`）。`[P-24]`
pub const MAX_NUMBER_MR: u32 = 399_915;
/// `LR` の最大デバイス番号（`LR00000`〜`LR99915`）。`[P-24]`
pub const MAX_NUMBER_LR: u32 = 99_915;
