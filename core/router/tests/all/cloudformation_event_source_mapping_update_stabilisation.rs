// SPDX-License-Identifier: CC0-1.0

//! A stack whose `AWS::Lambda::EventSourceMapping` declaration CHANGES is not reported updated
//! before AWS Lambda reports that mapping settled again — so the first call a client makes
//! after the update completes finds a trigger that is really polling, under the changed
//! declaration.
//!
//! The rule under test is the one
//! [`crate::cloudformation_event_source_mapping_stabilisation`] states for the create, and it
//! is stated about a stack OPERATION rather than about the create alone: **CloudFormation does
//! not report a resource (and so the stack operation holding it) created or changed while the
//! service that owns it still reports the resource short of the state it is settled in; it
//! holds the resource `*_IN_PROGRESS`, under the identifier that service already assigned,
//! until that service reports it settled.** For an event source mapping the settled states are
//! the two its `Enabled` setting asks for — "When true, the event source mapping is active.
//! When false, Lambda pauses polling and invocation" — while `Creating` is the window Lambda
//! stands the poller up in and `Updating` the window it applies an accepted change in
//! (`EventSourceMappingConfiguration$State`: "Creating, Enabling, Enabled, Disabling,
//! Disabled, Updating, or Deleting").
//!
//! What this pins is the observation an infrastructure-as-code client makes about its SECOND
//! deployment: the instant the stack FIRST reports the update complete,
//! `lambda get-event-source-mapping` resolves the id the stack still publishes for the mapping
//! and reports it settled in the state the CHANGED declaration asks for, with that
//! declaration's batch size in force. An update reported complete while Lambda still reports
//! the mapping `Updating` — or still `Creating`, for a change CloudFormation applies by making
//! a new mapping — fails these cases, because the deployment step that follows reads a trigger
//! whose new settings are not in effect, and every further change or delete of it is refused
//! as a conflict with a mapping Lambda is still working on. WHICH of the two ways a change is
//! applied in is not this rule's subject: what the mapping is read by is the id the stack
//! publishes for it once the update has completed, exactly as a client reads it.
//!
//! The rows differ along what the rule generalises over: WHICH event source the mapping polls
//! (an Amazon DynamoDB table's stream — the kind the recorded deployment used — an Amazon
//! Kinesis data stream and an Amazon SQS queue), WHICH state the change settles the mapping in
//! and from which one (polling left alone, polling paused by the update, polling resumed by
//! it), and the batch size before and after. Every expected value is computed here from what
//! the row declares, never read back out of the answer being checked — except the UUID Lambda
//! minted for the mapping, which is that service's to assign.
//!
//! The stack, its template and the router are this suite's sibling's
//! ([`crate::cloudformation_event_source_mapping_stabilisation`]): what differs here is that
//! the same declaration is deployed a second time, changed.

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_apigatewayv2_resources::cloudformation;
use crate::cloudformation_event_source_mapping_stabilisation::{
    build_router, deployment_finished, failure_reasons, mapping_properties, published_id,
    reported_mapping, template, Source, CREATE_COMPLETE, CREATE_IN_PROGRESS, MAPPING_ID,
};
use crate::cloudformation_provisions_lambda_event_source_mappings::{
    MAPPING_STATE_DISABLED, MAPPING_STATE_ENABLED,
};

/// The `StackStatus` an update settles in, and the two it reports while it runs: the second is
/// the window an update that REPLACED a resource deletes what it replaced in, which is part of
/// the operation rather than the status it finished in.
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";
const UPDATE_IN_PROGRESS: &str = "UPDATE_IN_PROGRESS";
const UPDATE_COMPLETE_CLEANUP_IN_PROGRESS: &str = "UPDATE_COMPLETE_CLEANUP_IN_PROGRESS";

/// What one deployment of a row declares about the mapping itself: the `Enabled` setting, where
/// it states one at all, and the batch size.
#[derive(Debug, Clone, Copy)]
struct Declaration {
    /// The `Enabled` property, where the declaration states one ("Default: True" when it does
    /// not).
    enabled: Option<bool>,
    /// The `BatchSize` the mapping is declared with.
    batch_size: i64,
}

/// One row: the source the mapping polls, the declaration the stack is created with, the
/// CHANGED declaration it is then updated to, and the state that change settles the mapping in.
struct Row {
    case: &'static str,
    stack: &'static str,
    source: Source,
    created: Declaration,
    updated: Declaration,
    /// The state the CHANGED declaration settles the mapping in — `Enabled` for a mapping asked
    /// to poll, `Disabled` for one asked not to.
    settles_into: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // The source kind the recorded deployment used, with the change a client makes most
            // often: more records per batch, polling left as declared.
            case: "a DynamoDB table's stream, batch size raised, polling left to the default",
            stack: "esm-update-ddb-stream",
            source: Source::DynamodbStream,
            created: Declaration { enabled: None, batch_size: 1 },
            updated: Declaration { enabled: None, batch_size: 5 },
            settles_into: MAPPING_STATE_ENABLED,
        },
        Row {
            case: "a DynamoDB table's stream, polling paused by the update",
            stack: "esm-update-ddb-stream-paused",
            source: Source::DynamodbStream,
            created: Declaration { enabled: Some(true), batch_size: 2 },
            updated: Declaration { enabled: Some(false), batch_size: 2 },
            settles_into: MAPPING_STATE_DISABLED,
        },
        Row {
            case: "a Kinesis data stream, polling resumed by the update",
            stack: "esm-update-kinesis",
            source: Source::KinesisStream,
            created: Declaration { enabled: Some(false), batch_size: 10 },
            updated: Declaration { enabled: Some(true), batch_size: 10 },
            settles_into: MAPPING_STATE_ENABLED,
        },
        Row {
            case: "an SQS queue, batch size lowered and polling stated by the update",
            stack: "esm-update-queue",
            source: Source::SqsQueue,
            created: Declaration { enabled: None, batch_size: 9 },
            updated: Declaration { enabled: Some(true), batch_size: 4 },
            settles_into: MAPPING_STATE_ENABLED,
        },
    ]
}

/// Deploy one of a row's declarations and answer the status the operation finished in, with the
/// `DescribeStacks` answer it was read from.
async fn deploy(
    router: &MultiServiceRouter,
    row: &Row,
    action: &str,
    while_running: &[&str],
    declaration: Declaration,
) -> (String, String) {
    let case = row.case;
    let declared = mapping_properties(row.source, declaration.enabled, declaration.batch_size);
    let (status, body) = cloudformation(
        router,
        action,
        &[
            ("StackName", row.stack),
            ("TemplateBody", &template(row.source, row.stack, &declared)),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    deployment_finished(router, row.stack, while_running).await
}

/// Every row: the stack is created, its mapping declaration is CHANGED, the stack reaches
/// `UPDATE_COMPLETE`, and at that FIRST observation Lambda reports the mapping the stack still
/// publishes in the state the changed declaration asked for — never in the `Updating` window it
/// applies a change in — with that declaration's batch size in force.
#[tokio::test]
async fn a_stack_update_completes_only_once_lambda_settles_the_changed_mapping() {
    for row in rows() {
        let router = build_router();
        let case = row.case;

        let (reached, described) =
            deploy(&router, &row, "CreateStack", &[CREATE_IN_PROGRESS], row.created).await;
        assert_eq!(
            reached, CREATE_COMPLETE,
            "case {case:?}: expected the stack to deploy before the update, got {described} \
             with resource failures {:?}",
            failure_reasons(&router, row.stack).await
        );

        let (reached, described) = deploy(
            &router,
            &row,
            "UpdateStack",
            &[UPDATE_IN_PROGRESS, UPDATE_COMPLETE_CLEANUP_IN_PROGRESS],
            row.updated,
        )
        .await;
        assert_eq!(
            reached, UPDATE_COMPLETE,
            "case {case:?}: expected the changed declaration to deploy, got {described} with \
             resource failures {:?}",
            failure_reasons(&router, row.stack).await
        );

        // The mapping the completed update published, which is what a client reads it by.
        let uuid = published_id(&router, case, row.stack, MAPPING_ID).await;
        let reported = reported_mapping(&router, case, &uuid).await;
        assert_eq!(
            reported.get("State").and_then(Value::as_str),
            Some(row.settles_into),
            "case {case:?}: expected the mapping to be {:?} the instant the stack reported \
             {UPDATE_COMPLETE}, got {reported}",
            row.settles_into
        );
        assert_eq!(
            reported.get("BatchSize").and_then(Value::as_i64),
            Some(row.updated.batch_size),
            "case {case:?}: expected the changed batch size {} to be in force, got {reported}",
            row.updated.batch_size
        );
    }
}
