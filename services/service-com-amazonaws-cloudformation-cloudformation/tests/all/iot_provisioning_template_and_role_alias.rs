// SPDX-License-Identifier: CC0-1.0

//! `AWS::IoT::ProvisioningTemplate` and `AWS::IoT::RoleAlias` end to end: the resources a
//! stack declares exist in AWS IoT itself, under the physical ids the stack publishes.
//!
//! RULE: a resource type CloudFormation deploys is created in the service that OWNS it,
//! through that service's own public API, under the physical id the stack publishes — so the
//! name a client reads out of a stack's outputs is one that service answers for, the
//! attribute the stack publishes is the ARN that service minted, and every precondition that
//! service enforces on the create is the stack's answer too.
//!
//! The rows differ along what the rule generalises over: whether the template NAMES each
//! resource or leaves CloudFormation to name it, the template spelling (JSON or YAML),
//! whether the provisioning template is enabled and carries a description, and whether the
//! role alias states a credential duration.
//!
//! Every case drives the real request path for both services through one router — the
//! CloudFormation calls over the awsQuery wire (the harness `stack_lifecycle.rs` owns), the
//! AWS IoT calls over its REST wire, and the IAM role the provisioning template provisions
//! as declared in the same template — so what is asserted is what a client that deploys the
//! template and then reads the resources back observes.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::stack_lifecycle::{
    acknowledging_iam, call, create_stack, describe_stack, member, output_value, router,
    wait_for_the_transition, xml_text, ACCOUNT_ID, REGION,
};

/// Call one AWS IoT operation through the same router, on the method and URI that service's
/// model binds it to (`restJson1`): the status and the parsed answer document.
async fn iot(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map(ToString::to_string).unwrap_or_default();
    let http_request = http::Request::builder()
        .method(method)
        .uri(format!("https://iot.{REGION}.amazonaws.com{uri}"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iot/\
                 aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed AWS IoT request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let answer = response.into_body().to_vec();
    (status, serde_json::from_slice(&answer).unwrap_or(Value::Null))
}

/// The provisioning template AWS IoT holds under `name`, or `None` where it holds none.
async fn described_template(router: &MultiServiceRouter, name: &str) -> Option<Value> {
    let (status, document) = iot(
        router,
        "GET",
        &format!("/provisioning-templates/{name}"),
        None,
    )
    .await;
    (status == 200).then_some(document)
}

/// The role alias AWS IoT holds under `name`, or `None` where it holds none.
async fn described_role_alias(router: &MultiServiceRouter, name: &str) -> Option<Value> {
    let (status, document) = iot(router, "GET", &format!("/role-aliases/{name}"), None).await;
    (status == 200).then(|| document.get("roleAliasDescription").cloned())?
}

/// The trust policy a provisioning role carries: AWS IoT assumes it, which is the
/// precondition that service enforces on `CreateProvisioningTemplate`.
fn provisioning_trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "iot.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// A provisioning template body: one `AWS::IoT::Thing` resource provisioned from a serial
/// number, which is the structure AWS documents for a provisioning template.
fn template_body() -> String {
    json!({
        "Parameters": { "SerialNumber": { "Type": "String" } },
        "Resources": {
            "thing": {
                "Type": "AWS::IoT::Thing",
                "Properties": { "ThingName": "device-1" },
            },
        },
    })
    .to_string()
}

/// One deployment: the template, the names the two IoT resources must end up under, and what
/// the declaration asked for.
struct Row {
    case: &'static str,
    stack: &'static str,
    template: String,
    /// The template name the declaration states, or `None` where CloudFormation names it.
    template_name: Option<&'static str>,
    /// The alias name the declaration states, or `None` where CloudFormation names it.
    alias_name: Option<&'static str>,
    /// Whether the provisioning template is enabled, as the declaration asked.
    enabled: bool,
    /// The credential duration the alias states, where it states one.
    duration: Option<i64>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "both resources named by the template, in JSON, enabled and with a \
                   credential duration",
            stack: "iot-provisioning-named",
            template: json!({
                "Resources": {
                    "ProvisioningRole": {
                        "Type": "AWS::IAM::Role",
                        "Properties": {
                            "RoleName": "named-provisioning-role",
                            "AssumeRolePolicyDocument": provisioning_trust_policy()
                        }
                    },
                    "Template": {
                        "Type": "AWS::IoT::ProvisioningTemplate",
                        "Properties": {
                            "TemplateName": "named-template",
                            "TemplateBody": template_body(),
                            "ProvisioningRoleArn": {"Fn::GetAtt": ["ProvisioningRole", "Arn"]},
                            "Description": "a fleet provisioning template",
                            "Enabled": true,
                            "Tags": [{"Key": "Owner", "Value": "fleet"}]
                        }
                    },
                    "Alias": {
                        "Type": "AWS::IoT::RoleAlias",
                        "Properties": {
                            "RoleAlias": "named-alias",
                            "RoleArn": {"Fn::GetAtt": ["ProvisioningRole", "Arn"]},
                            "CredentialDurationSeconds": 3600
                        }
                    }
                },
                "Outputs": {
                    "TemplateName": {"Value": {"Ref": "Template"}},
                    "TemplateArn": {"Value": {"Fn::GetAtt": ["Template", "TemplateArn"]}},
                    "AliasName": {"Value": {"Ref": "Alias"}},
                    "AliasArn": {"Value": {"Fn::GetAtt": ["Alias", "RoleAliasArn"]}}
                }
            })
            .to_string(),
            template_name: Some("named-template"),
            alias_name: Some("named-alias"),
            enabled: true,
            duration: Some(3600),
        },
        Row {
            case: "nothing named by the template, in YAML, not enabled and with no \
                   credential duration",
            stack: "iotgen",
            template: [
                "Resources:",
                "  ProvisioningRole:",
                "    Type: AWS::IAM::Role",
                "    Properties:",
                "      AssumeRolePolicyDocument: >-",
                &format!("        {}", provisioning_trust_policy()),
                "  Template:",
                "    Type: AWS::IoT::ProvisioningTemplate",
                "    Properties:",
                "      TemplateBody: >-",
                &format!("        {}", template_body()),
                "      ProvisioningRoleArn: !GetAtt ProvisioningRole.Arn",
                // A template a declaration does not enable is created disabled, which is
                // the owning service's own default for the member.
                "      Enabled: 'false'",
                "  Alias:",
                "    Type: AWS::IoT::RoleAlias",
                "    Properties:",
                "      RoleArn: !GetAtt ProvisioningRole.Arn",
                "Outputs:",
                "  TemplateName:",
                "    Value: !Ref Template",
                "  TemplateArn:",
                "    Value: !GetAtt Template.TemplateArn",
                "  AliasName:",
                "    Value: !Ref Alias",
                "  AliasArn:",
                "    Value: !GetAtt Alias.RoleAliasArn",
            ]
            .join("\n"),
            template_name: None,
            alias_name: None,
            enabled: false,
            duration: None,
        },
    ]
}

/// RULE: both IoT types a stack declares are created in AWS IoT itself, the stack publishes
/// the names they were created under and the ARNs that service minted, a provisioning claim
/// can be issued through the deployed template, and deleting the stack releases both.
#[tokio::test]
async fn the_declared_iot_resources_are_created_in_the_owning_service() {
    for row in rows() {
        let case = row.case;
        let router = router();
        let (status, body) =
            create_stack(&router, row.stack, &row.template, &acknowledging_iam(&[])).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {body}"
        );
        wait_for_the_transition().await;
        let (_, body) = describe_stack(&router, row.stack).await;
        assert_eq!(
            xml_text(&body, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to complete, got {body}"
        );

        // The names the stack PUBLISHES are the ones the resources answer to.
        let template_name = output_value(&body, "TemplateName")
            .unwrap_or_else(|| panic!("case {case:?}: no TemplateName output in {body}"));
        let alias_name = output_value(&body, "AliasName")
            .unwrap_or_else(|| panic!("case {case:?}: no AliasName output in {body}"));
        if let Some(declared) = row.template_name {
            assert_eq!(
                template_name, declared,
                "case {case:?}: expected the declared template name, got {template_name:?}"
            );
        }
        if let Some(declared) = row.alias_name {
            assert_eq!(
                alias_name, declared,
                "case {case:?}: expected the declared alias name, got {alias_name:?}"
            );
        }

        // AWS IoT's OWN read answers for both, with what the declaration asked for.
        let template = described_template(&router, &template_name)
            .await
            .unwrap_or_else(|| {
                panic!("case {case:?}: AWS IoT holds no template {template_name:?}")
            });
        assert_eq!(
            template.get("templateName").and_then(Value::as_str),
            Some(template_name.as_str()),
            "case {case:?}: expected the template to answer under the published name, got \
             {template}"
        );
        assert_eq!(
            template.get("templateBody").and_then(Value::as_str),
            Some(template_body().as_str()),
            "case {case:?}: expected the declared body to have reached AWS IoT, got {template}"
        );
        assert_eq!(
            template.get("enabled").and_then(Value::as_bool),
            Some(row.enabled),
            "case {case:?}: expected enabled {}, got {template}",
            row.enabled
        );
        let alias = described_role_alias(&router, &alias_name)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: AWS IoT holds no alias {alias_name:?}"));
        assert_eq!(
            alias.get("roleAlias").and_then(Value::as_str),
            Some(alias_name.as_str()),
            "case {case:?}: expected the alias to answer under the published name, got {alias}"
        );
        if let Some(duration) = row.duration {
            assert_eq!(
                alias.get("credentialDurationSeconds").and_then(Value::as_i64),
                Some(duration),
                "case {case:?}: expected the declared duration to have reached AWS IoT, got \
                 {alias}"
            );
        }

        // The ARNs the stack publishes are the ones that service minted — read back off its
        // own records rather than compared against a literal.
        assert_eq!(
            output_value(&body, "TemplateArn"),
            template
                .get("templateArn")
                .and_then(Value::as_str)
                .map(str::to_string),
            "case {case:?}: expected the published TemplateArn to be the one AWS IoT minted, \
             got {body}"
        );
        assert_eq!(
            output_value(&body, "AliasArn"),
            alias
                .get("roleAliasArn")
                .and_then(Value::as_str)
                .map(str::to_string),
            "case {case:?}: expected the published RoleAliasArn to be the one AWS IoT minted, \
             got {body}"
        );
        assert!(
            output_value(&body, "TemplateArn").is_some_and(|arn| arn
                .starts_with(&format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:"))),
            "case {case:?}: expected the minted ARN to be an AWS IoT ARN of this account, got \
             {body}"
        );

        // The deployed template really provisions: a claim issued through it is the owning
        // service's own, which is only possible because the template exists there.
        let (claim_status, claim) = iot(
            &router,
            "POST",
            &format!("/provisioning-templates/{template_name}/provisioning-claim"),
            Some(&json!({})),
        )
        .await;
        assert_eq!(
            claim_status, 200,
            "case {case:?}: expected a provisioning claim through the deployed template, got \
             {claim_status} with {claim}"
        );
        assert!(
            claim
                .get("certificatePem")
                .and_then(Value::as_str)
                .is_some_and(|pem| pem.contains("BEGIN CERTIFICATE")),
            "case {case:?}: expected the claim to carry a certificate, got {claim}"
        );

        // Deleting the stack releases both in the owning service.
        let (status, body) = call(&router, "DeleteStack", &[member("StackName", row.stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with {body}"
        );
        wait_for_the_transition().await;
        assert!(
            described_template(&router, &template_name).await.is_none(),
            "case {case:?}: expected the deleted stack to have released the template \
             {template_name:?}"
        );
        assert!(
            described_role_alias(&router, &alias_name).await.is_none(),
            "case {case:?}: expected the deleted stack to have released the alias \
             {alias_name:?}"
        );
    }
}

/// A declaration the OWNING SERVICE refuses fails the resource and rolls the stack back:
/// `CredentialDurationSeconds` outside the range AWS IoT models for it, and a provisioning
/// role that service cannot assume, are each its answer rather than CloudFormation's.
#[tokio::test]
async fn a_declaration_the_owning_service_refuses_rolls_the_stack_back() {
    // The rows span the two preconditions AWS IoT enforces on these creates, each with the
    // resource that must fail and the name nothing may be left under.
    let cases: Vec<(&str, &str, String, &str)> = vec![
        (
            "a credential duration under the modelled minimum",
            "iot-alias-short",
            json!({
                "Resources": {
                    "Alias": {
                        "Type": "AWS::IoT::RoleAlias",
                        "Properties": {
                            "RoleAlias": "too-short-alias",
                            "RoleArn": format!("arn:aws:iam::{ACCOUNT_ID}:role/device"),
                            "CredentialDurationSeconds": 60
                        }
                    }
                }
            })
            .to_string(),
            "Alias",
        ),
        (
            "a credential duration past the modelled maximum",
            "iot-alias-long",
            json!({
                "Resources": {
                    "Alias": {
                        "Type": "AWS::IoT::RoleAlias",
                        "Properties": {
                            "RoleAlias": "too-long-alias",
                            "RoleArn": format!("arn:aws:iam::{ACCOUNT_ID}:role/device"),
                            "CredentialDurationSeconds": 86400
                        }
                    }
                }
            })
            .to_string(),
            "Alias",
        ),
        (
            "a provisioning role AWS IoT cannot assume",
            "iot-template-untrusted",
            json!({
                "Resources": {
                    "Template": {
                        "Type": "AWS::IoT::ProvisioningTemplate",
                        "Properties": {
                            "TemplateName": "untrusted-template",
                            "TemplateBody": template_body(),
                            "ProvisioningRoleArn": format!(
                                "arn:aws:iam::{ACCOUNT_ID}:role/no-such-role"
                            )
                        }
                    }
                }
            })
            .to_string(),
            "Template",
        ),
    ];

    for (case, stack, template, logical_id) in cases {
        let router = router();
        let (status, body) = create_stack(&router, stack, &template, &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {body}"
        );
        wait_for_the_transition().await;
        let (_, body) = describe_stack(&router, stack).await;
        assert_eq!(
            xml_text(&body, "StackStatus").as_deref(),
            Some("ROLLBACK_COMPLETE"),
            "case {case:?}: expected the refused create to be rolled back rather than report a \
             resource it did not create, got {body}"
        );
        let reason = xml_text(&body, "StackStatusReason").unwrap_or_default();
        assert!(
            reason.contains(logical_id),
            "case {case:?}: expected the reason to name the resource that failed, got {reason:?}"
        );
    }

    // Nothing was left behind by any of them: the names the refused stacks declared resolve
    // to nothing in the owning service.
    let router = router();
    for name in ["too-short-alias", "too-long-alias"] {
        assert!(
            described_role_alias(&router, name).await.is_none(),
            "case {name:?}: a refused create must leave no alias behind"
        );
    }
    assert!(
        described_template(&router, "untrusted-template")
            .await
            .is_none(),
        "case \"untrusted-template\": a refused create must leave no template behind"
    );
}

/// A changed declaration reaches the owning service's own update: the resources keep their
/// identity and report what the new revision states.
#[tokio::test]
async fn a_changed_declaration_is_applied_through_the_owning_service() {
    let router = router();
    let stack = "iot-provisioning-updated";
    let template = |enabled: bool, duration: i64| {
        json!({
            "Resources": {
                "ProvisioningRole": {
                    "Type": "AWS::IAM::Role",
                    "Properties": {
                        "RoleName": "updated-provisioning-role",
                        "AssumeRolePolicyDocument": provisioning_trust_policy()
                    }
                },
                "Template": {
                    "Type": "AWS::IoT::ProvisioningTemplate",
                    "Properties": {
                        "TemplateName": "updated-template",
                        "TemplateBody": template_body(),
                        "ProvisioningRoleArn": {"Fn::GetAtt": ["ProvisioningRole", "Arn"]},
                        "Enabled": enabled
                    }
                },
                "Alias": {
                    "Type": "AWS::IoT::RoleAlias",
                    "Properties": {
                        "RoleAlias": "updated-alias",
                        "RoleArn": {"Fn::GetAtt": ["ProvisioningRole", "Arn"]},
                        "CredentialDurationSeconds": duration
                    }
                }
            }
        })
        .to_string()
    };

    let case = "the first revision";
    let (status, body) =
        create_stack(&router, stack, &template(true, 900), &acknowledging_iam(&[])).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {body}"
    );
    wait_for_the_transition().await;

    let case = "the revision that disables the template and lengthens the credential";
    let (status, body) = call(
        &router,
        "UpdateStack",
        &acknowledging_iam(&[
            member("StackName", stack),
            member("TemplateBody", &template(false, 7200)),
        ]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with {body}"
    );
    wait_for_the_transition().await;
    let (_, body) = describe_stack(&router, stack).await;
    assert_eq!(
        xml_text(&body, "StackStatus").as_deref(),
        Some("UPDATE_COMPLETE"),
        "case {case:?}: expected the update to complete, got {body}"
    );

    let updated = described_template(&router, "updated-template")
        .await
        .unwrap_or_else(|| panic!("case {case:?}: AWS IoT holds no updated template"));
    assert_eq!(
        updated.get("enabled").and_then(Value::as_bool),
        Some(false),
        "case {case:?}: expected the revision to have reached AWS IoT, got {updated}"
    );
    let alias = described_role_alias(&router, "updated-alias")
        .await
        .unwrap_or_else(|| panic!("case {case:?}: AWS IoT holds no updated alias"));
    assert_eq!(
        alias.get("credentialDurationSeconds").and_then(Value::as_i64),
        Some(7200),
        "case {case:?}: expected the revised duration to have reached AWS IoT, got {alias}"
    );
}
