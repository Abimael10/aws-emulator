// SPDX-License-Identifier: CC0-1.0

//! What CloudFormation's OWN record of a stack's resources says after a second deployment of
//! the same declarations.
//!
//! RULE: a deployment acts on the resources whose declaration it CHANGES and on nothing else, so
//! a resource the submitted template re-declares unchanged is still the ONE resource the stack
//! records — under the identifier it already carries, with no second instance beside it. That
//! holds for a resource CloudFormation NAMED as much as for one the template named: the
//! identifier such a resource carries is the one its owning service answered with
//! ([`service_com_amazonaws_cloudformation_cloudformation::state::template::identity_is_a_reported_address`]
//! is the half of that no deployment can recompute), and a deployment that read it as a
//! different resource would record a replacement and abandon the first.
//!
//! Read here through CloudFormation's own reads — `DescribeStackResource` for the identifier of
//! one resource and `DescribeStackResources` for the set the stack holds — which is where a
//! client sees which resources its stack manages. The sibling suite
//! `redeployed_queue_keeps_its_reported_url.rs` judges the same rule where the OWNING service
//! sees it, and those two are the whole observable.
//!
//! The rows differ along what the second deployment revises: another resource's property, and
//! nothing about any resource at all.

use std::sync::Arc;

use serde_json::json;
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

use crate::harness::{
    call, create_stack, member, settled_stack_status, stack_event_reasons, xml_blocks, xml_text,
    REGION,
};
use crate::stack_lifecycle::acknowledging_iam;

/// One address this emulator can be SERVED at, which this suite's router announces as its own
/// endpoint — the shape in which a queue's identifier is the url Amazon SQS reported at the
/// endpoint the create reached rather than a value the naming rules recompute.
const SERVED_AT: &str = "https://127.0.0.1:9298";

/// A router carrying CloudFormation, Amazon SQS and IAM on ONE engine.
fn router_with_a_queue_service() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router.set_url_base(EndpointUrl::parse(SERVED_AT).expect("the suite's base URL must parse"));
    router
}

/// The template a row deploys: a queue the template names, a queue CloudFormation names, and a
/// topic whose declared property the property row revises.
fn template(named_queue: &str, description: &str, retention_seconds: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": description,
        "Resources": {
            "Named": {
                "Type": "AWS::SQS::Queue",
                "Properties": {
                    "QueueName": named_queue,
                    // The one property a row revises, applied to the queue in place:
                    // `SetQueueAttributes` takes it, so it is no reason to replace anything.
                    "MessageRetentionPeriod": retention_seconds,
                },
            },
            "Generated": { "Type": "AWS::SQS::Queue" },
        },
    })
    .to_string()
}

/// The logical ids every deployment of that template declares, which is the set the stack has
/// to hold after the second one as well.
const DECLARED: &[&str] = &["Generated", "Named"];

/// The `PhysicalResourceId` the stack records for one of its logical resources.
async fn recorded_identity(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> String {
    let (status, body) = call(
        router,
        "DescribeStackResource",
        &[
            member("StackName", stack),
            member("LogicalResourceId", logical_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResource({logical_id}), got {status} \
         with {body}"
    );
    xml_text(&body, "PhysicalResourceId").unwrap_or_else(|| {
        panic!("case {case:?}: no PhysicalResourceId for {logical_id} in {body}")
    })
}

/// The logical ids the stack reports it holds, sorted.
async fn recorded_resources(router: &MultiServiceRouter, case: &str, stack: &str) -> Vec<String> {
    let (status, body) = call(
        router,
        "DescribeStackResources",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with {body}"
    );
    let mut held: Vec<String> = xml_blocks(&body, "member")
        .into_iter()
        .filter_map(|block| xml_text(&block, "LogicalResourceId"))
        .collect();
    held.sort();
    held
}

/// One row: what the second deployment of the stack revises.
struct Row {
    case: &'static str,
    stack: &'static str,
    named_queue: &'static str,
    description: &'static str,
    /// The `MessageRetentionPeriod` it states, in seconds.
    retention_seconds: &'static str,
}

/// The retention the first deployment of every row states. Source: that property is
/// `SetQueueAttributes`'s own `MessageRetentionPeriod` attribute, in seconds.
const FIRST_RETENTION: &str = "120";

const ROWS: &[Row] = &[
    Row {
        case: "an update that changes a property of one declared resource",
        stack: "redeployed-record-changed",
        named_queue: "redeployed-record-changed-sink",
        description: "the retention raised",
        retention_seconds: "240",
    },
    Row {
        case: "an update that revises nothing about any resource",
        stack: "redeployed-record-untouched",
        named_queue: "redeployed-record-untouched-sink",
        description: "the same resources, redeployed",
        retention_seconds: FIRST_RETENTION,
    },
];

#[tokio::test]
async fn a_redeployed_stack_records_one_resource_per_declaration() {
    for row in ROWS {
        let case = row.case;
        let router = router_with_a_queue_service();
        let (status, created) = create_stack(
            &router,
            row.stack,
            &template(
                row.named_queue,
                "the deployment the stack was created from",
                FIRST_RETENTION,
            ),
            &acknowledging_iam(&[]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {created}"
        );
        let reached = settled_stack_status(case, &router, REGION, row.stack).await;
        let reasons = stack_event_reasons(&router, REGION, row.stack).await;
        assert_eq!(
            reached, "CREATE_COMPLETE",
            "case {case:?}: expected the create to complete, got {reached} with {reasons:?}"
        );
        let mut deployed = Vec::new();
        for logical_id in DECLARED {
            deployed.push(recorded_identity(&router, case, row.stack, logical_id).await);
        }

        let (status, answered) = call(
            &router,
            "UpdateStack",
            &acknowledging_iam(&[
                member("StackName", row.stack),
                member(
                    "TemplateBody",
                    &template(row.named_queue, row.description, row.retention_seconds),
                ),
            ]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with {answered}"
        );
        let reached = settled_stack_status(case, &router, REGION, row.stack).await;
        let reasons = stack_event_reasons(&router, REGION, row.stack).await;
        assert_eq!(
            reached, "UPDATE_COMPLETE",
            "case {case:?}: expected the update to complete, got {reached} with {reasons:?}"
        );

        for (logical_id, before) in DECLARED.iter().zip(&deployed) {
            let after = recorded_identity(&router, case, row.stack, logical_id).await;
            assert_eq!(
                &after, before,
                "case {case:?}: expected {logical_id} to keep the identifier {before:?}, got \
                 {after:?}"
            );
        }
        let held = recorded_resources(&router, case, row.stack).await;
        let expected: Vec<String> = DECLARED.iter().map(ToString::to_string).collect();
        assert_eq!(
            held, expected,
            "case {case:?}: expected the stack to hold one resource per declaration, got {held:?}"
        );
    }
}
