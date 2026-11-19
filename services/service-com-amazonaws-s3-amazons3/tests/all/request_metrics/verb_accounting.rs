// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the ACCOUNTING a bucket's request metrics must add up to, asserted
//! where a client reads it: CloudWatch's own `GetMetricStatistics` over the `AWS/S3` series of
//! one metrics configuration, with S3 and CloudWatch mounted on one shared engine the way
//! `core/router` assembles them.
//!
//! RULE under test: each request a bucket's metrics configuration counts contributes exactly
//! ONE observation to `AllRequests`, to the counter of its own request type and to each error
//! counter ("with a value of either 0 or 1"), and ONE observation to each per-request latency
//! S3 publishes — `FirstByteLatency` and `TotalRequestLatency` (Amazon S3 User Guide, *Amazon
//! S3 CloudWatch request metrics*). So over whatever traffic a client drives, the per-type
//! counters sum to `AllRequests`, every series the configuration publishes holds exactly that
//! many samples — the latencies included, which is what makes the counters and the latencies
//! of one window reconcilable against the requests really made — a request type nobody made
//! has no series at all, and the byte counters account only for the bodies the counted
//! requests carried.
//!
//! The table below spans what the rule generalises over: the verb mix, how MANY requests of
//! each verb the client makes (not one each), the body sizes written, how many answers are
//! refused, and whether the configuration counting them is filtered. The recorded case — a
//! prefix-filtered configuration under a burst of writes of distinct sizes, repeated reads,
//! metadata reads, a delete and reads of keys that are not there — is ONE row.
//!
//! Declared by [`super`] rather than by `tests/all.rs`, which declares the suites directly
//! under `tests/all/`: a file deeper than that is declared by its own parent module, whose
//! call driver, metric read and window this suite shares.

use serde_json::Value;
// The metric names and units under test, read from this service's own publication rule so the
// suite and the publisher cannot disagree about which series is meant.
use service_com_amazonaws_s3_amazons3::service::request_metrics::{
    ALL_REQUESTS, BYTES_UPLOADED, DELETE_REQUESTS, FIRST_BYTE_LATENCY, FOUR_XX_ERRORS,
    GET_REQUESTS, HEAD_REQUESTS, LIST_REQUESTS, MILLISECONDS_UNIT, POST_REQUESTS, PUT_REQUESTS,
    TOTAL_REQUEST_LATENCY,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::bucket_lifecycle::create_bucket;
use crate::bucket_metrics_configuration::{put_metrics, Filter};
// The harness of the suite this module belongs to: the emulator both services are mounted on,
// the window a metric read spans, the read itself, the fold over its datapoints and the one
// driver that puts a row's calls on S3's wire — one harness for both halves of the same
// publication rule.
use super::{datapoints, drive, emulator, now_secs, total_of, Body, Call, WINDOW_SECS};

/// Every per-type counter a counted request can be counted into: the series `RequestKind`
/// names in this service's own publication rule
/// (`service::request_metrics::RequestKind::counter`), so a verb a row does not drive is
/// asserted to publish nothing at all rather than being left unexamined.
const VERB_COUNTERS: [&str; 6] = [
    GET_REQUESTS,
    PUT_REQUESTS,
    HEAD_REQUESTS,
    DELETE_REQUESTS,
    LIST_REQUESTS,
    POST_REQUESTS,
];

/// The id of the one metrics configuration each row stores.
const FILTER_ID: &str = "verbaccounting";

/// One row: the configuration the bucket carries, the requests the client then makes, how many
/// of them each per-type counter must report, how many were refused with a 4xx, and the bytes
/// the counted requests carried in a request body (`None` where none carried one).
struct Row {
    case: &'static str,
    bucket: &'static str,
    filter: Option<Filter>,
    calls: Vec<Call>,
    verbs: Vec<(&'static str, i64)>,
    refused: i64,
    uploaded: Option<i64>,
}

/// The table.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a mixed burst of verbs under a prefix filter, each verb repeated",
            bucket: "verb-accounting-prefix",
            filter: Some(Filter::Prefix("obs/".to_string())),
            calls: vec![
                Call::Put {
                    key: "obs/small",
                    body: Body::Sized(7),
                    tags: "",
                },
                Call::Put {
                    key: "obs/medium",
                    body: Body::Sized(1_024),
                    tags: "",
                },
                Call::Put {
                    key: "obs/large",
                    body: Body::Sized(3_001),
                    tags: "",
                },
                Call::Get {
                    key: "obs/small",
                    status: 200,
                },
                Call::Get {
                    key: "obs/medium",
                    status: 200,
                },
                Call::Get {
                    key: "obs/large",
                    status: 200,
                },
                Call::Get {
                    key: "obs/small",
                    status: 200,
                },
                Call::Get {
                    key: "obs/medium",
                    status: 200,
                },
                Call::Head {
                    key: "obs/small",
                    status: 200,
                },
                Call::Head {
                    key: "obs/large",
                    status: 200,
                },
                Call::Get {
                    key: "obs/absent-one",
                    status: 404,
                },
                Call::Get {
                    key: "obs/absent-two",
                    status: 404,
                },
                Call::Delete { key: "obs/large" },
            ],
            verbs: vec![
                (PUT_REQUESTS, 3),
                (GET_REQUESTS, 7),
                (HEAD_REQUESTS, 2),
                (DELETE_REQUESTS, 1),
            ],
            refused: 2,
            uploaded: Some(4_032),
        },
        Row {
            case: "an unfiltered configuration counts the listings and the metadata reads too",
            bucket: "verb-accounting-whole",
            filter: None,
            calls: vec![
                Call::Put {
                    key: "first",
                    body: Body::Sized(512),
                    tags: "",
                },
                Call::Head {
                    key: "first",
                    status: 200,
                },
                Call::Head {
                    key: "absent",
                    status: 404,
                },
                Call::List,
                Call::List,
                Call::Delete { key: "first" },
                Call::Get {
                    key: "first",
                    status: 404,
                },
            ],
            verbs: vec![
                (PUT_REQUESTS, 1),
                (GET_REQUESTS, 1),
                (HEAD_REQUESTS, 2),
                (DELETE_REQUESTS, 1),
                (LIST_REQUESTS, 2),
            ],
            refused: 2,
            uploaded: Some(512),
        },
        Row {
            case: "a burst of refusals is a burst of counted requests",
            bucket: "verb-accounting-refused",
            filter: Some(Filter::Prefix("gone/".to_string())),
            calls: vec![
                Call::Get {
                    key: "gone/one",
                    status: 404,
                },
                Call::Get {
                    key: "gone/two",
                    status: 404,
                },
                Call::Get {
                    key: "gone/three",
                    status: 404,
                },
                Call::Head {
                    key: "gone/four",
                    status: 404,
                },
            ],
            verbs: vec![(GET_REQUESTS, 3), (HEAD_REQUESTS, 1)],
            refused: 4,
            uploaded: None,
        },
        Row {
            case: "a prefix filter counts none of the traffic outside it",
            bucket: "verb-accounting-outside",
            filter: Some(Filter::Prefix("inside/".to_string())),
            calls: vec![
                Call::Put {
                    key: "inside/one",
                    body: Body::Sized(64),
                    tags: "",
                },
                Call::Get {
                    key: "inside/one",
                    status: 200,
                },
                Call::Put {
                    key: "outside/one",
                    body: Body::Sized(2_048),
                    tags: "",
                },
                Call::Get {
                    key: "outside/one",
                    status: 200,
                },
                Call::Get {
                    key: "outside/absent",
                    status: 404,
                },
                Call::Head {
                    key: "outside/one",
                    status: 200,
                },
                Call::List,
            ],
            verbs: vec![(PUT_REQUESTS, 1), (GET_REQUESTS, 1)],
            refused: 0,
            uploaded: Some(64),
        },
    ]
}

/// The whole count one statistic of a span reports.
///
/// Every series read here carries a whole number — of requests, or of bytes — and a total of
/// them round-trips through the answer's JSON exactly, so the comparison a row makes is
/// between integers rather than within a tolerance.
#[allow(clippy::cast_possible_truncation)]
fn count_of(points: &[Value], statistic: &str) -> i64 {
    total_of(points, statistic).round() as i64
}

/// One configuration's series of `metric` over the whole window around a row's requests.
async fn series(
    router: &MultiServiceRouter,
    bucket: &str,
    metric: &str,
    window: (f64, f64),
) -> Vec<Value> {
    datapoints(router, bucket, FILTER_ID, metric, window.0, window.1).await
}

/// RULE: over any traffic, a configuration's per-type counters sum to `AllRequests`, each
/// series it publishes holds one sample per counted request — the latencies included — and a
/// series nothing was counted into does not exist.
#[tokio::test]
async fn every_counted_request_is_one_sample_of_every_series_the_configuration_publishes() {
    for row in rows() {
        let case = row.case;
        let bucket = row.bucket;
        let router = emulator();
        let opened = now_secs();

        let created = create_bucket(&router, bucket, None, None).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the bucket must be created first, got {}",
            created.status
        );
        let put = put_metrics(&router, bucket, FILTER_ID, row.filter.as_ref()).await;
        assert_eq!(
            put.status, 204,
            "case {case:?}: the configuration must be stored, got {} with body {:?}",
            put.status,
            String::from_utf8_lossy(&put.body)
        );
        for call in &row.calls {
            drive(&router, bucket, call).await;
        }

        let window = (opened - WINDOW_SECS, now_secs() + WINDOW_SECS);
        let counted: i64 = row.verbs.iter().map(|(_, requests)| requests).sum();

        // Each per-type counter reports the requests of its own type and nothing else, and a
        // type nobody made publishes no series at all.
        for counter in VERB_COUNTERS {
            let expected = row
                .verbs
                .iter()
                .find(|(metric, _)| *metric == counter)
                .map(|(_, requests)| *requests);
            let points = series(&router, bucket, counter, window).await;
            match expected {
                None => assert!(
                    points.is_empty(),
                    "case {case:?}: no request of that type was made, so {counter} must have no \
                     datapoint at all, got {points:?}"
                ),
                Some(requests) => {
                    let sum = count_of(&points, "Sum");
                    assert_eq!(
                        sum, requests,
                        "case {case:?}: {counter} must report {requests} request(s), got {sum} \
                         from {points:?}"
                    );
                    let samples = count_of(&points, "SampleCount");
                    assert_eq!(
                        samples, requests,
                        "case {case:?}: {counter} must hold one sample per request of its own \
                         type ({requests}), got {samples} from {points:?}"
                    );
                }
            }
        }

        // `AllRequests` counts every one of them, once: it is the sum of the per-type counters.
        let all = series(&router, bucket, ALL_REQUESTS, window).await;
        let total = count_of(&all, "Sum");
        assert_eq!(
            total, counted,
            "case {case:?}: {ALL_REQUESTS} must count the {counted} counted request(s), the sum \
             of the per-type counters, got {total} from {all:?}"
        );

        // An error counter observes EVERY counted request, with 1 for the ones of its class and
        // 0 for the rest: so its sample count is the traffic and its sum is the refusals.
        let errors = series(&router, bucket, FOUR_XX_ERRORS, window).await;
        let refused = count_of(&errors, "Sum");
        assert_eq!(
            refused, row.refused,
            "case {case:?}: {FOUR_XX_ERRORS} must report {} refused request(s), got {refused} \
             from {errors:?}",
            row.refused
        );
        let error_samples = count_of(&errors, "SampleCount");
        assert_eq!(
            error_samples, counted,
            "case {case:?}: {FOUR_XX_ERRORS} observes every counted request ({counted}), got \
             {error_samples} sample(s) from {errors:?}"
        );

        // Each latency carries one sample per counted request as well, in the unit it is
        // published in, and every observation of it is a measured span: a finite,
        // non-negative number of milliseconds.
        for latency in [FIRST_BYTE_LATENCY, TOTAL_REQUEST_LATENCY] {
            let points = series(&router, bucket, latency, window).await;
            assert!(
                !points.is_empty(),
                "case {case:?}: {latency} must carry a datapoint for the {counted} counted \
                 request(s), got none"
            );
            let samples = count_of(&points, "SampleCount");
            assert_eq!(
                samples, counted,
                "case {case:?}: {latency} must hold one observation per counted request \
                 ({counted}), got {samples} from {points:?}"
            );
            for point in &points {
                assert_eq!(
                    point.get("Unit").and_then(Value::as_str),
                    Some(MILLISECONDS_UNIT),
                    "case {case:?}: {latency} must be published in {MILLISECONDS_UNIT}, got \
                     {point:?}"
                );
                let sum = point.get("Sum").and_then(Value::as_f64);
                assert!(
                    sum.is_some_and(|sum| sum.is_finite() && sum >= 0.0),
                    "case {case:?}: every observation of {latency} is a measured span, so the \
                     total of a period is a finite, non-negative number of milliseconds, got \
                     {sum:?} in {point:?}"
                );
            }
        }

        // "The elapsed per-request time from the first byte received to the last byte sent"
        // includes the span to the first byte, so over the same requests the whole-request
        // total cannot be the smaller of the two.
        let to_first_byte = total_of(&series(&router, bucket, FIRST_BYTE_LATENCY, window).await, "Sum");
        let whole = total_of(&series(&router, bucket, TOTAL_REQUEST_LATENCY, window).await, "Sum");
        assert!(
            to_first_byte <= whole,
            "case {case:?}: {FIRST_BYTE_LATENCY} ({to_first_byte} ms) cannot exceed \
             {TOTAL_REQUEST_LATENCY} ({whole} ms) over the same requests"
        );

        // The byte counter accounts for the bodies the counted requests really carried, and
        // where none carried one it has no series.
        let uploaded = series(&router, bucket, BYTES_UPLOADED, window).await;
        match row.uploaded {
            None => assert!(
                uploaded.is_empty(),
                "case {case:?}: no counted request carried a request body, so {BYTES_UPLOADED} \
                 must have no datapoint at all, got {uploaded:?}"
            ),
            Some(bytes) => {
                let actual = count_of(&uploaded, "Sum");
                assert_eq!(
                    actual, bytes,
                    "case {case:?}: {BYTES_UPLOADED} must account for {bytes} byte(s), got \
                     {actual} from {uploaded:?}"
                );
            }
        }
    }
}
