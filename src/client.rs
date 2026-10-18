//! 非同期 TCP クライアント [`KvClient`]。
//!
//! 守っている性質（理由は lib.rs のモジュール文書「1 要求ずつ・毒化」）:
//!
//! - 同時に飛ぶ要求は常に 1 つ（要求系メソッドはすべて `&mut self`）。
//! - 応答は `\r\n` まで、上限 [`MAX_RESPONSE_BYTES`] バイトで読む。分割受信に
//!   対応し、終端の後ろに同じ読み出しで余分なバイトがあれば `Framing`。
//! - 致命エラー（`Framing`・`Timeout`・`Io`）の後はソケットを捨てて毒化し、
//!   以後の呼び出しはネットワークに触れず `NotConnected` を返す。
//! - 要求を送る直前に、頼んでいないバイトが届いていないかを確かめる
//!   （届いていれば `Framing`）。

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, ToSocketAddrs};

use crate::address::Address;
use crate::codec::{self, KvValue};
use crate::error::KvError;
use crate::protocol;

/// 応答 1 行（終端込み）の上限バイト数。ライブラリの方針で、プロトコルの事実ではない。
///
/// 16 KiB にした理由: (1) 終端を送ってこない相手（ゴミを流し続ける機器、
/// 別プロトコルのポートにつないだ場合など）に対してメモリを有限に抑える、
/// (2) 想定する正しい応答はこれより十分小さい（`i32` でも 1 点 12 バイト
/// なので約 1,360 点まで入る）。上限に収まらない点数の読み出しは、送る前に
/// `InvalidRequest` で断る（正しい応答で接続を失わないため）。
/// RDS の点数上限が照合できたら（docs/protocol.md P-25）見直す。
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024;

/// 接続設定。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct KvConfig {
    /// TCP 接続の確立を待つ時間。既定 5 秒。
    pub connect_timeout: Duration,
    /// 1 要求の送信から応答の終端までを待つ時間（送信と受信を合わせた期限）。
    /// 既定 2 秒。
    pub response_timeout: Duration,
}

impl Default for KvConfig {
    fn default() -> Self {
        KvConfig {
            connect_timeout: Duration::from_secs(5),
            response_timeout: Duration::from_secs(2),
        }
    }
}

impl KvConfig {
    /// 接続タイムアウトを変えたものを返す。
    pub fn with_connect_timeout(mut self, d: Duration) -> Self {
        self.connect_timeout = d;
        self
    }

    /// 応答タイムアウトを変えたものを返す。
    pub fn with_response_timeout(mut self, d: Duration) -> Self {
        self.response_timeout = d;
        self
    }
}

/// KV シリーズ 1 台への 1 本の上位リンク接続。
///
/// 要求系メソッドは `&mut self` を取るので、1 つのクライアントで同時に複数の
/// 要求が飛ぶことは型で起きない。複数の利用者から 1 本の接続を共有する
/// 直列化（キュー・ロック）・再接続・品質フラグは利用側（banto）の責務で、
/// このクレートは持たない。
///
/// 致命エラー（[`KvError::is_connection_fatal`] が `true`）の後、この
/// クライアントは毒化され、以後は [`KvError::NotConnected`] しか返さない。
/// 続けるには [`KvClient::connect`] で作り直す。
#[derive(Debug)]
pub struct KvClient {
    stream: Option<TcpStream>,
    config: KvConfig,
}

impl KvClient {
    /// 接続する。`TCP_NODELAY` を有効にする（1 要求ずつ応答を待つので、
    /// Nagle は遅延を足すだけで、まとめて送れる相手がいない）。
    ///
    /// 接続タイムアウトは [`KvError::Timeout`]、接続拒否などは [`KvError::Io`]。
    pub async fn connect<A: ToSocketAddrs>(addr: A, config: KvConfig) -> Result<KvClient, KvError> {
        let stream = tokio::time::timeout(config.connect_timeout, TcpStream::connect(addr))
            .await
            .map_err(|_| KvError::Timeout)??;
        stream.set_nodelay(true)?;
        Ok(KvClient {
            stream: Some(stream),
            config,
        })
    }

    /// 接続設定。
    pub fn config(&self) -> &KvConfig {
        &self.config
    }

    /// まだ使えるか（毒化・切断されていないか）。`false` なら以後の要求は
    /// すべて [`KvError::NotConnected`]。
    pub fn is_connected(&self) -> bool {
        self.stream.is_some()
    }

    /// 接続を閉じる。以後の要求は [`KvError::NotConnected`]。
    pub fn close(&mut self) {
        self.stream = None;
    }

    /// 生のコマンド（終端なし）を 1 つ送り、応答 1 行（終端なし）を返す。
    ///
    /// `cmd` は印字可能な ASCII（0x20〜0x7E）だけで、空でないこと（CR/LF を
    /// 含めて 1 行に複数のコマンドを詰めることを防ぐ）。違反は送らずに
    /// [`KvError::InvalidRequest`]。
    ///
    /// 応答がエラー応答（`E` + 数字 1〜2 桁）なら [`KvError::Device`] を返す。
    /// `.H` 形式の読み出しでは値とエラー応答が見分けにくい点に注意
    /// （docs/protocol.md P-15）。
    pub async fn raw_command(&mut self, cmd: &str) -> Result<String, KvError> {
        if cmd.is_empty() || !cmd.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
            return Err(KvError::InvalidRequest(
                "raw command must be non-empty printable ASCII without CR/LF".into(),
            ));
        }
        let line = self.transact(cmd).await?;
        codec::check_device_error(&line)?;
        Ok(line)
    }

    /// 1 点読む（`RD`）。
    pub async fn read<T: KvValue>(&mut self, addr: &Address) -> Result<T, KvError> {
        let cmd = codec::build_read::<T>(addr)?;
        let line = self.transact(&cmd).await?;
        let mut v = self.poison_on_fatal(codec::parse_read_response::<T>(&line, 1))?;
        Ok(v.remove(0))
    }

    /// `addr` から連続 `count` 点読む（`RDS`）。32 bit 型は 1 点 2 ワード。
    ///
    /// `count == 0`、または応答が [`MAX_RESPONSE_BYTES`] に収まらない点数は
    /// 送らずに [`KvError::InvalidRequest`]。
    pub async fn read_many<T: KvValue>(
        &mut self,
        addr: &Address,
        count: usize,
    ) -> Result<Vec<T>, KvError> {
        let cmd = codec::build_read_many::<T>(addr, count)?;
        codec::check_response_fits::<T>(count, MAX_RESPONSE_BYTES)?;
        let line = self.transact(&cmd).await?;
        self.poison_on_fatal(codec::parse_read_response::<T>(&line, count))
    }

    /// 1 点書く（`WR`）。
    ///
    /// 要求を送った後の接続致命エラー（[`KvError::Timeout`]・[`KvError::Io`]・
    /// [`KvError::Framing`]。例: 壊れた応答・余分なバイト・終端なし）は結果不明
    /// （書けたかどうか分からない）。成功扱いにせず、このメソッドは再送しない。
    /// [`KvError::InvalidRequest`]・[`KvError::NotConnected`] は何も送っていない。
    pub async fn write<T: KvValue>(&mut self, addr: &Address, value: T) -> Result<(), KvError> {
        let cmd = codec::build_write(addr, value)?;
        let line = self.transact(&cmd).await?;
        self.poison_on_fatal(codec::parse_write_response(&line))
    }

    /// `addr` から連続して `values` を書く（`WRS`）。空なら送らずに
    /// [`KvError::InvalidRequest`]。
    ///
    /// 要求を送った後の接続致命エラー（[`KvError::Timeout`]・[`KvError::Io`]・
    /// [`KvError::Framing`]。例: 壊れた応答・余分なバイト・終端なし）は結果不明
    /// （一部だけ書けた可能性も含む）。成功扱いにせず、このメソッドは再送しない。
    /// [`KvError::InvalidRequest`]・[`KvError::NotConnected`] は何も送っていない。
    pub async fn write_many<T: KvValue>(
        &mut self,
        addr: &Address,
        values: &[T],
    ) -> Result<(), KvError> {
        let cmd = codec::build_write_many(addr, values)?;
        let line = self.transact(&cmd).await?;
        self.poison_on_fatal(codec::parse_write_response(&line))
    }

    /// 致命エラーならソケットを捨てる。完全な 1 行を受け取った後の解析失敗
    /// （`Framing`）も、応答がこの要求のものだと信用できないので毒化する。
    fn poison_on_fatal<T>(&mut self, r: Result<T, KvError>) -> Result<T, KvError> {
        if let Err(e) = &r {
            if e.is_connection_fatal() {
                self.stream = None;
            }
        }
        r
    }

    /// 1 要求を送り、応答 1 行（終端なし）を受け取る。致命エラーで毒化する。
    async fn transact(&mut self, cmd: &str) -> Result<String, KvError> {
        let Some(stream) = self.stream.as_mut() else {
            return Err(KvError::NotConnected);
        };
        let deadline = self.config.response_timeout;
        // 期限切れで内側の future は読み出しの途中で捨てられうるが、その場合は
        // 下で必ず毒化してソケットごと捨てるので、途中まで読んだストリームを
        // 使い続けることはない。
        let r = match tokio::time::timeout(deadline, exchange(stream, cmd)).await {
            Ok(r) => r,
            Err(_) => Err(KvError::Timeout),
        };
        self.poison_on_fatal(r)
    }
}

/// 送信前の確認 → 送信 → 応答 1 行の受信。
async fn exchange(stream: &mut TcpStream, cmd: &str) -> Result<String, KvError> {
    reject_unsolicited_bytes(stream)?;
    let mut frame = Vec::with_capacity(cmd.len() + protocol::COMMAND_TERMINATOR.len());
    frame.extend_from_slice(cmd.as_bytes());
    frame.extend_from_slice(protocol::COMMAND_TERMINATOR);
    stream.write_all(&frame).await?;
    read_line(stream).await
}

/// 要求を送る前に、受信バッファに頼んでいないバイトが無いことを確かめる。
///
/// 前の応答の終端の後ろに余分なバイトが **別の TCP セグメントで** 遅れて
/// 届いた場合、`read_line` の「同じ読み出し内の余分なバイト」の検査では
/// 捕まえられず、次の要求の応答の先頭として読んでしまう。送る直前なら
/// 正しい相手は何も送ってこないはずなので、何か届いていればストリームが
/// ずれている証拠として `Framing` にする。届く途中のものとは競合しうる
/// ので、ずれを防ぐ最後の砦ではなく、安く拾える分を拾う追加の検査。
fn reject_unsolicited_bytes(stream: &mut TcpStream) -> Result<(), KvError> {
    let mut probe = [0u8; 64];
    match stream.try_read(&mut probe) {
        Ok(0) => Err(KvError::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "connection closed by the peer",
        ))),
        Ok(n) => Err(KvError::Framing(format!(
            "{n} unsolicited byte(s) arrived before the request was sent: {:?}",
            String::from_utf8_lossy(&probe[..n])
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
        Err(e) => Err(KvError::Io(e)),
    }
}

/// 応答を終端 `\r\n` まで読み、終端を除いた 1 行を返す。
async fn read_line(stream: &mut TcpStream) -> Result<String, KvError> {
    let term = protocol::RESPONSE_TERMINATOR;
    let mut buf: Vec<u8> = Vec::with_capacity(64);
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(KvError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed by the peer before the response terminator",
            )));
        }
        // 終端が前回の読み出しとの境目をまたぐ場合に備え、1 バイト手前から探す。
        let search_from = buf.len().saturating_sub(term.len() - 1);
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf[search_from..]
            .windows(term.len())
            .position(|w| w == term)
            .map(|p| p + search_from)
        {
            let end = pos + term.len();
            if end != buf.len() {
                return Err(KvError::Framing(format!(
                    "{} extra byte(s) after the response terminator",
                    buf.len() - end
                )));
            }
            buf.truncate(pos);
            if let Some(b) = buf.iter().find(|b| !(0x20..=0x7e).contains(*b)) {
                return Err(KvError::Framing(format!(
                    "non-printable byte 0x{b:02X} in the response"
                )));
            }
            // 上で印字可能 ASCII だけと確かめたので UTF-8 として正しい。
            return Ok(String::from_utf8(buf).expect("printable ASCII is valid UTF-8"));
        }
        if buf.len() >= MAX_RESPONSE_BYTES {
            return Err(KvError::Framing(format!(
                "no response terminator within {MAX_RESPONSE_BYTES} bytes"
            )));
        }
    }
}
