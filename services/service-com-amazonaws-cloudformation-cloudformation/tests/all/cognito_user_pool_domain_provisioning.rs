// SPDX-License-Identifier: CC0-1.0

//! What a stack deploys for an `AWS::Cognito::UserPoolDomain`, end to end over the awsQuery
//! wire with the owning service mounted beside CloudFormation.
//!
//! RULE under test: **a resource type this emulator catalogues and a stack reports created is
//! created in the service that OWNS it** — so a declared user-pool domain is created through
//! `CreateUserPoolDomain`, and once the stack reports `CREATE_COMPLETE` a client holding
//! nothing but the stack's own `Ref` reads that domain back out of Amazon Cognito user pools:
//! `DescribeUserPoolDomain` resolves it, reports the pool it belongs to, the branding version
//! the declaration asked for, and the `ACTIVE` status that is the documented readiness signal
//! of a domain (`DomainDescriptionType.Status`). A domain nothing created would answer that
//! read with an EMPTY `DomainDescription`, which is what the delete half asserts.
//!
//! The rows differ along what the rule generalises over — the DECLARATION of the domain: the
//! domain string the template states (a plain prefix, one carrying digits and hyphens, a long
//! one), a declaration that states none and leaves CloudFormation to name the resource, and
//! the managed-login branding version it asks for (none, hosted UI classic, managed login).
//! Each row's domain belongs to a pool of its own, because a pool holds at most one prefix
//! domain.
//!
//! `cognito_user_pool_client_identity.rs` is the suite of an app client's published identity
//! and `cognito_resource_types.rs` of which Amazon Cognito types a template may declare at
//! all; this one is about the domain really existing in the owning service.

use serde_json::{json, Map, Value};
use service_com_amazonaws_cloudformation_cloudformation::state::status;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::REGION;
use crate::stack_lifecycle::{
    call, create_stack, member, output_value, resource_event_reason, router, xml_text,
};
use crate::stack_owned_secret_teardown::poll_to_terminal;

/// The resource types this suite deploys.
const USER_POOL: &str = "AWS::Cognito::UserPool";
const USER_POOL_DOMAIN: &str = "AWS::Cognito::UserPoolDomain";

/// The signing name and target prefix of the owning service, as its own model declares them
/// (`smithy_query(query="service_info")`). It speaks `awsJson1_1`.
const COGNITO_IDP: (&str, &str) = ("cognito-idp", "AWSCognitoIdentityProviderService");

/// The status a domain reports once it serves the pool's managed login pages, which is the
/// readiness signal a client waits for (`DomainDescriptionType.Status`, one of the modelled
/// `DomainStatusType` values).
const ACTIVE: &str = "ACTIVE";

/// The branding version `CreateUserPoolDomain` applies when a request states none: "a value of
/// 1 indicates hosted UI (classic) and a version of 2 indicates managed login"
/// (`CreateUserPoolDomain.ManagedLoginVersion`), and 1 is the one a domain comes up with.
const DEFAULT_BRANDING_VERSION: i64 = 1;

/// One declared domain: how the template declares it and what the stack must then publish and
/// leave in the owning service.
struct Row {
    case: &'static str,
    /// The logical id of the domain, whose pool is this id with `Pool` appended.
    logical_id: &'static str,
    /// The `Domain` the declaration states, or `None` to leave CloudFormation to name it.
    declared_domain: Option<&'static str>,
    /// The `ManagedLoginVersion` the declaration states, or `None` to leave it out.
    branding_version: Option<i64>,
}

/// The rows: the forms a declaration of a user-pool domain takes.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a plain prefix the template names, with no branding version",
            logical_id: "PlainDomain",
            declared_domain: Some("ci-cfn-plain-domain"),
            branding_version: None,
        },
        Row {
            case: "a prefix carrying digits and hyphens, asking for hosted UI classic",
            logical_id: "DigitsDomain",
            declared_domain: Some("ci-cfn-domain-42-auth"),
            branding_version: Some(1),
        },
        Row {
            case: "a prefix asking for managed login",
            logical_id: "ManagedLoginDomain",
            declared_domain: Some("ci-cfn-managed-login-domain"),
            branding_version: Some(2),
        },
        Row {
            case: "a long prefix, inside the 63 characters DomainType admits",
            logical_id: "LongDomain",
            declared_domain: Some(
                "ci-cfn-domain-with-a-deliberately-long-prefix-for-this-row",
            ),
            branding_version: None,
        },
        Row {
            case: "a declaration that states no domain, so CloudFormation names it",
            logical_id: "UnnamedDomain",
            declared_domain: None,
            branding_version: None,
        },
    ]
}

/// The logical id of the pool a row's domain belongs to.
fn pool_of(logical_id: &str) -> String {
    format!("{logical_id}Pool")
}

/// The output key a row's `Ref` is published under, its pool's id, and its CloudFront
/// endpoint.
fn ref_key(logical_id: &str) -> String {
    format!("Ref{logical_id}")
}

fn pool_key(logical_id: &str) -> String {
    format!("Pool{logical_id}")
}

fn distribution_key(logical_id: &str) -> String {
    format!("Distribution{logical_id}")
}

/// The template the table deploys: one user pool per row, the row's domain in it, and the
/// three values the stack publishes for each — the domain's `Ref`, the pool's id and the
/// `CloudFrontDistribution` the type's attribute names.
fn table_template(rows: &[Row]) -> String {
    let mut resources = Map::new();
    let mut outputs = Map::new();
    for row in rows {
        let pool = pool_of(row.logical_id);
        resources.insert(
            pool.clone(),
            json!({
                "Type": USER_POOL,
                "Properties": {
                    "UserPoolName": format!("ci-{}-pool", row.logical_id.to_ascii_lowercase()),
                    "MfaConfiguration": "OFF"
                }
            }),
        );
        let mut properties = Map::new();
        properties.insert("UserPoolId".to_string(), json!({"Ref": pool.clone()}));
        if let Some(domain) = row.declared_domain {
            properties.insert("Domain".to_string(), json!(domain));
        }
        if let Some(version) = row.branding_version {
            properties.insert("ManagedLoginVersion".to_string(), json!(version));
        }
        resources.insert(
            row.logical_id.to_string(),
            json!({"Type": USER_POOL_DOMAIN, "Properties": Value::Object(properties)}),
        );
        outputs.insert(
            ref_key(row.logical_id),
            json!({"Value": {"Ref": row.logical_id}}),
        );
        outputs.insert(pool_key(row.logical_id), json!({"Value": {"Ref": pool}}));
        outputs.insert(
            distribution_key(row.logical_id),
            json!({"Value": {"Fn::GetAtt": [row.logical_id, "CloudFrontDistribution"]}}),
        );
    }
    json!({"Resources": resources, "Outputs": outputs}).to_string()
}

/// One awsJson call to the owning service over the same router, signed the way a client's SDK
/// signs it: the HTTP status and the parsed answer.
async fn aws_json(
    router: &MultiServiceRouter,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let (signing_name, target_prefix) = COGNITO_IDP;
    let http_request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(request.to_string()))
        .expect("a well-formed awsJson request");
    let response = router.dispatch(http_request).await;
    let status_code = response.status().as_u16();
    let body = response.into_body().to_vec();
    let document: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status_code, document)
}

/// Whether `candidate` is a value `DomainType` admits: 1..63 characters of lowercase
/// alphanumerics and hyphens, beginning and ending with an alphanumeric.
///
/// Source: `DomainType` in the cognito-idp model, `@length(min: 1, max: 63)` with pattern
/// `^[a-z0-9](?:[a-z0-9\-]{0,61}[a-z0-9])?$`
/// (`smithy_query(query="shape",shape="DomainType")`).
fn is_a_domain(candidate: &str) -> bool {
    let inside = |character: char| character.is_ascii_lowercase() || character.is_ascii_digit();
    !candidate.is_empty()
        && candidate.len() <= 63
        && candidate.starts_with(inside)
        && candidate.ends_with(inside)
        && candidate
            .chars()
            .all(|character| inside(character) || character == '-')
}

/// Every declared domain exists in the owning service once the stack completes: the stack
/// publishes the domain string as its `Ref`, and that value alone reads the domain back out of
/// Amazon Cognito, `ACTIVE`, in the pool the declaration named and at the branding version it
/// asked for.
#[tokio::test]
async fn a_declared_user_pool_domain_is_created_in_the_owning_service() {
    let router = router();
    let rows = rows();
    let stack_name = "cfn-user-pool-domain";
    let (status_code, body) = create_stack(&router, stack_name, &table_template(&rows), &[]).await;
    assert_eq!(
        status_code, 200,
        "expected HTTP 200 from CreateStack, got {status_code} with body {body}"
    );
    let described = poll_to_terminal(&router, "the deployment", stack_name).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::CREATE_COMPLETE),
        "expected the stack declaring a domain per pool to reach {:?}, got body {described}; \
         the first domain's failure was {:?}",
        status::CREATE_COMPLETE,
        resource_event_reason(&router, stack_name, "PlainDomain", "CREATE_FAILED").await
    );

    for row in &rows {
        let case = row.case;
        let published = output_value(&described, &ref_key(row.logical_id)).unwrap_or_else(|| {
            panic!("case {case:?}: expected the stack to publish Ref of the domain, got {described}")
        });
        let pool_id = output_value(&described, &pool_key(row.logical_id)).unwrap_or_default();
        match row.declared_domain {
            // A declared domain IS the resource's identity, so the published value is it.
            Some(declared) => assert_eq!(
                published, declared,
                "case {case:?}: expected Ref to be the declared domain, got {published:?}"
            ),
            // A declaration naming none is created under the name CloudFormation computed,
            // which still has to be a value the owning service's own `Domain` admits.
            None => assert!(
                is_a_domain(&published),
                "case {case:?}: expected the computed name to be a value DomainType admits, \
                 got {published:?}"
            ),
        }

        // The round trip: a client holding nothing but the published value reads the domain
        // back out of the owning service by it.
        let (status_code, answer) =
            aws_json(&router, "DescribeUserPoolDomain", &json!({"Domain": published})).await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected DescribeUserPoolDomain({published}) — driven by nothing \
             but the published Ref — to be served, got {status_code} with {answer}"
        );
        let domain = &answer["DomainDescription"];
        assert_eq!(
            domain["Domain"].as_str(),
            Some(published.as_str()),
            "case {case:?}: expected the owning service to hold the domain the stack \
             published, got {domain}"
        );
        assert_eq!(
            domain["UserPoolId"].as_str(),
            Some(pool_id.as_str()),
            "case {case:?}: expected the domain to belong to the pool the declaration named, \
             got {domain}"
        );
        assert_eq!(
            domain["Status"].as_str(),
            Some(ACTIVE),
            "case {case:?}: expected the deployed domain to report the readiness signal \
             {ACTIVE:?}, got {domain}"
        );
        assert_eq!(
            domain["ManagedLoginVersion"].as_i64(),
            Some(row.branding_version.unwrap_or(DEFAULT_BRANDING_VERSION)),
            "case {case:?}: expected the branding version the declaration asked for, got \
             {domain}"
        );

        // The attribute the type publishes is the endpoint the domain is served through,
        // which the owning service reports for the domain the stack created.
        let distribution =
            output_value(&described, &distribution_key(row.logical_id)).unwrap_or_default();
        assert_eq!(
            Some(distribution.as_str()),
            domain["CloudFrontDistribution"].as_str(),
            "case {case:?}: expected the published CloudFrontDistribution to be the endpoint \
             the owning service reports, got {distribution:?} against {domain}"
        );
        assert!(
            !distribution.is_empty(),
            "case {case:?}: expected a published CloudFrontDistribution, got {distribution:?}"
        );

        // The pool itself reports the prefix domain it now has, which is how a client of the
        // pool finds it without reading the stack at all.
        let (status_code, pool) =
            aws_json(&router, "DescribeUserPool", &json!({"UserPoolId": pool_id})).await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected DescribeUserPool({pool_id}) to be served, got \
             {status_code} with {pool}"
        );
        assert_eq!(
            pool["UserPool"]["Domain"].as_str(),
            Some(published.as_str()),
            "case {case:?}: expected the pool to report the domain the stack created for it, \
             got {pool}"
        );
    }
}

/// A hosted-UI stack of the shape a client deploys one in: a pool with its sign-in
/// configuration, an app client wired for the authorization-code flow, and the domain the
/// managed login pages are served on — deployed together, which is how the domain's create
/// meets a pool that the same deployment minted.
#[tokio::test]
async fn a_hosted_ui_stack_deploys_its_pool_client_and_domain_together() {
    let router = router();
    let stack_name = "cfn-hosted-ui-stack";
    let domain = "ci-cfn-hosted-ui";
    let template = json!({
        "Resources": {
            "Pool": {
                "Type": USER_POOL,
                "Properties": {
                    "UserPoolName": "ci-hosted-ui-pool",
                    "AutoVerifiedAttributes": ["email"],
                    "MfaConfiguration": "OFF",
                    "Policies": {"PasswordPolicy": {"MinimumLength": 8}},
                    "Schema": [{
                        "Name": "email",
                        "AttributeDataType": "String",
                        "Mutable": true,
                        "Required": true
                    }]
                }
            },
            "Client": {
                "Type": "AWS::Cognito::UserPoolClient",
                "Properties": {
                    "UserPoolId": {"Ref": "Pool"},
                    "ClientName": "ci-hosted-ui-client",
                    "GenerateSecret": true,
                    "AllowedOAuthFlows": ["code"],
                    "AllowedOAuthScopes": ["openid", "email"],
                    "AllowedOAuthFlowsUserPoolClient": true,
                    "CallbackURLs": ["https://app.example.com/callback"],
                    "LogoutURLs": ["https://app.example.com/signout"],
                    "SupportedIdentityProviders": ["COGNITO"],
                    "ExplicitAuthFlows": ["ALLOW_USER_PASSWORD_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"]
                }
            },
            "Domain": {
                "Type": USER_POOL_DOMAIN,
                "Properties": {
                    "UserPoolId": {"Ref": "Pool"},
                    "Domain": domain,
                    "ManagedLoginVersion": 2
                }
            }
        },
        "Outputs": {
            "PoolId": {"Value": {"Ref": "Pool"}},
            "DomainRef": {"Value": {"Ref": "Domain"}}
        }
    })
    .to_string();

    let (status_code, body) = create_stack(&router, stack_name, &template, &[]).await;
    assert_eq!(
        status_code, 200,
        "expected HTTP 200 from CreateStack, got {status_code} with body {body}"
    );
    let described = poll_to_terminal(&router, "the deployment", stack_name).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::CREATE_COMPLETE),
        "expected the hosted-UI stack to reach {:?}, got body {described}; the domain's \
         failure was {:?} and the client's was {:?}",
        status::CREATE_COMPLETE,
        resource_event_reason(&router, stack_name, "Domain", "CREATE_FAILED").await,
        resource_event_reason(&router, stack_name, "Client", "CREATE_FAILED").await
    );

    // The pool really holds the domain, at the branding version the declaration asked for.
    let pool_id = output_value(&described, "PoolId").unwrap_or_default();
    let (status_code, answer) =
        aws_json(&router, "DescribeUserPoolDomain", &json!({"Domain": domain})).await;
    assert_eq!(
        status_code, 200,
        "expected DescribeUserPoolDomain to be served, got {status_code} with {answer}"
    );
    assert_eq!(
        (
            answer["DomainDescription"]["UserPoolId"].as_str(),
            answer["DomainDescription"]["Status"].as_str(),
            answer["DomainDescription"]["ManagedLoginVersion"].as_i64(),
        ),
        (Some(pool_id.as_str()), Some(ACTIVE), Some(2)),
        "expected the deployed domain to belong to the stack's pool, be {ACTIVE:?} and carry \
         the declared branding version, got {answer}"
    );
}

/// Deleting the stack removes the domain from the owning service: the read that resolved it
/// answers the empty description a domain this emulator holds none of earns.
#[tokio::test]
async fn deleting_the_stack_removes_the_domain_from_the_owning_service() {
    let router = router();
    let stack_name = "cfn-user-pool-domain-teardown";
    let domain = "ci-cfn-domain-teardown";
    let template = json!({
        "Resources": {
            "Pool": {
                "Type": USER_POOL,
                "Properties": {"UserPoolName": "ci-domain-teardown-pool", "MfaConfiguration": "OFF"}
            },
            "Domain": {
                "Type": USER_POOL_DOMAIN,
                "Properties": {"UserPoolId": {"Ref": "Pool"}, "Domain": domain}
            }
        },
        "Outputs": {"RefDomain": {"Value": {"Ref": "Domain"}}}
    })
    .to_string();

    let (status_code, body) = create_stack(&router, stack_name, &template, &[]).await;
    assert_eq!(
        status_code, 200,
        "expected HTTP 200 from CreateStack, got {status_code} with body {body}"
    );
    let described = poll_to_terminal(&router, "the deployment", stack_name).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::CREATE_COMPLETE),
        "expected the stack to reach {:?}, got body {described}",
        status::CREATE_COMPLETE
    );
    assert_eq!(
        output_value(&described, "RefDomain").as_deref(),
        Some(domain),
        "expected the stack to publish the declared domain, got body {described}"
    );

    let (status_code, body) = call(&router, "DeleteStack", &[member("StackName", stack_name)]).await;
    assert_eq!(
        status_code, 200,
        "expected HTTP 200 from DeleteStack, got {status_code} with body {body}"
    );
    // A stack the teardown has removed is readable by its STACK ID alone, so the id is read
    // off the deployment's own description before the delete is waited for.
    let stack_id = xml_text(&described, "StackId").unwrap_or_default();
    let described = poll_to_terminal(&router, "the teardown", &stack_id).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::DELETE_COMPLETE),
        "expected the stack to reach {:?}, got body {described}",
        status::DELETE_COMPLETE
    );

    // The domain is gone: that read reports the absence by answering an empty description
    // rather than refusing, so there is no `Domain` member left in it.
    let (status_code, answer) =
        aws_json(&router, "DescribeUserPoolDomain", &json!({"Domain": domain})).await;
    assert_eq!(
        status_code, 200,
        "expected DescribeUserPoolDomain to answer after the teardown, got {status_code} with \
         {answer}"
    );
    assert_eq!(
        answer["DomainDescription"].get("Domain"),
        None,
        "expected the torn-down domain to resolve to nothing, got {answer}"
    );
}
