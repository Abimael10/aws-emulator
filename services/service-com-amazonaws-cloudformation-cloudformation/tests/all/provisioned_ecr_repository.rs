// SPDX-License-Identifier: CC0-1.0

//! An `AWS::ECR::Repository` a stack declares is CREATED in Amazon ECR itself.
//!
//! The rule under test: **a stack operation that deploys a template calls the owning
//! service's own create API once per resource the `Resources` section declares**, applied
//! to the private container registry — so once the deployment completes the repository
//! EXISTS under the physical resource id the stack reports, carries the settings the
//! declaration asked for, and is deleted again with the stack. A repository Amazon ECR
//! refuses is not created, so the deployment fails instead of completing.
//!
//! The rows differ along what that rule generalises over: a repository the template NAMES
//! and one whose name CloudFormation generated, a declaration carrying every settings
//! document the type publishes and one carrying none, both template spellings (JSON and
//! YAML), and the two verdicts Amazon ECR can give.
//!
//! Everything goes over the wire each service speaks — awsQuery for CloudFormation,
//! awsJson1.1 for Amazon ECR — through the one router `harness.rs` builds.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::settled_stack_status;
use crate::stack_lifecycle::{
    call, create_stack, describe_stack, member, resource_event_reason, router,
    wait_for_the_transition, xml_text, ACCOUNT_ID, REGION,
};
use service_com_amazonaws_cloudformation_cloudformation::state::resource_status;

/// The `X-Amz-Target` prefix Amazon ECR's model fixes (service shape
/// `AmazonEC2ContainerRegistry_V20150921`).
const ECR_TARGET_PREFIX: &str = "AmazonEC2ContainerRegistry_V20150921";

/// One Amazon ECR request, over the awsJson1.1 wire it speaks.
async fn ecr(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/ecr/aws4_request, \
         SignedHeaders=host, Signature=0123456789abcdef"
    );
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://api.ecr.{REGION}.amazonaws.com/"))
        .header("host", format!("api.ecr.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{ECR_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization)
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The repository Amazon ECR holds under `name`, or `None` when it holds none.
async fn repository(router: &MultiServiceRouter, name: &str) -> Option<Value> {
    let (status, body) = ecr(
        router,
        "DescribeRepositories",
        &json!({"repositoryNames": [name]}),
    )
    .await;
    if status != 200 {
        return None;
    }
    body["repositories"]
        .as_array()
        .and_then(|listed| listed.first())
        .cloned()
}

/// Deploy `template` as stack `name` and answer the status and reason it settles with.
async fn deploy(
    router: &MultiServiceRouter,
    name: &str,
    template: &str,
) -> (Option<String>, Option<String>) {
    let (status, body) = create_stack(router, name, template, &[]).await;
    assert_eq!(status, 200, "CreateStack({name}) answered {status}: {body}");
    wait_for_the_transition().await;
    let (status, body) = describe_stack(router, name).await;
    assert_eq!(
        status, 200,
        "DescribeStacks({name}) answered {status}: {body}"
    );
    (
        xml_text(&body, "StackStatus"),
        xml_text(&body, "StackStatusReason"),
    )
}

/// The physical resource id the stack reports for one logical id.
async fn physical_id(router: &MultiServiceRouter, stack: &str, logical_id: &str) -> String {
    let (status, body) = call(
        router,
        "DescribeStackResources",
        &[
            member("StackName", stack),
            member("LogicalResourceId", logical_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeStackResources({stack}, {logical_id}) answered {status}: {body}"
    );
    xml_text(&body, "PhysicalResourceId")
        .unwrap_or_else(|| panic!("case {logical_id:?}: expected a physical id, got {body}"))
}

/// RULE: a declared repository exists in Amazon ECR under the physical id the stack
/// reports, with the settings the declaration asked for.
#[tokio::test]
async fn a_declared_repository_exists_in_ecr_with_its_settings() {
    // (case, the template, the logical id, the name the stack must report, the settings
    // the repository must carry)
    let rows: Vec<(&str, String, &str, Option<&str>, Value)> = vec![
        (
            "json, a named repository carrying every settings document the type publishes",
            json!({
                "Resources": {"Registry": {
                    "Type": "AWS::ECR::Repository",
                    "Properties": {
                        "RepositoryName": "cfn-provisioned-images",
                        "ImageTagMutability": "IMMUTABLE",
                        "ImageScanningConfiguration": {"ScanOnPush": true},
                        "EncryptionConfiguration": {"EncryptionType": "AES256"},
                    },
                }},
            })
            .to_string(),
            "Registry",
            Some("cfn-provisioned-images"),
            json!({
                "imageTagMutability": "IMMUTABLE",
                "imageScanningConfiguration": {"scanOnPush": true},
                "encryptionConfiguration": {"encryptionType": "AES256"},
            }),
        ),
        (
            "json, a repository declaring nothing but its name",
            json!({
                "Resources": {"Registry": {
                    "Type": "AWS::ECR::Repository",
                    "Properties": {"RepositoryName": "cfn-bare-images"},
                }},
            })
            .to_string(),
            "Registry",
            Some("cfn-bare-images"),
            // The defaults Amazon ECR documents for a repository created with neither
            // setting: "If this parameter is omitted, the default setting of MUTABLE will
            // be used", and AES256 encryption "when no encryption configuration is set".
            json!({
                "imageTagMutability": "MUTABLE",
                "encryptionConfiguration": {"encryptionType": "AES256"},
            }),
        ),
        (
            "yaml, a repository whose name CloudFormation generated",
            concat!(
                "Resources:\n",
                "  Registry:\n",
                "    Type: AWS::ECR::Repository\n",
                "    Properties:\n",
                "      ImageTagMutability: MUTABLE\n",
            )
            .to_string(),
            "Registry",
            // CloudFormation generates the name, so the row asserts it is the one the
            // stack reports rather than a value fixed here.
            None,
            json!({"imageTagMutability": "MUTABLE"}),
        ),
        (
            "yaml, a repository whose name the template declares, with its URI an output",
            concat!(
                "Resources:\n",
                "  Registry:\n",
                "    Type: AWS::ECR::Repository\n",
                "    Properties:\n",
                "      RepositoryName: warehouse-images\n",
                "Outputs:\n",
                "  RepositoryUri:\n",
                "    Value: !GetAtt Registry.RepositoryUri\n",
            )
            .to_string(),
            "Registry",
            Some("warehouse-images"),
            json!({"imageTagMutability": "MUTABLE"}),
        ),
    ];

    for (index, (case, template, logical_id, expected_name, settings)) in rows.iter().enumerate() {
        let router = router();
        let stack = format!("ecr-stack-{index}");
        let (status, reason) = deploy(&router, &stack, template).await;
        assert_eq!(
            status.as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the create to complete, got {status:?} ({reason:?})"
        );
        let name = physical_id(&router, &stack, logical_id).await;
        if let Some(expected) = expected_name {
            assert_eq!(
                name, *expected,
                "case {case:?}: expected the declared name as the physical id, got {name:?}"
            );
        }
        let held = repository(&router, &name)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: expected ECR to hold {name:?}"));
        assert_eq!(
            held["repositoryName"], json!(name),
            "case {case:?}: expected the repository to be keyed by the physical id, got {held}"
        );
        assert_eq!(
            held["repositoryArn"],
            json!(format!(
                "arn:aws:ecr:{REGION}:{ACCOUNT_ID}:repository/{name}"
            )),
            "case {case:?}: unexpected ARN on the created repository, got {held}"
        );
        for (member, expected) in settings.as_object().expect("the settings are an object") {
            assert_eq!(
                held[member], *expected,
                "case {case:?}: expected {member} to be {expected}, got {held}"
            );
        }
    }
}

/// RULE: the resource-based policy a repository's declaration states is set on the
/// repository the stack created, so a client reads it back from Amazon ECR.
#[tokio::test]
async fn a_declared_repository_policy_is_set_on_the_created_repository() {
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowPull",
            "Effect": "Allow",
            "Principal": {"AWS": format!("arn:aws:iam::{ACCOUNT_ID}:root")},
            "Action": "ecr:BatchGetImage",
        }],
    });
    let template = json!({
        "Resources": {"Registry": {
            "Type": "AWS::ECR::Repository",
            "Properties": {
                "RepositoryName": "cfn-policy-images",
                "RepositoryPolicyText": policy,
            },
        }},
    })
    .to_string();

    let router = router();
    let (status, reason) = deploy(&router, "ecr-policy-stack", &template).await;
    assert_eq!(
        status.as_deref(),
        Some("CREATE_COMPLETE"),
        "expected the create to complete, got {status:?} ({reason:?})"
    );
    let (code, body) = ecr(
        &router,
        "GetRepositoryPolicy",
        &json!({"repositoryName": "cfn-policy-images"}),
    )
    .await;
    assert_eq!(
        code, 200,
        "expected the repository to carry a policy, got {code}: {body}"
    );
    let text = body["policyText"].as_str().unwrap_or_default();
    let read: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    assert_eq!(
        read, policy,
        "expected the declared policy document on the repository, got {text:?}"
    );
}

/// RULE: a repository Amazon ECR REFUSES is not created, so the deployment fails and the
/// failing resource's event carries that service's own refusal.
#[tokio::test]
async fn a_repository_ecr_refuses_fails_the_deployment() {
    // Amazon ECR's `RepositoryName` is lower case only
    // (`^[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*(\/[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*)*$`), so a
    // declaration naming one with capitals is a repository that service will not create.
    let template = json!({
        "Resources": {"Registry": {
            "Type": "AWS::ECR::Repository",
            "Properties": {"RepositoryName": "CfnRefusedImages"},
        }},
    })
    .to_string();

    let router = router();
    let (status, reason) = deploy(&router, "ecr-refused-stack", &template).await;
    assert_eq!(
        status.as_deref(),
        Some("ROLLBACK_COMPLETE"),
        "expected the failed create to be rolled back, got {status:?} ({reason:?})"
    );
    let on_the_resource = resource_event_reason(
        &router,
        "ecr-refused-stack",
        "Registry",
        resource_status::CREATE_FAILED,
    )
    .await;
    assert!(
        on_the_resource.contains("HandlerErrorCode: InvalidRequest"),
        "expected the owning service's refusal on the failed resource's event, got \
         {on_the_resource:?}"
    );
    assert!(
        repository(&router, "CfnRefusedImages").await.is_none(),
        "a refused repository must not exist in Amazon ECR"
    );
}

/// RULE: a repository whose declared policy grants to another resource of the SAME stack
/// is created after that resource, so the grant names an identity Amazon ECR can resolve
/// and the policy is stored.
#[tokio::test]
async fn a_policy_granting_to_a_role_the_stack_creates_is_set_on_the_repository() {
    let template = json!({
        "Resources": {
            "Role": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": {"Service": "codebuild.amazonaws.com"},
                            "Action": "sts:AssumeRole",
                        }],
                    },
                },
            },
            "Registry": {
                "Type": "AWS::ECR::Repository",
                "Properties": {
                    "RepositoryName": "cfn-granted-images",
                    "RepositoryPolicyText": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": {"AWS": {"Fn::GetAtt": ["Role", "Arn"]}},
                            "Action": "ecr:BatchGetImage",
                        }],
                    },
                },
            },
        },
    })
    .to_string();

    let router = router();
    let (status, body) = create_stack(
        &router,
        "ecr-grant-stack",
        &template,
        &crate::stack_lifecycle::acknowledging_iam(&[]),
    )
    .await;
    assert_eq!(status, 200, "CreateStack answered {status}: {body}");
    wait_for_the_transition().await;
    let (_, stack) = describe_stack(&router, "ecr-grant-stack").await;
    assert_eq!(
        xml_text(&stack, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "expected the create to complete, got {stack}"
    );
    let role_arn = format!(
        "arn:aws:iam::{ACCOUNT_ID}:role/{}",
        physical_id(&router, "ecr-grant-stack", "Role").await
    );
    let (code, body) = ecr(
        &router,
        "GetRepositoryPolicy",
        &json!({"repositoryName": "cfn-granted-images"}),
    )
    .await;
    assert_eq!(
        code, 200,
        "expected the repository to carry a policy, got {code}: {body}"
    );
    let text = body["policyText"].as_str().unwrap_or_default();
    assert!(
        text.contains(&role_arn),
        "expected the policy to grant to {role_arn}, got {text:?}"
    );
}

/// Deploy `template` as stack `name` and answer the status it SETTLES on, through the
/// suite-wide waiter: what a case asserts is the CONDITION, never how long the emulator
/// took to reach it.
async fn deploy_until_settled(
    case: &str,
    router: &MultiServiceRouter,
    name: &str,
    template: &str,
) -> (String, Option<String>) {
    let (status, body) = create_stack(router, name, template, &[]).await;
    assert_eq!(status, 200, "CreateStack({name}) answered {status}: {body}");
    let settled = settled_stack_status(case, router, REGION, name).await;
    let (_, body) = describe_stack(router, name).await;
    (settled, xml_text(&body, "StackStatusReason"))
}

/// The lifecycle policy Amazon ECR holds for `name`, or the refusal code it answers with.
async fn lifecycle_policy(router: &MultiServiceRouter, name: &str) -> Result<Value, String> {
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
    Err(body["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string())
}

/// One rule of a lifecycle policy document, as the format spells one: the members every
/// rule carries, with `selection` taking whichever count the rule counts by.
///
/// Source: the two forms of a `selection` the lifecycle policy format publishes — an
/// `imageCountMoreThan` keeping a number of images, and a `sinceImagePushed` counting in
/// days — read off ECR's own policy parse
/// (`smithy_query(query="doc",shape="LifecyclePolicyText")` carries the member's own text).
fn lifecycle_rule(priority: i64, description: &str, selection: Value) -> Value {
    json!({
        "rulePriority": priority,
        "description": description,
        "selection": selection,
        "action": {"type": "expire"},
    })
}

/// RULE: a nested settings document a resource's declaration carries that the owning
/// service configures through a call of its own is applied to the resource the stack
/// created — for a repository's `LifecyclePolicy`, through `PutLifecyclePolicy` — so a
/// read of that configuration after the deployment answers what the template declared,
/// and a declaration carrying none leaves the resource with none.
#[tokio::test]
async fn a_declared_lifecycle_policy_is_put_on_the_created_repository() {
    // The rows differ along how a template spells the property — the document itself, the
    // JSON TEXT of one, the registry it names, and the YAML spelling — and along the rule
    // forms the policy format publishes, from one rule to several.
    let untagged = lifecycle_rule(
        1,
        "Expire untagged images after 14 days",
        json!({
            "tagStatus": "untagged",
            "countType": "sinceImagePushed",
            "countUnit": "days",
            "countNumber": 14,
        }),
    );
    let tagged = lifecycle_rule(
        2,
        "Keep the last 10 release images",
        json!({
            "tagStatus": "tagged",
            "tagPrefixList": ["release"],
            "countType": "imageCountMoreThan",
            "countNumber": 10,
        }),
    );
    let one_rule = json!({"rules": [untagged.clone()]});
    let two_rules = json!({"rules": [untagged, tagged]});

    // (case, the template, the repository's name where the template declares one, the
    // policy it must hold)
    let rows: Vec<(&str, String, Option<&str>, Option<Value>)> = vec![
        (
            "json, the policy written as the document itself",
            json!({
                "Resources": {"Registry": {
                    "Type": "AWS::ECR::Repository",
                    "Properties": {
                        "RepositoryName": "cfn-lifecycle-images",
                        "LifecyclePolicy": {"LifecyclePolicyText": one_rule.clone()},
                    },
                }},
            })
            .to_string(),
            "cfn-lifecycle-images".into(),
            Some(one_rule.clone()),
        ),
        (
            "json, the policy written as JSON text, under the registry it names",
            json!({
                "Resources": {"Registry": {
                    "Type": "AWS::ECR::Repository",
                    "Properties": {
                        "RepositoryName": "cfn-lifecycle-text-images",
                        "LifecyclePolicy": {
                            "LifecyclePolicyText": two_rules.to_string(),
                            "RegistryId": ACCOUNT_ID,
                        },
                    },
                }},
            })
            .to_string(),
            "cfn-lifecycle-text-images".into(),
            Some(two_rules.clone()),
        ),
        (
            // A registry id is twelve digits, which a template may spell as the number it
            // is rather than as text; this emulator's own registry id is twelve ZEROS,
            // which no number can spell, so the number's path is graded by the refusal row
            // of the next test instead.
            "json, the policy written as the document itself, under the registry it names",
            json!({
                "Resources": {"Registry": {
                    "Type": "AWS::ECR::Repository",
                    "Properties": {
                        "RepositoryName": "cfn-named-registry-images",
                        "LifecyclePolicy": {
                            "LifecyclePolicyText": two_rules.clone(),
                            "RegistryId": ACCOUNT_ID,
                        },
                    },
                }},
            })
            .to_string(),
            "cfn-named-registry-images".into(),
            Some(two_rules),
        ),
        (
            "yaml, the policy written as JSON text on a repository whose name \
             CloudFormation generated",
            format!(
                concat!(
                    "Resources:\n",
                    "  Registry:\n",
                    "    Type: AWS::ECR::Repository\n",
                    "    Properties:\n",
                    "      LifecyclePolicy:\n",
                    "        LifecyclePolicyText: '{}'\n",
                ),
                one_rule
            ),
            // CloudFormation generates the name, so the row reads the policy under the
            // physical id the stack reports rather than a value fixed here.
            None,
            Some(one_rule),
        ),
        (
            "json, a repository declaring no lifecycle policy at all",
            json!({
                "Resources": {"Registry": {
                    "Type": "AWS::ECR::Repository",
                    "Properties": {"RepositoryName": "cfn-unpoliced-images"},
                }},
            })
            .to_string(),
            "cfn-unpoliced-images".into(),
            None,
        ),
    ];

    for (index, (case, template, declared_name, expected)) in rows.iter().enumerate() {
        let router = router();
        let stack = format!("ecr-lifecycle-stack-{index}");
        let (status, reason) = deploy_until_settled(case, &router, &stack, template).await;
        assert_eq!(
            status, "CREATE_COMPLETE",
            "case {case:?}: expected the create to complete, got {status:?} ({reason:?})"
        );
        // The repository the policy must sit on is the one the stack reports, which is the
        // name `PutLifecyclePolicy` was asked for.
        let name = match declared_name {
            Some(declared) => (*declared).to_string(),
            None => physical_id(&router, &stack, "Registry").await,
        };
        let held = lifecycle_policy(&router, &name).await;
        match expected {
            Some(policy) => assert_eq!(
                held.as_ref().ok(),
                Some(policy),
                "case {case:?}: expected the declared lifecycle policy on {name}, got {held:?}"
            ),
            // A repository with no policy is NOT a repository that does not exist: the
            // registry holds it, and the read answers only that it carries no policy.
            None => assert_eq!(
                held.as_ref().err().map(String::as_str),
                Some("LifecyclePolicyNotFoundException"),
                "case {case:?}: expected no lifecycle policy on {name}, got {held:?}"
            ),
        }
    }
}

/// RULE: a `LifecyclePolicy` this provider cannot build a `PutLifecyclePolicy` from, or
/// one Amazon ECR refuses, FAILS the resource rather than completing with that part of the
/// declaration dropped — and the repository is not left behind.
#[tokio::test]
async fn a_lifecycle_policy_declaring_no_document_fails_the_deployment() {
    // A policy document every row below is otherwise free to carry, so each row's refusal
    // is the one thing it varies.
    let document = json!({"rules": [lifecycle_rule(
        1,
        "Expire untagged images after 30 days",
        json!({
            "tagStatus": "untagged",
            "countType": "sinceImagePushed",
            "countUnit": "days",
            "countNumber": 30,
        }),
    )]});
    // (case, the declared `LifecyclePolicy`, what the failed resource's event must name)
    let rows: [(&str, Value, &str); 3] = [
        (
            "the property declares no LifecyclePolicyText",
            json!({}),
            "LifecyclePolicy",
        ),
        (
            "the property is not a settings document",
            json!("{\"rules\":[]}"),
            "LifecyclePolicy",
        ),
        (
            "the property declares a RegistryId that is no registry id",
            json!({"LifecyclePolicyText": document, "RegistryId": [ACCOUNT_ID]}),
            "RegistryId",
        ),
    ];

    for (index, (case, declared, named)) in rows.iter().enumerate() {
        let template = json!({
            "Resources": {"Registry": {
                "Type": "AWS::ECR::Repository",
                "Properties": {
                    "RepositoryName": "cfn-undocumented-lifecycle-images",
                    "LifecyclePolicy": declared,
                },
            }},
        })
        .to_string();

        let router = router();
        let stack = format!("ecr-lifecycle-refused-stack-{index}");
        let (status, reason) = deploy_until_settled(case, &router, &stack, &template).await;
        assert_eq!(
            status, "ROLLBACK_COMPLETE",
            "case {case:?}: expected the failed create to be rolled back, got {status:?} \
             ({reason:?})"
        );
        let on_the_resource =
            resource_event_reason(&router, &stack, "Registry", resource_status::CREATE_FAILED)
                .await;
        assert!(
            on_the_resource.contains(named),
            "case {case:?}: expected the failed resource's event to name {named:?}, got \
             {on_the_resource:?}"
        );
        assert!(
            repository(&router, "cfn-undocumented-lifecycle-images")
                .await
                .is_none(),
            "case {case:?}: a refused declaration must leave no repository in Amazon ECR"
        );
    }
}

/// RULE: a declared `RegistryId` reaches `PutLifecyclePolicy` as its one textual spelling,
/// whether the template writes it as text or as the twelve-digit number it is — so a
/// declaration naming ANOTHER account's registry is refused by Amazon ECR itself, which is
/// what shows the declared value was forwarded rather than dropped.
#[tokio::test]
async fn a_lifecycle_policy_naming_another_registry_is_refused_by_ecr() {
    // Written as a number, which is how a template may spell a twelve-digit account id,
    // and not this emulator's registry.
    let other_registry = 123_456_789_012_u64;
    let template = json!({
        "Resources": {"Registry": {
            "Type": "AWS::ECR::Repository",
            "Properties": {
                "RepositoryName": "cfn-foreign-registry-images",
                "LifecyclePolicy": {
                    "LifecyclePolicyText": {"rules": [lifecycle_rule(
                        1,
                        "Expire untagged images after 30 days",
                        json!({
                            "tagStatus": "untagged",
                            "countType": "sinceImagePushed",
                            "countUnit": "days",
                            "countNumber": 30,
                        }),
                    )]},
                    "RegistryId": other_registry,
                },
            },
        }},
    })
    .to_string();

    let case = "a lifecycle policy named for another account's registry";
    let router = router();
    let stack = "ecr-foreign-registry-stack";
    let (status, reason) = deploy_until_settled(case, &router, stack, &template).await;
    assert_eq!(
        status, "ROLLBACK_COMPLETE",
        "case {case:?}: expected the failed create to be rolled back, got {status:?} \
         ({reason:?})"
    );
    let on_the_resource =
        resource_event_reason(&router, stack, "Registry", resource_status::CREATE_FAILED).await;
    assert!(
        on_the_resource.contains(&other_registry.to_string()),
        "case {case:?}: expected the owning service's refusal to name the declared registry \
         {other_registry}, got {on_the_resource:?}"
    );
}

/// RULE: deleting the stack deletes the repository it created, so the registry no longer
/// holds it.
#[tokio::test]
async fn deleting_the_stack_deletes_the_repository_it_created() {
    let template = json!({
        "Resources": {"Registry": {
            "Type": "AWS::ECR::Repository",
            "Properties": {"RepositoryName": "cfn-deleted-images"},
        }},
    })
    .to_string();

    let router = router();
    let (status, reason) = deploy(&router, "ecr-delete-stack", &template).await;
    assert_eq!(
        status.as_deref(),
        Some("CREATE_COMPLETE"),
        "expected the create to complete, got {status:?} ({reason:?})"
    );
    assert!(
        repository(&router, "cfn-deleted-images").await.is_some(),
        "the stack's repository must exist before the delete"
    );

    let (code, body) = call(
        &router,
        "DeleteStack",
        &[member("StackName", "ecr-delete-stack")],
    )
    .await;
    assert_eq!(code, 200, "DeleteStack answered {code}: {body}");
    wait_for_the_transition().await;
    assert!(
        repository(&router, "cfn-deleted-images").await.is_none(),
        "the repository must be gone with the stack that created it"
    );
}

/// Deploy `template` on the stack `name` as an UPDATE and answer the status it SETTLES on,
/// through the suite-wide waiter — the companion of [`deploy_until_settled`] for the second
/// deployment of one stack.
async fn update_until_settled(
    case: &str,
    router: &MultiServiceRouter,
    name: &str,
    template: &str,
) -> (String, Option<String>) {
    let (status, body) = call(
        router,
        "UpdateStack",
        &[member("StackName", name), member("TemplateBody", template)],
    )
    .await;
    assert_eq!(status, 200, "UpdateStack({name}) answered {status}: {body}");
    let settled = settled_stack_status(case, router, REGION, name).await;
    let (_, body) = describe_stack(router, name).await;
    (settled, xml_text(&body, "StackStatusReason"))
}

/// RULE: the same settings document that reaches the owning service's own call on the CREATE
/// reaches it again when a LATER deployment states it differently — for a repository's
/// `LifecyclePolicy`, through `PutLifecyclePolicy` for the document the new declaration
/// carries and `DeleteLifecyclePolicy` for a declaration that stops carrying one — so the
/// repository the stack already has ends up with the policy the template now declares rather
/// than the deployment reporting a change it did not apply.
#[tokio::test]
async fn a_changed_lifecycle_policy_is_applied_to_the_repository_the_stack_already_has() {
    let untagged = lifecycle_rule(
        1,
        "Expire untagged images after 14 days",
        json!({
            "tagStatus": "untagged",
            "countType": "sinceImagePushed",
            "countUnit": "days",
            "countNumber": 14,
        }),
    );
    let released = lifecycle_rule(
        2,
        "Keep the last 10 release images",
        json!({
            "tagStatus": "tagged",
            "tagPrefixList": ["release"],
            "countType": "imageCountMoreThan",
            "countNumber": 10,
        }),
    );
    let one_rule = json!({"rules": [untagged.clone()]});
    let two_rules = json!({"rules": [untagged, released]});

    // The rows differ along WHAT the second deployment does to the declared policy: replaces
    // the document, adds one to a repository created without any, and drops the one the
    // repository carries.
    let declaration = |name: &str, policy: Option<&Value>| {
        let mut properties = json!({"RepositoryName": name});
        if let Some(policy) = policy {
            properties["LifecyclePolicy"] = json!({"LifecyclePolicyText": policy.clone()});
        }
        json!({
            "Resources": {"Registry": {
                "Type": "AWS::ECR::Repository",
                "Properties": properties,
            }},
        })
        .to_string()
    };
    // (case, the repository's name, the policy the create declares, the policy the update
    // declares, the policy the repository must hold afterwards)
    type LifecycleUpdateRow = (
        &'static str,
        &'static str,
        Option<Value>,
        Option<Value>,
        Option<Value>,
    );
    let rows: Vec<LifecycleUpdateRow> = vec![
        (
            "the document replaced by the update",
            "cfn-updated-lifecycle",
            Some(one_rule.clone()),
            Some(two_rules.clone()),
            Some(two_rules.clone()),
        ),
        (
            "a policy added by the update to a repository created without one",
            "cfn-late-lifecycle",
            None,
            Some(one_rule.clone()),
            Some(one_rule.clone()),
        ),
        (
            "the policy dropped by the update",
            "cfn-unpoliced-again",
            Some(two_rules),
            None,
            None,
        ),
    ];

    for (index, (case, name, created, updated, expected)) in rows.iter().enumerate() {
        let router = router();
        let stack = format!("ecr-lifecycle-update-stack-{index}");
        let (status, reason) =
            deploy_until_settled(case, &router, &stack, &declaration(name, created.as_ref())).await;
        assert_eq!(
            status, "CREATE_COMPLETE",
            "case {case:?}: expected the create to complete, got {status:?} ({reason:?})"
        );
        let (status, reason) =
            update_until_settled(case, &router, &stack, &declaration(name, updated.as_ref())).await;
        assert_eq!(
            status, "UPDATE_COMPLETE",
            "case {case:?}: expected the update to complete, got {status:?} ({reason:?})"
        );
        // An in-place change keeps the repository's identity: the policy is read back under
        // the name the create published.
        let reported = physical_id(&router, &stack, "Registry").await;
        assert_eq!(
            reported, *name,
            "case {case:?}: expected the update to keep the repository {name:?}, got {reported:?}"
        );
        let held = lifecycle_policy(&router, &reported).await;
        match expected {
            Some(policy) => assert_eq!(
                held.as_ref().ok(),
                Some(policy),
                "case {case:?}: expected the policy the update declares on {reported}, got \
                 {held:?}"
            ),
            None => assert_eq!(
                held.as_ref().err().map(String::as_str),
                Some("LifecyclePolicyNotFoundException"),
                "case {case:?}: expected the update to take the policy off {reported}, got \
                 {held:?}"
            ),
        }
    }
}
