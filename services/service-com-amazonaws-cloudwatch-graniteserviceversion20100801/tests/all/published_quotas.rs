// SPDX-License-Identifier: CC0-1.0

//! The QUOTAS this service enforces, and what it answers once one is full.
//!
//! The rule under test: **a published quota of this service is counted over the resources it is
//! a quota ON, enforced on the call that would ADD one, and answered with the class that
//! quota's own operation models — so a caller that keeps creating reaches a refusal rather than
//! an account this emulator would hold more of than AWS would.** Three quotas, each with the
//! axis it counts along:
//!
//! * the Contributor Insights RULES one account holds (`PutInsightRule`, 100);
//! * the alarms that watch a Metrics Insights QUERY (`PutMetricAlarm`, 200), which is a
//!   ceiling of its own under the alarm quota as a whole;
//! * the composite alarms one ALARM may be a child of (`PutCompositeAlarm`, 150): "Any single
//!   alarm can be included in the rule expressions of as many as 150 composite alarms."
//!
//! Each table's rows span the quota itself: the last create the quota admits is SERVED and the
//! next one is refused, and a write that REPLACES a resource already counted is served at the
//! quota, because replacing adds none. The counting axis is asserted too — a resource of
//! another kind, or another child, does not consume the quota under test.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{build_router, call, error_code, error_message, ok};

/// CloudWatch's published Contributor Insights rule quota.
const RULE_QUOTA: usize = 100;

/// Its published quota for the alarms that watch a Metrics Insights query.
const METRICS_INSIGHTS_ALARM_QUOTA: usize = 200;

/// The composite alarms one alarm may be a child of (`PutCompositeAlarm` documentation).
const RULE_PARENT_QUOTA: usize = 150;

/// A Contributor Insights rule definition, as `PutInsightRule` takes it.
fn rule_definition() -> String {
    json!({
        "Schema": {"Name": "CloudWatchLogRule", "Version": 1},
        "LogGroupNames": ["API-Gateway-Access-Logs"],
        "LogFormat": "JSON",
        "Contribution": {"Keys": ["$.ip"]},
    })
    .to_string()
}

/// One `PutInsightRule` call for the rule `name`.
async fn put_rule(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    call(
        router,
        "PutInsightRule",
        &json!({"RuleName": name, "RuleDefinition": rule_definition()}),
    )
    .await
}

/// A `PutMetricAlarm` request whose series is a Metrics Insights QUERY.
fn insights_alarm(name: &str) -> Value {
    json!({
        "AlarmName": name,
        "EvaluationPeriods": 1,
        "Threshold": 1.0,
        "ComparisonOperator": "GreaterThanThreshold",
        "Metrics": [{
            "Id": "q1",
            "Expression": "SELECT AVG(Latency) FROM \"Custom/Quotas\"",
            "Period": 60,
            "ReturnData": true,
        }],
    })
}

/// A `PutMetricAlarm` request whose series is ONE metric, which the Insights quota does not
/// count.
fn metric_alarm(name: &str) -> Value {
    json!({
        "AlarmName": name,
        "Namespace": "Custom/Quotas",
        "MetricName": "Latency",
        "Statistic": "Average",
        "Period": 60,
        "EvaluationPeriods": 1,
        "Threshold": 1.0,
        "ComparisonOperator": "GreaterThanThreshold",
    })
}

#[tokio::test]
async fn the_contributor_insights_rule_quota_is_enforced_on_the_write_that_would_pass_it() {
    let router = build_router();
    for index in 0..RULE_QUOTA {
        let (status, body) = put_rule(&router, &format!("rule-{index}")).await;
        assert_eq!(
            status, 200,
            "case \"rule {index} of the quota\": every create inside the quota is served, got \
             {status} with {body}"
        );
    }

    // The create past it is refused, under the class the operation models for a quota.
    let (status, body) = put_rule(&router, "one-too-many").await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "LimitExceededException"),
        "the create past the quota is refused, got {status} with {body}"
    );
    let message = error_message(&body);
    let expected =
        format!("TOTAL_RULES_EXCEEDED: Put would exceed the total rule limit of {RULE_QUOTA}.");
    assert_eq!(
        message, expected,
        "expected the coded quota sentence {expected:?}, got {message:?}"
    );

    // Replacing a rule the quota already counts adds none, so it is served AT the quota.
    let (status, body) = put_rule(&router, "rule-0").await;
    assert_eq!(
        status, 200,
        "rewriting a rule the quota already counts adds none, got {status} with {body}"
    );
}

#[tokio::test]
async fn the_metrics_insights_alarm_quota_counts_only_the_alarms_it_is_a_quota_on() {
    let router = build_router();

    // The axis first: an alarm that watches ONE metric is not an alarm of this quota's kind,
    // so a store full of them leaves the whole quota free.
    for index in 0..10 {
        ok(
            &router,
            "PutMetricAlarm",
            &metric_alarm(&format!("plain-{index}")),
        )
        .await;
    }

    for index in 0..METRICS_INSIGHTS_ALARM_QUOTA {
        let (status, body) = call(
            &router,
            "PutMetricAlarm",
            &insights_alarm(&format!("insights-{index}")),
        )
        .await;
        assert_eq!(
            status, 200,
            "case \"Metrics Insights alarm {index} of the quota\": every create inside the \
             quota is served, got {status} with {body}"
        );
    }

    let (status, body) = call(
        &router,
        "PutMetricAlarm",
        &insights_alarm("insights-one-too-many"),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "LimitExceededFault"),
        "the Metrics Insights alarm past the quota is refused, got {status} with {body}"
    );

    // Replacing an alarm the quota already counts adds none; and an alarm of the other kind is
    // still created, because this quota does not count it.
    let (status, body) = call(&router, "PutMetricAlarm", &insights_alarm("insights-0")).await;
    assert_eq!(
        status, 200,
        "rewriting an alarm the quota already counts adds none, got {status} with {body}"
    );
    let (status, body) = call(&router, "PutMetricAlarm", &metric_alarm("plain-last")).await;
    assert_eq!(
        status, 200,
        "an alarm of another kind is not counted against this quota, got {status} with {body}"
    );
}

#[tokio::test]
async fn an_alarm_may_be_a_child_of_as_many_composite_alarms_as_the_quota_admits() {
    let router = build_router();
    ok(&router, "PutMetricAlarm", &metric_alarm("child")).await;
    ok(&router, "PutMetricAlarm", &metric_alarm("other-child")).await;

    for index in 0..RULE_PARENT_QUOTA {
        let (status, body) = call(
            &router,
            "PutCompositeAlarm",
            &json!({
                "AlarmName": format!("parent-{index}"),
                "AlarmRule": "ALARM(\"child\")",
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case \"parent {index} of the quota\": every create inside the quota is served, got \
             {status} with {body}"
        );
    }

    let (status, body) = call(
        &router,
        "PutCompositeAlarm",
        &json!({
            "AlarmName": "parent-one-too-many",
            "AlarmRule": "ALARM(\"child\")",
        }),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ValidationException"),
        "the composite alarm past the quota is refused, got {status} with {body}"
    );
    let message = error_message(&body);
    let expected = format!(
        "Alarm arn:aws:cloudwatch:us-east-1:000000000000:alarm:child references too many \
         parents. Maximum allowed referenced parent count is {RULE_PARENT_QUOTA} per alarm."
    );
    assert_eq!(
        message, expected,
        "expected the quota sentence naming the child, got {message:?}"
    );

    // The quota is counted per CHILD: another alarm has its own, and rewriting a parent that
    // already references the child adds none.
    let (status, body) = call(
        &router,
        "PutCompositeAlarm",
        &json!({
            "AlarmName": "other-parent",
            "AlarmRule": "ALARM(\"other-child\")",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "another child has a quota of its own, got {status} with {body}"
    );
    let (status, body) = call(
        &router,
        "PutCompositeAlarm",
        &json!({
            "AlarmName": "parent-0",
            "AlarmRule": "ALARM(\"child\") OR OK(\"child\")",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "rewriting a parent the quota already counts adds none, got {status} with {body}"
    );
}
