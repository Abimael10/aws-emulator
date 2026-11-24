// SPDX-License-Identifier: CC0-1.0

//! What becomes of a change set once its own execution has been CARRIED OUT, end to end over
//! the awsQuery wire.
//!
//! RULE: a change set is a PLAN its stack holds until that plan is carried out. An execution
//! that completes carries it out — the plan became the revision the stack now carries — so the
//! stack holds that record no longer: no read of the stack's change sets reports it
//! (`ListChangeSets` does not list it) and the NAME it deployed under addresses no change set,
//! so `DescribeChangeSet` for that name answers the modelled `ChangeSetNotFound` and the next
//! deployment may plan under it. The RECORD itself stays, addressed by its own ARN: it reports
//! the execution it carried out, which is what keeps `ExecuteChangeSet` for it answered on that
//! execution rather than as a change set that never existed. Every plan the stack has still to
//! carry out is held, whatever state it is in — one being computed, one ready to execute, one
//! being applied right now, one another execution superseded, one whose creation left no plan
//! and one whose execution did not carry its change through.
//!
//! The table below spans every value of the model's `ExecutionStatus`, so which states a stack
//! holds a record in is pinned over the whole enum rather than over one of them, and the two
//! cases over the real path cover both change-set types whose execution changes a stack.

use std::sync::Arc;

use service_com_amazonaws_cloudformation_cloudformation::state::{
    change_set_status, execution_status,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::{
    create_change_set, describe_change_set, execute_change_set, settle_the_store,
};
use crate::harness::{
    call, create_stack, member, refusal, router_with_engine, store, xml_text, xml_texts, REGION,
};

/// The template a stack is created with, and the ones a plan would deploy over it: one more
/// resource beside the topic, so each plan is a real change.
const TOPIC: &str = r#"{"Resources":{"A":{"Type":"AWS::SNS::Topic"}}}"#;
const TOPIC_AND_GROUP: &str =
    r#"{"Resources":{"A":{"Type":"AWS::SNS::Topic"},"G":{"Type":"AWS::Logs::LogGroup"}}}"#;
const RENAMED_TOPIC_AND_GROUP: &str = r#"{"Resources":{
        "A":{"Type":"AWS::SNS::Topic","Properties":{"DisplayName":"renamed"}},
        "G":{"Type":"AWS::Logs::LogGroup"}}}"#;

/// The change-set names `ListChangeSets` reports for one stack.
async fn listed(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (code, body) = call(router, "ListChangeSets", &[member("StackName", stack)]).await;
    assert_eq!(code, 200, "ListChangeSets must succeed, got {code} {body}");
    xml_texts(&body, "ChangeSetName")
}

/// Create one change set of `stack` and answer the ARN it reports for itself.
async fn plan(
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
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: the change set must be created, got {code} {body}"
    );
    xml_text(&body, "Id")
        .unwrap_or_else(|| panic!("case {case:?}: CreateChangeSet answers the change set's ARN"))
}

/// Put the change set into the `(ChangeSetStatus, ExecutionStatus)` pair a row is about.
///
/// The statuses a change set reaches by itself are driven through the wire by the two cases
/// below; the table is about WHICH states the stack holds a record in, so it states the pair
/// directly and can cover every status without the flow that reaches it (another change set's
/// execution for `OBSOLETE`, a failing deployment for `EXECUTE_FAILED`).
fn put_in_status(
    engine: &Arc<engine::InMemoryEngine>,
    stack: &str,
    change_set: &str,
    status: &str,
    execution: &str,
) {
    let state = store(engine);
    let mut held = state.lock();
    let record = held
        .resolve_change_set_mut(REGION, change_set, Some(stack))
        .expect("the change set the row created");
    record.pending = None;
    record.status = status.to_string();
    record.execution_status = execution.to_string();
}

/// Every value of the model's `ExecutionStatus`
/// (`smithy_query(query="shape",shape="ExecutionStatus")` = `[UNAVAILABLE, AVAILABLE,
/// EXECUTE_IN_PROGRESS, EXECUTE_COMPLETE, EXECUTE_FAILED, OBSOLETE]`), with whether the stack
/// still HOLDS a record in it.
const EVERY_EXECUTION_STATUS: [(&str, bool); 6] = [
    // (the ExecutionStatus, whether the stack holds the record in it)
    (execution_status::UNAVAILABLE, true),
    (execution_status::AVAILABLE, true),
    (execution_status::EXECUTE_IN_PROGRESS, true),
    // The one state the plan is no longer the stack's to carry out: it was carried out.
    (execution_status::EXECUTE_COMPLETE, false),
    // An execution that did not carry its change through left the stack where it was, so the
    // record is still one of the stack's own.
    (execution_status::EXECUTE_FAILED, true),
    (execution_status::OBSOLETE, true),
];

#[tokio::test]
async fn a_stack_holds_every_plan_but_the_one_an_execution_carried_out() {
    let (router, engine) = router_with_engine();

    for (execution, held) in EVERY_EXECUTION_STATUS {
        let case = execution;
        let stack = format!("holds-{}", execution.to_lowercase().replace('_', "-"));
        let change_set = format!("{stack}-set");
        let arn = plan(case, &router, &stack, &change_set, "CREATE", TOPIC).await;
        settle_the_store(&router, &engine).await;
        put_in_status(
            &engine,
            &stack,
            &change_set,
            change_set_status::CREATE_COMPLETE,
            execution,
        );

        // The NAME addresses the record only while the stack holds it as a plan.
        let (code, body) = describe_change_set(&router, &change_set, Some(&stack)).await;
        if held {
            assert_eq!(
                (code, xml_text(&body, "ExecutionStatus").unwrap_or_default()),
                (200, execution.to_string()),
                "case {case:?}: the name must address the plan the stack holds, got {code} \
                 {body}"
            );
        } else {
            assert_eq!(
                (code, refusal(&body)),
                (
                    404,
                    (
                        "ChangeSetNotFound".to_string(),
                        format!("ChangeSet [{change_set}] does not exist"),
                    ),
                ),
                "case {case:?}: the name must address no plan the stack holds, got {code} \
                 {body}"
            );
        }

        // ...and so does a read of the stack's change sets.
        let names = listed(&router, &stack).await;
        let expected: Vec<String> = if held {
            vec![change_set.clone()]
        } else {
            Vec::new()
        };
        assert_eq!(
            names, expected,
            "case {case:?}: expected the stack to list {expected:?}, got {names:?}"
        );

        // The RECORD is there either way, addressed by its own ARN.
        let (code, body) = describe_change_set(&router, &arn, None).await;
        assert_eq!(
            (
                code,
                xml_text(&body, "ChangeSetId").unwrap_or_default(),
                xml_text(&body, "ExecutionStatus").unwrap_or_default(),
            ),
            (200, arn.clone(), execution.to_string()),
            "case {case:?}: the ARN must address the record whatever became of the plan, got \
             {code} {body}"
        );
    }
}

/// The same rule over the REAL path, for each change-set type whose execution changes a stack:
/// the execution carries the plan out, the name it deployed under is free for the next plan —
/// with no delete in between — and the record of that execution answers under its own ARN.
#[tokio::test]
async fn an_execution_frees_the_name_it_deployed_under_for_the_next_plan() {
    // (case, the change-set type, the template the executed plan deploys, the template the
    // plan that re-uses the name would deploy)
    for (case, change_set_type, applied, next) in [
        ("a change set that creates its stack", "CREATE", TOPIC, TOPIC_AND_GROUP),
        (
            "a change set that updates a stack that stands",
            "UPDATE",
            TOPIC_AND_GROUP,
            RENAMED_TOPIC_AND_GROUP,
        ),
    ] {
        let (router, engine) = router_with_engine();
        let stack = format!("freed-{change_set_type}").to_lowercase();
        let change_set = "deploy-change-set";
        if change_set_type == "UPDATE" {
            let (code, body) = create_stack(&router, &stack, TOPIC, &[]).await;
            assert_eq!(
                code, 200,
                "case {case:?}: the stack the plan updates must exist, got {code} {body}"
            );
            settle_the_store(&router, &engine).await;
        }
        let arn = plan(case, &router, &stack, change_set, change_set_type, applied).await;
        settle_the_store(&router, &engine).await;

        let (code, body) = execute_change_set(&router, change_set, Some(&stack)).await;
        assert_eq!(
            code, 200,
            "case {case:?}: the plan must execute, got {code} {body}"
        );
        settle_the_store(&router, &engine).await;
        let (code, body) = describe_change_set(&router, &arn, None).await;
        assert_eq!(
            (code, xml_text(&body, "ExecutionStatus").unwrap_or_default()),
            (200, execution_status::EXECUTE_COMPLETE.to_string()),
            "case {case:?}: the execution must have been carried out, got {code} {body}"
        );

        // The stack holds that plan no longer: its name addresses nothing and the stack lists
        // no change set at all.
        let (code, body) = describe_change_set(&router, change_set, Some(&stack)).await;
        assert_eq!(
            (code, refusal(&body).0),
            (404, "ChangeSetNotFound".to_string()),
            "case {case:?}: the name of the plan that was carried out addresses nothing, got \
             {code} {body}"
        );
        let names = listed(&router, &stack).await;
        assert!(
            names.is_empty(),
            "case {case:?}: expected the stack to list no change set, got {names:?}"
        );

        // ...so the next deployment plans under that very name, with no delete in between,
        // and gets a record of its own.
        let next_arn = plan(case, &router, &stack, change_set, "UPDATE", next).await;
        assert_ne!(
            next_arn, arn,
            "case {case:?}: the new plan must be a record of its own, got the executed one's ARN"
        );
        settle_the_store(&router, &engine).await;
        let (code, body) = describe_change_set(&router, change_set, Some(&stack)).await;
        assert_eq!(
            (
                code,
                xml_text(&body, "ChangeSetId").unwrap_or_default(),
                xml_text(&body, "ExecutionStatus").unwrap_or_default(),
            ),
            (
                200,
                next_arn.clone(),
                execution_status::AVAILABLE.to_string()
            ),
            "case {case:?}: the name must address the plan the stack now holds, got {code} \
             {body}"
        );
        assert_eq!(
            listed(&router, &stack).await,
            vec![change_set.to_string()],
            "case {case:?}: the stack lists the plan it holds"
        );
        // ...and the record of the execution that was carried out is still there, under its
        // own ARN, refusing another execution on the execution it carried out.
        let (code, body) = describe_change_set(&router, &arn, None).await;
        assert_eq!(
            (
                code,
                xml_text(&body, "ChangeSetId").unwrap_or_default(),
                xml_text(&body, "ExecutionStatus").unwrap_or_default(),
            ),
            (
                200,
                arn.clone(),
                execution_status::EXECUTE_COMPLETE.to_string()
            ),
            "case {case:?}: the executed record must stay addressable by its ARN, got {code} \
             {body}"
        );
        let (code, body) = execute_change_set(&router, &arn, None).await;
        assert_eq!(
            (code, refusal(&body)),
            (
                400,
                (
                    "InvalidChangeSetStatus".to_string(),
                    format!(
                        "ChangeSet [{arn}] cannot be executed in its current execution status \
                         of [{}]",
                        execution_status::EXECUTE_COMPLETE
                    ),
                ),
            ),
            "case {case:?}: expected the execution that happened to refuse the call, got \
             {code} {body}"
        );
    }
}
