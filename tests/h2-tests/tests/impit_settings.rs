//! impit fork: `IMPIT_H2_OMIT_SETTINGS` and `IMPIT_H2_NO_RFC7540_PRIORITIES` change the client's first
//! SETTINGS frame and nothing else, except that with ENABLE_PUSH left out a pushed stream is refused
//! instead of closing the connection. The variables are process-wide, so these tests are a test
//! binary of their own and take turns (`with_env`).

use futures::StreamExt;
use h2_support::prelude::*;
use std::future::Future;
use std::sync::Mutex;

const OMIT_SETTINGS: &str = "IMPIT_H2_OMIT_SETTINGS";
const NO_RFC7540_PRIORITIES: &str = "IMPIT_H2_NO_RFC7540_PRIORITIES";

static ENV: Mutex<()> = Mutex::new(());

/// Runs `test` to completion with the two variables set to `omit` and `no_rfc7540_priorities`
/// (`None`: unset), while no other test in this file runs.
fn with_env<F: Future>(
    omit: Option<&str>,
    no_rfc7540_priorities: Option<&str>,
    test: F,
) -> F::Output {
    let _turn = ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    for (name, value) in [
        (OMIT_SETTINGS, omit),
        (NO_RFC7540_PRIORITIES, no_rfc7540_priorities),
    ] {
        match value {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
    }

    let output = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(test);

    std::env::remove_var(OMIT_SETTINGS);
    std::env::remove_var(NO_RFC7540_PRIORITIES);
    output
}

/// A client set up like hyper's, which always sends these three settings.
fn hyper_like_client() -> client::Builder {
    let mut builder = client::Builder::new();
    builder
        .max_frame_size(16_384)
        .max_header_list_size(16_384)
        .enable_push(false);
    builder
}

/// The client's first SETTINGS frame, as the server reads it.
async fn client_settings_frame() -> frame::Settings {
    let (io, mut srv) = mock::new();

    let srv = async move { srv.assert_client_handshake().await };

    let client = async move {
        let (_client, conn) = hyper_like_client()
            .handshake::<_, Bytes>(io)
            .await
            .expect("handshake");
        conn.await.expect("connection");
    };

    join(srv, client).await.0
}

#[test]
fn client_settings_frame_without_env_is_unchanged() {
    h2_support::trace_init!();
    let settings = with_env(None, None, client_settings_frame());

    assert_eq!(settings.max_frame_size(), Some(16_384));
    assert_eq!(settings.max_header_list_size(), Some(16_384));
    assert_eq!(settings.is_push_enabled(), Some(false));
    assert_frame_eq(
        settings.clone(),
        frames::settings()
            .disable_push()
            .max_frame_size(16_384)
            .max_header_list_size(16_384),
    );

    // Empty is the same as unset.
    assert_eq!(
        with_env(Some(""), Some(""), client_settings_frame()),
        settings
    );
}

#[test]
fn client_settings_frame_leaves_out_5_and_6() {
    h2_support::trace_init!();
    let settings = with_env(Some("5,6"), None, client_settings_frame());

    assert_eq!(settings.max_frame_size(), None);
    assert_eq!(settings.max_header_list_size(), None);
    assert_eq!(settings.is_push_enabled(), Some(false));
    assert_frame_eq(settings, frames::settings().disable_push());
}

/// Connects a client built by `builder` to a peer that expects the client's first SETTINGS frame to
/// be `settings`, byte for byte (`mock_io` panics on any other byte). The mock server can't check
/// setting 9: it drops it when it reads the frame.
async fn client_writes_settings(builder: client::Builder, settings: &[u8]) {
    let mock = mock_io::Builder::new()
        .write(MAGIC_PREFACE)
        .write(settings)
        .read(frames::SETTINGS)
        .read(frames::SETTINGS_ACK)
        .write(frames::SETTINGS_ACK)
        .build();
    let (_client, conn) = builder
        .handshake::<_, Bytes>(mock)
        .await
        .expect("handshake");
    conn.await.expect("connection");
}

#[test]
fn setting_9_is_on_the_wire() {
    h2_support::trace_init!();

    // The default builder sends no settings of its own, so 9 is the only one.
    #[rustfmt::skip]
    let only_9 = [
        0, 0, 6, 4, 0, 0, 0, 0, 0, // SETTINGS, length 6, no flags, stream 0
        0, 9, 0, 0, 0, 1,          // NO_RFC7540_PRIORITIES = 1
    ];
    with_env(
        None,
        Some("1"),
        client_writes_settings(client::Builder::new(), &only_9),
    );

    // h2 writes the builder's settings in id order, and 9 after them.
    #[rustfmt::skip]
    let after_hyper_like = [
        0, 0, 24, 4, 0, 0, 0, 0, 0, // SETTINGS, length 24, no flags, stream 0
        0, 2, 0, 0, 0, 0,           // ENABLE_PUSH = 0
        0, 5, 0, 0, 0x40, 0,        // MAX_FRAME_SIZE = 16384
        0, 6, 0, 0, 0x40, 0,        // MAX_HEADER_LIST_SIZE = 16384
        0, 9, 0, 0, 0, 0,           // NO_RFC7540_PRIORITIES = 0
    ];
    with_env(
        None,
        Some("0"),
        client_writes_settings(hyper_like_client(), &after_hyper_like),
    );
}

/// How the client dealt with a server that broke one of the client's own limits.
#[derive(Debug, PartialEq)]
struct Outcome {
    /// The frame the client answered with.
    answer: frame::Frame,
    /// The error the response, or its body, ended with.
    request: String,
    /// What the connection ended with.
    connection: Result<(), String>,
}

/// The server sends a DATA frame of 16385 bytes, one over the client's MAX_FRAME_SIZE.
async fn oversized_data() -> Outcome {
    let (io, mut srv) = mock::new();
    // A bad peer: the mock's codec would refuse to send the frame otherwise.
    srv.codec_mut().set_max_send_frame_size(16_384 * 4);

    let srv = async move {
        srv.assert_client_handshake().await;
        srv.recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .await;
        srv.send_frame(frames::headers(1).response(200)).await;
        srv.send_frame(frames::data(1, vec![0; 16_385]).eos()).await;
        srv.next().await.expect("answer").expect("frame")
    };

    let client = async move {
        let (mut client, conn) = hyper_like_client()
            .handshake::<_, Bytes>(io)
            .await
            .expect("handshake");
        let connection = async move { conn.await.map_err(|e| e.to_string()) };
        let request = async move {
            let response = client.get("https://example.com/").await.expect("response");
            let body = util::concat(response.into_body()).await;
            body.expect_err("body").to_string()
        };
        join(connection, request).await
    };

    let (answer, (connection, request)) = join(srv, client).await;
    Outcome {
        answer,
        request,
        connection,
    }
}

/// The server sends a DATA frame of 20000 bytes to a client built with `max_frame_size(32_768)`:
/// over the initial 16384, within the client's own limit. Returns how the connection ended and the
/// length of the body the client read.
async fn data_within_32768_limit() -> (Result<(), String>, Result<usize, String>) {
    let (io, mut srv) = mock::new();
    // The mock's codec would refuse to send the frame otherwise.
    srv.codec_mut().set_max_send_frame_size(32_768);

    let srv = async move {
        srv.assert_client_handshake().await;
        srv.recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .await;
        srv.send_frame(frames::headers(1).response(200)).await;
        srv.send_frame(frames::data(1, vec![0; 20_000]).eos()).await;
    };

    let client = async move {
        let (mut client, conn) = hyper_like_client()
            .max_frame_size(32_768)
            .handshake::<_, Bytes>(io)
            .await
            .expect("handshake");
        let connection = async move { conn.await.map_err(|e| e.to_string()) };
        let request = async move {
            let response = client.get("https://example.com/").await.expect("response");
            let body = util::concat(response.into_body()).await;
            body.map(|body| body.len()).map_err(|e| e.to_string())
        };
        join(connection, request).await
    };

    join(srv, client).await.1
}

/// The server answers with a header list over the client's MAX_HEADER_LIST_SIZE (16384).
async fn oversized_header_list() -> Outcome {
    let (io, mut srv) = mock::new();

    let srv = async move {
        srv.assert_client_handshake().await;
        srv.recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .await;
        srv.send_frame(
            frames::headers(1)
                .response(200)
                .field("x-large", "a".repeat(16_384)),
        )
        .await;
        srv.next().await.expect("answer").expect("frame")
    };

    let client = async move {
        let (mut client, conn) = hyper_like_client()
            .handshake::<_, Bytes>(io)
            .await
            .expect("handshake");
        let connection = async move { conn.await.map_err(|e| e.to_string()) };
        let request = async move {
            let response = client.get("https://example.com/").await;
            response.expect_err("response").to_string()
        };
        join(connection, request).await
    };

    let (answer, (connection, request)) = join(srv, client).await;
    Outcome {
        answer,
        request,
        connection,
    }
}

#[test]
fn omitted_settings_keep_local_limits() {
    h2_support::trace_init!();

    let data = with_env(Some("5,6"), None, oversized_data());
    assert_eq!(data, with_env(None, None, oversized_data()));
    assert_frame_eq(data.answer, frames::go_away(0).frame_size());

    // The builder's limit, not the initial 16384 the server is told when 5 is left out.
    let within = with_env(Some("5"), None, data_within_32768_limit());
    assert_eq!(within, with_env(None, None, data_within_32768_limit()));
    assert_eq!(within, (Ok(()), Ok(20_000)));

    let headers = with_env(Some("5,6"), None, oversized_header_list());
    assert_eq!(headers, with_env(None, None, oversized_header_list()));
    assert_frame_eq(headers.answer, frames::reset(1).protocol_error());
}

/// A server that pushes to a client with push disabled whose first SETTINGS frame left ENABLE_PUSH
/// out (`IMPIT_H2_OMIT_SETTINGS=2`). Each promise on stream 1 (promised streams `promised`, in
/// order) must be refused with RST_STREAM REFUSED_STREAM, stream 1's response must arrive in full,
/// and the connection must stay up: the client's next frame is its second request, not a GOAWAY.
/// With `pushed_frames`, the server also sends HEADERS and DATA on each promised stream after the
/// refusals, as a server whose frames were already in flight would; the client must ignore them.
async fn refused_push(promised: &[u32], pushed_frames: bool) {
    let (io, mut srv) = mock::new();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_eq!(settings.is_push_enabled(), None, "ENABLE_PUSH left out");
        srv.recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .await;
        for &id in promised {
            srv.send_frame(
                frames::push_promise(1, id).request("GET", "https://example.com/style.css"),
            )
            .await;
            srv.recv_frame(frames::reset(id).refused()).await;
        }
        srv.send_frame(frames::headers(1).response(200)).await;
        if pushed_frames {
            for &id in promised {
                srv.send_frame(frames::headers(id).response(200)).await;
                srv.send_frame(frames::data(id, "pushed").eos()).await;
            }
        }
        srv.send_frame(frames::data(1, "hello").eos()).await;
        srv.recv_frame(
            frames::headers(3)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .await;
        srv.send_frame(frames::headers(3).response(200).eos()).await;
    };

    let client = async move {
        let (mut client, mut conn) = client::Builder::new()
            .enable_push(false)
            .handshake::<_, Bytes>(io)
            .await
            .expect("handshake");
        let requests = async {
            let response = client.get("https://example.com/").await.expect("response");
            assert_eq!(response.status(), StatusCode::OK);
            let body = util::concat(response.into_body()).await.expect("body");
            assert_eq!(body, "hello");

            let response = client
                .get("https://example.com/")
                .await
                .expect("second response");
            assert_eq!(response.status(), StatusCode::OK);
        };
        conn.drive(requests).await;
        conn.await.expect("connection");
        drop(client);
    };

    join(srv, client).await;
}

#[test]
fn pushed_stream_refused_when_enable_push_left_out() {
    h2_support::trace_init!();
    with_env(Some("2"), None, refused_push(&[2], false));
}

#[test]
fn pushed_stream_frames_after_refusal_are_ignored() {
    h2_support::trace_init!();
    with_env(Some("2"), None, refused_push(&[2], true));
}

#[test]
fn second_pushed_stream_is_refused_too() {
    h2_support::trace_init!();
    with_env(Some("2"), None, refused_push(&[2, 4], false));
}

/// Like any stream h2 resets itself, a refused pushed stream counts toward the connection's
/// local-reset limit: with a limit of 1, the first promise is refused and the second ends the
/// connection with GOAWAY ENHANCE_YOUR_CALM.
#[test]
fn refused_pushed_streams_count_toward_local_reset_limit() {
    h2_support::trace_init!();

    with_env(Some("2"), None, async {
        let (io, mut srv) = mock::new();

        let srv = async move {
            let settings = srv.assert_client_handshake().await;
            assert_eq!(settings.is_push_enabled(), None, "ENABLE_PUSH left out");
            srv.recv_frame(
                frames::headers(1)
                    .request("GET", "https://example.com/")
                    .eos(),
            )
            .await;
            srv.send_frame(
                frames::push_promise(1, 2).request("GET", "https://example.com/style.css"),
            )
            .await;
            srv.recv_frame(frames::reset(2).refused()).await;
            srv.send_frame(
                frames::push_promise(1, 4).request("GET", "https://example.com/style.css"),
            )
            .await;
            srv.recv_frame(frames::go_away(0).calm().data("too_many_internal_resets"))
                .await;
        };

        let client = async move {
            let (mut client, conn) = client::Builder::new()
                .enable_push(false)
                .max_local_error_reset_streams(Some(1))
                .handshake::<_, Bytes>(io)
                .await
                .expect("handshake");
            let request = async move {
                let err = client
                    .get("https://example.com/")
                    .await
                    .expect_err("response");
                assert_eq!(err.reason(), Some(Reason::ENHANCE_YOUR_CALM));
            };
            let connection = async move {
                let err = conn.await.expect_err("connection");
                assert!(err.is_go_away());
                assert!(err.is_library());
                assert_eq!(err.reason(), Some(Reason::ENHANCE_YOUR_CALM));
            };
            join(connection, request).await;
        };

        join(srv, client).await;
    });
}

/// A server that promises the streams `promised`, in order, on stream 1 to a client that left
/// ENABLE_PUSH out. All but the last are valid and refused with RST_STREAM REFUSED_STREAM. The last
/// has a bad promised id, so it must end the connection with GOAWAY PROTOCOL_ERROR, as it does with
/// push enabled: h2 checks the id before it refuses the stream.
async fn bad_promised_id(promised: &[u32]) {
    let (io, mut srv) = mock::new();

    let srv = async move {
        let settings = srv.assert_client_handshake().await;
        assert_eq!(settings.is_push_enabled(), None, "ENABLE_PUSH left out");
        srv.recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .await;
        let (&bad, valid) = promised.split_last().expect("a promised id");
        for &id in valid {
            srv.send_frame(
                frames::push_promise(1, id).request("GET", "https://example.com/style.css"),
            )
            .await;
            srv.recv_frame(frames::reset(id).refused()).await;
        }
        srv.send_frame(
            frames::push_promise(1, bad).request("GET", "https://example.com/style.css"),
        )
        .await;
        srv.recv_frame(frames::go_away(0).protocol_error()).await;
    };

    let client = async move {
        let (mut client, conn) = client::Builder::new()
            .enable_push(false)
            .handshake::<_, Bytes>(io)
            .await
            .expect("handshake");
        let request = async move {
            let err = client
                .get("https://example.com/")
                .await
                .expect_err("response");
            assert_eq!(
                err.to_string(),
                "connection error detected: unspecific protocol error detected"
            );
        };
        let connection = async move {
            let err = conn.await.expect_err("connection");
            assert_eq!(
                err.to_string(),
                "connection error detected: unspecific protocol error detected"
            );
        };
        join(connection, request).await;
    };

    join(srv, client).await;
}

#[test]
fn bad_promised_id_with_push_left_out_is_conn_error() {
    h2_support::trace_init!();
    // Not server-initiated.
    with_env(Some("2"), None, bad_promised_id(&[3]));
    // Already used, by the refused stream.
    with_env(Some("2"), None, bad_promised_id(&[2, 2]));
}

/// A server that answers stream 1 in full and then promises a stream on it, to a client with push
/// disabled. The parent stream is closed, so the promise must end the connection with GOAWAY
/// PROTOCOL_ERROR. The client keeps the response, so h2 still has stream 1.
async fn push_on_closed_parent() {
    let (io, mut srv) = mock::new();

    let srv = async move {
        srv.assert_client_handshake().await;
        srv.recv_frame(
            frames::headers(1)
                .request("GET", "https://example.com/")
                .eos(),
        )
        .await;
        srv.send_frame(frames::headers(1).response(200).eos()).await;
        // The client has read the response before the promise arrives.
        srv.ping_pong([1; 8]).await;
        srv.send_frame(frames::push_promise(1, 2).request("GET", "https://example.com/style.css"))
            .await;
        srv.recv_frame(frames::go_away(0).protocol_error()).await;
    };

    let client = async move {
        let (mut client, mut conn) = client::Builder::new()
            .enable_push(false)
            .handshake::<_, Bytes>(io)
            .await
            .expect("handshake");
        let response = conn
            .drive(client.get("https://example.com/"))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let err = conn.await.expect_err("connection");
        assert_eq!(
            err.to_string(),
            "connection error detected: unspecific protocol error detected"
        );
        drop((client, response));
    };

    join(srv, client).await;
}

/// h2 checks the parent stream before it refuses a pushed stream, so leaving ENABLE_PUSH out
/// changes nothing here.
#[test]
fn push_promise_on_closed_parent_is_still_conn_error() {
    h2_support::trace_init!();
    with_env(Some("2"), None, push_on_closed_parent());
    with_env(None, None, push_on_closed_parent());
}

/// `recv_push_when_push_disabled_is_conn_error` (push_promise.rs) in this binary, with a promised
/// id the client would accept: with ENABLE_PUSH = 0 sent, a PUSH_PROMISE is still a connection
/// error.
#[test]
fn push_promise_with_enable_push_sent_is_still_conn_error() {
    h2_support::trace_init!();

    with_env(None, None, async {
        let (io, mut srv) = mock::new();
        let mock = async move {
            let settings = srv.assert_client_handshake().await;
            assert_eq!(settings.is_push_enabled(), Some(false));
            srv.recv_frame(
                frames::headers(1)
                    .request("GET", "https://http2.akamai.com/")
                    .eos(),
            )
            .await;
            srv.send_frame(
                frames::push_promise(1, 2).request("GET", "https://http2.akamai.com/style.css"),
            )
            .await;
            srv.send_frame(frames::headers(1).response(200).eos()).await;
            srv.recv_frame(frames::go_away(0).protocol_error()).await;
        };

        let h2 = async move {
            let (mut client, h2) = client::Builder::new()
                .enable_push(false)
                .handshake::<_, Bytes>(io)
                .await
                .unwrap();
            let request = Request::builder()
                .method(Method::GET)
                .uri("https://http2.akamai.com/")
                .body(())
                .unwrap();

            let req = async move {
                let res = client.send_request(request, true).unwrap().0.await;
                let err = res.unwrap_err();
                assert_eq!(
                    err.to_string(),
                    "connection error detected: unspecific protocol error detected"
                );
            };

            // client should see a protocol error
            let conn = async move {
                let res = h2.await;
                let err = res.unwrap_err();
                assert_eq!(
                    err.to_string(),
                    "connection error detected: unspecific protocol error detected"
                );
            };

            join(conn, req).await;
        };

        join(mock, h2).await;
    });
}
