// SPDX-License-Identifier: CC0-1.0

//! An alarm action of the scaling vendor is written in a form CloudWatch itself publishes —
//! "Autoscaling action:
//! arn:aws:autoscaling:`region`:`account-id`:scalingPolicy:`policy-id`:autoScalingGroupName/`group-friendly-name`:policyName/`policy-friendly-name`"
//! (`smithy_query(query="doc", shape="PutMetricAlarmInput", member="AlarmActions")`) — so an ARN
//! of that vendor whose relative id is written some other way names no policy any scaling
//! service could resolve: every alarm writer refuses it with HTTP 400 under the
//! request-validation class, naming the relative id the request sent and the form the vendor
//! publishes, and stores nothing.
//!
//! The rows span what the rule generalises over rather than repeating one request: WHICH field
//! of the relative id is missing or unnamed (the policy name, the scaled resource, the policy
//! id, the resource type itself), which other resource type of the same vendor the ARN names,
//! which of the three action-list members carries the action, the Region the alarm lives in,
//! and which of the three alarm writers is driven.
//!
//! The accepted table is the other half: a policy of either scaling service that mints under
//! this vendor — the Auto Scaling group form the model documents, and the scalable-target form
//! Application Auto Scaling's own `PolicyARN` carries, including a scaled resource whose id
//! itself contains colons — is stored and read back verbatim through `DescribeAlarms`.
//!
//! The Region of an action is judged off the ARN AHEAD of its relative id, so a well-formed
//! policy naming another Region is answered with the Region refusal
//! (`crate::alarm_action_region`) and nothing of this vendor's form; one row pins that order.

use crate::harness::{
    build_router, call_in_region, error_code, log_alarm_request, merged, metric_alarm_request,
    ok, AlarmKind, DEFAULT_REGION,
};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The account the policy ARNs of this suite name.
const ACCOUNT: &str = "000000000000";

/// The three action-list members every alarm writer carries; each one's documentation lists the
/// same action values.
const ACTION_MEMBERS: [&str; 3] = ["AlarmActions", "OKActions", "InsufficientDataActions"];

/// The vendor every scaling policy is minted under, as the documented action value writes it.
const VENDOR: &str = "autoscaling";

/// The form a refusal states the relative id of an action of this vendor has to be written in,
/// as the documented action value spells it.
const FORM: &str = "scalingPolicy:policy-id:autoScalingGroupName/group-friendly-name:\
                    policyName/policy-friendly-name";

/// The refusal CloudWatch answers for a relative id of this vendor outside that form, built
/// from the request: the vendor the ARN named, the relative id it carried, and the form. The
/// second sentence runs straight into the form with no separator, as the service words it.
fn refusal(relative_id: &str) -> String {
    format!(
        "The vendor \"{VENDOR}\" cannot be used with relative id {relative_id}. Allowed \
         relative IDs must be in the format{FORM}"
    )
}

/// The ways a relative id of this vendor misses the published form.
///
/// Rows differ in WHICH field is at fault — the policy's own name, the resource the policy
/// scales, the policy id, the resource type — and in how many fields the id carries at all,
/// including another resource type of the same vendor (an Auto Scaling group is no action).
fn malformed_relative_ids() -> Vec<(&'static str, String)> {
    let policy = "7bb0e3a5-0000-4000-8000-000000000000";
    vec![
        (
            "a policy truncated after the group it scales",
            format!("scalingPolicy:{policy}:autoScalingGroupName/web"),
        ),
        (
            "a policy truncated after its id",
            format!("scalingPolicy:{policy}"),
        ),
        (
            "a policy naming no scaled resource",
            format!("scalingPolicy:{policy}:policyName/scale-out"),
        ),
        (
            "a policy naming no policy id",
            "scalingPolicy::autoScalingGroupName/web:policyName/scale-out".to_string(),
        ),
        (
            "a policy whose group is unnamed",
            format!("scalingPolicy:{policy}:autoScalingGroupName/:policyName/scale-out"),
        ),
        (
            "a policy whose own name is empty",
            format!("scalingPolicy:{policy}:autoScalingGroupName/web:policyName/"),
        ),
        (
            "a policy scaling a resource of no scalable kind",
            format!("scalingPolicy:{policy}:launchConfiguration/web:policyName/scale-out"),
        ),
        (
            "the resource type with nothing after it",
            "scalingPolicy".to_string(),
        ),
        (
            "an Auto Scaling group of the same vendor",
            format!("autoScalingGroup:{policy}:autoScalingGroupName/web"),
        ),
        (
            "the policy's fields in the wrong order",
            format!("scalingPolicy:{policy}:policyName/scale-out:autoScalingGroupName/web"),
        ),
    ]
}

/// Every relative id of this vendor the published form admits.
///
/// The first is the Auto Scaling group form the model documents; the others are the
/// scalable-target form Application Auto Scaling mints its `PolicyARN` under
/// (`scalingPolicy:<policy id>:resource/<service namespace>/<resource id>:policyName/<name>`),
/// whose resource id may itself carry slashes and colons.
fn well_formed_relative_ids() -> Vec<(&'static str, String)> {
    let policy = "7bb0e3a5-0000-4000-8000-000000000000";
    vec![
        (
            "an Amazon EC2 Auto Scaling policy",
            format!("scalingPolicy:{policy}:autoScalingGroupName/web:policyName/scale-out"),
        ),
        (
            "an Application Auto Scaling policy over an ECS service",
            format!("scalingPolicy:{policy}:resource/ecs/service/default/api:policyName/track-cpu"),
        ),
        (
            "an Application Auto Scaling policy over a resource whose id carries colons",
            format!(
                "scalingPolicy:{policy}:resource/lambda/function:send-mail:BLUE:\
                 policyName/track-concurrency"
            ),
        ),
        (
            // The policy's own name is the last field and runs to the end of the relative id,
            // since `PutScalingPolicy$PolicyName` is one string of the printable range and so
            // admits a colon of its own.
            "a policy whose own name carries a colon",
            format!("scalingPolicy:{policy}:autoScalingGroupName/web:policyName/out:east"),
        ),
    ]
}

/// The action ARN one relative id is written as, in `region`.
fn action_arn(region: &str, relative_id: &str) -> String {
    format!("arn:aws:{VENDOR}:{region}:{ACCOUNT}:{relative_id}")
}

/// A request of `kind` for the alarm `name` carrying `patch`, and the operation that writes it.
///
/// Every writer carries the same three action lists, so a row is driven through each of them
/// off one place. A composite alarm's rule references another alarm, so `child` is the alarm
/// its rule names; the caller creates it.
fn write_request(kind: AlarmKind, name: &str, child: &str, patch: &Value) -> (&'static str, Value) {
    match kind {
        AlarmKind::Metric => ("PutMetricAlarm", merged(metric_alarm_request(name), patch)),
        AlarmKind::Composite => (
            "PutCompositeAlarm",
            merged(
                json!({"AlarmName": name, "AlarmRule": format!("ALARM(\"{child}\")")}),
                patch,
            ),
        ),
        AlarmKind::Log => ("PutLogAlarm", merged(log_alarm_request(name), patch)),
    }
}

/// The alarms of `kind` `DescribeAlarms` reports for exactly `name`, as seen from `region`.
async fn described(
    router: &MultiServiceRouter,
    region: &str,
    kind: AlarmKind,
    name: &str,
) -> Vec<Value> {
    let (status, body) = call_in_region(
        router,
        region,
        "DescribeAlarms",
        &json!({"AlarmNames": [name], "AlarmTypes": [kind.alarm_type()]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAlarms {name}: expected HTTP 200, got {status} with body {body}"
    );
    body.get(kind.member())
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Drive the writer of `kind` in `region` with `request` and require the vendor refusal for
/// `relative_id`, with no alarm of `name` left behind.
async fn assert_refused(
    router: &MultiServiceRouter,
    region: &str,
    kind: AlarmKind,
    case: &str,
    name: &str,
    request: &(&str, Value),
    relative_id: &str,
) {
    let (status, body) = call_in_region(router, region, request.0, &request.1).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "ValidationException",
        "case {case:?}: expected the request-validation class, got {body}"
    );
    assert_eq!(
        body.get("message").and_then(Value::as_str),
        Some(refusal(relative_id).as_str()),
        "case {case:?}: expected the vendor refusal, got {body}"
    );
    let alarms = described(router, region, kind, name).await;
    assert!(
        alarms.is_empty(),
        "case {case:?}: a refused request must store no alarm, got {alarms:?}"
    );
}

#[tokio::test]
async fn a_malformed_scaling_policy_action_is_refused_and_stores_nothing() {
    let router = build_router();

    let mut index = 0usize;
    for member in ACTION_MEMBERS {
        for (case, relative_id) in malformed_relative_ids() {
            let name = format!("refused-{index}");
            index += 1;
            let case = format!("{case} in {member}");
            let request = write_request(
                AlarmKind::Metric,
                &name,
                "",
                &json!({member: [action_arn(DEFAULT_REGION, &relative_id)]}),
            );
            assert_refused(
                &router,
                DEFAULT_REGION,
                AlarmKind::Metric,
                &case,
                &name,
                &request,
                &relative_id,
            )
            .await;
        }
    }
}

#[tokio::test]
async fn every_alarm_writer_refuses_a_malformed_scaling_policy_action() {
    let router = build_router();

    // The three writers carry the same action values, and the rows run in two Regions so the
    // relative id a refusal names is the request's own and not a constant.
    for (index, region) in ["us-east-1", "eu-central-1"].iter().enumerate() {
        for kind in [AlarmKind::Metric, AlarmKind::Composite, AlarmKind::Log] {
            // The composite alarm's rule needs an alarm to reference.
            let child = format!("child-{index}-{}", kind.alarm_type());
            let (operation, request) = write_request(AlarmKind::Metric, &child, "", &json!({}));
            let (status, body) = call_in_region(&router, region, operation, &request).await;
            assert_eq!(
                status, 200,
                "the child alarm {child}: expected HTTP 200, got {status} with body {body}"
            );

            for (row, (case, relative_id)) in malformed_relative_ids().into_iter().enumerate() {
                let name = format!("writer-{index}-{}-{row}", kind.alarm_type());
                let case = format!("{case} on a {} in {region}", kind.alarm_type());
                let request = write_request(
                    kind,
                    &name,
                    &child,
                    &json!({"AlarmActions": [action_arn(region, &relative_id)]}),
                );
                assert_refused(
                    &router, region, kind, &case, &name, &request, &relative_id,
                )
                .await;
            }
        }
    }
}

#[tokio::test]
async fn one_malformed_action_refuses_the_whole_request_wherever_it_sits() {
    let router = build_router();

    let good = action_arn(DEFAULT_REGION, &well_formed_relative_ids()[0].1);
    let topic = format!("arn:aws:sns:{DEFAULT_REGION}:{ACCOUNT}:oncall");
    for (row, (case, relative_id)) in malformed_relative_ids().into_iter().enumerate() {
        let bad = action_arn(DEFAULT_REGION, &relative_id);
        // Rows differ in where the malformed action sits among the actions an alarm may carry:
        // one of them outside the published form refuses the request whatever surrounds it.
        let arrangements = [
            ("the malformed action alone", vec![bad.clone()]),
            ("the malformed action first", vec![bad.clone(), good.clone()]),
            (
                "the malformed action between two valid ones",
                vec![good.clone(), bad.clone(), topic.clone()],
            ),
        ];
        for (slot, (arrangement, actions)) in arrangements.into_iter().enumerate() {
            let name = format!("beside-{row}-{slot}");
            let case = format!("{case} with {arrangement}");
            let request =
                write_request(AlarmKind::Metric, &name, "", &json!({"AlarmActions": actions}));
            assert_refused(
                &router,
                DEFAULT_REGION,
                AlarmKind::Metric,
                &case,
                &name,
                &request,
                &relative_id,
            )
            .await;
        }
    }
}

#[tokio::test]
async fn a_well_formed_scaling_policy_action_round_trips() {
    let router = build_router();

    let mut index = 0usize;
    for region in ["us-east-1", "eu-central-1"] {
        for member in ACTION_MEMBERS {
            for (case, relative_id) in well_formed_relative_ids() {
                let arn = action_arn(region, &relative_id);
                let name = format!("accepted-{index}");
                index += 1;
                let case = format!("{case} in {member} on an alarm in {region}");
                let (operation, request) = write_request(
                    AlarmKind::Metric,
                    &name,
                    "",
                    &json!({member: [arn.clone()]}),
                );
                let (status, body) =
                    call_in_region(&router, region, operation, &request).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected HTTP 200, got {status} with body {body}"
                );
                let alarms = described(&router, region, AlarmKind::Metric, &name).await;
                assert_eq!(
                    alarms.len(),
                    1,
                    "case {case:?}: expected exactly the one alarm, got {alarms:?}"
                );
                assert_eq!(
                    alarms[0].get(member),
                    Some(&json!([arn])),
                    "case {case:?}: expected the action to round-trip verbatim, got {}",
                    alarms[0]
                );
            }
        }
    }
}

#[tokio::test]
async fn a_refused_update_leaves_the_stored_action() {
    let router = build_router();

    let good = action_arn(DEFAULT_REGION, &well_formed_relative_ids()[0].1);
    for (index, (case, relative_id)) in malformed_relative_ids().into_iter().enumerate() {
        let name = format!("configured-{index}");
        let (operation, created) = write_request(
            AlarmKind::Metric,
            &name,
            "",
            &json!({"AlarmActions": [good.clone()]}),
        );
        ok(&router, operation, &created).await;

        let update = write_request(
            AlarmKind::Metric,
            &name,
            "",
            &json!({"AlarmActions": [action_arn(DEFAULT_REGION, &relative_id)]}),
        );
        let case = format!("an update to {name} carrying {case}");
        let (status, body) =
            call_in_region(&router, DEFAULT_REGION, update.0, &update.1).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            body.get("message").and_then(Value::as_str),
            Some(refusal(&relative_id).as_str()),
            "case {case:?}: expected the vendor refusal, got {body}"
        );
        let alarms = described(&router, DEFAULT_REGION, AlarmKind::Metric, &name).await;
        assert_eq!(
            alarms.len(),
            1,
            "case {case:?}: the configured alarm must still be there, got {alarms:?}"
        );
        assert_eq!(
            alarms[0].get("AlarmActions"),
            Some(&json!([good.clone()])),
            "case {case:?}: the refused update must leave the stored action, got {}",
            alarms[0]
        );
    }
}

#[tokio::test]
async fn a_policy_naming_another_region_is_refused_on_its_region() {
    let router = build_router();

    // The Region is read off the ARN ahead of the relative id, so a policy in another Region is
    // answered with the Region refusal whether or not its relative id is well formed — and
    // nothing is stored either way.
    let alarm_region = DEFAULT_REGION;
    let foreign = "us-west-2";
    let rows = well_formed_relative_ids()
        .into_iter()
        .chain(malformed_relative_ids());
    for (index, (case, relative_id)) in rows.enumerate() {
        let name = format!("foreign-{index}");
        let case = format!("{case} in {foreign} on an alarm in {alarm_region}");
        let (operation, request) = write_request(
            AlarmKind::Metric,
            &name,
            "",
            &json!({"AlarmActions": [action_arn(foreign, &relative_id)]}),
        );
        let (status, body) = call_in_region(&router, alarm_region, operation, &request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected the request-validation class, got {body}"
        );
        assert_eq!(
            body.get("message").and_then(Value::as_str),
            Some(
                format!("Invalid region {foreign} specified. Only {alarm_region} is supported.")
                    .as_str()
            ),
            "case {case:?}: expected the Region refusal, got {body}"
        );
        let alarms = described(&router, alarm_region, AlarmKind::Metric, &name).await;
        assert!(
            alarms.is_empty(),
            "case {case:?}: a refused request must store no alarm, got {alarms:?}"
        );
    }
}
