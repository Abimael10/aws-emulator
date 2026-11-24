// SPDX-License-Identifier: CC0-1.0

//! Which `CreateChangeSet` requests the `ResourceTypes` member bounds, end to end over the
//! awsQuery wire.
//!
//! RULE: the list a request states bounds the UPDATE the change set plans — "if the list of
//! resource types doesn't include a resource that you're updating, the stack update fails"
//! (`CreateChangeSetInput$ResourceTypes`). So a change set over a stack that already stands is
//! refused, with `ValidationError` and `Resource type <type> is not allowed by parameter
//! ResourceTypes [<the list as stated>]`, when the template that plan would DEPLOY declares a
//! type no entry of the list covers — the type named being the first such type the template
//! declares — and nothing is written. A change set of a type that brings its stack INTO
//! EXISTENCE plans no update of a stack: it opens a stack that holds no revision and stays in
//! review until an execution carries the plan out, so such a request is admitted whatever its
//! template declares, with the list recorded on the change set.
//!
//! The rows below span the dimension the rule turns on — what the change set does to its stack
//! (updates one that stands, brings one into existence, plans again on one already in review) —
//! and, for the bound itself, the entry forms a list may take, a request that states no list at
//! all, and the two ways the template a plan would deploy is named (a submitted body, and the
//! stack's own template reused). `change_set_bounds_and_property_values.rs` holds the same bound
//! over `CreateStack`/`UpdateStack`-shaped plans; this suite is about which change sets it is
//! weighed on.

use service_com_amazonaws_cloudformation_cloudformation::state::{
    change_set_status, execution_status, status,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::{create_change_set, describe_change_set, settle_the_store};
use crate::harness::{create_stack, member, refusal, router_with_engine, xml_text};
use crate::stack_lifecycle::describe_stack;

/// A type a service of this emulator holds: an `AWS::SSM::Parameter` is created in AWS Systems
/// Manager.
const PARAMETER_TYPE: &str = "AWS::SSM::Parameter";

/// A type CloudFormation serves itself, so a template may declare it beside the one above
/// without reaching another service: a `WaitConditionHandle` is "a resource that has no
/// properties".
const HANDLE_TYPE: &str = "AWS::CloudFormation::WaitConditionHandle";

/// A declaration of one `AWS::SSM::Parameter` carrying `value`.
fn parameter(value: &str) -> String {
    format!(r#"{{"Type":"{PARAMETER_TYPE}","Properties":{{"Type":"String","Value":"{value}"}}}}"#)
}

/// A declaration of one `AWS::CloudFormation::WaitConditionHandle`.
fn handle() -> String {
    format!(r#"{{"Type":"{HANDLE_TYPE}"}}"#)
}

/// A template declaring each `(logical id, declaration)` pair, in that order.
fn template(resources: &[(&str, String)]) -> String {
    let declared: Vec<String> = resources
        .iter()
        .map(|(logical_id, declaration)| format!(r#""{logical_id}":{declaration}"#))
        .collect();
    format!(r#"{{"Resources":{{{}}}}}"#, declared.join(","))
}

/// The `ResourceTypes.member.N` request members one stated list is sent as.
fn resource_types(allowed: &[&str]) -> Vec<(String, String)> {
    allowed
        .iter()
        .enumerate()
        .map(|(position, entry)| member(&format!("ResourceTypes.member.{}", position + 1), entry))
        .collect()
}

/// The refusal a stated list that does not cover `resource_type` earns, as the service words
/// it.
fn not_allowed(resource_type: &str, allowed: &[&str]) -> (String, String) {
    (
        "ValidationError".to_string(),
        format!(
            "Resource type {resource_type} is not allowed by parameter ResourceTypes [{}]",
            allowed.join(", ")
        ),
    )
}

/// What the change set a row plans does to its stack, which is what decides whether the stated
/// list is weighed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plans {
    /// An `UPDATE` of a stack that already stands, from a submitted template.
    AnUpdate,
    /// An `UPDATE` of a stack that already stands, reusing the stack's own template — the
    /// template that plan would deploy is the stack's.
    AnUpdateReusingTheTemplate,
    /// A `CREATE` change set for a stack that does not exist yet.
    ANewStack,
    /// A second `CREATE` change set for a stack an earlier one left in `REVIEW_IN_PROGRESS`.
    AStackAlreadyInReview,
}

/// One row: what the change set plans, the template it submits, the list it states, and the
/// type the refusal must name — `None` for a request the service admits.
struct Row {
    case: &'static str,
    plans: Plans,
    submitted: String,
    allowed: Vec<&'static str>,
    refused: Option<&'static str>,
}

/// The template the stacks the UPDATE rows plan against are created with: one resource of each
/// type, so a list covering one of them leaves the other uncovered.
fn deployed() -> String {
    template(&[("Held", parameter("v1")), ("Handle", handle())])
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an update whose template declares a type the list does not cover",
            plans: Plans::AnUpdate,
            submitted: template(&[
                ("Held", parameter("v2")),
                ("Handle", handle()),
            ]),
            allowed: vec![HANDLE_TYPE],
            refused: Some(PARAMETER_TYPE),
        },
        Row {
            case: "an update whose list covers every declared type outright",
            plans: Plans::AnUpdate,
            submitted: template(&[("Held", parameter("v2")), ("Handle", handle())]),
            allowed: vec![PARAMETER_TYPE, HANDLE_TYPE],
            refused: None,
        },
        Row {
            case: "an update under a service wildcard that covers one type and not the other",
            plans: Plans::AnUpdate,
            submitted: template(&[("Held", parameter("v2")), ("Handle", handle())]),
            allowed: vec!["AWS::SSM::*"],
            refused: Some(HANDLE_TYPE),
        },
        Row {
            case: "an update under the wildcard for every published type",
            plans: Plans::AnUpdate,
            submitted: template(&[("Held", parameter("v2")), ("Handle", handle())]),
            allowed: vec!["AWS::*"],
            refused: None,
        },
        Row {
            case: "an update that states no list at all, which bounds nothing",
            plans: Plans::AnUpdate,
            submitted: template(&[("Held", parameter("v2")), ("Handle", handle())]),
            allowed: Vec::new(),
            refused: None,
        },
        Row {
            // The type named is the FIRST declared type the list does not cover, which is
            // what the author has to act on. The logical ids are in alphabetical order, so
            // the row says the same whichever order a reader holds the declarations in.
            case: "an update whose template declares two uncovered types",
            plans: Plans::AnUpdate,
            submitted: template(&[
                ("Ahandle", handle()),
                ("Bheld", parameter("v2")),
                ("Cheld", parameter("v3")),
            ]),
            allowed: vec!["AWS::SQS::*"],
            refused: Some(HANDLE_TYPE),
        },
        Row {
            // No body is submitted: the template the plan would deploy is the stack's own,
            // and it is what the list is weighed against.
            case: "an update reusing the stack's template, which declares an uncovered type",
            plans: Plans::AnUpdateReusingTheTemplate,
            submitted: String::new(),
            allowed: vec![HANDLE_TYPE],
            refused: Some(PARAMETER_TYPE),
        },
        Row {
            case: "a change set that brings its stack into existence, over an uncovered type",
            plans: Plans::ANewStack,
            submitted: template(&[("Handle", handle()), ("Held", parameter("v1"))]),
            allowed: vec![HANDLE_TYPE],
            refused: None,
        },
        Row {
            case: "a change set that brings its stack into existence, over covered types",
            plans: Plans::ANewStack,
            submitted: template(&[("Handle", handle()), ("Held", parameter("v1"))]),
            allowed: vec![PARAMETER_TYPE, HANDLE_TYPE],
            refused: None,
        },
        Row {
            case: "a second plan on a stack still in review, over an uncovered type",
            plans: Plans::AStackAlreadyInReview,
            submitted: template(&[("Handle", handle()), ("Held", parameter("v1"))]),
            allowed: vec![HANDLE_TYPE],
            refused: None,
        },
    ]
}

/// `CreateChangeSet` for the row, with the members its plan is spelled by.
async fn plan(
    router: &MultiServiceRouter,
    row: &Row,
    stack: &str,
    change_set: &str,
) -> (u16, String) {
    let mut extra: Vec<(String, String)> = match row.plans {
        Plans::AnUpdate => vec![
            member("ChangeSetType", "UPDATE"),
            member("TemplateBody", &row.submitted),
        ],
        Plans::AnUpdateReusingTheTemplate => vec![
            member("ChangeSetType", "UPDATE"),
            member("UsePreviousTemplate", "true"),
        ],
        Plans::ANewStack | Plans::AStackAlreadyInReview => vec![
            member("ChangeSetType", "CREATE"),
            member("TemplateBody", &row.submitted),
        ],
    };
    extra.extend(resource_types(&row.allowed));
    create_change_set(router, stack, change_set, &extra).await
}

#[tokio::test]
async fn the_stated_list_bounds_the_update_a_change_set_plans() {
    let (router, engine) = router_with_engine();

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let stack = format!("csrt-{index}");
        // The stack the row plans against, in the state its own case needs.
        match row.plans {
            Plans::AnUpdate | Plans::AnUpdateReusingTheTemplate => {
                let (code, body) = create_stack(&router, &stack, &deployed(), &[]).await;
                assert_eq!(
                    code, 200,
                    "case {case:?}: the stack the plan changes must exist, got {code} {body}"
                );
                settle_the_store(&router, &engine).await;
            }
            Plans::AStackAlreadyInReview => {
                let (code, body) = create_change_set(
                    &router,
                    &stack,
                    "under-review",
                    &[
                        member("ChangeSetType", "CREATE"),
                        member("TemplateBody", &row.submitted),
                    ],
                )
                .await;
                assert_eq!(
                    code, 200,
                    "case {case:?}: the stack must be put in review first, got {code} {body}"
                );
                settle_the_store(&router, &engine).await;
                let (code, body) = describe_stack(&router, &stack).await;
                assert_eq!(
                    (code, xml_text(&body, "StackStatus").unwrap_or_default()),
                    (200, status::REVIEW_IN_PROGRESS.to_string()),
                    "case {case:?}: the stack must be in review, got {code} {body}"
                );
            }
            Plans::ANewStack => {}
        }

        let (code, body) = plan(&router, row, &stack, "plan").await;
        match row.refused {
            Some(resource_type) => {
                assert_eq!(
                    code, 400,
                    "case {case:?}: the call must be refused, got {code} {body}"
                );
                let answered = refusal(&body);
                let expected = not_allowed(resource_type, &row.allowed);
                assert_eq!(
                    answered, expected,
                    "case {case:?}: expected {expected:?}, got {answered:?}"
                );
                // Nothing was written: no plan under that name.
                let (code, body) = describe_change_set(&router, "plan", Some(&stack)).await;
                assert_eq!(
                    (code, refusal(&body).0),
                    (404, "ChangeSetNotFound".to_string()),
                    "case {case:?}: a refused call leaves no plan, got {code} {body}"
                );
            }
            None => {
                assert_eq!(
                    code, 200,
                    "case {case:?}: the request must be admitted, got {code} {body}"
                );
                settle_the_store(&router, &engine).await;
                let (code, described) = describe_change_set(&router, "plan", Some(&stack)).await;
                assert_eq!(
                    (
                        code,
                        xml_text(&described, "Status").unwrap_or_default(),
                        xml_text(&described, "ExecutionStatus").unwrap_or_default(),
                    ),
                    (
                        200,
                        change_set_status::CREATE_COMPLETE.to_string(),
                        execution_status::AVAILABLE.to_string(),
                    ),
                    "case {case:?}: the admitted plan must be created and ready, got {described}"
                );
            }
        }

        // A refused call brought no stack into existence either, and an admitted one left the
        // stack where its own case put it.
        let expected_stack_status = match row.plans {
            Plans::AnUpdate | Plans::AnUpdateReusingTheTemplate => {
                Some(status::CREATE_COMPLETE.to_string())
            }
            Plans::AStackAlreadyInReview => Some(status::REVIEW_IN_PROGRESS.to_string()),
            // A `CREATE` change set the service admitted opened the stack in review; one it
            // refused opened nothing at all.
            Plans::ANewStack => row
                .refused
                .is_none()
                .then(|| status::REVIEW_IN_PROGRESS.to_string()),
        };
        let (code, body) = describe_stack(&router, &stack).await;
        let reported = (code == 200).then(|| xml_text(&body, "StackStatus").unwrap_or_default());
        assert_eq!(
            reported, expected_stack_status,
            "case {case:?}: expected the stack in {expected_stack_status:?}, got {code} {body}"
        );
    }
}
