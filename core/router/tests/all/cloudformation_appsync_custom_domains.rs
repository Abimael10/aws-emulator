// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack provisions the `AWS::AppSync::DomainName` and
//! `AWS::AppSync::DomainNameApiAssociation` resources its template declares, in the AppSync
//! control plane that owns them.
//!
//! The rule under test: **a stack reports a resource created only once the service that owns
//! its type really created it, through that service's own create API, and never while that
//! service still reports the resource short of the state it is usable in — so a client that
//! deployed a template declaring an AppSync custom domain reads that domain back with
//! `appsync GetDomainName`, under the name the template stated, carrying the configuration
//! the template declared and the endpoint, zone and ARN only AppSync assigns; a template
//! declaring the mapping of an API onto it has `GetApiAssociation` report that API as
//! `SUCCESS`, while a domain no template mapped reports the service's own
//! not-associated refusal; and both stop existing when the stack is deleted.** Two crates
//! have to agree for that to hold (CloudFormation makes the calls, AppSync serves them), so
//! the suite lives here rather than in either of them.
//!
//! The rows differ along what the rule generalises over: whether the template declares the
//! mapping at all, how it names the domain the mapping is made on (a literal and a `Ref` to
//! the domain resource), whether the domain declares a description and tags, the FORM of the
//! domain name (three labels, and four with a hyphen in one), which attributes the stack
//! publishes as outputs, and the language the template is written in (JSON, and YAML with
//! the short-form intrinsic tags). Every expected value is computed here from what the row
//! declares or read from the owning service, never from the answer being checked.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery call, the stack readers and the output readers this test crate writes once
// (`tests/all/cloudformation_apigatewayv2_resources.rs`, and
// `tests/all/cloudformation_provisions_stack_resources.rs` for the XML readers).
use crate::cloudformation_apigatewayv2_resources::{
    cloudformation, create_stack, describe_stack, every_output_has_a_value, member, REGION,
};
use crate::cloudformation_provisions_stack_resources::xml_text;
use crate::rds_docdb_neptune_signing_name_routing::authorization;

/// How often the suite asks the stack whether it has completed, and how long it goes on
/// asking. A mapping of an API onto a domain is reported `PROCESSING` when its create
/// answers, so the stack stays `CREATE_IN_PROGRESS` until AppSync reports it deployed: the
/// FIRST observation that reports the stack complete is the one every assertion is made
/// about. The budget is long enough that a loaded machine is slower than the spans these
/// services choose without the rule being any different.
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const POLLS: usize = 800;

/// The `StackStatus` values the rule is stated in.
const CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `AssociationStatus` a deployed mapping is in, out of the enum AppSync's model
/// declares for that member ("SUCCESS: The API association was successful").
const SUCCESS: &str = "SUCCESS";

/// What AppSync answers for a custom domain, or a mapping, it does not hold: the modelled
/// 404, with that condition's own sentence — nothing of the request is interpolated into
/// either.
const NOT_FOUND: &str = "NotFoundException";
const NO_SUCH_DOMAIN_NAME: &str = "Domain name not found";
const NOT_ASSOCIATED: &str = "Domain name is not associated.";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly
/// as `core/router/src/lib.rs` mounts them.
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
        service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::EmulatedService::new(engine),
    );
    router
}

/// Call one AppSync operation over its restJson1 wire: this service signs as `appsync` and
/// binds its custom-domain operations under `/v1/domainnames`.
///
/// Answers the HTTP status, the `x-amzn-errortype` a refusal carries (the wire error code a
/// client reads) and the answered document.
async fn appsync(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Option<String>, Value) {
    let payload = match &body {
        Some(document) => Bytes::from(document.to_string()),
        None => Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("appsync.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("appsync"))
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(['/', ':'])
                .next()
                .unwrap_or(value)
                .trim()
                .to_string()
        });
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, error_type, answered)
}

/// The certificate the custom domain's endpoint presents. A literal ARN in the one Region a
/// custom domain's distribution reads certificates from: the stack under test is about the
/// domain the template declares, and AppSync takes the certificate ARN as the configuration
/// it reports back.
fn certificate_arn() -> String {
    "arn:aws:acm:us-east-1:000000000000:certificate/8f4c1b1e-2a1d-4a57-9c0e-1f7b2a3d4e5f"
        .to_string()
}

/// A GraphQL API of the client's own, created straight through AppSync, which is what a
/// template's mapping names. Answers its `apiId`.
async fn given_graphql_api(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, _, api) = appsync(
        router,
        "POST",
        "/v1/apis",
        Some(json!({ "name": name, "authenticationType": "API_KEY" })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from CreateGraphqlApi, got {status} {api}"
    );
    let graphql_api = api
        .get("graphqlApi")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected a graphqlApi in {api}"));
    member(case, &graphql_api, "apiId")
}

/// What one row declares and what the stack must then have created.
struct Row {
    case: &'static str,
    /// The template body, built from the stack name the row runs under (which is what makes
    /// the declared domain name unique per row) and the `apiId` of the API created for it.
    template: fn(&str, &str) -> String,
    /// The domain name the row declares, built from the stack name.
    domain: fn(&str) -> String,
    /// The `Description` the row declares for its domain, or `None` where it declares none.
    description: Option<&'static str>,
    /// The tags the row declares, each of which the created domain must carry.
    tags: &'static [(&'static str, &'static str)],
    /// Whether the row declares the mapping of its API onto the domain.
    associated: bool,
    /// The attributes the row publishes as outputs, as `(output key, the member AppSync
    /// reports that value under in its `domainNameConfig`)`.
    domain_outputs: &'static [(&'static str, &'static str)],
}

/// The three-label domain name most rows declare: one DNS name per stack, so two rows never
/// collide.
fn domain_of(stack: &str) -> String {
    format!("{stack}.example.test")
}

/// A four-label name with a hyphen in one of them, which the modelled `DomainName` pattern
/// admits as much as the three-label form.
fn deep_domain_of(stack: &str) -> String {
    format!("graphql-api.{stack}.example.test")
}

const ROWS: &[Row] = &[
    Row {
        case: "a domain with a description and two tags, and a mapping naming it literally",
        template: |stack, api_id| {
            json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Resources": {
                    "Domain": {
                        "Type": "AWS::AppSync::DomainName",
                        "Properties": {
                            "DomainName": domain_of(stack),
                            "CertificateArn": certificate_arn(),
                            "Description": "the orders graph",
                            "Tags": [
                                { "Key": "team", "Value": "payments" },
                                { "Key": "env", "Value": "staging" }
                            ]
                        }
                    },
                    "Association": {
                        "Type": "AWS::AppSync::DomainNameApiAssociation",
                        "Properties": {
                            "DomainName": domain_of(stack),
                            "ApiId": api_id
                        }
                    }
                },
                "Outputs": {
                    "DomainName": { "Value": { "Ref": "Domain" } },
                    "AppSyncDomainName": {
                        "Value": { "Fn::GetAtt": ["Domain", "AppSyncDomainName"] }
                    },
                    "HostedZoneId": { "Value": { "Fn::GetAtt": ["Domain", "HostedZoneId"] } },
                    "DomainNameArn": { "Value": { "Fn::GetAtt": ["Domain", "DomainNameArn"] } },
                    "ApiAssociationIdentifier": {
                        "Value": { "Fn::GetAtt": ["Association", "ApiAssociationIdentifier"] }
                    },
                    "AssociationRef": { "Value": { "Ref": "Association" } }
                }
            })
            .to_string()
        },
        domain: domain_of,
        description: Some("the orders graph"),
        tags: &[("team", "payments"), ("env", "staging")],
        associated: true,
        domain_outputs: &[
            ("AppSyncDomainName", "appsyncDomainName"),
            ("DomainNameArn", "domainNameArn"),
            ("HostedZoneId", "hostedZoneId"),
        ],
    },
    Row {
        case: "a four-label domain with no description or tags, whose mapping names it by \
               Ref, in YAML with the short-form tags",
        template: |stack, api_id| {
            format!(
                "\
AWSTemplateFormatVersion: '2010-09-09'
Description: an AppSync custom domain and the api mapped onto it
Resources:
  Domain:
    Type: AWS::AppSync::DomainName
    Properties:
      DomainName: {domain}
      CertificateArn: {certificate}
  Association:
    Type: AWS::AppSync::DomainNameApiAssociation
    Properties:
      DomainName: !Ref Domain
      ApiId: {api_id}
Outputs:
  DomainName:
    Value: !Ref Domain
  AppSyncDomainName:
    Value: !GetAtt Domain.AppSyncDomainName
  ApiAssociationIdentifier:
    Value: !GetAtt Association.ApiAssociationIdentifier
  AssociationRef:
    Value: !Ref Association
",
                domain = deep_domain_of(stack),
                certificate = certificate_arn(),
            )
        },
        domain: deep_domain_of,
        description: None,
        tags: &[],
        associated: true,
        domain_outputs: &[("AppSyncDomainName", "appsyncDomainName")],
    },
    Row {
        case: "a domain alone, which no template maps an api onto",
        template: |stack, _api_id| {
            json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Resources": {
                    "Domain": {
                        "Type": "AWS::AppSync::DomainName",
                        "Properties": {
                            "DomainName": domain_of(stack),
                            "CertificateArn": certificate_arn(),
                            "Description": "unmapped"
                        }
                    }
                },
                "Outputs": {
                    "DomainName": { "Value": { "Ref": "Domain" } },
                    "HostedZoneId": { "Value": { "Fn::GetAtt": ["Domain", "HostedZoneId"] } },
                    "DomainNameArn": { "Value": { "Fn::GetAtt": ["Domain", "DomainNameArn"] } }
                }
            })
            .to_string()
        },
        domain: domain_of,
        description: Some("unmapped"),
        tags: &[],
        associated: false,
        domain_outputs: &[
            ("DomainNameArn", "domainNameArn"),
            ("HostedZoneId", "hostedZoneId"),
        ],
    },
    Row {
        case: "a domain with one tag and a mapping, publishing only the endpoint it is \
               served through",
        template: |stack, api_id| {
            json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Resources": {
                    "Domain": {
                        "Type": "AWS::AppSync::DomainName",
                        "Properties": {
                            "DomainName": deep_domain_of(stack),
                            "CertificateArn": certificate_arn(),
                            "Tags": [{ "Key": "tier", "Value": "edge-api" }]
                        }
                    },
                    "Association": {
                        "Type": "AWS::AppSync::DomainNameApiAssociation",
                        "Properties": {
                            "DomainName": { "Ref": "Domain" },
                            "ApiId": api_id
                        }
                    }
                },
                "Outputs": {
                    "DomainName": { "Value": { "Ref": "Domain" } },
                    "AppSyncDomainName": {
                        "Value": { "Fn::GetAtt": ["Domain", "AppSyncDomainName"] }
                    },
                    "AssociationRef": { "Value": { "Ref": "Association" } },
                    "ApiAssociationIdentifier": {
                        "Value": { "Fn::GetAtt": ["Association", "ApiAssociationIdentifier"] }
                    }
                }
            })
            .to_string()
        },
        domain: deep_domain_of,
        description: None,
        tags: &[("tier", "edge-api")],
        associated: true,
        domain_outputs: &[("AppSyncDomainName", "appsyncDomainName")],
    },
];

/// The stack's body at the FIRST observation that reports it complete, which is what every
/// assertion about a provisioned resource is made against.
async fn awaited_complete(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let mut last = String::new();
    for _ in 0..POLLS {
        let body = describe_stack(router, case, stack).await;
        match xml_text(&body, "StackStatus").as_deref() {
            Some(CREATE_COMPLETE) => return body,
            Some(CREATE_IN_PROGRESS) => {}
            other => panic!(
                "case {case:?}: expected the stack to be creating or complete, got {other:?} \
                 in {body}"
            ),
        }
        last = body;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: the stack never completed; the last answer was {last}")
}

/// The custom domain AppSync holds under `domain_name`, read the way a client reads it.
async fn get_domain_name(
    router: &MultiServiceRouter,
    domain_name: &str,
) -> (u16, Option<String>, Value) {
    appsync(
        router,
        "GET",
        &format!("/v1/domainnames/{domain_name}"),
        None,
    )
    .await
}

/// The mapping AppSync holds on `domain_name`, read the way a client reads it.
async fn get_api_association(
    router: &MultiServiceRouter,
    domain_name: &str,
) -> (u16, Option<String>, Value) {
    appsync(
        router,
        "GET",
        &format!("/v1/domainnames/{domain_name}/apiassociation"),
        None,
    )
    .await
}

/// The `domainNameConfig` of a `GetDomainName` answer.
fn domain_name_config(case: &str, answered: &Value) -> Value {
    answered
        .get("domainNameConfig")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected a domainNameConfig in {answered}"))
}

#[tokio::test]
async fn a_stack_creates_the_appsync_custom_domains_it_declares() {
    let router = build_router();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let stack_name = format!("appsync-domain-{index}");
        let declared = (row.domain)(&stack_name);
        let api_id = given_graphql_api(&router, case, &format!("{stack_name}-api")).await;
        create_stack(
            &router,
            case,
            &stack_name,
            &(row.template)(&stack_name, &api_id),
        )
        .await;

        let body = awaited_complete(&router, case, &stack_name).await;
        let outputs = every_output_has_a_value(case, &body);

        // The domain the template declared exists in AppSync, and the stack's `Ref` output is
        // the name a client reads it back by.
        assert_eq!(
            outputs.get("DomainName"),
            Some(&declared),
            "case {case:?}: expected the Ref of the domain to be the declared name, got \
             {outputs:?}"
        );
        let (status, _, answered) = get_domain_name(&router, &declared).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetDomainName for {declared:?}, got {status} \
             {answered}"
        );
        let config = domain_name_config(case, &answered);
        assert_eq!(
            member(case, &config, "domainName"),
            declared,
            "case {case:?}: expected the declared domain name, got {config}"
        );

        // Every value the declaration carried reached AppSync.
        assert_eq!(
            member(case, &config, "certificateArn"),
            certificate_arn(),
            "case {case:?}: expected the declared certificate, got {config}"
        );
        assert_eq!(
            config.get("description").and_then(Value::as_str),
            row.description,
            "case {case:?}: expected the declared description {:?}, got {config}",
            row.description
        );
        for (key, value) in row.tags {
            assert_eq!(
                config.get("tags").and_then(|tags| tags.get(*key)),
                Some(&Value::String((*value).to_string())),
                "case {case:?}: expected the declared tag {key}={value} on the domain, got \
                 {config}"
            );
        }
        // The reserved namespace is AWS's own, and `TagKey` refuses it: the records
        // CloudFormation keeps of its own accord are not sent to be refused.
        if let Some(Value::Object(tags)) = config.get("tags") {
            let reserved: Vec<&String> = tags
                .keys()
                .filter(|key| key.to_ascii_lowercase().starts_with("aws:"))
                .collect();
            assert!(
                reserved.is_empty(),
                "case {case:?}: expected no reserved tag key on the domain, got {reserved:?}"
            );
        }

        // The values AppSync itself assigned the domain are the ones the stack published:
        // read off that service's own report rather than computed here.
        for (output, reported) in row.domain_outputs {
            let assigned = member(case, &config, reported);
            assert_eq!(
                outputs.get(*output),
                Some(&assigned),
                "case {case:?}: expected the output {output} to be the {reported} AppSync \
                 assigned ({assigned:?}), got {outputs:?}"
            );
        }

        let (status, error_type, association) = get_api_association(&router, &declared).await;
        if row.associated {
            // The stack was not reported complete before AppSync had deployed the mapping, so
            // the first completed observation reads it successful.
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200 from GetApiAssociation for {declared:?}, got \
                 {status} {association}"
            );
            let reported = association
                .get("apiAssociation")
                .cloned()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected an apiAssociation in {association}")
                });
            assert_eq!(
                member(case, &reported, "apiId"),
                api_id,
                "case {case:?}: expected the mapping to name the declared api, got {reported}"
            );
            assert_eq!(
                member(case, &reported, "associationStatus"),
                SUCCESS,
                "case {case:?}: expected the mapping deployed once the stack completed, got \
                 {reported}"
            );
            assert_eq!(
                member(case, &reported, "domainName"),
                declared,
                "case {case:?}: expected the mapping to be reported on the declared domain, \
                 got {reported}"
            );
            // The mapping's identity is the domain it was made on, which is what the stack
            // publishes for it and the only handle AppSync addresses one by.
            assert_eq!(
                outputs.get("ApiAssociationIdentifier"),
                Some(&declared),
                "case {case:?}: expected the mapping's identifier to be the domain it was \
                 made on, got {outputs:?}"
            );
            assert_eq!(
                outputs.get("AssociationRef"),
                Some(&declared),
                "case {case:?}: expected the Ref of the mapping to be its identifier, got \
                 {outputs:?}"
            );
        } else {
            // Nothing mapped an API onto this domain, so the read is the service's own
            // refusal rather than a mapping the stack invented.
            assert_eq!(
                status, 404,
                "case {case:?}: expected GetApiAssociation to refuse an unmapped domain, got \
                 {status} {association}"
            );
            assert_eq!(
                error_type.as_deref(),
                Some(NOT_FOUND),
                "case {case:?}: expected the modelled not-found code, got {error_type:?} \
                 {association}"
            );
            assert_eq!(
                association.get("message").and_then(Value::as_str),
                Some(NOT_ASSOCIATED),
                "case {case:?}: expected the service's own not-associated message, got \
                 {association}"
            );
            assert!(
                !outputs.contains_key("ApiAssociationIdentifier"),
                "case {case:?}: a stack declaring no mapping publishes none, got {outputs:?}"
            );
        }
    }
}

/// A stack's mapping is a real association: a client can take it off through AppSync's own
/// `DisassociateApi` and the domain the stack created then reports no mapping at all, which
/// is how a client can tell the stack mapped the API rather than reporting that it had.
#[tokio::test]
async fn the_stacks_mapping_is_the_services_own_association() {
    let router = build_router();
    let case = "a stack-made mapping a client then takes off";
    let stack_name = "appsync-domain-disassociated";
    let declared = domain_of(stack_name);
    let api_id = given_graphql_api(&router, case, "disassociated-api").await;
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Domain": {
                "Type": "AWS::AppSync::DomainName",
                "Properties": {
                    "DomainName": declared,
                    "CertificateArn": certificate_arn()
                }
            },
            "Association": {
                "Type": "AWS::AppSync::DomainNameApiAssociation",
                "Properties": { "DomainName": { "Ref": "Domain" }, "ApiId": api_id }
            }
        },
        "Outputs": { "DomainName": { "Value": { "Ref": "Domain" } } }
    })
    .to_string();
    create_stack(&router, case, stack_name, &template).await;
    awaited_complete(&router, case, stack_name).await;

    let (status, _, _) = appsync(
        &router,
        "DELETE",
        &format!("/v1/domainnames/{declared}/apiassociation"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from DisassociateApi on the stack's domain, got {status}"
    );

    let (status, error_type, association) = get_api_association(&router, &declared).await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected the domain to report no mapping once it was taken off, got \
         {status} {association}"
    );
    assert_eq!(
        error_type.as_deref(),
        Some(NOT_FOUND),
        "case {case:?}: expected the modelled not-found code, got {error_type:?} {association}"
    );
    // The DOMAIN is still the stack's, so the mapping is what was taken off and nothing else.
    let (status, _, answered) = get_domain_name(&router, &declared).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the domain itself to survive the disassociation, got \
         {status} {answered}"
    );
}

/// Deleting the stack deletes what it created: the domain and its mapping stop existing in
/// AppSync, which is how a client can tell the stack really owned them — and the read that
/// no longer resolves is the service's own modelled refusal.
#[tokio::test]
async fn deleting_the_stack_deletes_the_domain_it_created() {
    let router = build_router();
    let case = "a deleted appsync custom-domain stack";
    let stack_name = "appsync-domain-deleted";
    let declared = domain_of(stack_name);
    let api_id = given_graphql_api(&router, case, "deleted-api").await;
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Domain": {
                "Type": "AWS::AppSync::DomainName",
                "Properties": {
                    "DomainName": declared,
                    "CertificateArn": certificate_arn(),
                    "Description": "deleted with its stack"
                }
            },
            "Association": {
                "Type": "AWS::AppSync::DomainNameApiAssociation",
                "Properties": { "DomainName": { "Ref": "Domain" }, "ApiId": api_id }
            }
        },
        "Outputs": { "DomainName": { "Value": { "Ref": "Domain" } } }
    })
    .to_string();
    create_stack(&router, case, stack_name, &template).await;
    awaited_complete(&router, case, stack_name).await;

    let (status, _, answered) = get_domain_name(&router, &declared).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the stack's domain must exist before the stack is deleted, got \
         {status} {answered}"
    );

    let (status, deleted) =
        cloudformation(&router, "DeleteStack", &[("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {deleted}"
    );
    // The stack's deletion is the clock's to make due, so the read a client makes next is
    // asked until it answers for a domain nothing holds.
    let mut answer = get_domain_name(&router, &declared).await;
    for _ in 0..POLLS {
        if answer.0 != 200 {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
        answer = get_domain_name(&router, &declared).await;
    }
    let (status, error_type, gone) = answer;
    assert_eq!(
        status, 404,
        "case {case:?}: expected GetDomainName to report the deleted domain gone, got \
         {status} {gone}"
    );
    assert_eq!(
        error_type.as_deref(),
        Some(NOT_FOUND),
        "case {case:?}: expected the modelled not-found code, got {error_type:?} {gone}"
    );
    assert_eq!(
        gone.get("message").and_then(Value::as_str),
        Some(NO_SUCH_DOMAIN_NAME),
        "case {case:?}: expected the service's own not-found message, got {gone}"
    );
    // The API the mapping named is the client's, not the stack's, so it is still there.
    let (status, _, api) = appsync(&router, "GET", &format!("/v1/apis/{api_id}"), None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the api the mapping named to survive the stack, got \
         {status} {api}"
    );
}
