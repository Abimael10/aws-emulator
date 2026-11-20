// SPDX-License-Identifier: CC0-1.0

//! What a CloudFormation stack DECLARES about an `AWS::ECR::Repository` is what Amazon ECR
//! holds for that repository — after the create, and after every deployment that states it
//! differently.
//!
//! The rule under test: **a setting a resource's declaration carries that the owning service
//! writes through a call of its own is applied to the resource the stack has, through that
//! call — when the stack CREATES the resource and again when a later deployment states the
//! setting differently; a setting the deployment stops declaring goes back to the owning
//! service's own documented default, or off the resource where the service lets it carry
//! none.** A stack that completes without making those calls hands a client a resource whose
//! configuration is not the one the template declares: the read a post-deploy step makes
//! answers that the repository has no policy at all
//! (`GetLifecyclePolicy`/`LifecyclePolicyNotFoundException`), and the retention, the access
//! grant, the tag-mutability rule and the scan-on-push setting the template asked for are
//! silently absent.
//!
//! Amazon ECR publishes one call per setting of a repository that is not a member of
//! `CreateRepository` — `PutLifecyclePolicy`/`DeleteLifecyclePolicy`,
//! `SetRepositoryPolicy`/`DeleteRepositoryPolicy`, `PutImageTagMutability`,
//! `PutImageScanningConfiguration`, `TagResource`/`UntagResource` — so those are the calls a
//! deployment of this type makes. Two crates have to agree for one of these templates to
//! deploy: CloudFormation makes the calls and Amazon ECR serves them and answers the reads.
//! The CREATE half of that is also graded inside the CloudFormation crate
//! (`tests/all/provisioned_ecr_repository.rs`, which mounts Amazon ECR beside it); what lives
//! here is the rule read over the WHOLE life of a declared repository — create, then a second
//! deployment that re-states each setting — on the assembled emulator the e2e client that
//! filed this behaviour talks to, where the registry, the container service and the stack are
//! one router.
//!
//! The rows differ along what the rule generalises over: WHICH setting the deployment states
//! (the lifecycle policy, the resource-based policy, the tag-mutability setting, the scanning
//! configuration, the tags), and HOW it states it — declared at the create and left alone,
//! replaced by an update, added by an update to a repository that had none, and dropped by an
//! update — plus the spellings a template may carry a policy document in (a document, its
//! JSON text, under an explicit `RegistryId`, in JSON and in YAML).

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery CloudFormation caller, the poll that waits for a stack to FIRST report a
// status, the Amazon ECR caller with the three-service router behind it, and the XML readers
// are each written ONCE in this test crate: a suite that copies one drifts from the others.
use crate::cloudformation_apigatewayv2_resources::cloudformation;
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::cloudformation_registry_backed_task_definition::{build_router, ecr};
use crate::query_wire::{elements, physical_ids_of};

/// The `StackStatus` values the rule is stated in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// The logical id every template below declares its repository under.
const REGISTRY_LOGICAL_ID: &str = "Registry";

/// The error codes Amazon ECR answers for a repository that carries no such policy, which is
/// what a repository the declaration asks none for must answer.
const LIFECYCLE_POLICY_NOT_FOUND: &str = "LifecyclePolicyNotFoundException";
const REPOSITORY_POLICY_NOT_FOUND: &str = "RepositoryPolicyNotFoundException";

/// The registry this emulator serves, which is the account every repository below belongs to.
const ACCOUNT_ID: &str = "000000000000";

/// The tag keys CloudFormation records on every resource it provisions, which no declaration
/// states and which therefore no row asserts over.
const CLOUDFORMATION_TAG_PREFIX: &str = "aws:cloudformation:";

/// One rule of a lifecycle policy document, as that format spells one: the members every rule
/// carries, with `selection` taking whichever count the rule counts by.
///
/// Source: the two forms of a `selection` the lifecycle policy format publishes — an
/// `imageCountMoreThan` keeping a number of images and a `sinceImagePushed` counting in days
/// — as Amazon ECR's own policy member documents them
/// (`smithy_query(query="doc",shape="LifecyclePolicyText")`).
fn lifecycle_rule(priority: i64, description: &str, selection: Value) -> Value {
    json!({
        "rulePriority": priority,
        "description": description,
        "selection": selection,
        "action": {"type": "expire"},
    })
}

/// A policy document holding `rules`.
fn lifecycle_policy(rules: Vec<Value>) -> Value {
    json!({ "rules": rules })
}

/// A rule that expires untagged images after `days` days.
fn expire_untagged(priority: i64, days: i64) -> Value {
    lifecycle_rule(
        priority,
        &format!("Expire untagged images after {days} days"),
        json!({
            "tagStatus": "untagged",
            "countType": "sinceImagePushed",
            "countUnit": "days",
            "countNumber": days,
        }),
    )
}

/// A rule that keeps the last `count` images carrying the `release` tag prefix.
fn keep_released(priority: i64, count: i64) -> Value {
    lifecycle_rule(
        priority,
        &format!("Keep the last {count} release images"),
        json!({
            "tagStatus": "tagged",
            "tagPrefixList": ["release"],
            "countType": "imageCountMoreThan",
            "countNumber": count,
        }),
    )
}

/// A repository policy document whose one statement carries `sid`.
fn repository_policy(sid: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": sid,
            "Effect": "Allow",
            "Principal": {"AWS": format!("arn:aws:iam::{ACCOUNT_ID}:root")},
            "Action": ["ecr:BatchGetImage", "ecr:GetDownloadUrlForLayer"],
        }],
    })
}

/// A template declaring one `AWS::ECR::Repository` with `properties`.
fn template(properties: Value) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            REGISTRY_LOGICAL_ID: {
                "Type": "AWS::ECR::Repository",
                "Properties": properties,
            }
        }
    })
    .to_string()
}

/// Deploy `template` on `stack` through `action` and read the stack until it FIRST reports
/// `wanted`, which it must reach.
async fn deploy(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    stack: &str,
    template: &str,
    wanted: &str,
) {
    let (status, body) = cloudformation(
        router,
        action,
        &[("StackName", stack), ("TemplateBody", template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    let (reached, described) = wait_for_stack_status(router, stack, wanted).await;
    let reasons = if reached == wanted {
        Vec::new()
    } else {
        let (_, events) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        reached, wanted,
        "case {case:?}: expected {action} on {stack:?} to reach {wanted}, got {described} with \
         resource failures {reasons:?}"
    );
}

/// The physical resource id the stack reports for its repository — the name a client calls
/// Amazon ECR with.
async fn repository_name_of(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let (status, body) =
        cloudformation(router, "DescribeStackResources", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with body \
         {body}"
    );
    physical_ids_of(&body)
        .remove(REGISTRY_LOGICAL_ID)
        .unwrap_or_else(|| panic!("case {case:?}: no physical id for the repository in {body}"))
}

/// The lifecycle policy document Amazon ECR holds for `name`, or the error code it answers
/// when the repository carries none.
async fn held_lifecycle_policy(router: &MultiServiceRouter, name: &str) -> Result<Value, String> {
    let (status, body) = ecr(
        router,
        "GetLifecyclePolicy",
        &json!({"repositoryName": name}),
    )
    .await;
    if status == 200 {
        let text = body["lifecyclePolicyText"].as_str().unwrap_or_default();
        return Ok(serde_json::from_str(text).unwrap_or(Value::Null));
    }
    Err(error_code_of(&body))
}

/// The repository policy document Amazon ECR holds for `name`, or the error code it answers
/// when the repository carries none.
async fn held_repository_policy(router: &MultiServiceRouter, name: &str) -> Result<Value, String> {
    let (status, body) = ecr(
        router,
        "GetRepositoryPolicy",
        &json!({"repositoryName": name}),
    )
    .await;
    if status == 200 {
        let text = body["policyText"].as_str().unwrap_or_default();
        return Ok(serde_json::from_str(text).unwrap_or(Value::Null));
    }
    Err(error_code_of(&body))
}

/// The short name of the error an awsJson answer carries.
fn error_code_of(body: &Value) -> String {
    body["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The repository Amazon ECR holds under `name`, as its own read reports it.
async fn held_repository(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let (status, body) = ecr(
        router,
        "DescribeRepositories",
        &json!({"repositoryNames": [name]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected Amazon ECR to hold the repository {name:?}, got {status} with \
         {body}"
    );
    body["repositories"][0].clone()
}

/// The tags Amazon ECR holds for the repository `arn` names, by key.
async fn held_tags(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
) -> BTreeMap<String, String> {
    let (status, body) = ecr(router, "ListTagsForResource", &json!({"resourceArn": arn})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListTagsForResource on {arn:?}, got {status} with \
         {body}"
    );
    body["tags"]
        .as_array()
        .map(|listed| {
            listed
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

/// One row of the lifecycle table: the two templates a deployment pair carries, and the policy
/// the repository must hold after each of them — `None` where it must hold none.
struct LifecycleRow {
    case: &'static str,
    stack: &'static str,
    create: String,
    update: String,
    after_create: Option<Value>,
    after_update: Option<Value>,
}

fn lifecycle_rows() -> Vec<LifecycleRow> {
    let one_rule = lifecycle_policy(vec![expire_untagged(1, 14)]);
    let two_rules = lifecycle_policy(vec![expire_untagged(1, 30), keep_released(2, 10)]);
    vec![
        LifecycleRow {
            // The recorded case, and then an update that changes something ELSE in place: the
            // policy the create put on the repository is still the one it holds.
            case: "a policy declared at the create, with an unrelated in-place change after it",
            stack: "ecr-kept-lifecycle",
            create: template(json!({
                "RepositoryName": "cfn-kept-lifecycle",
                "LifecyclePolicy": {"LifecyclePolicyText": one_rule.clone()},
            })),
            update: template(json!({
                "RepositoryName": "cfn-kept-lifecycle",
                "LifecyclePolicy": {"LifecyclePolicyText": one_rule.clone()},
                "Tags": [{"Key": "owner", "Value": "platform"}],
            })),
            after_create: Some(one_rule.clone()),
            after_update: Some(one_rule.clone()),
        },
        LifecycleRow {
            case: "the policy document replaced by the update",
            stack: "ecr-replaced-lifecycle",
            create: template(json!({
                "RepositoryName": "cfn-replaced-lifecycle",
                "LifecyclePolicy": {"LifecyclePolicyText": one_rule.clone()},
            })),
            update: template(json!({
                "RepositoryName": "cfn-replaced-lifecycle",
                "LifecyclePolicy": {"LifecyclePolicyText": two_rules.clone()},
            })),
            after_create: Some(one_rule.clone()),
            after_update: Some(two_rules.clone()),
        },
        LifecycleRow {
            case: "a policy added by the update to a repository created without one",
            stack: "ecr-added-lifecycle",
            create: template(json!({"RepositoryName": "cfn-added-lifecycle"})),
            update: template(json!({
                "RepositoryName": "cfn-added-lifecycle",
                "LifecyclePolicy": {"LifecyclePolicyText": two_rules.clone()},
            })),
            after_create: None,
            after_update: Some(two_rules.clone()),
        },
        LifecycleRow {
            case: "the policy dropped by the update",
            stack: "ecr-dropped-lifecycle",
            create: template(json!({
                "RepositoryName": "cfn-dropped-lifecycle",
                "LifecyclePolicy": {"LifecyclePolicyText": two_rules.clone()},
            })),
            update: template(json!({"RepositoryName": "cfn-dropped-lifecycle"})),
            after_create: Some(two_rules.clone()),
            after_update: None,
        },
        LifecycleRow {
            // The policy written as its JSON TEXT rather than as a document, under the
            // registry the repository belongs to — the other two spellings the property takes.
            case: "the policy written as JSON text under an explicit registry, and replaced",
            stack: "ecr-text-lifecycle",
            create: template(json!({
                "RepositoryName": "cfn-text-lifecycle",
                "LifecyclePolicy": {
                    "LifecyclePolicyText": one_rule.to_string(),
                    "RegistryId": ACCOUNT_ID,
                },
            })),
            update: template(json!({
                "RepositoryName": "cfn-text-lifecycle",
                "LifecyclePolicy": {
                    "LifecyclePolicyText": two_rules.to_string(),
                    "RegistryId": ACCOUNT_ID,
                },
            })),
            after_create: Some(one_rule.clone()),
            after_update: Some(two_rules.clone()),
        },
        LifecycleRow {
            // The same pair of deployments in YAML, which is the other spelling a template
            // reaches CloudFormation in.
            case: "the policy declared in YAML, and replaced in YAML",
            stack: "ecr-yaml-lifecycle",
            create: format!(
                concat!(
                    "Resources:\n",
                    "  Registry:\n",
                    "    Type: AWS::ECR::Repository\n",
                    "    Properties:\n",
                    "      RepositoryName: cfn-yaml-lifecycle\n",
                    "      LifecyclePolicy:\n",
                    "        LifecyclePolicyText: '{}'\n",
                ),
                one_rule
            ),
            update: format!(
                concat!(
                    "Resources:\n",
                    "  Registry:\n",
                    "    Type: AWS::ECR::Repository\n",
                    "    Properties:\n",
                    "      RepositoryName: cfn-yaml-lifecycle\n",
                    "      LifecyclePolicy:\n",
                    "        LifecyclePolicyText: '{}'\n",
                ),
                two_rules
            ),
            after_create: Some(one_rule),
            after_update: Some(two_rules),
        },
    ]
}

/// RULE: the lifecycle policy a declaration carries is on the repository Amazon ECR holds,
/// after the create and after every deployment that states it differently — and a deployment
/// that stops declaring one takes it off the repository rather than leaving the old one in
/// force.
#[tokio::test]
async fn a_declared_lifecycle_policy_is_what_amazon_ecr_holds_after_each_deployment() {
    for row in lifecycle_rows() {
        let router = build_router();
        let case = row.case;
        deploy(
            &router,
            case,
            "CreateStack",
            row.stack,
            &row.create,
            CREATE_COMPLETE,
        )
        .await;
        let name = repository_name_of(&router, case, row.stack).await;

        let held = held_lifecycle_policy(&router, &name).await;
        match &row.after_create {
            Some(policy) => assert_eq!(
                held.as_ref().ok(),
                Some(policy),
                "case {case:?}: expected the created repository {name:?} to hold the declared \
                 lifecycle policy, got {held:?}"
            ),
            None => assert_eq!(
                held.as_ref().err().map(String::as_str),
                Some(LIFECYCLE_POLICY_NOT_FOUND),
                "case {case:?}: expected the created repository {name:?} to hold no lifecycle \
                 policy, got {held:?}"
            ),
        }

        deploy(
            &router,
            case,
            "UpdateStack",
            row.stack,
            &row.update,
            UPDATE_COMPLETE,
        )
        .await;
        // An in-place change keeps the repository's identity, so the policy is read back under
        // the same name the create published.
        let updated_name = repository_name_of(&router, case, row.stack).await;
        assert_eq!(
            updated_name, name,
            "case {case:?}: expected the update to keep the repository's name {name:?}, got \
             {updated_name:?}"
        );
        let held = held_lifecycle_policy(&router, &updated_name).await;
        match &row.after_update {
            Some(policy) => assert_eq!(
                held.as_ref().ok(),
                Some(policy),
                "case {case:?}: expected the updated repository {updated_name:?} to hold the \
                 lifecycle policy the new template declares, got {held:?}"
            ),
            None => assert_eq!(
                held.as_ref().err().map(String::as_str),
                Some(LIFECYCLE_POLICY_NOT_FOUND),
                "case {case:?}: expected the updated repository {updated_name:?} to hold no \
                 lifecycle policy, got {held:?}"
            ),
        }
    }
}

/// What Amazon ECR must report for a repository at one point of a row: the `Sid` of the
/// resource-based policy it holds (`None` where it holds none), its tag-mutability setting,
/// whether it scans on push, and the tags the declaration gives it.
struct Expected {
    policy_sid: Option<&'static str>,
    tag_mutability: &'static str,
    scan_on_push: bool,
    tags: &'static [(&'static str, &'static str)],
}

/// One row of the settings table: the two templates a deployment pair carries, and what the
/// repository must report after each of them.
struct SettingRow {
    case: &'static str,
    stack: &'static str,
    create: String,
    update: String,
    after_create: Expected,
    after_update: Expected,
}

fn setting_rows() -> Vec<SettingRow> {
    vec![
        SettingRow {
            case: "the resource-based policy replaced by the update",
            stack: "ecr-replaced-policy",
            create: template(json!({
                "RepositoryName": "cfn-replaced-policy",
                "RepositoryPolicyText": repository_policy("FirstGrant"),
            })),
            update: template(json!({
                "RepositoryName": "cfn-replaced-policy",
                "RepositoryPolicyText": repository_policy("SecondGrant"),
            })),
            after_create: Expected {
                policy_sid: Some("FirstGrant"),
                tag_mutability: "MUTABLE",
                scan_on_push: false,
                tags: &[],
            },
            after_update: Expected {
                policy_sid: Some("SecondGrant"),
                tag_mutability: "MUTABLE",
                scan_on_push: false,
                tags: &[],
            },
        },
        SettingRow {
            case: "the resource-based policy dropped by the update",
            stack: "ecr-dropped-policy",
            create: template(json!({
                "RepositoryName": "cfn-dropped-policy",
                "RepositoryPolicyText": repository_policy("OnlyGrant"),
            })),
            update: template(json!({"RepositoryName": "cfn-dropped-policy"})),
            after_create: Expected {
                policy_sid: Some("OnlyGrant"),
                tag_mutability: "MUTABLE",
                scan_on_push: false,
                tags: &[],
            },
            after_update: Expected {
                policy_sid: None,
                tag_mutability: "MUTABLE",
                scan_on_push: false,
                tags: &[],
            },
        },
        SettingRow {
            case: "the tag-mutability setting changed by the update",
            stack: "ecr-tag-mutability",
            create: template(json!({
                "RepositoryName": "cfn-tag-mutability",
                "ImageTagMutability": "MUTABLE",
            })),
            update: template(json!({
                "RepositoryName": "cfn-tag-mutability",
                "ImageTagMutability": "IMMUTABLE",
            })),
            after_create: Expected {
                policy_sid: None,
                tag_mutability: "MUTABLE",
                scan_on_push: false,
                tags: &[],
            },
            after_update: Expected {
                policy_sid: None,
                tag_mutability: "IMMUTABLE",
                scan_on_push: false,
                tags: &[],
            },
        },
        SettingRow {
            // The property dropped: the repository goes back to the setting Amazon ECR
            // documents for a repository created without one ("if this parameter is omitted,
            // the default setting of MUTABLE will be used").
            case: "the tag-mutability setting dropped by the update",
            stack: "ecr-dropped-mutability",
            create: template(json!({
                "RepositoryName": "cfn-dropped-mutability",
                "ImageTagMutability": "IMMUTABLE",
            })),
            update: template(json!({"RepositoryName": "cfn-dropped-mutability"})),
            after_create: Expected {
                policy_sid: None,
                tag_mutability: "IMMUTABLE",
                scan_on_push: false,
                tags: &[],
            },
            after_update: Expected {
                policy_sid: None,
                tag_mutability: "MUTABLE",
                scan_on_push: false,
                tags: &[],
            },
        },
        SettingRow {
            case: "the scanning configuration turned on by the update",
            stack: "ecr-scan-on-push",
            create: template(json!({
                "RepositoryName": "cfn-scan-on-push",
                "ImageScanningConfiguration": {"ScanOnPush": false},
            })),
            update: template(json!({
                "RepositoryName": "cfn-scan-on-push",
                "ImageScanningConfiguration": {"ScanOnPush": true},
            })),
            after_create: Expected {
                policy_sid: None,
                tag_mutability: "MUTABLE",
                scan_on_push: false,
                tags: &[],
            },
            after_update: Expected {
                policy_sid: None,
                tag_mutability: "MUTABLE",
                scan_on_push: true,
                tags: &[],
            },
        },
        SettingRow {
            // The document dropped: back to the registry's own default ("if this parameter is
            // not specified, it will default to false and images will not be scanned").
            case: "the scanning configuration dropped by the update",
            stack: "ecr-dropped-scanning",
            create: template(json!({
                "RepositoryName": "cfn-dropped-scanning",
                "ImageScanningConfiguration": {"ScanOnPush": true},
            })),
            update: template(json!({"RepositoryName": "cfn-dropped-scanning"})),
            after_create: Expected {
                policy_sid: None,
                tag_mutability: "MUTABLE",
                scan_on_push: true,
                tags: &[],
            },
            after_update: Expected {
                policy_sid: None,
                tag_mutability: "MUTABLE",
                scan_on_push: false,
                tags: &[],
            },
        },
        SettingRow {
            // One tag re-valued, one added and one dropped, with every other setting the
            // declaration carries left alone.
            case: "the tags re-stated by the update, beside the settings it keeps",
            stack: "ecr-retagged",
            create: template(json!({
                "RepositoryName": "cfn-retagged",
                "ImageTagMutability": "IMMUTABLE",
                "ImageScanningConfiguration": {"ScanOnPush": true},
                "RepositoryPolicyText": repository_policy("KeptGrant"),
                "Tags": [
                    {"Key": "owner", "Value": "platform"},
                    {"Key": "tier", "Value": "bronze"},
                ],
            })),
            update: template(json!({
                "RepositoryName": "cfn-retagged",
                "ImageTagMutability": "IMMUTABLE",
                "ImageScanningConfiguration": {"ScanOnPush": true},
                "RepositoryPolicyText": repository_policy("KeptGrant"),
                "Tags": [
                    {"Key": "owner", "Value": "registry"},
                    {"Key": "stage", "Value": "live"},
                ],
            })),
            after_create: Expected {
                policy_sid: Some("KeptGrant"),
                tag_mutability: "IMMUTABLE",
                scan_on_push: true,
                tags: &[("owner", "platform"), ("tier", "bronze")],
            },
            after_update: Expected {
                policy_sid: Some("KeptGrant"),
                tag_mutability: "IMMUTABLE",
                scan_on_push: true,
                tags: &[("owner", "registry"), ("stage", "live")],
            },
        },
    ]
}

/// Check every dimension one row states about the repository `name`, at one point of that row.
async fn assert_settings(
    router: &MultiServiceRouter,
    case: &str,
    when: &str,
    name: &str,
    expected: &Expected,
) {
    let held_policy = held_repository_policy(router, name).await;
    match expected.policy_sid {
        Some(sid) => assert_eq!(
            held_policy
                .as_ref()
                .ok()
                .and_then(|document| document["Statement"][0]["Sid"].as_str()),
            Some(sid),
            "case {case:?}: expected the repository policy declared {when} (Sid {sid:?}) on \
             {name:?}, got {held_policy:?}"
        ),
        None => assert_eq!(
            held_policy.as_ref().err().map(String::as_str),
            Some(REPOSITORY_POLICY_NOT_FOUND),
            "case {case:?}: expected no repository policy on {name:?} {when}, got {held_policy:?}"
        ),
    }

    let repository = held_repository(router, case, name).await;
    assert_eq!(
        repository["imageTagMutability"].as_str(),
        Some(expected.tag_mutability),
        "case {case:?}: expected the tag mutability declared {when} on {name:?} to be {:?}, got \
         {repository}",
        expected.tag_mutability
    );
    assert_eq!(
        repository["imageScanningConfiguration"]["scanOnPush"].as_bool(),
        Some(expected.scan_on_push),
        "case {case:?}: expected scanOnPush {when} on {name:?} to be {}, got {repository}",
        expected.scan_on_push
    );

    let arn = repository["repositoryArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no ARN for {name:?} in {repository}"))
        .to_string();
    let held = held_tags(router, case, &arn).await;
    for (key, value) in expected.tags {
        assert_eq!(
            held.get(*key).map(String::as_str),
            Some(*value),
            "case {case:?}: expected the tag {key:?} declared {when} on {name:?} to be \
             {value:?}, got {held:?}"
        );
    }
    // A key the declaration no longer states is off the repository; the tags CloudFormation
    // records itself are no declaration's to state, so they are not asserted over here.
    for key in held.keys() {
        assert!(
            key.starts_with(CLOUDFORMATION_TAG_PREFIX)
                || expected.tags.iter().any(|(stated, _)| stated == key),
            "case {case:?}: the repository {name:?} still carries the tag {key:?} {when}, which \
             the declaration no longer states; held {held:?}"
        );
    }
}

/// RULE, read over the other settings of the same type: each one a declaration states reaches
/// the repository through the Amazon ECR call that writes it — on the create and again on a
/// deployment that states it differently — and one the deployment stops declaring goes back to
/// that registry's own documented default, or off the repository where it may carry none.
#[tokio::test]
async fn every_declared_repository_setting_is_what_amazon_ecr_reports_after_each_deployment() {
    for row in setting_rows() {
        let router = build_router();
        let case = row.case;
        deploy(
            &router,
            case,
            "CreateStack",
            row.stack,
            &row.create,
            CREATE_COMPLETE,
        )
        .await;
        let name = repository_name_of(&router, case, row.stack).await;
        assert_settings(&router, case, "at the create", &name, &row.after_create).await;

        deploy(
            &router,
            case,
            "UpdateStack",
            row.stack,
            &row.update,
            UPDATE_COMPLETE,
        )
        .await;
        let updated_name = repository_name_of(&router, case, row.stack).await;
        assert_eq!(
            updated_name, name,
            "case {case:?}: expected the update to keep the repository's name {name:?}, got \
             {updated_name:?}"
        );
        assert_settings(
            &router,
            case,
            "after the update",
            &updated_name,
            &row.after_update,
        )
        .await;
    }
}
