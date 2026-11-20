// SPDX-License-Identifier: CC0-1.0

//! The Amazon CloudWatch alarms that block a practice run, driven end to end over the
//! wire.
//!
//! RULE: a practice run configuration names the alarms ARC watches for a resource — an
//! outcome alarm, which ends a run, and optional blocking alarms, which block one — so
//! `StartPracticeRun` refuses a run 409 `ConflictException`, with the
//! `ConflictExceptionReason` the model names for that list, while one of those alarms is
//! in the `ALARM` state, and admits it when none of them is.
//!
//! The alarms belong to CloudWatch, not to ARC, so this suite mounts both services on ONE
//! shared engine — the way the router mounts them — and creates each alarm and forces its
//! state through CloudWatch's OWN front door (`PutMetricAlarm`, `SetAlarmState`), never by
//! reaching into a store. The ARC calls are real `restJson1` requests, as in the sibling
//! suites.
//!
//! The rows differ along what the rule generalises over: WHICH list the sounding alarm is
//! in (blocking, outcome, both), WHICH state it is in (`ALARM`, `OK`,
//! `INSUFFICIENT_DATA`), how MANY alarms the list holds and which of them sounds, and
//! whether the alarm the configuration names exists in CloudWatch at all.

use std::sync::Arc;

use serde_json::{json, Value};

use service_com_amazonaws_arczonalshift_percdataplane::state::{
    PRACTICE_BLOCKING_ALARMS_RED_MESSAGE, PRACTICE_BLOCKING_ALARMS_RED_REASON,
    PRACTICE_OUTCOME_ALARMS_RED_MESSAGE, PRACTICE_OUTCOME_ALARMS_RED_REASON,
};
use service_com_amazonaws_arczonalshift_percdataplane::EmulatedService as ZonalShift;
use service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService as CloudWatch;
use smithy_server_runtime::MultiServiceRouter;

use super::practice_run_configuration::alarm_arn;
use super::practice_run_start::start_practice_run;
use super::registered_managed_resources::register;

/// One emulator with CloudWatch mounted beside ARC on the SAME engine: the alarms a
/// practice run is blocked by belong to CloudWatch, so both services are driven through
/// this one front door.
pub(crate) fn emulator_with_cloudwatch() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signatures are the emulator core's business, not this service's.
    router.set_request_authentication(false);
    router.register_service(ZonalShift::new(Arc::clone(&engine)));
    router.register_service(CloudWatch::new(Arc::clone(&engine)));
    (router, engine)
}

/// Call one CLOUDWATCH operation through the same front door and require success,
/// answering the response body.
///
/// CloudWatch's model declares `awsJson1_0` (`smithy_query(query="service_info")`), so the
/// envelope and the target prefix are that service's own, and the SigV4 credential scope
/// names its `monitoring` signing name, which is what routes the request there.
async fn cloudwatch_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header(
            "x-amz-target",
            format!("GraniteServiceVersion20100801.{operation}"),
        )
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/monitoring/\
             aws4_request, SignedHeaders=host, Signature=0000",
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a valid test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    assert_eq!(
        status,
        200,
        "cloudwatch {operation} must succeed, got {status} with {}",
        String::from_utf8_lossy(response.body())
    );
    serde_json::from_slice(response.body()).unwrap_or(Value::Null)
}

/// Create the CloudWatch alarm `name` and force it into `state`, the way a caller does:
/// `PutMetricAlarm` then `SetAlarmState`. Creating an alarm twice re-states it, so a row
/// may name one alarm in two lists.
async fn given_alarm(router: &MultiServiceRouter, name: &str, state: &str) {
    cloudwatch_ok(
        router,
        "PutMetricAlarm",
        json!({
            "AlarmName": name,
            "Namespace": "Sim/ArcPracticeRun",
            "MetricName": "Errors",
            "Statistic": "Sum",
            "Period": 60,
            "EvaluationPeriods": 1,
            "Threshold": 1.0,
            "ComparisonOperator": "GreaterThanOrEqualToThreshold",
            "TreatMissingData": "notBreaching",
        }),
    )
    .await;
    cloudwatch_ok(
        router,
        "SetAlarmState",
        json!({
            "AlarmName": name,
            "StateValue": state,
            "StateReason": "set by a test of the practice run blockers",
        }),
    )
    .await;
}

/// The state CloudWatch reports for the alarm `name`, read through `DescribeAlarms`.
async fn alarm_state(router: &MultiServiceRouter, name: &str) -> Option<String> {
    let described = cloudwatch_ok(router, "DescribeAlarms", json!({"AlarmNames": [name]})).await;
    described["MetricAlarms"]
        .as_array()?
        .iter()
        .find(|alarm| alarm["AlarmName"] == json!(name))
        .and_then(|alarm| alarm["StateValue"].as_str())
        .map(str::to_string)
}

/// One control condition naming the alarm `name` by its ARN, which is what a
/// `ControlCondition` names an alarm by. `CLOUDWATCH` is the only condition type the model
/// declares (`ControlConditionType`).
fn condition(name: &str) -> Value {
    json!({"type": "CLOUDWATCH", "alarmIdentifier": alarm_arn(name)})
}

/// What one row of the table sets up and expects.
struct Row {
    case: &'static str,
    /// The alarms the configuration's `outcomeAlarms` names.
    outcome_alarms: &'static [&'static str],
    /// The alarms its `blockingAlarms` names, if it names any.
    blocking_alarms: &'static [&'static str],
    /// The alarms CREATED in CloudWatch, each with the state it is forced into; an alarm
    /// the configuration names and this list does not is one CloudWatch holds nothing for.
    alarms: &'static [(&'static str, &'static str)],
    /// The `ConflictExceptionReason` and sentence the start is refused with, or `None`
    /// when the run is admitted.
    refusal: Option<(&'static str, &'static str)>,
}

/// RULE (above), asserted over the table this suite's module documentation describes.
#[tokio::test]
async fn a_practice_run_is_blocked_while_one_of_its_alarms_is_in_the_alarm_state() {
    let rows: &[Row] = &[
        Row {
            case: "no alarm sounding",
            outcome_alarms: &["quiet-outcome"],
            blocking_alarms: &["quiet-blocking"],
            alarms: &[("quiet-outcome", "OK"), ("quiet-blocking", "OK")],
            refusal: None,
        },
        Row {
            case: "the outcome alarm in ALARM",
            outcome_alarms: &["red-outcome"],
            blocking_alarms: &["calm-blocking"],
            alarms: &[("red-outcome", "ALARM"), ("calm-blocking", "OK")],
            refusal: Some((
                PRACTICE_OUTCOME_ALARMS_RED_REASON,
                PRACTICE_OUTCOME_ALARMS_RED_MESSAGE,
            )),
        },
        Row {
            case: "a blocking alarm in ALARM",
            outcome_alarms: &["calm-outcome"],
            blocking_alarms: &["red-blocking"],
            alarms: &[("calm-outcome", "OK"), ("red-blocking", "ALARM")],
            refusal: Some((
                PRACTICE_BLOCKING_ALARMS_RED_REASON,
                PRACTICE_BLOCKING_ALARMS_RED_MESSAGE,
            )),
        },
        Row {
            case: "both lists sounding, which the blocking alarms answer for",
            outcome_alarms: &["both-outcome"],
            blocking_alarms: &["both-blocking"],
            alarms: &[("both-outcome", "ALARM"), ("both-blocking", "ALARM")],
            refusal: Some((
                PRACTICE_BLOCKING_ALARMS_RED_REASON,
                PRACTICE_BLOCKING_ALARMS_RED_MESSAGE,
            )),
        },
        Row {
            case: "the last of three blocking alarms sounding",
            outcome_alarms: &["three-outcome"],
            blocking_alarms: &["three-first", "three-second", "three-third"],
            alarms: &[
                ("three-outcome", "OK"),
                ("three-first", "OK"),
                ("three-second", "OK"),
                ("three-third", "ALARM"),
            ],
            refusal: Some((
                PRACTICE_BLOCKING_ALARMS_RED_REASON,
                PRACTICE_BLOCKING_ALARMS_RED_MESSAGE,
            )),
        },
        Row {
            case: "an alarm with no data yet, which is not the ALARM state",
            outcome_alarms: &["unknown-outcome"],
            blocking_alarms: &[],
            alarms: &[("unknown-outcome", "INSUFFICIENT_DATA")],
            refusal: None,
        },
        Row {
            case: "an alarm CloudWatch holds nothing for, which sounds for nothing",
            outcome_alarms: &["absent-outcome"],
            blocking_alarms: &["absent-blocking"],
            alarms: &[],
            refusal: None,
        },
        Row {
            case: "an alarm of another resource's configuration sounding",
            outcome_alarms: &["shared-quiet"],
            blocking_alarms: &[],
            alarms: &[("shared-quiet", "OK"), ("another-resource-red", "ALARM")],
            refusal: None,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let (router, engine) = emulator_with_cloudwatch();
        let zones = ["use1-az1", "use1-az2"];
        let resource = format!("arn:aws:eks:us-east-1:000000000000:cluster/alarmed-{index}");
        register(&engine, &resource, &format!("alarmed-{index}"), &zones);
        for (alarm, state) in row.alarms {
            given_alarm(&router, alarm, state).await;
            assert_eq!(
                alarm_state(&router, alarm).await.as_deref(),
                Some(*state),
                "case {:?}: the alarm {alarm} must stand in {state} before the run",
                row.case
            );
        }

        let mut configuration = serde_json::Map::new();
        configuration.insert("resourceIdentifier".to_string(), json!(resource));
        configuration.insert(
            "outcomeAlarms".to_string(),
            Value::Array(row.outcome_alarms.iter().map(|name| condition(name)).collect()),
        );
        if !row.blocking_alarms.is_empty() {
            configuration.insert(
                "blockingAlarms".to_string(),
                Value::Array(
                    row.blocking_alarms
                        .iter()
                        .map(|name| condition(name))
                        .collect(),
                ),
            );
        }
        let (status, created) = super::zonal_shift_lifecycle::call(
            &router,
            "POST",
            "/configuration",
            &Value::Object(configuration).to_string(),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {:?}: the configuration under test is created first, got {status} {created}",
            row.case
        );

        let (status, answered) =
            start_practice_run(&router, &resource, zones[0], "an alarm-gated run").await;
        match row.refusal {
            Some((reason, message)) => {
                assert_eq!(
                    status, 409,
                    "case {:?}: expected the sounding alarm to block the run, got {status} \
                     {answered}",
                    row.case
                );
                assert_eq!(
                    (
                        super::zonal_shift_lifecycle::error_code(&answered).as_str(),
                        answered["reason"].as_str(),
                        answered["message"].as_str(),
                    ),
                    ("ConflictException", Some(reason), Some(message)),
                    "case {:?}: expected the conflict the model names for that list, got \
                     {answered}",
                    row.case
                );
            }
            None => assert_eq!(
                status, 200,
                "case {:?}: expected the run admitted, got {status} {answered}",
                row.case
            ),
        }
    }
}

/// RULE: the alarm state a run is blocked by is read where it LIVES, so a run blocked at
/// one instant is admitted once the alarm stops sounding, and no alarm state is copied
/// into ARC's own store.
#[tokio::test]
async fn a_run_blocked_by_an_alarm_starts_once_the_alarm_stops_sounding() {
    let (router, engine) = emulator_with_cloudwatch();
    let zones = ["use1-az1", "use1-az2"];
    let resource = "arn:aws:eks:us-east-1:000000000000:cluster/alarm-recovery";
    register(&engine, resource, "alarm-recovery", &zones);
    given_alarm(&router, "recovering-outcome", "ALARM").await;
    let (status, created) = super::zonal_shift_lifecycle::call(
        &router,
        "POST",
        "/configuration",
        &json!({
            "resourceIdentifier": resource,
            "outcomeAlarms": [condition("recovering-outcome")],
        })
        .to_string(),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup: the configuration must be created, got {status} {created}"
    );

    let (status, refused) = start_practice_run(&router, resource, zones[0], "while red").await;
    assert_eq!(
        status, 409,
        "case while the outcome alarm sounds: expected the run blocked, got {status} {refused}"
    );

    // The alarm recovers, in the store that owns it.
    given_alarm(&router, "recovering-outcome", "OK").await;
    let (status, started) = start_practice_run(&router, resource, zones[0], "once green").await;
    assert_eq!(
        status, 200,
        "case once the outcome alarm recovers: expected the run admitted, got {status} {started}"
    );
    assert_eq!(
        started["status"].as_str(),
        Some("ACTIVE"),
        "case once the outcome alarm recovers: expected an ACTIVE practice run, got {started}"
    );
}
