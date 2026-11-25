// SPDX-License-Identifier: CC0-1.0

//! The deploy CYCLE a deployment tool repeats against one stack: plan under a fixed change-set
//! name, execute the plan, wait for the stack to settle, then discard the change set it just
//! executed — and plan the next deployment under that same name.
//!
//! The rule every case here holds the service to: `DeleteChangeSet` discards a change-set record
//! whenever CloudFormation has no operation
//! of its own in flight on it, so a record in a TERMINAL execution status — one whose execution
//! was carried out among them — is answered with the operation's own empty result: HTTP 200, the
//! `DeleteChangeSetResponse` envelope carrying a `RequestId` and no member of its own (the
//! modelled output shape declares none), and no error. The call takes nothing but the change set
//! it addressed — by its own ARN or by the name it was created under, both of which address that
//! one record: the stack the executed plan changed stays in the status that execution left it
//! and keeps every resource it applied, a request that addresses no record at all is the same
//! empty success, and discarding a record a second time changes nothing — so the plan, execute
//! and discard cycle runs again over the same stack under the very same name.
//!
//! Both forms reach the record of the execution: a bare name addresses the record it was created
//! under for as long as that record is there, so the record a delete discarded reads back as the
//! modelled `ChangeSetNotFound` whichever form the delete addressed it by
//! (`executed_change_set_delete_refusal.rs` holds that same rule over each change-set type).
//!
//! The table spans the ROUND of the cycle — a deployment tool diverged on the second round, which
//! is one row here — the kind of change each round plans (a stack brought into existence, a
//! resource of another type added, a property changed in place), and both forms `ChangeSetName`
//! admits: the executed record's own ARN, and the name it deployed under beside its stack.
//!
//! `change_set_status_refusals.rs` holds the same guard over every `ExecutionStatus` the model
//! declares and `executed_change_set_delete_refusal.rs` over one execution's conclusion; this
//! suite is the repeated client cycle, and the one place the empty RESULT of the delete is read.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use service_com_amazonaws_cloudformation_cloudformation::state::execution_status;
use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::{
    create_change_set, describe_change_set, execute_change_set, settle_the_store,
};
use crate::harness::{call, member, refusal, router_with_engine, xml_text, xml_texts};
use crate::stack_lifecycle::describe_stack;

/// The ONE change-set name every round of the deployment plans under, as a deployment tool
/// does: "the name must be unique among all change sets that are associated with the specified
/// stack", so the name is free again only once the round before it is done with it.
const DEPLOY_SET: &str = "deploy-change-set";

const TOPIC: &str = r#"{"Resources":{"A":{"Type":"AWS::SNS::Topic"}}}"#;
const TOPIC_AND_GROUP: &str =
    r#"{"Resources":{"A":{"Type":"AWS::SNS::Topic"},"G":{"Type":"AWS::Logs::LogGroup"}}}"#;
const RENAMED_TOPIC_AND_GROUP: &str = r#"{"Resources":{
        "A":{"Type":"AWS::SNS::Topic","Properties":{"DisplayName":"renamed"}},
        "G":{"Type":"AWS::Logs::LogGroup"}}}"#;

/// What each round plans, and the logical ids the stack must carry once that round's plan has
/// been carried out: the stack is brought into existence, then a resource of another type is
/// added, then a property of one is changed in place — so the cycle is driven over the kinds of
/// change a redeployment makes rather than over one of them.
const ROUNDS: [(&str, &[&str]); 3] = [
    (TOPIC, &["A"]),
    (TOPIC_AND_GROUP, &["A", "G"]),
    (RENAMED_TOPIC_AND_GROUP, &["A", "G"]),
];

/// How long a round waits for the change set to report the conclusion of the stack operation its
/// execution started, and how often it looks. Far longer than the spans this emulator gives a
/// transition, so neither a loaded nor a fast host can decide a case: what is asserted is the
/// CONDITION, never the time it took.
const EXECUTION_DEADLINE: Duration = Duration::from_secs(180);
const EXECUTION_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How a request addresses the record it is about: the two forms `ChangeSetName` admits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddressedBy {
    /// The executed record's own ARN, which names it on its own.
    Arn,
    /// The name the plan deployed under, with the stack it belonged to beside it.
    NameAndStack,
}

async fn delete_change_set(
    router: &MultiServiceRouter,
    change_set: &str,
    stack: &str,
) -> (u16, String) {
    call(
        router,
        "DeleteChangeSet",
        &[
            member("ChangeSetName", change_set),
            member("StackName", stack),
        ],
    )
    .await
}

/// Assert one answer is the delete's own EMPTY result: its envelope, a request id in it, no
/// error element, and none of the members a change-set read would carry — the modelled
/// `DeleteChangeSetOutput` declares no member at all
/// (`smithy_query(query="operation",operation="DeleteChangeSet")` reports `output: {}`).
fn assert_empty_result(case: &str, http_status: u16, body: &str) {
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected the delete to be answered, got {http_status} {body}"
    );
    assert!(
        body.contains("<DeleteChangeSetResponse"),
        "case {case:?}: expected the DeleteChangeSetResponse envelope, got {body:?}"
    );
    assert!(
        !xml_text(body, "RequestId").unwrap_or_default().is_empty(),
        "case {case:?}: expected the envelope to carry a RequestId, got {body:?}"
    );
    assert!(
        !body.contains("<Error>"),
        "case {case:?}: an answered call carries no error element, got {body:?}"
    );
    for absent in ["ChangeSetId", "ChangeSetName", "ExecutionStatus", "StackId"] {
        assert_eq!(
            xml_text(body, absent),
            None,
            "case {case:?}: the empty result carries no {absent}, got {body:?}"
        );
    }
}

/// The logical ids `DescribeStackResources` reports for one stack.
async fn deployed_logical_ids(
    case: &str,
    router: &MultiServiceRouter,
    stack: &str,
) -> BTreeSet<String> {
    let (code, body) = call(
        router,
        "DescribeStackResources",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStackResources must succeed, got {code} {body}"
    );
    xml_texts(&body, "LogicalResourceId").into_iter().collect()
}

/// The change-set names `ListChangeSets` reports for one stack.
async fn listed(case: &str, router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (code, body) = call(router, "ListChangeSets", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: ListChangeSets must succeed, got {code} {body}"
    );
    xml_texts(&body, "ChangeSetName")
}

/// Look again until the record at `arn` reports the conclusion of the stack operation its
/// execution started, the way a deployment tool's waiter does, settling whatever the store owes
/// between reads.
async fn awaiting_the_executions_conclusion(
    case: &str,
    router: &MultiServiceRouter,
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    arn: &str,
) {
    let deadline = Instant::now() + EXECUTION_DEADLINE;
    loop {
        settle_the_store(router, engine).await;
        let (code, body) = describe_change_set(router, arn, None).await;
        let reported = xml_text(&body, "ExecutionStatus").unwrap_or_default();
        if code == 200 && reported == execution_status::EXECUTE_COMPLETE {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the execution to be carried out, last read {code} \
             {reported:?}"
        );
        tokio::time::sleep(EXECUTION_POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn every_deploy_discards_the_plan_its_execution_carried_out() {
    for addressed_by in [AddressedBy::Arn, AddressedBy::NameAndStack] {
        let (router, engine) = router_with_engine();
        let stack = format!("deploy-cycle-{addressed_by:?}").to_lowercase();
        // The ARN each round's plan reported, so no round can be answered with another's record.
        let mut arns: Vec<String> = Vec::new();

        for (round, (template, expected_ids)) in ROUNDS.iter().enumerate() {
            let case = format!("{addressed_by:?} round {round}");
            let change_set_type = if round == 0 { "CREATE" } else { "UPDATE" };
            let (code, body) = create_change_set(
                &router,
                &stack,
                DEPLOY_SET,
                &[
                    member("ChangeSetType", change_set_type),
                    member("TemplateBody", template),
                ],
            )
            .await;
            assert_eq!(
                code, 200,
                "case {case:?}: the round must plan under the shared name, got {code} {body}"
            );
            let arn = xml_text(&body, "Id").unwrap_or_else(|| {
                panic!("case {case:?}: CreateChangeSet answers the change set's ARN")
            });
            assert!(
                !arns.contains(&arn),
                "case {case:?}: expected a record of its own, got the ARN {arn:?} an earlier \
                 round already reported"
            );
            arns.push(arn.clone());
            settle_the_store(&router, &engine).await;

            let (code, body) = execute_change_set(&router, DEPLOY_SET, Some(&stack)).await;
            assert_eq!(
                code, 200,
                "case {case:?}: the round's plan must execute, got {code} {body}"
            );
            awaiting_the_executions_conclusion(&case, &router, &engine, &arn).await;

            // What the stack looks like after the deployment and BEFORE the delete: the delete
            // must leave all of it exactly as it is.
            let (code, deployed) = describe_stack(&router, &stack).await;
            let status_before = xml_text(&deployed, "StackStatus").unwrap_or_default();
            assert_eq!(
                code, 200,
                "case {case:?}: the deployed stack must be readable, got {code} {deployed}"
            );
            assert!(
                status_before.ends_with("_COMPLETE"),
                "case {case:?}: the execution must have carried the change through, got \
                 {status_before:?}"
            );

            // The delete the tool issues straight after the execution settles, in the form it
            // addresses the change set by. Either form addresses the record of that execution —
            // its own ARN, and the name it was created under beside its stack, both of which go
            // on addressing the record for as long as it is there — so either is the
            // operation's own empty result.
            let addressed = match addressed_by {
                AddressedBy::Arn => arn.as_str(),
                AddressedBy::NameAndStack => DEPLOY_SET,
            };
            let (code, body) = delete_change_set(&router, addressed, &stack).await;
            assert_empty_result(&case, code, &body);
            // ...and discarding what this call already discarded changes nothing: the same
            // request is the same empty success, now addressing no record at all.
            let (code, body) = delete_change_set(&router, addressed, &stack).await;
            assert_empty_result(&format!("{case} repeated"), code, &body);

            // The record the delete took away is gone whichever form addressed it: a read of it
            // is the modelled not-found.
            let (code, body) = describe_change_set(&router, &arn, None).await;
            assert_eq!(
                (code, refusal(&body)),
                (
                    404,
                    (
                        "ChangeSetNotFound".to_string(),
                        format!("ChangeSet [{arn}] does not exist"),
                    ),
                ),
                "case {case:?}: expected the discarded record to read back as not found, got \
                 {code} {body}"
            );

            // Either way the stack holds no plan, so the next round plans under the same name...
            let names = listed(&case, &router, &stack).await;
            assert!(
                names.is_empty(),
                "case {case:?}: expected the stack to hold no plan, got {names:?}"
            );
            // ...and the delete took nothing of the stack itself: it is in the status its
            // execution left it in and carries every resource that execution applied.
            let (code, after) = describe_stack(&router, &stack).await;
            assert_eq!(
                (code, xml_text(&after, "StackStatus").unwrap_or_default()),
                (200, status_before.clone()),
                "case {case:?}: discarding a plan leaves its stack alone, got {code} {after}"
            );
            let expected: BTreeSet<String> =
                expected_ids.iter().map(|id| (*id).to_string()).collect();
            let carried = deployed_logical_ids(&case, &router, &stack).await;
            assert_eq!(
                carried, expected,
                "case {case:?}: expected the stack to carry {expected:?}, got {carried:?}"
            );
        }
    }
}
