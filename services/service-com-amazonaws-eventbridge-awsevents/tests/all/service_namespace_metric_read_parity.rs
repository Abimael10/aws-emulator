// SPDX-License-Identifier: CC0-1.0

//! A custom bus's rule's routing counters are READ back out of CloudWatch on
//! exactly the same terms as the same observations ingested through
//! `PutMetricData`, end to end over the wire (`awsJson1_1`, `POST /` with
//! `X-Amz-Target: AWSEvents.<Operation>`) through the real `MultiServiceRouter`,
//! with Amazon SQS and CloudWatch mounted on the same engine.
//!
//! The RULE asserted here: CloudWatch identifies a metric by its (namespace,
//! metric name, dimension set) triple — "CloudWatch treats each unique combination
//! of dimensions as a separate metric" (`GetMetricStatisticsInput$Dimensions`) —
//! and the read path over a stored series does not depend on which writer appended
//! it, so a series a service appended for its own `AWS/` namespace is aggregated,
//! unit-reported, unit-filtered, enumerated and left absent by exactly the rules a
//! client-published series is. Concretely, for the same metric name, the same
//! dimension set and the same observations: `GetMetricStatistics` folds them into
//! the same period windows and totals, reports the `Unit` they were stored with,
//! serves nothing when the request's `Unit` names another ("If you specify a unit
//! that does not match the data collected, the results of the operation are null",
//! `GetMetricStatisticsInput$Unit`), answers with NO datapoint at all for a
//! dimension set nothing was published under ("If a specific combination of
//! dimensions was not published, you can't retrieve statistics for it",
//! `GetMetricStatisticsInput$Dimensions`), and `ListMetrics` enumerates the same
//! dimension sets, narrowed by a `DimensionFilter` the same way.
//!
//! The comparison is run against a MIRROR series: the same observations the matched
//! rule's counters hold, re-published by a client with `PutMetricData` under the
//! same metric names and the same dimension set in a namespace of its own — which
//! `PutMetricData` is the only way to reach, because CloudWatch reserves the
//! `AWS/` namespaces for the services that own them ("you should not specify a
//! namespace that begins with `AWS/`", `PutMetricDataInput$Namespace`). Every
//! probe below reads BOTH namespaces and asserts the two answers agree, so a read
//! path that serves one writer's datapoints and not the other's fails here.
//!
//! The tables span the class rather than one reading: the rows differ along the BUS
//! the rules live on and HOW MANY matched events the round publishes (so a count is
//! a count rather than a flag); the probes differ along the PERIOD the series is
//! read at, along the `Unit` the read names, along the DIMENSION SET it names, and
//! along the PASS, every probe being taken twice so a read that created, consumed
//! or shifted anything fails the second.
//!
//! The harness (the router with EventBridge, Amazon SQS and CloudWatch on one
//! engine) is `tests/all/target_dispatch.rs`'s.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state::{
    EVENT_BUS_NAME_DIMENSION, MATCHED_EVENTS_METRIC, METRIC_NAMESPACE, METRIC_UNIT_COUNT,
    RULE_NAME_DIMENSION, SERVICE_PRINCIPAL, SOURCE_ACCOUNT_CONDITION_KEY, TRIGGERED_RULES_METRIC,
};

use crate::target_dispatch::{
    bus_dimensions, cloudwatch, drain, emulator, events, given_queue_with_policy, ordered,
    rule_dimensions, rule_name_dimensions, Emulator, DETAIL_TYPE, SOURCE,
};

/// The rule the published events match, and the twin on the same bus that differs
/// from it only in the source it pins.
const MATCHING_RULE: &str = "hit";
const TWIN_RULE: &str = "miss";

/// The source the twin pins, which nothing published below carries.
const OTHER_SOURCE: &str = "com.demo.nomatch";

/// The two counters a pattern match is accounted on: the series this suite reads.
const ROUTING_METRICS: [&str; 2] = [MATCHED_EVENTS_METRIC, TRIGGERED_RULES_METRIC];

/// The namespace the MIRROR of those counters is published into by a client. A
/// namespace of its own, because `PutMetricData` cannot write into an `AWS/` one.
const MIRROR_NAMESPACE: &str = "Probe/RoutingCounterMirror";

/// A dimension a reader may name that neither writer publishes anything under: the
/// name of an event SOURCE, which is a property of what was published rather than
/// of the bus's routing table.
const EVENT_SOURCE_NAME_DIMENSION: &str = "EventSourceName";

/// How far back and how far ahead of now a read's window reaches, in seconds: wide
/// enough that a just-published datapoint is inside it whichever minute it landed
/// in, and reaching past now the way a client watching a live counter reads it.
const WINDOW_BEFORE_SECS: f64 = 1_200.0;
const WINDOW_AFTER_SECS: f64 = 300.0;

/// How long a counter is polled for before its absence is taken as an answer, and
/// how long one wait between two reads is. Generous: the deadline is only there so
/// a counter that never arrives fails an assertion instead of hanging, and nothing
/// below asserts on how long anything took.
const READ_BUDGET: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The periods a just-published regular-resolution series is read at here. "For
/// metrics with regular resolution, a period can be as short as one minute (60
/// seconds) and must be a multiple of 60" (`GetMetricStatisticsInput$Period`); 60,
/// 300 and 3600 are the three the retention tiers are stated in
/// (`GetMetricStatistics`).
const PERIODS: [i64; 3] = [60, 300, 3_600];

/// The `Unit` values a read may name, with whether a series published in `Count`
/// answers it. The names are the model's own `StandardUnit` values
/// (`smithy_query(query="shape",shape="StandardUnit")`): the unit these counters
/// carry, and three the model declares that they do not — one of them `None`, the
/// value that stands for a measurement whose publisher named no unit.
const UNIT_PROBES: [(Option<&str>, bool); 5] = [
    (None, true),
    (Some("Count"), true),
    (Some("Count/Second"), false),
    (Some("Bytes"), false),
    (Some("None"), false),
];

/// How many times every probe is taken: more than once, so a read that is not a
/// pure report of the store fails a later pass.
const PASSES: u32 = 2;

/// Seconds since the epoch, as a read's `StartTime` / `EndTime` carry it.
fn epoch_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// One row: the bus the two rules live on, and how many matching events the round
/// publishes.
struct Row {
    case: &'static str,
    bus: &'static str,
    events: usize,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a custom bus, one matched event",
            bus: "probe-bus",
            events: 1,
        },
        Row {
            case: "a custom bus whose name carries a dot, two matched events",
            bus: "probe.bus-2",
            events: 2,
        },
    ]
}

/// The queue policy a client writes for an EventBridge target: `sqs:SendMessage`
/// allowed to the service principal, scoped to this account's own rules by
/// `aws:SourceAccount`.
fn granting_this_account(queue_arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowEventBridgeFromThisAccount",
            "Effect": "Allow",
            "Principal": {"Service": SERVICE_PRINCIPAL},
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
            "Condition": {
                "StringEquals": {
                    (SOURCE_ACCOUNT_CONDITION_KEY): engine::emulator_account_id(),
                },
            },
        }],
    })
    .to_string()
}

/// Create the row's custom bus.
async fn given_bus(emulator: &Emulator, case: &str, bus: &str) {
    let (status, created) = events(emulator, "CreateEventBus", &json!({ "Name": bus })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateEventBus answered {status} {created}"
    );
}

/// An ENABLED rule called `name` on `bus` pinning `source`, carrying the queue at
/// `target_arn` as its one target.
async fn given_rule_on(
    emulator: &Emulator,
    case: &str,
    bus: &str,
    name: &str,
    source: &str,
    target_arn: &str,
) {
    let pattern = json!({ "source": [source], "detail-type": [DETAIL_TYPE] }).to_string();
    let (status, body) = events(
        emulator,
        "PutRule",
        &json!({
            "Name": name,
            "EventBusName": bus,
            "EventPattern": pattern,
            "State": "ENABLED",
        }),
    )
    .await;
    assert_eq!(status, 200, "case {case:?}: PutRule answered {status} {body}");
    let (status, body) = events(
        emulator,
        "PutTargets",
        &json!({
            "Rule": name,
            "EventBusName": bus,
            "Targets": [{ "Id": "t1", "Arn": target_arn }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutTargets answered {status} {body}"
    );
    assert_eq!(
        body["FailedEntryCount"],
        json!(0),
        "case {case:?}: PutTargets reported a failed entry: {body}"
    );
}

/// Publish one matching event onto `bus` and answer the `EventId` it was given.
async fn publish_onto(emulator: &Emulator, case: &str, bus: &str, nonce: &str) -> String {
    let (status, body) = events(
        emulator,
        "PutEvents",
        &json!({
            "Entries": [{
                "EventBusName": bus,
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "nonce": nonce }).to_string(),
            }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutEvents answered {status} {body}"
    );
    assert_eq!(
        body["FailedEntryCount"],
        json!(0),
        "case {case:?}: PutEvents reported a failed entry: {body}"
    );
    body["Entries"][0]["EventId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no EventId in {body}"))
        .to_string()
}

/// Everything the queue at `url` holds once it holds `want` messages, so a delivery
/// still in flight is read again rather than read as missing.
async fn drain_until(emulator: &Emulator, url: &str, want: usize) -> Vec<Value> {
    let deadline = Instant::now() + READ_BUDGET;
    let mut held = Vec::new();
    loop {
        held.extend(drain(emulator, url).await);
        if held.len() >= want || Instant::now() >= deadline {
            return held;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Re-publish, as a CLIENT does, the `count` observations of value 1 in
/// [`METRIC_UNIT_COUNT`] that the matched rule's counter holds — under the same
/// metric name and the same dimension set, in the mirror namespace.
async fn mirror_observations(
    emulator: &Emulator,
    case: &str,
    metric: &str,
    dimensions: &Value,
    count: usize,
) {
    let data: Vec<Value> = (0..count)
        .map(|_| {
            json!({
                "MetricName": metric,
                "Dimensions": dimensions,
                "Value": 1.0,
                "Unit": METRIC_UNIT_COUNT,
            })
        })
        .collect();
    let (status, body) = cloudwatch(
        emulator,
        "PutMetricData",
        &json!({ "Namespace": MIRROR_NAMESPACE, "MetricData": data }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutMetricData of the {metric} mirror answered {status} {body}"
    );
}

/// What one `GetMetricStatistics` read of a series answers, reduced to the facts a
/// reader of a counter acts on: whether the series was served at all, the totals
/// over the window, and the set of units its datapoints carry. Timestamps and the
/// number of windows are left out on purpose — the mirror was published a moment
/// after the counters, so which minute each observation landed in is not a
/// property of the read path.
#[derive(Debug, PartialEq)]
struct Reading {
    served: bool,
    sum: f64,
    sample_count: f64,
    minimum: f64,
    maximum: f64,
    units: Vec<String>,
}

/// Read the series `(namespace, metric, dimensions)` the way a client watching a
/// live counter reads it, and reduce it to a [`Reading`].
async fn read_series(
    emulator: &Emulator,
    case: &str,
    namespace: &str,
    metric: &str,
    dimensions: &Value,
    period: i64,
    unit: Option<&str>,
) -> Reading {
    let now = epoch_seconds();
    let mut request = json!({
        "Namespace": namespace,
        "MetricName": metric,
        "Dimensions": dimensions,
        "StartTime": now - WINDOW_BEFORE_SECS,
        "EndTime": now + WINDOW_AFTER_SECS,
        "Period": period,
        "Statistics": ["Sum", "SampleCount", "Minimum", "Maximum"],
    });
    if let (Some(unit), Value::Object(members)) = (unit, &mut request) {
        members.insert("Unit".to_string(), json!(unit));
    }
    let (status, stats) = cloudwatch(emulator, "GetMetricStatistics", &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetMetricStatistics on {namespace} answered {status} {stats}"
    );
    assert_eq!(
        stats["Label"],
        json!(metric),
        "case {case:?}: expected the {namespace} read to be labelled with the metric, got {stats}"
    );
    let points = stats["Datapoints"].as_array().cloned().unwrap_or_default();
    let total = |statistic: &str| -> f64 {
        points
            .iter()
            .filter_map(|point| point[statistic].as_f64())
            .sum()
    };
    let mut units: Vec<String> = points
        .iter()
        .map(|point| point["Unit"].as_str().unwrap_or_default().to_string())
        .collect();
    units.sort();
    units.dedup();
    Reading {
        served: !points.is_empty(),
        sum: total("Sum"),
        sample_count: total("SampleCount"),
        // A count's observations are all 1, so the extremes of every window are 1
        // whichever window they fell in: totalling them keeps the comparison free
        // of how many windows each writer's observations spread over.
        minimum: total("Minimum"),
        maximum: total("Maximum"),
        units,
    }
}

/// The `Sum` the matched rule's counter reports once it accounts for `want`
/// matches, so a counter still being written is read again rather than read as
/// missing.
async fn settled_sum(emulator: &Emulator, case: &str, metric: &str, dimensions: &Value, want: f64) {
    let deadline = Instant::now() + READ_BUDGET;
    loop {
        let reading =
            read_series(emulator, case, METRIC_NAMESPACE, metric, dimensions, 60, None).await;
        if reading.sum >= want {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {metric} for the matched rule to account for {want} \
             match(es), got {reading:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The dimension sets, by name, that `ListMetrics` reports for `metric` in
/// `namespace` when narrowed to the series carrying the matched rule's name.
async fn listed_dimension_names(
    emulator: &Emulator,
    case: &str,
    namespace: &str,
    metric: &str,
) -> Vec<Vec<String>> {
    let (status, listed) = cloudwatch(
        emulator,
        "ListMetrics",
        &json!({
            "Namespace": namespace,
            "Dimensions": [{ "Name": RULE_NAME_DIMENSION, "Value": MATCHING_RULE }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListMetrics on {namespace} answered {status} {listed}"
    );
    let mut sets: Vec<Vec<String>> = listed["Metrics"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|found| found["MetricName"] == json!(metric))
        .map(|found| {
            found["Dimensions"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|dimension| dimension["Name"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .collect();
    sets.sort();
    sets
}

/// Every dimension set a client may name for the row, with whether the matched
/// rule's counters were published under it.
fn dimension_probes(bus: &str) -> Vec<(&'static str, Value, bool)> {
    vec![
        (
            "the whole identity of the matched rule",
            rule_dimensions(bus, MATCHING_RULE),
            true,
        ),
        ("the bus with no rule", bus_dimensions(bus), false),
        (
            "the rule name with no bus",
            rule_name_dimensions(MATCHING_RULE),
            false,
        ),
        (
            "the whole identity of the twin the events did not match",
            rule_dimensions(bus, TWIN_RULE),
            false,
        ),
        (
            "the name of the event's source",
            json!([{ "Name": EVENT_SOURCE_NAME_DIMENSION, "Value": SOURCE }]),
            false,
        ),
        (
            "the published set plus a third dimension",
            json!([
                { "Name": EVENT_BUS_NAME_DIMENSION, "Value": bus },
                { "Name": RULE_NAME_DIMENSION, "Value": MATCHING_RULE },
                { "Name": EVENT_SOURCE_NAME_DIMENSION, "Value": SOURCE },
            ]),
            false,
        ),
    ]
}

/// Take every probe once, comparing the `AWS/Events` series against its mirror.
async fn assert_parity(emulator: &Emulator, row: &Row, pass: u32, matched: f64) {
    let case = format!("{} (pass {pass})", row.case);
    let case = case.as_str();

    for metric in ROUTING_METRICS {
        for (dimensioned, dimensions, published) in dimension_probes(row.bus) {
            for period in PERIODS {
                for (unit, served) in UNIT_PROBES {
                    let named = unit.unwrap_or("no unit");
                    let appended = read_series(
                        emulator,
                        case,
                        METRIC_NAMESPACE,
                        metric,
                        &dimensions,
                        period,
                        unit,
                    )
                    .await;
                    let ingested = read_series(
                        emulator,
                        case,
                        MIRROR_NAMESPACE,
                        metric,
                        &dimensions,
                        period,
                        unit,
                    )
                    .await;
                    assert_eq!(
                        appended, ingested,
                        "case {case:?}: expected {metric} read under {dimensioned} at period \
                         {period}s under {named} to answer the same for the series \
                         {METRIC_NAMESPACE} appended as for the one {MIRROR_NAMESPACE} \
                         ingested, got {appended:?} against {ingested:?}"
                    );
                    // And what that shared answer IS: the count in the unit it was
                    // stored in where the set was published and the read's unit
                    // matches, and no datapoint at all otherwise.
                    if published && served {
                        assert_eq!(
                            (appended.sum, appended.sample_count),
                            (matched, matched),
                            "case {case:?}: expected {metric} read under {dimensioned} at \
                             period {period}s under {named} to hold {matched} occurrence(s) \
                             summing to {matched}, got {appended:?}"
                        );
                        assert_eq!(
                            appended.units,
                            vec![METRIC_UNIT_COUNT.to_string()],
                            "case {case:?}: expected every {metric} datapoint under \
                             {dimensioned} at period {period}s under {named} in \
                             {METRIC_UNIT_COUNT}, got {appended:?}"
                        );
                    } else {
                        assert!(
                            !appended.served,
                            "case {case:?}: expected {metric} read under {dimensioned} at \
                             period {period}s under {named} to answer with no datapoint at \
                             all, got {appended:?}"
                        );
                    }
                }
            }
        }

        // The enumeration of the dimension sets each store holds for the counter,
        // narrowed by a `DimensionFilter` on the rule's name the way a client
        // discovers a rule's series: the same one set in both, and that set is the
        // bus together with the rule.
        let appended = listed_dimension_names(emulator, case, METRIC_NAMESPACE, metric).await;
        let ingested = listed_dimension_names(emulator, case, MIRROR_NAMESPACE, metric).await;
        assert_eq!(
            appended, ingested,
            "case {case:?}: expected ListMetrics to enumerate the same {metric} dimension \
             sets for the series {METRIC_NAMESPACE} appended as for the one \
             {MIRROR_NAMESPACE} ingested, got {appended:?} against {ingested:?}"
        );
        assert_eq!(
            appended,
            vec![vec![
                EVENT_BUS_NAME_DIMENSION.to_string(),
                RULE_NAME_DIMENSION.to_string(),
            ]],
            "case {case:?}: expected the one {metric} series the filter answers to carry \
             exactly the bus and the rule, got {appended:?}"
        );
    }
}

#[tokio::test]
async fn a_services_own_namespace_is_read_on_the_same_terms_as_an_ingested_one() {
    for row in rows() {
        let case = row.case;
        let emulator = emulator();
        given_bus(&emulator, case, row.bus).await;
        let (queue_url, queue_arn) =
            given_queue_with_policy(&emulator, "deliveries", Some(&granting_this_account)).await;
        given_rule_on(
            &emulator,
            case,
            row.bus,
            MATCHING_RULE,
            SOURCE,
            &queue_arn,
        )
        .await;
        given_rule_on(
            &emulator,
            case,
            row.bus,
            TWIN_RULE,
            OTHER_SOURCE,
            &queue_arn,
        )
        .await;

        let mut ids = Vec::new();
        for round in 0..row.events {
            ids.push(publish_onto(&emulator, case, row.bus, &format!("n-{round}")).await);
        }

        // The delivery the match caused, read where a consumer reads it: one
        // message per published event, from the matched rule's target only — two
        // arrivals per event would mean the twin matched too.
        let delivered = drain_until(&emulator, &queue_url, row.events).await;
        let mut envelopes: Vec<(String, String)> = delivered
            .iter()
            .map(|message| {
                let body = message["Body"].as_str().unwrap_or_default();
                let envelope: Value = serde_json::from_str(body).unwrap_or_else(|error| {
                    panic!("case {case:?}: a delivered body is not JSON ({error}): {body}")
                });
                (
                    envelope["source"].as_str().unwrap_or_default().to_string(),
                    envelope["id"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        envelopes.sort();
        let mut expected: Vec<(String, String)> = ids
            .iter()
            .map(|id| (SOURCE.to_string(), id.clone()))
            .collect();
        expected.sort();
        assert_eq!(
            envelopes, expected,
            "case {case:?}: expected one delivery per published event, each naming the \
             matched source and the id its publication returned, got {envelopes:?}"
        );

        let matched = f64::from(u32::try_from(row.events).unwrap_or(u32::MAX));
        let published = rule_dimensions(row.bus, MATCHING_RULE);
        // Wait for the accounting of the round to be complete before the mirror is
        // written, so the two series carry the same observations.
        for metric in ROUTING_METRICS {
            settled_sum(&emulator, case, metric, &published, matched).await;
            mirror_observations(&emulator, case, metric, &published, row.events).await;
        }

        // The two namespaces now hold the same observations under the same metric
        // names and the same dimension set, and nothing anywhere else.
        let (status, listed) = cloudwatch(
            &emulator,
            "ListMetrics",
            &json!({ "Namespace": MIRROR_NAMESPACE }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListMetrics answered {status} {listed}"
        );
        let mirror: Vec<Value> = listed["Metrics"].as_array().cloned().unwrap_or_default();
        let expected_mirror: Vec<Value> = ROUTING_METRICS
            .iter()
            .map(|metric| {
                json!({
                    "Namespace": MIRROR_NAMESPACE,
                    "MetricName": metric,
                    "Dimensions": published,
                })
            })
            .collect();
        assert_eq!(
            ordered(&mirror),
            ordered(&expected_mirror),
            "case {case:?}: expected the mirror namespace to hold one series per counter \
             under the published dimension set, got {mirror:?}"
        );

        for pass in 1..=PASSES {
            assert_parity(&emulator, &row, pass, matched).await;
        }
    }
}
