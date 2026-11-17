// SPDX-License-Identifier: CC0-1.0

//! What a deployed WEBSOCKET stage answers a correct RFC 6455 OPENING HANDSHAKE with, read
//! off the wire byte for byte, and the `AWS/ApiGateway` counters the traffic that upgrade
//! carries raises — read back through `GetMetricData`, the call a dashboard or an alarm
//! resolves a series with.
//!
//! The rule both tables are one slice of:
//!
//! > A deployed WEBSOCKET stage answers a correct opening handshake with `101 Switching
//! > Protocols` carrying `Upgrade: websocket`, `Connection: Upgrade` and the
//! > `Sec-WebSocket-Accept` the CLIENT'S OWN key derives (RFC 6455 §1.3: the base64 of the
//! > SHA-1 of the offered `Sec-WebSocket-Key` concatenated with the protocol's GUID), and
//! > every observable event of the connection that handshake opened — the connection request
//! > sent to the `$connect` route's integration, each frame the client sends, each frame the
//! > stage sends back (a route's reply and an out-of-band `@connections` push alike) and the
//! > close — raises the stage's own series in namespace `AWS/ApiGateway` under the dimension
//! > set {ApiId, Stage}.
//!
//! The rows span what the rule generalises over: the `Sec-WebSocket-Key` the client offers
//! (the specification's own worked example, and keys whose sixteen bytes are all-zero,
//! all-ones and all-distinct), the field spellings RFC 6455 §4.1 allows a client (`Upgrade`
//! and `keep-alive, Upgrade`, `websocket` and `WebSocket`, a lower-cased field name), and the
//! traffic a connection carries (none beyond its own lifecycle, one frame, several frames,
//! and frames beside out-of-band pushes).
//!
//! Nothing here is a value this emulator fills in: the handshake is written and read as raw
//! bytes on the api's announced socket, the routes are answered by functions AWS Lambda RAN,
//! and every count is what CloudWatch answers for the series — which is why Lambda, the
//! `@connections` management API and CloudWatch are mounted beside this service on one
//! engine ([`crate::websocket_stage_metrics::emulator`]).

use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde_json::{json, Value};
use sha1::Digest as _;
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::http_api_backend_dispatch::given_function;
use crate::websocket_stage_metrics::{
    closed, emulator, given_api, given_route, given_stage, pushed, ECHO_HANDLER, OK_HANDLER,
};
use crate::websocket_transport_data_plane::{
    dial_loopback, exchanged_reply, handshake_at, ClientSocket,
};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The namespace an API Gateway stage publishes its own metrics to, and the dimensions the
/// stage-wide series carry.
const NAMESPACE: &str = "AWS/ApiGateway";
const API_ID_DIMENSION: &str = "ApiId";
const STAGE_DIMENSION: &str = "Stage";

/// The GUID RFC 6455 §1.3 has a server concatenate the offered `Sec-WebSocket-Key` with
/// before hashing, which is what lets the client check the answer is a WebSocket server's.
const WEBSOCKET_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// The handshake RFC 6455 §1.3 publishes as its own worked example: the key a client offers,
/// and the accept value a server must answer it with. The one pair below taken from the
/// specification rather than computed here, so the derivation is checked against it.
const RFC_EXAMPLE_KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";
const RFC_EXAMPLE_ACCEPT: &str = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";

/// The status line RFC 6455 §4.2.2 requires of a completed handshake.
const SWITCHING_PROTOCOLS_LINE: &str = "HTTP/1.1 101 Switching Protocols";

/// How long a case waits for the socket to answer a handshake it has written, and for a push
/// to arrive over an established one: generous, so a loaded machine does not fail a case, and
/// bounded so a case that will never be answered fails instead of hanging the suite.
const ANSWER_DEADLINE: Duration = Duration::from_secs(60);

/// How long a case waits for the series to reach the values its traffic owes them, and how
/// often it looks again: the last call of a connection (`$disconnect`) is served after the
/// client has closed, so what is waited for is a datapoint the traffic ALREADY owes rather
/// than anything a clock schedules.
const SERIES_DEADLINE: Duration = Duration::from_secs(60);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// The window every read below spans, before and after the instant it is made at: wide enough
/// to hold the whole of a case's traffic whichever minute bucket each datapoint landed in.
const WINDOW: f64 = 600.0;

/// The granularity a read lays its windows at: one minute, the resolution a
/// regular-resolution datapoint is stored at.
const PERIOD: i64 = 60;

/// How many bytes of response head a case reads before giving up on the socket: far above the
/// few hundred of a server's handshake.
const MAX_HEAD_BYTES: usize = 16 * 1024;

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTAPIGWV2/20261021/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// The `Sec-WebSocket-Accept` the key `key` derives: "the base64-encoded SHA-1 of the
/// concatenation of the `Sec-WebSocket-Key` (as a string, not base64-decoded) with
/// [`WEBSOCKET_GUID`]" (RFC 6455 §1.3). Computed here rather than asked of the WebSocket
/// implementation that answers the handshake, so the two cannot agree on a wrong value.
fn accept_for(key: &str) -> String {
    let mut hash = sha1::Sha1::new();
    hash.update(key.as_bytes());
    hash.update(WEBSOCKET_GUID.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(hash.finalize())
}

/// A `Sec-WebSocket-Key` offering `bytes` as its nonce: "a base64-encoded value that, when
/// decoded, is 16 bytes in length" (RFC 6455 §4.1), encoded the way a client encodes one.
fn key_of(bytes: [u8; 16]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// One opening handshake a client writes: the key it offers, the spellings it writes the
/// upgrade fields in, and the accept value the specification publishes for it when it is the
/// worked example.
struct Offer {
    case: &'static str,
    key: String,
    /// The `Connection` field's value, as written.
    connection: &'static str,
    /// The `Upgrade` field's value, as written.
    upgrade: &'static str,
    /// The spelling of the key field's NAME, which a server reads case-insensitively.
    key_field: &'static str,
    /// The accept value RFC 6455 publishes for this key, where it publishes one.
    published_accept: Option<&'static str>,
}

/// The head the socket answered one handshake with, as the client read it off the wire.
struct Head {
    status: u16,
    /// The status line as written.
    line: String,
    /// Every field, under its lower-cased name.
    fields: BTreeMap<String, String>,
}

impl Head {
    /// The value the answer carries for `name`, read case-insensitively as a field name is.
    fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(&name.to_ascii_lowercase()).map(String::as_str)
    }
}

/// Write `offer` to `authority`'s `/{stage}` path as raw bytes and read the head the socket
/// answers it with.
///
/// The bytes are written and read directly rather than through a client library because what
/// this suite pins is the head itself: a library checks the accept value and then hides it.
async fn answered(authority: &str, stage: &str, offer: &Offer) -> Head {
    let case = offer.case;
    let mut stream = dial_loopback(case, authority).await.unwrap_or_else(|error| {
        panic!(
            "case {case:?}: the announced socket {authority:?} must accept a connection, \
             got {error}"
        )
    });
    let mut request = format!("GET /{stage} HTTP/1.1\r\n");
    for (name, value) in [
        ("Host", authority),
        ("Connection", offer.connection),
        ("Upgrade", offer.upgrade),
        (offer.key_field, offer.key.as_str()),
        ("Sec-WebSocket-Version", "13"),
    ] {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the handshake must be sent, got {error}"));
    stream
        .flush()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the handshake must be flushed, got {error}"));
    let head = tokio::time::timeout(ANSWER_DEADLINE, read_head(case, &mut stream))
        .await
        .unwrap_or_else(|_elapsed| {
            panic!(
                "case {case:?}: the opening handshake must be answered — a client that reads no \
                 status at all cannot tell a refusal from an endpoint that serves nothing"
            )
        });
    let line = head
        .lines()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected a status line, got {head:?}"))
        .to_string();
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("case {case:?}: expected a status in {line:?}"));
    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    for field in head.lines().skip(1) {
        if let Some((name, value)) = field.split_once(':') {
            fields.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    Head {
        status,
        line,
        fields,
    }
}

/// Read bytes until the response HEAD is complete, and answer it as text: a completed
/// handshake is followed by frames rather than by a body, so reading stops at the blank line
/// that ends the head (RFC 9112 §2.1).
async fn read_head(case: &str, stream: &mut tokio::net::TcpStream) -> String {
    /// The blank line that ends an HTTP response head.
    const HEAD_TERMINATOR: &[u8] = b"\r\n\r\n";
    let mut bytes: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        let read = stream
            .read(&mut chunk)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the answer must be readable: {error}"));
        assert!(
            read > 0,
            "case {case:?}: the socket closed before answering the handshake, got {:?}",
            String::from_utf8_lossy(&bytes)
        );
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(end) = bytes
            .windows(HEAD_TERMINATOR.len())
            .position(|window| window == HEAD_TERMINATOR)
        {
            return String::from_utf8_lossy(&bytes[..end]).to_string();
        }
        assert!(
            bytes.len() < MAX_HEAD_BYTES,
            "case {case:?}: the socket wrote {} bytes without ending a response head",
            bytes.len()
        );
    }
}

/// One expectation of a stage's series: the metric, the statistic read, and the value the
/// traffic owes it. `Sum` reads a counter's total; `SampleCount` reads how many observations
/// it holds, which is what a measured series (and an explicit zero) is counted by.
struct Series {
    metric: &'static str,
    statistic: &'static str,
    value: f64,
}

/// What `GetMetricData` answers for ONE series of a stage over the window this read spans:
/// the statistic folded over every value in it, which is nothing at all for a series no call
/// ever published.
async fn observed(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    stage: &str,
    series: &Series,
) -> f64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is past the epoch")
        .as_secs_f64();
    let host = format!("monitoring.{REGION}.amazonaws.com");
    let body = json!({
        "MetricDataQueries": [{
            "Id": "stage",
            "MetricStat": {
                "Metric": {
                    "Namespace": NAMESPACE,
                    "MetricName": series.metric,
                    "Dimensions": [
                        { "Name": API_ID_DIMENSION, "Value": api_id },
                        { "Name": STAGE_DIMENSION, "Value": stage },
                    ],
                },
                "Period": PERIOD,
                "Stat": series.statistic,
            },
            "ReturnData": true,
        }],
        "StartTime": now - WINDOW,
        "EndTime": now + WINDOW,
    });
    let request = smithy_server_runtime::http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", "GraniteServiceVersion20100801.GetMetricData")
        .header("authorization", authorization("monitoring"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the request builds");
    let response = emulator.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(&response.into_body()).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "case {case:?}: GetMetricData for {metric:?} must answer 200, got {status} {answered}",
        metric = series.metric
    );
    answered
        .pointer("/MetricDataResults/0/Values")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_f64).sum())
        .unwrap_or(0.0)
}

/// Wait until every expectation of `stage` holds, failing with the case, the series and what
/// was actually answered when the deadline passes.
///
/// A counted datapoint is owed by a call that has already been made, so this looks again for
/// the condition rather than waiting a fixed time for it.
async fn reached(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    stage: &str,
    expected: &[Series],
) {
    let deadline = Instant::now() + SERIES_DEADLINE;
    loop {
        let mut seen: Vec<String> = Vec::new();
        let mut all = true;
        for want in expected {
            let got = observed(emulator, case, api_id, stage, want).await;
            // Counts and sample counts are whole numbers, so half a unit apart is a different
            // answer; nothing here compares two measured durations.
            if (got - want.value).abs() > 0.5 {
                all = false;
            }
            seen.push(format!(
                "{metric} {statistic}={got} (expected {want_value})",
                metric = want.metric,
                statistic = want.statistic,
                want_value = want.value,
            ));
        }
        if all {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: stage {stage:?} of api {api_id:?} must publish the series its \
             traffic produced; got {seen:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// A counter's total.
fn total(metric: &'static str, value: f64) -> Series {
    Series {
        metric,
        statistic: "Sum",
        value,
    }
}

/// How many observations a series holds.
fn samples(metric: &'static str, value: f64) -> Series {
    Series {
        metric,
        statistic: "SampleCount",
        value,
    }
}

/// Every correct handshake is switched, with the accept value the client's own key derives,
/// and each one is counted as a connection request of the stage it addressed.
#[tokio::test]
async fn a_correct_handshake_is_switched_with_the_accept_the_clients_key_derives() {
    let case = "the opening handshakes a client writes against one deployed stage";
    let stage = "live";
    let (emulator, engine) = emulator();

    let connect = given_function(&emulator, &engine, case, "ws-upgrade-connect", OK_HANDLER).await;
    let api = given_api(&emulator, case, "upgrade-head-api").await;
    given_route(&emulator, case, &api.api_id, "$connect", &connect, false).await;
    given_stage(&emulator, case, &api.api_id, stage, false).await;

    let offers = vec![
        Offer {
            case: "the key RFC 6455 publishes its own worked example for",
            key: RFC_EXAMPLE_KEY.to_string(),
            connection: "Upgrade",
            upgrade: "websocket",
            key_field: "Sec-WebSocket-Key",
            published_accept: Some(RFC_EXAMPLE_ACCEPT),
        },
        Offer {
            case: "a nonce of sixteen zero bytes",
            key: key_of([0u8; 16]),
            connection: "Upgrade",
            upgrade: "websocket",
            key_field: "Sec-WebSocket-Key",
            published_accept: None,
        },
        Offer {
            case: "a nonce of sixteen all-ones bytes, whose base64 carries the whole alphabet's \
                   tail",
            key: key_of([0xffu8; 16]),
            connection: "keep-alive, Upgrade",
            upgrade: "WebSocket",
            key_field: "Sec-WebSocket-Key",
            published_accept: None,
        },
        Offer {
            case: "a nonce of sixteen distinct bytes, offered under a lower-cased field name",
            key: key_of([
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc,
                0xdd, 0xee, 0xff,
            ]),
            connection: "upgrade",
            upgrade: "websocket",
            key_field: "sec-websocket-key",
            published_accept: None,
        },
    ];

    for offer in &offers {
        let case = offer.case;
        let head = answered(&api.authority, stage, offer).await;
        assert_eq!(
            head.status, 101,
            "case {case:?}: a correct handshake to a deployed stage must be switched, got \
             {status} with head {line:?} {fields:?}",
            status = head.status,
            line = head.line,
            fields = head.fields
        );
        assert_eq!(
            head.line, SWITCHING_PROTOCOLS_LINE,
            "case {case:?}: expected the status line {SWITCHING_PROTOCOLS_LINE:?}, got {:?}",
            head.line
        );
        let upgrade = head.field("upgrade").unwrap_or_default();
        assert!(
            upgrade.eq_ignore_ascii_case("websocket"),
            "case {case:?}: expected the Upgrade field to name websocket, got {upgrade:?}"
        );
        let connection = head.field("connection").unwrap_or_default();
        assert!(
            connection.eq_ignore_ascii_case("upgrade"),
            "case {case:?}: expected the Connection field to name Upgrade, got {connection:?}"
        );
        let derived = accept_for(&offer.key);
        if let Some(published) = offer.published_accept {
            assert_eq!(
                derived, published,
                "case {case:?}: the derivation must answer what RFC 6455 publishes for its own \
                 example key, got {derived:?}"
            );
        }
        let accept = head.field("sec-websocket-accept").unwrap_or_default();
        assert_eq!(
            accept, derived,
            "case {case:?}: expected the accept the offered key {key:?} derives, got {accept:?}",
            key = offer.key
        );
    }

    // Every one of those handshakes was a connection request SENT to the `$connect` route's
    // integration, and the one message each connection carried: the api fronts no
    // `$disconnect` route, so the close calls nothing. Each error class stands at an explicit
    // nought over as many observations as there were calls, so a series that was never
    // published at all — which reads as nothing, and so as zero — fails these rows.
    let upgrades = offers.len() as f64;
    reached(
        &emulator,
        case,
        &api.api_id,
        stage,
        &[
            total("ConnectCount", upgrades),
            total("MessageCount", upgrades),
            samples("IntegrationLatency", upgrades),
            total("ClientError", 0.0),
            total("ExecutionError", 0.0),
            total("IntegrationError", 0.0),
            samples("ClientError", upgrades),
            samples("ExecutionError", upgrades),
            samples("IntegrationError", upgrades),
        ],
    )
    .await;
}

/// Every message a connection carries — the connection request, each frame in, each frame
/// back out, and the close — is counted into the stage's own series.
#[tokio::test]
async fn each_message_a_carried_connection_exchanges_raises_the_stage_counters() {
    let case = "connections of one api carrying different amounts of traffic";
    let (emulator, engine) = emulator();

    let lifecycle = given_function(&emulator, &engine, case, "ws-counts-cycle", OK_HANDLER).await;
    let echo = given_function(&emulator, &engine, case, "ws-counts-echo", ECHO_HANDLER).await;

    let api = given_api(&emulator, case, "message-counts-api").await;
    for (route_key, function, two_way) in [
        ("$connect", &lifecycle, false),
        ("$disconnect", &lifecycle, false),
        ("reply", &echo, true),
    ] {
        given_route(&emulator, case, &api.api_id, route_key, function, two_way).await;
    }

    // (case, the stage carrying it, frames the client sends, out-of-band pushes)
    let rows: &[(&str, &str, usize, usize)] = &[
        (
            "a connection that carries nothing beyond its own lifecycle",
            "quiet",
            0,
            0,
        ),
        ("one frame, answered by its two-way route", "single", 1, 0),
        ("two frames and one out-of-band push", "pushed", 2, 1),
        ("three frames and two out-of-band pushes", "busy", 3, 2),
    ];

    for (case, stage, frames, pushes) in rows {
        given_stage(&emulator, case, &api.api_id, stage, false).await;
        let mut socket: ClientSocket = handshake_at(case, &api.authority, &format!("/{stage}"))
            .await
            .unwrap_or_else(|status| {
                panic!("case {case:?}: the handshake must complete, got {status}")
            });

        let mut connection_id: Option<String> = None;
        for frame in 0..*frames {
            // The reply MUST come, so it is awaited for as long as `exchanged_reply` waits
            // rather than for one frame deadline: the route's function has to run first, and
            // on a loaded host that start takes longer than one.
            let replied = exchanged_reply(
                case,
                &mut socket,
                &format!(r#"{{"action":"reply","frame":{frame}}}"#),
            )
            .await;
            let event: Value = serde_json::from_str(&replied).unwrap_or_else(|error| {
                panic!("case {case:?}: expected the route's event back, got {error}: {replied:?}")
            });
            connection_id = event
                .pointer("/requestContext/connectionId")
                .and_then(Value::as_str)
                .map(str::to_string);
        }

        for push in 0..*pushes {
            let addressed = connection_id.clone().unwrap_or_else(|| {
                panic!("case {case:?}: a push needs the connection id the reply carried")
            });
            pushed(
                &emulator,
                case,
                &addressed,
                &format!(r#"{{"pushed":{push}}}"#),
            )
            .await;
            let delivered = tokio::time::timeout(
                ANSWER_DEADLINE,
                futures_util::StreamExt::next(&mut socket),
            )
            .await;
            assert!(
                matches!(&delivered, Ok(Some(Ok(_)))),
                "case {case:?}: push {push} must arrive over the socket, got {delivered:?}"
            );
        }
        closed(case, socket).await;

        // The CALLS the stage served: the connection request, each frame it dispatched to a
        // route, and the close. A push belongs to no route and is no call of its own.
        let calls = *frames as f64 + 2.0;
        // The MESSAGES it carried in either direction: the connection request, each frame in
        // with the reply its two-way route wrote back, each push out, and the close.
        let messages = 1.0 + 2.0 * *frames as f64 + *pushes as f64 + 1.0;
        reached(
            &emulator,
            case,
            &api.api_id,
            stage,
            &[
                total("ConnectCount", 1.0),
                total("MessageCount", messages),
                // One latency sample per integration that answered: every call of this api
                // fronts one.
                samples("IntegrationLatency", calls),
                total("ClientError", 0.0),
                total("ExecutionError", 0.0),
                total("IntegrationError", 0.0),
                // Each error class carries an explicit nought per CALL, so healthy traffic
                // reads as a zero rather than as a gap.
                samples("ClientError", calls),
                samples("ExecutionError", calls),
                samples("IntegrationError", calls),
            ],
        )
        .await;
    }
}
