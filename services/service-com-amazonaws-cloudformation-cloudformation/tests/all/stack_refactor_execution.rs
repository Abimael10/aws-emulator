// SPDX-License-Identifier: CC0-1.0

//! EXECUTING a stack refactor, end to end over the awsQuery wire: `ExecuteStackRefactor`
//! carries out the plan `CreateStackRefactor` concluded, and the stacks it covers report what
//! it did.
//!
//! **The rule every case here holds the service to: executing a refactor carries out ITS PLAN —
//! every stack the plan is to create is created, and every resource it is to move leaves the
//! stack that holds it for the stack that is to hold it, under the logical id the move names
//! and as the SAME physical instance, while the template each definition submitted becomes its
//! stack's. The refactor reports `EXECUTE_IN_PROGRESS` until the conclusion the clock owes falls
//! due and `EXECUTE_COMPLETE` after it; a plan that no longer holds against the stacks as they
//! stand concludes `EXECUTE_FAILED` with the reason and moves nothing; and a request to execute
//! a refactor with no plan anything can execute is refused.**
//!
//! The rows span the class the rule generalises over rather than one recorded case: a resource
//! renamed inside ONE stack, a resource moved between two stacks that both exist, a resource
//! moved into a stack the refactor has to create, TWO resources moved out of one stack, and a
//! refactor that changes a stack's template and moves nothing — a refactor that creates no
//! resource either, since a refactor moves resources rather than provisioning them.

use std::collections::BTreeMap;
use std::time::Instant;

use service_com_amazonaws_cloudformation_cloudformation::state::stack_refactors as refactors;
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};
use smithy_server_runtime::MultiServiceRouter;

use crate::generated_templates::{deploy, error_code};
use crate::stack_lifecycle::{
    call, call_in, get_template, list_members, member, router, settled_stack,
    wait_for_the_transition, xml_text, REGION,
};
use crate::stack_refactors::{
    create_stack_refactor, reportable, Definition, Mapping, NO_RESOURCES, ONE_TOPIC,
    TOPIC_AND_BUCKET,
};
use crate::stack_resources::{resources_of, wire_resources};

/// A second Region, for the case that executes a refactor another Region holds.
const OTHER_REGION: &str = "us-west-2";

/// The same one resource under another logical id: the final state of a stack whose resource
/// the refactor RENAMES, which is a move whose two ends are the same stack.
const RENAMED_TOPIC: &str = r#"{"Resources":{"Renamed":{"Type":"AWS::SNS::Topic"}}}"#;

/// `ExecuteStackRefactor` for one refactor, by the id the create answered.
async fn execute_stack_refactor(
    router: &MultiServiceRouter,
    stack_refactor_id: &str,
) -> (u16, String) {
    execute_stack_refactor_in(router, REGION, stack_refactor_id).await
}

/// [`execute_stack_refactor`], signed for one Region: a refactor is executable only in the
/// Region the call that created it was signed for.
async fn execute_stack_refactor_in(
    router: &MultiServiceRouter,
    region: &str,
    stack_refactor_id: &str,
) -> (u16, String) {
    call_in(
        router,
        region,
        "ExecuteStackRefactor",
        &[member("StackRefactorId", stack_refactor_id)],
    )
    .await
}

/// `DescribeStackRefactor` for one refactor, by the id the create answered.
async fn describe_stack_refactor(
    router: &MultiServiceRouter,
    stack_refactor_id: &str,
) -> (u16, String) {
    call(
        router,
        "DescribeStackRefactor",
        &[member("StackRefactorId", stack_refactor_id)],
    )
    .await
}

/// What one stack reports for each resource it holds right now: its physical id and the
/// resource-level status, by logical id.
async fn held_resources(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> BTreeMap<String, (String, String)> {
    let (status_code, body) = resources_of(router, stack).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected the resources of {stack} to be reported, got {status_code} {body}"
    );
    wire_resources(&body)
        .into_iter()
        .filter_map(|resource| {
            Some((
                resource.logical_id?,
                (
                    resource.physical_id.unwrap_or_default(),
                    resource.status.unwrap_or_default(),
                ),
            ))
        })
        .collect()
}

/// The `TemplateBody` one stack carries, read back through `GetTemplate`.
async fn template_of(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let (status_code, body) = get_template(router, stack, None).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected the template of {stack} to be served, got {status_code} {body}"
    );
    xml_text(&body, "TemplateBody")
        .unwrap_or_else(|| panic!("case {case:?}: expected a TemplateBody for {stack}, got {body}"))
}

/// One move the execution has to carry out: where the resource is now, and where it has to end
/// up — `(source stack, source logical id, destination stack, destination logical id)`.
type Move = (&'static str, &'static str, &'static str, &'static str);

/// One row of the execution table.
struct ExecutionCase {
    case: &'static str,
    /// The stacks deployed before the refactor, one `(name, template)` each.
    stacks: &'static [(&'static str, &'static str)],
    enable_stack_creation: Option<bool>,
    definitions: Vec<Definition>,
    mappings: Vec<Mapping>,
    /// The moves executing the refactor has to carry out.
    moved: &'static [Move],
    /// The stacks executing it has to CREATE.
    creates: &'static [&'static str],
    /// The template each stack has to carry once the refactor has been executed.
    final_templates: &'static [(&'static str, &'static str)],
}

/// The rows of the execution table.
fn execution_cases() -> Vec<ExecutionCase> {
    vec![
        ExecutionCase {
            // A move whose two ends are the SAME stack: the resource keeps its stack and its
            // physical instance and is known by another logical id, which is what renaming a
            // resource is.
            case: "a resource renamed inside one stack",
            stacks: &[("refactor-exec-rename", ONE_TOPIC)],
            enable_stack_creation: Some(false),
            definitions: vec![Definition::inline("refactor-exec-rename", RENAMED_TOPIC)],
            mappings: vec![Mapping {
                source: ("refactor-exec-rename", "Topic"),
                destination: ("refactor-exec-rename", "Renamed"),
            }],
            moved: &[(
                "refactor-exec-rename",
                "Topic",
                "refactor-exec-rename",
                "Renamed",
            )],
            creates: &[],
            final_templates: &[("refactor-exec-rename", RENAMED_TOPIC)],
        },
        ExecutionCase {
            case: "a resource moved between two stacks that both exist",
            stacks: &[
                ("refactor-exec-source", ONE_TOPIC),
                ("refactor-exec-target", NO_RESOURCES),
            ],
            enable_stack_creation: Some(false),
            definitions: vec![
                Definition::inline("refactor-exec-source", NO_RESOURCES),
                Definition::inline("refactor-exec-target", ONE_TOPIC),
            ],
            mappings: vec![Mapping {
                source: ("refactor-exec-source", "Topic"),
                destination: ("refactor-exec-target", "Topic"),
            }],
            moved: &[(
                "refactor-exec-source",
                "Topic",
                "refactor-exec-target",
                "Topic",
            )],
            creates: &[],
            final_templates: &[
                ("refactor-exec-source", NO_RESOURCES),
                ("refactor-exec-target", ONE_TOPIC),
            ],
        },
        ExecutionCase {
            // "Determines if a new stack is created with the refactor": the destination does
            // not exist yet, so executing the refactor is what creates it.
            case: "a resource moved into a stack the refactor creates",
            stacks: &[("refactor-exec-mover", ONE_TOPIC)],
            enable_stack_creation: Some(true),
            definitions: vec![
                Definition::inline("refactor-exec-mover", NO_RESOURCES),
                Definition::inline("refactor-exec-fresh", ONE_TOPIC),
            ],
            mappings: vec![Mapping {
                source: ("refactor-exec-mover", "Topic"),
                destination: ("refactor-exec-fresh", "Topic"),
            }],
            moved: &[(
                "refactor-exec-mover",
                "Topic",
                "refactor-exec-fresh",
                "Topic",
            )],
            creates: &["refactor-exec-fresh"],
            final_templates: &[
                ("refactor-exec-mover", NO_RESOURCES),
                ("refactor-exec-fresh", ONE_TOPIC),
            ],
        },
        ExecutionCase {
            // TWO moves, out of one stack into another: the count of what one execution
            // carries out is a caller's choice, not one.
            case: "two resources moved out of one stack",
            stacks: &[
                ("refactor-exec-pair", TOPIC_AND_BUCKET),
                ("refactor-exec-split", NO_RESOURCES),
            ],
            enable_stack_creation: Some(false),
            definitions: vec![
                Definition::inline("refactor-exec-pair", NO_RESOURCES),
                Definition::inline("refactor-exec-split", TOPIC_AND_BUCKET),
            ],
            mappings: vec![
                Mapping {
                    source: ("refactor-exec-pair", "Alpha"),
                    destination: ("refactor-exec-split", "Alpha"),
                },
                Mapping {
                    source: ("refactor-exec-pair", "Bucket"),
                    destination: ("refactor-exec-split", "Bucket"),
                },
            ],
            moved: &[
                ("refactor-exec-pair", "Alpha", "refactor-exec-split", "Alpha"),
                (
                    "refactor-exec-pair",
                    "Bucket",
                    "refactor-exec-split",
                    "Bucket",
                ),
            ],
            creates: &[],
            final_templates: &[
                ("refactor-exec-pair", NO_RESOURCES),
                ("refactor-exec-split", TOPIC_AND_BUCKET),
            ],
        },
        ExecutionCase {
            // A refactor that moves NOTHING: the stack ends up with the template its
            // definition submitted, and the resource that template newly declares is not
            // created — a refactor moves resources rather than provisioning them.
            case: "a final template and no move at all",
            stacks: &[("refactor-exec-retemplate", ONE_TOPIC)],
            enable_stack_creation: Some(false),
            definitions: vec![Definition::inline(
                "refactor-exec-retemplate",
                TOPIC_AND_BUCKET,
            )],
            mappings: vec![],
            moved: &[],
            creates: &[],
            final_templates: &[("refactor-exec-retemplate", TOPIC_AND_BUCKET)],
        },
    ]
}

/// Executing a refactor carries out its plan: the stacks it creates, the resources it moves and
/// the template each stack ends up with — reported `EXECUTE_IN_PROGRESS` while it runs and
/// `EXECUTE_COMPLETE` once the clock has made that conclusion due.
#[tokio::test]
async fn executing_a_refactor_carries_out_its_plan() {
    for row in execution_cases() {
        let case = row.case;
        let router = router();

        for (name, template) in row.stacks {
            deploy(&router, name, template).await;
        }
        // What each stack held BEFORE the refactor: the physical instance a move has to keep is
        // the one the stack reported then.
        let mut before: BTreeMap<&str, BTreeMap<String, (String, String)>> = BTreeMap::new();
        for (name, _) in row.stacks {
            before.insert(name, held_resources(&router, case, name).await);
        }

        let (status_code, body) = create_stack_refactor(
            &router,
            Some(case),
            row.enable_stack_creation,
            &row.definitions,
            &row.mappings,
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected CreateStackRefactor to succeed, got {status_code} {body}"
        );
        let stack_refactor_id = xml_text(&body, "StackRefactorId")
            .unwrap_or_else(|| panic!("case {case:?}: expected a StackRefactorId, got {body}"));
        wait_for_the_transition().await;
        let (_, body) = describe_stack_refactor(&router, &stack_refactor_id).await;
        assert_eq!(
            xml_text(&body, "ExecutionStatus").as_deref(),
            Some(refactors::execution_status::AVAILABLE),
            "case {case:?}: expected the concluded creation to leave a plan to execute, got {body}"
        );

        let started = Instant::now();
        let (status_code, body) = execute_stack_refactor(&router, &stack_refactor_id).await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected ExecuteStackRefactor to be accepted, got {status_code} {body}"
        );
        // An operation whose modelled output carries no member still answers in its own
        // envelope, which is what an SDK reads the call's success out of.
        assert!(
            body.contains("<ExecuteStackRefactorResponse"),
            "case {case:?}: expected the ExecuteStackRefactorResponse envelope, got {body:?}"
        );
        assert!(
            xml_text(&body, "RequestId").is_some_and(|request_id| !request_id.is_empty()),
            "case {case:?}: expected the envelope to carry a RequestId, got {body:?}"
        );

        // Read straight after the call: the execution is still being carried out.
        let sent_at = Instant::now();
        let (status_code, body) = describe_stack_refactor(&router, &stack_refactor_id).await;
        let answered_at = Instant::now();
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected DescribeStackRefactor to succeed, got {status_code} {body}"
        );
        let allowed = reportable(
            started,
            sent_at,
            answered_at,
            refactors::execution_status::EXECUTE_IN_PROGRESS,
            refactors::execution_status::EXECUTE_COMPLETE,
        );
        assert!(
            xml_text(&body, "ExecutionStatus")
                .as_deref()
                .is_some_and(|reported| allowed.contains(&reported)),
            "case {case:?}: expected one of {allowed:?} while the execution runs, got {body}"
        );
        assert_eq!(
            xml_text(&body, "Status").as_deref(),
            Some(refactors::status::CREATE_COMPLETE),
            "case {case:?}: the refactor's own creation concluded before this, got {body}"
        );

        wait_for_the_transition().await;
        let (_, body) = describe_stack_refactor(&router, &stack_refactor_id).await;
        assert_eq!(
            xml_text(&body, "ExecutionStatus").as_deref(),
            Some(refactors::execution_status::EXECUTE_COMPLETE),
            "case {case:?}: expected the execution to conclude EXECUTE_COMPLETE, got {body}"
        );
        assert_eq!(
            xml_text(&body, "ExecutionStatusReason"),
            None,
            "case {case:?}: an execution that was carried out explains nothing, got {body}"
        );

        // Every stack the refactor created exists and concluded its own create.
        let reported_ids = list_members(&body, "StackIds");
        for created in row.creates {
            let settled = settled_stack(&router, case, created).await;
            assert_eq!(
                xml_text(&settled, "StackStatus").as_deref(),
                Some(status::CREATE_COMPLETE),
                "case {case:?}: expected {created} to have been created, got {settled}"
            );
            // "The unique ID for each stack" the refactor covers: a stack it was still to
            // create had no id to report until the execution brought it into existence.
            let stack_id = xml_text(&settled, "StackId").unwrap_or_default();
            assert!(
                reported_ids.contains(&stack_id),
                "case {case:?}: expected the refactor to report the id of {created}, got \
                 {reported_ids:?}"
            );
        }
        // ...and every stack it acted on concluded the update it opened on it.
        for (name, _) in row.stacks {
            let settled = settled_stack(&router, case, name).await;
            assert_eq!(
                xml_text(&settled, "StackStatus").as_deref(),
                Some(status::UPDATE_COMPLETE),
                "case {case:?}: expected {name} to have concluded its update, got {settled}"
            );
        }

        // The moves: the resource is gone from the stack that held it and is reported by the
        // stack that is to hold it, as the same physical instance it already was.
        for (source, source_id, destination, destination_id) in row.moved {
            let held = held_resources(&router, case, source).await;
            assert!(
                !held.contains_key(*source_id),
                "case {case:?}: expected {source_id} to have left {source}, got {held:?}"
            );
            let arrived = held_resources(&router, case, destination).await;
            let (physical_id, reported_status) = arrived
                .get(*destination_id)
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: expected {destination} to hold {destination_id}, got \
                         {arrived:?}"
                    )
                })
                .clone();
            let (was, _) = before
                .get(source)
                .and_then(|held| held.get(*source_id))
                .unwrap_or_else(|| {
                    panic!("case {case:?}: {source} never held {source_id} to move")
                })
                .clone();
            assert_eq!(
                physical_id, was,
                "case {case:?}: expected {destination_id} to be the instance {source_id} already \
                 was, got {physical_id:?}"
            );
            assert_eq!(
                reported_status,
                resource_status::IMPORT_COMPLETE,
                "case {case:?}: expected the moved resource to report the status of one brought \
                 under a stack's management, got {reported_status:?}"
            );
        }

        // ...and each stack carries the template its definition submitted.
        for (stack, template) in row.final_templates {
            let served = template_of(&router, case, stack).await;
            assert_eq!(
                served, *template,
                "case {case:?}: expected {stack} to carry the final template it was given, got \
                 {served:?}"
            );
        }
    }
}

/// A plan is concluded against the stacks of the instant the refactor was CREATED, and the
/// stacks can move on: an execution whose plan no longer holds is accepted, concludes
/// `EXECUTE_FAILED` naming what stopped it, and moves nothing.
#[tokio::test]
async fn an_execution_whose_plan_no_longer_holds_concludes_execute_failed() {
    let case = "the moved resource left its stack before the execution";
    let router = router();
    deploy(&router, "refactor-stale-source", ONE_TOPIC).await;
    deploy(&router, "refactor-stale-target", NO_RESOURCES).await;

    let (status_code, body) = create_stack_refactor(
        &router,
        Some(case),
        Some(false),
        &[
            Definition::inline("refactor-stale-source", NO_RESOURCES),
            Definition::inline("refactor-stale-target", ONE_TOPIC),
        ],
        &[Mapping {
            source: ("refactor-stale-source", "Topic"),
            destination: ("refactor-stale-target", "Topic"),
        }],
    )
    .await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected CreateStackRefactor to succeed, got {status_code} {body}"
    );
    let stack_refactor_id = xml_text(&body, "StackRefactorId")
        .unwrap_or_else(|| panic!("case {case:?}: expected a StackRefactorId, got {body}"));
    wait_for_the_transition().await;

    // The resource the plan is to move is taken out of its stack by an update of that stack,
    // so the move has nothing left to act on.
    let (status_code, body) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", "refactor-stale-source"),
            member("TemplateBody", NO_RESOURCES),
        ],
    )
    .await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected the update to be accepted, got {status_code} {body}"
    );
    settled_stack(&router, case, "refactor-stale-source").await;

    let (status_code, body) = execute_stack_refactor(&router, &stack_refactor_id).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected the execution to be accepted, got {status_code} {body}"
    );
    wait_for_the_transition().await;

    let (_, body) = describe_stack_refactor(&router, &stack_refactor_id).await;
    assert_eq!(
        xml_text(&body, "ExecutionStatus").as_deref(),
        Some(refactors::execution_status::EXECUTE_FAILED),
        "case {case:?}: expected the execution to conclude EXECUTE_FAILED, got {body}"
    );
    let reason = xml_text(&body, "ExecutionStatusReason").unwrap_or_default();
    for named in ["Topic", "refactor-stale-source"] {
        assert!(
            reason.contains(named),
            "case {case:?}: expected the reason to name {named:?}, got {reason:?}"
        );
    }
    let held = held_resources(&router, case, "refactor-stale-target").await;
    assert!(
        held.is_empty(),
        "case {case:?}: an execution that failed moved nothing, got {held:?}"
    );
}

/// What a row of the refusal table submits, and what the refusal has to name.
struct RefusalCase {
    case: &'static str,
    /// `true` where the row executes the refactor once first, so the second call is made
    /// against a refactor that is already being executed.
    execute_first: bool,
    /// The Region the refusal is asked for in.
    region: &'static str,
    definitions: Vec<Definition>,
    enable_stack_creation: Option<bool>,
    /// The values the refusal's message has to carry, beside the id the request named.
    names: &'static [&'static str],
}

/// Executing a refactor that has no plan anything can execute is refused 400 `ValidationError`,
/// naming the refactor and the state it is in — and so is an id this account and Region holds no
/// refactor for.
#[tokio::test]
async fn executing_a_refactor_with_no_plan_is_refused() {
    let router = router();
    deploy(&router, "refactor-exec-refused", ONE_TOPIC).await;

    // An id nothing created: there is no refactor to execute at all.
    let unknown = "00000000-0000-0000-0000-000000000000";
    let (status_code, body) = execute_stack_refactor(&router, unknown).await;
    assert_eq!(
        status_code, 400,
        "an id nothing created must be refused, got {status_code} {body}"
    );
    assert_eq!(
        error_code("an id nothing created", &body),
        "ValidationError",
        "an id nothing created is refused as a bad request, got {body}"
    );
    assert!(
        body.contains(unknown),
        "expected the refusal to name the id the request asked for, got {body}"
    );

    let rows = vec![
        RefusalCase {
            // A creation that FAILED leaves nothing to execute, so the plan is `UNAVAILABLE`
            // for good: the definition names a stack this Region holds none of while the
            // request did not ask for one to be created.
            case: "a refactor whose creation failed",
            execute_first: false,
            region: REGION,
            definitions: vec![Definition::named("refactor-exec-never-deployed")],
            enable_stack_creation: Some(false),
            names: &[refactors::execution_status::UNAVAILABLE],
        },
        RefusalCase {
            // A refactor is executed ONCE: the second call finds it past the one state an
            // execution starts from.
            case: "a refactor that is already being executed",
            execute_first: true,
            region: REGION,
            definitions: vec![Definition::named("refactor-exec-refused")],
            enable_stack_creation: Some(false),
            names: &[],
        },
        RefusalCase {
            // A refactor belongs to the Region its own call was signed for, so another
            // Region holds no refactor under that id to execute.
            case: "a refactor another Region holds",
            execute_first: false,
            region: OTHER_REGION,
            definitions: vec![Definition::named("refactor-exec-refused")],
            enable_stack_creation: Some(false),
            names: &[],
        },
    ];

    for row in rows {
        let case = row.case;
        let (status_code, body) = create_stack_refactor(
            &router,
            Some(case),
            row.enable_stack_creation,
            &row.definitions,
            &[],
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected CreateStackRefactor to succeed, got {status_code} {body}"
        );
        let stack_refactor_id = xml_text(&body, "StackRefactorId")
            .unwrap_or_else(|| panic!("case {case:?}: expected a StackRefactorId, got {body}"));
        wait_for_the_transition().await;
        if row.execute_first {
            let (status_code, body) = execute_stack_refactor(&router, &stack_refactor_id).await;
            assert_eq!(
                status_code, 200,
                "case {case:?}: expected the first execution to be accepted, got {status_code} \
                 {body}"
            );
        }

        let (status_code, body) =
            execute_stack_refactor_in(&router, row.region, &stack_refactor_id).await;
        assert_eq!(
            status_code, 400,
            "case {case:?}: expected the execution to be refused, got {status_code} {body}"
        );
        assert_eq!(
            error_code(case, &body),
            "ValidationError",
            "case {case:?}: expected CloudFormation's own refusal code, got {body}"
        );
        assert!(
            body.contains(&stack_refactor_id),
            "case {case:?}: expected the refusal to name the refactor, got {body}"
        );
        for named in row.names {
            assert!(
                body.contains(named),
                "case {case:?}: expected the refusal to name {named:?}, got {body}"
            );
        }
    }
}
