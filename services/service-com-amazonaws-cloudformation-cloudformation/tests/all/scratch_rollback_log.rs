// SPDX-License-Identifier: CC0-1.0

//! What the STACK reports when a wait of its own ran out, end to end over the awsQuery wire.
//!
//! RULE under test: **the reason a STACK-level event carries for a failure of the stack's own
//! RESOURCES is the summary naming those resources — "The following resource(s) failed to
//! <action>: [<logical ids>]. " — and never one resource's own diagnosis, which is a
//! per-resource field: `StackEvent$ResourceStatusReason` is the "success/failure message
//! associated with the resource", so the sentence a wait's expiry is worded with belongs to
//! the event of the resource the wait was for. A wait that ran out is no different from a
//! call an owning service refused: the stack names the resources, the resources carry the
//! sentence.**
//!
//! Why it matters: that stack-level sentence is the one field a client parses to recover WHICH
//! resources a failed operation could not carry through — the first thing deployment tooling
//! reports and the only place a multi-resource failure is enumerated. Filled with one
//! resource's own message, a client cannot tell which resources failed, and a stack whose
//! wait ran out reads differently from one whose resource an owning service refused although
//! both failed on their resources.
//!
//! The rows differ along what the rule generalises over: HOW MANY resources the expiry failed
//! (one and two, which is what the comma-separated list is for), which declaration opened the
//! wait (an `AWS::CloudFormation::WaitCondition` and a `CreationPolicy`'s `ResourceSignal`,
//! whose own diagnoses are worded differently), and which OPERATION ran out — a create that
//! is rolled back, where the summary is carried by the event that opens the rollback, and an
//! update that keeps its failed state, where the stack rests in the failed status and that
//! status's own event carries it.

use std::time::Duration;

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::stack_lifecycle::{
    call, create_stack, member, router, wait_for_the_transition, xml_blocks, xml_text,
};
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

/// How long a row gives a stack operation to settle, and how often it looks: the expiry is
/// the CLOCK's and the status the failure owes falls due a transition later, so a row waits
/// for the status to settle rather than for a span to pass. Generous: the only limit a row is
/// about is the one its own declaration states.
const POLL_ATTEMPTS: usize = 600;
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// One row of the create table: the template, the resources the expiry fails, and the
/// diagnosis each of those resources carries. Every row's declaration asks for a one-second
/// span, which the clock is what makes due.
struct Expired {
    case: &'static str,
    stack: &'static str,
    template: String,
    /// The logical ids the stack-level summary must name, in the order it names them.
    failed: &'static [&'static str],
    /// The sentence the failing resources' OWN events carry — the wording CloudFormation
    /// gives that declaration's expiry.
    diagnosis: &'static str,
}

fn expired_creates() -> Vec<Expired> {
    vec![
        Expired {
            case: "one wait condition that is never signalled",
            stack: "summary-one-wait",
            failed: &["Wait"],
            diagnosis: "WaitCondition timed out. Received 0 conditions when expecting 1",
            template: json!({
                "Resources": {
                    "Handle": {"Type": "AWS::CloudFormation::WaitConditionHandle"},
                    "Wait": {
                        "Type": "AWS::CloudFormation::WaitCondition",
                        "Properties": {
                            "Handle": {"Ref": "Handle"},
                            "Timeout": "1",
                            "Count": "1",
                        },
                    },
                },
            })
            .to_string(),
        },
        // TWO waits run out in the same operation, which is what the comma-separated list is
        // for: the summary names both, in the order the stack declared them.
        Expired {
            case: "two wait conditions that are never signalled",
            stack: "summary-two-waits",
            failed: &["Fast", "Slow"],
            diagnosis: "WaitCondition timed out. Received 0 conditions when expecting 1",
            template: json!({
                "Resources": {
                    "Handle": {"Type": "AWS::CloudFormation::WaitConditionHandle"},
                    "Fast": {
                        "Type": "AWS::CloudFormation::WaitCondition",
                        "Properties": {
                            "Handle": {"Ref": "Handle"},
                            "Timeout": "1",
                            "Count": "1",
                        },
                    },
                    "Slow": {
                        "Type": "AWS::CloudFormation::WaitCondition",
                        "Properties": {
                            "Handle": {"Ref": "Handle"},
                            "Timeout": "1",
                            "Count": "1",
                        },
                    },
                },
            })
            .to_string(),
        },
        // A `CreationPolicy`'s `ResourceSignal`, whose expiry is worded differently on the
        // resource: the summary is the same sentence either way, because what it reports is
        // WHICH resources failed rather than why.
        Expired {
            case: "a CreationPolicy's resource signal that never arrives",
            stack: "summary-creation-policy",
            failed: &["Gated"],
            diagnosis: "Failed to receive 1 resource signal(s) within the specified duration",
            template: json!({
                "Resources": {
                    "Gated": {
                        "Type": "AWS::SSM::Parameter",
                        "CreationPolicy": {
                            "ResourceSignal": {"Count": 1, "Timeout": "PT1S"},
                        },
                        "Properties": {
                            "Name": "/summary-creation-policy/gated",
                            "Type": "String",
                            "Value": "waits-for-a-signal",
                        },
                    },
                },
            })
            .to_string(),
        },
    ]
}

/// The sentence a stack reports for the failure of the resources `failed` names, as
/// CloudFormation words it.
fn summary(action: &str, failed: &[&str]) -> String {
    format!(
        "The following resource(s) failed to {action}: [{}]. ",
        failed.join(", ")
    )
}

/// One stack's whole event log as `(logical id, status, reason)`, oldest first.
async fn events(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<(String, String, String)> {
    let (code, body) = call(
        router,
        "DescribeStackEvents",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStackEvents({stack}) must answer 200, got {code} {body}"
    );
    let mut log: Vec<(String, String, String)> = xml_blocks(&body, "member")
        .iter()
        .map(|entry| {
            (
                xml_text(entry, "LogicalResourceId").unwrap_or_default(),
                xml_text(entry, "ResourceStatus").unwrap_or_default(),
                xml_text(entry, "ResourceStatusReason").unwrap_or_default(),
            )
        })
        .collect();
    log.reverse();
    log
}

/// One stack's reported status and the reason it carries.
async fn described(router: &MultiServiceRouter, case: &str, stack: &str) -> (String, String) {
    let (code, body) = call(router, "DescribeStacks", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStacks({stack}) must answer 200, got {code} {body}"
    );
    (
        xml_text(&body, "StackStatus").unwrap_or_default(),
        xml_text(&body, "StackStatusReason").unwrap_or_default(),
    )
}

/// Look at one stack until the operation in flight has SETTLED — no `*_IN_PROGRESS` status
/// left — and answer the status it settled in with the reason it carries.
async fn settled(router: &MultiServiceRouter, case: &str, stack: &str) -> (String, String) {
    let mut reported = (String::new(), String::new());
    for _ in 0..POLL_ATTEMPTS {
        reported = described(router, case, stack).await;
        if !reported.0.ends_with("_IN_PROGRESS") {
            return reported;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    reported
}

#[tokio::test]
async fn a_create_whose_waits_run_out_reports_the_summary_naming_the_resources() {
    for row in expired_creates() {
        let case = row.case;
        let router = router();
        let (code, body) = create_stack(&router, row.stack, &row.template, &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must be accepted, got {code} {body}"
        );
        let (reported, stack_reason) = settled(&router, case, row.stack).await;
        assert_eq!(
            reported,
            status::ROLLBACK_COMPLETE,
            "case {case:?}: expected the create whose wait ran out to roll back, got \
             {reported:?}"
        );

        // The stack's own report names the resources, under the action that did not go
        // through — the create — with the clause saying the rollback was asked for.
        let expected = summary("create", row.failed);
        assert!(
            stack_reason.starts_with(&expected),
            "case {case:?}: expected the stack to report {expected:?}, got {stack_reason:?}"
        );
        let log = events(&router, case, row.stack).await;
        let opened_the_rollback: Vec<&(String, String, String)> = log
            .iter()
            .filter(|(logical_id, reported, _)| {
                logical_id == row.stack && reported == status::ROLLBACK_IN_PROGRESS
            })
            .collect();
        assert_eq!(
            opened_the_rollback.len(),
            1,
            "case {case:?}: expected one event to open the rollback, got {log:?}"
        );
        assert!(
            opened_the_rollback[0].2.starts_with(&expected),
            "case {case:?}: expected the rollback's own event to report {expected:?}, got {:?}",
            opened_the_rollback[0].2
        );

        // ...and the diagnosis of each failure is carried by the resource it is about, on no
        // event of the stack's.
        for failed in row.failed {
            let reported: Vec<&(String, String, String)> = log
                .iter()
                .filter(|(logical_id, reported, _)| {
                    logical_id == failed && reported == resource_status::CREATE_FAILED
                })
                .collect();
            assert_eq!(
                reported.len(),
                1,
                "case {case:?}: expected {failed:?} to report its own failure once, got {log:?}"
            );
            assert_eq!(
                reported[0].2, row.diagnosis,
                "case {case:?}: expected {failed:?} to carry {:?}, got {:?}",
                row.diagnosis, reported[0].2
            );
        }
        let stack_level_diagnosis: Vec<&(String, String, String)> = log
            .iter()
            .filter(|(logical_id, _, reason)| {
                logical_id == row.stack && reason.contains(row.diagnosis)
            })
            .collect();
        assert!(
            stack_level_diagnosis.is_empty(),
            "case {case:?}: expected no stack-level event to carry a resource's own \
             diagnosis, got {stack_level_diagnosis:?}"
        );
    }
}

/// The stack an update row starts from: one handle, which the update below does not touch.
const BEFORE_THE_UPDATE: &str = r#"{"Resources":{
    "Handle":{"Type":"AWS::CloudFormation::WaitConditionHandle"}}}"#;

/// What the update deploys: the same handle, plus a wait condition nothing signals.
const THE_FAILING_UPDATE: &str = r#"{"Resources":{
    "Handle":{"Type":"AWS::CloudFormation::WaitConditionHandle"},
    "Fast":{"Type":"AWS::CloudFormation::WaitCondition",
      "Properties":{"Handle":{"Ref":"Handle"},"Timeout":"1","Count":"1"}}}}"#;

#[tokio::test]
async fn an_update_that_keeps_its_failed_state_reports_the_summary_on_that_status() {
    let case = "an update that adds a resource behind a wait nothing signals";
    let stack = "summary-failed-update";
    let router = router();
    let (code, body) = create_stack(&router, stack, BEFORE_THE_UPDATE, &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must be accepted, got {code} {body}"
    );
    wait_for_the_transition().await;

    // "Preserve the state of previously provisioned resources when an operation fails"
    // (`UpdateStackInput$DisableRollback`): the stack RESTS in the failed status, so that
    // status's own event is the one that carries the summary.
    let (code, body) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", stack),
            member("TemplateBody", THE_FAILING_UPDATE),
            member("DisableRollback", "true"),
            member("ClientRequestToken", "summary-update-token"),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: UpdateStack must be accepted, got {code} {body}"
    );
    let (reported, stack_reason) = settled(&router, case, stack).await;
    assert_eq!(
        reported,
        status::UPDATE_FAILED,
        "case {case:?}: expected the update to keep its failed state, got {reported:?}"
    );
    let expected = summary("create", &["Fast"]);
    assert_eq!(
        stack_reason, expected,
        "case {case:?}: expected the stack to report {expected:?}, got {stack_reason:?}"
    );

    let log = events(&router, case, stack).await;
    let rested: Vec<&(String, String, String)> = log
        .iter()
        .filter(|(logical_id, reported, _)| {
            logical_id == stack && reported == status::UPDATE_FAILED
        })
        .collect();
    assert_eq!(
        rested.len(),
        1,
        "case {case:?}: expected one stack-level UPDATE_FAILED event, got {log:?}"
    );
    assert_eq!(
        rested[0].2, expected,
        "case {case:?}: expected that event to report {expected:?}, got {:?}",
        rested[0].2
    );
    // The resource the update could not create carries the diagnosis, and reports the
    // failure of its own CREATE: the stack did not have it before this update.
    let diagnosis = "WaitCondition timed out. Received 0 conditions when expecting 1";
    let failed: Vec<&(String, String, String)> = log
        .iter()
        .filter(|(logical_id, reported, _)| {
            logical_id == "Fast" && reported == resource_status::CREATE_FAILED
        })
        .collect();
    assert_eq!(
        failed.len(),
        1,
        "case {case:?}: expected the added resource to report its own failure, got {log:?}"
    );
    assert_eq!(
        failed[0].2, diagnosis,
        "case {case:?}: expected {diagnosis:?} on the resource's own event, got {:?}",
        failed[0].2
    );
}
