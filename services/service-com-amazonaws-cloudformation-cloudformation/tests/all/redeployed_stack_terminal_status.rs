// SPDX-License-Identifier: CC0-1.0

//! A stack REDEPLOYED through a second change set reaches a TERMINAL status.
//!
//! The rule every case here holds the service to: **every stack operation a change set's
//! execution opens reaches a terminal status that `DescribeStacks` publishes — whatever the
//! executed template does to the stack's resources, and however many times the same stack is
//! deployed again.** An `UPDATE` change set the emulator can deploy settles the stack
//! `UPDATE_COMPLETE`; one whose deployment CloudFormation cannot resolve (an
//! `Fn::ImportValue` of an export no live stack publishes) settles it
//! `UPDATE_ROLLBACK_COMPLETE`; and the stack takes the next change set afterwards. So a
//! client that polls `DescribeStacks` after `ExecuteChangeSet` — which is what every CLI and
//! SDK waiter does — always learns the outcome rather than reading an operation in flight for
//! ever.
//!
//! The recorded trace this suite was written for is ONE row of the table below (a second
//! deployment that adds a queue). The rows span what the rule generalises over: what the
//! redeployed template DOES to the stack — adds a resource, changes one in place, replaces
//! one, withdraws one, publishes a new export — and the resource TYPE it does it to. Every row
//! asserts the EFFECT the redeployment had beside the status it settled in (which resources
//! the stack reports, whether the resource kept the identifier it already had, and which
//! exports the Region now holds), so a row cannot pass against a deployment that only moved
//! the status. Every read goes through the real request path (form-encoded body in, XML
//! envelope out), and the calls are made in the order the AWS CLI's `deploy` makes them, so
//! what is asserted is what a client sees.
//!
//! `unresolvable_imports.rs` already pins the rollback an `Fn::ImportValue` of an unpublished
//! export forces, including its reason; what the second test here adds is the REDEPLOYMENT
//! that follows it, which the rule says the stack must take.

use std::sync::Arc;
use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::{create_change_set, describe_change_set, execute_change_set};
use crate::harness::{
    call, member, settled_stack_status, stack_event_reasons, xml_text, xml_texts, REGION,
};
use crate::stack_resource_provisioning::physical_id;

/// How long a poll loop waits for a change set's creation to settle. Generous: what a case
/// asserts is the CONDITION (a creation that is over), never the time it took, so a loaded
/// host cannot fail one.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);
/// See [`SETTLE_DEADLINE`].
const SETTLE_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The services that OWN the resource types the templates below declare, mounted beside
/// CloudFormation on the SAME engine exactly as `core/router/src/lib.rs` mounts them: a
/// stack's deployment calls them, so a suite that deploys one of those types must have them
/// there.
///
/// Built here rather than taken from [`crate::harness::router`] because that shared router
/// does NOT mount Amazon SQS, and the queue these templates declare is really created there.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router
}

/// Poll `DescribeChangeSet` the way a client's waiter does, and answer the status the
/// creation SETTLED on.
async fn settled_change_set_status(
    case: &str,
    router: &MultiServiceRouter,
    change_set: &str,
    stack: &str,
) -> String {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (code, body) = describe_change_set(router, change_set, Some(stack)).await;
        assert_eq!(
            code, 200,
            "case {case:?}: DescribeChangeSet must answer 200, got {code} {body}"
        );
        let reported = xml_text(&body, "Status").unwrap_or_default();
        if reported == "CREATE_COMPLETE" || reported == "FAILED" {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the change set {change_set} to settle, last saw \
             {reported:?}"
        );
        tokio::time::sleep(SETTLE_POLL_INTERVAL).await;
    }
}

/// What `aws cloudformation deploy` does to one stack: create a change set, wait for its
/// creation, execute it, and wait for the stack's terminal status — which is what this
/// answers.
///
/// The members an AWS CLI `deploy` always sends travel with it: an empty `Parameters`, `Tags`
/// and `Capabilities` list (the query protocol serializes an empty list as the bare member
/// name) and the `Description` it stamps on the change set. The change set is then addressed
/// by the `Id` the create answered with, as that client addresses it.
async fn deploy(
    case: &str,
    router: &MultiServiceRouter,
    stack: &str,
    change_set: &str,
    change_set_type: &str,
    template: &str,
) -> String {
    let (code, body) = create_change_set(
        router,
        stack,
        change_set,
        &[
            member("ChangeSetType", change_set_type),
            member("TemplateBody", template),
            member("Parameters", ""),
            member("Tags", ""),
            member("Capabilities", ""),
            member("Description", "Created by AWS CLI"),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateChangeSet must answer 200, got {code} {body}"
    );
    let id = xml_text(&body, "Id").unwrap_or_default();
    assert!(
        !id.is_empty(),
        "case {case:?}: CreateChangeSet must answer an Id, got {body}"
    );
    let created = settled_change_set_status(case, router, &id, stack).await;
    assert_eq!(
        created, "CREATE_COMPLETE",
        "case {case:?}: a change set with changes must complete its creation"
    );
    let (code, body) = execute_change_set(router, &id, Some(stack)).await;
    assert_eq!(
        code, 200,
        "case {case:?}: ExecuteChangeSet must answer 200, got {code} {body}"
    );
    settled_stack_status(case, router, REGION, stack).await
}

/// The logical ids `DescribeStackResources` reports for a stack, sorted — the set a client
/// reads back to see WHICH resources the deployment left the stack holding.
async fn reported_logical_ids(case: &str, router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (code, body) = call(
        router,
        "DescribeStackResources",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStackResources must answer 200, got {code} {body}"
    );
    let mut ids = xml_texts(&body, "LogicalResourceId");
    ids.sort();
    ids
}

/// The export names `ListExports` reports for this Region.
async fn reported_exports(case: &str, router: &MultiServiceRouter) -> Vec<String> {
    let (code, body) = call(router, "ListExports", &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: ListExports must answer 200, got {code} {body}"
    );
    xml_texts(&body, "Name")
}

/// One row of the redeploy table: the template the first deployment creates the stack from,
/// the one the SECOND deployment applies to it, and the EFFECT that redeployment must have had
/// — so a row cannot pass against a deployment that only moved the stack's status.
struct Redeploy<'a> {
    case: &'a str,
    stack: &'a str,
    first: &'a str,
    second: &'a str,
    /// The logical ids the stack must report afterwards, sorted.
    holds: &'a [&'a str],
    /// Whether `A` must still be the resource the first deployment created, under the
    /// identifier its owning service had already assigned it: `false` for a row whose change
    /// REPLACES it, which mints a new one.
    keeps_a: bool,
    /// The export name the Region must hold afterwards, for a row that publishes one.
    exports: Option<&'a str>,
}

fn redeployments() -> Vec<Redeploy<'static>> {
    vec![
        Redeploy {
            case: "a resource added",
            stack: "redeploy-added",
            first: r#"{"Resources":{"A":{"Type":"AWS::SQS::Queue","Properties":{"QueueName":"redeploy-added-a"}}}}"#,
            second: r#"{"Resources":{"A":{"Type":"AWS::SQS::Queue","Properties":{"QueueName":"redeploy-added-a"}},"B":{"Type":"AWS::SQS::Queue","Properties":{"QueueName":"redeploy-added-b"}}}}"#,
            holds: &["A", "B"],
            keeps_a: true,
            exports: None,
        },
        Redeploy {
            case: "a resource changed in place",
            stack: "redeploy-changed",
            first: r#"{"Resources":{"A":{"Type":"AWS::SQS::Queue","Properties":{"QueueName":"redeploy-changed-a","VisibilityTimeout":"30"}}}}"#,
            second: r#"{"Resources":{"A":{"Type":"AWS::SQS::Queue","Properties":{"QueueName":"redeploy-changed-a","VisibilityTimeout":"60"}}}}"#,
            holds: &["A"],
            keeps_a: true,
            exports: None,
        },
        Redeploy {
            case: "a resource replaced",
            stack: "redeploy-replaced",
            first: r#"{"Resources":{"A":{"Type":"AWS::SQS::Queue","Properties":{"QueueName":"redeploy-replaced-a"}}}}"#,
            second: r#"{"Resources":{"A":{"Type":"AWS::SQS::Queue","Properties":{"QueueName":"redeploy-replaced-a2"}}}}"#,
            holds: &["A"],
            keeps_a: false,
            exports: None,
        },
        Redeploy {
            case: "a resource withdrawn",
            stack: "redeploy-withdrawn",
            first: r#"{"Resources":{"A":{"Type":"AWS::SQS::Queue"},"B":{"Type":"AWS::SQS::Queue"}}}"#,
            second: r#"{"Resources":{"A":{"Type":"AWS::SQS::Queue"}}}"#,
            holds: &["A"],
            keeps_a: true,
            exports: None,
        },
        Redeploy {
            case: "an exported output published",
            stack: "redeploy-exported",
            first: r#"{"Resources":{"A":{"Type":"AWS::SQS::Queue"}}}"#,
            second: r#"{"Resources":{"A":{"Type":"AWS::SQS::Queue"},"B":{"Type":"AWS::SQS::Queue"}},"Outputs":{"BUrl":{"Value":{"Ref":"B"},"Export":{"Name":"redeploy-exported-b"}}}}"#,
            holds: &["A", "B"],
            keeps_a: true,
            exports: Some("redeploy-exported-b"),
        },
        Redeploy {
            case: "a resource of another type added",
            stack: "redeploy-other-type",
            first: r#"{"Resources":{"A":{"Type":"AWS::SNS::Topic"}}}"#,
            second: r#"{"Resources":{"A":{"Type":"AWS::SNS::Topic"},"B":{"Type":"AWS::SNS::Topic"}}}"#,
            holds: &["A", "B"],
            keeps_a: true,
            exports: None,
        },
    ]
}

#[tokio::test]
async fn a_second_change_set_deployment_settles_the_stack() {
    for row in redeployments() {
        let router = router();
        let settled = deploy(row.case, &router, row.stack, "cs-1", "CREATE", row.first).await;
        assert_eq!(
            settled, "CREATE_COMPLETE",
            "case {:?}: the first deployment must settle CREATE_COMPLETE, got {settled}",
            row.case
        );
        // The identifier the owning service assigned the resource the first deployment made,
        // which says whether the second one kept that resource or minted another.
        let a_before = physical_id(&router, row.stack, "A").await;
        assert!(
            !a_before.is_empty(),
            "case {:?}: the first deployment must report a physical id for A",
            row.case
        );

        let settled = deploy(row.case, &router, row.stack, "cs-2", "UPDATE", row.second).await;
        assert_eq!(
            settled, "UPDATE_COMPLETE",
            "case {:?}: executing an UPDATE change set must settle the stack UPDATE_COMPLETE, \
             got {settled} with the events {:?}",
            row.case,
            stack_event_reasons(&router, REGION, row.stack).await
        );
        // ...and the template it carried really reached the stack: the resources the stack
        // holds are the ones that template declares, and no others.
        let holds = reported_logical_ids(row.case, &router, row.stack).await;
        assert_eq!(
            holds,
            row.holds
                .iter()
                .map(|id| (*id).to_string())
                .collect::<Vec<_>>(),
            "case {:?}: the redeployed template's resources are the ones the stack holds, got \
             {holds:?}",
            row.case
        );
        let a_after = physical_id(&router, row.stack, "A").await;
        assert_eq!(
            a_after == a_before,
            row.keeps_a,
            "case {:?}: expected A to keep its identity: {}, it was {a_before:?} and is now \
             {a_after:?}",
            row.case,
            row.keeps_a
        );
        let exports = reported_exports(row.case, &router).await;
        assert_eq!(
            exports,
            row.exports
                .iter()
                .map(|name| (*name).to_string())
                .collect::<Vec<_>>(),
            "case {:?}: the exports the redeployment publishes are the ones the Region holds, \
             got {exports:?}",
            row.case
        );
    }
}

#[tokio::test]
async fn a_redeployment_whose_import_cannot_be_resolved_settles_rolled_back() {
    let router = router();
    let stack = "redeploy-unresolvable";

    let first = r#"{"Resources":{"A":{"Type":"AWS::SQS::Queue","Properties":{"QueueName":"redeploy-unresolvable-a"}}}}"#;
    let settled = deploy("the first deployment", &router, stack, "cs-1", "CREATE", first).await;
    assert_eq!(
        settled, "CREATE_COMPLETE",
        "the first deployment must settle CREATE_COMPLETE, got {settled}"
    );

    // The added resource reads an export no live stack of this Region publishes, so the
    // deployment cannot be carried out and the update is rolled back.
    let second = r#"{"Resources":{"A":{"Type":"AWS::SQS::Queue","Properties":{"QueueName":"redeploy-unresolvable-a"}},"B":{"Type":"AWS::SQS::Queue","Properties":{"QueueName":"redeploy-unresolvable-b","Tags":[{"Key":"owner","Value":{"Fn::ImportValue":"redeploy-never-declared"}}]}}}}"#;
    let settled = deploy("an unresolvable import", &router, stack, "cs-2", "UPDATE", second).await;
    assert_eq!(
        settled, "UPDATE_ROLLBACK_COMPLETE",
        "an update importing an export nothing publishes must settle the stack \
         UPDATE_ROLLBACK_COMPLETE, got {settled} with the events {:?}",
        stack_event_reasons(&router, REGION, stack).await
    );

    // ...and the stack takes the next deployment afterwards, which settles in its own
    // terminal status like any other.
    let third = r#"{"Resources":{"A":{"Type":"AWS::SQS::Queue","Properties":{"QueueName":"redeploy-unresolvable-a"}},"C":{"Type":"AWS::SQS::Queue","Properties":{"QueueName":"redeploy-unresolvable-c"}}}}"#;
    let settled = deploy("the deployment after the rollback", &router, stack, "cs-3", "UPDATE", third)
        .await;
    assert_eq!(
        settled, "UPDATE_COMPLETE",
        "a valid deployment after a rolled-back one must settle UPDATE_COMPLETE, got {settled}"
    );
}
