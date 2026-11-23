// SPDX-License-Identifier: CC0-1.0

//! What a failed `AWS::AutoScaling::WarmPool` reports on its own event, end to end over the
//! awsQuery wire.
//!
//! RULE under test: **a resource provider built on the CloudFormation resource-provider plugin
//! reports a refusal it classified as that plugin's invalid-request failure with the class's own
//! sentence opener before the called service's message — so the failed resource's
//! `ResourceStatusReason` quotes `Invalid request provided: <the owning service's sentence and
//! its SDK's diagnostic>` and closes on the `InvalidRequest` class — whichever member of the
//! declaration earned the refusal and at whatever stage of the stack's life the refused call was
//! made.** The warm pool's provider is such a provider; the stack rolls back, and nothing of the
//! refused change reaches the pool.
//!
//! Why it matters: a client reading a rolled-back deployment triages it from the failed
//! resource's reason, and a template-driven negative test asserts on exactly that text. The
//! opener is what says the provider refused the REQUEST — a declaration to fix — rather than
//! having passed on a service that failed, and a reason that drops it does not match what the
//! provider reports, so the triage keyed on it sees nothing.
//!
//! The rows differ along what the rule generalises over: WHICH member of the declaration the
//! owning service refused and WHY — a `PoolState` outside the set that member admits, a `MinSize`
//! below the range it admits, a `MaxGroupPreparedCapacity` below the range it admits, and a group
//! name no group of that service answers to — plus the STAGE the refused call was made at, the
//! create of a new pool and the update of one already deployed.
//!
//! Nothing here waits out a span: every case waits for the CONDITION — the stack settled, the
//! event recorded — so what it observes never depends on how fast the host runs.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::status;

use crate::harness::{
    assert_sdk_diagnostic_with_attempts, failed_resource_reason, quoted_and_class,
};
use crate::stack_lifecycle::{
    autoscaling, call, create_stack, member, router, settled_stack, xml_blocks, xml_text, REGION,
};

/// The opener the resource-provider plugin's invalid-request exception words, which is part of
/// the message the provider reports and therefore of what CloudFormation quotes.
const INVALID_REQUEST_PROVIDED: &str = "Invalid request provided: ";

/// The `HandlerErrorCode` class a refusal of the REQUEST falls in — one of the model's own
/// `HandlerErrorCode` values, and the class the opener above belongs to.
const INVALID_REQUEST: &str = "InvalidRequest";

/// The SDK's own name for Amazon EC2 Auto Scaling, as the exception its SDK raises states it,
/// and the status that service answers a call it will not carry out with.
const AUTO_SCALING_SERVICE: &str = "AutoScaling";
const REFUSED_STATUS: u16 = 400;

/// The statuses a resource records for a create and an update its provider could not carry out.
const CREATE_FAILED: &str = "CREATE_FAILED";
const UPDATE_FAILED: &str = "UPDATE_FAILED";

/// The AMI the launch template's data names, and the zone the groups are placed in.
const IMAGE_ID: &str = "ami-00000000000000001";
const ZONE: &str = "us-east-1a";

/// The logical ids of the template's group and of the warm pool declared beside it.
const GROUP_ID: &str = "Asg";
const POOL_ID: &str = "Pool";

/// The states a warm pool's instances may be held in, which is the set
/// `PutWarmPoolType$PoolState` admits
/// (`smithy_query(query="shape",shape="WarmPoolState",
/// model_path="services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/\
/// smithy-model/com-amazonaws-autoscaling-autoscaling-2011-01-01.json")`: `Stopped`, `Running`,
/// `Hibernated`) — so a refusal of a value outside it names these three and no others. The row
/// below asks for `Terminated`, which is a state an instance of the GROUP reaches and no state a
/// pool holds one in.
const POOL_STATES: &[&str] = &["Stopped", "Running", "Hibernated"];

/// One declaration the owning service refuses: the stack to deploy, the warm pool properties
/// that earn the refusal, and a fragment of that service's own sentence the quoted message must
/// carry — the member it attributed the refusal to, which is what says the refusal is the one the
/// row asked for rather than another.
struct Row {
    case: &'static str,
    stack: &'static str,
    properties: fn() -> Value,
    sentence: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a pool whose declared PoolState is outside the set the member admits",
        stack: "asg-pool-state",
        properties: || json!({"MinSize": 0, "MaxGroupPreparedCapacity": 0, "PoolState": "Terminated"}),
        sentence: "at 'poolState' failed to satisfy constraint",
    },
    Row {
        case: "a pool whose declared MinSize is below the range the member admits",
        stack: "asg-pool-min",
        properties: || json!({"MinSize": -1, "PoolState": "Stopped"}),
        sentence: "at 'minSize' failed to satisfy constraint",
    },
    Row {
        case: "a pool whose declared MaxGroupPreparedCapacity is below the range the member \
               admits",
        stack: "asg-pool-prepared",
        properties: || json!({"MinSize": 0, "MaxGroupPreparedCapacity": -2}),
        sentence: "at 'maxGroupPreparedCapacity' failed to satisfy constraint",
    },
    Row {
        case: "a pool declared on a group name no group of that service answers to",
        stack: "asg-pool-absent-group",
        properties: || json!({"AutoScalingGroupName": "asg-pool-absent-group-nowhere"}),
        sentence: "asg-pool-absent-group-nowhere",
    },
];

/// A template declaring a launch template, one group, and the warm pool the case states.
///
/// The pool addresses the group the same stack created unless the case declares a group name of
/// its own, so a row that varies nothing else varies only the member it was written for.
fn pool_template(properties: &Value) -> String {
    let mut declared = json!({"AutoScalingGroupName": {"Ref": GROUP_ID}});
    for (property, value) in properties.as_object().into_iter().flatten() {
        declared[property] = value.clone();
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Lt": {
                "Type": "AWS::EC2::LaunchTemplate",
                "Properties": {
                    "LaunchTemplateData": {"ImageId": IMAGE_ID, "InstanceType": "t3.micro"},
                },
            },
            GROUP_ID: {
                "Type": "AWS::AutoScaling::AutoScalingGroup",
                "Properties": {
                    "MinSize": "0",
                    "MaxSize": "1",
                    "DesiredCapacity": "0",
                    "AvailabilityZones": [ZONE],
                    "LaunchTemplate": {
                        "LaunchTemplateId": {"Ref": "Lt"},
                        "Version": {"Fn::GetAtt": ["Lt", "LatestVersionNumber"]},
                    },
                },
            },
            POOL_ID: {
                "Type": "AWS::AutoScaling::WarmPool",
                "Properties": declared,
            },
        },
        "Outputs": {GROUP_ID: {"Value": {"Ref": GROUP_ID}}},
    })
    .to_string()
}

/// Hold one failed resource's reason to the rule: the plugin's opener, then the owning service's
/// own sentence and its SDK's diagnostic of the refused call, under the invalid-request class.
fn assert_plugin_worded(case: &str, reason: &str, sentence: &str) {
    let (quoted, class) = quoted_and_class(case, reason);
    let service_message = quoted.strip_prefix(INVALID_REQUEST_PROVIDED).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the quoted message to open with \
             {INVALID_REQUEST_PROVIDED:?}, got {quoted:?}"
        )
    });
    assert!(
        service_message.contains(sentence),
        "case {case:?}: expected the owning service's own sentence carrying {sentence:?}, got \
         {service_message:?}"
    );
    assert_eq!(
        class, INVALID_REQUEST,
        "case {case:?}: expected the {INVALID_REQUEST} class, got {class:?} in {reason:?}"
    );
    assert_sdk_diagnostic_with_attempts(case, service_message, AUTO_SCALING_SERVICE, REFUSED_STATUS);
}

/// The warm pool Amazon EC2 Auto Scaling reports for `group` right now — the body of its
/// `WarmPoolConfiguration`, or `None` where it reports no pool at all.
async fn reported_pool(case: &str, router: &MultiServiceRouter, group: &str) -> Option<String> {
    let (code, body) = autoscaling(
        router,
        "DescribeWarmPool",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeWarmPool({group}) must answer 200, got {code} {body}"
    );
    xml_blocks(&body, "WarmPoolConfiguration").into_iter().next()
}

#[tokio::test]
async fn a_refused_warm_pool_reports_the_plugins_invalid_request_wording() {
    for row in ROWS {
        let case = row.case;
        let router = router();
        let template = pool_template(&(row.properties)());
        let (code, created) = create_stack(&router, row.stack, &template, &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must answer 200, got {code} {created}"
        );
        let described = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::ROLLBACK_COMPLETE),
            "case {case:?}: expected the refused create to roll back, got {described}"
        );

        let reason =
            failed_resource_reason(case, &router, REGION, row.stack, POOL_ID, CREATE_FAILED).await;
        assert_plugin_worded(case, &reason, row.sentence);
    }
}

#[tokio::test]
async fn the_refusal_of_a_value_outside_the_pool_state_set_names_every_state_a_pool_holds() {
    let case = "the states the refusal names";
    let router = router();
    let stack = "asg-pool-state-set";
    let template = pool_template(&json!({"MinSize": 0, "PoolState": "Terminated"}));
    let (code, created) = create_stack(&router, stack, &template, &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must answer 200, got {code} {created}"
    );
    let described = settled_stack(&router, case, stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::ROLLBACK_COMPLETE),
        "case {case:?}: expected the refused create to roll back, got {described}"
    );
    let reason =
        failed_resource_reason(case, &router, REGION, stack, POOL_ID, CREATE_FAILED).await;
    let (quoted, _) = quoted_and_class(case, &reason);
    // The value the declaration asked for is echoed, and every state a pool may hold its
    // instances in is named — so a client reading the refusal is told what it may ask for.
    assert!(
        quoted.contains("Value 'Terminated'"),
        "case {case:?}: expected the refused value echoed, got {quoted:?}"
    );
    for state in POOL_STATES {
        assert!(
            quoted.contains(state),
            "case {case:?}: expected the refusal to name {state:?}, got {quoted:?}"
        );
    }
}

#[tokio::test]
async fn a_refused_update_reports_the_same_wording_and_leaves_the_deployed_pool_alone() {
    let case = "a deployed pool a revision states outside the set the member admits";
    let stack = "asg-pool-update";
    let router = router();

    let deployed = pool_template(&json!({"MinSize": 0, "PoolState": "Stopped"}));
    let (code, created) = create_stack(&router, stack, &deployed, &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must answer 200, got {code} {created}"
    );
    let described = settled_stack(&router, case, stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::CREATE_COMPLETE),
        "case {case:?}: expected the stack to deploy, got {described}"
    );
    let group = xml_text(&described, "OutputValue")
        .unwrap_or_else(|| panic!("case {case:?}: expected the group's name, got {described}"));
    let pool = reported_pool(case, &router, &group)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: expected a warm pool on {group}"));
    assert_eq!(
        xml_text(&pool, "PoolState").as_deref(),
        Some("Stopped"),
        "case {case:?}: expected the declared pool, got {pool}"
    );

    let revised = pool_template(&json!({"MinSize": 0, "PoolState": "Terminated"}));
    let (code, body) = call(
        &router,
        "UpdateStack",
        &[member("StackName", stack), member("TemplateBody", &revised)],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: UpdateStack must answer 200, got {code} {body}"
    );
    let described = settled_stack(&router, case, stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::UPDATE_ROLLBACK_COMPLETE),
        "case {case:?}: expected the refused update to roll back, got {described}"
    );
    let reason =
        failed_resource_reason(case, &router, REGION, stack, POOL_ID, UPDATE_FAILED).await;
    assert_plugin_worded(case, &reason, "at 'poolState' failed to satisfy constraint");

    // The refused call changed nothing: the pool the stack deployed is still the one that
    // service holds, in the state it was put in.
    let pool = reported_pool(case, &router, &group)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: expected the deployed pool on {group}"));
    assert_eq!(
        xml_text(&pool, "PoolState").as_deref(),
        Some("Stopped"),
        "case {case:?}: expected the deployed pool to be untouched, got {pool}"
    );
}
