// SPDX-License-Identifier: CC0-1.0

//! What a stack SAYS when the provider of a custom resource reached over an Amazon SNS topic
//! answers its `Create` with `FAILED`: the sentence the failed resource's own event carries,
//! the sentence the rollback that failure causes carries, and what the settled stack
//! publishes afterwards.
//!
//! RULE under test: **a custom resource whose provider ANSWERS `FAILED` takes
//! `CREATE_FAILED` under CloudFormation's answered-failure sentence — `Received response
//! status [FAILED] from custom resource. Message returned: <the message that answer returned>
//! (RequestId: <the id of the request it answered>)`, whose only variable parts are that
//! message (the provider's own `Reason`, or this service's sentence for an answer stating
//! none) and the id of the request it answered — and the create it failed is rolled back
//! under the aggregate naming the resources the operation could not create, `The following
//! resource(s) failed to create: [<those logical ids>]. Rollback requested by user.`, so the
//! resource is unmade again (its own `DELETE_COMPLETE` recorded strictly after its failure)
//! and the stack settles `ROLLBACK_COMPLETE` publishing NO output at all, however many of the
//! template's outputs read the failed resource.**
//!
//! Three crates have to agree for that to hold — CloudFormation publishes the request, reads
//! the answer and records the rollback, Amazon SNS delivers the publish to the subscribed
//! endpoint, and Amazon SQS holds it for the provider to read — so the suite lives here. The
//! provider harness is [`super::cloudformation_sns_custom_resource_protocol`]'s and the event
//! reader is [`super::cloudformation_records_per_resource_stack_events`]'s: nothing here is a
//! second copy of either. What no sibling suite pins, and this one does, is the WORDING a
//! topic-backed provider's refusal leaves behind — the sibling
//! [`super::cloudformation_custom_resource_answered_identity`] pins the IDENTITY a `FAILED`
//! answer leaves (the id its events and its rollback's `Delete` carry) and
//! [`super::cloudformation_sns_custom_resource_protocol`] pins WHICH requests that answer
//! owes, neither of them what either sentence reads.
//!
//! The rows differ along what the rule generalises over: the message the answer returns (a
//! clause, a diagnostic carrying its own punctuation and parentheses, and NOTHING at all,
//! which is the service's own sentence for an unstated reason), how the custom resource's
//! type is spelled (an author's own `Custom::<Name>` and the generic
//! `AWS::CloudFormation::CustomResource`), the logical id the aggregate has to name, and how
//! many of the template's outputs read the resource that failed (one and two).
//!
//! Every expected value here is the row's own or this run's: the only part of either sentence
//! not written out below is the `RequestId`, which is whatever this run's CloudFormation
//! minted for the request the provider answered. Nothing sleeps a span on a service's behalf
//! — every wait polls for the CONDITION under a deadline far longer than the spans these
//! services choose — and nothing asserts on elapsed time.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::cloudformation_records_per_resource_stack_events::{wire_events, WireEvent};
use super::cloudformation_sns_custom_resource_protocol::{
    answer, answered_success, build_router, cloudformation, create_stack, next_request, provider,
    settled_with_a_responsive_provider, ROLLBACK_COMPLETE,
};
use super::query_wire::outputs_of;

/// How long this suite waits for something the emulator owes — an event becoming readable, a
/// stack settling — and how often it looks while waiting. Generous: what is asserted is that
/// the record appears at all, never how fast.
const WAIT: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(50);

/// The statuses the record is read under.
const CREATE_FAILED: &str = "CREATE_FAILED";
const DELETE_COMPLETE: &str = "DELETE_COMPLETE";
const ROLLBACK_IN_PROGRESS: &str = "ROLLBACK_IN_PROGRESS";

/// The type CloudFormation reports a stack's OWN events under, which is what separates the
/// stack-level rows of `DescribeStackEvents` from its resources'.
const STACK_RESOURCE_TYPE: &str = "AWS::CloudFormation::Stack";

/// The `Status` a provider answers with when it did not carry the request out.
const ANSWER_FAILED: &str = "FAILED";

/// What CloudFormation reports as the message of a failure whose answer stated no `Reason` of
/// its own. Spelled out here rather than imported from the code that writes it: an
/// expectation taken from the implementation's own constant could not fail when that constant
/// changes, which is one of the things this suite is for.
const UNSTATED_REASON: &str = "The custom resource provider reported FAILED";

/// The sentence CloudFormation reports a custom resource whose provider ANSWERED `FAILED`
/// with: its fixed text, whose only variable parts are the message that answer returned and
/// the `RequestId` of the request it answered. Written out here for the same reason
/// [`UNSTATED_REASON`] is.
fn answered_failure_reason(message: &str, request_id: &str) -> String {
    format!(
        "Received response status [FAILED] from custom resource. Message returned: {message} \
         (RequestId: {request_id})"
    )
}

/// The sentence CloudFormation rolls a failed create back under: the aggregate naming every
/// resource the operation could not create, then the clause saying the rollback was asked for
/// (`CreateStackInput$OnFailure` documents `ROLLBACK` as the default). Written out here for
/// the same reason.
fn rollback_reason(failed_logical_ids: &str) -> String {
    format!(
        "The following resource(s) failed to create: [{failed_logical_ids}]. Rollback requested \
         by user."
    )
}

/// One row: how the custom resource is declared, what its provider answers, and what the
/// template publishes about it.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The logical id the resource is declared under — what the aggregate has to name.
    logical_id: &'static str,
    /// How the type is spelled.
    resource_type: &'static str,
    /// The `Reason` the `FAILED` answer states, or `None` where it states none at all.
    reason: Option<&'static str>,
    /// The `PhysicalResourceId` that answer carries, which the failure is recorded under.
    answered_id: &'static str,
    /// The attributes the template's outputs read off the custom resource with `Fn::GetAtt`,
    /// none of which a failed create can resolve.
    reads: &'static [&'static str],
}

/// The message the stack has to report for a row: the provider's own `Reason` where it stated
/// one, and this service's sentence for an unstated reason where it did not.
fn message_of(row: &Row) -> &'static str {
    row.reason.unwrap_or(UNSTATED_REASON)
}

/// The rows the answered-failure SENTENCE is read over: the message the answer returns, and
/// how the type is spelled.
const MESSAGES: &[Row] = &[
    Row {
        case: "an answer whose Reason is a plain clause",
        stack: "cr-topic-failed-clause",
        logical_id: "Thing",
        resource_type: "Custom::Verdict",
        reason: Some("the provider could not reach the backend it provisions"),
        answered_id: "verdict-instance-clause",
        reads: &["Greeting"],
    },
    Row {
        case: "an answer whose Reason carries its own punctuation and parentheses",
        stack: "cr-topic-failed-punctuated",
        logical_id: "Verdict",
        resource_type: "AWS::CloudFormation::CustomResource",
        reason: Some("provisioning stopped at step 2 (quota exhausted). Retry later."),
        answered_id: "arn:aws:service:us-east-1:000000000000:verdict/punctuated",
        reads: &["Greeting", "Endpoint"],
    },
    Row {
        case: "an answer that states no Reason at all",
        stack: "cr-topic-failed-unstated",
        logical_id: "Probe",
        resource_type: "Custom::Verdict",
        reason: None,
        answered_id: "verdict-instance-unstated",
        reads: &["Greeting"],
    },
];

/// The rows the ROLLBACK record is read over: the logical id the aggregate names, and how many
/// of the template's outputs read the resource that failed.
const RECORDS: &[Row] = &[
    Row {
        case: "one output reading the resource that failed",
        stack: "cr-topic-rollback-one-output",
        logical_id: "Thing",
        resource_type: "Custom::Verdict",
        reason: Some("the provider refused the create"),
        answered_id: "verdict-rollback-1",
        reads: &["Greeting"],
    },
    Row {
        case: "two outputs reading the resource that failed, under another logical id",
        stack: "cr-topic-rollback-two-outputs",
        logical_id: "Provisioned",
        resource_type: "AWS::CloudFormation::CustomResource",
        reason: None,
        answered_id: "arn:aws:service:us-east-1:000000000000:verdict/rollback-2",
        reads: &["Greeting", "Endpoint"],
    },
];

/// A template with one custom resource of the provider `token`, publishing one output per
/// attribute the row reads off it — the outputs a create that failed resolves none of.
fn template(row: &Row, token: &str) -> String {
    let outputs: Map<String, Value> = row
        .reads
        .iter()
        .map(|key| {
            (
                format!("{key}Output"),
                json!({ "Value": { "Fn::GetAtt": [row.logical_id, key] } }),
            )
        })
        .collect();
    json!({
        "Resources": {
            row.logical_id: {
                "Type": row.resource_type,
                "Properties": {
                    "ServiceToken": token,
                    "Marker": row.stack,
                },
            },
        },
        "Outputs": outputs,
    })
    .to_string()
}

/// What one row's rolled-back create left behind.
struct Rolled {
    /// The `RequestId` of the `Create` request the provider answered — the one variable part
    /// of the failure's sentence that is this run's rather than the row's.
    request_id: String,
    /// Every event the stack recorded, OLDEST first: the order it went through them, which is
    /// the order a client reconstructing the deploy reads.
    events: Vec<WireEvent>,
    /// What the settled stack publishes.
    outputs: BTreeMap<String, String>,
}

/// Deploy one row and let its rollback conclude: the stack is created, the `Create` request is
/// read off the provider's own queue, answered `FAILED` as the row says, the `Delete` that
/// failure owes is answered, and the stack is polled until it settles — then the record is
/// read off the wires a client has.
async fn rolled_back(row: &Row) -> Rolled {
    let case = row.case;
    let router = build_router();
    let mut provider = provider(&router, row.stack).await;
    create_stack(&router, case, row.stack, &template(row, &provider.topic_arn)).await;

    let created = next_request(&router, &mut provider, case, "the Create").await;
    assert_eq!(
        created["RequestType"].as_str(),
        Some("Create"),
        "case {case:?}: expected the first request to be a Create, got {created}"
    );
    let request_id = created["RequestId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: every request carries a RequestId: {created}"))
        .to_string();

    let mut document = json!({
        "Status": ANSWER_FAILED,
        "PhysicalResourceId": row.answered_id,
    });
    if let Some(reason) = row.reason {
        document["Reason"] = json!(reason);
    }
    let (code, body) = answer(&router, &created, &document).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the provider's answer must be accepted, got {code} with {body}"
    );

    // The rollback's own `Delete` is CloudFormation's to publish, with nothing calling it in
    // between, and a provider that carries it out is what lets the rollback conclude.
    let deleted = next_request(&router, &mut provider, case, "the rollback's Delete").await;
    assert_eq!(
        (
            deleted["RequestType"].as_str(),
            deleted["PhysicalResourceId"].as_str(),
        ),
        (Some("Delete"), Some(row.answered_id)),
        "case {case:?}: expected the rollback to delete the id the answer carried, got {deleted}"
    );
    answered_success(&router, case, &deleted, row.answered_id, &Value::Null).await;

    let settled = settled_with_a_responsive_provider(
        &router,
        &mut provider,
        case,
        row.stack,
        ROLLBACK_COMPLETE,
    )
    .await;
    assert_eq!(
        settled, ROLLBACK_COMPLETE,
        "case {case:?}: expected the failed create to settle at {ROLLBACK_COMPLETE}, got \
         {settled:?}"
    );

    let events = recorded_events(&router, row).await;
    Rolled {
        request_id,
        events,
        outputs: published_outputs(&router, row.stack).await,
    }
}

/// Every event the stack has recorded, OLDEST first, read by LOOKING again until the row's own
/// resource reports both the failure and the deletion that unmade it — a stack's events become
/// readable as its own clock reaches them, so the read waits for the CONDITION.
async fn recorded_events(router: &MultiServiceRouter, row: &Row) -> Vec<WireEvent> {
    let looks = WAIT.as_millis() / LOOK.as_millis();
    let mut events = Vec::new();
    for _ in 0..looks {
        let (code, body) =
            cloudformation(router, "DescribeStackEvents", &[("StackName", row.stack)]).await;
        assert_eq!(
            code, 200,
            "DescribeStackEvents({:?}) must be answered, got {code} with {body}",
            row.stack
        );
        events = wire_events(&body);
        events.reverse();
        let reported = |status: &str| {
            events
                .iter()
                .any(|event| event.logical_id == row.logical_id && event.status == status)
        };
        if reported(CREATE_FAILED) && reported(DELETE_COMPLETE) {
            return events;
        }
        tokio::time::sleep(LOOK).await;
    }
    events
}

/// What one stack publishes now, as the key/value map a client reads.
async fn published_outputs(router: &MultiServiceRouter, stack: &str) -> BTreeMap<String, String> {
    let (code, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "DescribeStacks({stack:?}) must be answered, got {code} with {body}"
    );
    outputs_of(&body)
}

/// The statuses one resource's events report, for an assertion's own diagnostic.
fn statuses_of(events: &[WireEvent], logical_id: &str) -> Vec<String> {
    events
        .iter()
        .filter(|event| event.logical_id == logical_id)
        .map(|event| event.status.clone())
        .collect()
}

/// Every row: the resource's failure reads as the answered-failure sentence — the message the
/// answer returned, under the id of the request it answered — and is recorded against the
/// physical resource that answer named.
#[tokio::test]
async fn a_topic_backed_providers_failed_answer_reads_as_the_answered_failure_sentence() {
    for row in MESSAGES {
        let case = row.case;
        let rolled = rolled_back(row).await;
        let expected = answered_failure_reason(message_of(row), &rolled.request_id);
        let failures: Vec<&WireEvent> = rolled
            .events
            .iter()
            .filter(|event| event.logical_id == row.logical_id && event.status == CREATE_FAILED)
            .collect();
        assert!(
            !failures.is_empty(),
            "case {case:?}: expected a {CREATE_FAILED} event for {}, got the statuses {:?}",
            row.logical_id,
            statuses_of(&rolled.events, row.logical_id)
        );
        for failure in failures {
            assert_eq!(
                failure.status_reason.as_deref(),
                Some(expected.as_str()),
                "case {case:?}: expected the failure to read {expected:?}, got {:?}",
                failure.status_reason
            );
            assert_eq!(
                failure.physical_id.as_deref(),
                Some(row.answered_id),
                "case {case:?}: expected the failure to be recorded against {:?}, got {:?}",
                row.answered_id,
                failure.physical_id
            );
        }
    }
}

/// Every row: the rollback the failure causes names the resource that failed in its own
/// sentence, the resource is unmade again strictly after it failed, and the settled stack
/// publishes nothing — none of the outputs that read the resource whose create failed.
#[tokio::test]
async fn the_rollback_names_the_failed_resource_and_the_settled_stack_publishes_nothing() {
    for row in RECORDS {
        let case = row.case;
        let rolled = rolled_back(row).await;

        let expected = rollback_reason(row.logical_id);
        let rollbacks: Vec<&WireEvent> = rolled
            .events
            .iter()
            .filter(|event| {
                event.resource_type == STACK_RESOURCE_TYPE && event.status == ROLLBACK_IN_PROGRESS
            })
            .collect();
        assert!(
            !rollbacks.is_empty(),
            "case {case:?}: expected a stack-level {ROLLBACK_IN_PROGRESS} event, got the \
             statuses {:?}",
            rolled
                .events
                .iter()
                .map(|event| event.status.clone())
                .collect::<Vec<String>>()
        );
        for rollback in rollbacks {
            assert_eq!(
                rollback.status_reason.as_deref(),
                Some(expected.as_str()),
                "case {case:?}: expected the rollback to read {expected:?}, got {:?}",
                rollback.status_reason
            );
        }

        // The resource the create had failed is unmade AFTER it failed: a record that reports
        // both, but not in that sequence, cannot be told from one where the rollback ran before
        // the provider was ever asked.
        let position = |status: &str| {
            rolled
                .events
                .iter()
                .position(|event| event.logical_id == row.logical_id && event.status == status)
        };
        let failed_at = position(CREATE_FAILED);
        let deleted_at = position(DELETE_COMPLETE);
        assert!(
            matches!((failed_at, deleted_at), (Some(failed), Some(deleted)) if failed < deleted),
            "case {case:?}: expected {CREATE_FAILED} then {DELETE_COMPLETE} for {}, got the \
             statuses {:?}",
            row.logical_id,
            statuses_of(&rolled.events, row.logical_id)
        );

        assert!(
            rolled.outputs.is_empty(),
            "case {case:?}: a rolled-back create resolves none of its {} output(s), got {:?}",
            row.reads.len(),
            rolled.outputs
        );
    }
}
