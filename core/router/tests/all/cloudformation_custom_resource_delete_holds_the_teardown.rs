// SPDX-License-Identifier: CC0-1.0

//! A stack DELETE whose custom resource is removed by its PROVIDER: what the resources of
//! that stack report while the provider has not answered, and what becomes of them when it
//! does.
//!
//! RULE under test: **a stack's teardown is driven through the provider of every custom
//! resource it removes — the `Delete` request goes to the resource's `ServiceToken` and the
//! removal is not carried out until the provider answers at that request's `ResponseURL` —
//! and while such a removal is outstanding the resource reports `DELETE_IN_PROGRESS` while
//! every resource the teardown is HOLDING BACK behind it is left exactly as it is: it reports
//! the status its create left and the service that owns it still serves it. A provider answer
//! of `Status` `FAILED` fails the removal: the custom resource reports `DELETE_FAILED`, the
//! stack settles `DELETE_FAILED`, and the resources that removal was holding back are never
//! deleted at all, while a `SUCCESS` lets the teardown carry on and take them down.**
//!
//! "CloudFormation deletes the resources in reverse order", so the resource a custom resource
//! DEPENDS ON is removed AFTER it: that is the edge these rows are built around, and it is
//! what makes the queues observable through Amazon SQS's own wire while the provider has not
//! answered. Three crates have to agree for any of it to hold — CloudFormation publishes the
//! request and holds the removal, Amazon SNS delivers the publish to the subscribed endpoint,
//! and Amazon SQS both holds the request for the provider and owns the queues the stack
//! declares — so the suite lives here, over the assembled router, and the provider is stood
//! up by the sibling suite's own spelling of it
//! ([`super::cloudformation_sns_custom_resource_protocol`]).
//!
//! The rows differ along what the rule generalises over: the TYPE the custom resource is
//! declared with (`Custom::<Name>` and the modelled `AWS::CloudFormation::CustomResource`),
//! how many resources the removal holds back behind it (one, and a chain of two), the
//! `PhysicalResourceId` the provider minted, and the `Status` the provider answers the
//! `Delete` with — the two terminals a removal has, the refused one then retried.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::cloudformation_sns_custom_resource_protocol::{
    answer, answered_success, build_router, cloudformation, create_stack, next_request, provider,
    stack_status, CREATE_COMPLETE,
};
use super::query_wire::{element, opt_element};

/// The Region every request here is signed for, and so the Region the stacks belong to.
const REGION: &str = "us-east-1";

/// How long an observation of work the emulator owes — a stack settling, a request being
/// published — goes on looking before this suite calls it a hang, and how long it hands the
/// runtime back between two looks.
///
/// A generous deadline over a loop that waits for the CONDITION: a row fails only when the work
/// did not happen at all, never because the host was busy. Nothing here asserts how long
/// anything took.
const BUDGET: Duration = Duration::from_secs(60);
/// See [`BUDGET`].
const LOOK: Duration = Duration::from_millis(25);

/// The logical id every row declares its custom resource under.
const GATE: &str = "Gate";

/// The statuses the rows are stated in.
const CREATE_COMPLETE_RESOURCE: &str = "CREATE_COMPLETE";
const DELETE_IN_PROGRESS: &str = "DELETE_IN_PROGRESS";
const DELETE_COMPLETE: &str = "DELETE_COMPLETE";
const DELETE_FAILED: &str = "DELETE_FAILED";

/// One row: the custom resource's declared type, the id its provider mints, the queues the
/// removal of that resource holds back (declared oldest first, each depending on the one
/// before it), the `Status` the provider answers the `Delete` with, and what the stack and
/// those queues are then left in.
struct Row {
    case: &'static str,
    slug: &'static str,
    resource_type: &'static str,
    minted_id: &'static str,
    /// The logical ids of the queues the custom resource DEPENDS ON — so the teardown reaches
    /// them only once its removal of the custom resource has been carried out.
    held_back: &'static [&'static str],
    /// What the provider answers the `Delete` with.
    answered_status: &'static str,
    /// The status the stack settles in under that answer.
    settles: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a FAILED answer leaves the custom resource and the queue it depends on alive",
        slug: "cr-delete-failed",
        resource_type: "Custom::Gate",
        minted_id: "gate-instance-1",
        held_back: &["Probe"],
        answered_status: "FAILED",
        settles: DELETE_FAILED,
    },
    // The same rule under the MODELLED custom resource type, with a CHAIN of two queues behind
    // the removal and the other terminal a provider answer has: the teardown carries on and
    // takes both of them down.
    Row {
        case: "a SUCCESS answer lets the teardown take the chain it was holding back down",
        slug: "cr-delete-succeeded",
        resource_type: "AWS::CloudFormation::CustomResource",
        minted_id: "gate/instance 2",
        held_back: &["Probe", "Chained"],
        answered_status: "SUCCESS",
        settles: DELETE_COMPLETE,
    },
];

/// The stack one row deploys.
fn stack_name(row: &Row) -> String {
    format!("{}-stack", row.slug)
}

/// The queue the row's logical id `logical` declares — a name of its own per row, so no two
/// rows share a queue.
fn queue_name(row: &Row, logical: &str) -> String {
    format!("{}-{}", row.slug, logical.to_lowercase())
}

/// One Amazon SQS call (`POST /` with `X-Amz-Target: AmazonSQS.<Operation>`), as its status and
/// parsed body.
///
/// Its own spelling rather than the provider suite's, which keeps it private: what this suite
/// asks Amazon SQS is whether the QUEUES THE STACK DECLARED are still there, which is a
/// question about the stack's resources rather than about the provider's transport.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/sqs/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// Whether Amazon SQS still holds the queue `name` — the one reading of "the resource is still
/// there", asked of the service that OWNS it rather than of the stack that declared it.
async fn queue_exists(router: &MultiServiceRouter, name: &str) -> bool {
    let (status, _) = sqs(router, "GetQueueUrl", &json!({ "QueueName": name })).await;
    status == 200
}

/// The `ResourceStatus` the stack `stack` reports for its resource `logical` right now, or
/// `None` where the stack reports no such resource.
async fn resource_status(router: &MultiServiceRouter, stack: &str, logical: &str) -> Option<String> {
    let (status, body) = cloudformation(
        router,
        "DescribeStackResource",
        &[("StackName", stack), ("LogicalResourceId", logical)],
    )
    .await;
    if status != 200 {
        return None;
    }
    opt_element(&body, "ResourceStatus")
}

/// Poll the stack until it reports `wanted`, with NOTHING answering the provider: the statuses
/// these rows wait for are ones the emulator owes on its own.
async fn settled(router: &MultiServiceRouter, case: &str, stack: &str, wanted: &str) -> String {
    let deadline = Instant::now() + BUDGET;
    let mut reported = stack_status(router, stack).await;
    while reported != wanted && Instant::now() < deadline {
        tokio::time::sleep(LOOK).await;
        reported = stack_status(router, stack).await;
    }
    assert_eq!(
        reported, wanted,
        "case {case:?}: expected the stack to settle {wanted}, got {reported:?}"
    );
    reported
}

/// The template one row deploys: the custom resource its provider backs, and the chain of
/// queues it DEPENDS ON — so the teardown removes the custom resource first and reaches the
/// queues only once that removal has been carried out.
fn template(row: &Row, topic_arn: &str) -> String {
    let mut resources = serde_json::Map::new();
    for (at, logical) in row.held_back.iter().enumerate() {
        let mut declaration = json!({
            "Type": "AWS::SQS::Queue",
            "Properties": { "QueueName": queue_name(row, logical) },
        });
        if at > 0 {
            declaration["DependsOn"] = Value::String(row.held_back[at - 1].to_string());
        }
        resources.insert((*logical).to_string(), declaration);
    }
    resources.insert(
        GATE.to_string(),
        json!({
            "Type": row.resource_type,
            // The custom resource depends on the last queue of the chain, so every one of them
            // is removed after it.
            "DependsOn": row.held_back[row.held_back.len() - 1],
            "Properties": { "ServiceToken": topic_arn, "Marker": row.slug },
        }),
    );
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a provider-backed custom resource the stack's queues are removed after",
        "Resources": Value::Object(resources),
    })
    .to_string()
}

#[tokio::test]
async fn a_custom_resources_delete_holds_the_resources_it_depends_on() {
    for row in ROWS {
        let case = row.case;
        let router = build_router();
        let mut provider = provider(&router, row.slug).await;
        let stack = stack_name(row);
        create_stack(&router, case, &stack, &template(row, &provider.topic_arn)).await;

        // The CREATE settles on the provider's answer, which is what the DELETE then carries
        // back: the id the provider minted.
        let created = next_request(&router, &mut provider, case, "the Create of the gate").await;
        assert_eq!(
            created["RequestType"].as_str(),
            Some("Create"),
            "case {case:?}: the stack's first request must be the Create, got {created}"
        );
        answered_success(&router, case, &created, row.minted_id, &Value::Null).await;
        settled(&router, case, &stack, CREATE_COMPLETE).await;
        // The stack's own ID, which stays addressable once a deletion has COMPLETED and taken
        // the stack out of the live set: every read below is by id for that reason.
        let (status, described) =
            cloudformation(&router, "DescribeStacks", &[("StackName", &stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStacks must be answered, got {status} with {described}"
        );
        let stack_id = element(&described, "StackId");
        for logical in row.held_back {
            assert!(
                queue_exists(&router, &queue_name(row, logical)).await,
                "case {case:?}: the stack must have created the queue {logical:?}"
            );
        }

        // ...and now the stack comes DOWN. The custom resource is removed FIRST — every queue
        // it depends on is removed after it — and that removal goes through the provider.
        let (status, body) = cloudformation(&router, "DeleteStack", &[("StackName", &stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteStack must be accepted, got {status} with {body}"
        );
        let delete = next_request(&router, &mut provider, case, "the Delete of the gate").await;
        assert_eq!(
            (
                delete["RequestType"].as_str(),
                delete["LogicalResourceId"].as_str(),
                delete["PhysicalResourceId"].as_str(),
            ),
            (Some("Delete"), Some(GATE), Some(row.minted_id)),
            "case {case:?}: the Delete must name the resource and the id the provider minted, \
             got {delete}"
        );

        // The provider has NOT answered yet: the removal of the custom resource is under way,
        // and everything the teardown is holding back behind it is untouched — it reports what
        // its create left and its own service still serves it.
        let reported = stack_status(&router, &stack_id).await;
        assert_eq!(
            reported, DELETE_IN_PROGRESS,
            "case {case:?}: the deletion must be in flight while the provider is asked, got \
             {reported:?}"
        );
        let reported = resource_status(&router, &stack_id, GATE).await;
        assert_eq!(
            reported.as_deref(),
            Some(DELETE_IN_PROGRESS),
            "case {case:?}: the custom resource must report the removal under way while the \
             provider has not answered, got {reported:?}"
        );
        for logical in row.held_back {
            let reported = resource_status(&router, &stack_id, logical).await;
            assert_eq!(
                reported.as_deref(),
                Some(CREATE_COMPLETE_RESOURCE),
                "case {case:?}: {logical:?} is held back behind that removal, so it must report \
                 the status its create left, got {reported:?}"
            );
            assert!(
                queue_exists(&router, &queue_name(row, logical)).await,
                "case {case:?}: the queue {logical:?} must still be there while the removal \
                 that holds it back is unanswered"
            );
        }

        // ...and now the provider answers.
        let document = json!({
            "Status": row.answered_status,
            "Reason": "the provider decided this teardown",
            "PhysicalResourceId": row.minted_id,
        });
        let (status, body) = answer(&router, &delete, &document).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the provider's answer must be accepted, got {status} with {body}"
        );
        settled(&router, case, &stack_id, row.settles).await;

        // What the answer DID: a FAILED removal is the custom resource's own failure and
        // nothing it was holding back is deleted at all — and the resource it failed on is
        // still there, so the deletion that is the documented way out of `DELETE_FAILED` asks
        // the provider about it again. A SUCCESS lets the teardown take them down at once.
        if row.settles == DELETE_FAILED {
            let reported = resource_status(&router, &stack_id, GATE).await;
            assert_eq!(
                reported.as_deref(),
                Some(DELETE_FAILED),
                "case {case:?}: the custom resource must report the failed removal, got \
                 {reported:?}"
            );
            for logical in row.held_back {
                assert!(
                    queue_exists(&router, &queue_name(row, logical)).await,
                    "case {case:?}: the queue {logical:?} must be left exactly as it is by the \
                     removal the provider refused"
                );
            }

            let (status, body) =
                cloudformation(&router, "DeleteStack", &[("StackName", &stack_id)]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the retried DeleteStack must be accepted, got {status} with \
                 {body}"
            );
            let retried =
                next_request(&router, &mut provider, case, "the retried Delete of the gate").await;
            assert_eq!(
                (
                    retried["RequestType"].as_str(),
                    retried["LogicalResourceId"].as_str(),
                    retried["PhysicalResourceId"].as_str(),
                ),
                (Some("Delete"), Some(GATE), Some(row.minted_id)),
                "case {case:?}: the retried deletion must ask the provider to remove the same \
                 resource again, got {retried}"
            );
            answered_success(&router, case, &retried, row.minted_id, &Value::Null).await;
            settled(&router, case, &stack_id, DELETE_COMPLETE).await;
        }

        // Once the removal has been carried out the teardown takes down what it was holding
        // back, so nothing the stack declared is left in the service that owned it.
        for logical in row.held_back {
            let held = queue_exists(&router, &queue_name(row, logical)).await;
            assert!(
                !held,
                "case {case:?}: expected the queue {logical:?} to be deleted with the stack"
            );
        }
    }
}
