// SPDX-License-Identifier: CC0-1.0

//! A discovered data endpoint OUTLIVES the stream it was announced for: the call made
//! there after the stream is gone is ANSWERED, with the serving operation's own modelled
//! refusal, instead of failing at the socket.
//!
//! The rule under test: **an address this emulator announces for a client to dial keeps
//! listening and keeps being answered for as long as the emulator does — the removal of the
//! resource it was announced for releases none of it** — so a request arriving there for a
//! resource that no longer resolves is refused by the service that binds the route, with
//! that operation's modelled error. `GetDataEndpoint` is the only documented way to find a
//! stream's media planes and hands back an address the application then keeps ("To read
//! data from the stream or to write data to it, specify this endpoint in your
//! application"); a published data host does not stop answering because one stream reached
//! through it was deleted. The difference matters to every caller: a modelled 404 is a
//! client error an SDK raises and does not retry, while a refused connection is a transport
//! fault SDK retry policies treat as transient and clients escalate as an outage.
//!
//! Two crates have to agree for that to hold — the Kinesis Video Streams control plane owns
//! the announced listener and the stream record, and Kinesis Video Media serves `/getMedia`
//! on the announced address — so the suite lives here, and every call it makes after the
//! discovery is dialled at the announced authority over TCP, exactly as a client holding
//! that endpoint makes it.
//!
//! The rows differ along what the rule generalises over: how much media the removed stream
//! held (none, and one ingested fragment), which identifier form addresses it
//! (`StreamName`, `StreamARN`), the start selector the read uses, and which plane's
//! announced address is dialled (the live media endpoint and the archived-media one).
//!
//! The harness is this suite's own rather than the sibling discovery suite's
//! (`kinesisvideo_discovered_data_plane.rs`, which pins that the announced address is
//! reachable at all) because what this one drives, that one has no need of: a handle on the
//! shared engine, so the removal the control plane already scheduled can be brought forward
//! instead of waited out, so a producer can put media on a stream before it is removed; and
//! an HTTP/1.1 exchange on the dialled socket that reads the REFUSAL back — its status, its
//! error code and its message — where reachability alone only has to connect.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_kinesisvideo_kinesisvideo_20170930 as kvs;
use service_com_amazonaws_kinesisvideomedia_awsacuityinletservice as kv_media;

/// The region every call here is signed for.
const REGION: &str = "us-west-2";

/// The control-plane address the calls that create and discover arrive on — a bare loopback
/// listener, the form a client on this machine reaches the emulator at.
const CONTROL_AUTHORITY: &str = "127.0.0.1:9297";

/// The one sentence Kinesis Video Media states for a stream that is not there to serve.
/// Fixed text, with no part of it taken from the request: "the same sentence covers the
/// absent-stream and the exists-but-not-ACTIVE cases".
const STREAM_NOT_FOUND_MESSAGE: &str = "The requested stream is not found or not active.";

/// A backstop for a transition the control plane's own clock owes, far longer than any span
/// it fixes: the cases poll for the CONDITION and this only bounds a hang.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// The emulator with the services this suite exercises, mounted on ONE engine as the router
/// mounts them, with request authentication off so each call carries a credential scope —
/// which is what names the Region and the service — without a computed signature.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(kvs::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(kv_media::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_kinesisvideoarchivedmedia_awsacuityreader::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    (router, engine)
}

/// What one call answered: its status and its parsed JSON body.
#[derive(Debug)]
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The error code the refusal envelope names, or `""` for a body that names none.
    fn error_code(&self) -> String {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .rsplit('#')
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// The `Message` the refusal carries, or `""` for a body that carries none.
    fn message(&self) -> String {
        self.body
            .get("Message")
            .or_else(|| self.body.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// POST one restJson1 call through the router, addressed at `authority`.
async fn call(router: &MultiServiceRouter, authority: &str, path: &str, body: &Value) -> Answer {
    let request = ::http::Request::builder()
        .method("POST")
        .uri(format!("https://{authority}{path}"))
        .header("host", authority)
        .header("content-type", "application/json")
        .header("authorization", credential_scope())
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Answer { status, body }
}

/// The SigV4 credential scope that routes a call to this family and names its Region.
fn credential_scope() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261118/{REGION}/kinesisvideo/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// The authority of an announced origin — scheme and host only, since "The returned
/// endpoint does not have the API name appended".
fn announced_authority(case: &str, endpoint: &str) -> String {
    let scheme = format!("{}://", kvs::state::DATA_PLANE_SCHEME);
    let authority = endpoint
        .strip_prefix(&scheme)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {scheme} origin, got {endpoint:?}"));
    assert!(
        !authority.is_empty() && !authority.contains('/'),
        "case {case:?}: expected scheme and host only with no path, got {endpoint:?}"
    );
    authority.to_string()
}

/// Open a connection to an announced authority, as the client handed it does.
///
/// This dial is what the whole value exists for: an address nothing listens on fails right
/// here, which is the transport failure a client sees in place of any modelled answer. The
/// announced host has to be one of this machine's own loopback addresses — the interface the
/// emulator's core binds its listeners on.
fn dial(case: &str, authority: &str) -> TcpStream {
    let (host, port) = authority.rsplit_once(':').unwrap_or_else(|| {
        panic!("case {case:?}: expected the announced endpoint to carry its port, got {authority:?}")
    });
    let port = port
        .parse::<u16>()
        .unwrap_or_else(|_| panic!("case {case:?}: expected a port, got {authority:?}"));
    let dialed = match host.trim_start_matches('[').trim_end_matches(']') {
        "127.0.0.1" => TcpStream::connect((Ipv4Addr::LOCALHOST, port)),
        "::1" => TcpStream::connect((Ipv6Addr::LOCALHOST, port)),
        "localhost" => TcpStream::connect(("localhost", port)),
        other => panic!(
            "case {case:?}: the announced endpoint must be an address of this machine, got \
             {other:?} in {authority:?}"
        ),
    };
    dialed.unwrap_or_else(|error| {
        panic!(
            "case {case:?}: the announced endpoint {authority:?} must accept a connection, \
             got {error}"
        )
    })
}

/// Send one HTTP/1.1 request to the announced authority and read the whole answer.
///
/// The request goes onto the connection as it stands, over the transport the announced
/// scheme names. `Connection: close` is what makes the answer end at EOF, so no body framing
/// has to be re-implemented here.
fn exchange(case: &str, authority: &str, path: &str, body: &[u8]) -> Answer {
    let mut conn = dial(case, authority);
    let head = format!(
        "POST {path} HTTP/1.1\r\n\
         host: {authority}\r\n\
         content-type: application/json\r\n\
         authorization: {}\r\n\
         content-length: {}\r\n\
         connection: close\r\n\r\n",
        credential_scope(),
        body.len()
    );
    conn.write_all(head.as_bytes())
        .unwrap_or_else(|error| panic!("case {case:?}: writing to {authority:?} failed: {error}"));
    conn.write_all(body)
        .unwrap_or_else(|error| panic!("case {case:?}: writing to {authority:?} failed: {error}"));
    conn.flush()
        .unwrap_or_else(|error| panic!("case {case:?}: writing to {authority:?} failed: {error}"));

    let mut answered = Vec::new();
    conn.read_to_end(&mut answered)
        .unwrap_or_else(|error| panic!("case {case:?}: reading {authority:?} failed: {error}"));
    let (head, tail) = split_response(case, authority, &answered);
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|status| status.parse::<u16>().ok())
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected an HTTP status line from {authority:?}, got {head:?}")
        });
    Answer {
        status,
        body: serde_json::from_slice(tail).unwrap_or(Value::Null),
    }
}

/// The header block and the body of one HTTP/1.1 answer, split at the blank line.
fn split_response<'a>(case: &str, authority: &str, answered: &'a [u8]) -> (String, &'a [u8]) {
    let separator = answered
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected an HTTP response from {authority:?}, got {:?}",
                String::from_utf8_lossy(answered)
            )
        });
    (
        String::from_utf8_lossy(&answered[..separator]).to_string(),
        &answered[separator + 4..],
    )
}

/// Send one call to the announced authority off the runtime's worker threads: the exchange
/// is blocking, and what answers it is this same process.
async fn dialled_call(case: &str, authority: &str, path: &str, body: Value) -> Answer {
    let case = case.to_string();
    let authority = authority.to_string();
    let path = path.to_string();
    let body = serde_json::to_vec(&body).expect("the request body serializes");
    tokio::task::spawn_blocking(move || exchange(&case, &authority, &path, &body))
        .await
        .expect("the dialling task joins")
}

/// The `DataEndpoint` the control plane announces for `api_name` on `stream`.
async fn data_endpoint(router: &MultiServiceRouter, stream: &str, api_name: &str) -> String {
    let answer = call(
        router,
        CONTROL_AUTHORITY,
        "/getDataEndpoint",
        &json!({ "StreamName": stream, "APIName": api_name }),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {api_name:?}: GetDataEndpoint expected HTTP 200, got {answer:?}"
    );
    answer
        .body
        .get("DataEndpoint")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {api_name:?}: no DataEndpoint in {answer:?}"))
        .to_string()
}

/// Create `stream` and answer the `StreamARN` the control plane minted.
async fn create_stream(router: &MultiServiceRouter, stream: &str) -> String {
    let answer = call(
        router,
        CONTROL_AUTHORITY,
        "/createStream",
        &json!({ "StreamName": stream, "DataRetentionInHours": 1 }),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case \"create {stream}\": expected HTTP 200, got {answer:?}"
    );
    answer
        .body
        .get("StreamARN")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case \"create {stream}\": no StreamARN in {answer:?}"))
        .to_string()
}

/// Wait until the control plane reports `stream` as `ACTIVE` — the status its own clock owes
/// a freshly created stream ("CreateStream is an asynchronous operation") and the one
/// `DeleteStream` requires.
///
/// Waits for the CONDITION, not for a span: it looks again until the status holds, under a
/// deadline far longer than the transition needs.
async fn await_active(router: &MultiServiceRouter, stream: &str) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let answer = call(
            router,
            CONTROL_AUTHORITY,
            "/describeStream",
            &json!({ "StreamName": stream }),
        )
        .await;
        if answer.body["StreamInfo"]["Status"].as_str() == Some("ACTIVE") {
            return;
        }
        last = answer.body;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("case \"activate {stream}\": never reached ACTIVE, last read {last:?}");
}

/// Delete `stream` and wait until the control plane no longer holds its record.
///
/// `DeleteStream` "marks the stream for deletion" and the removal falls due at an instant
/// the control plane itself fixed; bringing that instant forward is what lets the case read
/// the state a caller arriving after the span would have seen without waiting the span out.
/// Which record is gone is then read through `DescribeStream`, polled for the condition.
async fn remove_stream(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    stream: &str,
    arn: &str,
) {
    await_active(router, stream).await;
    let deleted = call(
        router,
        CONTROL_AUTHORITY,
        "/deleteStream",
        &json!({ "StreamARN": arn }),
    )
    .await;
    assert_eq!(
        deleted.status, 200,
        "case \"delete {stream}\": expected HTTP 200, got {deleted:?}"
    );
    {
        let state = kvs::state::service_state(&**engine).expect("the control-plane store");
        let mut store = state.lock();
        let record = store.streams.get_mut(stream).unwrap_or_else(|| {
            panic!("case \"delete {stream}\": the record must still be there to fall due")
        });
        assert!(
            record.deletes_at.is_some(),
            "case \"delete {stream}\": the delete must have scheduled the removal"
        );
        record.deletes_at = Some(Instant::now());
    }

    let deadline = Instant::now() + SETTLE_DEADLINE;
    while Instant::now() < deadline {
        let described = call(
            router,
            CONTROL_AUTHORITY,
            "/describeStream",
            &json!({ "StreamName": stream }),
        )
        .await;
        if described.status == 404 {
            assert_eq!(
                described.error_code(),
                "ResourceNotFoundException",
                "case \"delete {stream}\": a removed stream is a missing resource, got {described:?}"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("case \"delete {stream}\": the record was never removed");
}

/// Put `chunks` on the stream's media log, as a producer session does.
///
/// Through the ingest write Kinesis Video Media's own store exposes, rather than over the
/// `/putMedia` wire: what this suite is about is the LIFETIME of the announced address, and
/// the only thing a row needs from the ingest is that the removed stream had held media.
/// The wire ingest (a Matroska container, its acknowledgements and their fragment numbers)
/// is covered by that crate's own suite.
fn produce(engine: &Arc<engine::InMemoryEngine>, stream: &str, chunks: &[&[u8]]) {
    let state = kv_media::state::service_state(&**engine).expect("the media store");
    let mut store = state.lock();
    for (index, chunk) in chunks.iter().enumerate() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_millis(1_761_000_000_000 + index as u64);
        store
            .put_fragment(&**engine, Some(stream), None, chunk.to_vec(), at, at)
            .unwrap_or_else(|| panic!("case \"produce to {stream}\": a live stream takes media"));
    }
}

/// One row of the table: how much media the stream held before it was removed.
struct Row {
    case: &'static str,
    stream: &'static str,
    chunks: &'static [&'static [u8]],
}

/// The rows differ in what the removed stream held: nothing at all, and one ingested
/// fragment that `GetMedia` had already read back.
const ROWS: &[Row] = &[
    Row {
        case: "a removed stream that held no media",
        stream: "announced-endpoint-empty",
        chunks: &[],
    },
    Row {
        case: "a removed stream that held an ingested fragment",
        stream: "announced-endpoint-ingested",
        chunks: &[b"one-ingested-fragment"],
    },
];

/// RULE: the announced endpoint keeps answering after the stream it was announced for is
/// removed, so the read made there is refused with the modelled missing-resource error
/// instead of failing at the socket.
#[tokio::test(flavor = "multi_thread")]
async fn a_read_at_the_announced_endpoint_is_refused_once_the_stream_is_removed() {
    let (router, engine) = emulator();

    for row in ROWS {
        let arn = create_stream(&router, row.stream).await;
        produce(&engine, row.stream, row.chunks);
        await_active(&router, row.stream).await;

        // The discovery a client is told to make, and the only endpoint it is given.
        let read_endpoint = data_endpoint(&router, row.stream, "GET_MEDIA").await;
        let read_authority = announced_authority(row.case, &read_endpoint);
        let archive_endpoint = data_endpoint(&router, row.stream, "LIST_FRAGMENTS").await;
        let archive_authority = announced_authority(row.case, &archive_endpoint);
        assert_ne!(
            read_authority, CONTROL_AUTHORITY,
            "case {:?}: the data authority must not be the control-plane one, got {read_endpoint:?}",
            row.case
        );

        // While the stream exists, that address serves the read.
        let live = dialled_call(
            row.case,
            &read_authority,
            "/getMedia",
            json!({
                "StreamName": row.stream,
                "StartSelector": { "StartSelectorType": "EARLIEST" },
            }),
        )
        .await;
        assert_eq!(
            live.status, 200,
            "case {:?}: the announced endpoint must serve the live stream, got {live:?}",
            row.case
        );

        remove_stream(&router, &engine, row.stream, &arn).await;

        // The SAME client, on the SAME endpoint, after the stream record is gone. The rows
        // span the identifier form and the start selector, because the refusal is about the
        // stream no longer resolving and not about how the read was addressed.
        for (form, addressing) in [
            ("StreamName", json!({ "StreamName": row.stream })),
            ("StreamARN", json!({ "StreamARN": arn })),
        ] {
            for selector in ["EARLIEST", "NOW"] {
                let case = format!("{} read by {form} from {selector}", row.case);
                let mut body = addressing.as_object().cloned().unwrap_or_default();
                body.insert(
                    "StartSelector".to_string(),
                    json!({ "StartSelectorType": selector }),
                );
                let refused =
                    dialled_call(&case, &read_authority, "/getMedia", Value::Object(body)).await;
                assert_eq!(
                    refused.status, 404,
                    "case {case:?}: expected HTTP 404 from the announced endpoint, got {refused:?}"
                );
                assert_eq!(
                    refused.error_code(),
                    "ResourceNotFoundException",
                    "case {case:?}: expected the modelled missing resource, got {refused:?}"
                );
                assert_eq!(
                    refused.message(),
                    STREAM_NOT_FOUND_MESSAGE,
                    "case {case:?}: expected the fixed sentence, got {refused:?}"
                );
            }
        }

        // The other plane's announced address is held on the same terms, and the rule is the
        // same there: the archived-media endpoint still answers, with ITS own modelled
        // refusal for a stream that is no longer there to read. `FragmentSelector` is
        // required on a first page ("This is only required when the NextToken isn't passed
        // in the API"), so the call is a complete one and the refusal is about the stream.
        let case = format!("{} listed on the archived-media endpoint", row.case);
        let listed = dialled_call(
            &case,
            &archive_authority,
            "/listFragments",
            json!({
                "StreamName": row.stream,
                "FragmentSelector": {
                    "FragmentSelectorType": "SERVER_TIMESTAMP",
                    "TimestampRange": { "StartTimestamp": 0, "EndTimestamp": 4_000_000_000_i64 },
                },
            }),
        )
        .await;
        assert_eq!(
            listed.status, 404,
            "case {case:?}: expected HTTP 404 from the announced endpoint, got {listed:?}"
        );
        assert_eq!(
            listed.error_code(),
            "ResourceNotFoundException",
            "case {case:?}: expected the modelled missing resource, got {listed:?}"
        );
        assert_eq!(
            listed.message(),
            STREAM_NOT_FOUND_MESSAGE,
            "case {case:?}: expected the fixed sentence, got {listed:?}"
        );

        // And keeping the addresses did not keep the STREAM: discovery for it is refused.
        let discovered = call(
            &router,
            CONTROL_AUTHORITY,
            "/getDataEndpoint",
            &json!({ "StreamName": row.stream, "APIName": "GET_MEDIA" }),
        )
        .await;
        assert_eq!(
            discovered.status, 404,
            "case {:?}: discovery for a removed stream must be refused, got {discovered:?}",
            row.case
        );
    }
}
