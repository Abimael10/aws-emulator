// SPDX-License-Identifier: CC0-1.0

//! What a stack publishes for an `AWS::Cognito::UserPoolClient`, end to end over the
//! awsQuery wire with the owning service mounted beside CloudFormation.
//!
//! RULE under test: **the physical resource id of a resource whose identity the OWNING
//! service assigns is the identifier that service's own create answered with** — so for an
//! app client it is the `ClientId` `CreateUserPoolClient` minted (`ClientIdType`, length
//! 1..128, `@pattern ^[\w+]+$`, `smithy_query(query="shape",shape="ClientIdType")`), never a
//! name CloudFormation computed from the stack name and the logical id. Two things follow,
//! and both are asserted here: the published value is one the owning service's own calls
//! take, so `DescribeUserPoolClient` resolves the app client by nothing but the stack's
//! `Ref`; and a resource DOWNSTREAM that consumes it is created with it — an identity
//! pool's `CognitoIdentityProviders[].ClientId` targets `CognitoIdentityProviderClientId`,
//! length 1..128, `@pattern ^[\w_]+$`
//! (`smithy_query(query="shape",shape="CognitoIdentityProviderClientId")`), which a
//! hyphenated generated name cannot satisfy, so a stack wiring one into an
//! `AWS::Cognito::IdentityPool` reaches `CREATE_COMPLETE`.
//!
//! The rows differ along what the rule generalises over — the DECLARATION of the app client
//! whose identity is published: a client the template names and one it leaves CloudFormation
//! to name, a client that asks the owning service to generate a secret and ones that do not,
//! and clients configured with the members a declaration may carry beside the name (explicit
//! auth flows, token validities, read/write attributes). Every row's app client is also fed
//! into the identity pool of the same stack, so the downstream half of the rule is asserted
//! once per row. A second table re-declares one app client three ways over `UpdateStack`,
//! since an identifier the owning service assigned is carried FORWARD rather than recomputed:
//! the update is applied to the client that exists and the published id does not move. The
//! ids are per-run, so what a row asserts is the FORM, the round trip and the wiring, never a
//! recorded value.
//!
//! `cognito_user_pool_return_values.rs` is the suite of the POOL's published attributes and
//! `cognito_principal_tag_identity.rs` of a principal-tag mapping's composite identifier;
//! this one is about the app client's own identity and what consumes it.

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};
use service_com_amazonaws_cloudformation_cloudformation::state::status;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::REGION;
use crate::stack_lifecycle::{
    acknowledging_iam, call, create_stack, member, output_value, resource_event_reason, router,
    xml_text,
};
use crate::stack_owned_secret_teardown::poll_to_terminal;
use crate::stack_resources::{resources_of, wire_resources};

/// The resource types this suite deploys.
const USER_POOL: &str = "AWS::Cognito::UserPool";
const USER_POOL_CLIENT: &str = "AWS::Cognito::UserPoolClient";
const IDENTITY_POOL: &str = "AWS::Cognito::IdentityPool";

/// The logical ids of the two resources every row's client is declared against.
const POOL_LOGICAL_ID: &str = "UserPool";
const IDENTITY_POOL_LOGICAL_ID: &str = "IdentityPool";

/// The awsJson version both Amazon Cognito services declare (`awsJson1_1`).
const AWS_JSON_1_1: &str = "application/x-amz-json-1.1";

/// The signing names and target prefixes of the two owning services, as their own models
/// declare them (`smithy_query(query="service_info")`).
const COGNITO_IDP: (&str, &str) = ("cognito-idp", "AWSCognitoIdentityProviderService");
const COGNITO_IDENTITY: (&str, &str) = ("cognito-identity", "AWSCognitoIdentityService");

/// One declared app client: how the template declares it and what the stack must then
/// publish for it.
struct Row {
    case: &'static str,
    logical_id: &'static str,
    /// The properties the declaration states beside `UserPoolId`.
    declared: fn() -> Value,
    /// Whether the declaration asks the owning service to generate a client secret, which is
    /// what `Fn::GetAtt … ClientSecret` then publishes.
    generates_a_secret: bool,
}

/// The rows: every form a declaration of an app client takes, each one fed into the identity
/// pool of the same stack.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a client the template names, with no secret",
            logical_id: "NamedClient",
            declared: || json!({"ClientName": "ci-named-client", "GenerateSecret": false}),
            generates_a_secret: false,
        },
        Row {
            case: "a client CloudFormation names itself",
            logical_id: "UnnamedClient",
            declared: || json!({}),
            generates_a_secret: false,
        },
        Row {
            case: "a client whose secret the owning service generates",
            logical_id: "ConfidentialClient",
            declared: || json!({"ClientName": "ci-confidential-client", "GenerateSecret": true}),
            generates_a_secret: true,
        },
        Row {
            case: "a client configured with explicit auth flows",
            logical_id: "AuthFlowClient",
            declared: || {
                json!({
                    "ClientName": "ci-auth-flow-client",
                    "ExplicitAuthFlows": ["ALLOW_USER_PASSWORD_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"]
                })
            },
            generates_a_secret: false,
        },
        Row {
            case: "a client configured with token validities and attribute permissions",
            logical_id: "DetailedClient",
            declared: || {
                json!({
                    "ClientName": "ci-detailed-client",
                    "RefreshTokenValidity": 30,
                    "ReadAttributes": ["email"],
                    "WriteAttributes": ["email"]
                })
            },
            generates_a_secret: false,
        },
    ]
}

/// The output key a row's `Ref` is published under, and the one its `ClientSecret` is.
fn ref_key(logical_id: &str) -> String {
    format!("Ref{logical_id}")
}

fn secret_key(logical_id: &str) -> String {
    format!("Secret{logical_id}")
}

/// The template the table deploys: one user pool, every row's app client in it, and ONE
/// identity pool federating all of them by the `Ref` of each client — the documented wiring
/// whose `ClientId` the published value has to satisfy.
fn table_template(rows: &[Row]) -> String {
    let mut resources = Map::new();
    resources.insert(
        POOL_LOGICAL_ID.to_string(),
        json!({
            "Type": USER_POOL,
            "Properties": {"UserPoolName": "ci-client-identity-pool", "MfaConfiguration": "OFF"}
        }),
    );
    let mut outputs = Map::new();
    outputs.insert(
        POOL_LOGICAL_ID.to_string(),
        json!({"Value": {"Ref": POOL_LOGICAL_ID}}),
    );
    let mut providers = Vec::new();
    for row in rows {
        let mut properties = (row.declared)();
        let members = properties
            .as_object_mut()
            .expect("a row's declared properties are an object");
        members.insert(
            "UserPoolId".to_string(),
            json!({"Ref": POOL_LOGICAL_ID}),
        );
        resources.insert(
            row.logical_id.to_string(),
            json!({"Type": USER_POOL_CLIENT, "Properties": properties}),
        );
        outputs.insert(
            ref_key(row.logical_id),
            json!({"Value": {"Ref": row.logical_id}}),
        );
        if row.generates_a_secret {
            outputs.insert(
                secret_key(row.logical_id),
                json!({"Value": {"Fn::GetAtt": [row.logical_id, "ClientSecret"]}}),
            );
        }
        providers.push(json!({
            "ClientId": {"Ref": row.logical_id},
            "ProviderName": {"Fn::GetAtt": [POOL_LOGICAL_ID, "ProviderName"]}
        }));
    }
    resources.insert(
        IDENTITY_POOL_LOGICAL_ID.to_string(),
        json!({
            "Type": IDENTITY_POOL,
            "Properties": {
                "IdentityPoolName": "ci_client_identity_federation",
                "AllowUnauthenticatedIdentities": false,
                "CognitoIdentityProviders": Value::Array(providers)
            }
        }),
    );
    outputs.insert(
        IDENTITY_POOL_LOGICAL_ID.to_string(),
        json!({"Value": {"Ref": IDENTITY_POOL_LOGICAL_ID}}),
    );
    json!({"Resources": resources, "Outputs": outputs}).to_string()
}

/// One awsJson call to an owning service over the same router, signed the way a client's SDK
/// signs it: the HTTP status and the parsed answer.
async fn aws_json(
    router: &MultiServiceRouter,
    service: (&str, &str),
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let (signing_name, target_prefix) = service;
    let http_request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", AWS_JSON_1_1)
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

/// Whether `candidate` is a value `ClientIdType` admits: length 1..128 over `^[\w+]+$`,
/// i.e. word characters and `+` — which a CloudFormation-style `<stack>-<logical id>-<suffix>`
/// name is not.
///
/// Source: `ClientIdType` in the cognito-idp model
/// (`smithy_query(query="shape",shape="ClientIdType")`).
fn is_a_client_id(candidate: &str) -> bool {
    !candidate.is_empty()
        && candidate.chars().count() <= 128
        && candidate
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_' || character == '+')
}

/// Whether `candidate` is a value an identity pool's `CognitoIdentityProviders[].ClientId`
/// admits: length 1..128 over `^[\w_]+$`.
///
/// Source: `CognitoIdentityProviderClientId` in the cognito-identity model
/// (`smithy_query(query="shape",shape="CognitoIdentityProviderClientId")`).
fn is_a_federated_client_id(candidate: &str) -> bool {
    !candidate.is_empty()
        && candidate.chars().count() <= 128
        && candidate
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_')
}

/// Every declared app client publishes the id the owning service minted for it, that id
/// answers there, and the identity pool fed with it is created.
#[tokio::test]
async fn a_declared_app_client_publishes_the_client_id_the_owning_service_minted() {
    let router = router();
    let rows = rows();
    let stack_name = "cognito-user-pool-client-identity";
    let (status_code, body) = create_stack(&router, stack_name, &table_template(&rows), &[]).await;
    assert_eq!(
        status_code, 200,
        "expected HTTP 200 from CreateStack, got {status_code} with body {body}"
    );
    let described = poll_to_terminal(&router, "the deployment", stack_name).await;
    // The downstream half of the rule, stated once for the whole stack: the identity pool
    // consumes every client's `Ref`, so a published value outside its `ClientId` pattern is
    // what rolls the stack back.
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::CREATE_COMPLETE),
        "expected the stack wiring every app client into an identity pool to reach {:?}, got \
         body {described}; the identity pool's failure was {:?}",
        status::CREATE_COMPLETE,
        resource_event_reason(
            &router,
            stack_name,
            IDENTITY_POOL_LOGICAL_ID,
            "CREATE_FAILED"
        )
        .await
    );

    let pool_id = output_value(&described, POOL_LOGICAL_ID).unwrap_or_default();
    let (status_code, resources) = resources_of(&router, stack_name).await;
    assert_eq!(
        status_code, 200,
        "expected DescribeStackResources to answer, got {status_code} with body {resources}"
    );
    let reported = wire_resources(&resources);

    let mut published_ids: BTreeSet<String> = BTreeSet::new();
    for row in &rows {
        let case = row.case;
        let published = output_value(&described, &ref_key(row.logical_id)).unwrap_or_else(|| {
            panic!("case {case:?}: expected the stack to publish Ref of the client, got {described}")
        });
        assert!(
            is_a_client_id(&published),
            "case {case:?}: expected Ref to be an id ClientIdType admits (word characters and \
             '+', at most 128 of them), got {published:?}"
        );
        assert!(
            is_a_federated_client_id(&published),
            "case {case:?}: expected Ref to satisfy the ClientId an identity pool's \
             CognitoIdentityProviders takes, got {published:?}"
        );
        assert!(
            published_ids.insert(published.clone()),
            "case {case:?}: expected an id of its own for every app client, {published:?} was \
             published for another row too"
        );
        // A `Ref` IS the physical resource id, so the stack's own read cannot differ.
        let recorded = reported
            .iter()
            .find(|resource| resource.logical_id.as_deref() == Some(row.logical_id))
            .and_then(|resource| resource.physical_id.clone());
        assert_eq!(
            recorded.as_deref(),
            Some(published.as_str()),
            "case {case:?}: expected DescribeStackResources to report the same physical id \
             {published:?}, got {recorded:?}"
        );

        // The round trip: a client holding nothing but the published value reads the app
        // client back out of the owning service by it.
        let (status_code, answer) = aws_json(
            &router,
            COGNITO_IDP,
            "DescribeUserPoolClient",
            &json!({"UserPoolId": pool_id, "ClientId": published}),
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected DescribeUserPoolClient({pool_id}, {published}) — driven by \
             nothing but the published Ref — to be served, got {status_code} with {answer}"
        );
        let client = &answer["UserPoolClient"];
        assert_eq!(
            client["ClientId"].as_str(),
            Some(published.as_str()),
            "case {case:?}: expected the app client the published id names to carry it as its \
             ClientId, got {client}"
        );
        assert_eq!(
            client["UserPoolId"].as_str(),
            Some(pool_id.as_str()),
            "case {case:?}: expected the app client to live in the pool the stack published, got \
             {client}"
        );
        let secret = output_value(&described, &secret_key(row.logical_id));
        assert_eq!(
            secret.is_some(),
            row.generates_a_secret,
            "case {case:?}: expected a published ClientSecret only where the declaration asks \
             for one, got {secret:?}"
        );
        if let Some(secret) = secret {
            assert_eq!(
                client["ClientSecret"].as_str(),
                Some(secret.as_str()),
                "case {case:?}: expected the published secret to be the one the owning service \
                 generated, got {client}"
            );
        }
    }

    // The identity pool really federates every client, by the id each one published.
    let identity_pool_id = output_value(&described, IDENTITY_POOL_LOGICAL_ID).unwrap_or_default();
    let (status_code, pool) = aws_json(
        &router,
        COGNITO_IDENTITY,
        "DescribeIdentityPool",
        &json!({"IdentityPoolId": identity_pool_id}),
    )
    .await;
    assert_eq!(
        status_code, 200,
        "expected DescribeIdentityPool({identity_pool_id}) to be served, got {status_code} with \
         {pool}"
    );
    let federated: BTreeSet<String> = pool["CognitoIdentityProviders"]
        .as_array()
        .map(|providers| {
            providers
                .iter()
                .filter_map(|provider| provider["ClientId"].as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        federated, published_ids,
        "expected the identity pool to federate exactly the client ids the stack published, got \
         {pool}"
    );
    // Each provider names the pool's own provider name, which is what makes the pair a
    // federation of THIS user pool.
    let expected_provider = format!("cognito-idp.{REGION}.amazonaws.com/{pool_id}");
    for provider in pool["CognitoIdentityProviders"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        assert_eq!(
            provider["ProviderName"].as_str(),
            Some(expected_provider.as_str()),
            "expected every federated provider to name the user pool the stack created, got \
             {provider}"
        );
    }
}

/// The whole wiring the reported case deploys: a user pool, an app client of it, an identity
/// pool federating that client, the role a federated identity assumes, the role attachment
/// and the principal-tag mapping — one stack that reaches `CREATE_COMPLETE` because the app
/// client's `Ref` is a client id.
#[tokio::test]
async fn a_federation_estate_wired_on_the_clients_ref_is_created() {
    let case = "a federated estate gated by a session tag";
    let router = router();
    let stack_name = "cognito-client-federation-estate";
    let template = json!({
        "Resources": {
            POOL_LOGICAL_ID: {
                "Type": USER_POOL,
                "Properties": {"UserPoolName": "ci-federation-pool", "MfaConfiguration": "OFF"}
            },
            "UserPoolClient": {
                "Type": USER_POOL_CLIENT,
                "Properties": {
                    "UserPoolId": {"Ref": POOL_LOGICAL_ID},
                    "ClientName": "ci-federation-client",
                    "GenerateSecret": false
                }
            },
            IDENTITY_POOL_LOGICAL_ID: {
                "Type": IDENTITY_POOL,
                "Properties": {
                    "IdentityPoolName": "ci_federation_identity_pool",
                    "AllowUnauthenticatedIdentities": false,
                    "CognitoIdentityProviders": [{
                        "ClientId": {"Ref": "UserPoolClient"},
                        "ProviderName": {"Fn::GetAtt": [POOL_LOGICAL_ID, "ProviderName"]}
                    }]
                }
            },
            "FederatedRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": {"Federated": "cognito-identity.amazonaws.com"},
                            "Action": "sts:AssumeRoleWithWebIdentity",
                            "Condition": {
                                "StringEquals": {
                                    "cognito-identity.amazonaws.com:aud": {
                                        "Ref": IDENTITY_POOL_LOGICAL_ID
                                    }
                                }
                            }
                        }]
                    }
                }
            },
            "IdentityPoolRoleAttachment": {
                "Type": "AWS::Cognito::IdentityPoolRoleAttachment",
                "Properties": {
                    "IdentityPoolId": {"Ref": IDENTITY_POOL_LOGICAL_ID},
                    "Roles": {"authenticated": {"Fn::GetAtt": ["FederatedRole", "Arn"]}}
                }
            },
            "IdentityPoolPrincipalTag": {
                "Type": "AWS::Cognito::IdentityPoolPrincipalTag",
                "Properties": {
                    "IdentityPoolId": {"Ref": IDENTITY_POOL_LOGICAL_ID},
                    "IdentityProviderName": {
                        "Fn::GetAtt": [POOL_LOGICAL_ID, "ProviderName"]
                    },
                    "UseDefaults": false,
                    "PrincipalTags": {"tenant": "custom:tenant"}
                }
            }
        },
        "Outputs": {
            "Client": {"Value": {"Ref": "UserPoolClient"}},
            "Pool": {"Value": {"Ref": POOL_LOGICAL_ID}},
            "IdentityPool": {"Value": {"Ref": IDENTITY_POOL_LOGICAL_ID}}
        }
    })
    .to_string();

    let (status_code, body) = create_stack(
        &router,
        stack_name,
        &template,
        &acknowledging_iam(&[]),
    )
    .await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status_code} with body {body}"
    );
    let described = poll_to_terminal(&router, case, stack_name).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::CREATE_COMPLETE),
        "case {case:?}: expected the estate to reach {:?}, got body {described}; the identity \
         pool's failure was {:?}",
        status::CREATE_COMPLETE,
        resource_event_reason(
            &router,
            stack_name,
            IDENTITY_POOL_LOGICAL_ID,
            "CREATE_FAILED"
        )
        .await
    );

    let client_id = output_value(&described, "Client").unwrap_or_default();
    assert!(
        is_a_client_id(&client_id) && is_a_federated_client_id(&client_id),
        "case {case:?}: expected the client's Ref to be a client id both services admit, got \
         {client_id:?}"
    );
    let identity_pool_id = output_value(&described, "IdentityPool").unwrap_or_default();
    let (status_code, pool) = aws_json(
        &router,
        COGNITO_IDENTITY,
        "DescribeIdentityPool",
        &json!({"IdentityPoolId": identity_pool_id}),
    )
    .await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected DescribeIdentityPool to be served, got {status_code} with {pool}"
    );
    assert_eq!(
        pool["CognitoIdentityProviders"][0]["ClientId"].as_str(),
        Some(client_id.as_str()),
        "case {case:?}: expected the identity pool to federate the app client by the id the \
         stack published, got {pool}"
    );
}

/// The declaration of one app client an update re-states, and what the owning service must
/// then hold for the app client the published id names.
struct Revision {
    case: &'static str,
    /// The properties the re-declaration states beside `UserPoolId`.
    declared: fn() -> Value,
    /// The member the owning service must then report, and the value it must report for it.
    member: &'static str,
    held: Value,
}

/// The revisions: each changes a different member of the app client's configuration, so the
/// carried-forward identity is asserted over more than one kind of change.
fn revisions() -> Vec<Revision> {
    vec![
        Revision {
            case: "the client is renamed",
            declared: || json!({"ClientName": "ci-renamed-client"}),
            member: "ClientName",
            held: json!("ci-renamed-client"),
        },
        Revision {
            case: "the client's auth flows change",
            declared: || {
                json!({
                    "ClientName": "ci-renamed-client",
                    "ExplicitAuthFlows": ["ALLOW_REFRESH_TOKEN_AUTH"]
                })
            },
            member: "ExplicitAuthFlows",
            held: json!(["ALLOW_REFRESH_TOKEN_AUTH"]),
        },
        Revision {
            case: "the client's refresh-token validity changes",
            declared: || {
                json!({
                    "ClientName": "ci-renamed-client",
                    "ExplicitAuthFlows": ["ALLOW_REFRESH_TOKEN_AUTH"],
                    "RefreshTokenValidity": 7
                })
            },
            member: "RefreshTokenValidity",
            held: json!(7),
        },
    ]
}

/// The identifier in force is the one the owning service assigned, so a re-declaration of the
/// same app client is APPLIED to the client that exists rather than minting a second one: the
/// published id does not move across an update, and it still answers there.
#[tokio::test]
async fn an_updated_app_client_keeps_the_id_the_owning_service_assigned() {
    let router = router();
    let stack_name = "cognito-client-identity-updates";
    let client_logical_id = "UserPoolClient";
    let template = |declared: Value| {
        let mut properties = declared;
        properties
            .as_object_mut()
            .expect("the declared properties are an object")
            .insert("UserPoolId".to_string(), json!({"Ref": POOL_LOGICAL_ID}));
        json!({
            "Resources": {
                POOL_LOGICAL_ID: {
                    "Type": USER_POOL,
                    "Properties": {"UserPoolName": "ci-update-pool", "MfaConfiguration": "OFF"}
                },
                client_logical_id: {"Type": USER_POOL_CLIENT, "Properties": properties}
            },
            "Outputs": {
                "Client": {"Value": {"Ref": client_logical_id}},
                "Pool": {"Value": {"Ref": POOL_LOGICAL_ID}}
            }
        })
        .to_string()
    };

    let (status_code, body) = create_stack(
        &router,
        stack_name,
        &template(json!({"ClientName": "ci-original-client"})),
        &[],
    )
    .await;
    assert_eq!(
        status_code, 200,
        "expected HTTP 200 from CreateStack, got {status_code} with body {body}"
    );
    let described = poll_to_terminal(&router, "the first deployment", stack_name).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::CREATE_COMPLETE),
        "expected the stack to reach {:?}, got body {described}",
        status::CREATE_COMPLETE
    );
    let pool_id = output_value(&described, "Pool").unwrap_or_default();
    let minted = output_value(&described, "Client").unwrap_or_default();
    assert!(
        is_a_client_id(&minted),
        "expected the create to publish the id the owning service minted, got {minted:?}"
    );

    for revision in revisions() {
        let case = revision.case;
        let (status_code, body) = call(
            &router,
            "UpdateStack",
            &[
                member("StackName", stack_name),
                member("TemplateBody", &template((revision.declared)())),
            ],
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected UpdateStack to succeed, got {status_code} with body {body}"
        );
        let described = poll_to_terminal(&router, case, stack_name).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::UPDATE_COMPLETE),
            "case {case:?}: expected the update to reach {:?}, got body {described}",
            status::UPDATE_COMPLETE
        );
        let published = output_value(&described, "Client").unwrap_or_default();
        assert_eq!(
            published, minted,
            "case {case:?}: expected the app client to keep the id the owning service assigned, \
             got {published:?}"
        );
        let (status_code, answer) = aws_json(
            &router,
            COGNITO_IDP,
            "DescribeUserPoolClient",
            &json!({"UserPoolId": pool_id, "ClientId": published}),
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected the carried-forward id to still answer there, got \
             {status_code} with {answer}"
        );
        let held = answer["UserPoolClient"][revision.member].clone();
        assert_eq!(
            held, revision.held,
            "case {case:?}: expected the update to be applied to the app client the id names, \
             expected {} to be {}, got {held}",
            revision.member, revision.held
        );
    }
}
