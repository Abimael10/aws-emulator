// SPDX-License-Identifier: CC0-1.0

//! What a stack operation reports when a declaration carries, in a member NESTED inside a
//! structured property, a value the resource type's registry schema does not state for it —
//! end to end over the awsQuery wire.
//!
//! RULE: CloudFormation validates each declared resource against its resource type's registry
//! schema before invoking any provider, so a property that schema states an `enum` for admits
//! those values and no other wherever that property sits in the declaration — a member nested
//! inside a structured property included — and a value outside the set fails the operation at
//! that stage: nothing is asked of the service that owns the resource, the stack carries
//! CloudFormation's own summary over the stage's errors, and the diagnosis is the validation
//! result recorded for the resource it failed on — `<value> is not a valid enum value.
//! Supported values: [<the set>]` at the JSON pointer of that member in the submitted
//! template.
//!
//! The rows of the first table differ along what the rule generalises over: whether the nested
//! value is one the schema states (both of them, including the one the report leading here did
//! not cite) or not, how it differs from one it states (a name the set does not carry at all,
//! a spelling that differs only in case), and whether the declaration states the structure the
//! member sits in without the member, which carries no value to judge.
//!
//! The second test is the recorded sequence: a stack deployed under a merge type the schema
//! states, then updated to one it does not — the update is refused at the same stage, rolls
//! back, reports the one validation result, and leaves the link AppSync holds exactly as it
//! was.
//!
//! Nothing here waits out a span: every stack is polled to its terminal status.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::appsync_event_and_merged_api_resources::appsync;
use crate::harness::{
    call, create_stack, member, router, settled_stack, settled_stack_status, xml_blocks, xml_text,
    REGION,
};
use service_com_amazonaws_cloudformation_cloudformation::state::status;

/// The validation CloudFormation names for the check that holds a declaration's properties to
/// its type's schema. Source: `state::property_validation::PROPERTY_VALIDATION`.
const PROPERTY_VALIDATION: &str = "PROPERTY_VALIDATION";

/// The `EventType` a validation result is reported under, from the model's `EventType` enum
/// (`smithy_query(query="shape", shape="EventType")`).
const VALIDATION_ERROR: &str = "VALIDATION_ERROR";

/// `OperationEvent$ValidationFailureMode` for a validation whose failure was handled by
/// FAILING the operation, from the model's failure-mode enum.
const FAIL: &str = "FAIL";

/// `OperationEvent$ValidationStatus` for a validation the declaration did not pass.
const FAILED: &str = "FAILED";

/// The logical ids the template below declares its resources under.
const ROLE: &str = "MergeRole";
const SOURCE: &str = "Source";
const SOURCE_SCHEMA: &str = "SourceSchema";
const MERGED: &str = "Merged";
const LINK: &str = "Association";

/// The structured property carrying the link's merge configuration, and the member inside it
/// whose value the type's schema states an `enum` for.
const ASSOCIATION_CONFIG: &str = "SourceApiAssociationConfig";
const MERGE_TYPE: &str = "MergeType";

/// The values `AWS::AppSync::SourceApiAssociation`'s schema states for that member, in the
/// order a refusal lists them back. Source: AppSync's own `MergeType`
/// (`smithy_query(query="shape", shape="MergeType")`), the enum
/// `SourceApiAssociationConfig$mergeType` targets — "valid merge types are MANUAL_MERGE
/// (default) and AUTO_MERGE".
const MERGE_TYPES: &str = "AUTO_MERGE, MANUAL_MERGE";

/// How the schema validator reports a value outside the set its property's schema states.
/// Source: `state::property_validation::enum_properties::enum_failure_reason`.
fn not_a_valid_enum_value(value: &str) -> String {
    format!("{value} is not a valid enum value. Supported values: [{MERGE_TYPES}]")
}

/// The JSON pointer of the judged member in the submitted template: the resource's own
/// declaration, its property document, the structure the member sits in, and the member.
fn merge_type_path() -> String {
    format!("/Resources/{LINK}/Properties/{ASSOCIATION_CONFIG}/{MERGE_TYPE}")
}

/// A template declaring a source GraphQL API with a schema, a Merged API, and the link between
/// them carrying `config` as its merge configuration — or carrying none at all for `None`.
fn template(config: Option<Value>) -> String {
    let mut link = json!({
        "MergedApiIdentifier": {"Fn::GetAtt": [MERGED, "ApiId"]},
        "SourceApiIdentifier": {"Fn::GetAtt": [SOURCE, "ApiId"]},
        "Description": "the merge-type link",
    });
    if let (Value::Object(document), Some(config)) = (&mut link, config) {
        document.insert(ASSOCIATION_CONFIG.to_string(), config);
    }
    json!({
        "Resources": {
            ROLE: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": {"Service": "appsync.amazonaws.com"},
                            "Action": "sts:AssumeRole",
                        }],
                    },
                },
            },
            SOURCE: {
                "Type": "AWS::AppSync::GraphQLApi",
                "Properties": {"Name": "merge-type-source", "AuthenticationType": "API_KEY"},
            },
            SOURCE_SCHEMA: {
                "Type": "AWS::AppSync::GraphQLSchema",
                "Properties": {
                    "ApiId": {"Fn::GetAtt": [SOURCE, "ApiId"]},
                    "Definition": "type Order { id: ID! } type Query { order(id: ID!): Order } \
                                   schema { query: Query }",
                },
            },
            MERGED: {
                "Type": "AWS::AppSync::GraphQLApi",
                "Properties": {
                    "Name": "merge-type-merged",
                    "AuthenticationType": "API_KEY",
                    "ApiType": "MERGED",
                    "MergedApiExecutionRoleArn": {"Fn::GetAtt": [ROLE, "Arn"]},
                },
            },
            LINK: {
                "Type": "AWS::AppSync::SourceApiAssociation",
                "DependsOn": [SOURCE_SCHEMA],
                "Properties": link,
            },
        },
        "Outputs": {
            "MergedApiIdOut": {"Value": {"Fn::GetAtt": [LINK, "MergedApiId"]}},
            "AssociationIdOut": {"Value": {"Fn::GetAtt": [LINK, "AssociationId"]}},
        },
    })
    .to_string()
}

/// The role the template declares has to be acknowledged, as it does for any template naming an
/// IAM resource.
fn capabilities() -> Vec<(String, String)> {
    vec![member("Capabilities.member.1", "CAPABILITY_IAM")]
}

/// One row: the merge configuration the link declares, and the value the validation must refuse
/// — `None` for a declaration whose nested value the schema states (or states nothing about),
/// which is deployed.
struct Row {
    case: &'static str,
    stack: &'static str,
    config: Option<Value>,
    refused: Option<&'static str>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a merge type the schema does not state at all",
            stack: "nested-enum-unknown-merge-type",
            config: Some(json!({MERGE_TYPE: "NOT_A_REAL_MERGE_TYPE"})),
            refused: Some("NOT_A_REAL_MERGE_TYPE"),
        },
        Row {
            case: "a merge type differing from one it states only in case",
            stack: "nested-enum-lowercase-merge-type",
            config: Some(json!({MERGE_TYPE: "auto_merge"})),
            refused: Some("auto_merge"),
        },
        Row {
            case: "the merge type the schema states first",
            stack: "nested-enum-auto-merge",
            config: Some(json!({MERGE_TYPE: "AUTO_MERGE"})),
            refused: None,
        },
        Row {
            case: "the default merge type the schema states",
            stack: "nested-enum-manual-merge",
            config: Some(json!({MERGE_TYPE: "MANUAL_MERGE"})),
            refused: None,
        },
        Row {
            case: "a merge configuration stating no merge type at all",
            stack: "nested-enum-empty-config",
            config: Some(json!({})),
            refused: None,
        },
    ]
}

/// The validation RESULTS `DescribeEvents` reports for `stack`: one tuple per event carrying a
/// `ValidationName`, in the order the operation's events are reported.
async fn validation_results(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<(String, String, String, String, String, String, String)> {
    let (code, body) = call(router, "DescribeEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the operation's events must be readable, got {code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .filter(|event| xml_text(event, "EventType").as_deref() == Some(VALIDATION_ERROR))
        .map(|event| {
            (
                xml_text(event, "LogicalResourceId").unwrap_or_default(),
                xml_text(event, "EventType").unwrap_or_default(),
                xml_text(event, "ValidationName").unwrap_or_default(),
                xml_text(event, "ValidationStatus").unwrap_or_default(),
                xml_text(event, "ValidationFailureMode").unwrap_or_default(),
                xml_text(event, "ValidationStatusReason").unwrap_or_default(),
                xml_text(event, "ValidationPath").unwrap_or_default(),
            )
        })
        .collect()
}

/// The one validation result a refused declaration owes, as the tuple above carries it.
fn expected_result(rejected: &str) -> Vec<(String, String, String, String, String, String, String)> {
    vec![(
        LINK.to_string(),
        VALIDATION_ERROR.to_string(),
        PROPERTY_VALIDATION.to_string(),
        FAILED.to_string(),
        FAIL.to_string(),
        not_a_valid_enum_value(rejected),
        merge_type_path(),
    )]
}

/// How many events `stack` recorded about one of its DECLARED resources, which is how a
/// declaration refused before provisioning is told from one that was deployed: the refused
/// operation asks nothing of any owning service, so no resource of the stack reports an event.
async fn resource_events(router: &MultiServiceRouter, case: &str, stack: &str) -> usize {
    let (code, body) = call(router, "DescribeStackEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the stack's events must be readable, got {code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .filter(|event| {
            matches!(
                xml_text(event, "LogicalResourceId").as_deref(),
                Some(ROLE) | Some(SOURCE) | Some(SOURCE_SCHEMA) | Some(MERGED) | Some(LINK)
            )
        })
        .count()
}

/// One output's value off a settled `DescribeStacks` body, keyed by `OutputKey`.
fn output(case: &str, described: &str, key: &str) -> String {
    let keys = xml_blocks(described, "Outputs");
    let outputs = keys.first().cloned().unwrap_or_default();
    let found = xml_blocks(&outputs, "member")
        .into_iter()
        .find(|entry| xml_text(entry, "OutputKey").as_deref() == Some(key))
        .and_then(|entry| xml_text(&entry, "OutputValue"));
    found.unwrap_or_else(|| panic!("case {case:?}: expected the output {key:?} in {described}"))
}

/// The merge type AppSync itself holds for the link the stack published, read back through that
/// service's own `GetSourceApiAssociation`.
async fn held_merge_type(router: &MultiServiceRouter, case: &str, described: &str) -> String {
    let merged_api_id = output(case, described, "MergedApiIdOut");
    let association_id = output(case, described, "AssociationIdOut");
    let uri = format!("/v1/mergedApis/{merged_api_id}/sourceApiAssociations/{association_id}");
    let (code, read) = appsync(router, &uri).await;
    assert_eq!(
        code, 200,
        "case {case:?}: GetSourceApiAssociation({uri:?}) must answer 200, got {code} {read}"
    );
    read.get("sourceApiAssociation")
        .and_then(|association| association.get("sourceApiAssociationConfig"))
        .and_then(|config| config.get("mergeType"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Every row: a nested value the type's schema does not state fails the stack before anything
/// is provisioned — the validation result naming the resource, the rejected value, the set and
/// the member's own path, and no resource event at all — while a value the schema states is
/// deployed and read back in the service that owns the link.
#[tokio::test]
async fn a_nested_value_outside_its_schemas_enum_rolls_the_stack_back() {
    for row in rows() {
        let case = row.case;
        // A router of its own per row: what each case is about is its own declaration, not what
        // another row left behind.
        let router = router();

        let (code, created) = create_stack(
            &router,
            row.stack,
            &template(row.config.clone()),
            &capabilities(),
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must answer 200 — the request is valid and the stack \
             exists — got {code} {created}"
        );

        let described = settled_stack(case, &router, REGION, row.stack).await;
        let settled = xml_text(&described, "StackStatus").unwrap_or_default();
        let expected_status = match row.refused {
            Some(_) => status::ROLLBACK_COMPLETE,
            None => status::CREATE_COMPLETE,
        };
        assert_eq!(
            settled, expected_status,
            "case {case:?}: expected the stack to settle {expected_status}, got {settled:?} \
             ({described})"
        );

        let results = validation_results(&router, case, row.stack).await;
        let expected_results = row.refused.map(expected_result).unwrap_or_default();
        assert_eq!(
            results, expected_results,
            "case {case:?}: expected the validation results {expected_results:?}, got {results:?}"
        );

        let events = resource_events(&router, case, row.stack).await;
        match row.refused {
            Some(_) => assert_eq!(
                events, 0,
                "case {case:?}: a refusal before provisioning must record no resource event, \
                 got {events} of them"
            ),
            None => {
                assert!(
                    events > 0,
                    "case {case:?}: a deployed declaration must record resource events, got \
                     {events}"
                );
                // The merge type the schema states is the one AppSync was really asked for:
                // the stated value where the declaration stated one, and the modelled default
                // where it stated the structure without the member.
                let expected_merge_type = row
                    .config
                    .as_ref()
                    .and_then(|config| config.get(MERGE_TYPE))
                    .and_then(Value::as_str)
                    .unwrap_or("MANUAL_MERGE");
                let held = held_merge_type(&router, case, &described).await;
                assert_eq!(
                    held, expected_merge_type,
                    "case {case:?}: expected AppSync to hold the declared merge type \
                     {expected_merge_type:?}, got {held:?}"
                );
            }
        }
    }
}

/// The recorded sequence: a stack deployed under a merge type the schema states, then updated
/// to one it does not. The update is refused at the same stage, rolls back, reports exactly one
/// validation result about the member it refused, and leaves AppSync's own record of the link
/// untouched.
#[tokio::test]
async fn an_update_to_a_nested_value_outside_the_enum_is_refused_and_rolled_back() {
    let case = "an update changing the merge type to one the schema does not state";
    let router = router();
    let stack = "nested-enum-updated-merge-type";

    let (code, created) = create_stack(
        &router,
        stack,
        &template(Some(json!({MERGE_TYPE: "MANUAL_MERGE"}))),
        &capabilities(),
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must answer 200, got {code} {created}"
    );
    let described = settled_stack(case, &router, REGION, stack).await;
    let settled = xml_text(&described, "StackStatus").unwrap_or_default();
    assert_eq!(
        settled,
        status::CREATE_COMPLETE,
        "case {case:?}: expected the stack to be created, got {settled:?} ({described})"
    );
    let held = held_merge_type(&router, case, &described).await;
    assert_eq!(
        held, "MANUAL_MERGE",
        "case {case:?}: expected AppSync to hold the declared merge type, got {held:?}"
    );

    // The same template, with the one member changed to a value the schema does not state.
    let mut members = vec![
        member("StackName", stack),
        member(
            "TemplateBody",
            &template(Some(json!({MERGE_TYPE: "NOT_A_REAL_MERGE_TYPE"}))),
        ),
    ];
    members.extend(capabilities());
    let (code, updated) = call(&router, "UpdateStack", &members).await;
    assert_eq!(
        code, 200,
        "case {case:?}: UpdateStack must answer 200 — the request itself is valid — got {code} \
         {updated}"
    );

    let settled = settled_stack_status(case, &router, REGION, stack).await;
    assert_eq!(
        settled,
        status::UPDATE_ROLLBACK_COMPLETE,
        "case {case:?}: expected the refused update to roll back, got {settled:?}"
    );

    let results = validation_results(&router, case, stack).await;
    let expected_results = expected_result("NOT_A_REAL_MERGE_TYPE");
    assert_eq!(
        results, expected_results,
        "case {case:?}: expected the validation results {expected_results:?}, got {results:?}"
    );

    // Nothing was asked of AppSync: the link it holds is still the one the create made.
    let described = settled_stack(case, &router, REGION, stack).await;
    let held = held_merge_type(&router, case, &described).await;
    assert_eq!(
        held, "MANUAL_MERGE",
        "case {case:?}: expected the refused update to leave the link as it was, got {held:?}"
    );
}
