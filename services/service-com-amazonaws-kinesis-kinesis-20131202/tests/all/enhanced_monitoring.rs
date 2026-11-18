// SPDX-License-Identifier: CC0-1.0

//! A Kinesis data stream's ENHANCED SHARD-LEVEL METRICS, end to end over the wire:
//! `CreateStream` -> `EnableEnhancedMonitoring` / `DisableEnhancedMonitoring` ->
//! `DescribeStreamSummary` / `DescribeStream`.
//!
//! RULE: a stream carries a SET of enhanced shard-level metrics, empty until something
//! enhances one. `EnableEnhancedMonitoring` ADDS the metrics its request names to that set
//! and `DisableEnhancedMonitoring` REMOVES them, the catch-all value standing for every
//! metric the model's `MetricsName` enum lists; each call reports the set as it was before
//! the call (`CurrentShardLevelMetrics`) beside the set it leaves behind
//! (`DesiredShardLevelMetrics`) and persists the latter, so whatever a change stored is
//! what every later read of that stream reports. Naming a metric that is already enhanced,
//! or disabling one nothing enhanced, therefore leaves the same set behind. A value the
//! enum does not accept is refused with a 400 and leaves the stored set exactly as it was;
//! either call resolves its stream by `StreamName`, by `StreamARN` or by both inside the
//! region the request was signed for, is `ResourceNotFoundException` for a stream this
//! account and region do not hold, `InvalidArgumentException` for a request naming no
//! stream at all, and `ResourceInUseException` while the stream is not `ACTIVE`.
//!
//! The recorded flows this was derived from enable metrics on ONE stream and ask one
//! invalid metric of the disabling call; those are two rows of the tables below. The rows
//! differ along what the rule generalises over: which operation is called, how many metrics
//! one call names, whether they are already enhanced, whether the catch-all or individual
//! metrics are named, how many calls one stream takes, the identifier the request resolves
//! the stream by, and the status the stream is in when the call arrives.
//!
//! Every case drives the real awsJson1_1 request path (`POST /` with `X-Amz-Target:
//! Kinesis_20131202.<Operation>` through `MultiServiceRouter::dispatch`), so the statuses,
//! bodies and modelled errors asserted here are the ones a client sees.
//!
//! What that stored set DOES is the second rule of this suite: a stream's data plane publishes
//! its traffic as `AWS/Kinesis` CloudWatch series — stream-level for every stream with no
//! opt-in, and additionally dimensioned by `ShardId` for exactly the metrics the stream's
//! enhanced monitoring names — so the last case mounts CloudWatch beside Kinesis on one engine
//! and reads those series back where a client reads them.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_kinesis_kinesis_20131202::state::{
    shard_id, stream_not_found_message, STREAM_CREATING, STREAM_IDENTIFIER_REQUIRED_MESSAGE,
};
use service_com_amazonaws_kinesis_kinesis_20131202::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::stream_tags::{
    error_code, error_message, past_activation, poll_until, stream_arn, Emulator, REGION,
};

/// Every shard-level metric enhanced monitoring reports, spelled out here rather than read
/// from the service so a catalog that drifts fails this test instead of agreeing with
/// itself: the values of the `MetricsName` enum
/// (`smithy_query(query="shape",shape="MetricsName")`) less the catch-all `ALL`, which
/// "enables every metric" (`EnableEnhancedMonitoringInput$ShardLevelMetrics`) rather than
/// naming one. This is also the order every report of the set is in, so two reads of one
/// stream cannot disagree about it.
const EVERY_METRIC: &[&str] = &[
    "IncomingBytes",
    "IncomingRecords",
    "IteratorAgeMilliseconds",
    "OutgoingBytes",
    "OutgoingRecords",
    "ReadProvisionedThroughputExceeded",
    "WriteProvisionedThroughputExceeded",
];

/// The catch-all value that stands for [`EVERY_METRIC`].
const ALL: &str = "ALL";

/// Which way a step moves the stream's set.
#[derive(Debug, Clone, Copy)]
enum Change {
    /// `EnableEnhancedMonitoring`, which adds to the set.
    Enable,
    /// `DisableEnhancedMonitoring`, which takes away from it.
    Disable,
}

impl Change {
    /// The operation this direction is served by.
    fn operation(self) -> &'static str {
        match self {
            Self::Enable => "EnableEnhancedMonitoring",
            Self::Disable => "DisableEnhancedMonitoring",
        }
    }
}

/// How a step addresses its stream: either identifier resolves it, and so do both together.
#[derive(Debug, Clone, Copy)]
enum By {
    Name,
    Arn,
    Both,
}

impl By {
    /// The request body naming `stream`, carrying `metrics`.
    fn body(self, stream: &str, metrics: &[&str]) -> Value {
        match self {
            Self::Name => json!({ "StreamName": stream, "ShardLevelMetrics": metrics }),
            Self::Arn => json!({
                "StreamARN": stream_arn(REGION, stream),
                "ShardLevelMetrics": metrics,
            }),
            Self::Both => json!({
                "StreamName": stream,
                "StreamARN": stream_arn(REGION, stream),
                "ShardLevelMetrics": metrics,
            }),
        }
    }
}

/// One step of a row: the change it asks for, the metrics it names, how it addresses the
/// stream, and the set the stream must report once it is served.
struct Step {
    change: Change,
    metrics: &'static [&'static str],
    by: By,
    leaves: &'static [&'static str],
}

/// The metrics `expected` names, in the order every report of the set is in.
fn in_catalog_order(expected: &[&str]) -> Vec<String> {
    EVERY_METRIC
        .iter()
        .filter(|metric| expected.contains(metric))
        .map(|metric| (*metric).to_string())
        .collect()
}

/// The `ShardLevelMetrics` list a response member carries, as strings.
fn metric_list(case: &str, response: &Value, member: &str) -> Vec<String> {
    let list = response
        .get(member)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case}: the response must carry {member}, got {response}"));
    list.iter()
        .map(|metric| {
            metric
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: {member} holds strings, got {metric}"))
                .to_string()
        })
        .collect()
}

/// The enhanced set `DescribeStreamSummary` reports for `stream`.
async fn summarised_metrics(emulator: &Emulator, case: &str, stream: &str) -> Vec<String> {
    let described = emulator
        .ok(case, "DescribeStreamSummary", &json!({ "StreamName": stream }))
        .await;
    let entries = described["StreamDescriptionSummary"]
        .get("EnhancedMonitoring")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!("case {case}: the summary must carry EnhancedMonitoring, got {described}")
        });
    assert_eq!(
        entries.len(),
        1,
        "case {case}: expected one EnhancedMonitoring entry, got {described}"
    );
    metric_list(case, &entries[0], "ShardLevelMetrics")
}

/// The enhanced set `DescribeStream` reports for `stream`.
async fn described_metrics(emulator: &Emulator, case: &str, stream: &str) -> Vec<String> {
    let described = emulator
        .ok(case, "DescribeStream", &json!({ "StreamName": stream }))
        .await;
    let entries = described["StreamDescription"]
        .get("EnhancedMonitoring")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!("case {case}: the description must carry EnhancedMonitoring, got {described}")
        });
    assert_eq!(
        entries.len(),
        1,
        "case {case}: expected one EnhancedMonitoring entry, got {described}"
    );
    metric_list(case, &entries[0], "ShardLevelMetrics")
}

/// Every accepted change reports the set it replaced beside the set it stored, and the
/// stored one is what both reads of the stream report afterwards.
#[tokio::test]
async fn a_metric_change_is_what_a_read_reports() {
    // Each row is a sequence of changes applied to ONE stream, each with the set the stream
    // must carry afterwards. A freshly created stream enhances no metric, so every row
    // starts from the empty set.
    let rows: Vec<(&str, Vec<Step>)> = vec![
        (
            "one-metric",
            vec![Step {
                change: Change::Enable,
                metrics: &["IncomingBytes"],
                by: By::Name,
                leaves: &["IncomingBytes"],
            }],
        ),
        (
            "several-metrics-at-once",
            vec![Step {
                change: Change::Enable,
                // Named out of the catalog's order: the request's order is not the set's.
                metrics: &["OutgoingRecords", "IncomingRecords"],
                by: By::Arn,
                leaves: &["IncomingRecords", "OutgoingRecords"],
            }],
        ),
        (
            "the-catch-all",
            vec![Step {
                change: Change::Enable,
                metrics: &[ALL],
                by: By::Both,
                leaves: EVERY_METRIC,
            }],
        ),
        (
            "metrics-accumulate",
            vec![
                Step {
                    change: Change::Enable,
                    metrics: &["IteratorAgeMilliseconds"],
                    by: By::Name,
                    leaves: &["IteratorAgeMilliseconds"],
                },
                Step {
                    change: Change::Enable,
                    metrics: &["OutgoingBytes", "IncomingBytes"],
                    by: By::Arn,
                    leaves: &["IncomingBytes", "IteratorAgeMilliseconds", "OutgoingBytes"],
                },
            ],
        ),
        (
            "enabling-what-is-already-enhanced-changes-nothing",
            vec![
                Step {
                    change: Change::Enable,
                    metrics: &["ReadProvisionedThroughputExceeded"],
                    by: By::Name,
                    leaves: &["ReadProvisionedThroughputExceeded"],
                },
                Step {
                    change: Change::Enable,
                    // The same metric twice in one request, and one already enhanced.
                    metrics: &[
                        "ReadProvisionedThroughputExceeded",
                        "ReadProvisionedThroughputExceeded",
                    ],
                    by: By::Name,
                    leaves: &["ReadProvisionedThroughputExceeded"],
                },
            ],
        ),
        (
            "one-metric-back-off-the-catch-all",
            vec![
                Step {
                    change: Change::Enable,
                    metrics: &[ALL],
                    by: By::Name,
                    leaves: EVERY_METRIC,
                },
                Step {
                    change: Change::Disable,
                    metrics: &["IncomingBytes"],
                    by: By::Arn,
                    leaves: &[
                        "IncomingRecords",
                        "IteratorAgeMilliseconds",
                        "OutgoingBytes",
                        "OutgoingRecords",
                        "ReadProvisionedThroughputExceeded",
                        "WriteProvisionedThroughputExceeded",
                    ],
                },
            ],
        ),
        (
            "enable-then-disable-the-same-metrics",
            vec![
                Step {
                    change: Change::Enable,
                    metrics: &["WriteProvisionedThroughputExceeded", "OutgoingBytes"],
                    by: By::Name,
                    leaves: &["OutgoingBytes", "WriteProvisionedThroughputExceeded"],
                },
                Step {
                    change: Change::Disable,
                    metrics: &["OutgoingBytes", "WriteProvisionedThroughputExceeded"],
                    by: By::Name,
                    leaves: &[],
                },
            ],
        ),
        (
            "disabling-what-nothing-enhanced-changes-nothing",
            vec![
                Step {
                    change: Change::Enable,
                    metrics: &["IncomingRecords"],
                    by: By::Name,
                    leaves: &["IncomingRecords"],
                },
                Step {
                    change: Change::Disable,
                    metrics: &["OutgoingRecords"],
                    by: By::Name,
                    leaves: &["IncomingRecords"],
                },
            ],
        ),
        (
            "the-catch-all-takes-every-metric-off",
            vec![
                Step {
                    change: Change::Enable,
                    metrics: &["IncomingBytes", "OutgoingBytes", "IteratorAgeMilliseconds"],
                    by: By::Name,
                    leaves: &["IncomingBytes", "IteratorAgeMilliseconds", "OutgoingBytes"],
                },
                Step {
                    change: Change::Disable,
                    metrics: &[ALL],
                    by: By::Both,
                    leaves: &[],
                },
            ],
        ),
        (
            "disabling-nothing-enhanced-at-all",
            vec![Step {
                change: Change::Disable,
                metrics: &["IncomingBytes"],
                by: By::Name,
                leaves: &[],
            }],
        ),
    ];

    let emulator = Emulator::new();
    for (name, _) in &rows {
        emulator
            .ok(
                name,
                "CreateStream",
                &json!({ "StreamName": name, "ShardCount": 1 }),
            )
            .await;
    }
    tokio::time::sleep(past_activation()).await;

    for (name, steps) in &rows {
        let mut carried: Vec<String> = Vec::new();
        assert_eq!(
            summarised_metrics(&emulator, name, name).await,
            carried,
            "case {name}: expected a new stream to enhance no metric"
        );

        for (index, step) in steps.iter().enumerate() {
            let case = format!("{name}-{index}-{}", step.change.operation());
            let response = emulator
                .ok(
                    &case,
                    step.change.operation(),
                    &step.by.body(name, step.metrics),
                )
                .await;

            // "The name of the Kinesis data stream" and "the ARN of the stream" are the
            // stream the call resolved, whichever identifier named it.
            assert_eq!(
                response.get("StreamName").and_then(Value::as_str),
                Some(*name),
                "case {case}: expected the response to name the stream, got {response}"
            );
            assert_eq!(
                response.get("StreamARN").and_then(Value::as_str),
                Some(stream_arn(REGION, name).as_str()),
                "case {case}: expected the response to carry the stream's ARN, got {response}"
            );

            let current = metric_list(&case, &response, "CurrentShardLevelMetrics");
            assert_eq!(
                current, carried,
                "case {case}: expected the set before the call, got {current:?}"
            );
            let desired = metric_list(&case, &response, "DesiredShardLevelMetrics");
            let expected = in_catalog_order(step.leaves);
            assert_eq!(
                desired, expected,
                "case {case}: expected the call to leave {expected:?}, got {desired:?}"
            );

            // The stored set is what both reads of the stream report.
            let summarised = summarised_metrics(&emulator, &case, name).await;
            assert_eq!(
                summarised, expected,
                "case {case}: expected the summary to report {expected:?}, got {summarised:?}"
            );
            let described = described_metrics(&emulator, &case, name).await;
            assert_eq!(
                described, expected,
                "case {case}: expected the description to report {expected:?}, got {described:?}"
            );
            carried = expected;
        }
    }
}

/// One refused change: the metrics it names, the stream it addresses and the refusal it
/// must answer.
struct Refusal {
    case: &'static str,
    change: Change,
    metrics: &'static [&'static str],
    code: &'static str,
    /// What the refusal's message must name, built from the row's own stream and metrics.
    says: fn(&str, &[&str]) -> Vec<String>,
    /// Whether the row's stream exists and is `ACTIVE` when the call arrives.
    subject: Subject,
}

/// What the stream a refusal row addresses looks like when the call arrives.
#[derive(Debug, Clone, Copy)]
enum Subject {
    /// An `ACTIVE` stream carrying one enhanced metric, so a refusal that wrote anything
    /// would show.
    Enhanced,
    /// A stream whose create has not fallen due yet.
    Creating,
    /// No stream at all.
    Absent,
}

/// The metrics the `Enhanced` subject carries when the call arrives.
const CARRIED: &[&str] = &["IncomingBytes"];

/// A refused change answers a client error and leaves the stored set exactly as it was.
#[tokio::test]
async fn a_refused_metric_change_leaves_the_set_as_it_was() {
    let rows: Vec<Refusal> = vec![
        Refusal {
            case: "unknown-metric",
            change: Change::Disable,
            metrics: &["BytesIncoming"],
            code: "ValidationException",
            says: |_, metrics| refused_value(metrics),
            subject: Subject::Enhanced,
        },
        Refusal {
            case: "unknown-metric-enabling",
            change: Change::Enable,
            metrics: &["BytesIncoming"],
            code: "ValidationException",
            says: |_, metrics| refused_value(metrics),
            subject: Subject::Enhanced,
        },
        Refusal {
            // The enum's values are case-sensitive, so a metric spelled another way names
            // no metric.
            case: "a-metric-in-another-casing",
            change: Change::Enable,
            metrics: &["incomingbytes"],
            code: "ValidationException",
            says: |_, metrics| refused_value(metrics),
            subject: Subject::Enhanced,
        },
        Refusal {
            case: "the-catch-all-in-another-casing",
            change: Change::Enable,
            metrics: &["all"],
            code: "ValidationException",
            says: |_, metrics| refused_value(metrics),
            subject: Subject::Enhanced,
        },
        Refusal {
            // One good metric does not carry a bad one through: the whole list is refused
            // and nothing is enhanced.
            case: "one-good-metric-beside-a-bad-one",
            change: Change::Enable,
            metrics: &["OutgoingBytes", "SomethingElse"],
            code: "ValidationException",
            says: |_, _| refused_value(&["SomethingElse"]),
            subject: Subject::Enhanced,
        },
        Refusal {
            case: "no-such-stream",
            change: Change::Enable,
            metrics: &["IncomingBytes"],
            code: "ResourceNotFoundException",
            says: |stream, _| vec![stream_not_found_message(stream)],
            subject: Subject::Absent,
        },
        Refusal {
            case: "no-such-stream-disabling",
            change: Change::Disable,
            metrics: &[ALL],
            code: "ResourceNotFoundException",
            says: |stream, _| vec![stream_not_found_message(stream)],
            subject: Subject::Absent,
        },
        Refusal {
            case: "still-creating",
            change: Change::Enable,
            metrics: &["IncomingBytes"],
            code: "ResourceInUseException",
            says: |_, _| vec![STREAM_CREATING.to_string()],
            subject: Subject::Creating,
        },
    ];

    let emulator = Emulator::new();
    for row in &rows {
        if let Subject::Enhanced = row.subject {
            emulator
                .ok(
                    row.case,
                    "CreateStream",
                    &json!({ "StreamName": row.case, "ShardCount": 1 }),
                )
                .await;
        }
    }
    tokio::time::sleep(past_activation()).await;

    for row in &rows {
        let case = row.case;
        match row.subject {
            Subject::Enhanced => {
                emulator
                    .ok(
                        case,
                        "EnableEnhancedMonitoring",
                        &json!({ "StreamName": case, "ShardLevelMetrics": CARRIED }),
                    )
                    .await;
            }
            Subject::Creating => {
                emulator
                    .ok(
                        case,
                        "CreateStream",
                        &json!({ "StreamName": case, "ShardCount": 1 }),
                    )
                    .await;
            }
            Subject::Absent => {}
        }

        let (status, response) = emulator
            .call(
                row.change.operation(),
                &json!({ "StreamName": case, "ShardLevelMetrics": row.metrics }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case}: expected a client error, got {status} with body {response}"
        );
        assert_eq!(
            error_code(&response),
            row.code,
            "case {case}: expected {}, got {response}",
            row.code
        );
        let message = error_message(&response);
        for says in (row.says)(case, row.metrics) {
            assert!(
                message.contains(&says),
                "case {case}: expected the message to name {says:?}, got {message:?}"
            );
        }

        if let Subject::Enhanced = row.subject {
            let after = summarised_metrics(&emulator, case, case).await;
            assert_eq!(
                after,
                in_catalog_order(CARRIED),
                "case {case}: a refused change must leave the stored set untouched, got \
                 {after:?}"
            );
        }
    }
}

/// What a refusal of a value outside the enum must name: the count of refused values, and
/// each value at fault, taken from the request.
fn refused_value(metrics: &[&str]) -> Vec<String> {
    let mut says = vec!["1 validation error detected".to_string()];
    says.extend(metrics.iter().map(|metric| (*metric).to_string()));
    says.push("shardLevelMetrics".to_string());
    says
}

/// A refusal of a value outside the enum names the value at fault, the member that carried
/// it and every value the enum accepts — the three variable parts of the sentence the
/// service states such a refusal with, taken from the request and from the model.
///
/// The sentence itself is the framework's, stated once before any handler runs for every
/// modelled constraint of every operation; what this pins is that the refusal of this
/// member says which value was refused, where it sat and what it had to be.
#[tokio::test]
async fn an_unsupported_metric_is_refused_with_what_the_enum_accepts() {
    let emulator = Emulator::new();
    emulator
        .ok(
            "wording",
            "CreateStream",
            &json!({ "StreamName": "wording", "ShardCount": 1 }),
        )
        .await;
    tokio::time::sleep(past_activation()).await;

    for operation in ["EnableEnhancedMonitoring", "DisableEnhancedMonitoring"] {
        let (status, response) = emulator
            .call(
                operation,
                &json!({
                    "StreamName": "wording",
                    "ShardLevelMetrics": ["IncomingBytes", "NotAMetric"],
                }),
            )
            .await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "ValidationException"),
            "case {operation}: expected the refusal, got {status} with body {response}"
        );
        let message = error_message(&response);
        for says in ["1 validation error detected", "NotAMetric", "shardLevelMetrics"] {
            assert!(
                message.contains(says),
                "case {operation}: expected the message to name {says:?}, got {message:?}"
            );
        }
        for metric in EVERY_METRIC.iter().chain(std::iter::once(&ALL)) {
            assert!(
                message.contains(metric),
                "case {operation}: expected the message to name the accepted value \
                 {metric:?}, got {message:?}"
            );
        }
    }
}

/// A request naming no stream at all is refused by the rule every stream operation applies,
/// before any metric is read or written.
#[tokio::test]
async fn a_metric_change_needs_an_identifier() {
    let emulator = Emulator::new();
    for operation in ["EnableEnhancedMonitoring", "DisableEnhancedMonitoring"] {
        let (status, response) = emulator
            .call(operation, &json!({ "ShardLevelMetrics": ["IncomingBytes"] }))
            .await;
        assert_eq!(
            status, 400,
            "case {operation}: expected a client error, got {status} with body {response}"
        );
        assert_eq!(
            error_code(&response),
            "InvalidArgumentException",
            "case {operation}: expected InvalidArgumentException, got {response}"
        );
        let message = error_message(&response);
        assert!(
            message.contains(STREAM_IDENTIFIER_REQUIRED_MESSAGE),
            "case {operation}: expected the message to name the identifier rule, got \
             {message:?}"
        );
    }
}

/// A router carrying this service AND CloudWatch over ONE shared engine: the same assembly
/// `core/router` performs, with the two services a published metric involves — the stream's
/// data plane writes the series, and CloudWatch's own store is where a client reads them.
struct Monitored {
    router: MultiServiceRouter,
}

impl Monitored {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The signature itself is not under test here, so requests are unauthenticated.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
                engine,
            ),
        );
        Self { router }
    }

    /// Send one request to the service `signing_name` signs for, named under `target`.
    async fn call(&self, signing_name: &str, target: &str, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", target)
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
                     aws4_request, SignedHeaders=host;x-amz-target, Signature=deadbeef"
                ),
            )
            .body(Bytes::from(body.to_string()))
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        (status, parsed)
    }

    /// One Kinesis operation that must succeed, returning its parsed body.
    async fn kinesis(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self
            .call("kinesis", &format!("Kinesis_20131202.{operation}"), body)
            .await;
        assert_eq!(
            status, 200,
            "case {case}: expected {operation} to be served, got {status} with {parsed}"
        );
        parsed
    }

    /// One CloudWatch operation that must succeed, returning its parsed body.
    async fn cloudwatch(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self
            .call(
                "monitoring",
                &format!("GraniteServiceVersion20100801.{operation}"),
                body,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case}: expected CloudWatch's {operation} to be served, got {status} with \
             {parsed}"
        );
        parsed
    }
}

/// The namespace Amazon Kinesis Data Streams publishes a stream's own metrics under
/// (<https://docs.aws.amazon.com/streams/latest/dev/monitoring-with-cloudwatch.html>).
const KINESIS_NAMESPACE: &str = "AWS/Kinesis";

/// The dimension that names the stream a series belongs to.
const STREAM_NAME_DIMENSION: &str = "StreamName";

/// The dimension a shard-level series carries beside it.
const SHARD_ID_DIMENSION: &str = "ShardId";

/// What one series holds over a window wide enough for everything a case published.
#[derive(Debug)]
struct Reading {
    /// How many datapoints the window reported.
    datapoints: usize,
    /// The sum of their `Sum` statistics: what the calls contributed.
    sum: f64,
    /// The sum of their `SampleCount` statistics: how many calls contributed.
    sample_count: f64,
    /// The unit each datapoint carries.
    units: Vec<String>,
}

/// The largest difference two of these figures may show and still be the same figure: both
/// sides are sums of whole records and whole bytes, so anything above rounding is a real
/// divergence.
const TOLERANCE: f64 = 1e-9;

/// Read one `AWS/Kinesis` series back through CloudWatch, as a client reads it.
async fn series(monitored: &Monitored, case: &str, metric: &str, dimensions: Value) -> Reading {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("an instant after the epoch")
        .as_secs_f64();
    let answered = monitored
        .cloudwatch(
            case,
            "GetMetricStatistics",
            &json!({
                "Namespace": KINESIS_NAMESPACE,
                "MetricName": metric,
                "Dimensions": dimensions,
                // A window far wider than any case needs on either side, so nothing here
                // depends on how fast the host runs.
                "StartTime": now - 3_600.0,
                "EndTime": now + 3_600.0,
                "Period": 60,
                "Statistics": ["Sum", "SampleCount"],
            }),
        )
        .await;
    let points: Vec<Value> = answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    Reading {
        datapoints: points.len(),
        sum: points.iter().filter_map(|point| point["Sum"].as_f64()).sum(),
        sample_count: points
            .iter()
            .filter_map(|point| point["SampleCount"].as_f64())
            .sum(),
        units: points
            .iter()
            .filter_map(|point| point["Unit"].as_str().map(str::to_string))
            .collect(),
    }
}

/// The dimension set a stream-level series is published under.
fn stream_level(stream: &str) -> Value {
    json!([{ "Name": STREAM_NAME_DIMENSION, "Value": stream }])
}

/// The dimension set a shard-level series is published under.
fn shard_level(stream: &str, shard: &str) -> Value {
    json!([
        { "Name": STREAM_NAME_DIMENSION, "Value": stream },
        { "Name": SHARD_ID_DIMENSION, "Value": shard },
    ])
}

/// The record data every write of the metrics cases carries, so a byte sum is a record count
/// times one record's size.
const PAYLOAD: &str = "metrics-payload";

/// The partition key every one of those writes carries, which counts towards the record's size
/// beside its data: "when the data blob (the payload before base64-encoding) is added to the
/// partition key size, the total size must not exceed the maximum record size"
/// (`PutRecordInput$Data`).
const PARTITION_KEY: &str = "metrics";

/// One `PutRecord` of [`PAYLOAD`] into `stream`.
async fn put_one(monitored: &Monitored, case: &str, stream: &str) {
    monitored
        .kinesis(
            case,
            "PutRecord",
            &json!({
                "StreamName": stream,
                "PartitionKey": PARTITION_KEY,
                "Data": aws_smithy_types::base64::encode(PAYLOAD),
            }),
        )
        .await;
}

/// Create `stream` with one shard and wait for the ACTIVE its create owes it.
async fn active_stream(monitored: &Monitored, case: &str, stream: &str) {
    monitored
        .kinesis(
            case,
            "CreateStream",
            &json!({ "StreamName": stream, "ShardCount": 1 }),
        )
        .await;
    poll_until(case, &format!("the stream {stream} is active"), || async {
        let (status, response) = monitored
            .call(
                "kinesis",
                "Kinesis_20131202.DescribeStreamSummary",
                &json!({ "StreamName": stream }),
            )
            .await;
        status == 200
            && response["StreamDescriptionSummary"]["StreamStatus"].as_str() == Some("ACTIVE")
    })
    .await;
}

/// RULE: a stream's data plane publishes its traffic as `AWS/Kinesis` series — stream-level
/// for every stream with no opt-in, and additionally dimensioned by `ShardId` for exactly the
/// metrics the stream's own enhanced monitoring names. The sums are what the calls did: N
/// writes of equal-length payloads come to `IncomingRecords` N and `IncomingBytes` N times
/// that length, a read comes to `OutgoingRecords` for what it served, and a stream nothing
/// enhanced has no shard-dimensioned series at all.
///
/// The cases span what the rule generalises over: both write APIs (`PutRecord` and the
/// `PutRecords` batch) and the read, a stream with enhanced monitoring and a twin without one,
/// metrics inside the enabled set and metrics outside it, and the window before the enable
/// beside the window after it.
#[tokio::test]
async fn a_streams_data_plane_publishes_its_aws_kinesis_series() {
    let monitored = Monitored::new();
    let armed = "metrics-armed";
    let bare = "metrics-bare";
    let shard = shard_id(0);
    let record_bytes = f64::from(
        u32::try_from(PAYLOAD.len() + PARTITION_KEY.len())
            .expect("the records of these cases are a few bytes long"),
    );

    let case = "before the enable";
    active_stream(&monitored, case, armed).await;
    active_stream(&monitored, case, bare).await;
    for _ in 0..4 {
        put_one(&monitored, case, armed).await;
    }
    for _ in 0..2 {
        put_one(&monitored, case, bare).await;
    }

    // Every stream publishes the stream-level series with no opt-in at all...
    for (stream, records) in [(armed, 4.0), (bare, 2.0)] {
        let reading = series(
            &monitored,
            case,
            "IncomingRecords",
            stream_level(stream),
        )
        .await;
        assert!(
            (reading.sum - records).abs() < TOLERANCE,
            "case {case}: expected {stream}'s stream-level IncomingRecords to sum to \
             {records}, got {reading:?}"
        );
        assert!(
            (reading.sample_count - records).abs() < TOLERANCE,
            "case {case}: expected one observation per write of {stream}, got {reading:?}"
        );
        assert!(
            reading.units.iter().all(|unit| unit == "Count"),
            "case {case}: expected a record tally in Count, got {reading:?}"
        );
        let bytes = series(&monitored, case, "IncomingBytes", stream_level(stream)).await;
        assert!(
            (bytes.sum - records * record_bytes).abs() < TOLERANCE,
            "case {case}: expected {stream}'s stream-level IncomingBytes to sum to the \
             records' own data, got {bytes:?}"
        );
        assert!(
            bytes.units.iter().all(|unit| unit == "Bytes"),
            "case {case}: expected a data measurement in Bytes, got {bytes:?}"
        );
    }

    // ... and NO shard-dimensioned series until enhanced monitoring names the metric.
    for stream in [armed, bare] {
        for metric in ["IncomingRecords", "IncomingBytes", "OutgoingRecords"] {
            let reading = series(&monitored, case, metric, shard_level(stream, &shard)).await;
            assert_eq!(
                reading.datapoints, 0,
                "case {case}: a stream whose enhanced monitoring names nothing publishes no \
                 ShardId-dimensioned {metric} for {stream}, got {reading:?}"
            );
        }
    }

    // Arm ONE of the two streams, and write to both again: three single writes and one batch
    // of two, so both write APIs contribute.
    let case = "after the enable";
    monitored
        .kinesis(
            case,
            "EnableEnhancedMonitoring",
            &json!({
                "StreamName": armed,
                "ShardLevelMetrics": ["IncomingRecords", "IncomingBytes", "OutgoingRecords"],
            }),
        )
        .await;
    for _ in 0..3 {
        put_one(&monitored, case, armed).await;
    }
    monitored
        .kinesis(
            case,
            "PutRecords",
            &json!({
                "StreamName": armed,
                "Records": [
                    {
                        "PartitionKey": PARTITION_KEY,
                        "Data": aws_smithy_types::base64::encode(PAYLOAD),
                    },
                    {
                        "PartitionKey": PARTITION_KEY,
                        "Data": aws_smithy_types::base64::encode(PAYLOAD),
                    },
                ],
            }),
        )
        .await;
    put_one(&monitored, case, bare).await;

    // The stream-level series keep counting every write, before and after the enable.
    for (stream, records) in [(armed, 9.0), (bare, 3.0)] {
        let reading = series(&monitored, case, "IncomingRecords", stream_level(stream)).await;
        assert!(
            (reading.sum - records).abs() < TOLERANCE,
            "case {case}: expected {stream}'s stream-level IncomingRecords to sum to \
             {records}, got {reading:?}"
        );
    }

    // The armed stream's shard-level series carry what was written AFTER the enable, under
    // the SAME metric names, one observation per call.
    for (metric, expected) in [
        ("IncomingRecords", 5.0),
        ("IncomingBytes", 5.0 * record_bytes),
    ] {
        let reading = series(&monitored, case, metric, shard_level(armed, &shard)).await;
        assert!(
            (reading.sum - expected).abs() < TOLERANCE,
            "case {case}: expected the ShardId-dimensioned {metric} of {armed} to sum to \
             {expected}, got {reading:?}"
        );
        assert!(
            (reading.sample_count - 4.0).abs() < TOLERANCE,
            "case {case}: expected one observation per call that wrote to the shard (three \
             PutRecord and one PutRecords), got {reading:?}"
        );
    }

    // A metric the enable did NOT name has no shard-dimensioned series, and the twin stream
    // has none at all.
    for (stream, metric) in [
        (armed, "OutgoingBytes"),
        (armed, "IteratorAgeMilliseconds"),
        (bare, "IncomingRecords"),
        (bare, "IncomingBytes"),
    ] {
        let reading = series(&monitored, case, metric, shard_level(stream, &shard)).await;
        assert_eq!(
            reading.datapoints, 0,
            "case {case}: expected no ShardId-dimensioned {metric} for {stream}, got \
             {reading:?}"
        );
    }

    // The per-API families of the calls themselves, which carry no shard dimension.
    for (metric, expected) in [
        ("PutRecord.Success", 7.0),
        ("PutRecords.SuccessfulRecords", 2.0),
        ("PutRecords.TotalRecords", 2.0),
    ] {
        let reading = series(&monitored, case, metric, stream_level(armed)).await;
        assert!(
            (reading.sum - expected).abs() < TOLERANCE,
            "case {case}: expected {metric} of {armed} to sum to {expected}, got {reading:?}"
        );
    }

    // A series exists where its own kind of traffic happened and nowhere else: the retrieval
    // counters have had no read yet, and the shard-dimensioned form of a counter the enable
    // did not name stays absent whatever the stream's calls do.
    for (metric, dimensions) in [
        ("OutgoingRecords", stream_level(armed)),
        ("OutgoingRecords", shard_level(armed, &shard)),
        ("WriteProvisionedThroughputExceeded", stream_level(armed)),
        ("WriteProvisionedThroughputExceeded", shard_level(armed, &shard)),
        ("OutgoingBytes", shard_level(armed, &shard)),
    ] {
        let reading = series(&monitored, case, metric, dimensions.clone()).await;
        assert_eq!(
            reading.datapoints, 0,
            "case {case}: expected no {metric} under {dimensions} while nothing of that kind \
             has happened, got {reading:?}"
        );
    }

    // A read of everything the armed stream holds publishes what it served, stream-level and
    // \u2014 since the enable named it \u2014 per shard.
    let case = "after the read";
    let iterator = monitored
        .kinesis(
            case,
            "GetShardIterator",
            &json!({
                "StreamName": armed,
                "ShardId": shard,
                "ShardIteratorType": "TRIM_HORIZON",
            }),
        )
        .await["ShardIterator"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: GetShardIterator reported no iterator"))
        .to_string();
    let page = monitored
        .kinesis(case, "GetRecords", &json!({ "ShardIterator": iterator }))
        .await;
    let served = page["Records"].as_array().map(Vec::len).unwrap_or_default();
    assert_eq!(
        served, 9,
        "case {case}: expected the read to serve every record written, got {page}"
    );

    for (metric, dimensions, expected) in [
        ("OutgoingRecords", stream_level(armed), 9.0),
        ("OutgoingRecords", shard_level(armed, &shard), 9.0),
        ("OutgoingBytes", stream_level(armed), 9.0 * record_bytes),
        ("GetRecords.Records", stream_level(armed), 9.0),
        ("GetRecords.Bytes", stream_level(armed), 9.0 * record_bytes),
        ("GetRecords.Success", stream_level(armed), 1.0),
    ] {
        let reading = series(&monitored, case, metric, dimensions).await;
        assert!(
            (reading.sum - expected).abs() < TOLERANCE,
            "case {case}: expected {metric} to sum to {expected} after the read, got \
             {reading:?}"
        );
    }

    // The PUSH reader retrieves records from the same shard, so it counts into the same
    // retrieval series beside its own per-API family: the rule is about records leaving the
    // shard, not about which reader asked for them.
    let case = "after the subscription";
    let consumer = monitored
        .kinesis(
            case,
            "RegisterStreamConsumer",
            &json!({ "StreamARN": stream_arn(REGION, armed), "ConsumerName": "metrics-reader" }),
        )
        .await["Consumer"]["ConsumerARN"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: a registration answers a ConsumerARN"))
        .to_string();
    poll_until(case, "the consumer reports ACTIVE", || async {
        let (status, response) = monitored
            .call(
                "kinesis",
                "Kinesis_20131202.DescribeStreamConsumer",
                &json!({ "ConsumerARN": consumer }),
            )
            .await;
        status == 200
            && response["ConsumerDescription"]["ConsumerStatus"].as_str() == Some("ACTIVE")
    })
    .await;
    let (status, _) = monitored
        .call(
            "kinesis",
            "Kinesis_20131202.SubscribeToShard",
            &json!({
                "ConsumerARN": consumer,
                "ShardId": shard,
                "StartingPosition": { "Type": "TRIM_HORIZON" },
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case}: expected the subscription to be established, got {status}"
    );

    for (metric, dimensions, expected) in [
        // The nine records left the shard twice: once to the pull reader, once to this
        // subscription.
        ("OutgoingRecords", stream_level(armed), 18.0),
        ("OutgoingRecords", shard_level(armed, &shard), 18.0),
        ("SubscribeToShardEvent.Records", stream_level(armed), 9.0),
        (
            "SubscribeToShardEvent.Bytes",
            stream_level(armed),
            9.0 * record_bytes,
        ),
        ("SubscribeToShard.Success", stream_level(armed), 1.0),
    ] {
        let reading = series(&monitored, case, metric, dimensions).await;
        assert!(
            (reading.sum - expected).abs() < TOLERANCE,
            "case {case}: expected {metric} to sum to {expected} after the subscription, got \
             {reading:?}"
        );
    }
}
