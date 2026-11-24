// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::ECR::SigningConfiguration` deploys: the
//! configuration is written onto the PRIVATE Amazon ECR registry itself, that registry's own
//! `GetSigningConfiguration` answers the rules the template declared, re-declaring them changes
//! the configuration in place, deleting the stack takes it away — and a SECOND declaration,
//! while the registry already holds one, fails the resource with the already-exists reason
//! rather than quietly taking the configuration over.
//!
//! The rule under test: **a resource type AWS publishes is one a template may declare, and a
//! declaration of it is deployed by calling the service that OWNS the type — so what a client
//! reads from that service afterwards is what the template declared, and a declaration that
//! service (or the resource's own handler) refuses is a FAILED RESOURCE rather than a
//! template-format error.** Two crates have to agree for that to hold (CloudFormation submits
//! the template and makes the call, Amazon ECR serves it), so the suite lives here: a
//! crate-local test can only ever see one side of it.
//!
//! The deploy rows differ along what the rule generalises over: how many rules the declared
//! configuration carries (none and ten, the two bounds its modelled `@length(min: 0, max: 10)`
//! publishes, and one and two between them), whether a rule carries repository filters and how
//! many (none, one, two), the template spelling (JSON with long-form intrinsics, YAML with
//! short-form tags), which published return value the output reads (`Ref`,
//! `Fn::GetAtt RegistryId`), and the Region the stack deploys in. No row may ever see the
//! `Template format error: Unrecognized resource types` a type CloudFormation does not know is
//! refused with.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode, opt_element, outputs_of};

/// The `X-Amz-Target` prefix Amazon ECR's model fixes, and the signing name that routes a
/// request to it (`sigv4Name: ecr`, service shape `AmazonEC2ContainerRegistry_V20150921`,
/// awsJson1.1).
const ECR_TARGET_PREFIX: &str = "AmazonEC2ContainerRegistry_V20150921";

/// The account id every emulated ARN carries, which is also the registry id of this emulator's
/// one private registry (`engine::emulator_account_id`).
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// How a stack read waits for the deployment's clock-owed span to come due: look again, with a
/// deadline generous enough that a loaded host still reaches it.
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const POLL_ATTEMPTS: usize = 120;

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with,
/// which is what the defect this suite pins answered for a type AWS publishes.
const UNRECOGNIZED_TYPES: &str = "Unrecognized resource types";

/// The error code Amazon ECR answers for a registry that holds no signing configuration, and
/// the one the type's own handler reports for a configuration that is already there.
const NOT_FOUND_CODE: &str = "SigningConfigurationNotFoundException";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which is
/// what routes the request. The signature is never checked: request authentication is off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation action over its awsQuery wire, in `region`.
async fn cloudformation(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    members: &[(&str, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation", region))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one Amazon ECR operation over its awsJson1.1 wire, in `region`.
async fn ecr(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ecr.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{ECR_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization("ecr", region))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// The signing configuration that registry holds, as it answers it, or the error code it
/// answered instead.
async fn held_configuration(
    router: &MultiServiceRouter,
    region: &str,
) -> Result<Value, (u16, String)> {
    let (status, payload) = ecr(router, region, "GetSigningConfiguration", &json!({})).await;
    if status == 200 {
        return Ok(payload
            .get("signingConfiguration")
            .cloned()
            .unwrap_or(Value::Null));
    }
    let code = payload
        .get("__type")
        .or_else(|| payload.get("code"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit(['#', ':'])
        .next()
        .unwrap_or_default()
        .to_string();
    Err((status, code))
}

/// Read a stack until it reports a status no longer in progress, and answer that read's body.
async fn poll_to_terminal(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    stack: &str,
) -> String {
    let mut described = String::new();
    for _ in 0..POLL_ATTEMPTS {
        let (status, body) = cloudformation(
            router,
            region,
            "DescribeStacks",
            &[("StackName", stack.to_string())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        described = body;
        if !opt_element(&described, "StackStatus")
            .unwrap_or_default()
            .ends_with("_IN_PROGRESS")
        {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    described
}

/// The stack's own events, which is where the reason a failed resource stopped is read.
async fn stack_events(router: &MultiServiceRouter, region: &str, stack: &str) -> String {
    let (_, body) = cloudformation(
        router,
        region,
        "DescribeStackEvents",
        &[("StackName", stack.to_string())],
    )
    .await;
    body
}

/// Submit a template and read the stack to a terminal status, answering that read's body. The
/// submission itself must be ADMITTED: a type AWS publishes is never a template-format error.
async fn submit(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    stack: &str,
    template: &str,
) -> String {
    let (status, body) = cloudformation(
        router,
        region,
        "CreateStack",
        &[
            ("StackName", stack.to_string()),
            ("TemplateBody", template.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack — an AWS::ECR::SigningConfiguration \
         is a type a template may declare — got {status} with body {body}"
    );
    assert!(
        !body.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: expected the type to be recognised, got {body}"
    );
    poll_to_terminal(router, case, region, stack).await
}

/// Submit a template and require the stack to COMPLETE.
async fn deploy(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    stack: &str,
    template: &str,
) -> String {
    let described = submit(router, case, region, stack, template).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the create to complete; reason {:?}; events {}",
        opt_element(&described, "StackStatusReason"),
        stack_events(router, region, stack).await,
    );
    described
}

/// The Signer profile ARN a declared rule signs with, in the form
/// `SigningRule$signingProfileArn`'s own pattern admits
/// (`^arn:aws(-[a-z]+)*:signer:[a-z0-9-]+:[0-9]{12}:\/signing-profiles\/[a-zA-Z0-9_]{2,}$`).
fn signing_profile(region: &str, name: &str) -> String {
    format!("arn:aws:signer:{region}:{ACCOUNT}:/signing-profiles/{name}")
}

/// One declared rule as a template writes it, and the filters it applies to.
fn declared_rule(profile: &str, filters: &[&str]) -> Value {
    let mut rule = json!({ "SigningProfileArn": profile });
    if !filters.is_empty() {
        rule["RepositoryFilters"] = Value::Array(
            filters
                .iter()
                .map(|filter| json!({ "Filter": filter, "FilterType": "WILDCARD_MATCH" }))
                .collect(),
        );
    }
    rule
}

/// What one row expects the registry to hold: each rule as the profile it signs with and the
/// filters it applies to, in the order the declaration stated them.
type ExpectedRules = Vec<(String, Vec<String>)>;

/// The rules that registry answered, read into the same shape.
fn reported_rules(configuration: &Value) -> ExpectedRules {
    configuration
        .get("rules")
        .and_then(Value::as_array)
        .map(|rules| {
            rules
                .iter()
                .map(|rule| {
                    let profile = rule
                        .get("signingProfileArn")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let filters = rule
                        .get("repositoryFilters")
                        .and_then(Value::as_array)
                        .map(|filters| {
                            filters
                                .iter()
                                .map(|filter| {
                                    format!(
                                        "{}/{}",
                                        filter
                                            .get("filter")
                                            .and_then(Value::as_str)
                                            .unwrap_or_default(),
                                        filter
                                            .get("filterType")
                                            .and_then(Value::as_str)
                                            .unwrap_or_default(),
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    (profile, filters)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The same reading of the rules a row DECLARED, so the two sides are compared in one shape.
fn expected_rules(rules: &[Value]) -> ExpectedRules {
    rules
        .iter()
        .map(|rule| {
            let profile = rule
                .get("SigningProfileArn")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let filters = rule
                .get("RepositoryFilters")
                .and_then(Value::as_array)
                .map(|filters| {
                    filters
                        .iter()
                        .map(|filter| {
                            format!(
                                "{}/{}",
                                filter.get("Filter").and_then(Value::as_str).unwrap_or_default(),
                                filter
                                    .get("FilterType")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            (profile, filters)
        })
        .collect()
}

/// One deploy row: the stack, the Region, the rules it declares and the output its template
/// publishes the registry under.
struct Row {
    case: &'static str,
    region: &'static str,
    stack: &'static str,
    /// Built per row, because a declared profile ARN carries the row's own Region.
    rules: fn(&'static str) -> Vec<Value>,
    /// The template, built from those rules.
    template: fn(&[Value]) -> String,
    /// The output key the registry id is published under.
    registry_output: &'static str,
}

/// The JSON spelling with long-form intrinsics, reading the identity through `Ref`.
fn json_template(rules: &[Value]) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Signing": {
                "Type": "AWS::ECR::SigningConfiguration",
                "Properties": { "Rules": rules }
            }
        },
        "Outputs": { "SigningRef": {"Value": {"Ref": "Signing"}} }
    })
    .to_string()
}

/// The JSON spelling reading the identity through `Fn::GetAtt RegistryId` instead.
fn json_template_with_get_att(rules: &[Value]) -> String {
    json!({
        "Resources": {
            "Signing": {
                "Type": "AWS::ECR::SigningConfiguration",
                "Properties": { "Rules": rules }
            }
        },
        "Outputs": {
            "SigningRegistry": {"Value": {"Fn::GetAtt": ["Signing", "RegistryId"]}}
        }
    })
    .to_string()
}

/// The YAML spelling with short-form tags, over the rules the row states.
fn yaml_template(rules: &[Value]) -> String {
    let mut body = String::from(
        "Resources:\n  Signing:\n    Type: AWS::ECR::SigningConfiguration\n    Properties:\n      \
         Rules:\n",
    );
    for rule in rules {
        let profile = rule
            .get("SigningProfileArn")
            .and_then(Value::as_str)
            .unwrap_or_default();
        body.push_str(&format!("        - SigningProfileArn: {profile}\n"));
        if let Some(filters) = rule.get("RepositoryFilters").and_then(Value::as_array) {
            body.push_str("          RepositoryFilters:\n");
            for filter in filters {
                let value = filter.get("Filter").and_then(Value::as_str).unwrap_or_default();
                let kind = filter
                    .get("FilterType")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                body.push_str(&format!(
                    "            - Filter: {value}\n              FilterType: {kind}\n"
                ));
            }
        }
    }
    body.push_str("Outputs:\n  SigningRegistry:\n    Value: !GetAtt Signing.RegistryId\n");
    body
}

const ROWS: &[Row] = &[
    // The issue's own stack: one rule matching one repository, read back through `Ref`.
    Row {
        case: "one rule with one repository filter, JSON spelling",
        region: "us-east-1",
        stack: "ecr-signing-round-trip",
        rules: |region| vec![declared_rule(&signing_profile(region, "release"), &["app/*"])],
        template: json_template,
        registry_output: "SigningRef",
    },
    // A rule carrying NO repository filters, which the member's own optionality admits, in
    // another Region and read back through `Fn::GetAtt RegistryId`.
    Row {
        case: "a rule with no repository filters, in eu-west-1",
        region: "eu-west-1",
        stack: "ecr-signing-unfiltered",
        rules: |region| vec![declared_rule(&signing_profile(region, "every_repo"), &[])],
        template: json_template_with_get_att,
        registry_output: "SigningRegistry",
    },
    // Two rules, one of them carrying two filters, in the YAML spelling.
    Row {
        case: "two rules, two filters on one of them, YAML spelling",
        region: "us-west-2",
        stack: "ecr-signing-pair",
        rules: |region| {
            vec![
                declared_rule(
                    &signing_profile(region, "team_profile"),
                    &["team/*", "shared/base"],
                ),
                declared_rule(&signing_profile(region, "fallback"), &[]),
            ]
        },
        template: yaml_template,
        registry_output: "SigningRegistry",
    },
    // A configuration at the modelled bound of ten rules.
    Row {
        case: "ten rules, the modelled maximum",
        region: "ap-southeast-2",
        stack: "ecr-signing-ten",
        rules: |region| {
            (1..=10)
                .map(|index| {
                    declared_rule(
                        &signing_profile(region, &format!("profile_{index}")),
                        &["*"],
                    )
                })
                .collect()
        },
        template: json_template,
        registry_output: "SigningRef",
    },
    // And the other published bound: NO rule at all, which `SigningRuleList`'s own
    // `@length(min: 0, max: 10)` admits — a configuration the registry holds and signs nothing
    // by.
    Row {
        case: "no rule at all, the modelled minimum",
        region: "us-east-2",
        stack: "ecr-signing-empty",
        rules: |_| Vec::new(),
        template: json_template,
        registry_output: "SigningRef",
    },
];

#[tokio::test]
async fn a_deployed_stack_writes_the_signing_configuration_onto_the_registry() {
    for row in ROWS {
        let router = build_router();
        let case = row.case;

        // Before the deployment the registry holds none, which is what makes the read after it
        // evidence of the deployment rather than of anything that was already there.
        assert_eq!(
            held_configuration(&router, row.region).await.err().map(|(_, code)| code),
            Some(NOT_FOUND_CODE.to_string()),
            "case {case:?}: expected the registry to hold no signing configuration before the \
             stack, it holds one"
        );

        let rules = (row.rules)(row.region);
        let template = (row.template)(&rules);
        let described = deploy(&router, case, row.region, row.stack, &template).await;

        // The stack publishes the registry the configuration belongs to.
        let outputs: BTreeMap<String, String> = outputs_of(&described);
        let published = outputs.get(row.registry_output).unwrap_or_else(|| {
            panic!("case {case:?}: no {} output in {described}", row.registry_output)
        });
        assert_eq!(
            published, ACCOUNT,
            "case {case:?}: expected the published identity to be the registry the \
             configuration belongs to, got {published:?}"
        );

        // And the OWNING service holds exactly what the template declared.
        let held = held_configuration(&router, row.region)
            .await
            .unwrap_or_else(|answered| {
                panic!(
                    "case {case:?}: expected Amazon ECR to hold the declared configuration, it \
                     answered {answered:?}"
                )
            });
        assert_eq!(
            reported_rules(&held),
            expected_rules(&rules),
            "case {case:?}: expected the declared rules to reach the registry, it holds {held}"
        );
    }
}

/// A declaration of the type, while the registry ALREADY holds a configuration, fails on the
/// resource: a registry holds one configuration, so there is nothing for a second declaration to
/// create and the put that would silently take the held one over is never made.
///
/// The two rows differ along WHO holds it — a client that wrote the configuration directly, and
/// another live stack of the Region that manages one. Both carry the resource HANDLER's own
/// already-exists report, because the identifier the failure is about is the registry the
/// configuration belongs to rather than a name the declaration states: there is no name the
/// caller asked for, so CloudFormation's own name-collision sentence (which names the stack
/// holding a contested name) is not what either failure reports. Either way the held
/// configuration is left exactly as it was.
#[tokio::test]
async fn a_declaration_fails_on_the_configuration_the_registry_already_holds() {
    // (case, the stack submitted, whether a FIRST STACK is what holds the configuration).
    let rows: [(&str, &str, bool); 2] = [
        (
            "a configuration a client wrote out of band",
            "ecr-signing-over-a-clients",
            false,
        ),
        (
            "a configuration another stack of the Region manages",
            "ecr-signing-over-a-stacks",
            true,
        ),
    ];

    for (case, stack, held_by_a_stack) in rows {
        let router = build_router();
        let region = "us-east-1";
        let held_rules = vec![declared_rule(&signing_profile(region, "first"), &["app/*"])];

        // What the registry holds before the declaration, written the row's own way.
        if held_by_a_stack {
            deploy(
                &router,
                case,
                region,
                "ecr-signing-holder",
                &json_template(&held_rules),
            )
            .await;
        } else {
            let (status, answered) = ecr(
                &router,
                region,
                "PutSigningConfiguration",
                &json!({
                    "signingConfiguration": {
                        "rules": [{
                            "signingProfileArn": signing_profile(region, "first"),
                            "repositoryFilters": [
                                { "filter": "app/*", "filterType": "WILDCARD_MATCH" }
                            ],
                        }]
                    }
                }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the client's own put to be served, got {status} with \
                 {answered}"
            );
        }

        // The declaration the registry has no room for.
        let declared = vec![declared_rule(&signing_profile(region, "second"), &["other/*"])];
        let described = submit(&router, case, region, stack, &json_template(&declared)).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("ROLLBACK_COMPLETE"),
            "case {case:?}: expected the declaration to fail and roll back, got {described}"
        );
        assert!(
            outputs_of(&described).is_empty(),
            "case {case:?}: expected no output for a resource nothing created, got {described}"
        );

        // The failure is the RESOURCE's, and never a refusal of the template.
        let events = stack_events(&router, region, stack).await;
        assert!(
            !events.contains(UNRECOGNIZED_TYPES),
            "case {case:?}: expected no template-format error for a type AWS publishes, got \
             {events}"
        );
        let reasons = elements(&events, "ResourceStatusReason");
        let expected = format!(
            "Resource handler returned message: \"Resource of type \
             'AWS::ECR::SigningConfiguration' with identifier '{ACCOUNT}' already exists.\""
        );
        assert!(
            reasons.iter().any(|reason| reason.contains(&expected)),
            "case {case:?}: expected a failure reason carrying {expected:?}, got {reasons:?}"
        );
        assert!(
            !reasons
                .iter()
                .any(|reason| reason.contains("already exists in stack")),
            "case {case:?}: expected no name-collision sentence for an identifier no \
             declaration names, got {reasons:?}"
        );

        // And the configuration that was already there is untouched: the refused deployment put
        // nothing over it.
        let held = held_configuration(&router, region)
            .await
            .unwrap_or_else(|answered| {
                panic!(
                    "case {case:?}: expected the held configuration to stand, got {answered:?}"
                )
            });
        assert_eq!(
            reported_rules(&held),
            expected_rules(&held_rules),
            "case {case:?}: expected the held rules to stand, the registry holds {held}"
        );
    }
}

#[tokio::test]
async fn re_declared_rules_replace_the_configuration_in_force() {
    let router = build_router();
    let region = "us-east-1";
    let stack = "ecr-signing-updated";

    // The rows differ along how the re-declaration changes the configuration: another profile,
    // another filter set, and more rules than before. Each replaces the one configuration the
    // registry holds rather than making a second.
    let rows: [(&str, Vec<Value>); 3] = [
        (
            "another profile",
            vec![declared_rule(&signing_profile(region, "second"), &["app/*"])],
        ),
        (
            "another filter set",
            vec![declared_rule(
                &signing_profile(region, "second"),
                &["app/*", "tools/*"],
            )],
        ),
        (
            "two rules where there was one",
            vec![
                declared_rule(&signing_profile(region, "second"), &["app/*"]),
                declared_rule(&signing_profile(region, "third"), &[]),
            ],
        ),
    ];

    let created = vec![declared_rule(&signing_profile(region, "first"), &["app/*"])];
    let described = deploy(
        &router,
        "the configuration a stack creates",
        region,
        stack,
        &json_template(&created),
    )
    .await;
    let identity = outputs_of(&described)
        .get("SigningRef")
        .cloned()
        .unwrap_or_default();

    for (case, rules) in rows {
        let (status, body) = cloudformation(
            &router,
            region,
            "UpdateStack",
            &[
                ("StackName", stack.to_string()),
                ("TemplateBody", json_template(&rules)),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {body}"
        );
        let described = poll_to_terminal(&router, case, region, stack).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("UPDATE_COMPLETE"),
            "case {case:?}: expected the update to complete; events {}",
            stack_events(&router, region, stack).await,
        );
        assert_eq!(
            outputs_of(&described).get("SigningRef"),
            Some(&identity),
            "case {case:?}: expected the configuration to keep its identity, got {described}"
        );

        let held = held_configuration(&router, region)
            .await
            .unwrap_or_else(|answered| {
                panic!("case {case:?}: expected the registry to hold a configuration, got {answered:?}")
            });
        assert_eq!(
            reported_rules(&held),
            expected_rules(&rules),
            "case {case:?}: expected the re-declared rules to reach the registry, it holds \
             {held}"
        );
    }
}

#[tokio::test]
async fn deleting_the_stack_takes_the_signing_configuration_away() {
    let router = build_router();
    let region = "eu-central-1";
    let stack = "ecr-signing-deleted";
    let case = "the configuration deleted with its stack";

    let rules = vec![declared_rule(&signing_profile(region, "release"), &["app/*"])];
    deploy(&router, case, region, stack, &json_template(&rules)).await;
    assert!(
        held_configuration(&router, region).await.is_ok(),
        "case {case:?}: expected the registry to hold the configuration the stack wrote"
    );

    let (status, body) = cloudformation(
        &router,
        region,
        "DeleteStack",
        &[("StackName", stack.to_string())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
    );
    // The stack is gone once the delete has settled; its resources are gone with it.
    for _ in 0..POLL_ATTEMPTS {
        if held_configuration(&router, region).await.is_err() {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    assert_eq!(
        held_configuration(&router, region)
            .await
            .err()
            .map(|(_, code)| code),
        Some(NOT_FOUND_CODE.to_string()),
        "case {case:?}: expected the configuration to be gone from Amazon ECR with its stack"
    );
}

#[tokio::test]
async fn a_declaration_amazon_ecr_refuses_rolls_the_stack_back() {
    let router = build_router();
    let region = "us-east-1";
    let case = "a signing profile ARN that is no Signer profile ARN";
    let stack = "ecr-signing-refused";

    // The ARN is outside `signingProfileArn`'s own pattern, so that registry refuses the put —
    // which is a FAILED RESOURCE, not a template-format error.
    let rules = vec![declared_rule("not-a-signer-profile-arn", &["app/*"])];
    let described = submit(&router, case, region, stack, &json_template(&rules)).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some("ROLLBACK_COMPLETE"),
        "case {case:?}: expected the refused declaration to roll the stack back, got {described}"
    );

    let events = stack_events(&router, region, stack).await;
    assert!(
        !events.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: expected no template-format error for a type AWS publishes, got {events}"
    );
    let reasons = elements(&events, "ResourceStatusReason");
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains("signingProfileArn")),
        "case {case:?}: expected a failure reason naming the parameter Amazon ECR refused, got \
         {reasons:?}"
    );

    // Nothing was written: the registry holds no configuration at all.
    assert_eq!(
        held_configuration(&router, region)
            .await
            .err()
            .map(|(_, code)| code),
        Some(NOT_FOUND_CODE.to_string()),
        "case {case:?}: expected the registry to hold nothing after a refused declaration"
    );
}
