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

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpStream, ToSocketAddrs};

use crate::address::Address;
use crate::codec::{self, KvValue};
use crate::error::KvError;
use crate::protocol;

/// 応答 1 行（終端込み）の上限バイト数。ライブラリの方針で、プロトコルの事実ではない。
///
/// 受信バッファはこの値を超えない（1 回の読み出しを残りバイト数で切るので、
/// 超過はなく、上限ちょうどの長さの応答までを受け付ける）。
///
/// 16 KiB にした理由: (1) 終端を送ってこない相手（ゴミを流し続ける機器、
/// 別プロトコルのポートにつないだ場合など）に対してメモリを有限に抑える、
/// (2) 型付き API の正しい応答はこれより十分小さい。点数上限（1 要求で
/// ビット・16 bit は 1000 点、32 bit は 500 点、docs/protocol.md P-25）と固定幅
/// （P-08）から、最大でも `.S` × 1000 点の約 7 KB に収まる。これはコンパイル時に
/// 検査しているので、型付き API の正しい応答が上限に当たって接続を失うことはない
/// （以前の「応答が上限に収まるか」の実行時の事前検査は、点数上限の検査に
/// 置き換えた）。`raw_command` の応答にも同じ上限が掛かる。
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024;

// 型付き API の最大の正しい応答が上限に収まること（収まらなければビルドが通らない）。
const _: () = assert!(codec::MAX_TYPED_RESPONSE_BYTES <= MAX_RESPONSE_BYTES);

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
    /// 応答がエラー応答（`E` + 数字 1 桁）なら [`KvError::Device`] を返す。
    /// それ以外の応答の中身は検査しない（呼び出し側が解釈する）。
    ///
    /// **応答サイズの事前検査は無い。** 型付き API は点数上限で応答が
    /// [`MAX_RESPONSE_BYTES`] に収まることを保証するが、ここで大きな `RDS` などを
    /// 生で送って応答が上限を超えると `Framing` になり、クライアントは毒化される。
    /// 通常は型付き API（`read` / `read_many` / `write` / `write_many`）を使うこと。
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
    ///
    /// 型とデバイスの組み合わせ違い、範囲外（32 bit 型は 2 ワード目も含む）、
    /// 32 bit 型の奇数番地は送らずに [`KvError::InvalidRequest`]
    /// （[`crate::codec`] の「送る前の検査」）。
    pub async fn read<T: KvValue>(&mut self, addr: &Address) -> Result<T, KvError> {
        let cmd = codec::build_read::<T>(addr)?;
        let line = self.transact(&cmd).await?;
        let mut v = self.poison_on_fatal(codec::parse_read_response::<T>(&line, 1))?;
        Ok(v.remove(0))
    }

    /// `addr` から連続 `count` 点読む（`RDS`）。32 bit 型は 1 点 2 ワード。
    ///
    /// 点数（ビット・16 bit 型は 1〜1000、32 bit 型は 1〜500）、範囲、32 bit 型の
    /// 奇数番地の違反は送らずに [`KvError::InvalidRequest`]。
    pub async fn read_many<T: KvValue>(
        &mut self,
        addr: &Address,
        count: usize,
    ) -> Result<Vec<T>, KvError> {
        let cmd = codec::build_read_many::<T>(addr, count)?;
        let line = self.transact(&cmd).await?;
        self.poison_on_fatal(codec::parse_read_response::<T>(&line, count))
    }

    /// 1 点書く（`WR`）。送る前の検査は [`KvClient::read`] と同じ。
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

    /// `addr` から連続して `values` を書く（`WRS`）。点数・範囲・奇数番地の
    /// 検査は [`KvClient::read_many`] と同じで、違反（空を含む）は送らずに
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
        // 頼んでいないバイトの検査は TcpStream 固有（`try_read`）なので、ジェネリックな
        // `exchange` の外でここだけが行う。同期の検査なので期限の外に置いても挙動は同じ。
        if let Err(e) = reject_unsolicited_bytes(stream) {
            return self.poison_on_fatal(Err(e));
        }
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

/// 送信 → 応答 1 行の受信。
///
/// 実際の接続は `TcpStream` だが、分割受信の境界条件を決定的にテストするため
/// `AsyncRead + AsyncWrite` に対してジェネリックにしている。
async fn exchange<S>(stream: &mut S, cmd: &str) -> Result<String, KvError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
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
///
/// 1 回の読み出しは「上限までの残り」を超えて読まないので、バッファは
/// [`MAX_RESPONSE_BYTES`] を超えない（上限ちょうどの長さの応答は受け付け、
/// 終端が上限内に現れなければ `Framing`）。
async fn read_line<S>(stream: &mut S) -> Result<String, KvError>
where
    S: AsyncRead + Unpin,
{
    let term = protocol::RESPONSE_TERMINATOR;
    let mut buf: Vec<u8> = Vec::with_capacity(64);
    let mut chunk = [0u8; 1024];
    loop {
        // ここに来るとき buf.len() < MAX_RESPONSE_BYTES（超えるなら下で打ち切る）。
        let want = chunk.len().min(MAX_RESPONSE_BYTES - buf.len());
        let n = stream.read(&mut chunk[..want]).await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::ReadBuf;

    /// 台本どおりのチャンクを 1 回の読み出しにつき 1 つずつ返す読み書きストリーム。
    /// 実ソケットと違い、チャンクの切れ目が決定的（読み出し側のバッファが小さい
    /// ときだけ、1 チャンクを複数回に分けて返す）。台本が尽きたら EOF。
    struct Scripted {
        chunks: VecDeque<Vec<u8>>,
        written: Vec<u8>,
    }

    impl Scripted {
        fn new(chunks: &[&[u8]]) -> Self {
            Scripted {
                chunks: chunks.iter().map(|c| c.to_vec()).collect(),
                written: Vec::new(),
            }
        }
    }

    impl AsyncRead for Scripted {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            if let Some(mut c) = self.chunks.pop_front() {
                let n = c.len().min(buf.remaining());
                buf.put_slice(&c[..n]);
                if n < c.len() {
                    self.chunks.push_front(c.split_off(n));
                }
            }
            Poll::Ready(Ok(()))
        }
    }

    impl AsyncWrite for Scripted {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            data: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            self.written.extend_from_slice(data);
            Poll::Ready(Ok(data.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    async fn line(chunks: &[&[u8]]) -> Result<String, KvError> {
        read_line(&mut Scripted::new(chunks)).await
    }

    fn is_framing(r: Result<String, KvError>) -> bool {
        matches!(r, Err(KvError::Framing(_)))
    }

    #[tokio::test]
    async fn cr_and_lf_in_separate_reads() {
        assert_eq!(line(&[b"OK\r", b"\n"]).await.unwrap(), "OK");
    }

    #[tokio::test]
    async fn terminator_split_across_chunks_with_payload() {
        assert_eq!(
            line(&[b"0001", b"2 0000", b"3\r", b"\n"]).await.unwrap(),
            "00012 00003"
        );
        // 1 バイトずつ届いても揃う。
        let bytes: Vec<&[u8]> = b"12345\r\n".chunks(1).collect();
        assert_eq!(line(&bytes).await.unwrap(), "12345");
    }

    #[tokio::test]
    async fn extra_bytes_after_terminator_in_same_read_is_framing() {
        assert!(is_framing(line(&[b"OK\r\nX"]).await));
        assert!(is_framing(line(&[b"OK\r", b"\n\r\n"]).await));
    }

    #[tokio::test]
    async fn no_terminator_within_cap_is_framing() {
        let big = vec![b'1'; MAX_RESPONSE_BYTES + 1000];
        assert!(is_framing(line(&[&big]).await));
    }

    #[tokio::test]
    async fn cap_is_exact() {
        // 終端込みでちょうど上限の応答は受け付ける。
        let mut ok = vec![b'1'; MAX_RESPONSE_BYTES - 2];
        ok.extend_from_slice(b"\r\n");
        assert_eq!(line(&[&ok]).await.unwrap().len(), MAX_RESPONSE_BYTES - 2);
        // 終端が上限の 1 バイト先になる応答は、1 回の読み出しが残りで切られるので
        // 上限を超えて読まずに Framing。
        let mut over = vec![b'1'; MAX_RESPONSE_BYTES - 1];
        over.extend_from_slice(b"\r\n");
        let mut s = Scripted::new(&[&over]);
        assert!(is_framing(read_line(&mut s).await));
        let left: usize = s.chunks.iter().map(Vec::len).sum();
        assert_eq!(left, 1, "上限を超えて読んでいない");
    }

    #[tokio::test]
    async fn non_printable_byte_is_framing() {
        assert!(is_framing(line(&[b"O\x00K\r\n"]).await));
        assert!(is_framing(line(&[b"OK\xff\r\n"]).await));
    }

    #[tokio::test]
    async fn eof_before_terminator_is_io() {
        assert!(matches!(line(&[b"OK"]).await, Err(KvError::Io(_))));
    }

    #[tokio::test]
    async fn exchange_sends_command_with_terminator_over_duplex() {
        use tokio::io::duplex;
        let (mut client, mut server) = duplex(64);
        let peer = tokio::spawn(async move {
            let mut req = [0u8; 8];
            server.read_exact(&mut req).await.unwrap();
            server.write_all(b"OK\r").await.unwrap();
            server.write_all(b"\n").await.unwrap();
            req
        });
        let r = exchange(&mut client, "WR R100").await.unwrap();
        assert_eq!(r, "OK");
        // コマンド本体の後ろに CR だけが付く。
        assert_eq!(&peer.await.unwrap(), b"WR R100\r");
    }
}
