// SPDX-License-Identifier: CC0-1.0

//! The MODEL VALIDATION a resource provider runs on the declaration it is handed, end to end
//! over the awsQuery wire with the services that own the declared types mounted beside
//! CloudFormation on one engine.
//!
//! RULE under test: **a resource provider validates the declaration it is handed against its
//! type's schema before it calls the service that owns the type, so a declaration that does
//! not carry a property that schema marks required fails THAT resource with the schema
//! validator's own sentence — `Model validation failed (#: required key [<member>] not found)`,
//! naming the member as the schema spells it — nothing is created in the owning service, and
//! the stack is rolled back.** The sentence is quoted in the envelope every handler refusal is
//! reported in: `Resource handler returned message: "<the provider's message>" (RequestToken:
//! <token>, HandlerErrorCode: InvalidRequest)`, whose token is this resource request's own.
//!
//! Why it matters: a client that reads the failed resource's reason to classify the failure —
//! a missing required property against any other refused request — needs the member name in
//! the machine-readable form the validator states it in. A sentence of the emulator's own
//! carries the same facts in a shape no such client can parse.
//!
//! The rows differ along what the rule generalises over: the resource TYPE whose provider
//! validates the model (five services), WHICH of that type's required properties the
//! declaration omits (the only one, the first of two, the second of two), and the FORM the
//! declaration takes (a property document missing one member, and no `Properties` member at
//! all). Each row's member is the one its type's entry in the "AWS resource and property types
//! reference" publishes with "Required: Yes"; the state machine's `RoleArn` is the one the
//! report that led here cited, and every other row is one it did not.
//!
//! The POSITIVE CONTROL deploys the same state machine WITH the role it omits, and reads the
//! machine back through AWS Step Functions' own wire: the stage refuses a declaration that is
//! missing something and nothing else, so a complete declaration still reaches the service
//! that owns it.

use serde_json::{json, Value};

use crate::stack_lifecycle::{
    create_stack, describe_stack, resource_event_reason, router, xml_text, ACCOUNT_ID, REGION,
};
use crate::stack_owned_secret_teardown::{error_code, poll_to_terminal};
use crate::step_functions_state_machine_replacement::sfn_json;
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

/// The workflow every declared state machine states, which is beside the point of every row:
/// what the rows are about is the role, not the document.
const DEFINITION: &str = r#"{"StartAt":"S","States":{"S":{"Type":"Pass","End":true}}}"#;

/// The role the positive control declares.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/StatesExecutionRole";

/// The error code AWS Step Functions refuses a call naming a state machine it does not hold
/// with (`StateMachineDoesNotExist`, that service's own modelled refusal).
const STATE_MACHINE_DOES_NOT_EXIST: &str = "StateMachineDoesNotExist";

/// The status `DescribeStateMachine` reports a usable state machine in (`StateMachineStatus`:
/// ACTIVE | DELETING).
const ACTIVE: &str = "ACTIVE";

/// One declaration whose model validation must fail, and the member it must be reported for.
struct Row {
    case: &'static str,
    stack: &'static str,
    logical_id: &'static str,
    resource_type: &'static str,
    /// The `Properties` the declaration states, or `Value::Null` for one that states no
    /// property document at all.
    properties: Value,
    /// The member the validator must name: the first property of the type the declaration
    /// does not carry.
    missing: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // The recorded shape: a workflow and a name, and no role for the executions to
            // run as.
            case: "a state machine declaring no role",
            stack: "model-validation-state-machine",
            logical_id: "SM",
            resource_type: "AWS::StepFunctions::StateMachine",
            properties: json!({
                "StateMachineName": "model-validation-machine",
                "DefinitionString": DEFINITION,
            }),
            missing: "RoleArn",
        },
        Row {
            // Two required properties, neither declared: the one reported is the first the
            // type's reference publishes.
            case: "a secret policy declaring neither the document nor the secret",
            stack: "model-validation-resource-policy-both",
            logical_id: "Access",
            resource_type: "AWS::SecretsManager::ResourcePolicy",
            properties: json!({}),
            missing: "ResourcePolicy",
        },
        Row {
            // The same type with the FIRST required property declared: the one reported is
            // then the second.
            case: "a secret policy declaring the document but no secret",
            stack: "model-validation-resource-policy-secret",
            logical_id: "Access",
            resource_type: "AWS::SecretsManager::ResourcePolicy",
            properties: json!({
                "ResourcePolicy": {"Version": "2012-10-17", "Statement": []},
            }),
            missing: "SecretId",
        },
        Row {
            case: "a rotation schedule naming no secret",
            stack: "model-validation-rotation-schedule",
            logical_id: "Rotation",
            resource_type: "AWS::SecretsManager::RotationSchedule",
            properties: json!({}),
            missing: "SecretId",
        },
        Row {
            case: "a metric filter declaring no log group",
            stack: "model-validation-metric-filter",
            logical_id: "Errors",
            resource_type: "AWS::Logs::MetricFilter",
            properties: json!({"FilterPattern": "ERROR"}),
            missing: "LogGroupName",
        },
        Row {
            case: "a function declaring no deployment package",
            stack: "model-validation-function",
            logical_id: "Handler",
            resource_type: "AWS::Lambda::Function",
            properties: json!({
                "Handler": "index.handler",
                "Runtime": "python3.12",
                "Role": "arn:aws:iam::000000000000:role/lambda-role",
            }),
            missing: "Code",
        },
        Row {
            // No `Properties` member at all: a declaration that carries no property document
            // carries none of the properties its type requires either.
            case: "an authorizer with no property document at all",
            stack: "model-validation-authorizer",
            logical_id: "Gate",
            resource_type: "AWS::IoT::Authorizer",
            properties: Value::Null,
            missing: "AuthorizerFunctionArn",
        },
    ]
}

/// The template one row deploys: the declaration as the row states it, and nothing else — the
/// resource under test is the only one, so nothing else can fail the stack.
fn template(row: &Row) -> String {
    let mut declaration = serde_json::Map::new();
    declaration.insert("Type".to_string(), json!(row.resource_type));
    if !row.properties.is_null() {
        declaration.insert("Properties".to_string(), row.properties.clone());
    }
    json!({"Resources": {row.logical_id: Value::Object(declaration)}}).to_string()
}

/// The head of the envelope CloudFormation quotes a provider's message in.
const QUOTED_PREFIX: &str = "Resource handler returned message: \"";

/// The tail of that envelope, from the end of the quoted message to the token.
const QUOTED_SUFFIX: &str = "\" (RequestToken: ";

/// The class a refused REQUEST is reported under (`HandlerErrorCode`, whose values the model
/// publishes).
const INVALID_REQUEST: &str = "InvalidRequest";

/// The sentence the schema validator states for a required key the document does not carry.
/// Source: the recorded `ResourceStatusReason` of a CloudFormation resource whose provider's
/// model validation failed that way.
fn model_validation_message(member: &str) -> String {
    format!("Model validation failed (#: required key [{member}] not found)")
}

/// The whole reason, with `token` as the request token: what a client reads off the failed
/// resource's event.
fn expected_reason(member: &str, token: &str) -> String {
    quoted(&model_validation_message(member), token)
}

/// The sentence the schema validator states for a property whose value is outside the SET its
/// type's schema publishes, at the location of that property in the validated document.
fn model_validation_enum_message(property: &str) -> String {
    format!(
        "Model validation failed (#/{property}: failed validation constraint for keyword [enum])"
    )
}

/// One provider message in the envelope CloudFormation quotes it in, with `token` as this
/// resource request's own.
fn quoted(message: &str, token: &str) -> String {
    format!("{QUOTED_PREFIX}{message}{QUOTED_SUFFIX}{token}, HandlerErrorCode: {INVALID_REQUEST})")
}

/// The request token a served reason carries, read back out of it — the value is this
/// request's own, so it is read rather than expected.
fn served_token(case: &str, reason: &str) -> String {
    let after = reason
        .split_once(QUOTED_SUFFIX)
        .unwrap_or_else(|| panic!("case {case:?}: expected the handler envelope, got {reason:?}"))
        .1;
    after
        .split_once(',')
        .unwrap_or_else(|| panic!("case {case:?}: expected a token in {reason:?}"))
        .0
        .to_string()
}

/// Whether `token` has the form of the identifier the service mints per resource request: the
/// 8-4-4-4-12 hexadecimal groups of a uuid.
fn is_a_uuid(token: &str) -> bool {
    let groups: Vec<&str> = token.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .map(|group| group.len())
            .eq([8, 4, 4, 4, 12])
        && groups
            .iter()
            .all(|group| group.chars().all(|c| c.is_ascii_hexdigit()))
}

/// The ARN a state machine of `name` would be addressed by, built from the Region, the account
/// and the name — "Ref returns the ARN of the state machine", which is a total function of the
/// three.
fn state_machine_arn(name: &str) -> String {
    format!("arn:aws:states:{REGION}:{ACCOUNT_ID}:stateMachine:{name}")
}

#[tokio::test]
async fn a_declaration_missing_a_required_property_fails_with_the_validators_sentence() {
    let router = router();
    for row in rows() {
        let case = row.case;
        let (code, created) = create_stack(&router, row.stack, &template(&row), &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must be accepted — the request is valid and the stack \
             exists — got {code} {created}"
        );

        let described = poll_to_terminal(&router, case, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::ROLLBACK_COMPLETE),
            "case {case:?}: expected the failed create to be rolled back, got {described}"
        );

        // The resource's own event carries the provider's sentence, quoted in the handler
        // envelope: the token is this request's own, so it is read back out of the reason and
        // held to its FORM, and everything around it is fixed text and the missing member.
        let reason =
            resource_event_reason(&router, row.stack, row.logical_id, resource_status::CREATE_FAILED)
                .await;
        let token = served_token(case, &reason);
        assert!(
            is_a_uuid(&token),
            "case {case:?}: expected the request token to be a uuid, got {token:?}"
        );
        let expected = expected_reason(row.missing, &token);
        assert_eq!(
            reason, expected,
            "case {case:?}: expected the reason {expected:?}, got {reason:?}"
        );
    }
}

/// RULE: the SAME provider validation judges a property whose value its type's schema
/// constrains to a published SET — a declared value outside that set fails THAT resource with
/// the validator's own sentence for the constraint that failed, at the LOCATION of that
/// property (`Model validation failed (#/<property>: failed validation constraint for keyword
/// [enum])`), in the same handler envelope and under the same `HandlerErrorCode`.
///
/// Why it matters for the same reason the rule above does: the value is one no resource of the
/// type could ever have, so the provider refuses it rather than the template being refused as
/// malformed, and a client classifying the failure reads the property's location off the
/// reason.
///
/// The rows are values of one such property, differing along HOW each misses the set: a value
/// of no published spelling at all, a published value in the wrong case, the empty string, and
/// two published values run together. Every value the set DOES publish passes this stage and is
/// deployed, which the router's own cross-service suite asserts
/// (`cloudformation_provisions_glue_table_optimizers.rs`).
#[tokio::test]
async fn a_value_outside_the_published_set_fails_with_the_validators_enum_sentence() {
    let router = router();
    // The type under test and the property whose values its schema publishes: an AWS Glue
    // table optimizer's `Type`, whose set is that service's own `TableOptimizerType`.
    let resource_type = "AWS::Glue::TableOptimizer";
    let property = "Type";
    let logical_id = "Optimizer";
    // (case, the value the declaration states for the property).
    let rows: [(&str, &str); 4] = [
        ("a value of no published spelling", "vacuum"),
        ("a published value in the wrong case", "COMPACTION"),
        ("the empty string", ""),
        ("two published values run together", "compaction,retention"),
    ];

    for (index, (case, declared)) in rows.into_iter().enumerate() {
        let stack = format!("model-validation-optimizer-{index}");
        let template = json!({"Resources": {logical_id: {
            "Type": resource_type,
            "Properties": {
                "CatalogId": ACCOUNT_ID,
                "DatabaseName": "lakehouse",
                "TableName": "orders",
                property: declared,
                "TableOptimizerConfiguration": {
                    "RoleArn": "arn:aws:iam::000000000000:role/glue-table-optimizer",
                    "Enabled": true,
                },
            },
        }}})
        .to_string();
        let (code, created) = create_stack(&router, &stack, &template, &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must be accepted — the refusal is the resource's — got \
             {code} {created}"
        );

        let described = poll_to_terminal(&router, case, &stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::ROLLBACK_COMPLETE),
            "case {case:?}: expected the failed create to be rolled back, got {described}"
        );

        let reason =
            resource_event_reason(&router, &stack, logical_id, resource_status::CREATE_FAILED)
                .await;
        let token = served_token(case, &reason);
        let expected = quoted(&model_validation_enum_message(property), &token);
        assert_eq!(
            reason, expected,
            "case {case:?}: expected the reason {expected:?}, got {reason:?}"
        );
    }
}

/// Nothing was created in the service that owns the type: the declaration was refused before
/// any call, so AWS Step Functions holds no machine under the name the template asked for.
#[tokio::test]
async fn a_failed_model_validation_creates_nothing_in_the_owning_service() {
    let router = router();
    let case = "a state machine declaring no role";
    let name = "model-validation-uncreated-machine";
    let template = json!({"Resources": {"SM": {
        "Type": "AWS::StepFunctions::StateMachine",
        "Properties": {"StateMachineName": name, "DefinitionString": DEFINITION},
    }}})
    .to_string();
    let (code, created) =
        create_stack(&router, "model-validation-uncreated", &template, &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must be accepted, got {code} {created}"
    );
    let described = poll_to_terminal(&router, case, "model-validation-uncreated").await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::ROLLBACK_COMPLETE),
        "case {case:?}: expected the failed create to be rolled back, got {described}"
    );

    let arn = state_machine_arn(name);
    let (code, refused) =
        sfn_json(&router, "DescribeStateMachine", &json!({"stateMachineArn": arn})).await;
    assert_eq!(
        (code, error_code(case, &refused).as_str()),
        (400, STATE_MACHINE_DOES_NOT_EXIST),
        "case {case:?}: expected Step Functions to hold no machine under {arn:?}, got {code} \
         {refused}"
    );
    let expected = format!("State Machine Does Not Exist: '{arn}'");
    assert_eq!(
        refused.get("message").and_then(Value::as_str),
        Some(expected.as_str()),
        "case {case:?}: expected the refusal to state {expected:?}, got {refused}"
    );
}

/// The positive control: the same declaration WITH the property it omitted is deployed, and
/// the machine is read back through the owning service's own wire.
#[tokio::test]
async fn the_same_declaration_carrying_the_required_property_is_deployed() {
    let router = router();
    let case = "a state machine declaring the role it runs as";
    let name = "model-validation-control-machine";
    let template = json!({"Resources": {"SM": {
        "Type": "AWS::StepFunctions::StateMachine",
        "Properties": {
            "StateMachineName": name,
            "DefinitionString": DEFINITION,
            "RoleArn": ROLE_ARN,
        },
    }}})
    .to_string();
    let (code, created) = create_stack(&router, "model-validation-control", &template, &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must be accepted, got {code} {created}"
    );
    let described = poll_to_terminal(&router, case, "model-validation-control").await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::CREATE_COMPLETE),
        "case {case:?}: expected the complete declaration to be deployed, got {described}"
    );
    let (code, body) = describe_stack(&router, "model-validation-control").await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStacks must answer 200, got {code} {body}"
    );

    let arn = state_machine_arn(name);
    let (code, answered) =
        sfn_json(&router, "DescribeStateMachine", &json!({"stateMachineArn": arn})).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected Step Functions to hold the machine the stack created, got \
         {code} {answered}"
    );
    assert_eq!(
        answered.get("status").and_then(Value::as_str),
        Some(ACTIVE),
        "case {case:?}: expected the created machine to be {ACTIVE}, got {answered}"
    );
    assert_eq!(
        answered.get("roleArn").and_then(Value::as_str),
        Some(ROLE_ARN),
        "case {case:?}: expected the declared role, got {answered}"
    );
    assert_eq!(
        answered.get("definition").and_then(Value::as_str),
        Some(DEFINITION),
        "case {case:?}: expected the declared workflow, got {answered}"
    );
}
