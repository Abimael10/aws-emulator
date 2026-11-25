// SPDX-License-Identifier: CC0-1.0

//! What a `DeleteChangeSet` takes away, driven end to end over the awsQuery wire.
//!
//! RULE: a `DeleteChangeSet` discards exactly the ONE record its request addressed — the
//! record its `ChangeSetName` resolves, as its own ARN or as the name it was created under
//! beside the stack it belongs to — and a request that resolves no record this Region holds is
//! the operation's own empty result (HTTP 200, the `DeleteChangeSetResponse` envelope carrying
//! a `RequestId` and no member of its own, no error) that takes NOTHING: every other record of
//! every stack still reads back with the `ExecutionStatus` it reached, every stack still lists
//! the plans it holds, and each stack stays in its status carrying the resources it carries.
//!
//! The rows of the first table span the ways a request can address no record at all: a name no
//! record of the named stack carries, that same name beside the stack's id, a name another
//! stack's record carries, an ARN no record carries, a record's own ARN beside a stack it does
//! not belong to, and a name on a stack that is not there — while the store holds a plan the
//! stack still has to carry out, a record whose execution was carried out, and a second
//! stack's plan, so a delete that took anything of any of them would be read here.
//!
//! The second test is the complement, over both forms `ChangeSetName` admits: the delete that
//! DOES address a record takes that record and only it, and repeating it addresses no record
//! and so takes nothing more.
//!
//! `change_set_record_addressing.rs` pins which requests address a record for the reads and
//! for `ExecuteChangeSet`; `executed_plan_discarded_each_deploy.rs` the repeated deploy cycle.
//! This suite is about what a discard REACHES.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use service_com_amazonaws_cloudformation_cloudformation::state::execution_status;
use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::{
    create_change_set, describe_change_set, execute_change_set, settle_the_store,
};
use crate::harness::{call, member, refusal, router_with_engine, xml_text, xml_texts};
use crate::stack_lifecycle::describe_stack;

/// The template the executed plan deploys, and the one the plan the stack still holds would
/// deploy over it: one more resource, so the held plan is a real change.
const TOPIC: &str = r#"{"Resources":{"A":{"Type":"AWS::SNS::Topic"}}}"#;
const TOPIC_AND_GROUP: &str =
    r#"{"Resources":{"A":{"Type":"AWS::SNS::Topic"},"G":{"Type":"AWS::Logs::LogGroup"}}}"#;

/// How long a fixture waits for the change set to report the conclusion of the stack operation
/// its execution started, and how often it looks: far longer than the spans this emulator
/// gives a transition, so what decides a case is the CONDITION and never the time it took.
const EXECUTION_DEADLINE: Duration = Duration::from_secs(180);
const EXECUTION_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How a request addresses the record it is about: the two forms `ChangeSetName` admits.
#[derive(Debug, Clone, Copy)]
enum AddressedBy {
    /// The record's own ARN, which names it on its own.
    Arn,
    /// The name it was created under, with the stack it belongs to beside it.
    NameAndStack,
}

async fn delete_change_set(
    router: &MultiServiceRouter,
    change_set: &str,
    stack: Option<&str>,
) -> (u16, String) {
    let mut members = vec![member("ChangeSetName", change_set)];
    if let Some(stack) = stack {
        members.push(member("StackName", stack));
    }
    call(router, "DeleteChangeSet", &members).await
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

/// The change-set names `ListChangeSets` reports for one stack.
async fn listed(case: &str, router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (code, body) = call(router, "ListChangeSets", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: ListChangeSets must succeed, got {code} {body}"
    );
    xml_texts(&body, "ChangeSetName")
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

/// The `(HTTP status, ChangeSetId, ExecutionStatus)` a read of one record reports.
async fn read_record(
    router: &MultiServiceRouter,
    change_set: &str,
    stack: Option<&str>,
) -> (u16, String, String) {
    let (code, body) = describe_change_set(router, change_set, stack).await;
    (
        code,
        xml_text(&body, "ChangeSetId").unwrap_or_default(),
        xml_text(&body, "ExecutionStatus").unwrap_or_default(),
    )
}

/// Look again until the record at `arn` reports the conclusion of the stack operation its
/// execution started, the way a deployment tool's waiter does, settling whatever the store
/// owes between reads.
async fn awaiting_the_executions_conclusion(
    case: &str,
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    arn: &str,
) {
    let deadline = Instant::now() + EXECUTION_DEADLINE;
    loop {
        settle_the_store(router, engine).await;
        let (code, _, reported) = read_record(router, arn, None).await;
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

/// The store every case of this suite deletes against: one deployed stack carrying the record
/// of the execution that deployed it AND a plan it still has to carry out, beside a second
/// stack with a plan of its own — so whatever a delete reaches beyond the record it addressed
/// is read back by the assertions below.
struct Deployed {
    router: MultiServiceRouter,
    stack: String,
    stack_id: String,
    /// The record of the execution that deployed the stack, and the name it deployed under.
    executed_arn: String,
    executed_name: String,
    /// The plan the stack still holds, and the name it was created under.
    held_arn: String,
    held_name: String,
    /// The status the deployed stack reports before any delete.
    stack_status: String,
    /// A second stack and the plan it holds.
    other_stack: String,
    other_arn: String,
    other_name: String,
}

/// Create one change set and answer the ARN it reports for itself beside the id of the stack
/// it plans changes for.
async fn plan(
    case: &str,
    router: &MultiServiceRouter,
    stack: &str,
    change_set: &str,
    change_set_type: &str,
    template: &str,
) -> (String, String) {
    let (code, body) = create_change_set(
        router,
        stack,
        change_set,
        &[
            member("ChangeSetType", change_set_type),
            member("TemplateBody", template),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: the change set must be created, got {code} {body}"
    );
    let arn = xml_text(&body, "Id")
        .unwrap_or_else(|| panic!("case {case:?}: CreateChangeSet answers the change set's ARN"));
    let stack_id = xml_text(&body, "StackId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateChangeSet answers the stack's id"));
    (arn, stack_id)
}

impl Deployed {
    /// Build the store over the real wire: a plan is created, executed and waited out, a
    /// second plan is created over the stack it deployed, and another stack is brought into
    /// review with a plan of its own.
    async fn new(case: &str, prefix: &str) -> Self {
        let (router, engine) = router_with_engine();
        let stack = format!("{prefix}-deployed");
        let executed_name = format!("{prefix}-applied-set");
        let held_name = format!("{prefix}-held-set");
        let (executed_arn, stack_id) =
            plan(case, &router, &stack, &executed_name, "CREATE", TOPIC).await;
        settle_the_store(&router, &engine).await;
        let (code, body) = execute_change_set(&router, &executed_name, Some(&stack)).await;
        assert_eq!(
            code, 200,
            "case {case:?}: the plan must execute, got {code} {body}"
        );
        awaiting_the_executions_conclusion(case, &router, &engine, &executed_arn).await;

        let (held_arn, _) = plan(
            case,
            &router,
            &stack,
            &held_name,
            "UPDATE",
            TOPIC_AND_GROUP,
        )
        .await;
        settle_the_store(&router, &engine).await;

        let other_stack = format!("{prefix}-other");
        let other_name = format!("{prefix}-other-set");
        let (other_arn, _) =
            plan(case, &router, &other_stack, &other_name, "CREATE", TOPIC).await;
        settle_the_store(&router, &engine).await;

        let (code, body) = describe_stack(&router, &stack).await;
        assert_eq!(
            code, 200,
            "case {case:?}: the deployed stack must be readable, got {code} {body}"
        );
        let stack_status = xml_text(&body, "StackStatus").unwrap_or_default();
        assert!(
            stack_status.ends_with("_COMPLETE"),
            "case {case:?}: the execution must have carried the change through, got \
             {stack_status:?}"
        );

        Self {
            router,
            stack,
            stack_id,
            executed_arn,
            executed_name,
            held_arn,
            held_name,
            stack_status,
            other_stack,
            other_arn,
            other_name,
        }
    }

    /// Assert the record of the execution that deployed the stack, the second stack's plan, the
    /// deployed stack's own status and the resources it carries are all exactly as the fixture
    /// left them — what a delete of anything else may never reach.
    async fn assert_the_rest_is_untouched(&self, case: &str) {
        assert_eq!(
            read_record(&self.router, &self.executed_arn, None).await,
            (
                200,
                self.executed_arn.clone(),
                execution_status::EXECUTE_COMPLETE.to_string()
            ),
            "case {case:?}: the record of the execution that was carried out must stay"
        );
        assert_eq!(
            read_record(&self.router, &self.other_arn, None).await,
            (
                200,
                self.other_arn.clone(),
                execution_status::AVAILABLE.to_string()
            ),
            "case {case:?}: another stack's plan must stay"
        );
        assert_eq!(
            listed(case, &self.router, &self.other_stack).await,
            vec![self.other_name.clone()],
            "case {case:?}: another stack still lists the plan it holds"
        );

        let (code, body) = describe_stack(&self.router, &self.stack).await;
        assert_eq!(
            (code, xml_text(&body, "StackStatus").unwrap_or_default()),
            (200, self.stack_status.clone()),
            "case {case:?}: discarding a plan leaves its stack alone, got {code} {body}"
        );
        let carried = deployed_logical_ids(case, &self.router, &self.stack).await;
        let expected: BTreeSet<String> = ["A".to_string()].into_iter().collect();
        assert_eq!(
            carried, expected,
            "case {case:?}: expected the stack to carry {expected:?}, got {carried:?}"
        );
    }

    /// ...and the plan the stack still has to carry out is there too, addressed both ways.
    async fn assert_the_held_plan_is_untouched(&self, case: &str) {
        assert_eq!(
            read_record(&self.router, &self.held_arn, None).await,
            (
                200,
                self.held_arn.clone(),
                execution_status::AVAILABLE.to_string()
            ),
            "case {case:?}: the plan the stack holds must stay, addressed by its ARN"
        );
        assert_eq!(
            read_record(&self.router, &self.held_name, Some(&self.stack)).await,
            (
                200,
                self.held_arn.clone(),
                execution_status::AVAILABLE.to_string()
            ),
            "case {case:?}: the plan the stack holds must stay, addressed by its name"
        );
        assert_eq!(
            listed(case, &self.router, &self.stack).await,
            vec![self.held_name.clone()],
            "case {case:?}: the stack still lists the plan it holds"
        );
    }
}

#[tokio::test]
async fn a_delete_that_addresses_no_record_takes_nothing() {
    let fixture = Deployed::new("the store every row deletes against", "takes-nothing").await;

    // (case, the ChangeSetName the request carries, the StackName beside it)
    let rows: Vec<(&str, String, Option<String>)> = vec![
        (
            "a name no record of the stack carries",
            "takes-nothing-unplanned-set".to_string(),
            Some(fixture.stack.clone()),
        ),
        (
            "the same name beside the stack's id",
            "takes-nothing-unplanned-set".to_string(),
            Some(fixture.stack_id.clone()),
        ),
        (
            "another stack's change-set name, beside this stack",
            fixture.other_name.clone(),
            Some(fixture.stack.clone()),
        ),
        (
            "an ARN no record carries: the held plan's, with another uuid",
            format!(
                "{}deadbeef",
                &fixture.held_arn[..fixture.held_arn.len() - 8]
            ),
            None,
        ),
        (
            "the held plan's own ARN, beside a stack it does not belong to",
            fixture.held_arn.clone(),
            Some(fixture.other_stack.clone()),
        ),
        (
            "a name on a stack that is not there",
            "takes-nothing-unplanned-set".to_string(),
            Some("takes-nothing-ghost".to_string()),
        ),
    ];

    for (case, addressed, stack) in rows {
        let (code, body) = delete_change_set(&fixture.router, &addressed, stack.as_deref()).await;
        assert_empty_result(case, code, &body);
        fixture.assert_the_held_plan_is_untouched(case).await;
        fixture.assert_the_rest_is_untouched(case).await;
    }
}

#[tokio::test]
async fn a_delete_takes_the_record_it_addressed_and_nothing_else() {
    for addressed_by in [AddressedBy::Arn, AddressedBy::NameAndStack] {
        let case = format!("{addressed_by:?}");
        let prefix = format!("takes-one-{}", case.to_lowercase());
        let fixture = Deployed::new(&case, &prefix).await;
        let (addressed, stack) = match addressed_by {
            AddressedBy::Arn => (fixture.held_arn.clone(), None),
            AddressedBy::NameAndStack => {
                (fixture.held_name.clone(), Some(fixture.stack.clone()))
            }
        };

        let (code, body) = delete_change_set(&fixture.router, &addressed, stack.as_deref()).await;
        assert_empty_result(&case, code, &body);

        // The record the delete addressed is gone: a read of it is the modelled not-found and
        // the stack holds no plan any more.
        let (code, body) = describe_change_set(&fixture.router, &fixture.held_arn, None).await;
        assert_eq!(
            (code, refusal(&body)),
            (
                404,
                (
                    "ChangeSetNotFound".to_string(),
                    format!("ChangeSet [{}] does not exist", fixture.held_arn),
                ),
            ),
            "case {case:?}: expected the discarded record to read back as not found, got \
             {code} {body}"
        );
        let names = listed(&case, &fixture.router, &fixture.stack).await;
        assert!(
            names.is_empty(),
            "case {case:?}: expected the stack to hold no plan, got {names:?}"
        );
        // ...and nothing else moved: not the record of the execution that deployed the stack,
        // not the other stack's plan, not the stack itself.
        fixture.assert_the_rest_is_untouched(&case).await;
        assert_eq!(
            read_record(
                &fixture.router,
                &fixture.executed_name,
                Some(&fixture.stack)
            )
            .await,
            (
                200,
                fixture.executed_arn.clone(),
                execution_status::EXECUTE_COMPLETE.to_string()
            ),
            "case {case:?}: the executed record answers under the name it deployed under"
        );

        // Repeating the very same request now addresses no record at all, so it is the same
        // empty success and takes nothing more.
        let repeated = format!("{case} repeated");
        let (code, body) = delete_change_set(&fixture.router, &addressed, stack.as_deref()).await;
        assert_empty_result(&repeated, code, &body);
        fixture.assert_the_rest_is_untouched(&repeated).await;
    }
}
