// SPDX-License-Identifier: CC0-1.0

//! What a resource AMAZON EC2 AUTO SCALING refused reports on its own failed event, end to end
//! over the awsQuery wire.
//!
//! RULE under test: **a resource provider reports a refused call as the exception its SDK
//! raised — the owning service's own sentence followed by that SDK's diagnostic of the call,
//! naming the service as its SDK names it, the HTTP status that service answered, the id of the
//! call and how many attempts the SDK made at it — whichever call of that service was refused
//! and at whatever stage of the stack's life.**
//!
//! Why it matters: a client reading a failed deployment diagnoses it from the failed resource's
//! `ResourceStatusReason`. The diagnostic is what attributes the failure to the service that
//! refused it and carries the id of the call, which is how the deploy is correlated with that
//! service's own log of it; a reason that stops at the service's sentence leaves a client with
//! nothing to attribute or correlate, and refusal-taxonomy automation that keys on the
//! originating service cannot see the refusal at all.
//!
//! The rows differ along what the rule generalises over: WHICH call of the owning service was
//! refused and WHY — a create whose declared `DeletionProtection` is outside the set that
//! member admits, a create whose declared sizes cannot describe a group, and a DELETE the
//! group's own deletion protection forbids — so the diagnostic is asserted over a creating and
//! a deleting deployment rather than over one recorded failure.

use smithy_server_runtime::MultiServiceRouter;

use serde_json::{json, Value};
use service_com_amazonaws_cloudformation_cloudformation::state::status;

use crate::harness::{
    assert_sdk_diagnostic_with_attempts, failed_resource_reason, quoted_and_class,
};
use crate::stack_lifecycle::{
    autoscaling, call, create_stack, member, router, settled_stack, xml_text, xml_texts, REGION,
};

/// The SDK's own name for Amazon EC2 Auto Scaling, as the exception its SDK raises states it:
/// that model's `awsService.cloudFormationName`.
const AUTO_SCALING_SERVICE: &str = "AutoScaling";

/// The status that service answers a call it will not carry out with — the `httpError` of the
/// fault its awsQuery answer carries.
const REFUSED_STATUS: u16 = 400;

/// The `HandlerErrorCode` class a refusal of the REQUEST falls in, which is what every row
/// here is: one of the model's own `HandlerErrorCode` values.
const INVALID_REQUEST: &str = "InvalidRequest";

/// The statuses a resource records for a create and a delete its provider could not carry out.
const CREATE_FAILED: &str = "CREATE_FAILED";
const DELETE_FAILED: &str = "DELETE_FAILED";

/// The AMI the launch template's data names, and the zone the groups are placed in.
const IMAGE_ID: &str = "ami-00000000000000001";
const ZONE: &str = "us-east-1a";

/// A deletion protection setting that forbids the delete of the group outright: one of the
/// values `DeletionProtection` admits ("valid values are none, prevent-force-deletion, and
/// prevent-all-deletion").
const PREVENT_ALL_DELETION: &str = "prevent-all-deletion";

/// One create the owning service refuses: the stack to deploy and the group declaration that
/// earns the refusal.
struct Row {
    case: &'static str,
    stack: &'static str,
    min_size: u32,
    max_size: u32,
    /// The `DeletionProtection` the declaration states, or `None` where it states none.
    deletion_protection: Option<&'static str>,
    /// A fragment of the owning service's OWN sentence the quoted message must carry, or `None`
    /// for a row whose subject is the diagnostic alone — how that service words a refusal is
    /// its own concern and not something this suite pins.
    sentence: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "a group whose declared DeletionProtection is outside the set the member admits",
        stack: "asg-diag-unknown-protection",
        min_size: 0,
        max_size: 1,
        deletion_protection: Some("prevent-every-deletion"),
        sentence: Some("at 'deletionProtection' failed to satisfy constraint"),
    },
    Row {
        case: "a group whose declared sizes cannot describe a group",
        stack: "asg-diag-sizes",
        min_size: 3,
        max_size: 1,
        deletion_protection: None,
        sentence: None,
    },
];

/// A template declaring one launch template and one group, with the properties the case needs.
fn group_template(group: &str, properties: Value) -> String {
    let mut declared = json!({
        "AutoScalingGroupName": group,
        "AvailabilityZones": [ZONE],
        "LaunchTemplate": {
            "LaunchTemplateId": {"Ref": "Lt"},
            "Version": {"Fn::GetAtt": ["Lt", "LatestVersionNumber"]},
        },
    });
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
            "Asg": {
                "Type": "AWS::AutoScaling::AutoScalingGroup",
                "Properties": declared,
            },
        },
    })
    .to_string()
}

/// The row's template: the sizes it declares, and its deletion protection where it states one.
fn row_template(row: &Row) -> String {
    let mut properties = json!({
        "MinSize": row.min_size.to_string(),
        "MaxSize": row.max_size.to_string(),
    });
    if let Some(protection) = row.deletion_protection {
        properties["DeletionProtection"] = json!(protection);
    }
    group_template(&format!("{}-group", row.stack), properties)
}

/// Hold one quoted exception to the diagnostic Amazon EC2 Auto Scaling's SDK appends: that
/// service's own sentence, then the service as its SDK names it, the status it answered, the id
/// of the call and the attempts the SDK made — each of the FORM that SDK words rather than one
/// recorded value.
///
/// The whole reading is the shape every SDK that counts its attempts words, so it is held to
/// the one assertion this target has for it
/// ([`assert_sdk_diagnostic_with_attempts`]) under this service's own name and status.
fn assert_auto_scaling_diagnostic(case: &str, quoted: &str) {
    assert_sdk_diagnostic_with_attempts(case, quoted, AUTO_SCALING_SERVICE, REFUSED_STATUS);
}

/// The group names Amazon EC2 Auto Scaling answers for the one it is asked about.
async fn groups_named(case: &str, router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let (status_code, body) = autoscaling(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: DescribeAutoScalingGroups({group}) must answer 200, got {status_code} \
         {body}"
    );
    xml_texts(&body, "AutoScalingGroupName")
}

#[tokio::test]
async fn a_refused_create_reports_the_sdk_exception_of_the_call() {
    for row in ROWS {
        let case = row.case;
        let router = router();
        let (status_code, created) = create_stack(&router, row.stack, &row_template(row), &[]).await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: CreateStack must answer 200, got {status_code} {created}"
        );
        let described = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::ROLLBACK_COMPLETE),
            "case {case:?}: expected the refused create to roll back, got {described}"
        );

        let reason =
            failed_resource_reason(case, &router, REGION, row.stack, "Asg", CREATE_FAILED).await;
        let (quoted, class) = quoted_and_class(case, &reason);
        if let Some(sentence) = row.sentence {
            assert!(
                quoted.contains(sentence),
                "case {case:?}: expected the owning service's own sentence carrying \
                 {sentence:?}, got {quoted:?}"
            );
        }
        assert_eq!(
            class, INVALID_REQUEST,
            "case {case:?}: expected the {INVALID_REQUEST} class, got {class:?} in {reason:?}"
        );
        assert_auto_scaling_diagnostic(case, &quoted);
    }
}

#[tokio::test]
async fn a_delete_the_owning_service_forbids_reports_the_same_exception_until_it_is_cleared() {
    let case = "a group whose deletion protection forbids the delete";
    let stack = "asg-diag-protected";
    let group = "asg-diag-protected-group";
    let router = router();
    let protected = group_template(
        group,
        json!({
            "MinSize": "0",
            "MaxSize": "1",
            "DeletionProtection": PREVENT_ALL_DELETION,
        }),
    );
    let (status_code, created) = create_stack(&router, stack, &protected, &[]).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: CreateStack must answer 200, got {status_code} {created}"
    );
    let described = settled_stack(&router, case, stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::CREATE_COMPLETE),
        "case {case:?}: expected the stack to deploy, got {described}"
    );
    // Addressed by stack id from here on, since a completed deletion leaves the live set.
    let stack_id = xml_text(&described, "StackId").unwrap_or_default();

    let (status_code, body) = call(&router, "DeleteStack", &[member("StackName", stack)]).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: DeleteStack must be accepted, got {status_code} {body}"
    );
    let reason =
        failed_resource_reason(case, &router, REGION, &stack_id, "Asg", DELETE_FAILED).await;
    let (quoted, class) = quoted_and_class(case, &reason);
    assert!(
        quoted.contains(PREVENT_ALL_DELETION),
        "case {case:?}: expected the owning service's sentence naming the protection that \
         forbids the delete, got {quoted:?}"
    );
    assert_eq!(
        class, INVALID_REQUEST,
        "case {case:?}: expected the {INVALID_REQUEST} class, got {class:?} in {reason:?}"
    );
    assert_auto_scaling_diagnostic(case, &quoted);

    // The refusal was real: the group is still registered in the owning service.
    assert_eq!(
        groups_named(case, &router, group).await,
        vec![group.to_string()],
        "case {case:?}: expected the protected group to survive the refused delete"
    );

    // ...and once the protection is CLEARED on the group itself, the same deletion goes
    // through and that service no longer holds the group: the refusal was the group's state
    // rather than anything the stack could not do.
    let (status_code, body) = autoscaling(
        &router,
        "UpdateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", group),
            ("DeletionProtection", "none"),
        ],
    )
    .await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: clearing the protection must answer 200, got {status_code} {body}"
    );

    let (status_code, body) = call(
        &router,
        "DeleteStack",
        &[member("StackName", &stack_id)],
    )
    .await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: the second DeleteStack must be accepted, got {status_code} {body}"
    );
    let described = settled_stack(&router, case, &stack_id).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::DELETE_COMPLETE),
        "case {case:?}: expected the deletion to complete once the protection is cleared, got \
         {described}"
    );
    assert!(
        groups_named(case, &router, group).await.is_empty(),
        "case {case:?}: expected the group to be gone once the delete went through"
    );
}
