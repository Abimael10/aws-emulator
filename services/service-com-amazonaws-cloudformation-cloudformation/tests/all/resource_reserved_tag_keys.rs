// SPDX-License-Identifier: CC0-1.0

//! The reserved `aws:` tag-key namespace in a RESOURCE's own `Tags`, end to end.
//!
//! RULE under test: **a tag key inside the namespace Amazon Web Services keeps for the tags it
//! sets itself is not one a caller may state — so a resource whose own `Tags` claim one FAIL
//! that resource, carrying the very sentence this service answers a caller-supplied reserved
//! key with, before the service that owns the type is asked for anything; the stack rolls back
//! to `ROLLBACK_COMPLETE` and the resources it had already made are removed. The three
//! `aws:cloudformation:*` records CloudFormation keeps itself are the exception — they are
//! CloudFormation's to set, so a declaration spelling one is overwritten rather than refused —
//! and every key outside the namespace deploys and reaches the owning service as declared.**
//!
//! The namespace is this service's own: `CreateStack` already refuses a caller the namespace on
//! a STACK's `Tags` with this same fixed sentence (`reserved_stack_tag_keys.rs`). What is
//! asserted here is the other half — the keys a RESOURCE's own `Tags` property states.
//!
//! The rows differ along what the rule generalises over: the SPELLING of the type's `Tags`
//! property (the MAP a few types publish and the `[{Key, Value}]` list most do, both of which
//! `AWS::SSM::Parameter` accepts), the shape of the offending key (a name after the prefix, the
//! bare prefix, the prefix in upper case, a key in another service's namespace), WHERE in the
//! set it sits, and the resource TYPE whose declaration carries it (`AWS::SSM::Parameter` and
//! `AWS::SSM::Document`). Every outcome is read through the OWNING service's own wire —
//! `ssm GetParameter`, `ssm DescribeDocument` and `ssm ListTagsForResource` — never out of
//! CloudFormation's record of what it thinks it did.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::stack_tags::{
    RESERVED_TAG_KEY, RESERVED_TAG_KEY_PREFIX,
};
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

use crate::harness::{create_stack, member, router, settled_stack_status, REGION};
use crate::stack_lifecycle::resource_event_reason;
use crate::update_rollback_recovery::ssm;

/// The logical id of the resource whose `Tags` each row varies. The sibling created BEFORE it
/// — so a rollback has a resource to take back out again — is `Sibling`, named by the
/// templates themselves.
const TAGGED: &str = "Tagged";

/// A minimal `Command` document, for the row whose tagged resource is an `AWS::SSM::Document`.
const DOCUMENT_CONTENT: &str = r#"{
  "schemaVersion": "2.2",
  "description": "A document a stack declares",
  "mainSteps": [
    {
      "action": "aws:runShellScript",
      "name": "runShell",
      "inputs": { "runCommand": ["echo hi"] }
    }
  ]
}"#;

/// Which spelling of a type's `Tags` property a row declares.
#[derive(Clone, Copy, Debug)]
enum TagShape {
    /// The MAP of key to value (`{"key": "value"}`), which is the form the report cited.
    Map,
    /// The `[{Key, Value}]` list most types publish.
    List,
}

impl TagShape {
    /// The `Tags` property value one tag set is written as in this spelling.
    fn property(self, pairs: &[(String, String)]) -> Value {
        match self {
            Self::Map => Value::Object(
                pairs
                    .iter()
                    .map(|(key, value)| (key.clone(), Value::String(value.clone())))
                    .collect(),
            ),
            Self::List => Value::Array(
                pairs
                    .iter()
                    .map(|(key, value)| json!({ "Key": key, "Value": value }))
                    .collect(),
            ),
        }
    }
}

/// One row: a stack whose tagged resource declares `pairs` in the spelling `shape`.
struct Row {
    case: &'static str,
    stack: &'static str,
    shape: TagShape,
    /// The `(key, value)` pairs the tagged resource's `Tags` states.
    pairs: Vec<(String, String)>,
}

/// The parameter name the tagged resource of `stack` is deployed under.
fn tagged_name(stack: &str) -> String {
    format!("/{stack}/tagged")
}

/// The parameter name the sibling of `stack` is deployed under.
fn sibling_name(stack: &str) -> String {
    format!("/{stack}/sibling")
}

/// The template a row deploys: a parameter with no tags of its own, and — after it — the
/// parameter whose `Tags` the row states.
fn template(stack: &str, tags: &Value) -> String {
    json!({"Resources": {
        "Sibling": {
            "Type": "AWS::SSM::Parameter",
            "Properties": {"Name": sibling_name(stack), "Type": "String", "Value": "v"},
        },
        "Tagged": {
            "Type": "AWS::SSM::Parameter",
            "DependsOn": "Sibling",
            "Properties": {
                "Name": tagged_name(stack),
                "Type": "String",
                "Value": "v",
                "Tags": tags,
            },
        },
    }})
    .to_string()
}

/// The rows whose declared key lies inside the reserved namespace. Every key is built from
/// [`RESERVED_TAG_KEY_PREFIX`] rather than spelled by hand, so the rows state the namespace
/// this crate reserves and not a copy of it.
fn refused_rows() -> Vec<Row> {
    let prefix = RESERVED_TAG_KEY_PREFIX;
    let named = format!("{prefix}hostile");
    let upper = format!("{}hostile", prefix.to_ascii_uppercase());
    // The namespace of ANOTHER service, whose keys that service sets and this caller may not.
    let another_service = format!("{prefix}servicecatalog:provisioningPrincipalArn");
    vec![
        Row {
            case: "the reserved prefix in the Map spelling of the property",
            stack: "reserved-resource-tag-map",
            shape: TagShape::Map,
            pairs: vec![(named.clone(), "y".to_string())],
        },
        Row {
            case: "the reserved prefix in the list spelling of the property",
            stack: "reserved-resource-tag-list",
            shape: TagShape::List,
            pairs: vec![(named.clone(), "y".to_string())],
        },
        Row {
            case: "the bare reserved prefix as the whole key",
            stack: "reserved-resource-tag-bare",
            shape: TagShape::Map,
            pairs: vec![(prefix.to_string(), "y".to_string())],
        },
        Row {
            case: "the reserved prefix in upper case, which claims the same namespace",
            stack: "reserved-resource-tag-upper",
            shape: TagShape::Map,
            pairs: vec![(upper, "y".to_string())],
        },
        Row {
            case: "a key in another AWS service's own namespace",
            stack: "reserved-resource-tag-other",
            shape: TagShape::List,
            pairs: vec![(another_service, "someone".to_string())],
        },
        Row {
            case: "a reserved key beside an ordinary one, so the set is read past its first \
                   entry",
            stack: "reserved-resource-tag-beside",
            shape: TagShape::List,
            pairs: vec![
                ("team".to_string(), "platform".to_string()),
                (named, "y".to_string()),
            ],
        },
    ]
}

/// The rows whose declared keys the declaration may state — the ones outside the namespace,
/// the one that merely RESEMBLES the prefix, and the keys CloudFormation records itself, which
/// it sets rather than refuses.
fn admitted_rows() -> Vec<Row> {
    let prefix = RESERVED_TAG_KEY_PREFIX;
    vec![
        Row {
            case: "an ordinary key in the Map spelling of the property",
            stack: "admitted-resource-tag-map",
            shape: TagShape::Map,
            pairs: vec![("param-resource-tag".to_string(), "pv".to_string())],
        },
        Row {
            case: "an ordinary key in the list spelling of the property",
            stack: "admitted-resource-tag-list",
            shape: TagShape::List,
            pairs: vec![("param-resource-tag".to_string(), "pv".to_string())],
        },
        Row {
            case: "a key that only resembles the reserved prefix, the prefix away from the \
                   start",
            stack: "admitted-resource-tag-lookalike",
            shape: TagShape::Map,
            pairs: vec![(format!("team-{prefix}mine"), "pv".to_string())],
        },
        Row {
            case: "the letters of the prefix without the colon that makes it",
            stack: "admitted-resource-tag-letters",
            shape: TagShape::List,
            pairs: vec![(
                prefix.trim_end_matches(':').to_string(),
                "pv".to_string(),
            )],
        },
    ]
}

/// Whether Parameter Store holds a parameter of this name, read through its own wire.
async fn parameter_exists(router: &MultiServiceRouter, name: &str) -> bool {
    let (status_code, _) = ssm(router, "GetParameter", &json!({"Name": name})).await;
    status_code == 200
}

/// The tags Systems Manager holds for a parameter, read through its own wire.
async fn parameter_tags(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> BTreeMap<String, String> {
    let (status_code, body) = ssm(
        router,
        "ListTagsForResource",
        &json!({"ResourceType": "Parameter", "ResourceId": name}),
    )
    .await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: ssm ListTagsForResource({name}) must answer 200, got {status_code} \
         {body}"
    );
    body["TagList"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|tag| {
                    Some((
                        tag["Key"].as_str()?.to_string(),
                        tag["Value"].as_str().unwrap_or_default().to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// RULE: a reserved key in a resource's own `Tags` fails that resource and rolls the stack
/// back, in either spelling of the property, with nothing created in the owning service.
#[tokio::test]
async fn a_reserved_tag_key_in_a_resources_tags_rolls_the_stack_back() {
    for row in refused_rows() {
        let router = router();
        let template = template(row.stack, &row.shape.property(&row.pairs));
        let (code, body) = create_stack(&router, row.stack, &template, &[]).await;
        assert_eq!(
            code, 200,
            "case {:?}: CreateStack must be accepted, got {code} {body}",
            row.case
        );

        let settled = settled_stack_status(row.case, &router, REGION, row.stack).await;
        assert_eq!(
            settled,
            status::ROLLBACK_COMPLETE,
            "case {:?}: expected the stack to roll back, got {settled:?}",
            row.case
        );

        let reason =
            resource_event_reason(&router, row.stack, TAGGED, resource_status::CREATE_FAILED).await;
        assert_eq!(
            reason, RESERVED_TAG_KEY,
            "case {:?}: expected the failed resource to carry this service's own \
             reserved-prefix sentence, got {reason:?}",
            row.case
        );

        assert!(
            !parameter_exists(&router, &tagged_name(row.stack)).await,
            "case {:?}: expected no parameter to be written for the refused resource",
            row.case
        );
        assert!(
            !parameter_exists(&router, &sibling_name(row.stack)).await,
            "case {:?}: expected the rollback to remove the sibling the create had made",
            row.case
        );
    }
}

/// RULE: a key the declaration may state deploys — the stack completes and the owning service
/// holds the declared tag beside the stack's own.
#[tokio::test]
async fn an_admitted_tag_key_deploys_and_reaches_the_owning_service() {
    for row in admitted_rows() {
        let router = router();
        let template = template(row.stack, &row.shape.property(&row.pairs));
        // A stack-level tag as well, so what the parameter carries is the declared key BESIDE
        // the stack's own.
        let stack_tags = &[
            member("Tags.member.1.Key", "owner"),
            member("Tags.member.1.Value", "platform"),
        ];
        let (code, body) = create_stack(&router, row.stack, &template, stack_tags).await;
        assert_eq!(
            code, 200,
            "case {:?}: CreateStack must be accepted, got {code} {body}",
            row.case
        );

        let settled = settled_stack_status(row.case, &router, REGION, row.stack).await;
        assert_eq!(
            settled,
            status::CREATE_COMPLETE,
            "case {:?}: expected the stack to complete, got {settled:?}",
            row.case
        );

        let name = tagged_name(row.stack);
        assert!(
            parameter_exists(&router, &name).await,
            "case {:?}: expected the parameter to be written",
            row.case
        );

        let held = parameter_tags(&router, row.case, &name).await;
        for (key, value) in &row.pairs {
            assert_eq!(
                held.get(key).map(String::as_str),
                Some(value.as_str()),
                "case {:?}: expected the declared tag {key:?} on the parameter, got {held:?}",
                row.case
            );
        }
        assert_eq!(
            held.get("owner").map(String::as_str),
            Some("platform"),
            "case {:?}: expected the stack's own tag on the parameter, got {held:?}",
            row.case
        );
    }
}

/// RULE: a declaration spelling one of the three keys CloudFormation records itself is the
/// exception — it is CloudFormation's to set, so the resource is created and the key carries
/// CloudFormation's own value rather than the one the template asked for.
#[tokio::test]
async fn a_key_cloudformation_records_itself_is_set_rather_than_refused() {
    let router = router();
    let stack = "cloudformation-own-resource-tag";
    // Source: the `aws:cloudformation:*` keys CloudFormation's own documentation names for a
    // resource a stack provisions, which `crate::stack_tag_propagation` reads too.
    let logical_id_key = crate::stack_tag_propagation::LOGICAL_ID_TAG;
    let stack_name_key = crate::stack_tag_propagation::STACK_NAME_TAG;
    let declared = json!({
        logical_id_key: "Forged",
        stack_name_key: "not-this-stack",
        "team": "platform",
    });
    let (code, body) = create_stack(&router, stack, &template(stack, &declared), &[]).await;
    assert_eq!(
        code, 200,
        "case \"CloudFormation's own keys\": CreateStack must be accepted, got {code} {body}"
    );
    let settled = settled_stack_status("CloudFormation's own keys", &router, REGION, stack).await;
    assert_eq!(
        settled,
        status::CREATE_COMPLETE,
        "case \"CloudFormation's own keys\": expected the stack to complete, got {settled:?}"
    );

    let name = tagged_name(stack);
    let held = parameter_tags(&router, "CloudFormation's own keys", &name).await;
    assert_eq!(
        held.get(logical_id_key).map(String::as_str),
        Some(TAGGED),
        "case \"CloudFormation's own keys\": expected CloudFormation's own logical id rather \
         than the declared value, got {held:?}"
    );
    assert_eq!(
        held.get(stack_name_key).map(String::as_str),
        Some(stack),
        "case \"CloudFormation's own keys\": expected CloudFormation's own stack name rather \
         than the declared value, got {held:?}"
    );
    assert_eq!(
        held.get("team").map(String::as_str),
        Some("platform"),
        "case \"CloudFormation's own keys\": expected the ordinary declared tag to stand, got \
         {held:?}"
    );
}

/// RULE: the same refusal holds for another resource type — an `AWS::SSM::Document`, whose
/// `Tags` are a list — so it is the tag key that fails the resource rather than anything about
/// one type.
#[tokio::test]
async fn a_reserved_tag_key_fails_another_resource_type_too() {
    let router = router();
    let stack = "reserved-resource-tag-document";
    let document = "reserved-tag-document";
    let reserved = format!("{RESERVED_TAG_KEY_PREFIX}hostile");
    let template = json!({"Resources": {
        "Tagged": {
            "Type": "AWS::SSM::Document",
            "Properties": {
                "Name": document,
                "DocumentType": "Command",
                "Content": serde_json::from_str::<Value>(DOCUMENT_CONTENT)
                    .expect("the document content is JSON"),
                "Tags": [{"Key": reserved, "Value": "y"}],
            },
        },
    }})
    .to_string();

    let (code, body) = create_stack(&router, stack, &template, &[]).await;
    assert_eq!(
        code, 200,
        "case \"a document\": CreateStack must be accepted, got {code} {body}"
    );
    let settled = settled_stack_status("a document", &router, REGION, stack).await;
    assert_eq!(
        settled,
        status::ROLLBACK_COMPLETE,
        "case \"a document\": expected the stack to roll back, got {settled:?}"
    );
    let reason =
        resource_event_reason(&router, stack, TAGGED, resource_status::CREATE_FAILED).await;
    assert_eq!(
        reason, RESERVED_TAG_KEY,
        "case \"a document\": expected this service's own reserved-prefix sentence, got \
         {reason:?}"
    );
    let (status_code, described) =
        ssm(&router, "DescribeDocument", &json!({"Name": document})).await;
    assert_ne!(
        status_code, 200,
        "case \"a document\": expected no document to be written, got {described}"
    );
}

/// RULE: the same refusal holds for a type of ANOTHER owning service whose tags ride on its
/// own create — an `AWS::Lambda::Function`, whose declared `Tags` AWS Lambda stores — so the
/// rule is the declaration's and not one owning service's. (This case lived as a row of
/// `crate::lambda_function_stack_tags` while a declared reserved key was expected to be
/// dropped and the stack to complete; the recorded behaviour is this one.)
#[tokio::test]
async fn a_reserved_tag_key_fails_a_type_of_another_owning_service() {
    let router = router();
    let stack = "reserved-resource-tag-function";
    let reserved = format!("{RESERVED_TAG_KEY_PREFIX}servicecatalog:provisioningPrincipalArn");
    // The execution role the function RUNS AS, which IAM really holds — the same declaration
    // `crate::lambda_function_stack_tags` deploys its functions with.
    let template = json!({"Resources": {
        "HandlerRole": {
            "Type": "AWS::IAM::Role",
            "Properties": {"AssumeRolePolicyDocument": {
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": {"Service": "lambda.amazonaws.com"},
                    "Action": "sts:AssumeRole",
                }],
            }},
        },
        "Tagged": {
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "FunctionName": "reserved-tag-function",
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Role": {"Fn::GetAtt": ["HandlerRole", "Arn"]},
                "Code": {"ZipFile": "def handler(event, context):\n    return {}\n"},
                "Tags": [
                    {"Key": reserved, "Value": "someone"},
                    {"Key": "stage", "Value": "prod"},
                ],
            },
        },
    }})
    .to_string();

    let extra = crate::stack_lifecycle::acknowledging_iam(&[]);
    let (code, body) = create_stack(&router, stack, &template, &extra).await;
    assert_eq!(
        code, 200,
        "case \"a function\": CreateStack must be accepted, got {code} {body}"
    );
    let settled = settled_stack_status("a function", &router, REGION, stack).await;
    assert_eq!(
        settled,
        status::ROLLBACK_COMPLETE,
        "case \"a function\": expected the stack to roll back, got {settled:?}"
    );
    let reason =
        resource_event_reason(&router, stack, TAGGED, resource_status::CREATE_FAILED).await;
    assert_eq!(
        reason, RESERVED_TAG_KEY,
        "case \"a function\": expected this service's own reserved-prefix sentence, got \
         {reason:?}"
    );
}

/// RULE: the same refusal holds for a type that publishes its own tag set under a property of
/// ANOTHER NAME — an `AWS::Cognito::UserPool`'s `UserPoolTags` — so the rule follows the
/// resource's own tag set rather than the one property name most types spell it with.
#[tokio::test]
async fn a_reserved_tag_key_fails_a_type_whose_tag_property_has_another_name() {
    let router = router();
    let stack = "reserved-resource-tag-user-pool";
    let reserved = format!("{RESERVED_TAG_KEY_PREFIX}hostile");
    let template = json!({"Resources": {
        "Tagged": {
            "Type": "AWS::Cognito::UserPool",
            "Properties": {
                "UserPoolName": "reserved-tag-pool",
                // The property Amazon Cognito's own arm reads a pool's declared tags from,
                // which the type publishes as a MAP rather than as a list.
                "UserPoolTags": {reserved.as_str(): "y"},
            },
        },
    }})
    .to_string();

    let (code, body) = create_stack(&router, stack, &template, &[]).await;
    assert_eq!(
        code, 200,
        "case \"a user pool\": CreateStack must be accepted, got {code} {body}"
    );
    let settled = settled_stack_status("a user pool", &router, REGION, stack).await;
    assert_eq!(
        settled,
        status::ROLLBACK_COMPLETE,
        "case \"a user pool\": expected the stack to roll back, got {settled:?}"
    );
    let reason =
        resource_event_reason(&router, stack, TAGGED, resource_status::CREATE_FAILED).await;
    assert_eq!(
        reason, RESERVED_TAG_KEY,
        "case \"a user pool\": expected this service's own reserved-prefix sentence, got \
         {reason:?}"
    );
}

/// RULE: an UPDATE's declaration is held to the same namespace — a changed `Tags` claiming a
/// reserved key fails that resource's update and the owning service is not asked to apply it,
/// so the parameter keeps the value and the tags the create left it with.
#[tokio::test]
async fn a_reserved_tag_key_in_an_update_fails_that_resource() {
    let router = router();
    let stack = "reserved-resource-tag-update";
    let deployed = template(
        stack,
        &TagShape::Map.property(&[("team".to_string(), "platform".to_string())]),
    );
    let (code, body) = create_stack(&router, stack, &deployed, &[]).await;
    assert_eq!(
        code, 200,
        "case \"an update\": CreateStack must be accepted, got {code} {body}"
    );
    let settled = settled_stack_status("an update", &router, REGION, stack).await;
    assert_eq!(
        settled,
        status::CREATE_COMPLETE,
        "case \"an update\": the stack under test must be created, got {settled:?}"
    );

    let revised = json!({"Resources": {
        "Sibling": {
            "Type": "AWS::SSM::Parameter",
            "Properties": {"Name": sibling_name(stack), "Type": "String", "Value": "v"},
        },
        "Tagged": {
            "Type": "AWS::SSM::Parameter",
            "DependsOn": "Sibling",
            "Properties": {
                "Name": tagged_name(stack),
                "Type": "String",
                "Value": "changed",
                "Tags": {format!("{RESERVED_TAG_KEY_PREFIX}hostile"): "y"},
            },
        },
    }})
    .to_string();
    let (code, body) = crate::harness::call(
        &router,
        "UpdateStack",
        &[
            member("StackName", stack),
            member("TemplateBody", &revised),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case \"an update\": UpdateStack must be accepted, got {code} {body}"
    );
    let settled = settled_stack_status("an update", &router, REGION, stack).await;
    assert_eq!(
        settled,
        status::UPDATE_ROLLBACK_COMPLETE,
        "case \"an update\": expected the update to roll back, got {settled:?}"
    );
    let reason =
        resource_event_reason(&router, stack, TAGGED, resource_status::UPDATE_FAILED).await;
    assert_eq!(
        reason, RESERVED_TAG_KEY,
        "case \"an update\": expected this service's own reserved-prefix sentence, got \
         {reason:?}"
    );

    let (status_code, read) = ssm(
        &router,
        "GetParameter",
        &json!({"Name": tagged_name(stack)}),
    )
    .await;
    assert_eq!(
        status_code, 200,
        "case \"an update\": the parameter must still be there, got {status_code} {read}"
    );
    assert_eq!(
        read["Parameter"]["Value"].as_str(),
        Some("v"),
        "case \"an update\": expected the refused update to leave the value alone, got {read}"
    );
    let held = parameter_tags(&router, "an update", &tagged_name(stack)).await;
    assert_eq!(
        held.get("team").map(String::as_str),
        Some("platform"),
        "case \"an update\": expected the tags the create left to stand, got {held:?}"
    );
}
