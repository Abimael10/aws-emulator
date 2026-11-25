// SPDX-License-Identifier: CC0-1.0

//! How a request ADDRESSES the change-set record it acts on, once that record is no longer a
//! plan the stack has to carry out — driven the whole way over the awsQuery wire.
//!
//! The rule the rows assert, one level above the recorded case (a second `ExecuteChangeSet`
//! naming a change set whose execution had been carried out, answered as a change set that
//! never existed):
//!
//! > A change-set record is addressed by its own ARN, and by the `ChangeSetName` it was
//! > created under beside its stack ("if you specify the name of a change set, you must also
//! > specify the stack name"), for as long as the record is there — whatever became of the
//! > plan it describes. So a call that would act on such a record again is decided from the
//! > record: it reads back with the `ExecutionStatus` it reached, and an execution of it is
//! > refused `InvalidChangeSetStatus` with `ChangeSet [<ChangeSetId>] cannot be executed in
//! > its current execution status of [<ExecutionStatus>]`, whichever of the two ways the
//! > request named it. Only a `ChangeSetName` no record of that stack carries — and only a
//! > record the store no longer holds at all — is answered the modelled
//! > `ChangeSetNotFound`.
//!
//! The rows span what the rule generalises over: HOW the request addresses the record (its
//! own ARN, and the name it was created under beside its stack name and beside its stack id),
//! WHAT became of its plan (an execution carried out, an execution that did not carry its
//! change through, a plan another execution superseded) and WHICH operation acts on it
//! (`DescribeChangeSet` and `ExecuteChangeSet`). The names that address nothing are rows of
//! their own, so the not-found answer is pinned beside the refusals rather than assumed.
//!
//! `change_set_status_refusals` holds the refusal WORDING over every `ExecutionStatus` the
//! model declares and `executed_change_set_frees_its_name` what the stack still HOLDS; this
//! suite is about the addressing.

use service_com_amazonaws_cloudformation_cloudformation::state::{
    change_set_status, execution_status,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::{
    create_change_set, describe_change_set, execute_change_set, settle_the_store,
};
use crate::harness::{member, refusal, router_with_engine, store, xml_text, REGION};

/// The template every plan of this suite deploys: one topic, which makes each plan a real
/// change against the empty stack it creates.
const TOPIC: &str = r#"{"Resources":{"A":{"Type":"AWS::SNS::Topic"}}}"#;

/// One state a record's plan can have reached once nothing can execute it any more, with the
/// `ExecutionStatus` the record then reports.
///
/// Every value here is one of the model's own
/// (`smithy_query(query="shape",shape="ExecutionStatus")`): the execution that was carried
/// out, the one that did not carry its change through, and the plan another execution
/// superseded.
const REACHED: [(&str, &str); 3] = [
    ("an execution that was carried out", execution_status::EXECUTE_COMPLETE),
    (
        "an execution that did not carry its change through",
        execution_status::EXECUTE_FAILED,
    ),
    (
        "a plan another execution superseded",
        execution_status::OBSOLETE,
    ),
];

/// How a request names the record: the label for the case, and whether the name is the
/// record's ARN.
#[derive(Clone, Copy)]
enum Addressing {
    /// The record's own ARN, which names it on its own.
    Arn,
    /// The `ChangeSetName` it was created under, beside the stack's NAME.
    NameAndStackName,
    /// The same name, beside the stack's ID — the other value `StackName` accepts.
    NameAndStackId,
}

impl Addressing {
    fn case(self) -> &'static str {
        match self {
            Self::Arn => "the record's own ARN",
            Self::NameAndStackName => "the name it was created under, beside the stack name",
            Self::NameAndStackId => "the name it was created under, beside the stack id",
        }
    }
}

/// The `(ChangeSetName, StackName)` pair a request carries to address `record` this way.
fn addressed(
    addressing: Addressing,
    change_set_name: &str,
    change_set_id: &str,
    stack_name: &str,
    stack_id: &str,
) -> (String, Option<String>) {
    match addressing {
        Addressing::Arn => (change_set_id.to_string(), None),
        Addressing::NameAndStackName => {
            (change_set_name.to_string(), Some(stack_name.to_string()))
        }
        Addressing::NameAndStackId => (change_set_name.to_string(), Some(stack_id.to_string())),
    }
}

/// Put the record into the `ExecutionStatus` a row is about, directly in the store.
///
/// The statuses a record reaches by itself are driven over the wire by the suites named in
/// this module's docs; what these rows are about is the ADDRESSING, so each one states the
/// status it addresses a record in and covers all three without the flow that reaches each
/// (another change set's execution for `OBSOLETE`, a failing deployment for
/// `EXECUTE_FAILED`).
fn put_in_execution_status(
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    stack: &str,
    change_set: &str,
    execution: &str,
) {
    let state = store(engine);
    let mut held = state.lock();
    let record = held
        .resolve_change_set_mut(REGION, change_set, Some(stack))
        .expect("the change set the row created");
    record.pending = None;
    record.status = change_set_status::CREATE_COMPLETE.to_string();
    record.execution_status = execution.to_string();
}

/// Create one `CREATE` change set of `stack` and answer the ARN it reports for itself and the
/// id of the stack it brought into review.
async fn plan(
    case: &str,
    router: &MultiServiceRouter,
    stack: &str,
    change_set: &str,
) -> (String, String) {
    let (code, body) = create_change_set(
        router,
        stack,
        change_set,
        &[
            member("ChangeSetType", "CREATE"),
            member("TemplateBody", TOPIC),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: the change set must be created, got {code} {body}"
    );
    let change_set_id = xml_text(&body, "Id")
        .unwrap_or_else(|| panic!("case {case:?}: CreateChangeSet answers the change set's ARN"));
    let stack_id = xml_text(&body, "StackId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateChangeSet answers the stack's id"));
    (change_set_id, stack_id)
}

/// Every way of naming a record resolves the same record, so the call is decided from what
/// that record reached rather than answered as a change set that never existed.
#[tokio::test]
async fn a_record_is_addressed_by_its_arn_and_by_the_name_it_was_created_under() {
    let (router, engine) = router_with_engine();

    for (index, (reached, execution)) in REACHED.iter().enumerate() {
        for addressing in [
            Addressing::Arn,
            Addressing::NameAndStackName,
            Addressing::NameAndStackId,
        ] {
            let case = format!("{reached} / {}", addressing.case());
            // One stack per row, so no row depends on what another left behind.
            let stack = format!("addressed-{index}-{}", addressing.case().len());
            let change_set = format!("{stack}-set");
            let (change_set_id, stack_id) = plan(&case, &router, &stack, &change_set).await;
            settle_the_store(&router, &engine).await;
            put_in_execution_status(&engine, &stack, &change_set, execution);

            let (named, stack_named) =
                addressed(addressing, &change_set, &change_set_id, &stack, &stack_id);
            let stack_named = stack_named.as_deref();

            // The read resolves the record and reports what its plan reached.
            let (code, body) = describe_change_set(&router, &named, stack_named).await;
            assert_eq!(
                (
                    code,
                    xml_text(&body, "ChangeSetId").unwrap_or_default(),
                    xml_text(&body, "ExecutionStatus").unwrap_or_default(),
                ),
                (200, change_set_id.clone(), (*execution).to_string()),
                "case {case:?}: the read must resolve the record, got {code} {body}"
            );

            // ...and an execution of it is refused on the status that record is in, naming
            // the record's own ARN however the request addressed it.
            let (code, body) = execute_change_set(&router, &named, stack_named).await;
            assert_eq!(
                (code, refusal(&body)),
                (
                    400,
                    (
                        "InvalidChangeSetStatus".to_string(),
                        format!(
                            "ChangeSet [{change_set_id}] cannot be executed in its current \
                             execution status of [{execution}]"
                        ),
                    ),
                ),
                "case {case:?}: the execution must be refused on the record's status, got \
                 {code} {body}"
            );
        }
    }
}

/// What IS answered the modelled not-found: a `ChangeSetName` no record of the stack carries,
/// and an ARN no record carries — so the refusals above are about the record that is there
/// rather than about every request.
#[tokio::test]
async fn a_name_no_record_of_the_stack_carries_is_answered_not_found() {
    let (router, engine) = router_with_engine();
    let stack = "addressed-absent";
    let change_set = "addressed-absent-set";
    let (change_set_id, stack_id) = plan("the stack's own plan", &router, stack, change_set).await;
    // A second stack with a plan of its own, whose change-set name this stack carries no
    // record under: a name is resolved WITHIN the stack the request names.
    let other_stack = "addressed-absent-other";
    let other_change_set = "addressed-absent-other-set";
    plan("another stack's plan", &router, other_stack, other_change_set).await;
    settle_the_store(&router, &engine).await;

    // (case, the ChangeSetName the request carries, the StackName beside it)
    let rows: Vec<(&str, String, Option<String>)> = vec![
        (
            "a name no record of this stack carries",
            "addressed-absent-unplanned".to_string(),
            Some(stack.to_string()),
        ),
        (
            "the same name beside the stack's id",
            "addressed-absent-unplanned".to_string(),
            Some(stack_id.clone()),
        ),
        (
            "another stack's change-set name, beside this stack",
            other_change_set.to_string(),
            Some(stack.to_string()),
        ),
        (
            "an ARN no record carries: this stack's own, with another uuid",
            format!("{}deadbeef", &change_set_id[..change_set_id.len() - 8]),
            None,
        ),
    ];
    for (case, named, stack_named) in rows {
        let (code, body) = describe_change_set(&router, &named, stack_named.as_deref()).await;
        assert_eq!(
            (code, refusal(&body).0),
            (404, "ChangeSetNotFound".to_string()),
            "case {case:?}: expected the modelled not-found, got {code} {body}"
        );
    }
}
