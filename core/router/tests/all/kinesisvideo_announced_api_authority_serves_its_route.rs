// SPDX-License-Identifier: CC0-1.0

//! Endpoint discovery hands a client an authority this emulator serves the DISCOVERED
//! API's own route on, for every API a stream's planes are discovered by.
//!
//! RULE: an address an operation announces for a caller to dial is an authority this
//! emulator answers on with the route of the API it was announced for bound there, so a
//! client built from the announced string verbatim reaches that route and reads back one
//! of that operation's modelled answers.
//!
//! Endpoint discovery is the only documented way into a Kinesis Video stream's media:
//! "Gets an endpoint for a specified stream for either reading or writing … The returned
//! endpoint does not have the API name appended. The client needs to add the API name to
//! the returned endpoint", and the value exists to be dialled — "To read data from the
//! stream or to write data to it, specify this endpoint in your application". A client
//! that follows it constructs its client on the announced origin with the configuration it
//! already has and appends the API name itself, so an announced authority nothing listens
//! on fails in connection setup: no status, no error code and no body for the caller to
//! branch on, which is indistinguishable at the client from the whole emulator being down.
//!
//! What this suite adds to the siblings beside it is the CATALOG and the socket together.
//! `kinesisvideo_discovered_data_plane.rs` pins that three of the announced authorities
//! accept a connection and dispatches the reads in process;
//! `kinesisvideo_archived_media_discovered_endpoint.rs` pins the archived-media plane's own
//! answers over a dialled origin; `kinesisvideo_announced_endpoint_outlives_the_stream.rs`
//! pins that a read dialled there is refused with the modelled missing-resource error once
//! the stream is removed. Here EVERY value of the discovery enum is a row — the catalog is
//! read off the model (`kvs::ops::types::A_P_I_NAME_VALUES`, so an API added to it cannot
//! silently go unserved) — and each row dials the authority announced for that one API, as
//! announced, at the route the model binds that API at.
//!
//! The rows differ along what the rule generalises over: which `APIName` was discovered,
//! which of the family's two media endpoints serves it, which route is then asked for on
//! the announced origin, which identifier form addresses the stream, and which answer class
//! the call earns.
//!
//! Three crates have to agree for the rule to hold — the Kinesis Video Streams control
//! plane announces the address and opens its listener, Kinesis Video Media binds the ingest
//! and the live read, and Kinesis Video Archived Media binds the six reads of stored
//! fragments — so the suite lives here.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_kinesisvideo_kinesisvideo_20170930 as kvs;
use service_com_amazonaws_kinesisvideoarchivedmedia_awsacuityreader as kv_archive;
use service_com_amazonaws_kinesisvideomedia_awsacuityinletservice as kv_media;

/// The region every call in this suite is signed for.
const REGION: &str = "us-west-2";

/// The control-plane address the discovery calls arrive on — the loopback form a client on
/// the machine this emulator runs on reaches it at.
const CONTROL_AUTHORITY: &str = "127.0.0.1:9297";

/// The stream whose planes the catalog row discovers, created with retention so the reads
/// of its archive are served rather than refused for retaining nothing.
const CATALOG_STREAM: &str = "announced-route-catalog";

/// The stream the identifier-form rows address, in both of its forms.
const EQUIVALENCE_STREAM: &str = "announced-route-equivalence";

/// The media type the streams here are created with.
const MEDIA_TYPE: &str = "video/h264";

/// A backstop for a transition the control plane's own clock owes, far longer than any span
/// it fixes: the waits poll for the CONDITION and this only bounds a hang.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// The four errors every operation of both data-plane models declares (the `common_errors`
/// of `com.amazonaws.kinesisvideomedia#AWSAcuityInletService` and of
/// `com.amazonaws.kinesisvideoarchivedmedia#AWSAcuityReader`).
const COMMON_ERRORS: [&str; 4] = [
    "ClientLimitExceededException",
    "InvalidArgumentException",
    "NotAuthorizedException",
    "ResourceNotFoundException",
];

/// The errors a stream's media reads declare on top of [`COMMON_ERRORS`], read off the two
/// models (`smithy_query(query="operation",operation="GetClip,GetImages,…")`): the live read
/// adds the two it is refused with for the endpoint and the connection count, and the
/// archived reads add the media-content ones.
const MEDIA_CONTENT_ERRORS: [&str; 7] = [
    "ConnectionLimitExceededException",
    "InvalidEndpointException",
    "InvalidCodecPrivateDataException",
    "InvalidMediaFrameException",
    "MissingCodecPrivateDataException",
    "NoDataRetentionException",
    "UnsupportedStreamMediaTypeException",
];

/// The stream a row addresses, in the two identifier forms the data-plane models accept —
/// "You must specify either this parameter or a StreamName in the request".
#[derive(Debug)]
struct Stream {
    /// The name the control plane created it under.
    name: String,
    /// The ARN the control plane minted for it, its one identity.
    arn: String,
}

/// One row of the catalog: an `APIName` a client discovers an endpoint for, the route the
/// API it names is bound at, and a complete call of that route.
struct Discovered {
    /// The `APIName` the discovery call asks for.
    api_name: &'static str,
    /// The route the API is served at, from its `http` binding in the model that declares
    /// it (`POST /listFragments`, `POST /getMedia`, …). The recorded ingest the write plane
    /// serves carries no modelled operation, and its path is the segment the producer plane
    /// publishes.
    path: &'static str,
    /// Headers this route needs beyond the two every row sends: the ingest addresses its
    /// stream and declares its timecode reading in headers rather than in a document.
    headers: &'static [(&'static str, &'static str)],
    /// The request, built from the stream's identifiers so the rows differ in which form
    /// addresses the stream. `None` is a route whose body is not a document.
    request: fn(&Stream) -> Option<Value>,
    /// Whether the route takes the stream in its document, where either identifier form
    /// names it — which is what lets the two forms be compared against each other. The
    /// ingest addresses its stream in headers instead, and is exercised by both forms in
    /// the crate that serves it.
    either_form: bool,
}

/// Every value of the discovery enum, each paired with the route of the API it names.
///
/// The order follows the enum (`kvs::ops::types::A_P_I_NAME_VALUES`); the test asserts the
/// two sets are the same, so a value the model publishes cannot be left without a row.
const DISCOVERED: &[Discovered] = &[
    Discovered {
        api_name: "GET_CLIP",
        path: "/getClip",
        headers: &[],
        request: clip_of_the_archive,
        either_form: true,
    },
    Discovered {
        api_name: "GET_DASH_STREAMING_SESSION_URL",
        path: "/getDASHStreamingSessionURL",
        headers: &[],
        request: dash_session,
        either_form: true,
    },
    Discovered {
        api_name: "GET_HLS_STREAMING_SESSION_URL",
        path: "/getHLSStreamingSessionURL",
        headers: &[],
        request: hls_session,
        either_form: true,
    },
    Discovered {
        api_name: "GET_IMAGES",
        path: "/getImages",
        headers: &[],
        request: images_of_the_archive,
        either_form: true,
    },
    Discovered {
        api_name: "GET_MEDIA",
        path: "/getMedia",
        headers: &[],
        request: live_read,
        either_form: true,
    },
    Discovered {
        api_name: "GET_MEDIA_FOR_FRAGMENT_LIST",
        path: "/getMediaForFragmentList",
        headers: &[],
        request: media_of_one_fragment,
        either_form: true,
    },
    Discovered {
        api_name: "LIST_FRAGMENTS",
        path: "/listFragments",
        headers: &[],
        request: fragment_listing,
        either_form: true,
    },
    Discovered {
        api_name: "PUT_MEDIA",
        path: "/putMedia",
        headers: INGEST_HEADERS,
        request: ingest_session,
        either_form: false,
    },
];

/// The headers a producer opens an ingest session with: the stream it writes to, how its
/// container's timecodes are to be read, and the payload marker its signature covers an
/// unsigned-length stream through.
const INGEST_HEADERS: &[(&str, &str)] = &[
    ("x-amzn-stream-name", CATALOG_STREAM),
    ("x-amzn-fragment-timecode-type", "ABSOLUTE"),
    ("x-amz-content-sha256", "UNSIGNED-PAYLOAD"),
];

/// A clip over a window of the archive, the member `GetClip` requires.
fn clip_of_the_archive(stream: &Stream) -> Option<Value> {
    Some(json!({
        "StreamARN": stream.arn,
        "ClipFragmentSelector": {
            "FragmentSelectorType": "SERVER_TIMESTAMP",
            "TimestampRange": {
                "StartTimestamp": 1_761_000_000_i64,
                "EndTimestamp": 1_761_000_600_i64,
            },
        },
    }))
}

/// A DASH session of the stream, whose every input member is optional.
fn dash_session(stream: &Stream) -> Option<Value> {
    Some(json!({ "StreamName": stream.name }))
}

/// An HLS session of the stream, whose every input member is optional.
fn hls_session(stream: &Stream) -> Option<Value> {
    Some(json!({ "StreamARN": stream.arn }))
}

/// Images over a one-minute window, inside the published maximum `GetImages` accepts, with
/// the four members the model marks required.
fn images_of_the_archive(stream: &Stream) -> Option<Value> {
    Some(json!({
        "StreamName": stream.name,
        "ImageSelectorType": "SERVER_TIMESTAMP",
        "StartTimestamp": 1_761_000_000_i64,
        "EndTimestamp": 1_761_000_060_i64,
        "Format": "JPEG",
    }))
}

/// A live read from the start of what the stream holds, with the selector `GetMedia`
/// requires.
fn live_read(stream: &Stream) -> Option<Value> {
    Some(json!({
        "StreamName": stream.name,
        "StartSelector": { "StartSelectorType": "EARLIEST" },
    }))
}

/// The media of one fragment number, the list `GetMediaForFragmentList` requires.
fn media_of_one_fragment(stream: &Stream) -> Option<Value> {
    Some(json!({ "StreamARN": stream.arn, "Fragments": ["1"] }))
}

/// A listing of the whole archive: `FragmentSelector` is required on a first page ("This is
/// only required when the NextToken isn't passed in the API").
fn fragment_listing(stream: &Stream) -> Option<Value> {
    Some(json!({
        "StreamName": stream.name,
        "FragmentSelector": {
            "FragmentSelectorType": "SERVER_TIMESTAMP",
            "TimestampRange": { "StartTimestamp": 0, "EndTimestamp": 4_000_000_000_i64 },
        },
    }))
}

/// An ingest session carrying no container: the producer plane is handed a stream of bytes
/// rather than a document, and what the row is about is the route being bound at the
/// announced authority rather than what a container ingests.
fn ingest_session(_stream: &Stream) -> Option<Value> {
    None
}

/// The same request with its stream addressed by ARN instead of by name, for the rows that
/// compare the two forms. A request that named neither would be refused for that, so the
/// swap replaces whichever form the row carries.
fn addressed_by_arn(request: &Value, stream: &Stream) -> Value {
    let mut members = request.as_object().cloned().unwrap_or_default();
    members.remove("StreamName");
    members.insert("StreamARN".to_string(), json!(stream.arn));
    Value::Object(members)
}

/// The same request addressed by name.
fn addressed_by_name(request: &Value, stream: &Stream) -> Value {
    let mut members = request.as_object().cloned().unwrap_or_default();
    members.remove("StreamARN");
    members.insert("StreamName".to_string(), json!(stream.name));
    Value::Object(members)
}

/// The emulator the suite drives: the Kinesis Video control plane, the live media endpoint
/// and the archived-media endpoint on ONE engine, exactly as the router mounts them, with
/// request authentication off so a call carries the credential scope that names its Region
/// and service without a computed signature.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(kvs::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(kv_media::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(kv_archive::EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// What one call answered: its status, the error class it named, its body as JSON where it
/// is a document, and how many bytes it carried (a media read answers with a payload).
#[derive(Debug)]
struct Answer {
    status: u16,
    error_type_field: String,
    body: Value,
    payload_bytes: usize,
}

impl Answer {
    /// The error class the answer names, under either of the two places a restJson1 answer
    /// of this family states one — the envelope's `__type` and the error-type field AWS
    /// documents beside the status — or `""` for an answer that names none.
    fn error_code(&self) -> String {
        let in_body = self
            .body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let named = if in_body.is_empty() {
            self.error_type_field.as_str()
        } else {
            in_body
        };
        named
            .rsplit('#')
            .next()
            .unwrap_or_default()
            .split(':')
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// Whether this is an answer the operation models: its success, or a refusal of one of
    /// the classes the model declares for it. The router's own refusal for a path no
    /// service claims, a scaffold's not-implemented answer and the front door's transport
    /// refusals are all outside that set, which is what makes this discriminating.
    fn is_modelled(&self) -> bool {
        if self.status == 200 {
            return true;
        }
        let code = self.error_code();
        COMMON_ERRORS.contains(&code.as_str()) || MEDIA_CONTENT_ERRORS.contains(&code.as_str())
    }
}

/// The SigV4 credential scope every call carries, which is what names the Region and the
/// service a request routes to.
fn credential_scope() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261118/{REGION}/kinesisvideo/aws4_request, \
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
    let error_type_field = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let bytes = response.into_body();
    Answer {
        status,
        error_type_field,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        payload_bytes: bytes.len(),
    }
}

/// A STOCK HTTP client: nothing is relaxed about how it reaches an origin, which is the
/// position of every client that was handed a discovered endpoint. Only the ambient proxy
/// environment is ignored, so a loopback origin is dialled directly whatever the host's own
/// configuration says.
fn stock_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("a default-configured HTTP client")
}

/// Dial the announced origin AS ANNOUNCED and ask it for `path`.
///
/// The failure an authority nothing serves produces lands here, in connection setup, before
/// a request is sent — there is no status, no error code and no body for a caller to branch
/// on, which is the defect this suite is about, so it fails the case by name.
async fn dial(
    client: &reqwest::Client,
    case: &str,
    endpoint: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&Value>,
) -> Answer {
    let mut request = client
        .post(format!("{endpoint}{path}"))
        .header("content-type", "application/json")
        .header("authorization", credential_scope());
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let payload = match body {
        Some(body) => serde_json::to_vec(body).expect("the request body serializes"),
        None => Vec::new(),
    };
    let response = request
        .body(payload)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!(
                "case {case:?}: a client built from the announced endpoint {endpoint:?} must \
                 reach {path:?} there, got {error}"
            )
        });
    let status = response.status().as_u16();
    let error_type_field = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let bytes = response
        .bytes()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: reading {endpoint:?} failed: {error}"));
    Answer {
        status,
        error_type_field,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        payload_bytes: bytes.len(),
    }
}

/// Create `name` with `retention_hours` of retention and answer the stream in both of its
/// identifier forms.
async fn create_stream(router: &MultiServiceRouter, name: &str, retention_hours: i64) -> Stream {
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
    let arn = created
        .body
        .get("StreamARN")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {name:?}: no StreamARN in {created:?}"))
        .to_string();
    Stream {
        name: name.to_string(),
        arn,
    }
}

/// Wait until the control plane reports `name` as `ACTIVE`, which is what a client does
/// before reading a stream it just created ("CreateStream is an asynchronous operation").
///
/// Waits for the CONDITION, not for a span: it looks again until the status holds, under a
/// deadline far longer than the transition needs.
async fn await_active(router: &MultiServiceRouter, name: &str) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let described = control(router, "/describeStream", &json!({ "StreamName": name })).await;
        if described.body["StreamInfo"]["Status"].as_str() == Some("ACTIVE") {
            return;
        }
        last = described.body;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("case {name:?}: DescribeStream never reported ACTIVE, last read {last:?}");
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

/// The authority of an announced origin, asserting the shape the value has to have for a
/// client to append the API name to it: a scheme, a host, and no path — "The returned
/// endpoint does not have the API name appended."
fn announced_authority(case: &str, endpoint: &str) -> String {
    let (_, authority) = endpoint.split_once("://").unwrap_or_else(|| {
        panic!("case {case:?}: expected an absolute origin to dial, got {endpoint:?}")
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

/// RULE: every API endpoint discovery names is served at the authority announced for it, so
/// a client built from the announced string reaches that API's route there and reads back
/// one of the operation's modelled answers.
#[tokio::test(flavor = "multi_thread")]
async fn every_discovered_api_is_served_at_the_authority_announced_for_it() {
    // The catalog is the model's: a value the enum publishes with no row here would leave
    // the rule untested for it, and a row for a value the model does not publish would test
    // an API no client can discover.
    let catalog: Vec<&str> = DISCOVERED.iter().map(|row| row.api_name).collect();
    let published: Vec<&str> = kvs::ops::types::A_P_I_NAME_VALUES.to_vec();
    assert_eq!(
        catalog, published,
        "case \"the discovery catalog\": every published APIName must have a row and nothing \
         else may, expected {published:?}, got {catalog:?}"
    );

    let (router, _engine) = emulator();
    let stream = create_stream(&router, CATALOG_STREAM, 1).await;
    await_active(&router, CATALOG_STREAM).await;

    let client = stock_client();
    for row in DISCOVERED {
        let case = format!("{} dialled at {}", row.api_name, row.path);
        let endpoint = data_endpoint(&router, &stream.name, row.api_name).await;
        let authority = announced_authority(&case, &endpoint);
        // The whole point of the discovery call: the media APIs are not served at the
        // address it was made to, so the value it reports is another authority of this
        // emulator.
        assert_ne!(
            authority, CONTROL_AUTHORITY,
            "case {case:?}: the announced authority must not be the control-plane one, got \
             {endpoint:?}"
        );

        let request = (row.request)(&stream);
        let answer = dial(
            &client,
            &case,
            &endpoint,
            row.path,
            row.headers,
            request.as_ref(),
        )
        .await;
        assert!(
            answer.is_modelled(),
            "case {case:?}: the announced authority must serve this API's own route, so the \
             answer is the operation's success or one of its modelled refusals, got {answer:?}"
        );
    }
}

/// RULE: the announced authority serves the stream it was discovered for by either of the
/// identifier forms the model accepts, so which form a read names it by does not change the
/// answer the dialled origin gives.
#[tokio::test(flavor = "multi_thread")]
async fn the_two_identifier_forms_agree_at_the_announced_authority() {
    let (router, _engine) = emulator();
    let stream = create_stream(&router, EQUIVALENCE_STREAM, 1).await;
    await_active(&router, EQUIVALENCE_STREAM).await;

    let client = stock_client();
    for row in DISCOVERED.iter().filter(|row| row.either_form) {
        let case = format!("{} dialled at {}", row.api_name, row.path);
        let endpoint = data_endpoint(&router, &stream.name, row.api_name).await;
        let request = (row.request)(&stream).unwrap_or_else(|| {
            panic!("case {case:?}: a row comparing identifier forms carries a document")
        });

        let by_name = dial(
            &client,
            &format!("{case} by name"),
            &endpoint,
            row.path,
            row.headers,
            Some(&addressed_by_name(&request, &stream)),
        )
        .await;
        let by_arn = dial(
            &client,
            &format!("{case} by ARN"),
            &endpoint,
            row.path,
            row.headers,
            Some(&addressed_by_arn(&request, &stream)),
        )
        .await;

        assert!(
            by_name.is_modelled() && by_arn.is_modelled(),
            "case {case:?}: both forms must be answered by the API's own route, got \
             {by_name:?} by name and {by_arn:?} by ARN"
        );
        assert_eq!(
            by_name.status, by_arn.status,
            "case {case:?}: the two identifier forms name one stream, so the status must \
             agree, got {by_name:?} by name and {by_arn:?} by ARN"
        );
        assert_eq!(
            by_name.error_code(),
            by_arn.error_code(),
            "case {case:?}: the two identifier forms name one stream, so the answer class \
             must agree, got {by_name:?} by name and {by_arn:?} by ARN"
        );

        // The listing of a stream that never ingested is the one answer of this set whose
        // whole document is fixed, so the two forms are compared member for member there:
        // an archive with nothing in it, read by either name.
        if row.path == "/listFragments" {
            for (form, answer) in [("name", &by_name), ("ARN", &by_arn)] {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?} by {form}: a retained stream's archive is listed, got \
                     {answer:?}"
                );
                assert_eq!(
                    answer.body.get("Fragments").and_then(Value::as_array),
                    Some(&Vec::new()),
                    "case {case:?} by {form}: a stream that never ingested lists nothing, got \
                     {answer:?}"
                );
            }
            assert_eq!(
                by_name.body, by_arn.body,
                "case {case:?}: the two identifier forms must list one archive, got \
                 {by_name:?} by name and {by_arn:?} by ARN"
            );
        }

        // A payload read answers the bytes it answers; what the comparison above pins is
        // that neither form was refused where the other was served.
        assert_eq!(
            by_name.payload_bytes > 0,
            by_arn.payload_bytes > 0,
            "case {case:?}: the two identifier forms must carry the same kind of answer, got \
             {by_name:?} by name and {by_arn:?} by ARN"
        );
    }
}
