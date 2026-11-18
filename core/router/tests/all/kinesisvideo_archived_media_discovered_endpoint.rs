// SPDX-License-Identifier: CC0-1.0

//! The archived-media plane a client reaches by FOLLOWING `GetDataEndpoint`, dialled by a
//! client that holds no trust material of this emulator.
//!
//! RULE: an endpoint an operation hands a caller to dial is announced under the scheme of
//! the transport its listener actually speaks, so a default-configured client connects to
//! the origin exactly as announced — nothing negotiated in between and no trust material
//! it was not already given — and the operations of that plane answer there with their
//! modelled responses.
//!
//! `GetDataEndpoint` is the only documented way to find a stream's media planes ("Gets an
//! endpoint for a specified stream for either reading or writing … The returned endpoint
//! does not have the API name appended. The client needs to add the API name to the
//! returned endpoint"), and the value exists to be DIALLED ("To read data from the stream
//! or to write data to it, specify this endpoint in your application"). A client therefore
//! builds its client on the announced origin with the configuration it already has: an
//! origin whose scheme promises a transport the listener does not serve is refused in
//! connection setup, before a single request is sent, and the whole discovered plane is
//! unreachable however well-formed the announced value looks. Which address belongs to
//! which plane is pinned by the control plane's own suites; what this suite adds is the
//! transport: the announced origin is fetched by a STOCK HTTP client with its default
//! verification left on, and the archived-media operations are judged on the answers that
//! client reads back over it.
//!
//! Three crates have to agree for that to hold — Kinesis Video Streams announces the
//! address and opens its listener, and Kinesis Video Archived Media binds the six reads
//! served there (Kinesis Video Media is mounted too, so the planes are partitioned exactly
//! as the assembled emulator partitions them) — so the suite lives here.
//!
//! The rows differ along what the rule generalises over: which `APIName` was discovered,
//! which endpoint of this family it belongs to (the archived-media one the issue is about
//! and the live media one beside it), which of that endpoint's routes the discovered origin
//! is then asked for, which identifier form addresses the stream, and which response class
//! the request earns — a success, a bad argument, a stream that cannot be read, a stream
//! that retains nothing, and a member outside a modelled constraint.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_kinesisvideo_kinesisvideo_20170930 as kvs;
use service_com_amazonaws_kinesisvideomedia_awsacuityinletservice as kv_media;

/// The region every call in this suite is signed for.
const REGION: &str = "us-west-2";

/// The control-plane address the discovery calls arrive on: the loopback form a client on
/// the machine this emulator runs on reaches it at.
const CONTROL_AUTHORITY: &str = "127.0.0.1:9297";

/// A stream that retains data, so the reads of its archive are served rather than refused
/// for its retention.
const RETAINED: &str = "kvam-discovered-retained";

/// A stream created with the service's documented default retention of 0 hours, which is
/// what the no-retention refusal is about.
const UNRETAINED: &str = "kvam-discovered-unretained";

/// A stream name no record of this store answers to.
const ABSENT: &str = "kvam-discovered-absent";

/// The media type every stream here is created with, the type the control plane records for
/// a stream's media.
const MEDIA_TYPE: &str = "video/h264";

/// The archived-media `APIName` values, every one of which resolves to the one endpoint
/// that binds the reads of a stream's stored fragments. Read off the model's `APIName`
/// enum (`smithy_query(query="shape",shape="APIName")`, mirrored in
/// `kvs::ops::types::A_P_I_NAME_VALUES`), restricted to the APIs
/// `com.amazonaws.kinesisvideoarchivedmedia#AWSAcuityReader` binds.
const ARCHIVED_MEDIA_API_NAMES: &[&str] = &[
    "LIST_FRAGMENTS",
    "GET_MEDIA_FOR_FRAGMENT_LIST",
    "GET_CLIP",
    "GET_IMAGES",
    "GET_HLS_STREAMING_SESSION_URL",
    "GET_DASH_STREAMING_SESSION_URL",
];

/// The sentence a request that does not name exactly one stream is refused with.
const ONE_IDENTIFIER: &str = "One of StreamName or StreamARN must be provided, but not both.";

/// The sentence a stream that cannot be read is refused with.
const STREAM_NOT_FOUND: &str = "The requested stream is not found or not active.";

/// The sentence a stream that retains nothing is refused with.
const NO_RETENTION: &str = "The stream data retention is set to 0 hours.";

/// The emulator the suite drives: the Kinesis Video control plane, the live media endpoint
/// and the archived-media endpoint, mounted on ONE engine exactly as the router mounts
/// them, with request authentication off so a call carries the credential scope that names
/// its Region and service without a computed signature.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(kvs::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(kv_media::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_kinesisvideoarchivedmedia_awsacuityreader::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// What one call answered: its status, the body as it arrived, its parsed JSON form, and
/// the content type it reported — a media read answers with a payload rather than a
/// document, so both readings are kept.
#[derive(Debug)]
struct Answer {
    status: u16,
    body: Value,
    payload_bytes: usize,
    content_type: String,
}

impl Answer {
    /// The error code the restJson1 envelope names, or `""` for a body that names none.
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

    /// The message the envelope carries, under either spelling a restJson1 answer uses.
    fn message(&self) -> String {
        self.body
            .get("Message")
            .or_else(|| self.body.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// The SigV4 credential scope every call carries, which is what names the Region and the
/// service the request routes to.
fn credential_scope() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/kinesisvideo/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// POST one restJson1 call to the CONTROL-plane address, in process.
async fn control(router: &MultiServiceRouter, path: &str, body: &Value) -> Answer {
    let request = ::http::Request::builder()
        .method("POST")
        .uri(format!("https://{CONTROL_AUTHORITY}{path}"))
        .header("host", CONTROL_AUTHORITY)
        .header("content-type", "application/json")
        .header("authorization", credential_scope())
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let content_type = header(response.headers().get("content-type"));
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Answer {
        status,
        body,
        payload_bytes: bytes.len(),
        content_type,
    }
}

/// One header value as a string, or `""` where the answer carries none.
fn header(value: Option<&::http::HeaderValue>) -> String {
    value
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

/// Create `name` with `retention_hours` of retention and answer its `StreamARN`.
async fn create_stream(router: &MultiServiceRouter, name: &str, retention_hours: i64) -> String {
    let created = control(
        router,
        "/createStream",
        &json!({
            "StreamName": name,
            "MediaType": MEDIA_TYPE,
            "DataRetentionInHours": retention_hours,
        }),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {name:?}: CreateStream expected HTTP 200, got {created:?}"
    );
    created
        .body
        .get("StreamARN")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {name:?}: no StreamARN in {created:?}"))
        .to_string()
}

/// Wait until the control plane reports `name` as `ACTIVE`, which is what a client does
/// before reading a stream it just created ("CreateStream is an asynchronous operation").
///
/// Waits for the CONDITION, not for a span: it looks again until the status holds, under a
/// deadline far longer than the transition needs.
async fn await_active(router: &MultiServiceRouter, name: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last = String::new();
    while Instant::now() < deadline {
        let described = control(router, "/describeStream", &json!({ "StreamName": name })).await;
        last = described.body["StreamInfo"]["Status"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if last == "ACTIVE" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("case {name:?}: DescribeStream expected Status ACTIVE before the deadline, got {last:?}");
}

/// The `DataEndpoint` the control plane announces for `api_name` on `stream`.
async fn data_endpoint(router: &MultiServiceRouter, stream: &str, api_name: &str) -> String {
    let answer = control(
        router,
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

/// The authority of an announced origin, asserting the shape the value has to have: the
/// scheme the plane's listener speaks, a host and port, and no path — "The returned
/// endpoint does not have the API name appended."
fn announced_authority(case: &str, endpoint: &str) -> String {
    let scheme = format!("{}://", kvs::state::DATA_PLANE_SCHEME);
    let authority = endpoint.strip_prefix(&scheme).unwrap_or_else(|| {
        panic!(
            "case {case:?}: the announced origin must name the transport its listener serves, \
             expected a {scheme} origin, got {endpoint:?}"
        )
    });
    assert!(
        !authority.is_empty()
            && !authority.contains('/')
            && !authority.contains('?')
            && !authority.contains('#'),
        "case {case:?}: expected scheme and host only with no path, got {endpoint:?}"
    );
    authority.to_string()
}

/// A STOCK HTTP client: nothing is relaxed about how it reaches an origin, so its default
/// certificate verification stays on and it holds no trust material of this emulator —
/// which is the position of every client that was handed a discovered endpoint. Only the
/// ambient proxy environment is ignored, so a loopback origin is dialled directly whatever
/// the host's own configuration says.
fn stock_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("a default-configured HTTP client")
}

/// Fetch one of the plane's routes from the announced origin, AS ANNOUNCED.
///
/// The failure a scheme the listener does not serve produces lands here, in connection
/// setup, before a request is sent — which is what makes a wrongly announced origin
/// indistinguishable from a service outage for a client, so it is asserted as its own
/// case.
async fn dial(
    client: &reqwest::Client,
    case: &str,
    endpoint: &str,
    path: &str,
    body: &Value,
) -> Answer {
    let response = client
        .post(format!("{endpoint}{path}"))
        .header("content-type", "application/json")
        .header("authorization", credential_scope())
        .body(serde_json::to_vec(body).expect("the request body serializes"))
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!(
                "case {case:?}: a client holding no trust material of this emulator must reach \
                 the announced origin {endpoint:?} as announced, got {error}"
            )
        });
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let payload = response
        .bytes()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: reading {endpoint:?} failed: {error}"));
    let body = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    Answer {
        status,
        body,
        payload_bytes: payload.len(),
        content_type,
    }
}

/// A fragment selector covering the whole archive, which `ListFragments` requires on a
/// first page ("This is only required when the NextToken isn't passed in the API").
fn whole_archive() -> Value {
    json!({
        "FragmentSelectorType": "SERVER_TIMESTAMP",
        "TimestampRange": { "StartTimestamp": 0, "EndTimestamp": 4_000_000_000_i64 },
    })
}

/// A one-minute window, inside the published 300-second maximum `GetImages` accepts.
fn image_window(stream: &str) -> Value {
    json!({
        "StreamName": stream,
        "ImageSelectorType": "SERVER_TIMESTAMP",
        "StartTimestamp": 1_761_000_000_i64,
        "EndTimestamp": 1_761_000_060_i64,
        "Format": "JPEG",
    })
}

/// A clip selector over a window of the archive.
fn clip_window(stream: &str) -> Value {
    json!({
        "StreamName": stream,
        "ClipFragmentSelector": {
            "FragmentSelectorType": "SERVER_TIMESTAMP",
            "TimestampRange": {
                "StartTimestamp": 1_761_000_000_i64,
                "EndTimestamp": 1_761_000_600_i64,
            },
        },
    })
}

/// What one row expects back from the discovered origin: either a success whose body the
/// row names, or a refusal of a named class.
#[derive(Debug)]
enum Expected {
    /// HTTP 200 and an empty `Fragments` list, which is what a stream that never ingested
    /// media lists.
    NoFragmentsListed,
    /// A refusal: the status, the error code, and the sentence it carries where the row
    /// pins one (the wording of a framework constraint refusal is pinned in the owning
    /// crate's own suite, so a row for it names the class alone).
    Refused(u16, &'static str, Option<&'static str>),
}

/// The discovered archived-media origin is dialled as announced by a stock client, and each
/// of the plane's six routes answers there with its modelled response.
#[tokio::test(flavor = "multi_thread")]
async fn the_discovered_archived_media_origin_serves_its_plane_to_a_stock_client() {
    let router = emulator();
    let retained_arn = create_stream(&router, RETAINED, 1).await;
    create_stream(&router, UNRETAINED, 0).await;
    await_active(&router, RETAINED).await;
    await_active(&router, UNRETAINED).await;

    // Every API the archived-media endpoint binds resolves to ONE origin, whichever of
    // them asked for it, and that origin is not the control-plane address the discovery
    // call arrived on.
    let endpoint = data_endpoint(&router, RETAINED, ARCHIVED_MEDIA_API_NAMES[0]).await;
    let authority = announced_authority(ARCHIVED_MEDIA_API_NAMES[0], &endpoint);
    assert_ne!(
        authority, CONTROL_AUTHORITY,
        "the archived-media origin must not be the control-plane address, got {authority:?}"
    );
    // Stated independently of the control plane's own published constant, so flipping the
    // constant and the listener together cannot pass unnoticed: a plane may be announced
    // only under a transport the client that just made the control call already reaches,
    // and the one such transport a plane of this emulator is served over is plaintext
    // HTTP/1.1 — the core's own certificate belongs to the core, so no plane presents it
    // and no client is handed trust material for one.
    assert!(
        endpoint.starts_with("http://"),
        "the announced origin must be published under the transport a client holding no trust \
         material of this emulator reaches, got {endpoint:?}"
    );
    for api_name in ARCHIVED_MEDIA_API_NAMES {
        let discovered = data_endpoint(&router, RETAINED, api_name).await;
        assert_eq!(
            discovered, endpoint,
            "case {api_name:?}: every API the archived-media endpoint binds resolves to one \
             origin, expected {endpoint:?}, got {discovered:?}"
        );
        // Another stream's archived-media origin is its OWN: the discovery call names the
        // stream it wants an endpoint for, so the value it reports belongs to that stream
        // rather than to the plane at large.
        let for_other_stream = data_endpoint(&router, UNRETAINED, api_name).await;
        assert_ne!(
            for_other_stream, endpoint,
            "case {api_name:?}: another stream must be announced its own origin, \
             got {endpoint:?} for both"
        );
        announced_authority(api_name, &discovered);
        announced_authority(api_name, &for_other_stream);
    }

    // The origin announced for the other stream's archive, which the rows about THAT
    // stream are dialled at.
    let unretained_endpoint = data_endpoint(&router, UNRETAINED, ARCHIVED_MEDIA_API_NAMES[0]).await;

    let client = stock_client();
    // (case, the route asked for on the discovered origin, the request, what it earns)
    let rows: Vec<(&str, &str, Value, Expected)> = vec![
        (
            "ListFragments on a stream that never ingested, addressed by name",
            "/listFragments",
            json!({ "StreamName": RETAINED, "FragmentSelector": whole_archive() }),
            Expected::NoFragmentsListed,
        ),
        (
            "ListFragments on the same stream, addressed by ARN",
            "/listFragments",
            json!({ "StreamARN": retained_arn.clone(), "FragmentSelector": whole_archive() }),
            Expected::NoFragmentsListed,
        ),
        (
            "ListFragments naming both identifiers",
            "/listFragments",
            json!({
                "StreamName": RETAINED,
                "StreamARN": retained_arn.clone(),
                "FragmentSelector": whole_archive(),
            }),
            Expected::Refused(400, "InvalidArgumentException", Some(ONE_IDENTIFIER)),
        ),
        (
            "ListFragments for a stream no record answers to",
            "/listFragments",
            json!({ "StreamName": ABSENT, "FragmentSelector": whole_archive() }),
            Expected::Refused(404, "ResourceNotFoundException", Some(STREAM_NOT_FOUND)),
        ),
        (
            "GetImages over a window of a stream that never ingested",
            "/getImages",
            image_window(RETAINED),
            Expected::Refused(
                404,
                "ResourceNotFoundException",
                Some("No fragments found in the stream for the GetImages request."),
            ),
        ),
        (
            "GetImages on a stream that retains nothing",
            "/getImages",
            image_window(UNRETAINED),
            Expected::Refused(400, "NoDataRetentionException", Some(NO_RETENTION)),
        ),
        (
            "GetClip over a window of a stream that never ingested",
            "/getClip",
            clip_window(RETAINED),
            Expected::Refused(
                404,
                "ResourceNotFoundException",
                Some("No fragments found in the stream for the clip request."),
            ),
        ),
        (
            "GetClip on a stream that retains nothing",
            "/getClip",
            clip_window(UNRETAINED),
            Expected::Refused(400, "NoDataRetentionException", Some(NO_RETENTION)),
        ),
        (
            "GetMediaForFragmentList with a fragment number outside the modelled element set",
            "/getMediaForFragmentList",
            json!({ "StreamName": RETAINED, "Fragments": ["not-a-fragment-number"] }),
            Expected::Refused(400, "ValidationException", None),
        ),
        (
            "GetMediaForFragmentList for a fragment number the archive does not hold",
            "/getMediaForFragmentList",
            json!({ "StreamName": RETAINED, "Fragments": ["9134385233318143200"] }),
            Expected::Refused(
                400,
                "InvalidArgumentException",
                Some("Fragment numbers are invalid : [9134385233318143200]"),
            ),
        ),
        (
            "GetHLSStreamingSessionURL in its default live mode, with no recent fragments",
            "/getHLSStreamingSessionURL",
            json!({ "StreamName": RETAINED }),
            Expected::Refused(
                404,
                "ResourceNotFoundException",
                Some("No fragments found in the stream for the streaming request."),
            ),
        ),
        (
            "GetDASHStreamingSessionURL on a stream that retains nothing",
            "/getDASHStreamingSessionURL",
            json!({ "StreamName": UNRETAINED }),
            Expected::Refused(400, "NoDataRetentionException", Some(NO_RETENTION)),
        ),
    ];

    for (case, path, body, expected) in &rows {
        // Each row is dialled at the origin announced for the stream it NAMES: an announced
        // endpoint serves the stream it was discovered for, so the row about a stream that
        // retains nothing is sent to that stream's own origin.
        let origin = if body.get("StreamName").and_then(Value::as_str) == Some(UNRETAINED) {
            &unretained_endpoint
        } else {
            &endpoint
        };
        let answer = dial(&client, case, origin, path, body).await;
        match expected {
            Expected::NoFragmentsListed => {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: expected HTTP 200 over the discovered origin, got {answer:?}"
                );
                assert_eq!(
                    answer.body.get("Fragments"),
                    Some(&json!([])),
                    "case {case:?}: a stream that never ingested media lists no fragments, got \
                     {answer:?}"
                );
            }
            Expected::Refused(status, code, message) => {
                assert_eq!(
                    answer.status, *status,
                    "case {case:?}: expected HTTP {status} over the discovered origin, got \
                     {answer:?}"
                );
                assert_eq!(
                    answer.error_code(),
                    *code,
                    "case {case:?}: expected {code}, got {answer:?}"
                );
                if let Some(message) = message {
                    assert_eq!(
                        answer.message(),
                        *message,
                        "case {case:?}: expected {message:?}, got {:?}",
                        answer.message()
                    );
                }
            }
        }
    }
}

/// The same announcement contract on the OTHER endpoint `GetDataEndpoint` publishes — the
/// live media endpoint — so the rule is about an announced endpoint rather than about one
/// plane: its origin is announced under the transport its own listener speaks, is not the
/// archived-media one, and a stock client reads that endpoint's own read back over it.
///
/// The rows differ in the start selector the read opens with, which is what a live reader
/// varies and what the plane's own route has to parse once the connection is made.
#[tokio::test(flavor = "multi_thread")]
async fn the_discovered_live_media_origin_is_dialled_the_same_way() {
    let router = emulator();
    create_stream(&router, RETAINED, 1).await;
    await_active(&router, RETAINED).await;

    let media = data_endpoint(&router, RETAINED, "GET_MEDIA").await;
    let archive = data_endpoint(&router, RETAINED, "LIST_FRAGMENTS").await;
    let media_authority = announced_authority("GET_MEDIA", &media);
    assert_ne!(
        media, archive,
        "the live media endpoint and the archived-media one are two endpoints, so they are \
         announced at two origins, got {media:?} for both"
    );
    assert_ne!(
        media_authority, CONTROL_AUTHORITY,
        "the live media origin must not be the control-plane address, got {media_authority:?}"
    );
    // The ingest API of that same endpoint resolves to the one origin, as every API one
    // endpoint binds does.
    let ingest = data_endpoint(&router, RETAINED, "PUT_MEDIA").await;
    assert_eq!(
        ingest, media,
        "every API the live media endpoint binds resolves to one origin, expected {media:?}, \
         got {ingest:?}"
    );

    let client = stock_client();
    // (case, the start selector the read opens with) — the two forms that need no further
    // member, per `StartSelector$StartSelectorType`.
    let reads: &[(&str, &str)] = &[
        ("GetMedia from the earliest fragment", "EARLIEST"),
        ("GetMedia from the latest fragment", "NOW"),
    ];
    for (case, selector_type) in reads {
        let answer = dial(
            &client,
            case,
            &media,
            "/getMedia",
            &json!({
                "StreamName": RETAINED,
                "StartSelector": { "StartSelectorType": selector_type },
            }),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200 over the discovered live media origin, got \
             {answer:?}"
        );
        // What the read reports is the content type of the media the live plane serves for a
        // stream nothing has ingested into — the live media service's own published default
        // (`state::DEFAULT_CONTENT_TYPE`), carried in the `Content-Type` header
        // `GetMedia$ContentType` binds — and a stream that never ingested carries no chunk,
        // so the payload is empty. A 200 alone would say neither.
        assert_eq!(
            answer.content_type,
            kv_media::state::DEFAULT_CONTENT_TYPE,
            "case {case:?}: the read must report the content type of the media it serves, got \
             {answer:?}"
        );
        assert_eq!(
            answer.payload_bytes, 0,
            "case {case:?}: a stream that never ingested carries no media, got {answer:?}"
        );
    }
}

/// RULE, from the other side: the announced scheme names the transport the plane's listener
/// serves and no other, so a client that assumes a different transport for the same
/// authority does not get an answer out of it — the announcement and what is served cannot
/// disagree without a client being left unable to tell a misannounced origin from a broken
/// service.
///
/// The rows differ in the route attempted, so the transport is a property of the announced
/// origin rather than of one request.
#[tokio::test(flavor = "multi_thread")]
async fn the_announced_scheme_is_the_only_transport_the_plane_serves() {
    let router = emulator();
    create_stream(&router, RETAINED, 1).await;
    await_active(&router, RETAINED).await;
    let endpoint = data_endpoint(&router, RETAINED, "LIST_FRAGMENTS").await;
    let authority = announced_authority("LIST_FRAGMENTS", &endpoint);

    // The transport the announcement does NOT name: the other of the two an AWS endpoint is
    // ever published under.
    let other_scheme = if kvs::state::DATA_PLANE_SCHEME == "https" {
        "http"
    } else {
        "https"
    };
    let client = stock_client();
    for path in ["/listFragments", "/getImages", "/getClip"] {
        let attempted = client
            .post(format!("{other_scheme}://{authority}{path}"))
            .header("content-type", "application/json")
            .header("authorization", credential_scope())
            .body(serde_json::to_vec(&json!({ "StreamName": RETAINED })).expect("serializes"))
            .send()
            .await;
        assert!(
            attempted.is_err(),
            "case {path:?}: {other_scheme:?} is not the transport the announced origin \
             {endpoint:?} names, so a client assuming it must not be served, got {:?}",
            attempted.map(|response| response.status())
        );
    }

    // And the origin as ANNOUNCED still answers, so the refusals above are about the
    // transport and not about the socket being gone.
    let answer = dial(
        &client,
        "the announced origin itself",
        &endpoint,
        "/listFragments",
        &json!({ "StreamName": RETAINED, "FragmentSelector": whole_archive() }),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "the announced origin must still serve its plane, got {answer:?}"
    );
}

/// RULE: an announced endpoint belongs to the STREAM it was discovered for — "Gets an
/// endpoint for a specified stream for either reading or writing", with the stream named in
/// the discovery call's own input — so the reads served there read that stream and no
/// other: a read naming a different stream asked an endpoint for a stream it does not
/// serve, which is as unreadable as a stream that is not there and is refused with the same
/// class and sentence, while each stream's own endpoint keeps serving it.
///
/// The rows differ in which endpoint the read was sent to (the archived-media one and the
/// live media one, so the scoping is a property of the announced address rather than of one
/// plane) and in which stream it named (the endpoint's own stream, another stream of the
/// account, and a name that exists nowhere).
#[tokio::test(flavor = "multi_thread")]
async fn an_announced_endpoint_reads_only_the_stream_it_was_discovered_for() {
    /// A second retained stream of the same account, so one stream's endpoint can be asked
    /// for the other's archive.
    const SECOND: &str = "kvam-discovered-second";

    let router = emulator();
    create_stream(&router, RETAINED, 1).await;
    create_stream(&router, SECOND, 1).await;
    await_active(&router, RETAINED).await;
    await_active(&router, SECOND).await;

    let own_archive = data_endpoint(&router, RETAINED, "LIST_FRAGMENTS").await;
    let own_media = data_endpoint(&router, RETAINED, "GET_MEDIA").await;
    let second_archive = data_endpoint(&router, SECOND, "LIST_FRAGMENTS").await;
    let second_media = data_endpoint(&router, SECOND, "GET_MEDIA").await;
    assert_ne!(
        own_archive, second_archive,
        "each stream is announced its own archived-media origin, got {own_archive:?} for both"
    );
    assert_ne!(
        own_media, second_media,
        "each stream is announced its own live media origin, got {own_media:?} for both"
    );

    let client = stock_client();
    // (case, the origin the read is sent to, the route, the stream it names, served).
    let rows: Vec<(&str, &String, &str, &str, bool)> = vec![
        (
            "the archive listing of another stream, on this stream's archive origin",
            &own_archive,
            "/listFragments",
            SECOND,
            false,
        ),
        (
            "the archive listing of a stream that exists nowhere",
            &own_archive,
            "/listFragments",
            ABSENT,
            false,
        ),
        (
            "the archive listing of this origin's own stream",
            &own_archive,
            "/listFragments",
            RETAINED,
            true,
        ),
        (
            "the live read of another stream, on this stream's media origin",
            &own_media,
            "/getMedia",
            SECOND,
            false,
        ),
        (
            "the live read of this origin's own stream",
            &own_media,
            "/getMedia",
            RETAINED,
            true,
        ),
        (
            "the other stream's archive listing on its OWN archive origin",
            &second_archive,
            "/listFragments",
            SECOND,
            true,
        ),
    ];
    for (case, origin, path, named, served) in rows {
        let body = match path {
            "/getMedia" => json!({
                "StreamName": named,
                "StartSelector": { "StartSelectorType": "EARLIEST" },
            }),
            _ => json!({ "StreamName": named, "FragmentSelector": whole_archive() }),
        };
        let answer = dial(&client, case, origin, path, &body).await;
        if served {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: an origin serves the stream it was announced for, got {answer:?}"
            );
        } else {
            assert_eq!(
                answer.status, 404,
                "case {case:?}: an origin must not read a stream it was not announced for, \
                 got {answer:?}"
            );
            assert_eq!(
                answer.error_code(),
                "ResourceNotFoundException",
                "case {case:?}: expected the missing-stream class, got {answer:?}"
            );
            assert_eq!(
                answer.message(),
                STREAM_NOT_FOUND,
                "case {case:?}: expected {STREAM_NOT_FOUND:?}, got {:?}",
                answer.message()
            );
        }
    }
}
