// SPDX-License-Identifier: CC0-1.0

//! The TWO identities this service mints, and the form each of them keeps.
//!
//! One rule, asserted over a table whose rows differ along what the resources of one store
//! watch: the two values this service mints name two different things, so each keeps the form of
//! what it names, and no client that parses one is handed the other's form.
//!
//! * the `CreationId` a metric alarm read reports names the WRITE that created the alarm, so it
//!   carries that write and the instant it happened at: `<uuid>/<epoch-millis>`
//!   ([`creation_identity_millis`]).
//! * the `AnomalyDetectorId` a write mints for an anomaly detection model names the MODEL ("The
//!   unique identifier of the anomaly detector that you created or updated.",
//!   `PutAnomalyDetectorOutput$AnomalyDetectorId`, length 1..128, pattern
//!   `^[A-Za-z0-9_./:%()+-]+$`), and an identifier a service hands a client is opaque, so it is
//!   ONE token: no separator to split it on and no clock reading in it
//!   ([`assert_one_opaque_token`], whose own suite asserts that form over every write that mints
//!   a model).
//!
//! The bytes of either value are the service's own per-run value; what every row here asserts is
//! the form each keeps, that each value is its resource's own, and — for the creation identity —
//! that the instant it names is the one its write happened at.

use serde_json::{json, Value};

use crate::anomaly_detector_id_form::{assert_one_opaque_token, put_detector, single_metric};
use crate::harness::{
    build_router, creation_identity_millis, described_alarm, merged, metric_alarm_request, ok,
    reported_creation_id, AlarmKind,
};

/// The epoch milliseconds the creation identity `identity` names, failing `case` when `identity`
/// is not of the form such an identity takes.
///
/// Read through the harness's one reader of that form ([`creation_identity_millis`]), so this
/// suite and every suite that asserts an alarm's creation identity agree on what the form is.
fn creation_millis(identity: &str, case: &str) -> i64 {
    let millis = creation_identity_millis(identity).unwrap_or_else(|| {
        panic!("case {case:?}: expected a creation identity of the form <uuid>/<epoch-millis>, got {identity:?}")
    });
    assert!(
        millis > 0,
        "case {case:?}: the identity names the instant the write happened at, got {millis} \
         milliseconds in {identity:?}"
    );
    millis
}

/// The creation identity a read reports for the alarm `name`.
async fn reported_identity(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
    case: &str,
) -> String {
    let alarm = described_alarm(router, AlarmKind::Metric, name)
        .await
        .unwrap_or(Value::Null);
    reported_creation_id(&alarm)
        .unwrap_or_else(|| panic!("case {case:?}: expected a CreationId on {alarm}"))
        .to_string()
}

#[tokio::test]
async fn the_two_identities_of_one_store_each_keep_their_own_form() {
    // Rows differ along what the alarm and the model of the row watch — a metric named flat, a
    // percentile statistic, and a metric-math array — so the two forms are asserted over the
    // writes a store meets rather than over one recorded request.
    let rows: Vec<(&str, Value, Value)> = vec![
        (
            "a metric alarm on a named metric, and a model of the same metric",
            json!({}),
            single_metric("Latency", None, "Average"),
        ),
        (
            "a percentile alarm, and a model with a percentile statistic",
            json!({"Statistic": null, "ExtendedStatistic": "p90"}),
            single_metric("Queued", None, "p99"),
        ),
        (
            "an alarm with dimensions, and a model of a dimensioned series",
            json!({"Dimensions": [{"Name": "Service", "Value": "checkout"}]}),
            single_metric(
                "Errors",
                Some(json!([{"Name": "Service", "Value": "checkout"}])),
                "Sum",
            ),
        ),
    ];

    for (case, patch, detector) in rows {
        let router = build_router();
        let alarm_name = "minted-identity-alarm";
        ok(
            &router,
            "PutMetricAlarm",
            &merged(metric_alarm_request(alarm_name), &patch),
        )
        .await;
        let creation_id = reported_identity(&router, alarm_name, case).await;
        creation_millis(&creation_id, case);

        let (detector_id, from_secs, to_secs) = put_detector(&router, &detector, case).await;
        assert_one_opaque_token(&detector_id, case, from_secs, to_secs);

        // Two values, two things named: neither resource is handed the other's identity.
        assert_ne!(
            creation_id, detector_id,
            "case {case:?}: each resource carries an identity of its own, got {creation_id:?} \
             twice"
        );
        // And a later read reports the same creation identity for the same alarm: the identity
        // belongs to the write that created it, not to the request that asked for it.
        let read_again = reported_identity(&router, alarm_name, case).await;
        assert_eq!(
            read_again, creation_id,
            "case {case:?}: every read reports the identity of the write that created the alarm"
        );
    }
}
