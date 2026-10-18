//! モックサーバーに対する結合テスト（公開 API だけを使う）。
//!
//! モックはマニュアル（KV-XLE02 UM 13 章）の読み取りに合わせてあるが、同じ理解から
//! 作っているので、ここで確かめているのはクライアントの振る舞い（往復・分割受信・
//! 毒化・エラー分類・送る前の検査）とマニュアルの例の再現であって、実機での
//! 正しさではない（docs/protocol.md「実機確認」）。

use std::time::Duration;

use kv_link::mock::{Fault, MockServer};
use kv_link::{Address, DeviceErrorCode, KvClient, KvConfig, KvError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn a(s: &str) -> Address {
    s.parse().unwrap()
}

fn fast_config() -> KvConfig {
    KvConfig::default()
        .with_connect_timeout(Duration::from_secs(2))
        .with_response_timeout(Duration::from_millis(300))
}

async fn setup() -> (MockServer, KvClient) {
    let mock = MockServer::start().await.unwrap();
    let client = KvClient::connect(mock.addr(), fast_config()).await.unwrap();
    (mock, client)
}

#[tokio::test]
async fn roundtrip_every_type_single() {
    let (mock, mut c) = setup().await;

    c.write::<u16>(&a("DM0"), 65535).await.unwrap();
    assert_eq!(c.read::<u16>(&a("DM0")).await.unwrap(), 65535);

    for v in [i16::MIN, -1, 0, i16::MAX] {
        c.write::<i16>(&a("DM1"), v).await.unwrap();
        assert_eq!(c.read::<i16>(&a("DM1")).await.unwrap(), v);
    }

    c.write::<u32>(&a("EM10"), u32::MAX).await.unwrap();
    assert_eq!(c.read::<u32>(&a("EM10")).await.unwrap(), u32::MAX);

    for v in [i32::MIN, -1, 0, i32::MAX] {
        c.write::<i32>(&a("DM20"), v).await.unwrap();
        assert_eq!(c.read::<i32>(&a("DM20")).await.unwrap(), v);
    }

    c.write::<bool>(&a("R1015"), true).await.unwrap();
    assert!(c.read::<bool>(&a("R1015")).await.unwrap());
    assert!(mock.bit(&a("R1015")));
    c.write::<bool>(&a("mr5"), true).await.unwrap();
    assert!(c.read::<bool>(&a("MR005")).await.unwrap());
    c.write::<bool>(&a("LR0"), false).await.unwrap();
    assert!(!c.read::<bool>(&a("LR0")).await.unwrap());

    assert!(mock.received_commands().contains(&"WR R1015 1".to_owned()));
    assert_eq!(mock.connection_count(), 1);
}

#[tokio::test]
async fn roundtrip_every_type_many() {
    let (mock, mut c) = setup().await;

    c.write_many::<u16>(&a("DM0"), &[1, 2, 65535])
        .await
        .unwrap();
    assert_eq!(
        c.read_many::<u16>(&a("DM0"), 3).await.unwrap(),
        [1, 2, 65535]
    );

    c.write_many::<i16>(&a("DM10"), &[i16::MIN, i16::MAX])
        .await
        .unwrap();
    assert_eq!(
        c.read_many::<i16>(&a("DM10"), 2).await.unwrap(),
        [i16::MIN, i16::MAX]
    );

    c.write_many::<u32>(&a("EM0"), &[0, u32::MAX, 70000])
        .await
        .unwrap();
    assert_eq!(
        c.read_many::<u32>(&a("EM0"), 3).await.unwrap(),
        [0, u32::MAX, 70000]
    );
    // 32 bit は 1 点 2 ワード（EM0..EM5 を使う）。
    assert_eq!(mock.word(&a("EM4")), (70000u32 & 0xFFFF) as u16);

    c.write_many::<i32>(&a("DM100"), &[i32::MIN, -1, i32::MAX])
        .await
        .unwrap();
    assert_eq!(
        c.read_many::<i32>(&a("DM100"), 3).await.unwrap(),
        [i32::MIN, -1, i32::MAX]
    );

    // ビット 15 の次は次チャネルのビット 0（番号表記からの推論、docs/protocol.md P-13）。
    c.write_many::<bool>(&a("R1014"), &[true, false, true])
        .await
        .unwrap();
    assert_eq!(
        c.read_many::<bool>(&a("R1014"), 3).await.unwrap(),
        [true, false, true]
    );
    assert!(mock.bit(&a("R1100")));
}

#[tokio::test]
async fn split_receive_is_reassembled() {
    let (mock, mut c) = setup().await;
    mock.set_word(&a("DM0"), 12345);
    mock.set_word(&a("DM1"), 1);
    mock.inject(
        Fault::Split {
            chunk_len: 1,
            pause: Duration::from_millis(5),
        },
        1,
    );
    assert_eq!(c.read_many::<u16>(&a("DM0"), 2).await.unwrap(), [12345, 1]);

    // 終端 `\r\n` の `\r` と `\n` が別のセグメントに分かれる場合。
    mock.inject(
        Fault::Split {
            chunk_len: 6,
            pause: Duration::from_millis(20),
        },
        1,
    );
    assert_eq!(c.read::<u16>(&a("DM0")).await.unwrap(), 12345);
    assert!(c.is_connected());
}

#[tokio::test]
async fn malformed_response_is_framing_and_poisons() {
    let (mock, mut c) = setup().await;
    mock.inject(Fault::Garbage(b"ZZZ\r\n".to_vec()), 1);
    let e = c.read::<u16>(&a("DM0")).await.unwrap_err();
    assert!(matches!(e, KvError::Framing(_)), "{e:?}");
    assert!(e.is_connection_fatal());
    assert!(!c.is_connected());

    let before = mock.request_count();
    let e = c.read::<u16>(&a("DM0")).await.unwrap_err();
    assert!(matches!(e, KvError::NotConnected), "{e:?}");
    assert_eq!(
        mock.request_count(),
        before,
        "poisoned client touched the network"
    );
}

#[tokio::test]
async fn wrong_value_count_is_framing() {
    let (mock, mut c) = setup().await;
    mock.inject(Fault::Garbage(b"00001 00002\r\n".to_vec()), 1);
    let e = c.read::<u16>(&a("DM0")).await.unwrap_err();
    assert!(matches!(e, KvError::Framing(_)), "{e:?}");
    assert!(!c.is_connected());
}

#[tokio::test]
async fn ok_where_values_expected_is_framing() {
    let (mock, mut c) = setup().await;
    mock.inject(Fault::Garbage(b"OK\r\n".to_vec()), 1);
    let e = c.read::<u16>(&a("DM0")).await.unwrap_err();
    assert!(matches!(e, KvError::Framing(_)), "{e:?}");
}

#[tokio::test]
async fn values_where_ok_expected_is_framing() {
    let (mock, mut c) = setup().await;
    mock.inject(Fault::Garbage(b"00001\r\n".to_vec()), 1);
    let e = c.write::<u16>(&a("DM0"), 1).await.unwrap_err();
    assert!(matches!(e, KvError::Framing(_)), "{e:?}");
    assert!(!c.is_connected());
}

#[tokio::test]
async fn extra_bytes_after_terminator_is_framing() {
    let (mock, mut c) = setup().await;
    mock.inject(Fault::ExtraAfterTerminator(b"00009\r\n".to_vec()), 1);
    let e = c.read::<u16>(&a("DM0")).await.unwrap_err();
    assert!(matches!(e, KvError::Framing(_)), "{e:?}");
    assert!(!c.is_connected());
}

#[tokio::test]
async fn missing_terminator_within_cap_is_framing() {
    let (mock, mut c) = setup().await;
    mock.inject(Fault::Garbage(vec![b'1'; 20 * 1024]), 1);
    let e = c.read::<u16>(&a("DM0")).await.unwrap_err();
    assert!(matches!(e, KvError::Framing(_)), "{e:?}");
}

#[tokio::test]
async fn device_error_is_not_fatal() {
    let (mock, mut c) = setup().await;
    mock.set_word(&a("DM0"), 7);
    mock.inject(Fault::ErrorCode("E0".into()), 1);
    match c.read::<u16>(&a("DM0")).await {
        Err(KvError::Device { code }) => assert_eq!(code, DeviceErrorCode::DeviceNumber),
        other => panic!("{other:?}"),
    }
    mock.inject(Fault::ErrorCode("E4".into()), 1);
    let e = c.write::<u16>(&a("DM0"), 1).await.unwrap_err();
    assert!(!e.is_connection_fatal());
    assert!(matches!(
        e,
        KvError::Device {
            code: DeviceErrorCode::WriteProhibited
        }
    ));
    mock.inject(Fault::ErrorCode("E9".into()), 1);
    match c.read::<u16>(&a("DM0")).await {
        Err(KvError::Device {
            code: DeviceErrorCode::Unknown(raw),
        }) => assert_eq!(raw, "E9"),
        other => panic!("{other:?}"),
    }

    // 同じクライアントで続けられる。注入した write は実行されていない。
    assert!(c.is_connected());
    assert_eq!(c.read::<u16>(&a("DM0")).await.unwrap(), 7);
}

#[tokio::test]
async fn real_device_error_from_mock_execution() {
    // 障害注入ではなく、モックが実際に E1 を返す経路（不正なコマンド）。
    let (_mock, mut c) = setup().await;
    match c.raw_command("XYZ DM0").await {
        Err(KvError::Device { code }) => assert_eq!(code, DeviceErrorCode::Command),
        other => panic!("{other:?}"),
    }
    assert_eq!(c.raw_command("RD DM0.H").await.unwrap(), "0000");
    assert!(c.is_connected());
}

#[tokio::test]
async fn timeout_poisons_and_late_reply_never_surfaces() {
    let (mock, mut c) = setup().await;
    mock.set_word(&a("DM0"), 111);
    mock.set_word(&a("DM1"), 222);
    // DM0 の応答（"00111"）を応答タイムアウト（300ms）より遅らせる。
    mock.inject(Fault::Delay(Duration::from_millis(800)), 1);

    let e = c.read::<u16>(&a("DM0")).await.unwrap_err();
    assert!(matches!(e, KvError::Timeout), "{e:?}");
    assert!(!c.is_connected());

    // 遅れた応答が届く頃まで待っても、次の要求は送られず NotConnected。
    tokio::time::sleep(Duration::from_millis(700)).await;
    let e = c.read::<u16>(&a("DM1")).await.unwrap_err();
    assert!(matches!(e, KvError::NotConnected), "{e:?}");
    assert_eq!(mock.request_count(), 1);

    // 作り直したクライアントは DM1 の正しい値を読む（DM0 の遅れた値ではない）。
    let mut c = KvClient::connect(mock.addr(), fast_config()).await.unwrap();
    assert_eq!(c.read::<u16>(&a("DM1")).await.unwrap(), 222);
    assert_eq!(mock.connection_count(), 2);
}

#[tokio::test]
async fn no_reply_is_timeout() {
    let (mock, mut c) = setup().await;
    mock.inject(Fault::NoReply, 1);
    let e = c.read::<u16>(&a("DM0")).await.unwrap_err();
    assert!(matches!(e, KvError::Timeout), "{e:?}");
    assert!(matches!(
        c.read::<u16>(&a("DM0")).await,
        Err(KvError::NotConnected)
    ));
}

#[tokio::test]
async fn closed_connection_is_io_then_not_connected() {
    let (mock, mut c) = setup().await;
    mock.inject(Fault::Close, 1);
    let e = c.read::<u16>(&a("DM0")).await.unwrap_err();
    assert!(matches!(e, KvError::Io(_)), "{e:?}");
    assert!(e.is_connection_fatal());
    assert!(matches!(
        c.read::<u16>(&a("DM0")).await,
        Err(KvError::NotConnected)
    ));
}

#[tokio::test]
async fn invalid_requests_never_reach_the_network_and_do_not_poison() {
    let (mock, mut c) = setup().await;
    let cases: Vec<KvError> = vec![
        c.read::<u16>(&a("R100")).await.unwrap_err(),
        c.read::<bool>(&a("DM100")).await.unwrap_err(),
        c.write::<i32>(&a("MR0"), 1).await.unwrap_err(),
        c.read_many::<u16>(&a("DM0"), 0).await.unwrap_err(),
        c.read_many::<i32>(&a("DM0"), 100_000).await.unwrap_err(),
        c.read_many::<u16>(&a("DM0"), 1001).await.unwrap_err(),
        c.read::<u32>(&a("DM65534")).await.unwrap_err(),
        c.read::<u32>(&a("DM1")).await.unwrap_err(),
        c.write_many::<u16>(&a("DM0"), &[]).await.unwrap_err(),
        c.raw_command("").await.unwrap_err(),
        c.raw_command("RD DM0\rRD DM1").await.unwrap_err(),
        c.raw_command("RD DM0\n").await.unwrap_err(),
        c.raw_command("RD DMé").await.unwrap_err(),
    ];
    for e in &cases {
        assert!(matches!(e, KvError::InvalidRequest(_)), "{e:?}");
        assert!(!e.is_connection_fatal());
    }
    // アドレス文法違反も InvalidRequest に変換される。
    let e: KvError = "R1016".parse::<Address>().unwrap_err().into();
    assert!(matches!(e, KvError::InvalidRequest(_)));

    assert_eq!(mock.request_count(), 0);
    assert!(c.is_connected());
    assert_eq!(c.read::<u16>(&a("DM0")).await.unwrap(), 0);
}

/// 点数が巨大でも、パニックも送信もせず（点数上限で）`InvalidRequest` で断り、
/// 接続は保たれる。
#[tokio::test]
async fn huge_read_many_count_is_invalid_request_without_sending() {
    let (mock, mut c) = setup().await;
    let e = c
        .read_many::<bool>(&a("R0"), usize::MAX / 2)
        .await
        .unwrap_err();
    assert!(matches!(e, KvError::InvalidRequest(_)), "{e:?}");
    assert!(c.is_connected());
    assert_eq!(mock.request_count(), 0);
}

#[tokio::test]
async fn closed_client_returns_not_connected() {
    let (mock, mut c) = setup().await;
    c.close();
    assert!(matches!(
        c.read::<u16>(&a("DM0")).await,
        Err(KvError::NotConnected)
    ));
    assert_eq!(mock.request_count(), 0);
}

#[tokio::test]
async fn faults_apply_to_the_next_n_requests_in_order() {
    let (mock, mut c) = setup().await;
    mock.inject(Fault::ErrorCode("E1".into()), 2);
    assert_eq!(mock.pending_faults(), 2);
    assert!(matches!(
        c.read::<u16>(&a("DM0")).await,
        Err(KvError::Device { .. })
    ));
    assert!(matches!(
        c.read::<u16>(&a("DM0")).await,
        Err(KvError::Device { .. })
    ));
    assert_eq!(mock.pending_faults(), 0);
    assert!(c.read::<u16>(&a("DM0")).await.is_ok());
    assert_eq!(
        mock.received_commands(),
        ["RD DM0.U", "RD DM0.U", "RD DM0.U"]
    );
}

/// 前の応答の後ろに、別の TCP セグメントで頼んでいないバイトが遅れて届いた
/// 場合、次の要求を送る前に検出して `Framing` にする（その遅れたバイトを
/// 次の要求の応答として読まない）。モックの障害注入では表せないので、
/// 素の TCP リスナーで相手を演じる。
#[tokio::test]
async fn unsolicited_bytes_before_next_request_are_framing() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // クライアントの write 完了後にだけ余分なバイトを送る（時間差に頼らない）。
    let (written_tx, written_rx) = tokio::sync::oneshot::channel::<()>();
    let peer = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 64];
        let _ = s.read(&mut buf).await.unwrap();
        s.write_all(b"OK\r\n").await.unwrap();
        written_rx.await.unwrap();
        s.write_all(b"00042\r\n").await.unwrap();
        // 相手が読むまで接続を保つ。
        let _ = s.read(&mut buf).await;
    });

    let mut c = KvClient::connect(addr, fast_config()).await.unwrap();
    c.write::<u16>(&a("DM0"), 1).await.unwrap();
    written_tx.send(()).unwrap();
    // 余分なバイトが届くのを待つ。
    tokio::time::sleep(Duration::from_millis(100)).await;
    let e = c.read::<u16>(&a("DM0")).await.unwrap_err();
    assert!(matches!(e, KvError::Framing(_)), "{e:?}");
    assert!(!c.is_connected());
    drop(c);
    peer.await.unwrap();
}

#[tokio::test]
async fn connection_refused_is_io() {
    // 一度 bind して閉じたポートに接続する（ほぼ確実に拒否される）。
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let e = KvClient::connect(("127.0.0.1", port), fast_config())
        .await
        .unwrap_err();
    assert!(matches!(e, KvError::Io(_)), "{e:?}");
}

/// マニュアル（13-17〜13-23）の例のやり取りをモックで再現する。
#[tokio::test]
async fn manual_example_exchanges() {
    let (mock, mut c) = setup().await;
    // WRS DM200.S 3 +15025 -05400 200 → OK（マニュアルの表記のまま送る）。
    assert_eq!(
        c.raw_command("WRS DM200.S 3 +15025 -05400 200")
            .await
            .unwrap(),
        "OK"
    );
    assert_eq!(
        c.read_many::<i16>(&a("DM200"), 3).await.unwrap(),
        [15025, -5400, 200]
    );
    // 同じ値をゼロサプレス・`+` 省略でも書ける。
    assert_eq!(
        c.raw_command("WRS DM200.S 3 +15025 -5400 200")
            .await
            .unwrap(),
        "OK"
    );

    // RDS DM200.S 3 → +15025 -25400 +00000
    c.write_many::<i16>(&a("DM200"), &[15025, -25400, 0])
        .await
        .unwrap();
    assert_eq!(
        c.raw_command("RDS DM200.S 3").await.unwrap(),
        "+15025 -25400 +00000"
    );
    assert_eq!(
        c.read_many::<i16>(&a("DM200"), 3).await.unwrap(),
        [15025, -25400, 0]
    );

    // WRS R100 4 1 0 1 0 → OK、RDS R100 4 → 1 0 1 0
    c.write_many::<bool>(&a("R100"), &[true, false, true, false])
        .await
        .unwrap();
    assert_eq!(c.raw_command("RDS R100 4").await.unwrap(), "1 0 1 0");
    assert_eq!(
        c.read_many::<bool>(&a("R100"), 4).await.unwrap(),
        [true, false, true, false]
    );
    assert_eq!(
        mock.received_commands(),
        [
            "WRS DM200.S 3 +15025 -05400 200",
            "RDS DM200.S 3",
            "WRS DM200.S 3 +15025 -5400 200",
            "WRS DM200.S 3 15025 -25400 0",
            "RDS DM200.S 3",
            "RDS DM200.S 3",
            "WRS R100 4 1 0 1 0",
            "RDS R100 4",
            "RDS R100 4",
        ]
    );
}

#[tokio::test]
async fn device_range_boundaries() {
    let (mock, mut c) = setup().await;
    // 端の番号は読み書きできる。
    c.write::<u16>(&a("DM65534"), 7).await.unwrap();
    assert_eq!(c.read::<u16>(&a("DM65534")).await.unwrap(), 7);
    assert_eq!(c.read::<u16>(&a("EM65534")).await.unwrap(), 0);
    c.write::<bool>(&a("R199915"), true).await.unwrap();
    assert!(c.read::<bool>(&a("R199915")).await.unwrap());
    assert!(!c.read::<bool>(&a("MR399915")).await.unwrap());
    assert!(!c.read::<bool>(&a("LR99915")).await.unwrap());
    assert_eq!(
        c.read_many::<bool>(&a("R199900"), 16).await.unwrap().len(),
        16
    );
    let sent = mock.request_count();

    // 範囲外のアドレスは作れない（InvalidRequest に変換される）。
    for s in [
        "DM65535", "EM65535", "R199916", "R200000", "MR400000", "LR100000",
    ] {
        let e: KvError = s.parse::<Address>().unwrap_err().into();
        assert!(matches!(e, KvError::InvalidRequest(_)), "{s}: {e:?}");
    }
    // 端から始まって範囲を超えるアクセスは送らずに断る。
    let cases = vec![
        c.read::<u32>(&a("DM65534")).await.unwrap_err(),
        c.write::<i32>(&a("EM65534"), 1).await.unwrap_err(),
        c.read::<f32>(&a("DM65534")).await.unwrap_err(),
        c.read_many::<u16>(&a("DM65534"), 2).await.unwrap_err(),
        c.read_many::<bool>(&a("R199900"), 17).await.unwrap_err(),
        c.write_many::<bool>(&a("LR99915"), &[true, true])
            .await
            .unwrap_err(),
        c.read_many::<bool>(&a("MR399915"), 2).await.unwrap_err(),
    ];
    for e in &cases {
        assert!(matches!(e, KvError::InvalidRequest(_)), "{e:?}");
    }
    assert_eq!(mock.request_count(), sent);
    assert!(c.is_connected());

    // モック自身も範囲外を E0 で断る（raw_command はライブラリの検査を通らない）。
    match c.raw_command("RD DM65535").await {
        Err(KvError::Device { code }) => assert_eq!(code, DeviceErrorCode::DeviceNumber),
        other => panic!("{other:?}"),
    }
    match c.raw_command("RD DM65534.D").await {
        Err(KvError::Device { code }) => assert_eq!(code, DeviceErrorCode::DeviceNumber),
        other => panic!("{other:?}"),
    }
    assert!(c.is_connected());
}

#[tokio::test]
async fn point_count_limits() {
    let (mock, mut c) = setup().await;
    assert_eq!(
        c.read_many::<u16>(&a("DM0"), 1000).await.unwrap().len(),
        1000
    );
    assert_eq!(
        c.read_many::<i16>(&a("EM0"), 1000).await.unwrap().len(),
        1000
    );
    assert_eq!(
        c.read_many::<bool>(&a("MR0"), 1000).await.unwrap().len(),
        1000
    );
    assert_eq!(c.read_many::<u32>(&a("DM0"), 500).await.unwrap().len(), 500);
    assert_eq!(c.read_many::<i32>(&a("DM0"), 500).await.unwrap().len(), 500);
    c.write_many::<u16>(&a("DM0"), &[1; 1000]).await.unwrap();
    c.write_many::<u32>(&a("DM0"), &[2; 500]).await.unwrap();
    let sent = mock.request_count();

    let cases = vec![
        c.read_many::<u16>(&a("DM0"), 1001).await.unwrap_err(),
        c.read_many::<bool>(&a("R0"), 1001).await.unwrap_err(),
        c.read_many::<u32>(&a("DM0"), 501).await.unwrap_err(),
        c.read_many::<f32>(&a("DM0"), 501).await.unwrap_err(),
        c.write_many::<i16>(&a("DM0"), &[0; 1001])
            .await
            .unwrap_err(),
        c.write_many::<i32>(&a("DM0"), &[0; 501]).await.unwrap_err(),
    ];
    for e in &cases {
        assert!(matches!(e, KvError::InvalidRequest(_)), "{e:?}");
    }
    assert_eq!(mock.request_count(), sent);

    // モックも上限外の点数を断る（巨大な点数でも大きな確保をしない）。
    for cmd in ["RDS DM0.U 1001", "RDS DM0.D 501", "RDS DM0.U 4294967295"] {
        match c.raw_command(cmd).await {
            Err(KvError::Device { code }) => assert_eq!(code, DeviceErrorCode::Command),
            other => panic!("{cmd}: {other:?}"),
        }
    }
    assert!(c.is_connected());
}

#[tokio::test]
async fn odd_start_32bit_is_rejected_without_sending() {
    let (mock, mut c) = setup().await;
    let cases = vec![
        c.read::<u32>(&a("DM1")).await.unwrap_err(),
        c.read::<i32>(&a("EM3")).await.unwrap_err(),
        c.read::<f32>(&a("DM5")).await.unwrap_err(),
        c.read_many::<u32>(&a("DM101"), 2).await.unwrap_err(),
        c.write::<i32>(&a("DM7"), -1).await.unwrap_err(),
        c.write::<f32>(&a("EM9"), 1.0).await.unwrap_err(),
        c.write_many::<u32>(&a("DM11"), &[1, 2]).await.unwrap_err(),
        c.write_many::<f32>(&a("DM13"), &[1.0]).await.unwrap_err(),
    ];
    for e in &cases {
        match e {
            KvError::InvalidRequest(m) => {
                assert!(m.contains("even") && m.contains("13-33"), "{m}")
            }
            other => panic!("{other:?}"),
        }
        assert!(!e.is_connection_fatal());
    }
    assert_eq!(mock.request_count(), 0);
    assert!(c.is_connected());
    // 偶数番地なら通る。16 bit は奇数番地でよい。
    c.write::<u32>(&a("DM2"), 70000).await.unwrap();
    assert_eq!(c.read::<u32>(&a("DM2")).await.unwrap(), 70000);
    assert_eq!(c.read::<u16>(&a("DM3")).await.unwrap(), 1);
}

#[tokio::test]
async fn f32_roundtrip() {
    let (mock, mut c) = setup().await;
    let values = [
        1.5f32,
        -2.25,
        0.0,
        -0.0,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::MIN_POSITIVE,
        f32::MAX,
        f32::MIN,
        -123.456,
    ];
    for v in values {
        c.write::<f32>(&a("DM100"), v).await.unwrap();
        let back = c.read::<f32>(&a("DM100")).await.unwrap();
        assert_eq!(back.to_bits(), v.to_bits(), "{v}");
    }
    c.write_many::<f32>(&a("EM0"), &values).await.unwrap();
    let back = c.read_many::<f32>(&a("EM0"), values.len()).await.unwrap();
    for (v, b) in values.iter().zip(&back) {
        assert_eq!(b.to_bits(), v.to_bits(), "{v}");
    }
    // NaN もビット列のまま往復する。
    let nan = f32::from_bits(0xFFC0_1234);
    c.write::<f32>(&a("DM0"), nan).await.unwrap();
    assert_eq!(
        c.read::<f32>(&a("DM0")).await.unwrap().to_bits(),
        0xFFC0_1234
    );

    // 下位ワードが指定番号、上位ワードが次の番号（13-33）。1.0 = 0x3F80_0000。
    c.write::<f32>(&a("DM10"), 1.0).await.unwrap();
    assert_eq!(mock.word(&a("DM10")), 0x0000);
    assert_eq!(mock.word(&a("DM11")), 0x3F80);
    c.write::<f32>(&a("DM10"), -1.0).await.unwrap();
    assert_eq!(c.read::<u32>(&a("DM10")).await.unwrap(), 0xBF80_0000);
    assert!(mock
        .received_commands()
        .contains(&"WR DM10.D 3212836864".to_owned()));
}

#[tokio::test]
async fn multi_digit_error_like_line_is_framing() {
    let (mock, mut c) = setup().await;
    // エラーコードは 1 桁だけ。`E10` はエラー応答ではなく、値として解釈できない。
    mock.inject(Fault::Garbage(b"E10\r\n".to_vec()), 1);
    let e = c.read::<u16>(&a("DM0")).await.unwrap_err();
    assert!(matches!(e, KvError::Framing(_)), "{e:?}");
    assert!(!c.is_connected());
}

#[tokio::test]
async fn overlong_token_is_framing() {
    let (mock, mut c) = setup().await;
    mock.inject(Fault::Garbage(b"000001\r\n".to_vec()), 1);
    let e = c.read::<u16>(&a("DM0")).await.unwrap_err();
    assert!(matches!(e, KvError::Framing(_)), "{e:?}");
    assert!(!c.is_connected());

    // 短い（ゼロサプレスされた）トークンは受け付ける。
    let (mock, mut c) = setup().await;
    mock.inject(Fault::Garbage(b"-1 +2 3\r\n".to_vec()), 1);
    assert_eq!(c.read_many::<i16>(&a("DM0"), 3).await.unwrap(), [-1, 2, 3]);
    assert!(c.is_connected());
}

#[tokio::test]
async fn injected_e4_and_e5_are_named_and_not_fatal() {
    let (mock, mut c) = setup().await;
    mock.inject(Fault::ErrorCode("E4".into()), 1);
    match c.write_many::<u16>(&a("DM0"), &[1, 2]).await {
        Err(KvError::Device { code }) => assert_eq!(code, DeviceErrorCode::WriteProhibited),
        other => panic!("{other:?}"),
    }
    // 注入した WRS は実行されていない。
    assert_eq!(mock.word(&a("DM0")), 0);
    mock.inject(Fault::ErrorCode("E5".into()), 1);
    match c.raw_command("M1").await {
        Err(KvError::Device { code }) => {
            assert_eq!(code, DeviceErrorCode::CpuError);
            assert_eq!(code.as_str(), "E5");
        }
        other => panic!("{other:?}"),
    }
    assert!(c.is_connected());
}

#[tokio::test]
async fn start_on_binds_given_address_and_serves_concurrent_clients() {
    // 8501 などの固定ポートは使わない（他のテストや実機と衝突する）。
    let mock = MockServer::start_on("127.0.0.1:0").await.unwrap();
    mock.set_word(&a("DM5"), 123);
    mock.set_word(&a("DM6"), 456);

    let mut c1 = KvClient::connect(mock.addr(), fast_config()).await.unwrap();
    let mut c2 = KvClient::connect(mock.addr(), fast_config()).await.unwrap();
    // 2 本とも張ったままで、交互に問い合わせても取り違えない。
    assert_eq!(c1.read::<u16>(&a("DM5")).await.unwrap(), 123);
    assert_eq!(c2.read::<u16>(&a("DM6")).await.unwrap(), 456);
    c1.write::<u16>(&a("DM7"), 9).await.unwrap();
    assert_eq!(c2.read::<u16>(&a("DM7")).await.unwrap(), 9);
    assert_eq!(mock.connection_count(), 2);
}

#[tokio::test]
async fn command_log_is_a_bounded_ring_but_requests_keep_counting() {
    let (mock, mut c) = setup().await;
    mock.set_command_log_capacity(3);
    for i in 0..5u16 {
        c.write::<u16>(&a("DM0"), i).await.unwrap();
    }
    assert_eq!(mock.request_count(), 5);
    assert_eq!(
        mock.received_commands(),
        ["WR DM0.U 2", "WR DM0.U 3", "WR DM0.U 4"]
    );

    // 追っかけ: 取りこぼし（溢れた分）と新着を数え間違えない。
    let (cmds, next, missed) = mock.commands_since(0);
    assert_eq!((cmds.len(), next, missed), (3, 5, 2));
    let (cmds, next2, missed) = mock.commands_since(next);
    assert_eq!((cmds.len(), next2, missed), (0, 5, 0));
    c.read::<u16>(&a("DM0")).await.unwrap();
    let (cmds, next3, missed) = mock.commands_since(next2);
    assert_eq!((cmds, next3, missed), (vec!["RD DM0.U".to_owned()], 6, 0));

    // 0 なら記録しないが、数えは続く。
    mock.set_command_log_capacity(0);
    c.read::<u16>(&a("DM0")).await.unwrap();
    assert!(mock.received_commands().is_empty());
    assert_eq!(mock.request_count(), 7);
}

/// `set_words` は要求の処理に対して原子的: 下位・上位ワードが連動する値を書き続ける
/// 書き手に対し、`u32` の読み出しが新下位 + 旧上位の混在を一度も観測しない。
#[tokio::test]
async fn set_words_is_atomic_against_u32_reads() {
    const N: u16 = 2000;
    let (mock, mut c) = setup().await;
    let mock = std::sync::Arc::new(mock);
    let dm10 = a("DM10");
    mock.set_words(&dm10, &[0, 0]);

    let writer = {
        let mock = std::sync::Arc::clone(&mock);
        tokio::spawn(async move {
            for k in 0..N {
                mock.set_words(&dm10, &[k, k]);
                tokio::task::yield_now().await;
            }
        })
    };
    for _ in 0..500 {
        let v = c.read::<u32>(&dm10).await.unwrap();
        assert_eq!(v & 0xFFFF, v >> 16, "混在した値を観測: {v:#010X}");
    }
    writer.await.unwrap();
}
