//! 疑似 KV（kv-sim）: PC 上で動かす常駐の偽 KV PLC。
//!
//! `kv_link::mock::MockServer` に周期的に値を書き込むだけの薄い実行ファイルで、
//! 実機が無くても banto-hub の画面・収集・書き込み経路を端から端まで試せるように
//! する。ワイヤ形式はクライアントと同じマニュアルの読みから作ってあるので、
//! 確かめられるのは「つながって動くか」までで、実機検証の代わりにはならない
//! （README「疑似 KV（kv-sim）」、docs/protocol.md「実機確認」）。
//!
//! 依存を増やさないため引数は `std::env::args` で手解析する。
//!
//! ```text
//! cargo run --example kv-sim --features mock -- --bind 127.0.0.1:8501
//! ```

use std::net::SocketAddr;
use std::process::ExitCode;
use std::time::Duration;

use kv_link::mock::MockServer;
use kv_link::Address;

const HELP: &str = "\
kv-sim: 疑似 KV PLC（kv-link のモックサーバーを常駐させる）

使い方: kv-sim [--bind <addr>] [--tick-ms <n>] [--quiet]

  --bind <addr>   待ち受けアドレス（既定 127.0.0.1:8501）
  --tick-ms <n>   デモ値の更新周期 ms（既定 1000。0 なら値を変えない）
  --quiet         受け取ったコマンドを stderr に出さない
  --help          この表示

デモ用デバイス（毎 tick 更新）:
  DM0        u16 カウンタ（折り返す）
  DM2-DM3    u32 カウンタ（偶数番地、下位ワードが先）
  DM10-DM11  f32 の正弦波（IEEE 754 のビット列、下位ワードが先）
  R000       tick ごとに反転するビット
  MR000      R000 の反転

上記のデバイスへクライアントが書き込んでも、次の tick で上書きされる。
それ以外のデバイスは普通のメモリ（書いた値が残る。未書き込みは 0）。

認証は無い。ループバック以外に bind すると同じネットワークの誰からでも
読み書きできる。テスト用の環境だけで使うこと。
実機ではない: クライアントと同じマニュアルの読みから作っているため、
統合の確認用であって実機検証にはならない。
";

struct Args {
    bind: String,
    tick_ms: u64,
    quiet: bool,
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut args = Args {
        bind: "127.0.0.1:8501".to_owned(),
        tick_ms: 1000,
        quiet: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--help" | "-h" => return Ok(None),
            "--quiet" => args.quiet = true,
            "--bind" => args.bind = it.next().ok_or("--bind に値がありません")?,
            "--tick-ms" => {
                args.tick_ms = it
                    .next()
                    .ok_or("--tick-ms に値がありません")?
                    .parse()
                    .map_err(|_| "--tick-ms は 0 以上の整数で指定してください")?;
            }
            other => return Err(format!("不明な引数: {other}（--help で使い方）")),
        }
    }
    Ok(Some(args))
}

fn addr(s: &str) -> Address {
    s.parse().expect("定数のアドレス")
}

/// 32 bit 値を下位ワードが先（指定番号が下位、docs/protocol.md P-12）で置く。
fn set_u32(mock: &MockServer, base: &str, v: u32) {
    let lo = addr(base);
    let hi = lo.offset(1).expect("上位ワード");
    mock.set_word(&lo, v as u16);
    mock.set_word(&hi, (v >> 16) as u16);
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(Some(a)) => a,
        Ok(None) => {
            print!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("kv-sim: {e}");
            return ExitCode::from(2);
        }
    };

    let mock = match MockServer::start_on(&args.bind).await {
        Ok(m) => m,
        Err(e) => {
            eprintln!("kv-sim: {} で待ち受けられません: {e}", args.bind);
            return ExitCode::FAILURE;
        }
    };
    let local: SocketAddr = mock.addr();
    if !local.ip().is_loopback() {
        eprintln!(
            "警告: {local} はループバックではありません。認証は無く、同じネットワークの誰からでも\n\
             読み書きできます。テスト用の環境だけで使ってください。"
        );
    }
    println!("kv-sim: {local} で待ち受け中（Ctrl-C で終了）");
    println!(
        "  DM0 u16 カウンタ / DM2-DM3 u32 カウンタ / DM10-DM11 f32 正弦波 / R000 反転 / MR000 = !R000"
    );
    println!("  （デモ用デバイスへの書き込みは次の tick で上書きされる。その他は普通のメモリ）");

    let dm0 = addr("DM0");
    let dm10 = addr("DM10");
    let dm11 = addr("DM11");
    let r000 = addr("R000");
    let mr000 = addr("MR000");

    // 初期値（tick 0 の値）。--tick-ms 0 のときはこのまま変わらない。
    mock.set_word(&dm0, 0);
    set_u32(&mock, "DM2", 0);
    mock.set_word(&dm10, 0);
    mock.set_word(&dm11, 0);
    mock.set_bit(&r000, false);
    mock.set_bit(&mr000, true);

    // --tick-ms 0 でも interval は周期 0 を許さないので長い周期で作り、select の
    // ガードで tick を受けないようにする。
    let period = if args.tick_ms == 0 {
        Duration::from_secs(3600)
    } else {
        Duration::from_millis(args.tick_ms)
    };
    let mut ticker = tokio::time::interval(period);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await; // 最初の tick は即時に来る。初期値は設定済みなので捨てる。

    // 受信ログは短い周期で追っかける（値の更新周期とは独立。--quiet なら不要）。
    let mut log_poll = tokio::time::interval(Duration::from_millis(100));
    let mut cursor = 0usize;
    let mut n: u64 = 0;
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    loop {
        tokio::select! {
            _ = &mut ctrl_c => break,
            _ = ticker.tick(), if args.tick_ms != 0 => {
                n += 1;
                mock.set_word(&dm0, n as u16);
                set_u32(&mock, "DM2", n as u32);
                // 周期 60 tick の正弦波。f32 のビット列を下位ワードが先で置く（P-26）。
                let v = (n as f32 * std::f32::consts::TAU / 60.0).sin() * 100.0;
                let bits = v.to_bits();
                mock.set_word(&dm10, bits as u16);
                mock.set_word(&dm11, (bits >> 16) as u16);
                let on = n % 2 == 1;
                mock.set_bit(&r000, on);
                mock.set_bit(&mr000, !on);
            }
            _ = log_poll.tick(), if !args.quiet => {
                let (cmds, next, missed) = mock.commands_since(cursor);
                cursor = next;
                if missed > 0 {
                    eprintln!("(ログが追いつかず {missed} 件を取りこぼし)");
                }
                for c in cmds {
                    eprintln!("<- {c}");
                }
            }
        }
    }
    println!(
        "kv-sim: 終了（接続 {} 本、要求 {} 件）",
        mock.connection_count(),
        mock.request_count()
    );
    ExitCode::SUCCESS
}
