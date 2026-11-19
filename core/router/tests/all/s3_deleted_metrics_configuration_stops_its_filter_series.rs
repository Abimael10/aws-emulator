// SPDX-License-Identifier: CC0-1.0

//! The LIFECYCLE of a bucket's CloudWatch request-metrics configuration, read where a client
//! reads it: `AWS/S3` series dimensioned on `[BucketName, FilterId]`.
//!
//! RULE under test: a bucket publishes an `AWS/S3` request-metric series for a metrics
//! configuration only for as long as that configuration is STORED on the bucket —
//! `DeleteBucketMetricsConfiguration` "deletes a metrics configuration for the Amazon
//! CloudWatch request metrics (specified by the metrics configuration ID) from the bucket", so
//! a request served after the delete contributes no observation to that configuration's
//! `FilterId` series, every configuration still stored whose `Filter` selects the request keeps
//! gaining observations, and a configuration put back under the same id publishes again. The
//! datapoints the deleted configuration already published are not withdrawn: they stay
//! readable through `GetMetricStatistics` and, having reported data, the series stays listed by
//! `ListMetrics` (which "doesn't return information about metrics if those metrics haven't
//! reported data in the past two weeks").
//!
//! Two crates have to agree for that (Amazon S3 decides which configurations one served
//! request falls under and publishes the observations, CloudWatch owns the metric store those
//! series live in, answers statistics reads over them and indexes them for discovery), so the
//! suite lives here, on both services mounted over ONE engine the way the production router
//! assembles them.
//!
//! The rows differ along what the rule generalises over: the FILTER FORM of the configuration
//! that is deleted (unfiltered, a prefix, an object tag, a conjunction), WHICH member of the
//! bucket's set is deleted (the unfiltered one, a filtered one, the only one), how many
//! configurations survive it (two, one, none), and which metric of the series is read (the
//! all-requests counter, the per-verb counters, and the error counter whose observations are
//! zero-valued). The recorded case — an unfiltered `EntireBucket` beside a prefix-filtered
//! configuration, the filtered one deleted while traffic keeps running under both prefixes —
//! is ONE row.
//!
//! Nothing here waits on the clock. Each row drives the SAME round of traffic three times and
//! asserts, after every round, that each still-stored series has gained exactly one round's
//! observations while the deleted one is frozen at the total it had when it was removed: the
//! "quiet window" of the lifecycle signal is pinned by those unchanged totals over two further
//! rounds of identical traffic rather than by any elapsed time.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// One S3 wire harness for this crate: the bucket is created and every request is dispatched
// path-style through the same helpers the notification suites drive S3 with, so this suite
// cannot reach the service a way a client cannot.
use crate::s3_notification_invokes_its_lambda_target::{authorization, create_bucket, s3};

/// The Region the shared S3 harness above signs its requests for, which the CloudWatch calls
/// below are addressed and signed for too.
const REGION: &str = "us-east-1";

/// The `X-Amz-Target` prefix the CloudWatch model declares.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";

/// The XML namespace a client's S3 payload carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The namespace, the dimension names and the metric names this suite reads — spelled out here
/// rather than imported from the publisher, so a wrong constant in the implementation cannot
/// make the suite pass. Their source is the Amazon S3 User Guide (*Amazon S3 CloudWatch request
/// metrics*, and *Metrics and dimensions* for the dimension pair).
const NAMESPACE: &str = "AWS/S3";
const BUCKET_NAME_DIMENSION: &str = "BucketName";
const FILTER_ID_DIMENSION: &str = "FilterId";
const ALL_REQUESTS: &str = "AllRequests";
const GET_REQUESTS: &str = "GetRequests";
const PUT_REQUESTS: &str = "PutRequests";
const DELETE_REQUESTS: &str = "DeleteRequests";
const LIST_REQUESTS: &str = "ListRequests";
const FOUR_XX_ERRORS: &str = "4xxErrors";

/// The status `PutBucketMetricsConfiguration` and `DeleteBucketMetricsConfiguration` answer a
/// success with, and the code and status a configuration that is not there is reported with.
const CONFIGURATION_WRITTEN_STATUS: u16 = 204;
const NO_SUCH_CONFIGURATION_CODE: &str = "NoSuchConfiguration";
const NO_SUCH_CONFIGURATION_STATUS: u16 = 404;

/// How far either side of now a statistics read spans, and the period it folds datapoints into:
/// a span wide enough to hold every datapoint the suite publishes whichever minute the clock is
/// in, folded at the one-minute granularity S3 publishes request metrics at.
const WINDOW_SECS: f64 = 1_800.0;
const PERIOD_SECS: i64 = 60;

/// How many rounds of a row's traffic are driven in all, and after how many of them the delete
/// is made: one round with every configuration stored, then two more with it gone — so what is
/// asserted about the deleted series is that it stayed frozen across repeated traffic.
const ROUNDS: usize = 3;
const ROUNDS_BEFORE_THE_DELETE: usize = 1;

/// Floating-point counts of requests compare to the integers they are, with room for the
/// representation only.
const TOLERANCE: f64 = 1e-9;

/// One `MetricsFilter`, in each of the forms the union declares: the whole bucket (no filter at
/// all), a key prefix, one object tag, and the conjunction of a prefix with a tag.
#[derive(Debug, Clone, Copy)]
enum Filter {
    Whole,
    Prefix(&'static str),
    Tag(&'static str, &'static str),
    And(&'static str, &'static str, &'static str),
}

impl Filter {
    /// The `MetricsConfiguration` payload for `id` under this filter, as a client's XML writer
    /// spells it.
    fn payload(self, id: &str) -> String {
        let filter = match self {
            Self::Whole => String::new(),
            Self::Prefix(prefix) => format!("<Filter><Prefix>{prefix}</Prefix></Filter>"),
            Self::Tag(key, value) => {
                format!("<Filter>{}</Filter>", tag_xml(key, value))
            }
            Self::And(prefix, key, value) => format!(
                "<Filter><And><Prefix>{prefix}</Prefix>{}</And></Filter>",
                tag_xml(key, value)
            ),
        };
        format!(
            "<MetricsConfiguration xmlns=\"{S3_XMLNS}\"><Id>{id}</Id>{filter}\
             </MetricsConfiguration>"
        )
    }
}

/// One `Tag` element, as `Tag$Key` and `Tag$Value` spell the pair.
fn tag_xml(key: &str, value: &str) -> String {
    format!("<Tag><Key>{key}</Key><Value>{value}</Value></Tag>")
}

/// An object a row puts in place BEFORE its baseline is read, with the tag set it carries: the
/// requests that create it are counted like any other, so taking the baseline after them keeps
/// a row's arithmetic about the rounds of traffic alone.
#[derive(Debug)]
struct Seed {
    key: &'static str,
    body: &'static str,
    tag: Option<(&'static str, &'static str)>,
}

/// One request of a row's round of traffic, and the status it must be answered with.
#[derive(Debug)]
enum Call {
    Get { key: &'static str, status: u16 },
    Put { key: &'static str, body: &'static str },
    Delete { key: &'static str },
    List,
}

/// What ONE round of a row's traffic contributes to one series while its configuration is
/// stored: how many observations it appends, and what those observations sum to.
#[derive(Debug)]
struct Expect {
    filter_id: &'static str,
    metric: &'static str,
    observations: f64,
    sum: f64,
}

/// One row: the configurations the bucket carries, the objects seeded before the baseline, the
/// round of traffic driven repeatedly, which configuration is deleted after the first round,
/// and what each series must report per round.
struct Row {
    case: &'static str,
    bucket: &'static str,
    configurations: &'static [(&'static str, Filter)],
    seeds: &'static [Seed],
    traffic: &'static [Call],
    deleted: &'static str,
    expect: &'static [Expect],
}

const ROWS: &[Row] = &[
    Row {
        case: "the recorded shape: an unfiltered configuration beside a prefix-filtered one, \
               the filtered one deleted",
        bucket: "metrics-config-delete-recorded",
        configurations: &[
            ("EntireBucket", Filter::Whole),
            ("ImportantPrefix", Filter::Prefix("important/")),
        ],
        seeds: &[],
        traffic: &[
            Call::Put {
                key: "important/report.csv",
                body: "report",
            },
            Call::Get {
                key: "important/report.csv",
                status: 200,
            },
            Call::Get {
                key: "other/report.csv",
                status: 404,
            },
            Call::List,
        ],
        deleted: "ImportantPrefix",
        expect: &[
            Expect {
                filter_id: "EntireBucket",
                metric: ALL_REQUESTS,
                observations: 4.0,
                sum: 4.0,
            },
            Expect {
                filter_id: "EntireBucket",
                metric: GET_REQUESTS,
                observations: 2.0,
                sum: 2.0,
            },
            Expect {
                filter_id: "EntireBucket",
                metric: PUT_REQUESTS,
                observations: 1.0,
                sum: 1.0,
            },
            Expect {
                filter_id: "EntireBucket",
                metric: LIST_REQUESTS,
                observations: 1.0,
                sum: 1.0,
            },
            // One observation per counted request, "with a value of either 0 or 1": the one
            // read of a key that is not there is the only 4xx of the round.
            Expect {
                filter_id: "EntireBucket",
                metric: FOUR_XX_ERRORS,
                observations: 4.0,
                sum: 1.0,
            },
            Expect {
                filter_id: "ImportantPrefix",
                metric: ALL_REQUESTS,
                observations: 2.0,
                sum: 2.0,
            },
            Expect {
                filter_id: "ImportantPrefix",
                metric: GET_REQUESTS,
                observations: 1.0,
                sum: 1.0,
            },
            Expect {
                filter_id: "ImportantPrefix",
                metric: PUT_REQUESTS,
                observations: 1.0,
                sum: 1.0,
            },
            // A listing addresses no key, so it falls under no prefix-filtered configuration:
            // this series never gains a datapoint, before the delete or after it.
            Expect {
                filter_id: "ImportantPrefix",
                metric: LIST_REQUESTS,
                observations: 0.0,
                sum: 0.0,
            },
            Expect {
                filter_id: "ImportantPrefix",
                metric: FOUR_XX_ERRORS,
                observations: 2.0,
                sum: 0.0,
            },
        ],
    },
    Row {
        case: "the UNFILTERED configuration is the one deleted, and its prefix-filtered sibling \
               keeps publishing",
        bucket: "metrics-config-delete-unfiltered",
        configurations: &[
            ("WholeBucket", Filter::Whole),
            ("LogsPrefix", Filter::Prefix("logs/")),
        ],
        seeds: &[],
        traffic: &[
            Call::Put {
                key: "logs/app.log",
                body: "one line",
            },
            Call::Get {
                key: "logs/app.log",
                status: 200,
            },
            Call::Delete { key: "logs/app.log" },
        ],
        deleted: "WholeBucket",
        expect: &[
            Expect {
                filter_id: "WholeBucket",
                metric: ALL_REQUESTS,
                observations: 3.0,
                sum: 3.0,
            },
            Expect {
                filter_id: "WholeBucket",
                metric: DELETE_REQUESTS,
                observations: 1.0,
                sum: 1.0,
            },
            Expect {
                filter_id: "LogsPrefix",
                metric: ALL_REQUESTS,
                observations: 3.0,
                sum: 3.0,
            },
            Expect {
                filter_id: "LogsPrefix",
                metric: GET_REQUESTS,
                observations: 1.0,
                sum: 1.0,
            },
            Expect {
                filter_id: "LogsPrefix",
                metric: DELETE_REQUESTS,
                observations: 1.0,
                sum: 1.0,
            },
        ],
    },
    Row {
        case: "a tag-filtered configuration deleted while TWO siblings keep publishing",
        bucket: "metrics-config-delete-tagged",
        configurations: &[
            ("AllOfIt", Filter::Whole),
            ("CriticalTag", Filter::Tag("class", "critical")),
            ("ReportsPrefix", Filter::Prefix("reports/")),
        ],
        seeds: &[
            Seed {
                key: "reports/metrics.csv",
                body: "rows",
                tag: Some(("class", "critical")),
            },
            Seed {
                key: "other/notes.txt",
                body: "notes",
                tag: None,
            },
        ],
        traffic: &[
            Call::Get {
                key: "reports/metrics.csv",
                status: 200,
            },
            Call::Get {
                key: "other/notes.txt",
                status: 200,
            },
        ],
        deleted: "CriticalTag",
        expect: &[
            Expect {
                filter_id: "AllOfIt",
                metric: ALL_REQUESTS,
                observations: 2.0,
                sum: 2.0,
            },
            Expect {
                filter_id: "AllOfIt",
                metric: FOUR_XX_ERRORS,
                observations: 2.0,
                sum: 0.0,
            },
            Expect {
                filter_id: "CriticalTag",
                metric: ALL_REQUESTS,
                observations: 1.0,
                sum: 1.0,
            },
            Expect {
                filter_id: "CriticalTag",
                metric: GET_REQUESTS,
                observations: 1.0,
                sum: 1.0,
            },
            Expect {
                filter_id: "ReportsPrefix",
                metric: ALL_REQUESTS,
                observations: 1.0,
                sum: 1.0,
            },
            Expect {
                filter_id: "ReportsPrefix",
                metric: GET_REQUESTS,
                observations: 1.0,
                sum: 1.0,
            },
        ],
    },
    Row {
        case: "a conjunction-filtered configuration deleted beside an unfiltered sibling",
        bucket: "metrics-config-delete-conjunction",
        configurations: &[
            ("Whole", Filter::Whole),
            ("CriticalReports", Filter::And("reports/", "class", "critical")),
        ],
        seeds: &[
            Seed {
                key: "reports/critical.csv",
                body: "a",
                tag: Some(("class", "critical")),
            },
            Seed {
                key: "reports/plain.csv",
                body: "b",
                tag: None,
            },
        ],
        traffic: &[
            Call::Get {
                key: "reports/critical.csv",
                status: 200,
            },
            Call::Get {
                key: "reports/plain.csv",
                status: 200,
            },
            Call::Get {
                key: "reports/missing.csv",
                status: 404,
            },
        ],
        deleted: "CriticalReports",
        expect: &[
            Expect {
                filter_id: "Whole",
                metric: ALL_REQUESTS,
                observations: 3.0,
                sum: 3.0,
            },
            Expect {
                filter_id: "Whole",
                metric: FOUR_XX_ERRORS,
                observations: 3.0,
                sum: 1.0,
            },
            // Only the read of the object that is under the prefix AND carries the tag falls
            // under the conjunction.
            Expect {
                filter_id: "CriticalReports",
                metric: ALL_REQUESTS,
                observations: 1.0,
                sum: 1.0,
            },
            Expect {
                filter_id: "CriticalReports",
                metric: GET_REQUESTS,
                observations: 1.0,
                sum: 1.0,
            },
        ],
    },
    Row {
        case: "the bucket's ONLY configuration is deleted, so the bucket publishes nothing \
               further at all",
        bucket: "metrics-config-delete-only",
        configurations: &[("OnlyOne", Filter::Whole)],
        seeds: &[],
        traffic: &[
            Call::Put {
                key: "data/one",
                body: "x",
            },
            Call::Get {
                key: "data/one",
                status: 200,
            },
            Call::List,
        ],
        deleted: "OnlyOne",
        expect: &[
            Expect {
                filter_id: "OnlyOne",
                metric: ALL_REQUESTS,
                observations: 3.0,
                sum: 3.0,
            },
            Expect {
                filter_id: "OnlyOne",
                metric: GET_REQUESTS,
                observations: 1.0,
                sum: 1.0,
            },
            Expect {
                filter_id: "OnlyOne",
                metric: PUT_REQUESTS,
                observations: 1.0,
                sum: 1.0,
            },
            Expect {
                filter_id: "OnlyOne",
                metric: LIST_REQUESTS,
                observations: 1.0,
                sum: 1.0,
            },
        ],
    },
];

/// The emulator under test: Amazon S3 and CloudWatch over ONE shared engine, exactly as the
/// production router mounts them, which is what makes the store the served requests publish
/// into the store CloudWatch answers from.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// Wall-clock now as epoch seconds, the clock a metric window is measured against.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// One `awsJson1_0` CloudWatch call that must be answered 200, as the parsed body.
async fn cloudwatch(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let host = format!("monitoring.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host.clone())
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("{CLOUDWATCH_TARGET_PREFIX}.{operation}"),
        )
        .header("authorization", authorization("monitoring"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "unparsed": String::from_utf8_lossy(&payload) }))
    };
    assert_eq!(
        status, 200,
        "{operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// What one series holds over the whole window: what its datapoints sum to, and how many
/// observations they fold.
#[derive(Debug, Clone, Copy, Default)]
struct Series {
    sum: f64,
    observations: f64,
}

/// `GetMetricStatistics` over the series of `metric` dimensioned on `bucket` and `filter_id`,
/// folded over the whole window around the traffic.
async fn series(
    router: &MultiServiceRouter,
    bucket: &str,
    filter_id: &str,
    metric: &str,
) -> Series {
    let now = now_secs();
    let answered = cloudwatch(
        router,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": [
                {"Name": BUCKET_NAME_DIMENSION, "Value": bucket},
                {"Name": FILTER_ID_DIMENSION, "Value": filter_id},
            ],
            "StartTime": now - WINDOW_SECS,
            "EndTime": now + WINDOW_SECS,
            "Period": PERIOD_SECS,
            "Statistics": ["Sum", "SampleCount"],
        }),
    )
    .await;
    let points = answered
        .get("Datapoints")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Series {
        sum: points
            .iter()
            .filter_map(|point| point.get("Sum").and_then(Value::as_f64))
            .sum(),
        observations: points
            .iter()
            .filter_map(|point| point.get("SampleCount").and_then(Value::as_f64))
            .sum(),
    }
}

/// The `FilterId` dimension values `ListMetrics` indexes for `bucket` in the `AWS/S3`
/// namespace: which configurations a client discovering the bucket's series finds.
async fn indexed_filter_ids(router: &MultiServiceRouter, bucket: &str) -> Vec<String> {
    let listed = cloudwatch(
        router,
        "ListMetrics",
        &json!({
            "Namespace": NAMESPACE,
            "Dimensions": [{"Name": BUCKET_NAME_DIMENSION, "Value": bucket}],
        }),
    )
    .await;
    let mut ids: Vec<String> = listed
        .get("Metrics")
        .and_then(Value::as_array)
        .map(|metrics| {
            metrics
                .iter()
                .filter_map(|metric| metric.get("Dimensions").and_then(Value::as_array))
                .flatten()
                .filter(|dimension| {
                    dimension.get("Name").and_then(Value::as_str) == Some(FILTER_ID_DIMENSION)
                })
                .filter_map(|dimension| dimension.get("Value").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids.dedup();
    ids
}

/// `PUT /<bucket>?metrics&id=<id>` — `PutBucketMetricsConfiguration`.
async fn put_configuration(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    id: &str,
    filter: Filter,
) {
    let answer = s3(
        router,
        "PUT",
        &format!("/{bucket}?metrics&id={id}"),
        &filter.payload(id),
    )
    .await;
    assert_eq!(
        answer.status, CONFIGURATION_WRITTEN_STATUS,
        "case {case:?}: PutBucketMetricsConfiguration({bucket:?}, {id:?}) must answer \
         {CONFIGURATION_WRITTEN_STATUS}, got {} with {}",
        answer.status, answer.body
    );
}

/// `DELETE /<bucket>?metrics&id=<id>` — `DeleteBucketMetricsConfiguration`.
async fn delete_configuration(router: &MultiServiceRouter, case: &str, bucket: &str, id: &str) {
    let answer = s3(router, "DELETE", &format!("/{bucket}?metrics&id={id}"), "").await;
    assert_eq!(
        answer.status, CONFIGURATION_WRITTEN_STATUS,
        "case {case:?}: DeleteBucketMetricsConfiguration({bucket:?}, {id:?}) must answer \
         {CONFIGURATION_WRITTEN_STATUS}, got {} with {}",
        answer.status, answer.body
    );
}

/// The control plane's own view of a configuration that was deleted: `GetBucketMetricsConfiguration`
/// reports it is not there, which is what makes the quiet series below a REMOVED configuration
/// rather than one that merely stopped being selected.
async fn assert_configuration_gone(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    id: &str,
) {
    let answer = s3(router, "GET", &format!("/{bucket}?metrics&id={id}"), "").await;
    assert_eq!(
        answer.status, NO_SUCH_CONFIGURATION_STATUS,
        "case {case:?}: GetBucketMetricsConfiguration({bucket:?}, {id:?}) must answer \
         {NO_SUCH_CONFIGURATION_STATUS} once the configuration is deleted, got {} with {}",
        answer.status, answer.body
    );
    assert!(
        answer.body.contains(NO_SUCH_CONFIGURATION_CODE),
        "case {case:?}: that refusal must carry {NO_SUCH_CONFIGURATION_CODE}, got {}",
        answer.body
    );
}

/// Put one seeded object in place, with the tag set it carries.
async fn seed_object(router: &MultiServiceRouter, case: &str, bucket: &str, seed: &Seed) {
    let written = s3(router, "PUT", &format!("/{bucket}/{}", seed.key), seed.body).await;
    assert_eq!(
        written.status, 200,
        "case {case:?}: PutObject({bucket:?}, {:?}) must succeed, got {} with {}",
        seed.key, written.status, written.body
    );
    if let Some((key, value)) = seed.tag {
        let payload = format!(
            "<Tagging xmlns=\"{S3_XMLNS}\"><TagSet>{}</TagSet></Tagging>",
            tag_xml(key, value)
        );
        let tagged = s3(
            router,
            "PUT",
            &format!("/{bucket}/{}?tagging", seed.key),
            &payload,
        )
        .await;
        assert_eq!(
            tagged.status, 200,
            "case {case:?}: PutObjectTagging({bucket:?}, {:?}) must succeed, got {} with {}",
            seed.key, tagged.status, tagged.body
        );
    }
}

/// Drive one round of a row's traffic through S3's own wire, requiring each answer's status.
async fn drive_round(router: &MultiServiceRouter, case: &str, bucket: &str, traffic: &[Call]) {
    for call in traffic {
        let (method, path, body, expected) = match call {
            Call::Get { key, status } => ("GET", format!("/{bucket}/{key}"), "", *status),
            Call::Put { key, body } => ("PUT", format!("/{bucket}/{key}"), *body, 200),
            // "Unit" output: a delete of an object answers 204 with no body.
            Call::Delete { key } => ("DELETE", format!("/{bucket}/{key}"), "", 204),
            Call::List => ("GET", format!("/{bucket}?list-type=2"), "", 200),
        };
        let answer = s3(router, method, &path, body).await;
        assert_eq!(
            answer.status, expected,
            "case {case:?}: the call {call:?} must answer {expected}, got {} with {}",
            answer.status, answer.body
        );
    }
}

/// Every series a row names, read in the order the row names them.
async fn read_series(router: &MultiServiceRouter, row: &Row) -> Vec<Series> {
    let mut read = Vec::with_capacity(row.expect.len());
    for expect in row.expect {
        read.push(series(router, row.bucket, expect.filter_id, expect.metric).await);
    }
    read
}

/// Assert each series a row names holds exactly the baseline plus one round's contribution per
/// round the series' own configuration was STORED for.
async fn assert_totals(
    router: &MultiServiceRouter,
    row: &Row,
    baseline: &[Series],
    stored_rounds: usize,
    driven_rounds: usize,
) {
    let case = row.case;
    for (index, expect) in row.expect.iter().enumerate() {
        let rounds = if expect.filter_id == row.deleted {
            ROUNDS_BEFORE_THE_DELETE
        } else {
            stored_rounds
        };
        let base = baseline[index];
        let want_sum = base.sum + expect.sum * rounds as f64;
        let want_observations = base.observations + expect.observations * rounds as f64;
        let got = series(router, row.bucket, expect.filter_id, expect.metric).await;
        let filter_id = expect.filter_id;
        let metric = expect.metric;
        assert!(
            (got.observations - want_observations).abs() < TOLERANCE,
            "case {case:?}: after {driven_rounds} round(s) of traffic, the {metric} series of \
             FilterId {filter_id:?} must hold {want_observations} observations — one round's \
             worth for each of the {rounds} round(s) its configuration was stored for — got \
             {got:?}"
        );
        assert!(
            (got.sum - want_sum).abs() < TOLERANCE,
            "case {case:?}: after {driven_rounds} round(s) of traffic, the {metric} series of \
             FilterId {filter_id:?} must sum to {want_sum} over the {rounds} round(s) its \
             configuration was stored for, got {got:?}"
        );
    }
}

/// RULE: a served request contributes to a configuration's `[BucketName, FilterId]` series only
/// while that configuration is stored on the bucket — so the deleted configuration's series
/// stops gaining observations, every surviving configuration's keeps gaining them from the same
/// traffic, and what was already published stays readable and indexed.
#[tokio::test]
async fn a_deleted_metrics_configuration_stops_its_series_while_its_siblings_keep_publishing() {
    for row in ROWS {
        let case = row.case;
        let router = emulator();
        create_bucket(&router, case, row.bucket).await;
        for (id, filter) in row.configurations {
            put_configuration(&router, case, row.bucket, id, *filter).await;
        }
        for seed in row.seeds {
            seed_object(&router, case, row.bucket, seed).await;
        }
        // The totals the seeding left behind: every assertion below is about what the ROUNDS
        // add to them.
        let baseline = read_series(&router, row).await;

        for round in 1..=ROUNDS_BEFORE_THE_DELETE {
            drive_round(&router, case, row.bucket, row.traffic).await;
            assert_totals(&router, row, &baseline, round, round).await;
        }

        delete_configuration(&router, case, row.bucket, row.deleted).await;
        assert_configuration_gone(&router, case, row.bucket, row.deleted).await;

        // The same traffic again, twice: the surviving configurations gain a round each time
        // while the deleted one stays exactly where it stopped.
        for round in ROUNDS_BEFORE_THE_DELETE + 1..=ROUNDS {
            drive_round(&router, case, row.bucket, row.traffic).await;
            assert_totals(&router, row, &baseline, round, round).await;
        }

        // The deleted configuration's series reported data minutes ago, so a client discovering
        // the bucket's metrics still finds it beside the surviving ones.
        let mut configured: Vec<String> = row
            .configurations
            .iter()
            .map(|(id, _)| (*id).to_string())
            .collect();
        configured.sort();
        assert_eq!(
            indexed_filter_ids(&router, row.bucket).await,
            configured,
            "case {case:?}: ListMetrics must index every FilterId the bucket published a series \
             for, the deleted {:?} included",
            row.deleted
        );
    }
}

/// One row of the re-put table: the configuration the bucket carries, the filter it is put back
/// under after the delete, and how many of the round's requests each form of it counts.
struct ReputRow {
    case: &'static str,
    bucket: &'static str,
    id: &'static str,
    filter: Filter,
    reput_filter: Filter,
    /// `AllRequests` observations one round contributes under the ORIGINAL filter.
    stored_per_round: f64,
    /// And under the filter it is put back with.
    resumed_per_round: f64,
}

/// Both rows drive the same two reads: one of an object the prefix filter selects, one of an
/// object outside it.
const REPUT_SEEDS: &[Seed] = &[
    Seed {
        key: "watched/one",
        body: "a",
        tag: None,
    },
    Seed {
        key: "outside/two",
        body: "b",
        tag: None,
    },
];
const REPUT_TRAFFIC: &[Call] = &[
    Call::Get {
        key: "watched/one",
        status: 200,
    },
    Call::Get {
        key: "outside/two",
        status: 200,
    },
];

const REPUT_ROWS: &[ReputRow] = &[
    ReputRow {
        case: "put back under the same filter, the series resumes on the same traffic",
        bucket: "metrics-config-reput-same",
        id: "Watched",
        filter: Filter::Prefix("watched/"),
        reput_filter: Filter::Prefix("watched/"),
        stored_per_round: 1.0,
        resumed_per_round: 1.0,
    },
    ReputRow {
        case: "put back under a wider filter, the resumed series counts by the NEW filter",
        bucket: "metrics-config-reput-widened",
        id: "Watched",
        filter: Filter::Prefix("watched/"),
        reput_filter: Filter::Whole,
        stored_per_round: 1.0,
        resumed_per_round: 2.0,
    },
];

/// RULE, the other way round: the series publishes again as soon as a configuration is stored
/// under that id again, counting by the filter it was put back with.
#[tokio::test]
async fn a_metrics_configuration_put_back_publishes_again() {
    for row in REPUT_ROWS {
        let case = row.case;
        let router = emulator();
        create_bucket(&router, case, row.bucket).await;
        put_configuration(&router, case, row.bucket, row.id, row.filter).await;
        for seed in REPUT_SEEDS {
            seed_object(&router, case, row.bucket, seed).await;
        }
        let baseline = series(&router, row.bucket, row.id, ALL_REQUESTS).await;

        drive_round(&router, case, row.bucket, REPUT_TRAFFIC).await;
        let stored = series(&router, row.bucket, row.id, ALL_REQUESTS).await;
        assert!(
            (stored.observations - (baseline.observations + row.stored_per_round)).abs()
                < TOLERANCE,
            "case {case:?}: while the configuration is stored, the {ALL_REQUESTS} series of \
             FilterId {:?} must gain {} observation(s) from one round, got {stored:?} against a \
             baseline of {baseline:?}",
            row.id,
            row.stored_per_round
        );

        delete_configuration(&router, case, row.bucket, row.id).await;
        drive_round(&router, case, row.bucket, REPUT_TRAFFIC).await;
        let quiet = series(&router, row.bucket, row.id, ALL_REQUESTS).await;
        assert!(
            (quiet.observations - stored.observations).abs() < TOLERANCE,
            "case {case:?}: with the configuration deleted, the {ALL_REQUESTS} series of \
             FilterId {:?} must gain nothing from the same round, got {quiet:?} where it held \
             {stored:?}",
            row.id
        );

        put_configuration(&router, case, row.bucket, row.id, row.reput_filter).await;
        for round in 1..=2_usize {
            drive_round(&router, case, row.bucket, REPUT_TRAFFIC).await;
            let resumed = series(&router, row.bucket, row.id, ALL_REQUESTS).await;
            let want = quiet.observations + row.resumed_per_round * round as f64;
            assert!(
                (resumed.observations - want).abs() < TOLERANCE,
                "case {case:?}: after the re-put, round {round} must leave the {ALL_REQUESTS} \
                 series of FilterId {:?} holding {want} observations, got {resumed:?}",
                row.id
            );
        }
    }
}
