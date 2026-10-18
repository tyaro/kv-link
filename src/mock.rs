//! テスト用のモック TCP サーバー（feature `mock`）。
//!
//! `127.0.0.1:0` で待ち受け、メモリ上のワード・ビット領域に対して
//! `RD`・`RDS`・`WR`・`WRS` を実装する。加えて、次の N 要求に対して
//! 障害を注入できる（[`Fault`]: エラーコード・遅延・分割送信・ゴミ・終端後の
//! 余分なバイト・無応答・切断）。
//!
//! **限界**: このモックは `protocol.rs` と同じ（未照合の）前提から作っている
//! ので、プロトコルの思い違いはモックでは見つからない。モックで確かめられる
//! のはクライアントの振る舞い（分割受信・毒化・エラー分類）まで。プロトコルの
//! 正しさは公式マニュアルとの照合と実機で確かめる（docs/protocol.md）。
//!
//! 接続数・要求数・受信したコマンドの記録（[`MockServer::connection_count`]
//! など）は、利用側（banto-industrial #438）が「全シミュレーション運転では
//! 実機向けの接続・要求が 1 つも出ていない」ことを確かめるためにも使う。
//!
//! 応答の書式（ゼロ詰め・符号）はモックの選択で、実機の書式は未照合
//! （docs/protocol.md P-08）。クライアントの解析が詰め物の有無どちらも
//! 受け付けることを確かめるため、あえて詰め物ありで返す。
//!
//! ```
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use kv_link::mock::MockServer;
//! use kv_link::{Address, KvClient, KvConfig};
//!
//! let mock = MockServer::start().await?;
//! let addr: Address = "DM100".parse()?;
//! mock.set_word(&addr, 42);
//!
//! let mut client = KvClient::connect(mock.addr(), KvConfig::default()).await?;
//! assert_eq!(client.read::<u16>(&addr).await?, 42);
//! assert_eq!(mock.received_commands(), ["RD DM100.U"]);
//! # Ok(())
//! # }
//! ```

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use crate::address::{Address, DeviceType};
use crate::codec::parse_hex16;
use crate::protocol::{self, DataFormat};

/// 1 要求に対して注入する障害。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// 実行せずに、このエラーコード（例 `"E0"`）を返す。
    ErrorCode(String),
    /// 実行し、応答をこの時間だけ遅らせて返す（遅れた応答は、クライアントが
    /// 切断済みなら届かずに捨てられる）。
    Delay(Duration),
    /// 実行し、応答を `chunk_len` バイトずつに分けて、間に `pause` を挟んで送る。
    Split {
        /// 1 回に送るバイト数（0 は 1 として扱う）。
        chunk_len: usize,
        /// 送る合間の待ち時間。
        pause: Duration,
    },
    /// 実行せずに、このバイト列をそのまま送る（終端を含めるかは呼び出し側が決める）。
    Garbage(Vec<u8>),
    /// 実行し、正しい応答の直後に（同じ書き込みで）このバイト列を足して送る。
    ExtraAfterTerminator(Vec<u8>),
    /// 受け取るだけで何も返さない（接続は開いたまま）。
    NoReply,
    /// 何も返さずに接続を閉じる。
    Close,
}

#[derive(Default)]
struct State {
    words: HashMap<(DeviceType, u32), u16>,
    bits: HashMap<(DeviceType, u32), bool>,
    faults: VecDeque<Fault>,
    connections: usize,
    commands: Vec<String>,
}

/// モックサーバー。drop すると待ち受けを止める（張られている接続は
/// 相手が閉じるまで残る）。
pub struct MockServer {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    accept_task: JoinHandle<()>,
}

impl std::fmt::Debug for MockServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockServer")
            .field("addr", &self.addr)
            .finish_non_exhaustive()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    // テスト用なので、他のタスクの panic で毒化していても中身は使ってよい。
    state.lock().unwrap_or_else(|e| e.into_inner())
}

impl MockServer {
    /// `127.0.0.1:0` で待ち受けを始める。tokio のランタイム内で呼ぶこと。
    pub async fn start() -> std::io::Result<MockServer> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let state = Arc::new(Mutex::new(State::default()));
        let accept_state = Arc::clone(&state);
        let accept_task = tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                lock(&accept_state).connections += 1;
                let _ = socket.set_nodelay(true);
                tokio::spawn(serve(socket, Arc::clone(&accept_state)));
            }
        });
        Ok(MockServer {
            addr,
            state,
            accept_task,
        })
    }

    /// 待ち受けているアドレス。
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// 次の `times` 要求に `fault` を注入する（先に積んだものから順に、
    /// 1 要求に 1 つずつ消費される）。
    pub fn inject(&self, fault: Fault, times: usize) {
        let mut s = lock(&self.state);
        s.faults.extend(std::iter::repeat_n(fault, times));
    }

    /// まだ消費されていない障害の数。
    pub fn pending_faults(&self) -> usize {
        lock(&self.state).faults.len()
    }

    /// これまでに受け付けた TCP 接続の数。
    pub fn connection_count(&self) -> usize {
        lock(&self.state).connections
    }

    /// これまでに受け取った要求（終端まで揃ったコマンド）の数。障害を注入した
    /// 要求も数える。
    pub fn request_count(&self) -> usize {
        lock(&self.state).commands.len()
    }

    /// 受け取ったコマンド（終端を除く）を受信順に返す。
    pub fn received_commands(&self) -> Vec<String> {
        lock(&self.state).commands.clone()
    }

    /// ワードを 1 つ設定する。`addr` がビットデバイスなら panic。
    pub fn set_word(&self, addr: &Address, value: u16) {
        assert!(!addr.device().is_bit(), "set_word needs a word device");
        lock(&self.state)
            .words
            .insert((addr.device(), addr.number()), value);
    }

    /// ワードを 1 つ読む（未設定は 0）。`addr` がビットデバイスなら panic。
    pub fn word(&self, addr: &Address) -> u16 {
        assert!(!addr.device().is_bit(), "word needs a word device");
        word_at(&lock(&self.state), addr)
    }

    /// ビットを 1 つ設定する。`addr` がワードデバイスなら panic。
    pub fn set_bit(&self, addr: &Address, value: bool) {
        assert!(addr.device().is_bit(), "set_bit needs a bit device");
        lock(&self.state)
            .bits
            .insert((addr.device(), addr.number()), value);
    }

    /// ビットを 1 つ読む（未設定は `false`）。`addr` がワードデバイスなら panic。
    pub fn bit(&self, addr: &Address) -> bool {
        assert!(addr.device().is_bit(), "bit needs a bit device");
        lock(&self.state)
            .bits
            .get(&(addr.device(), addr.number()))
            .copied()
            .unwrap_or(false)
    }
}

fn word_at(s: &State, addr: &Address) -> u16 {
    s.words
        .get(&(addr.device(), addr.number()))
        .copied()
        .unwrap_or(0)
}

/// 1 接続ぶんの処理。コマンドは `\r` 区切りで、届いた順に 1 つずつ処理する。
async fn serve(mut socket: TcpStream, state: Arc<Mutex<State>>) {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        while let Some(pos) = buf
            .iter()
            .position(|b| protocol::COMMAND_TERMINATOR.contains(b))
        {
            let raw: Vec<u8> = buf.drain(..=pos).collect();
            let mut line = String::from_utf8_lossy(&raw[..raw.len() - 1]).into_owned();
            // 相手が `\r\n` で送ってきた場合の `\n` は次の行頭に残るので捨てる。
            if line.starts_with('\n') {
                line.remove(0);
            }
            let fault = {
                let mut s = lock(&state);
                s.commands.push(line.clone());
                s.faults.pop_front()
            };
            if !respond(&mut socket, &state, &line, fault).await {
                return;
            }
        }
        match socket.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

/// 応答を送る。接続を閉じるべきなら `false`。
async fn respond(
    socket: &mut TcpStream,
    state: &Mutex<State>,
    line: &str,
    fault: Option<Fault>,
) -> bool {
    let reply = |body: String| {
        let mut v = body.into_bytes();
        v.extend_from_slice(protocol::RESPONSE_TERMINATOR);
        v
    };
    let bytes = match fault {
        None => reply(execute(&mut lock(state), line)),
        Some(Fault::ErrorCode(code)) => reply(code),
        Some(Fault::Garbage(bytes)) => bytes,
        Some(Fault::NoReply) => return true,
        Some(Fault::Close) => return false,
        Some(Fault::Delay(d)) => {
            let bytes = reply(execute(&mut lock(state), line));
            tokio::time::sleep(d).await;
            bytes
        }
        Some(Fault::ExtraAfterTerminator(extra)) => {
            let mut bytes = reply(execute(&mut lock(state), line));
            bytes.extend_from_slice(&extra);
            bytes
        }
        Some(Fault::Split { chunk_len, pause }) => {
            let bytes = reply(execute(&mut lock(state), line));
            for (i, part) in bytes.chunks(chunk_len.max(1)).enumerate() {
                if i > 0 {
                    tokio::time::sleep(pause).await;
                }
                if socket.write_all(part).await.is_err() {
                    return false;
                }
            }
            return true;
        }
    };
    socket.write_all(&bytes).await.is_ok()
}

/// コマンド 1 行を実行し、応答の本文（終端なし）を返す。
///
/// エラーの割り当て（文法違反 = `E1`、アドレス違反 = `E0`、値の範囲外 = `E1`）は
/// モックの選択で、実機がどう返すかは未照合。
fn execute(s: &mut State, line: &str) -> String {
    let e_cmd = || protocol::ERR_COMMAND.to_owned();
    let e_dev = || protocol::ERR_DEVICE_NUMBER.to_owned();
    let mut tokens = line.split(protocol::SEPARATOR);
    let Some(cmd) = tokens.next() else {
        return e_cmd();
    };
    let Some(operand) = tokens.next() else {
        return e_cmd();
    };
    let Some((addr, fmt)) = parse_operand(operand) else {
        return e_dev();
    };
    let rest: Vec<&str> = tokens.collect();
    let parse_count = |t: &str| t.parse::<u32>().ok().filter(|n| *n > 0);

    match cmd.to_ascii_uppercase().as_str() {
        c if c == protocol::CMD_READ && rest.is_empty() => {
            read_points(s, &addr, fmt, 1).unwrap_or_else(e_dev)
        }
        c if c == protocol::CMD_READ_CONSECUTIVE && rest.len() == 1 => match parse_count(rest[0]) {
            Some(n) => read_points(s, &addr, fmt, n).unwrap_or_else(e_dev),
            None => e_cmd(),
        },
        c if c == protocol::CMD_WRITE && rest.len() == 1 => {
            write_points(s, &addr, fmt, &rest).unwrap_or_else(|e| e)
        }
        c if c == protocol::CMD_WRITE_CONSECUTIVE && !rest.is_empty() => {
            match parse_count(rest[0]) {
                Some(n) if n as usize == rest.len() - 1 => {
                    write_points(s, &addr, fmt, &rest[1..]).unwrap_or_else(|e| e)
                }
                _ => e_cmd(),
            }
        }
        _ => e_cmd(),
    }
}

/// `DM100.U` → (アドレス, 形式)。ビットデバイスは形式なし、ワードデバイスは
/// 形式なしなら `.U` とみなす（実機の既定形式は未照合、docs/protocol.md P-14）。
fn parse_operand(operand: &str) -> Option<(Address, Option<DataFormat>)> {
    let (dev, fmt) = match operand.split_once(protocol::FORMAT_DELIMITER) {
        Some((d, f)) => (d, Some(DataFormat::from_suffix(f)?)),
        None => (operand, None),
    };
    let addr: Address = dev.parse().ok()?;
    match (addr.device().is_bit(), fmt) {
        (true, None) => Some((addr, None)),
        (true, Some(_)) => None,
        (false, f) => Some((addr, Some(f.unwrap_or(DataFormat::U16)))),
    }
}

/// 32 bit 形式の 2 ワードの並び（下位ワードが先）はモックの仮定で、実機は
/// 未照合（docs/protocol.md P-12、ワード順は P-26 で保留中）。
fn read_points(s: &State, addr: &Address, fmt: Option<DataFormat>, n: u32) -> Option<String> {
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        match fmt {
            None => {
                let a = addr.offset(i)?;
                let v = s
                    .bits
                    .get(&(a.device(), a.number()))
                    .copied()
                    .unwrap_or(false);
                out.push(if v { "1" } else { "0" }.to_owned());
            }
            Some(f) => {
                let a = addr.offset(i.checked_mul(f.words())?)?;
                let lo = word_at(s, &a);
                let hi = if f.words() == 2 {
                    word_at(s, &a.offset(1)?)
                } else {
                    0
                };
                let dword = u32::from(lo) | (u32::from(hi) << 16);
                out.push(match f {
                    DataFormat::U16 => format!("{lo:05}"),
                    DataFormat::S16 => format!("{:+06}", lo as i16),
                    DataFormat::U32 => format!("{dword:010}"),
                    DataFormat::S32 => format!("{:+011}", dword as i32),
                    DataFormat::Hex16 => format!("{lo:04X}"),
                });
            }
        }
    }
    Some(out.join(&protocol::SEPARATOR.to_string()))
}

fn write_points(
    s: &mut State,
    addr: &Address,
    fmt: Option<DataFormat>,
    values: &[&str],
) -> Result<String, String> {
    let e_cmd = || protocol::ERR_COMMAND.to_owned();
    let e_dev = || protocol::ERR_DEVICE_NUMBER.to_owned();
    // 先に全部を解析してから書く（途中で失敗したら何も書かない）。
    let mut words: Vec<(Address, u16)> = Vec::new();
    let mut bits: Vec<(Address, bool)> = Vec::new();
    for (i, token) in values.iter().enumerate() {
        let i = i as u32;
        match fmt {
            None => {
                let v = match *token {
                    "0" => false,
                    "1" => true,
                    _ => return Err(e_cmd()),
                };
                bits.push((addr.offset(i).ok_or_else(e_dev)?, v));
            }
            Some(f) => {
                let a = addr
                    .offset(i.checked_mul(f.words()).ok_or_else(e_dev)?)
                    .ok_or_else(e_dev)?;
                let dword: u32 = match f {
                    DataFormat::U16 => u32::from(token.parse::<u16>().map_err(|_| e_cmd())?),
                    DataFormat::S16 => u32::from(token.parse::<i16>().map_err(|_| e_cmd())? as u16),
                    DataFormat::U32 => token.parse::<u32>().map_err(|_| e_cmd())?,
                    DataFormat::S32 => token.parse::<i32>().map_err(|_| e_cmd())? as u32,
                    DataFormat::Hex16 => u32::from(parse_hex16(token).ok_or_else(e_cmd)?),
                };
                words.push((a, dword as u16));
                if f.words() == 2 {
                    words.push((a.offset(1).ok_or_else(e_dev)?, (dword >> 16) as u16));
                }
            }
        }
    }
    for (a, v) in words {
        s.words.insert((a.device(), a.number()), v);
    }
    for (a, v) in bits {
        s.bits.insert((a.device(), a.number()), v);
    }
    Ok(protocol::RESPONSE_OK.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(s: &mut State, line: &str) -> String {
        execute(s, line)
    }

    #[test]
    fn execute_reads_and_writes() {
        let mut s = State::default();
        assert_eq!(run(&mut s, "WR DM0.U 7"), "OK");
        assert_eq!(run(&mut s, "RD DM0.U"), "00007");
        assert_eq!(run(&mut s, "RD DM0"), "00007");
        assert_eq!(run(&mut s, "WRS DM10.L 2 -1 2147483647"), "OK");
        assert_eq!(run(&mut s, "RDS DM10.L 2"), "-0000000001 +2147483647");
        assert_eq!(run(&mut s, "RDS DM10.U 2"), "65535 65535");
        assert_eq!(run(&mut s, "WRS R1015 2 1 1"), "OK");
        assert_eq!(run(&mut s, "RD R1100"), "1");
        assert_eq!(run(&mut s, "RD DM0.H"), "0007");
    }

    #[test]
    fn execute_errors() {
        let mut s = State::default();
        assert_eq!(run(&mut s, "XX DM0"), "E1");
        assert_eq!(run(&mut s, "RD"), "E1");
        assert_eq!(run(&mut s, "RD ZZ0"), "E0");
        assert_eq!(run(&mut s, "RD R100.U"), "E0");
        assert_eq!(run(&mut s, "WR DM0.U 65536"), "E1");
        assert_eq!(run(&mut s, "WRS DM0.U 2 1"), "E1");
        assert_eq!(run(&mut s, "RDS DM0.U 0"), "E1");
    }
}
