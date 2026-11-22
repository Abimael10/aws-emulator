// SPDX-License-Identifier: CC0-1.0

//! The LENGTH of this data plane's failure messages.
//!
//! Every member this service fills a failure's text into targets the `Message` shape,
//! which is modelled `len 0..2048`: the `Message` of each modelled error structure
//! (`ValidationError`, `InternalFailure`, `ResourceNotFound`, `ServiceUnavailable`,
//! `AccessForbidden`, `ConflictException`) and the per-element `ErrorMessage` of
//! `BatchGetRecordError` and `BatchWriteRecordError`. A refusal is BUILT from the request
//! — it quotes the value that broke a rule, and it names the feature group by the ARN its
//! name resolves to, whose Region comes from the caller's own credential scope — so a
//! request can push that text past the member that carries it.
//!
//! The rule asserted here: whatever the operation and whichever of its messages it
//! chooses, the text a client reads is cut to the modelled maximum, at a character
//! boundary, and a message already inside the bound is answered verbatim. It holds for
//! the message an operation takes from the shared record preparation and for one its own
//! handler words (`DeleteRecord`'s `EventTime`, `UpdateRecord`'s record identifier)
//! alike, and the bound's own edges are driven by padding the value a refusal quotes.
//!
//! Every case goes through the real front door — the multi-service router, the restJson1
//! protocol, the modelled constraint validation — so what is asserted is the wire
//! behaviour a client sees.

use std::sync::Arc;

use serde_json::{json, Value};
use service_common::feature_store_groups::{FeatureGroupDirectory, FeatureGroupRecord};
use smithy_server_runtime::{http, MultiServiceRouter};

use service_com_amazonaws_sagemakerfeaturestoreruntime_amazonsagemakerfeaturestoreruntime::EmulatedService;

/// The modelled maximum length of a message this service answers a failure with.
///
/// Source: `smithy_query(query="shape", shape="Message")` on this crate's
/// `smithy-model/` — `length {min: 0, max: 2048}`, the target of every error message
/// member of this service.
const MAX_MESSAGE_BYTES: usize = 2048;

/// The feature group the rows work in, and the two feature names its definition fixes.
const GROUP: &str = "customers";
const IDENTIFIER_FEATURE: &str = "record_id";
const EVENT_TIME_FEATURE: &str = "event_time";

/// The account id every ARN this emulator mints carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// The Region a row is signed for unless it names another.
const REGION: &str = "us-east-1";

/// A booted emulator holding this service only.
struct Emulator {
    engine: Arc<engine::InMemoryEngine>,
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { engine, router }
    }

    /// Create a feature group the way the SageMaker control plane's `CreateFeatureGroup`
    /// does: by registering its definition in the ONE shared directory this data plane
    /// resolves names against.
    fn create_feature_group(&self, name: &str) {
        self.engine
            .state::<FeatureGroupDirectory>()
            .lock()
            .register(FeatureGroupRecord {
                name: name.to_owned(),
                arn: format!("arn:aws:sagemaker:{REGION}:{ACCOUNT}:feature-group/{name}"),
                record_identifier_feature_name: IDENTIFIER_FEATURE.to_owned(),
                event_time_feature_name: EVENT_TIME_FEATURE.to_owned(),
                online_store_enabled: true,
                offline_store: None,
                default_ttl_seconds: None,
            });
    }

    /// Send one request, signed for `region` — the 3rd field of the SigV4 credential
    /// scope, which is the Region a resolved ARN carries.
    async fn send_in(
        &self,
        region: &str,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20240101/{region}/sagemaker/\
                     aws4_request, SignedHeaders=host, Signature=0000"
                ),
            )
            .header("host", "featurestore-runtime.sagemaker.amazonaws.com")
            .header("content-type", "application/json")
            .body(
                body.map(|value| bytes::Bytes::from(value.to_string()))
                    .unwrap_or_default(),
            )
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = serde_json::from_slice::<Value>(response.body()).unwrap_or(Value::Null);
        (status, parsed)
    }
}

/// One `FeatureValue` with a scalar value.
fn feature(name: &str, value: &str) -> Value {
    json!({ "FeatureName": name, "ValueAsString": value })
}

/// The wire error code of an error response: restJson1 carries it as the body's `__type`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// The message an error response carries. This service's modelled errors declare a
/// `Message` member, which restJson1 serialises under that name; the framework's own
/// refusals use `message`, so both spellings are read.
fn message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// The first per-element `ErrorMessage` a batch response reports, and the code beside it.
fn first_batch_error(body: &Value) -> (String, String) {
    body.get("Errors")
        .and_then(Value::as_array)
        .and_then(|errors| errors.first())
        .map(|error| {
            (
                error["ErrorCode"].as_str().unwrap_or_default().to_owned(),
                error["ErrorMessage"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .unwrap_or_default()
}

/// How this service states a `FeatureGroupName` that resolved to nothing, unbounded: the
/// group named by the ARN it would carry in the caller's account and the signed Region.
fn no_such_feature_group(region: &str, name: &str) -> String {
    format!(
        "Resource Not Found: Amazon SageMaker can't find a FeatureGroup with name \
         [arn:aws:sagemaker:{region}:{ACCOUNT}:feature-group/{name}]."
    )
}

/// How this service states an event-time feature value that is no timestamp, unbounded.
fn unreadable_event_time(value: &str) -> String {
    format!(
        "EventTime: the event time feature '{EVENT_TIME_FEATURE}' must carry an ISO-8601 \
         timestamp or seconds since the epoch, not '{value}'"
    )
}

/// How this service states a record identifier that names no live record, unbounded.
fn no_such_record(identifier: &str) -> String {
    format!("Record Not Found: Record not found with record identifier [{identifier}].")
}

/// How `DeleteRecord` states an `EventTime` that is no timestamp, unbounded.
///
/// That operation carries the instant in its OWN `EventTime` member — a `@httpQuery`
/// `ValueAsString`, modelled `len 0..358400` — rather than among a record's feature
/// values, so it words this refusal itself.
fn unreadable_delete_event_time(value: &str) -> String {
    format!("EventTime: expected an ISO-8601 timestamp or seconds since the epoch, not '{value}'")
}

/// How `UpdateRecord` states a posted record-identifier feature whose value is not the
/// one of the record being updated, unbounded: the value it quotes is the request's own
/// `RecordIdentifierValueAsString`, a `ValueAsString`.
fn identifier_feature_mismatch(identifier: &str) -> String {
    format!(
        "Features: the record identifier feature '{IDENTIFIER_FEATURE}' must carry the \
         RecordIdentifierValueAsString of the record being updated ('{identifier}')"
    )
}

/// `value`, percent-encoded for the query string of a URI.
///
/// A `ValueAsString` admits any character (`pattern .*`), and a `@httpQuery` member
/// travels in the query string, where a byte outside the unreserved set is escaped.
fn query_encoded(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// `answered` must be `expected` cut to the modelled bound: a prefix of it, no longer
/// than the bound, and as long as the bound admits — so nothing is dropped that fits.
fn assert_cut_to_bound(case: &str, answered: &str, expected: &str) {
    assert!(
        expected.len() > MAX_MESSAGE_BYTES,
        "case {case:?}: the row must state a message longer than the {MAX_MESSAGE_BYTES}-byte \
         bound to be cut at all, but it is {} bytes",
        expected.len()
    );
    assert!(
        answered.len() <= MAX_MESSAGE_BYTES,
        "case {case:?}: expected a message of at most {MAX_MESSAGE_BYTES} bytes, got {} bytes \
         ({:?}…)",
        answered.len(),
        answered.chars().take(80).collect::<String>()
    );
    assert!(
        expected.starts_with(answered),
        "case {case:?}: expected the answered message to be a prefix of {:?}…, got {:?}…",
        expected.chars().take(120).collect::<String>(),
        answered.chars().take(120).collect::<String>()
    );
    // A cut lands on the last character boundary at or below the bound, so at most the
    // bytes of one character are given up: a cut that dropped more is dropping text that
    // fits.
    let shortfall = MAX_MESSAGE_BYTES - answered.len();
    assert!(
        shortfall < 4,
        "case {case:?}: expected the message cut at the last character boundary within \
         {MAX_MESSAGE_BYTES} bytes, got {} bytes, {shortfall} short of the bound",
        answered.len()
    );
}

/// One wording of the refusal an unreadable event time is answered with: the operation
/// that answers it, how it states the value it quotes, and whether that value travels in
/// the query string rather than in the request body.
struct Wording {
    operation: &'static str,
    word: fn(&str) -> String,
    in_query: bool,
}

/// One case: the call to make, and the failure its answer must state.
struct Row {
    case: &'static str,
    /// The Region the request is signed for, which is the Region a resolved ARN carries.
    region: String,
    method: &'static str,
    uri: String,
    body: Option<Value>,
    status: u16,
    code: &'static str,
    /// The UNBOUNDED text of the failure this call states.
    expected: String,
    /// Whether the message rides in a batch element rather than in a modelled error.
    per_element: bool,
}

#[tokio::test]
async fn every_operation_cuts_an_unresolvable_group_refusal_to_the_modelled_message_maximum() {
    // The Region a resolved ARN carries comes from the request's own credential scope,
    // which is bounded by nothing, so it is what pushes the ONE refusal every operation
    // of this data plane shares past the member that carries it. The rows span the Region
    // lengths either side of the bound and the group-name spellings the modelled
    // `FeatureGroupNameOrArn` admits.
    //
    // (case, the Region signed for, the group name, the spelling the call carries)
    let rows: Vec<(&str, String, &str, String)> = vec![
        (
            "a Region far past the bound",
            "a".repeat(4_000),
            "ghost",
            "ghost".to_owned(),
        ),
        (
            "a Region just past the bound",
            "r".repeat(MAX_MESSAGE_BYTES - 50),
            "ghost",
            "ghost".to_owned(),
        ),
        (
            "a Region past the bound, with the ARN spelling of the name",
            "z".repeat(2_500),
            "ghost",
            "arn:aws:sagemaker:eu-west-1:123456789012:feature-group/ghost".to_owned(),
        ),
        (
            "a Region past the bound, with a name of the maximum length a group may carry",
            "q".repeat(3_000),
            "a0123456789012345678901234567890123456789012345678901234567890b",
            "a0123456789012345678901234567890123456789012345678901234567890b".to_owned(),
        ),
    ];

    for (case, region, name, spelling) in rows {
        let emulator = Emulator::new();
        // One OTHER group exists in every row, so no verdict comes from an empty
        // directory.
        emulator.create_feature_group(GROUP);
        let expected = no_such_feature_group(&region, name);
        let label = spelling.replace(':', "%3A").replace('/', "%2F");

        // The single-record calls, which carry the refusal in the modelled
        // `ValidationError`: (the operation, its method, its URI, its body).
        let calls: Vec<(&str, &str, String, Option<Value>)> = vec![
            (
                "PutRecord",
                "PUT",
                format!("/FeatureGroup/{label}"),
                Some(json!({ "Record": [
                    feature(IDENTIFIER_FEATURE, "R1"),
                    feature(EVENT_TIME_FEATURE, "2026-10-23T18:20:13Z")
                ] })),
            ),
            (
                "GetRecord",
                "GET",
                format!("/FeatureGroup/{label}?RecordIdentifierValueAsString=R1"),
                None,
            ),
            (
                "UpdateRecord",
                "POST",
                format!("/FeatureGroup/{label}/Record"),
                Some(json!({
                    "RecordIdentifierValueAsString": "R1",
                    "Features": [feature("feature_a", "updated")]
                })),
            ),
            (
                "ListRecords",
                "POST",
                format!("/FeatureGroup/{label}/ListRecords"),
                Some(json!({})),
            ),
            (
                "DeleteRecord",
                "DELETE",
                format!(
                    "/FeatureGroup/{label}?RecordIdentifierValueAsString=R1\
                     &EventTime=2026-10-23T18:20:13Z"
                ),
                None,
            ),
        ];
        for (operation, method, uri, body) in calls {
            let (status, answered) = emulator.send_in(&region, method, &uri, body).await;
            let code = error_code(&answered);
            let stated = message(&answered);
            assert_eq!(
                (status, code.as_str()),
                (400, "ValidationError"),
                "case {case:?}/{operation}: expected HTTP 400 ValidationError, got {status} \
                 {code:?} with {:?}…",
                stated.chars().take(120).collect::<String>()
            );
            assert_cut_to_bound(&format!("{case}/{operation}"), &stated, &expected);
        }

        // The batch operations carry the same refusal per element, in an `ErrorMessage`
        // that targets the same `Message`.
        let batches: Vec<(&str, &str, Value)> = vec![
            (
                "BatchGetRecord",
                "/BatchGetRecord",
                json!({ "Identifiers": [{
                    "FeatureGroupName": spelling,
                    "RecordIdentifiersValueAsString": ["R1"]
                }] }),
            ),
            (
                "BatchWriteRecord",
                "/BatchWriteRecord",
                json!({ "Entries": [{
                    "FeatureGroupName": spelling,
                    "Record": [
                        feature(IDENTIFIER_FEATURE, "R1"),
                        feature(EVENT_TIME_FEATURE, "2026-10-23T18:20:13Z")
                    ]
                }] }),
            ),
        ];
        for (operation, uri, body) in batches {
            let (status, answered) = emulator.send_in(&region, "POST", uri, Some(body)).await;
            let (code, stated) = first_batch_error(&answered);
            assert_eq!(
                (status, code.as_str()),
                (200, "ValidationError"),
                "case {case:?}/{operation}: expected HTTP 200 reporting one ValidationError \
                 element, got {status} {code:?} with {:?}…",
                stated.chars().take(120).collect::<String>()
            );
            assert_cut_to_bound(&format!("{case}/{operation}"), &stated, &expected);
        }
    }
}

#[tokio::test]
async fn a_refusal_quoting_a_request_value_is_cut_at_a_character_boundary() {
    // A `ValueAsString` is modelled `len 0..358400`, so the value a refusal quotes is
    // itself far longer than the message bound. The rows span the error shapes that quote
    // one (`ValidationError$Message`, `ResourceNotFound$Message`, a batch element's
    // `ErrorMessage`), ASCII and multi-byte values, and both the value a request supplies
    // and the identifier it looks up.
    //
    // (case, the quoted value, the operation's method, URI, body, status, code, expected)
    // One ASCII value and one whose characters are each more than one byte, so the cut
    // lands on a boundary the byte bound does not fall on.
    let long_ascii = "9".repeat(4_000);
    let long_multibyte = "é".repeat(2_000);
    let rows = vec![
        Row {
            case: "PutRecord quoting an unreadable ASCII event time",
            region: REGION.to_owned(),
            method: "PUT",
            uri: format!("/FeatureGroup/{GROUP}"),
            body: Some(json!({ "Record": [
                feature(IDENTIFIER_FEATURE, "R1"),
                feature(EVENT_TIME_FEATURE, &long_ascii)
            ] })),
            status: 400,
            code: "ValidationError",
            expected: unreadable_event_time(&long_ascii),
            per_element: false,
        },
        Row {
            case: "PutRecord quoting an unreadable multi-byte event time",
            region: REGION.to_owned(),
            method: "PUT",
            uri: format!("/FeatureGroup/{GROUP}"),
            body: Some(json!({ "Record": [
                feature(IDENTIFIER_FEATURE, "R1"),
                feature(EVENT_TIME_FEATURE, &long_multibyte)
            ] })),
            status: 400,
            code: "ValidationError",
            expected: unreadable_event_time(&long_multibyte),
            per_element: false,
        },
        Row {
            case: "UpdateRecord quoting an unreadable multi-byte event time",
            region: REGION.to_owned(),
            method: "POST",
            uri: format!("/FeatureGroup/{GROUP}/Record"),
            body: Some(json!({
                "RecordIdentifierValueAsString": "R1",
                "Features": [feature(EVENT_TIME_FEATURE, &long_multibyte)]
            })),
            status: 400,
            code: "ValidationError",
            expected: unreadable_event_time(&long_multibyte),
            per_element: false,
        },
        Row {
            case: "UpdateRecord naming a record identifier no record carries",
            region: REGION.to_owned(),
            method: "POST",
            uri: format!("/FeatureGroup/{GROUP}/Record"),
            body: Some(json!({
                "RecordIdentifierValueAsString": &long_ascii,
                "Features": [feature("feature_a", "updated")]
            })),
            status: 404,
            code: "ResourceNotFound",
            expected: no_such_record(&long_ascii),
            per_element: false,
        },
        Row {
            case: "BatchWriteRecord quoting an unreadable ASCII event time per entry",
            region: REGION.to_owned(),
            method: "POST",
            uri: "/BatchWriteRecord".to_owned(),
            body: Some(json!({ "Entries": [{
                "FeatureGroupName": GROUP,
                "Record": [
                    feature(IDENTIFIER_FEATURE, "R1"),
                    feature(EVENT_TIME_FEATURE, &long_ascii)
                ]
            }] })),
            status: 200,
            code: "ValidationError",
            expected: unreadable_event_time(&long_ascii),
            per_element: true,
        },
    ];

    for row in rows {
        let emulator = Emulator::new();
        emulator.create_feature_group(GROUP);
        let (status, answered) = emulator
            .send_in(&row.region, row.method, &row.uri, row.body)
            .await;
        let (code, stated) = if row.per_element {
            first_batch_error(&answered)
        } else {
            (error_code(&answered), message(&answered))
        };
        assert_eq!(
            (status, code.as_str()),
            (row.status, row.code),
            "case {:?}: expected HTTP {} {}, got {status} {code:?} with {:?}…",
            row.case,
            row.status,
            row.code,
            stated.chars().take(120).collect::<String>()
        );
        assert_cut_to_bound(row.case, &stated, &row.expected);
    }
}

#[tokio::test]
async fn a_refusal_inside_the_bound_is_answered_verbatim() {
    // The bound cuts nothing a request did not push past it: the same classes of refusal,
    // stated with values a client supplies in practice, are answered whole.
    //
    // (the rows are the same classes of refusal, stated with values a client supplies in
    // practice)
    let identifier = "R1";
    let rows = vec![
        Row {
            case: "GetRecord against an unresolvable group",
            region: "us-east-1".to_owned(),
            method: "GET",
            uri: "/FeatureGroup/ghost?RecordIdentifierValueAsString=R1".to_owned(),
            body: None,
            status: 400,
            code: "ValidationError",
            expected: no_such_feature_group("us-east-1", "ghost"),
            per_element: false,
        },
        Row {
            case: "ListRecords against an unresolvable group, signed for another Region",
            region: "ap-southeast-2".to_owned(),
            method: "POST",
            uri: "/FeatureGroup/ghost/ListRecords".to_owned(),
            body: Some(json!({})),
            status: 400,
            code: "ValidationError",
            expected: no_such_feature_group("ap-southeast-2", "ghost"),
            per_element: false,
        },
        Row {
            case: "UpdateRecord naming a record the group does not hold",
            region: REGION.to_owned(),
            method: "POST",
            uri: format!("/FeatureGroup/{GROUP}/Record"),
            body: Some(json!({
                "RecordIdentifierValueAsString": identifier,
                "Features": [feature("feature_a", "updated")]
            })),
            status: 404,
            code: "ResourceNotFound",
            expected: no_such_record(identifier),
            per_element: false,
        },
        Row {
            case: "BatchWriteRecord against an unresolvable group",
            region: "eu-west-1".to_owned(),
            method: "POST",
            uri: "/BatchWriteRecord".to_owned(),
            body: Some(json!({ "Entries": [{
                "FeatureGroupName": "ghost",
                "Record": [
                    feature(IDENTIFIER_FEATURE, "R1"),
                    feature(EVENT_TIME_FEATURE, "2026-10-23T18:20:13Z")
                ]
            }] })),
            status: 200,
            code: "ValidationError",
            expected: no_such_feature_group("eu-west-1", "ghost"),
            per_element: true,
        },
        Row {
            case: "BatchGetRecord against an unresolvable group",
            region: "eu-west-1".to_owned(),
            method: "POST",
            uri: "/BatchGetRecord".to_owned(),
            body: Some(json!({ "Identifiers": [{
                "FeatureGroupName": "ghost",
                "RecordIdentifiersValueAsString": ["R1"]
            }] })),
            status: 200,
            code: "ValidationError",
            expected: no_such_feature_group("eu-west-1", "ghost"),
            per_element: true,
        },
    ];

    for row in rows {
        let case = row.case;
        let emulator = Emulator::new();
        emulator.create_feature_group(GROUP);
        let (answered_status, body) = emulator
            .send_in(&row.region, row.method, &row.uri, row.body)
            .await;
        let (answered_code, stated) = if row.per_element {
            first_batch_error(&body)
        } else {
            (error_code(&body), message(&body))
        };
        assert!(
            row.expected.len() <= MAX_MESSAGE_BYTES,
            "case {case:?}: the row must state a message inside the {MAX_MESSAGE_BYTES}-byte \
             bound, but it is {} bytes",
            row.expected.len()
        );
        assert_eq!(
            (answered_status, answered_code.as_str(), stated.as_str()),
            (row.status, row.code, row.expected.as_str()),
            "case {case:?}: expected HTTP {} {} stating {:?}, got {answered_status} \
             {answered_code:?} with {stated:?}",
            row.status,
            row.code,
            row.expected
        );
    }
}

#[tokio::test]
async fn a_refusal_an_operation_words_itself_is_cut_to_the_bound_too() {
    // The rule holds for the message an operation words in its OWN handler as much as for
    // the one it takes from the shared record preparation: `DeleteRecord` reads its
    // `EventTime` from a `@httpQuery` member of its own, and `UpdateRecord` quotes the
    // `RecordIdentifierValueAsString` of the record it was asked to update. Both members
    // are `ValueAsString`, modelled `len 0..358400`, so both refusals are BUILT past the
    // `Message` the operation answers them in. The rows span those two wordings, the
    // query string and the body as the place the quoted value travels in, ASCII and
    // multi-byte values, and the deletion modes the model declares.
    let long_ascii = "x".repeat(5_000);
    let long_other_ascii = "7".repeat(2_600);
    // Each character is three bytes, so the byte bound does not fall on a boundary
    // between them.
    let long_multibyte = "字".repeat(1_500);
    let rows = vec![
        Row {
            case: "DeleteRecord quoting an unreadable ASCII event time",
            region: REGION.to_owned(),
            method: "DELETE",
            uri: format!(
                "/FeatureGroup/{GROUP}?RecordIdentifierValueAsString=R1&EventTime={}",
                query_encoded(&long_ascii)
            ),
            body: None,
            status: 400,
            code: "ValidationError",
            expected: unreadable_delete_event_time(&long_ascii),
            per_element: false,
        },
        Row {
            case: "DeleteRecord quoting an unreadable multi-byte event time",
            region: REGION.to_owned(),
            method: "DELETE",
            uri: format!(
                "/FeatureGroup/{GROUP}?RecordIdentifierValueAsString=R1&EventTime={}",
                query_encoded(&long_multibyte)
            ),
            body: None,
            status: 400,
            code: "ValidationError",
            expected: unreadable_delete_event_time(&long_multibyte),
            per_element: false,
        },
        Row {
            case: "a HardDelete quoting an unreadable event time just past the bound",
            region: REGION.to_owned(),
            method: "DELETE",
            uri: format!(
                "/FeatureGroup/{GROUP}?RecordIdentifierValueAsString=R1&DeletionMode=HardDelete\
                 &EventTime={}",
                query_encoded(&long_other_ascii)
            ),
            body: None,
            status: 400,
            code: "ValidationError",
            expected: unreadable_delete_event_time(&long_other_ascii),
            per_element: false,
        },
        Row {
            case: "UpdateRecord quoting the record identifier a posted feature disagrees with",
            region: REGION.to_owned(),
            method: "POST",
            uri: format!("/FeatureGroup/{GROUP}/Record"),
            body: Some(json!({
                "RecordIdentifierValueAsString": &long_ascii,
                "Features": [feature(IDENTIFIER_FEATURE, "another-record")]
            })),
            status: 400,
            code: "ValidationError",
            expected: identifier_feature_mismatch(&long_ascii),
            per_element: false,
        },
    ];

    for row in rows {
        let emulator = Emulator::new();
        emulator.create_feature_group(GROUP);
        let (status, answered) = emulator
            .send_in(&row.region, row.method, &row.uri, row.body)
            .await;
        let code = error_code(&answered);
        let stated = message(&answered);
        assert_eq!(
            (status, code.as_str()),
            (row.status, row.code),
            "case {:?}: expected HTTP {} {}, got {status} {code:?} with {:?}…",
            row.case,
            row.status,
            row.code,
            stated.chars().take(120).collect::<String>()
        );
        assert_cut_to_bound(row.case, &stated, &row.expected);
    }
}

#[tokio::test]
async fn a_refusal_is_answered_whole_up_to_the_bound_and_cut_only_past_it() {
    // The bound's own edges, reached by PADDING the value a refusal quotes so the
    // unbounded message lands just below the modelled maximum, exactly on it, and just
    // past it: a message the bound admits is answered whole, and only one past it is cut,
    // and then to the maximum itself. Both wordings are driven — the one the shared record
    // preparation builds (`PutRecord`) and the one a handler builds itself
    // (`DeleteRecord`) — so the edge is the member's, not one operation's.
    //
    // (how long the UNBOUNDED message is)
    let lengths = [
        MAX_MESSAGE_BYTES - 1,
        MAX_MESSAGE_BYTES,
        MAX_MESSAGE_BYTES + 1,
        MAX_MESSAGE_BYTES + 2,
    ];

    for length in lengths {
        let wordings = vec![
            Wording {
                operation: "PutRecord",
                word: unreadable_event_time,
                in_query: false,
            },
            Wording {
                operation: "DeleteRecord",
                word: unreadable_delete_event_time,
                in_query: true,
            },
        ];
        for Wording {
            operation,
            word: wording,
            in_query,
        } in wordings
        {
            let case = format!("{operation} stating a {length}-byte refusal");
            let padding = length.checked_sub(wording("").len()).unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the {operation} wording is already longer than {length} bytes"
                )
            });
            // No padding of this value reads as a timestamp, so the refusal is reached
            // whatever its length.
            let value = "x".repeat(padding);
            let expected = wording(&value);
            assert_eq!(
                expected.len(),
                length,
                "case {case:?}: expected a {length}-byte message to state the row, got {} bytes",
                expected.len()
            );

            let emulator = Emulator::new();
            emulator.create_feature_group(GROUP);
            let (status, answered) = if in_query {
                emulator
                    .send_in(
                        REGION,
                        "DELETE",
                        &format!(
                            "/FeatureGroup/{GROUP}?RecordIdentifierValueAsString=R1&EventTime={}",
                            query_encoded(&value)
                        ),
                        None,
                    )
                    .await
            } else {
                emulator
                    .send_in(
                        REGION,
                        "PUT",
                        &format!("/FeatureGroup/{GROUP}"),
                        Some(json!({ "Record": [
                            feature(IDENTIFIER_FEATURE, "R1"),
                            feature(EVENT_TIME_FEATURE, &value)
                        ] })),
                    )
                    .await
            };
            let code = error_code(&answered);
            let stated = message(&answered);
            assert_eq!(
                (status, code.as_str()),
                (400, "ValidationError"),
                "case {case:?}: expected HTTP 400 ValidationError, got {status} {code:?} with \
                 {:?}…",
                stated.chars().take(120).collect::<String>()
            );
            if length <= MAX_MESSAGE_BYTES {
                assert_eq!(
                    stated, expected,
                    "case {case:?}: expected the message the bound admits answered whole, got \
                     {} bytes",
                    stated.len()
                );
            } else {
                assert_eq!(
                    stated.len(),
                    MAX_MESSAGE_BYTES,
                    "case {case:?}: expected a message past the bound cut to exactly \
                     {MAX_MESSAGE_BYTES} bytes, got {} bytes",
                    stated.len()
                );
                assert!(
                    expected.starts_with(&stated),
                    "case {case:?}: expected the answered message to be a prefix of the \
                     {length}-byte refusal, got {:?}…",
                    stated.chars().take(120).collect::<String>()
                );
            }
        }
    }
}
