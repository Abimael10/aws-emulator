// SPDX-License-Identifier: CC0-1.0

//! A DynamoDB stream is identified by its LABEL — "A timestamp, in ISO 8601 format, for
//! this stream" (`Stream$StreamLabel`, and `TableDescription$LatestStreamLabel` for the
//! table's latest one) — and that label is also the last segment of the stream's ARN, the
//! value a table reports as its `LatestStreamArn`.
//!
//! The rule this suite pins: a stream label is rendered at FIXED millisecond precision —
//! a civil ISO 8601 date-time followed by exactly three fractional-second digits — so
//! every label of every stream, whatever instant it names, is spelt in the one form. The
//! width belongs to the resolution the identity is minted at and not to the value, so an
//! instant on a whole second (`…:07.000`) and one whose milliseconds end in a zero
//! (`…:07.380`) are written with the same three digits as any other (`…:07.123`); a
//! renderer that drops a fraction's trailing zeros gives those streams an identifier, and
//! an ARN, spelt unlike every other stream's.
//!
//! This suite spans TWO services by construction: the label is minted by the DynamoDB
//! Streams registry and reported BOTH by that service's own consumer surface
//! (`ListStreams`/`DescribeStream`) and by the DynamoDB table that publishes to it
//! (`CreateTable`/`DescribeTable`), so the form can only be checked on both sides at once
//! here. Every assertion is made over the wire.
//!
//! The rows of the first test differ along what the rule generalises over — the instant's
//! sub-second component: a whole second, each decimal place carrying the only non-zero
//! digit, a fraction whose last digit is a zero, and the largest value a millisecond
//! field holds. The second test crosses the table surface with every `StreamViewType` the
//! model declares, on whatever instant the clock supplies.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_dynamodbstreams_dynamodbstreams_20120810 as streams;
use smithy_server_runtime::MultiServiceRouter;
use streams::ops::types::{KeySchemaElement, STREAM_VIEW_TYPE_VALUES};
use streams::state::{
    ComAmazonawsDynamodbstreamsDynamodbstreams20120810State as StreamsState, StreamRegistration,
    StreamViewType,
};

/// The Region every request below is signed for, and the one the ARNs these services mint
/// name.
const REGION: &str = "us-west-2";

/// The one form a stream label takes: `YYYY-MM-DDTHH:MM:SS.mmm`, written as a SHAPE in
/// which `d` stands for an ASCII digit and every other character stands for itself.
const LABEL_SHAPE: &str = "dddd-dd-ddTdd:dd:dd.ddd";

/// How many fractional-second digits that shape carries.
const FRACTION_DIGITS: usize = 3;

/// Whether `label` is written in [`LABEL_SHAPE`].
fn matches_label_shape(label: &str) -> bool {
    label.len() == LABEL_SHAPE.len()
        && label
            .chars()
            .zip(LABEL_SHAPE.chars())
            .all(|(actual, expected)| match expected {
                'd' => actual.is_ascii_digit(),
                literal => actual == literal,
            })
}

/// The fractional-second field of a label, or `None` where it carries none.
fn fraction_of(label: &str) -> Option<&str> {
    label.split_once('.').map(|(_, fraction)| fraction)
}

/// The instant a label names, in milliseconds since the epoch, read back with the
/// maintained RFC 3339 parser: a label is a civil timestamp in UTC, so the zone the parser
/// requires is appended rather than assumed away.
fn label_millis(case: &str, label: &str) -> i64 {
    let parsed = DateTime::from_str(&format!("{label}Z"), Format::DateTime)
        .unwrap_or_else(|error| panic!("case {case:?}: {label:?} must parse as ISO 8601: {error}"));
    parsed.secs() * 1_000 + i64::from(parsed.subsec_nanos() / 1_000_000)
}

/// Milliseconds since the epoch, as the registry reads an instant.
fn epoch_millis(at: SystemTime) -> i64 {
    i64::try_from(
        at.duration_since(UNIX_EPOCH)
            .expect("the clock is after the Unix epoch")
            .as_millis(),
    )
    .expect("an instant within the i64 millisecond range")
}

/// DynamoDB and DynamoDB Streams over ONE shared engine, as `core/router/src/lib.rs`
/// mounts them: both share the `dynamodb` signing name, so both are mounted for either to
/// be addressed the way a client addresses it.
fn build() -> (Arc<engine::InMemoryEngine>, MultiServiceRouter) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(streams::EmulatedService::new(Arc::clone(&engine)));
    (engine, router)
}

/// The SigV4 credential scope a request to `dynamodb` in [`REGION`] carries. Nothing in it
/// is verified — the harness turns request authentication off — and it is what Region the
/// resources these requests make are scoped to.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/dynamodb/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=deadbeef"
    )
}

/// One `awsJson1_0` RPC call: `POST /` carrying `X-Amz-Target: <prefix>.<operation>`, as
/// its status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("dynamodb.{REGION}.amazonaws.com"))
        .header("authorization", authorization())
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// A DynamoDB call that must answer 200.
async fn ddb(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "DynamoDB_20120810", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer HTTP 200, got {status} with {parsed}"
    );
    parsed
}

/// A DynamoDB Streams call that must answer 200.
async fn streams_call(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = rpc(router, "DynamoDBStreams_20120810", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer HTTP 200, got {status} with {parsed}"
    );
    parsed
}

/// A whole-second instant `seconds` before now. Registrations are placed in the past so
/// the sub-second component of each is the row's own rather than the clock's.
fn seconds_ago(seconds: u64) -> SystemTime {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the Unix epoch")
        .as_secs();
    UNIX_EPOCH + Duration::from_secs(now - seconds)
}

/// Enable a stream on `table_name` at `at` through the publishing seam DynamoDB's
/// `CreateTable`/`UpdateTable` call, and return the ARN it minted. This is the one way to
/// register a stream at a CHOSEN instant: over the wire the instant is the clock's.
fn register(engine: &engine::InMemoryEngine, table_name: &str, at: SystemTime) -> String {
    engine
        .state::<StreamsState>()
        .lock()
        .register_stream(
            &StreamRegistration {
                table_name: table_name.to_string(),
                stream_view_type: StreamViewType::from_wire("NEW_AND_OLD_IMAGES")
                    .expect("NEW_AND_OLD_IMAGES is a modelled StreamViewType"),
                key_schema: vec![KeySchemaElement {
                    attribute_name: "pk".to_string(),
                    key_type: "HASH".to_string(),
                }],
                region: REGION.to_string(),
            },
            at,
        )
        .unwrap_or_else(|rejected| {
            panic!("the registry must accept a stream for {table_name}: {rejected}")
        })
}

/// The sub-second components the rows span, each with the milliseconds the instant carries.
const SUBSECOND_MILLIS: [(&str, u64); 8] = [
    ("a whole second, carrying no fractional digits of its own", 0),
    ("a single millisecond", 1),
    ("tens of milliseconds only", 10),
    ("hundreds of milliseconds only", 100),
    ("a fraction whose last digit is a zero", 380),
    ("a fraction whose last two digits are zeros", 500),
    ("a fraction with no zero in it", 123),
    (
        "the largest value a millisecond fraction holds",
        999,
    ),
];

/// Every label the registry mints is a civil ISO 8601 timestamp with exactly three
/// fractional-second digits, those digits are the instant's own milliseconds, the stream's
/// ARN ends with that label, and the label reads back as the instant it was minted at —
/// for every sub-second component an instant can have.
#[tokio::test]
async fn every_stream_label_is_rendered_at_fixed_millisecond_precision() {
    let (engine, router) = build();

    // Each row is registered a whole second apart from the others, so the instant the row
    // asked for is the instant the stream is labelled with (the registry moves a
    // registration that collides with an existing label to the next free millisecond).
    let mut rows: Vec<(&str, String, u64, SystemTime, String)> = Vec::new();
    for (offset, (case, millis)) in (0_u64..).zip(SUBSECOND_MILLIS) {
        let table_name = format!("label-probe-{offset}");
        let at = seconds_ago(3_600 - offset) + Duration::from_millis(millis);
        let stream_arn = register(&engine, &table_name, at);
        rows.push((case, table_name, millis, at, stream_arn));
    }

    let listed = streams_call(&router, "the whole registry", "ListStreams", &json!({})).await;
    let by_arn: HashMap<String, Value> = listed["Streams"]
        .as_array()
        .unwrap_or_else(|| panic!("ListStreams must report a list, got {listed}"))
        .iter()
        .map(|entry| {
            (
                entry["StreamArn"].as_str().unwrap_or_default().to_string(),
                entry.clone(),
            )
        })
        .collect();

    let mut labels: Vec<String> = Vec::new();
    for (case, table_name, millis, at, stream_arn) in &rows {
        let described = streams_call(
            &router,
            case,
            "DescribeStream",
            &json!({"StreamArn": stream_arn}),
        )
        .await;
        let label = described["StreamDescription"]["StreamLabel"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the stream must report a label, got {described}"))
            .to_string();

        assert!(
            matches_label_shape(&label),
            "case {case:?}: the label must be written as {LABEL_SHAPE} (d for a digit), got \
             {label:?}"
        );
        let fraction = fraction_of(&label).unwrap_or_default();
        assert_eq!(
            fraction.len(),
            FRACTION_DIGITS,
            "case {case:?}: the label must carry {FRACTION_DIGITS} fractional digits, got \
             {fraction:?} in {label:?}"
        );
        assert_eq!(
            fraction,
            format!("{millis:03}"),
            "case {case:?}: the fraction must be the instant's own milliseconds, got \
             {fraction:?} in {label:?}"
        );
        assert_eq!(
            label_millis(case, &label),
            epoch_millis(*at),
            "case {case:?}: the label must read back as the instant it was minted at, got \
             {label:?}"
        );
        assert!(
            stream_arn.ends_with(&format!("/stream/{label}")),
            "case {case:?}: the stream ARN must end with that same label, got {stream_arn:?} and \
             {label:?}"
        );

        // The ARN is the one a client is handed, so the whole of it is checked, not only
        // its last segment.
        assert_eq!(
            *stream_arn,
            format!(
                "arn:aws:dynamodb:{REGION}:{}:table/{table_name}/stream/{label}",
                ::engine::emulator_account_id()
            ),
            "case {case:?}: the ARN must name this table's stream by its label"
        );

        // And the consumer's enumeration reports the same label as the description.
        let entry = by_arn
            .get(stream_arn)
            .unwrap_or_else(|| panic!("case {case:?}: ListStreams must report {stream_arn}"));
        assert_eq!(
            entry["StreamLabel"].as_str(),
            Some(label.as_str()),
            "case {case:?}: ListStreams must report the described label, got {entry}"
        );

        labels.push(label);
    }

    // One label per stream: the fixed width must not merge two instants into one
    // identifier.
    let mut unique = labels.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        labels.len(),
        "the rows name distinct instants, so their labels must differ, got {labels:?}"
    );
}

/// A table that publishes to a stream reports that stream's identity in the same form, and
/// the ARN it reports resolves in the streams service to a stream carrying that label —
/// for every `StreamViewType` the model declares.
#[tokio::test]
async fn a_table_reports_its_stream_identity_in_that_same_form() {
    // The view-type catalog is the list the DynamoDB Streams crate derives from its model,
    // so a value added to the enum is covered without this test being edited.
    for view_type in STREAM_VIEW_TYPE_VALUES {
        let (_engine, router) = build();
        let case = format!("a table publishing {view_type}");
        let table_name = format!("ledger-{}", view_type.to_lowercase().replace('_', "-"));

        let created = ddb(
            &router,
            &case,
            "CreateTable",
            &json!({
                "TableName": table_name,
                "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
                "BillingMode": "PAY_PER_REQUEST",
                "StreamSpecification": {"StreamEnabled": true, "StreamViewType": view_type},
            }),
        )
        .await;
        let created_arn = created["TableDescription"]["LatestStreamArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the create must report a stream ARN, got {created}"))
            .to_string();

        let described = ddb(
            &router,
            &case,
            "DescribeTable",
            &json!({"TableName": table_name}),
        )
        .await;
        let label = described["Table"]["LatestStreamLabel"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the table must report its stream label, got {described}"))
            .to_string();
        let stream_arn = described["Table"]["LatestStreamArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the table must report its stream ARN, got {described}"))
            .to_string();

        assert_eq!(
            stream_arn, created_arn,
            "case {case:?}: DescribeTable must report the stream the create minted, got \
             {described}"
        );
        assert!(
            matches_label_shape(&label),
            "case {case:?}: LatestStreamLabel must be written as {LABEL_SHAPE} (d for a digit), \
             got {label:?}"
        );
        assert_eq!(
            fraction_of(&label).map(str::len),
            Some(FRACTION_DIGITS),
            "case {case:?}: LatestStreamLabel must carry {FRACTION_DIGITS} fractional digits, got \
             {label:?}"
        );
        assert_eq!(
            stream_arn,
            format!(
                "arn:aws:dynamodb:{REGION}:{}:table/{table_name}/stream/{label}",
                ::engine::emulator_account_id()
            ),
            "case {case:?}: LatestStreamArn must name this table's stream by the label the table \
             reports, got {described}"
        );

        // The ARN the table reports is the streams service's own resource: it resolves, and
        // reports the same label.
        let stream = streams_call(
            &router,
            &case,
            "DescribeStream",
            &json!({"StreamArn": stream_arn}),
        )
        .await;
        assert_eq!(
            stream["StreamDescription"]["StreamLabel"].as_str(),
            Some(label.as_str()),
            "case {case:?}: the stream must report the label its table does, got {stream}"
        );
        assert_eq!(
            stream["StreamDescription"]["TableName"].as_str(),
            Some(table_name.as_str()),
            "case {case:?}: the stream must name the table that publishes to it, got {stream}"
        );
    }
}
