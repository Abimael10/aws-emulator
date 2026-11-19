// SPDX-License-Identifier: CC0-1.0

//! The `AWS::StepFunctions::StateMachineVersion` a stack publishes and the
//! `AWS::StepFunctions::StateMachineAlias` it creates, end to end over the awsQuery wire with
//! AWS Step Functions mounted beside CloudFormation on one engine.
//!
//! RULE under test: **a stack resource whose type belongs to another service is created BY
//! that service: a declared state-machine version is published in Step Functions and a
//! declared alias is created there, the stack publishes the ARNs that service minted for them,
//! and that service's own read reports both — so a call it REFUSES fails the resource and
//! rolls the stack back, carrying the refusal that service worded plus the diagnostic its SDK
//! appends.**
//!
//! Why it matters: a client that publishes a version through CloudFormation enumerates it with
//! `ListStateMachineVersions` and routes executions to it through the alias. A version the
//! stack reports created but that service never published is an ARN no listing knows and no
//! execution can reach, and a routing configuration that service would refuse — weights that
//! do not add up to the total it requires — would leave the client believing traffic is split
//! as declared while the real routing is undefined.
//!
//! The rows differ along what the rule generalises over: whether the template NAMES the alias
//! or leaves the name to CloudFormation, how the weight is written (a number, and the text a
//! template may have quoted it as), whether a description is declared, WHICH of the two types'
//! creates the refusal comes from, and which value makes it a refusal.

use serde_json::{json, Value};

use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};
use smithy_server_runtime::MultiServiceRouter;

use crate::stack_lifecycle::{
    call, create_stack, member, resource_event_reason, router, settled_stack, xml_text,
};
use crate::step_functions_state_machine_replacement::sfn_json;

/// The role a stack's state machine runs its executions as.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/StepFunctionsRole";

/// A workflow Step Functions accepts, which every row's state machine declares.
const DEFINITION: &str = r#"{"StartAt":"Done","States":{"Done":{"Type":"Pass","End":true}}}"#;

/// The logical ids every template below uses.
const MACHINE: &str = "Workflow";
const VERSION: &str = "Version";
const ALIAS: &str = "Live";

/// A template declaring a state machine, a version of it and an alias routing to that version.
///
/// `alias_properties` is the alias's whole property document, which is what the rows vary.
fn version_and_alias_template(alias_properties: Value) -> String {
    json!({
        "Resources": {
            MACHINE: {
                "Type": "AWS::StepFunctions::StateMachine",
                "Properties": { "DefinitionString": DEFINITION, "RoleArn": ROLE_ARN },
            },
            VERSION: {
                "Type": "AWS::StepFunctions::StateMachineVersion",
                "Properties": {
                    "StateMachineArn": { "Ref": MACHINE },
                    "Description": "the first cut",
                },
            },
            ALIAS: {
                "Type": "AWS::StepFunctions::StateMachineAlias",
                "Properties": alias_properties,
            },
        },
    })
    .to_string()
}

/// The physical id the stack publishes for `logical_id`, which for all three of these types is
/// the ARN the owning service minted.
async fn physical_id(case: &str, router: &MultiServiceRouter, stack: &str, logical_id: &str) -> String {
    let (code, body) = call(
        router,
        "DescribeStackResource",
        &[
            member("StackName", stack),
            member("LogicalResourceId", logical_id),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected DescribeStackResource({logical_id}) to answer, got {code} \
         {body}"
    );
    xml_text(&body, "PhysicalResourceId").unwrap_or_default()
}

/// The `Fn::GetAtt <logical_id>.Arn` the stack resolves, read off the stack's own outputs.
async fn output_arn(case: &str, router: &MultiServiceRouter, stack: &str, key: &str) -> String {
    let body = settled_stack(router, case, stack).await;
    crate::stack_lifecycle::output_value(&body, key).unwrap_or_default()
}

/// Every version ARN `ListStateMachineVersions` reports for `machine_arn`, and the empty list
/// where Step Functions holds no such machine at all.
async fn listed_versions(router: &MultiServiceRouter, machine_arn: &str) -> Vec<String> {
    let (status, document) = sfn_json(
        router,
        "ListStateMachineVersions",
        &json!({ "stateMachineArn": machine_arn }),
    )
    .await;
    if status != 200 {
        return Vec::new();
    }
    document
        .get("stateMachineVersions")
        .and_then(Value::as_array)
        .map(|versions| {
            versions
                .iter()
                .filter_map(|version| {
                    version
                        .get("stateMachineVersionArn")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// What `DescribeStateMachineAlias` answers for `arn`, or `None` where that service holds no
/// such alias.
async fn described_alias(router: &MultiServiceRouter, arn: &str) -> Option<Value> {
    let (status, document) = sfn_json(
        router,
        "DescribeStateMachineAlias",
        &json!({ "stateMachineAliasArn": arn }),
    )
    .await;
    (status == 200).then_some(document)
}

/// One row of the published-version table.
struct Published {
    case: &'static str,
    stack: &'static str,
    /// The alias's declared property document.
    alias: Value,
    /// The name the alias must be created under, or `None` for one CloudFormation names.
    alias_name: Option<&'static str>,
    /// The weight the routing configuration must carry, whichever way the template wrote it.
    weight: i64,
}

fn published_rows() -> Vec<Published> {
    vec![
        Published {
            case: "an alias the template names, with the weight written as a number",
            stack: "sfn-version-named-alias",
            alias: json!({
                "Name": "live",
                "Description": "the live alias",
                "RoutingConfiguration": [
                    { "StateMachineVersionArn": { "Ref": VERSION }, "Weight": 100 },
                ],
            }),
            alias_name: Some("live"),
            weight: 100,
        },
        Published {
            case: "an alias CloudFormation names, with the weight quoted as a template may \
                   write it",
            stack: "sfn-version-generated-alias",
            alias: json!({
                "RoutingConfiguration": [
                    { "StateMachineVersionArn": { "Fn::GetAtt": [VERSION, "Arn"] },
                      "Weight": "100" },
                ],
            }),
            alias_name: None,
            weight: 100,
        },
    ]
}

#[tokio::test]
async fn a_stack_published_version_and_alias_exist_in_step_functions() {
    for row in published_rows() {
        let Published {
            case,
            stack,
            alias,
            alias_name,
            weight,
        } = row;
        // One router per row, so what a row leaves in Step Functions cannot reach the next.
        let router = router();
        let mut template: Value =
            serde_json::from_str(&version_and_alias_template(alias)).expect("a JSON template");
        // The stack publishes the two ARNs as outputs, which is where `Fn::GetAtt … Arn` is
        // read from.
        template["Outputs"] = json!({
            "VersionArn": { "Value": { "Fn::GetAtt": [VERSION, "Arn"] } },
            "AliasArn": { "Value": { "Fn::GetAtt": [ALIAS, "Arn"] } },
        });
        let (code, body) = create_stack(&router, stack, &template.to_string(), &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected CreateStack to be accepted, got {code} {body}"
        );
        let settled = settled_stack(&router, case, stack).await;
        assert_eq!(
            xml_text(&settled, "StackStatus").as_deref(),
            Some(status::CREATE_COMPLETE),
            "case {case:?}: expected the create to complete, got {settled}"
        );

        let machine_arn = physical_id(case, &router, stack, MACHINE).await;
        let version_arn = physical_id(case, &router, stack, VERSION).await;
        let alias_arn = physical_id(case, &router, stack, ALIAS).await;

        // The version the stack published is the one Step Functions LISTS for the machine, and
        // the ARN the stack publishes for the resource is that version's own.
        let listed = listed_versions(&router, &machine_arn).await;
        assert_eq!(
            listed,
            vec![version_arn.clone()],
            "case {case:?}: expected ListStateMachineVersions to report the published version \
             {version_arn:?}, got {listed:?}"
        );
        assert_eq!(
            output_arn(case, &router, stack, "VersionArn").await,
            version_arn,
            "case {case:?}: expected Fn::GetAtt Arn to be the version ARN that service minted"
        );
        assert!(
            version_arn.starts_with(&format!("{machine_arn}:")),
            "case {case:?}: expected the version ARN to qualify the machine {machine_arn:?}, \
             got {version_arn:?}"
        );

        // ...and the alias really routes to that version, at the declared weight, under the
        // ARN the stack publishes.
        assert_eq!(
            output_arn(case, &router, stack, "AliasArn").await,
            alias_arn,
            "case {case:?}: expected Fn::GetAtt Arn to be the alias ARN that service minted"
        );
        let described = described_alias(&router, &alias_arn).await.unwrap_or_else(|| {
            panic!("case {case:?}: expected Step Functions to hold the alias {alias_arn:?}")
        });
        let reported_name = described
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        match alias_name {
            Some(expected) => assert_eq!(
                reported_name, expected,
                "case {case:?}: expected the declared alias name, got {described}"
            ),
            // A name CloudFormation generated is still the name the alias was created under,
            // which is the last segment of the ARN that service minted.
            None => assert!(
                !reported_name.is_empty() && alias_arn.ends_with(&format!(":{reported_name}")),
                "case {case:?}: expected the generated name to be the alias's own, got \
                 {reported_name:?} in {alias_arn:?}"
            ),
        }
        let routing = described
            .get("routingConfiguration")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            routing.len(),
            1,
            "case {case:?}: expected the declared routing configuration, got {described}"
        );
        assert_eq!(
            routing[0].get("stateMachineVersionArn").and_then(Value::as_str),
            Some(version_arn.as_str()),
            "case {case:?}: expected the alias to route to the published version, got \
             {described}"
        );
        assert_eq!(
            routing[0].get("weight").and_then(Value::as_i64),
            Some(weight),
            "case {case:?}: expected the declared weight {weight}, got {described}"
        );

        // The stack's deletion takes both away in the owning service, so no version and no
        // alias is left that nothing manages.
        let (code, body) = call(&router, "DeleteStack", &[member("StackName", stack)]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected DeleteStack to be accepted, got {code} {body}"
        );
        settled_stack(&router, case, stack).await;
        let left = listed_versions(&router, &machine_arn).await;
        assert!(
            !left.contains(&version_arn),
            "case {case:?}: expected the published version {version_arn:?} to be removed, got \
             {left:?}"
        );
        let left = described_alias(&router, &alias_arn).await;
        assert!(
            left.is_none(),
            "case {case:?}: expected the alias {alias_arn:?} to be removed, got {left:?}"
        );
    }
}

/// One row of the refused-create table: the template it deploys, the resource whose create the
/// owning service refuses, and a fragment of that service's own sentence.
struct Refused {
    case: &'static str,
    stack: &'static str,
    template: String,
    failing: &'static str,
    sentence: &'static str,
}

fn refused_rows() -> Vec<Refused> {
    vec![
        Refused {
            case: "an alias whose routing weights do not add up to the total that service \
                   requires",
            stack: "sfn-alias-weights-short",
            template: version_and_alias_template(json!({
                "Name": "live",
                "RoutingConfiguration": [
                    { "StateMachineVersionArn": { "Ref": VERSION }, "Weight": 50 },
                ],
            })),
            failing: ALIAS,
            sentence: "Sum of routing configuration weights must equal",
        },
        Refused {
            case: "an alias that routes no traffic at all",
            stack: "sfn-alias-weights-zero",
            template: version_and_alias_template(json!({
                "Name": "live",
                "RoutingConfiguration": [
                    { "StateMachineVersionArn": { "Ref": VERSION }, "Weight": 0 },
                ],
            })),
            failing: ALIAS,
            sentence: "Sum of routing configuration weights must equal",
        },
        Refused {
            case: "a version published from a state machine that service does not hold",
            stack: "sfn-version-absent-machine",
            template: json!({
                "Resources": {
                    VERSION: {
                        "Type": "AWS::StepFunctions::StateMachineVersion",
                        "Properties": {
                            "StateMachineArn":
                                "arn:aws:states:us-east-1:000000000000:stateMachine:no-such",
                        },
                    },
                },
            })
            .to_string(),
            failing: VERSION,
            sentence: "State Machine Does Not Exist",
        },
    ]
}

#[tokio::test]
async fn a_create_step_functions_refuses_fails_the_resource_and_rolls_the_stack_back() {
    for row in refused_rows() {
        let Refused {
            case,
            stack,
            template,
            failing,
            sentence,
        } = row;
        let router = router();
        let (code, body) = create_stack(&router, stack, &template, &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected CreateStack to be accepted — the creation starts and the \
             stack reports the outcome — got {code} {body}"
        );
        let settled = settled_stack(&router, case, stack).await;
        assert_eq!(
            xml_text(&settled, "StackStatus").as_deref(),
            Some(status::ROLLBACK_COMPLETE),
            "case {case:?}: expected the stack to roll back, got {settled}"
        );

        // The refusal reported on the failed resource is the one that service worded.
        let reason =
            resource_event_reason(&router, stack, failing, resource_status::CREATE_FAILED).await;
        assert!(
            reason.contains(sentence),
            "case {case:?}: expected the failed resource's reason to carry {sentence:?}, got \
             {reason:?}"
        );

        // ...and nothing was created: the resource's own id names no alias and no version
        // that service holds.
        let left = physical_id(case, &router, stack, failing).await;
        if left.starts_with("arn:") {
            let held = described_alias(&router, &left).await;
            assert!(
                held.is_none(),
                "case {case:?}: expected no alias to have been created, got {held:?}"
            );
        }
    }
}

/// One row of the SDK-diagnostic table: a template whose resource the owning service refuses,
/// and the sentence that service opens its refusal with.
struct Diagnosed {
    case: &'static str,
    stack: &'static str,
    template: String,
    failing: &'static str,
    opens_with: &'static str,
}

/// A state machine declaring `definition`, which is all a diagnostic row needs.
fn machine_template(definition: &str) -> String {
    json!({
        "Resources": {
            MACHINE: {
                "Type": "AWS::StepFunctions::StateMachine",
                "Properties": { "DefinitionString": definition, "RoleArn": ROLE_ARN },
            },
        },
    })
    .to_string()
}

fn diagnosed_rows() -> Vec<Diagnosed> {
    vec![
        Diagnosed {
            case: "a Pass state declaring both Next and End",
            stack: "sfn-next-and-end",
            template: machine_template(
                r#"{"StartAt":"Only","States":{"Only":{"Type":"Pass","Next":"Only","End":true}}}"#,
            ),
            failing: MACHINE,
            opens_with: "Invalid State Machine Definition",
        },
        Diagnosed {
            case: "a Choice whose Default names a state the definition does not declare",
            stack: "sfn-unknown-default",
            template: machine_template(
                r#"{"StartAt":"Pick","States":{"Pick":{"Type":"Choice","Choices":[{"Variable":"$.n","NumericEquals":1,"Next":"Done"}],"Default":"NoSuchState"},"Done":{"Type":"Pass","End":true}}}"#,
            ),
            failing: MACHINE,
            opens_with: "Invalid State Machine Definition",
        },
        Diagnosed {
            case: "an alias whose routing weights do not add up",
            stack: "sfn-diagnosed-alias",
            template: version_and_alias_template(json!({
                "Name": "live",
                "RoutingConfiguration": [
                    { "StateMachineVersionArn": { "Ref": VERSION }, "Weight": 40 },
                ],
            })),
            failing: ALIAS,
            opens_with: "Sum of routing configuration weights must equal",
        },
    ]
}

/// The quoted provider message of one `ResourceStatusReason`, and the class it was reported
/// under.
fn quoted_and_class(case: &str, reason: &str) -> (String, String) {
    crate::harness::quoted_and_class(case, reason)
}

/// Hold one quoted exception to the diagnostic AWS Step Functions' SDK appends: the service as
/// its SDK names it, the status that service answered, the id of the call and how many attempts
/// the SDK made — each present, and each of the FORM that service answers rather than one
/// recorded value.
fn assert_sfn_diagnostic(case: &str, quoted: &str, opens_with: &str) {
    assert!(
        quoted.starts_with(opens_with),
        "case {case:?}: expected the quoted text to open with the service's own message \
         {opens_with:?}, got {quoted:?}"
    );
    let (envelope, attempts) = quoted
        .rsplit_once(") (SDK Attempt Count: ")
        .unwrap_or_else(|| panic!("case {case:?}: expected the SDK diagnostic, got {quoted:?}"));
    let envelope = envelope
        .rsplit_once(" (")
        .map(|(_, fields)| fields)
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the diagnostic's own parenthesis, got {quoted:?}")
        });
    let fields: Vec<&str> = envelope.split(", ").collect();
    assert_eq!(
        fields.len(),
        3,
        "case {case:?}: expected the service, the status and the call's id, got {envelope:?}"
    );
    assert_eq!(
        fields[0], "Service: Sfn",
        "case {case:?}: expected the SDK's own name for the service, got {:?}",
        fields[0]
    );
    assert_eq!(
        fields[1], "Status Code: 400",
        "case {case:?}: expected the status Step Functions answered, got {:?}",
        fields[1]
    );
    let request_id = fields[2]
        .strip_prefix("Request ID: ")
        .unwrap_or_else(|| panic!("case {case:?}: expected a Request ID, got {:?}", fields[2]));
    assert!(
        !request_id.is_empty(),
        "case {case:?}: expected the id of the refused call, got {request_id:?}"
    );
    let attempts = attempts.strip_suffix(')').unwrap_or(attempts);
    assert!(
        attempts.parse::<u32>().is_ok_and(|made| made >= 1),
        "case {case:?}: expected the attempts the SDK made, got {attempts:?}"
    );
}

#[tokio::test]
async fn a_refused_step_functions_call_is_reported_with_its_sdk_diagnostic() {
    for row in diagnosed_rows() {
        let Diagnosed {
            case,
            stack,
            template,
            failing,
            opens_with,
        } = row;
        let router = router();
        let (code, body) = create_stack(&router, stack, &template, &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected CreateStack to be accepted, got {code} {body}"
        );
        let settled = settled_stack(&router, case, stack).await;
        assert_eq!(
            xml_text(&settled, "StackStatus").as_deref(),
            Some(status::ROLLBACK_COMPLETE),
            "case {case:?}: expected the stack to roll back, got {settled}"
        );
        let reason =
            resource_event_reason(&router, stack, failing, resource_status::CREATE_FAILED).await;
        let (quoted, class) = quoted_and_class(case, &reason);
        assert_sfn_diagnostic(case, &quoted, opens_with);
        assert_eq!(
            class, "InvalidRequest",
            "case {case:?}: expected the refused request's own class, got {class:?} in \
             {reason:?}"
        );
    }
}
