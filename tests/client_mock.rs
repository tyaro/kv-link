//! モックサーバーに対する結合テスト（公開 API だけを使う）。
//!
//! モックは `protocol.rs` と同じ未照合の前提から作っているので、ここで確かめて
//! いるのはクライアントの振る舞い（往復・分割受信・毒化・エラー分類）であって、
//! プロトコルの正しさではない（docs/protocol.md）。

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

    // ビット 15 の次は次チャネルのビット 0（モックの仮定、docs/protocol.md P-13）。
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
    let peer = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 64];
        let _ = s.read(&mut buf).await.unwrap();
        s.write_all(b"OK\r\n").await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        s.write_all(b"00042\r\n").await.unwrap();
        // 相手が読むまで接続を保つ。
        let _ = s.read(&mut buf).await;
    });

    let mut c = KvClient::connect(addr, fast_config()).await.unwrap();
    c.write::<u16>(&a("DM0"), 1).await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
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
