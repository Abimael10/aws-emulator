// SPDX-License-Identifier: CC0-1.0

//! An OpenSearch domain's own `AWS/ES` series drives a CloudWatch alarm from
//! `INSUFFICIENT_DATA` through `OK` to `ALARM`, and the alarm's SNS action reaches the
//! subscribed queue — through the WHOLE mounted service set, the one `core/router/src/lib.rs`
//! assembles for the binary.
//!
//! The RULE under test, one level above the recorded case: **a metric alarm over a series an
//! AWS service publishes about a resource it owns is evaluated from THOSE datapoints and
//! nothing else — so while the published reading sits on the non-breaching side of the
//! alarm's threshold the alarm settles out of `INSUFFICIENT_DATA` into `OK`, and when the
//! resource's own contents move the reading across that threshold the alarm transitions, once,
//! into `ALARM` with a `StateReason` naming the threshold it crossed, records that one
//! transition in its own history, and publishes the alarm notification to the topic its
//! `AlarmActions` names, which every subscribed queue receives; a transition that is not INTO
//! `ALARM` publishes no such notification, and an alarm whose threshold the reading never
//! reaches never leaves `OK`.**
//!
//! Four services have to agree for that to hold — Amazon OpenSearch Service publishes the
//! `AWS/ES` `SearchableDocuments` gauge of the domain whose indexes it holds, CloudWatch
//! stores the series and evaluates the alarm over it, SNS fans the alarm action out and SQS
//! holds what arrived — and the publish CloudWatch makes is an in-process call addressed the
//! way a client's own `sns`-scoped call is addressed, so only the production service set can
//! show the whole chain. The crate-local suites pin each link:
//! `services/service-com-amazonaws-opensearch-amazonopensearchservice/tests/all/cluster_metrics.rs`
//! pins what a cluster publishes and how an alarm over it evaluates, and
//! [`crate::cloudwatch_alarm_action_reaches_its_subscribed_queue`] pins the delivery and its
//! `ActionsEnabled` gate; what only this suite can reach is the same chain over the production
//! service set, with the search domain's data plane as the thing that moves the metric.
//!
//! The rows differ along what the rule generalises over: the threshold the load has to cross
//! (one the load reaches exactly, one it passes, one it never comes near), the direction of
//! the comparison (`GreaterThanOrEqualToThreshold`, `GreaterThanThreshold`,
//! `LessThanThreshold`), and therefore which transition each alarm makes — into `ALARM` on the
//! load, out of `ALARM` on the load, or none at all. The recorded case the issue was filed
//! about — ten documents indexed under an alarm at a threshold of ten — is the first row.
//!
//! Nothing here writes into a store to arrange what it then asserts: the domain is created
//! through `CreateDomain`, the documents are loaded over the domain's own REST data plane, the
//! alarms are created with `PutMetricAlarm`, and every reading is taken through
//! `DescribeAlarms`, `DescribeAlarmHistory` and `ReceiveMessage`.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_opensearch_amazonopensearchservice::state::cluster_metrics::{
    CLIENT_ID_DIMENSION, DOMAIN_NAME_DIMENSION, METRIC_NAMESPACE, SEARCHABLE_DOCUMENTS_METRIC,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{
    authorization, cw, drain, drain_until, given_topic_with_subscriber, router, Grant,
};

/// The Region the whole wiring lives in: the domain, its series, the alarms, the topics and
/// the queues all resolve inside one Region.
const REGION: &str = "us-east-1";

/// Amazon OpenSearch Service's signing name (`sigv4Name: es`) and the URI its domain resource
/// is bound to.
const OPENSEARCH_SIGNING_NAME: &str = "es";
const OPENSEARCH_DOMAIN_URI: &str = "/2021-01-01/opensearch/domain";

/// The domain the suite provisions, and the index the documents are loaded into.
const DOMAIN: &str = "alarmed-search-domain";
const INDEX: &str = "catalog";

/// The engine release the domain runs.
const ENGINE_VERSION: &str = "OpenSearch_2.11";

/// How many documents the load indexes — the load the recorded case makes.
const DOCUMENTS: usize = 10;

/// The state a freshly written alarm stands in: "When this operation creates an alarm, the
/// alarm state is immediately set to INSUFFICIENT_DATA" (`PutMetricAlarm`).
const INITIAL_STATE: &str = "INSUFFICIENT_DATA";

/// The alarm's `Period`, in seconds: the one-minute windows standard-resolution datapoints are
/// stored at ("which CloudWatch stores at 1-minute resolution",
/// `MetricDatum$StorageResolution`), which is the period the recorded alarm is configured
/// with.
const ALARM_PERIOD: i64 = 60;

/// How long each phase waits for the condition it is about: the domain reporting itself
/// provisioned, every alarm reaching the state its datapoints imply.
///
/// Generous on purpose. An evaluation grades the newest period that has CLOSED, so a state is
/// at most one period away from the change that implies it; this is only the point at which
/// waiting is called a failure, and nothing here asserts on how long anything took.
const PHASE_DEADLINE: Duration = Duration::from_secs(120);

/// How long each look waits before looking again. Non-zero, so the runtime gets to run the
/// sampler and the deliveries the wait is for.
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// One request answered by the router: the status and the parsed body.
struct Answer {
    status: u16,
    body: Value,
}

/// One Amazon OpenSearch Service request over its `restJson1` wire — the control plane under
/// `/2021-01-01/opensearch`, and the domain's own REST data plane under the prefixed path.
async fn es(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    payload: String,
) -> Answer {
    let host = format!("{OPENSEARCH_SIGNING_NAME}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", host)
        .header(
            "authorization",
            authorization(OPENSEARCH_SIGNING_NAME, REGION),
        )
        .header("content-type", "application/json")
        .body(Bytes::from(payload))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    Answer { status, body }
}

/// `CreateDomain` for a single-node domain, asserting it was accepted.
async fn given_domain(router: &MultiServiceRouter) {
    let body = json!({
        "DomainName": DOMAIN,
        "EngineVersion": ENGINE_VERSION,
        "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 1 },
        "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
    });
    let answer = es(router, "POST", OPENSEARCH_DOMAIN_URI, body.to_string()).await;
    assert_eq!(
        answer.status, 200,
        "setup CreateDomain: expected 200, got {} with {}",
        answer.status, answer.body
    );
}

/// Wait until `DescribeDomain` reports the domain created with nothing in flight — the
/// condition a client's readiness poll waits for, rather than a span of time.
async fn provisioned(router: &MultiServiceRouter) {
    let deadline = Instant::now() + PHASE_DEADLINE;
    loop {
        let answer = es(
            router,
            "GET",
            &format!("{OPENSEARCH_DOMAIN_URI}/{DOMAIN}"),
            String::new(),
        )
        .await;
        let status = &answer.body["DomainStatus"];
        if status["Created"].as_bool() == Some(true)
            && status["Processing"].as_bool() == Some(false)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup DescribeDomain: the domain never reported itself provisioned, last read {}",
            answer.body
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Create the index, then bulk-load `DOCUMENTS` documents into it over the domain's own REST
/// data plane — the prefixed addressing a client on this machine reaches the domain at.
async fn given_documents(router: &MultiServiceRouter) {
    let created = es(
        router,
        "PUT",
        &format!("/_aws/es/{DOMAIN}/{INDEX}"),
        json!({}).to_string(),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup PUT /{INDEX}: expected 200 from the index create, got {} with {}",
        created.status, created.body
    );
    let mut payload = String::new();
    for id in 0..DOCUMENTS {
        payload.push_str(&json!({ "index": { "_index": INDEX, "_id": id.to_string() } }).to_string());
        payload.push('\n');
        payload.push_str(&json!({ "position": id }).to_string());
        payload.push('\n');
    }
    let loaded = es(router, "POST", &format!("/_aws/es/{DOMAIN}/_bulk"), payload).await;
    assert_eq!(
        loaded.status, 200,
        "setup the bulk load: expected 200 from the bulk API, got {} with {}",
        loaded.status, loaded.body
    );
}

/// One row: the alarm's threshold and comparison, the state its datapoints imply while the
/// cluster holds nothing, and the state they imply once the load has landed.
struct Row {
    case: &'static str,
    threshold: f64,
    comparison: &'static str,
    empty_state: &'static str,
    loaded_state: &'static str,
}

/// The rows: thresholds the load reaches exactly, passes and never comes near, over both
/// directions of the comparison.
const ROWS: &[Row] = &[
    Row {
        case: "the recorded case: ten documents reaching a threshold of ten",
        threshold: 10.0,
        comparison: "GreaterThanOrEqualToThreshold",
        empty_state: "OK",
        loaded_state: "ALARM",
    },
    Row {
        case: "a strict comparison the load passes",
        threshold: 3.0,
        comparison: "GreaterThanThreshold",
        empty_state: "OK",
        loaded_state: "ALARM",
    },
    Row {
        case: "a threshold the load never comes near",
        threshold: 100.0,
        comparison: "GreaterThanOrEqualToThreshold",
        empty_state: "OK",
        loaded_state: "OK",
    },
    Row {
        case: "a comparison the empty cluster already breaches",
        threshold: 1.0,
        comparison: "LessThanThreshold",
        empty_state: "ALARM",
        loaded_state: "OK",
    },
];

/// `PutMetricAlarm` over the domain's `SearchableDocuments` series, with `topic` as the alarm
/// action — the configuration the recorded case creates, with this row's own threshold and
/// comparison.
async fn given_alarm(router: &MultiServiceRouter, row: &Row, name: &str, topic: &str) {
    cw(
        router,
        row.case,
        REGION,
        "PutMetricAlarm",
        &json!({
            "AlarmName": name,
            "Namespace": METRIC_NAMESPACE,
            "MetricName": SEARCHABLE_DOCUMENTS_METRIC,
            "Dimensions": [
                { "Name": CLIENT_ID_DIMENSION, "Value": engine::emulator_account_id() },
                { "Name": DOMAIN_NAME_DIMENSION, "Value": DOMAIN },
            ],
            "Statistic": "Maximum",
            "Period": ALARM_PERIOD,
            "EvaluationPeriods": 1,
            "Threshold": row.threshold,
            "ComparisonOperator": row.comparison,
            "TreatMissingData": "ignore",
            "AlarmActions": [topic],
        }),
    )
    .await;
}

/// The alarm `DescribeAlarms` reports for `name`.
async fn described(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let answer = cw(
        router,
        case,
        REGION,
        "DescribeAlarms",
        &json!({ "AlarmNames": [name] }),
    )
    .await;
    answer["MetricAlarms"][0].clone()
}

/// Wait until `DescribeAlarms` reports `name` in `state`, answering the alarm it then reported.
async fn alarm_reaches(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    state: &str,
) -> Value {
    let deadline = Instant::now() + PHASE_DEADLINE;
    loop {
        let alarm = described(router, case, name).await;
        if alarm["StateValue"].as_str() == Some(state) {
            return alarm;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected alarm {name:?} to reach {state:?}, got {alarm}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Every `StateUpdate` item of `name`'s history, as the `(oldState, newState)` pair its
/// `HistoryData` records.
async fn state_updates(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> Vec<(String, String)> {
    let answer = cw(
        router,
        case,
        REGION,
        "DescribeAlarmHistory",
        &json!({ "AlarmName": name, "HistoryItemType": "StateUpdate" }),
    )
    .await;
    answer["AlarmHistoryItems"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|item| {
            let data: Value = item["HistoryData"]
                .as_str()
                .and_then(|text| serde_json::from_str(text).ok())
                .unwrap_or_else(|| item["HistoryData"].clone());
            let state = |which: &str| {
                data[which]["stateValue"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            };
            (state("oldState"), state("newState"))
        })
        .collect()
}

/// How many notifications a phase expects on a row's queue: one for a transition INTO `ALARM`,
/// and none otherwise.
fn expected_notifications(from: &str, to: &str) -> usize {
    usize::from(to == "ALARM" && from != "ALARM")
}

#[tokio::test]
async fn the_domains_published_document_gauge_drives_its_alarms_and_their_queues() {
    let router = router();

    // The domain, provisioned through its own control plane: from here it publishes its
    // `AWS/ES` gauges, which is the only thing that gives these alarms datapoints at all.
    given_domain(&router).await;
    provisioned(&router).await;

    // One topic, one queue and one alarm per row, so a row's silence is read where a client
    // reads it rather than inferred from a queue shared with another row.
    let mut wiring: Vec<(String, String)> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let (topic, queue) = given_topic_with_subscriber(
            &router,
            row.case,
            REGION,
            &format!("es-documents-{index}"),
            if index % 2 == 0 {
                Grant::Unscoped
            } else {
                Grant::ScopedToTopic
            },
        )
        .await;
        let alarm = format!("es-searchable-documents-{index}");
        given_alarm(&router, row, &alarm, &topic).await;
        wiring.push((alarm, queue));
    }

    // PHASE ONE — the cluster holds nothing: every alarm settles on the state the gauge of an
    // empty cluster implies, and the alarms whose threshold that reading already breaches are
    // the only ones whose action publishes.
    for (row, (alarm, queue)) in ROWS.iter().zip(&wiring) {
        let reached = alarm_reaches(&router, row.case, alarm, row.empty_state).await;
        assert_eq!(
            reached["StateValue"].as_str(),
            Some(row.empty_state),
            "case {:?}: expected the empty cluster's reading to settle the alarm at {:?}, got \
             {reached}",
            row.case,
            row.empty_state
        );
        let wanted = expected_notifications(INITIAL_STATE, row.empty_state);
        let mut delivered = drain_until(&router, row.case, REGION, queue, wanted).await;
        // The queue is looked at once more whatever was wanted, so a row that expects nothing
        // asserts over a queue that was really read rather than over a poll that never ran.
        let extra = drain(&router, row.case, REGION, queue).await;
        delivered.extend(extra);
        assert_eq!(
            delivered.len(),
            wanted,
            "case {:?}: expected {wanted} notification(s) while the cluster is empty, got \
             {delivered:?}",
            row.case
        );
        for notification in &delivered {
            assert_eq!(
                (
                    notification["AlarmName"].as_str(),
                    notification["OldStateValue"].as_str(),
                    notification["NewStateValue"].as_str(),
                ),
                (
                    Some(alarm.as_str()),
                    // The state the write that configured the alarm left it in: "the alarm
                    // state is immediately set to INSUFFICIENT_DATA" (`PutMetricAlarm`), and
                    // with `TreatMissingData: ignore` it is kept until a datapoint exists.
                    Some(INITIAL_STATE),
                    Some("ALARM"),
                ),
                "case {:?}: only a transition into ALARM may publish, and it must name the \
                 alarm and the states it moved between, got {notification}",
                row.case
            );
        }
    }

    // PHASE TWO — the load the recorded case makes, over the domain's own data plane.
    given_documents(&router).await;

    for (row, (alarm, queue)) in ROWS.iter().zip(&wiring) {
        let reached = alarm_reaches(&router, row.case, alarm, row.loaded_state).await;

        // The explanation names the threshold the evaluation compared the datapoints against,
        // built from the ROW's own threshold: the sentence states a measurement, so a whole
        // number carries its fraction.
        let names_threshold = format!("threshold ({:.1})", row.threshold);
        let reason = reached["StateReason"].as_str().unwrap_or_default();
        assert!(
            reason.contains(&names_threshold),
            "case {:?}: expected the evaluated state's explanation to name {names_threshold:?}, \
             got {reached}",
            row.case
        );

        // Exactly one transition into ALARM is recorded for an alarm the load drove there, and
        // it comes from the state the alarm had settled in before the load.
        let updates = state_updates(&router, row.case, alarm).await;
        let into_alarm: Vec<&(String, String)> = updates
            .iter()
            .filter(|(_, new)| new == "ALARM")
            .collect();
        let wanted_updates = usize::from(row.empty_state == "ALARM" || row.loaded_state == "ALARM");
        assert_eq!(
            into_alarm.len(),
            wanted_updates,
            "case {:?}: expected {wanted_updates} StateUpdate(s) into ALARM in the alarm's own \
             history, got {updates:?}",
            row.case
        );
        if row.loaded_state == "ALARM" {
            assert_eq!(
                into_alarm.first().map(|(old, _)| old.as_str()),
                Some(row.empty_state),
                "case {:?}: expected the recorded transition to come from {:?}, got {updates:?}",
                row.case,
                row.empty_state
            );
        }

        // And the delivery: the transition the load drove publishes once to the row's topic,
        // and a transition that is not into ALARM publishes nothing.
        let wanted = expected_notifications(row.empty_state, row.loaded_state);
        let delivered = drain_until(&router, row.case, REGION, queue, wanted).await;
        // One more look, whatever was wanted: nothing arrives twice for one transition, and a
        // row that expects nothing asserts over a queue that was really read.
        let again = drain(&router, row.case, REGION, queue).await;
        assert!(
            again.is_empty(),
            "case {:?}: one transition may be delivered once, got {again:?} after {delivered:?}",
            row.case
        );
        assert_eq!(
            delivered.len(),
            wanted,
            "case {:?}: expected {wanted} notification(s) once the load had landed, got \
             {delivered:?}",
            row.case
        );
        for notification in &delivered {
            assert_eq!(
                (
                    notification["AlarmName"].as_str(),
                    notification["OldStateValue"].as_str(),
                    notification["NewStateValue"].as_str(),
                ),
                (Some(alarm.as_str()), Some(row.empty_state), Some("ALARM")),
                "case {:?}: expected the notification to name the alarm and the transition it \
                 reports, got {notification}",
                row.case
            );
        }
    }
}
