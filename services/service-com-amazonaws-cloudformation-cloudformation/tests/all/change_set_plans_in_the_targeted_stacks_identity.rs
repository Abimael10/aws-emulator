// SPDX-License-Identifier: CC0-1.0

//! The identity a change set's plan is computed in, end to end over the awsQuery wire.
//!
//! RULE: a change set is the PLAN of the operation that would be executed, so the template it
//! plans is evaluated in the scope that execution would evaluate it in — the targeted stack's
//! own `StackId` included. CloudFormation derives the physical name it GENERATES for an
//! unnamed resource from that id, so a declaration whose name is built from a reference to
//! another resource of the stack resolves, while the plan is computed, to the identifier that
//! resource really carries. The pre-deployment existence check therefore asks the owning
//! service about exactly the name the execution would create: a plan that would create a
//! resource under a name held OUTSIDE CloudFormation settles `FAILED` with `ExecutionStatus`
//! `UNAVAILABLE` and the validation's own `StatusReason`, and the validation result
//! `DescribeEvents` reports names that RESOLVED identifier; a plan whose resolved name nothing
//! holds settles `CREATE_COMPLETE` and is offered for execution.
//!
//! The rows differ along what the rule generalises over: the intrinsic the derived name is
//! built with (`Fn::Join` over a `Ref`, `Fn::Sub` over a logical id), the resource type the
//! derived name belongs to and the service that holds it (Amazon CloudWatch Logs, AWS Systems
//! Manager), and whether the resolved name is held at all — the control row, where the plan is
//! offered for execution and nothing is created.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::{create_change_set, settle_the_store, settled_change_set};
use crate::dynamic_references::put_parameter;
use crate::harness::physical_resource_id;
use crate::logs_group_children_resources::{group_exists, logs};
use crate::stack_lifecycle::{
    call, create_stack, member, router_with_engine, settled_stack, xml_blocks, xml_text,
};
use service_com_amazonaws_cloudformation_cloudformation::state::{
    change_set_status, execution_status, status,
};

/// The logical id of the resource whose name CloudFormation GENERATES — the one the derived
/// declaration below names itself from.
const BASE: &str = "Base";

/// ...and the logical id of the declaration whose own name is derived from it.
const DERIVED: &str = "Derived";

/// The resource types the derived declarations are of.
const LOG_GROUP: &str = "AWS::Logs::LogGroup";
const PARAMETER: &str = "AWS::SSM::Parameter";

/// The validation CloudFormation names for the check that a resource a plan would create does
/// not already exist, and the reason a change set that failed it carries. Source:
/// `state::early_validation::{NAME_CONFLICT_VALIDATION, RESOURCE_EXISTENCE_CHECK}`.
const NAME_CONFLICT_VALIDATION: &str = "NAME_CONFLICT_VALIDATION";
const RESOURCE_EXISTENCE_CHECK: &str = "AWS::EarlyValidation::ResourceExistenceCheck";

/// The `EventType` a validation result is reported under, from the model's `EventType` enum
/// (`smithy_query(query="shape", shape="EventType")`).
const VALIDATION_ERROR: &str = "VALIDATION_ERROR";

/// `OperationEvent$ValidationStatus` for a validation that failed, from the model's
/// `ValidationStatus` enum (`[FAILED, SKIPPED]`).
const VALIDATION_STATUS_FAILED: &str = "FAILED";

/// The `StatusReason` a change set that did not pass the pre-deployment validations carries:
/// which validations failed, and where their detail is read. Source:
/// `state::early_validation::failed_reason`.
fn failed_reason(validations: &[&str]) -> String {
    format!(
        "The following hook(s)/validation failed: [{}]. To troubleshoot Early Validation \
         errors, use the DescribeEvents API for detailed failure information.",
        validations.join(", ")
    )
}

/// CloudFormation's own sentence for a resource it cannot create because one already exists
/// under the identifier the declaration asked for. Source:
/// `state::early_validation::already_exists`.
fn already_exists(resource_type: &str, identifier: &str) -> String {
    format!("Resource of type '{resource_type}' with identifier '{identifier}' already exists.")
}

/// Who already holds the name a row's derived declaration resolves to.
#[derive(Debug, Clone, Copy)]
enum Held {
    /// A log group CloudWatch Logs holds, created through its own wire.
    Logs,
    /// A parameter Parameter Store holds.
    ParameterStore,
    /// Nobody: the control row, whose plan is offered for execution.
    Nobody,
}

/// One row: the declaration whose name is derived from the stack's own resource, and who holds
/// the name that derivation resolves to.
struct Row {
    case: &'static str,
    stack: &'static str,
    change_set: &'static str,
    resource_type: &'static str,
    /// The text the derived name begins with, which the base resource's identifier follows.
    prefix: &'static str,
    /// The derived declaration, naming itself from [`BASE`].
    derived: Value,
    held: Held,
}

/// The base declaration every stack below is created with: a log group whose name the template
/// states NOWHERE, so CloudFormation generates it from the stack's identity and the logical id.
fn base_declaration() -> Value {
    json!({"Type": LOG_GROUP, "Properties": {}})
}

/// A template declaring the base resource alone — what each stack is created with.
fn base_template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {BASE: base_declaration()},
    })
    .to_string()
}

/// ...and the revision a change set plans: the same base resource, plus one declaration whose
/// own name is built from it.
fn plan_template(derived: &Value) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {BASE: base_declaration(), DERIVED: derived},
    })
    .to_string()
}

/// A log group whose name is `prefix` followed by the base resource's identifier, built with
/// `Fn::Join` over a `Ref`.
fn joined_group(prefix: &str) -> Value {
    json!({
        "Type": LOG_GROUP,
        "Properties": {"LogGroupName": {"Fn::Join": ["", [prefix, {"Ref": BASE}]]}},
    })
}

/// The same name built with `Fn::Sub` over the base resource's logical id.
fn subbed_group(prefix: &str) -> Value {
    json!({
        "Type": LOG_GROUP,
        "Properties": {"LogGroupName": {"Fn::Sub": format!("{prefix}${{{BASE}}}")}},
    })
}

/// ...and a parameter of another service named the same way, so the rule is asserted over more
/// than one owning service.
fn subbed_parameter(prefix: &str) -> Value {
    json!({
        "Type": PARAMETER,
        "Properties": {
            "Name": {"Fn::Sub": format!("{prefix}${{{BASE}}}")},
            "Type": "String",
            "Value": "derived",
        },
    })
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a log group named with Fn::Join over a Ref to the stack's own group",
            stack: "csid-joined",
            change_set: "csid-joined-set",
            resource_type: LOG_GROUP,
            prefix: "/derived/joined/",
            derived: joined_group("/derived/joined/"),
            held: Held::Logs,
        },
        Row {
            case: "a log group named with Fn::Sub over the stack's own group",
            stack: "csid-subbed",
            change_set: "csid-subbed-set",
            resource_type: LOG_GROUP,
            prefix: "/derived/subbed/",
            derived: subbed_group("/derived/subbed/"),
            held: Held::Logs,
        },
        Row {
            case: "a parameter named with Fn::Sub over the stack's own group",
            stack: "csid-parameter",
            change_set: "csid-parameter-set",
            resource_type: PARAMETER,
            prefix: "/derived/parameter/",
            derived: subbed_parameter("/derived/parameter/"),
            held: Held::ParameterStore,
        },
        Row {
            case: "a derived log-group name nothing holds",
            stack: "csid-free",
            change_set: "csid-free-set",
            resource_type: LOG_GROUP,
            prefix: "/derived/free/",
            derived: joined_group("/derived/free/"),
            held: Held::Nobody,
        },
    ]
}

/// Create the stack the row plans against and answer the identifier its base resource really
/// carries, asserting that identifier is the one CloudFormation GENERATED for it: that is what
/// makes the derived name depend on the stack's own identity.
async fn deployed_base(
    router: &MultiServiceRouter,
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    case: &str,
    stack: &str,
) -> String {
    let (code, body) = create_stack(router, stack, &base_template(), &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must answer 200, got {code} {body}"
    );
    settle_the_store(router, engine).await;
    let settled = settled_stack(router, case, stack).await;
    let reported = xml_text(&settled, "StackStatus").unwrap_or_default();
    assert_eq!(
        reported,
        status::CREATE_COMPLETE,
        "case {case:?}: expected the base stack to reach {}, got {reported:?}",
        status::CREATE_COMPLETE
    );
    let base = physical_resource_id(router, case, stack, BASE).await;
    let generated = format!("{stack}-{BASE}-");
    assert!(
        base.starts_with(&generated),
        "case {case:?}: expected a generated name starting {generated:?}, got {base:?}"
    );
    base
}

/// Hold the derived name outside CloudFormation, in the service that owns the row's type.
async fn hold_the_name(router: &MultiServiceRouter, case: &str, row: &Row, name: &str) {
    match row.held {
        Held::Logs => {
            let (code, body) =
                logs(router, "CreateLogGroup", &json!({"logGroupName": name})).await;
            assert_eq!(
                code, 200,
                "case {case:?}: the setup CreateLogGroup({name}) must answer 200, got \
                 {code} {body}"
            );
        }
        Held::ParameterStore => put_parameter(router, case, name, "String", "held").await,
        Held::Nobody => {}
    }
}

/// The validation RESULTS `DescribeEvents` reports for `stack`: one tuple per event carrying a
/// `ValidationName`.
async fn validation_results(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<(String, String, String, String, String, String)> {
    let (code, body) = call(router, "DescribeEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the operation's events must be readable, got {code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .filter(|event| xml_text(event, "ValidationName").is_some())
        .map(|event| {
            (
                xml_text(event, "LogicalResourceId").unwrap_or_default(),
                xml_text(event, "ResourceType").unwrap_or_default(),
                xml_text(event, "EventType").unwrap_or_default(),
                xml_text(event, "ValidationName").unwrap_or_default(),
                xml_text(event, "ValidationStatus").unwrap_or_default(),
                xml_text(event, "ValidationStatusReason").unwrap_or_default(),
            )
        })
        .collect()
}

/// Every row: the change set's plan resolves the derived name against the identifier the
/// targeted stack's resource really carries, so a name held outside CloudFormation fails the
/// plan and names that resolved identifier, while a free one leaves the plan executable.
#[tokio::test]
async fn a_plan_resolves_a_derived_name_in_the_targeted_stacks_identity() {
    for row in rows() {
        let case = row.case;
        let (router, engine) = router_with_engine();
        let base = deployed_base(&router, &engine, case, row.stack).await;
        let derived_name = format!("{}{base}", row.prefix);
        hold_the_name(&router, case, &row, &derived_name).await;

        let (code, body) = create_change_set(
            &router,
            row.stack,
            row.change_set,
            &[member("TemplateBody", &plan_template(&row.derived))],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateChangeSet must answer 200, got {code} {body}"
        );
        let described =
            settled_change_set(&router, &engine, row.change_set, Some(row.stack)).await;
        let collides = !matches!(row.held, Held::Nobody);
        let (expected_status, expected_execution, expected_reason) = if collides {
            (
                change_set_status::FAILED,
                execution_status::UNAVAILABLE,
                Some(failed_reason(&[RESOURCE_EXISTENCE_CHECK])),
            )
        } else {
            (
                change_set_status::CREATE_COMPLETE,
                execution_status::AVAILABLE,
                None,
            )
        };
        let reported = xml_text(&described, "Status").unwrap_or_default();
        assert_eq!(
            reported, expected_status,
            "case {case:?}: expected the change set to settle {expected_status}, got \
             {reported:?} in {described}"
        );
        let execution = xml_text(&described, "ExecutionStatus").unwrap_or_default();
        assert_eq!(
            execution, expected_execution,
            "case {case:?}: expected {expected_execution}, got {execution:?}"
        );
        let reason = xml_text(&described, "StatusReason");
        assert_eq!(
            reason, expected_reason,
            "case {case:?}: expected {expected_reason:?}, got {reason:?}"
        );

        // The detail of the failure names the RESOLVED identifier — the derived name built
        // from what the stack's own resource really carries, which is the whole point of the
        // scope the plan was computed in.
        let results = validation_results(&router, case, row.stack).await;
        let expected_results = if collides {
            vec![(
                DERIVED.to_string(),
                row.resource_type.to_string(),
                VALIDATION_ERROR.to_string(),
                NAME_CONFLICT_VALIDATION.to_string(),
                VALIDATION_STATUS_FAILED.to_string(),
                already_exists(row.resource_type, &derived_name),
            )]
        } else {
            Vec::new()
        };
        assert_eq!(
            results, expected_results,
            "case {case:?}: expected the validation results {expected_results:?}, got \
             {results:?}"
        );

        // A change set is a plan either way: nothing of the derived declaration was created,
        // so a log group exists under the derived name only where the row's holder put one.
        let exists = group_exists(&router, &derived_name).await;
        let expected_exists = matches!(row.held, Held::Logs);
        assert_eq!(
            exists, expected_exists,
            "case {case:?}: expected a log group under {derived_name:?}: {expected_exists}, \
             got {exists}"
        );
    }
}

/// The scope is the TARGETED stack's: one template planned against two stacks resolves its
/// derived name twice, each time to the identifier that stack's own resource carries — so the
/// name one stack's plan collides on leaves the other's plan executable.
#[tokio::test]
async fn two_stacks_planning_one_template_resolve_two_derived_names() {
    let case = "one template planned against two stacks";
    let (router, engine) = router_with_engine();
    let prefix = "/derived/two-stacks/";
    let derived = joined_group(prefix);
    let template = plan_template(&derived);

    let colliding = deployed_base(&router, &engine, case, "csid-two-one").await;
    let free = deployed_base(&router, &engine, case, "csid-two-two").await;
    assert_ne!(
        colliding, free,
        "case {case:?}: two stacks must generate two names, got {colliding:?} twice"
    );
    // Only the FIRST stack's derived name is held.
    let held = format!("{prefix}{colliding}");
    let (code, body) = logs(&router, "CreateLogGroup", &json!({"logGroupName": held})).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the setup CreateLogGroup({held}) must answer 200, got {code} {body}"
    );

    let expected: &[(&str, &str, &str, &str)] = &[
        (
            "csid-two-one",
            "csid-two-one-set",
            change_set_status::FAILED,
            execution_status::UNAVAILABLE,
        ),
        (
            "csid-two-two",
            "csid-two-two-set",
            change_set_status::CREATE_COMPLETE,
            execution_status::AVAILABLE,
        ),
    ];
    for (stack, change_set, settles_to, execution_status) in expected {
        let (code, body) = create_change_set(
            &router,
            stack,
            change_set,
            &[member("TemplateBody", &template)],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?} ({stack}): CreateChangeSet must answer 200, got {code} {body}"
        );
        let described = settled_change_set(&router, &engine, change_set, Some(stack)).await;
        let reported = xml_text(&described, "Status").unwrap_or_default();
        assert_eq!(
            reported, *settles_to,
            "case {case:?} ({stack}): expected {settles_to}, got {reported:?} in {described}"
        );
        let execution = xml_text(&described, "ExecutionStatus").unwrap_or_default();
        assert_eq!(
            execution, *execution_status,
            "case {case:?} ({stack}): expected {execution_status}, got {execution:?}"
        );
    }
}
