// SPDX-License-Identifier: CC0-1.0

//! The AUTHORIZATION an `AWS::Serverless::Api` declares, read off the processed template the
//! `AWS::Serverless-2016-10-31` (AWS SAM) transform produced, over the awsQuery wire.
//!
//! RULE under test: **the transform writes an api's `Auth` into the OpenAPI definition of the
//! `AWS::ApiGateway::RestApi` it emits: each authorizer the declaration states becomes a
//! security SCHEME of that document, and the api's `DefaultAuthorizer` becomes the security
//! REQUIREMENT of every method the document leaves uncovered — a method that states a
//! requirement of its own keeps it, and one that states an EMPTY requirement has said it needs
//! none. An api that declares a default authorizer the `Authorizers` map does not define, or an
//! authorizer that could never authorize a request, is a document the macro FAILS on.**
//!
//! Why it matters: an api is created by IMPORTING that document, so a default authorizer the
//! transform never propagates deploys every uncovered route of the api to anonymous callers —
//! the one thing the declaration asked the macro to prevent. The deployed method's own
//! authorization is pinned end to end, through the service that serves it, in
//! `core/router/tests/all/cloudformation_sam_default_authorizer_covers_methods.rs`; this suite
//! is about the document the transform writes and the documents it refuses.
//!
//! The rows differ along what the rule generalises over: WHICH default the api states (the
//! caller's own credential and a declared authorizer), what the method it covers already
//! states (nothing, a requirement of its own, the empty requirement), where the method came
//! from (the `DefinitionBody` the declaration carries and a route generated from a function
//! event), and which KIND of authorizer the declaration states (a Lambda authorizer called
//! with a token, one called with the whole request, and an Amazon Cognito user pool).

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::settle_the_store;
use crate::harness::{call, member, router_with_engine, xml_text, xml_unescape};

/// The macro the AWS Serverless Application Model is published as.
const TRANSFORM: &str = "AWS::Serverless-2016-10-31";

/// The `DefaultAuthorizer` value that is the CALLER's own SigV4 credential rather than an
/// authorizer of the api, and the scheme name that authorization is declared under.
const AWS_IAM: &str = "AWS_IAM";

/// The authentication type API Gateway names the caller's own SigV4 credential by, which is
/// what says a security scheme stands for that authorization.
const AUTHTYPE_SIGV4: &str = "awsSigv4";

/// The authentication type a Lambda authorizer's scheme carries, and a user pool's.
const AUTHTYPE_CUSTOM: &str = "custom";
const AUTHTYPE_COGNITO: &str = "cognito_user_pools";

/// The status a change set the macro failed on settles in, and the reason envelope that
/// failure is reported in.
const FAILED: &str = "FAILED";

/// A template from its sections, with the transform declared.
fn template(sections: Value) -> String {
    let mut document = json!({ "Transform": TRANSFORM });
    if let (Some(document), Some(sections)) = (document.as_object_mut(), sections.as_object()) {
        for (name, value) in sections {
            document.insert(name.clone(), value.clone());
        }
    }
    document.to_string()
}

/// A `DefinitionBody` whose paths are the three methods the rows are stated over: one that
/// names no authorizer, one that names its own, and one that states the empty requirement.
fn definition_body() -> Value {
    json!({
        "swagger": "2.0",
        "info": { "title": "orders", "version": "1.0" },
        "paths": {
            "/open": { "get": { "responses": {} } },
            "/named": { "get": { "responses": {}, "security": [{ "Gate": [] }] } },
            "/public": { "get": { "responses": {}, "security": [] } },
        },
    })
}

/// The function an authorizer calls and the user pool one authorizes against, named as
/// resources the template does not declare: what the `Auth` expansion writes is the value the
/// declaration stated, whether that is a reference or an ARN.
const AUTHORIZER_FUNCTION: &str = "arn:aws:lambda:us-east-1:000000000000:function:guard";
const USER_POOL: &str = "arn:aws:cognito-idp:us-east-1:000000000000:userpool/us-east-1_abcDEF123";

/// One Lambda authorizer of each payload type, and one user-pool authorizer: the three kinds
/// an api may declare.
fn authorizers() -> Value {
    json!({
        "Gate": { "FunctionArn": AUTHORIZER_FUNCTION },
        "Whole": {
            "FunctionArn": AUTHORIZER_FUNCTION,
            "FunctionPayloadType": "REQUEST",
            "Identity": { "Headers": ["Authorization"], "QueryStrings": ["token"] },
        },
        "Pool": { "UserPoolArn": USER_POOL, "AuthorizationScopes": [POOL_SCOPE] },
    })
}

/// The scope the user-pool authorizer above states a method it covers is reached with.
const POOL_SCOPE: &str = "orders/read";

/// The most scopes the transform writes into one requirement, which is the most a method can
/// carry: the owning service's own guard on a caller-sized list
/// (`apigateway/src/state.rs::MAX_COLLECTION_ENTRIES`, which `bound_imported_method` bounds a
/// method's `authorizationScopes` by). Written out here so a change to that bound fails this
/// suite rather than following it.
const MAX_REQUIREMENT_SCOPES: usize = 50;

/// The api declaration one row submits: the definition above, the authorizers above, and the
/// default the row states.
fn api_template(default: &str) -> String {
    template(json!({
        "Resources": {
            "Web": {
                "Type": "AWS::Serverless::Api",
                "Properties": {
                    "StageName": "live",
                    "DefinitionBody": definition_body(),
                    "Auth": {
                        "DefaultAuthorizer": default,
                        "Authorizers": authorizers(),
                    },
                },
            },
        },
    }))
}

/// The processed template of a `CREATE` change set over `body`, as a JSON document.
async fn processed_template(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    stack: &str,
    body: &str,
) -> Value {
    let (code, answer) = call(
        router,
        "CreateChangeSet",
        &[
            member("StackName", stack),
            member("ChangeSetName", "plan"),
            member("ChangeSetType", "CREATE"),
            member("TemplateBody", body),
            member("Capabilities.member.1", "CAPABILITY_IAM"),
            member("Capabilities.member.2", "CAPABILITY_AUTO_EXPAND"),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected CreateChangeSet to be accepted, got {code} {answer}"
    );
    settle_the_store(router, engine).await;
    let (code, answer) = call(
        router,
        "GetTemplate",
        &[
            member("StackName", stack),
            member("ChangeSetName", "plan"),
            member("TemplateStage", "Processed"),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected the processed template to be readable, got {code} {answer}"
    );
    let processed = xml_text(&answer, "TemplateBody")
        .map(|body| xml_unescape(&body))
        .unwrap_or_else(|| panic!("case {case:?}: no TemplateBody in {answer}"));
    serde_json::from_str(&processed)
        .unwrap_or_else(|error| panic!("case {case:?}: expected a JSON document, got {error}"))
}

/// The definition the processed template's api carries.
fn definition_of<'a>(case: &str, processed: &'a Value, logical_id: &str) -> &'a Value {
    processed
        .get("Resources")
        .and_then(|resources| resources.get(logical_id))
        .and_then(|api| api.get("Properties"))
        .and_then(|properties| properties.get("Body"))
        .unwrap_or_else(|| panic!("case {case:?}: no definition on {logical_id} in {processed}"))
}

/// The security requirement one method of a definition states, as the scheme names it lists.
fn requirement(definition: &Value, path: &str, method: &str) -> Option<Vec<String>> {
    let security = definition
        .get("paths")
        .and_then(|paths| paths.get(path))
        .and_then(|item| item.get(method))
        .and_then(|operation| operation.get("security"))?
        .as_array()?
        .iter()
        .flat_map(|entry| {
            entry
                .as_object()
                .map(|names| names.keys().cloned().collect::<Vec<String>>())
                .unwrap_or_default()
        })
        .collect();
    Some(security)
}

#[tokio::test]
async fn the_default_authorizer_covers_every_method_the_definition_leaves_uncovered() {
    // (case, the default the api declares, the requirement the uncovered method inherits)
    let rows: &[(&str, &str, &str)] = &[
        (
            "the caller's own credential as the default",
            AWS_IAM,
            AWS_IAM,
        ),
        ("a declared authorizer as the default", "Whole", "Whole"),
        ("another declared authorizer as the default", "Pool", "Pool"),
    ];
    let (router, engine) = router_with_engine();
    for (index, (case, default, inherited)) in rows.iter().enumerate() {
        let stack = format!("sam-auth-default-{index}");
        let processed =
            processed_template(&router, &engine, case, &stack, &api_template(default)).await;
        let definition = definition_of(case, &processed, "Web");

        // The method the document names no authorizer for inherits the default...
        assert_eq!(
            requirement(definition, "/open", "get"),
            Some(vec![(*inherited).to_string()]),
            "case {case:?}: expected the uncovered method to inherit {inherited:?}, got \
             {definition}"
        );
        // ...the one that names its own keeps it...
        assert_eq!(
            requirement(definition, "/named", "get"),
            Some(vec!["Gate".to_string()]),
            "case {case:?}: expected the method's own requirement to stand, got {definition}"
        );
        // ...and the one that states the EMPTY requirement has said it needs none.
        assert_eq!(
            requirement(definition, "/public", "get"),
            Some(Vec::new()),
            "case {case:?}: expected the empty requirement to stand, got {definition}"
        );

        // Every declared authorizer is a scheme of the document, under the authentication
        // type API Gateway names that kind of authorization by.
        for (name, authtype) in [
            ("Gate", AUTHTYPE_CUSTOM),
            ("Whole", AUTHTYPE_CUSTOM),
            ("Pool", AUTHTYPE_COGNITO),
        ] {
            let scheme = definition
                .get("securityDefinitions")
                .and_then(|schemes| schemes.get(name))
                .unwrap_or_else(|| {
                    panic!("case {case:?}: no security scheme for {name} in {definition}")
                });
            assert_eq!(
                scheme.get("x-amazon-apigateway-authtype").and_then(Value::as_str),
                Some(authtype),
                "case {case:?}: expected {name} to be declared as {authtype}, got {scheme}"
            );
            assert!(
                scheme.get("x-amazon-apigateway-authorizer").is_some(),
                "case {case:?}: expected {name} to carry the authorizer API Gateway creates \
                 it from, got {scheme}"
            );
        }
    }
}

#[tokio::test]
async fn the_caller_credential_default_is_declared_as_the_sigv4_scheme() {
    // The one authorization that is a SCHEME rather than a resource: it is declared with the
    // header the credential travels in and the authentication type that names SigV4, and
    // nothing is created for it.
    let case = "the caller's own credential";
    let (router, engine) = router_with_engine();
    let processed =
        processed_template(&router, &engine, case, "sam-auth-sigv4", &api_template(AWS_IAM)).await;
    let definition = definition_of(case, &processed, "Web");
    let scheme = definition
        .get("securityDefinitions")
        .and_then(|schemes| schemes.get(AWS_IAM))
        .unwrap_or_else(|| panic!("case {case:?}: no scheme for {AWS_IAM} in {definition}"));
    assert_eq!(
        scheme.get("type").and_then(Value::as_str),
        Some("apiKey"),
        "case {case:?}: expected the credential to be carried in a named header, got {scheme}"
    );
    assert_eq!(
        scheme.get("name").and_then(Value::as_str),
        Some("Authorization"),
        "case {case:?}: expected the header a SigV4 credential travels in, got {scheme}"
    );
    assert_eq!(
        scheme.get("x-amazon-apigateway-authtype").and_then(Value::as_str),
        Some(AUTHTYPE_SIGV4),
        "case {case:?}: expected the authentication type that names SigV4, got {scheme}"
    );
    assert!(
        scheme.get("x-amazon-apigateway-authorizer").is_none(),
        "case {case:?}: expected no authorizer for an authorization that needs none, got \
         {scheme}"
    );
}

#[tokio::test]
async fn a_generated_route_inherits_the_default_authorizer_too() {
    // A method the transform GENERATED from a function event is a method of the same api, so
    // the default covers it exactly as it covers one the declaration wrote.
    let case = "a route generated from an Api event";
    let body = template(json!({
        "Resources": {
            "Web": {
                "Type": "AWS::Serverless::Api",
                "Properties": {
                    "StageName": "live",
                    "Auth": { "DefaultAuthorizer": AWS_IAM },
                },
            },
            "Place": {
                "Type": "AWS::Serverless::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "InlineCode": "def handler(event, context):\n    return {}\n",
                    "Events": {
                        "Route": {
                            "Type": "Api",
                            "Properties": {
                                "Path": "/orders",
                                "Method": "post",
                                "RestApiId": { "Ref": "Web" },
                            },
                        },
                    },
                },
            },
        },
    }));
    let (router, engine) = router_with_engine();
    let processed = processed_template(&router, &engine, case, "sam-auth-generated", &body).await;
    let definition = definition_of(case, &processed, "Web");
    assert_eq!(
        requirement(definition, "/orders", "post"),
        Some(vec![AWS_IAM.to_string()]),
        "case {case:?}: expected the generated route to inherit the default, got {definition}"
    );
}

/// The scopes one method's security requirement is reached with, flattened over the schemes
/// it names.
fn requirement_scopes(definition: &Value, path: &str, method: &str) -> Vec<String> {
    definition
        .get("paths")
        .and_then(|paths| paths.get(path))
        .and_then(|item| item.get(method))
        .and_then(|operation| operation.get("security"))
        .and_then(Value::as_array)
        .map(|requirement| {
            requirement
                .iter()
                .filter_map(Value::as_object)
                .flat_map(|named| named.values())
                .filter_map(Value::as_array)
                .flatten()
                .filter_map(|scope| scope.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn the_default_authorizers_scopes_travel_with_the_requirement() {
    // The scopes a declared authorizer states are the VALUE of the requirement naming it, so
    // a method the default covers is reached with exactly those — and the authorization that
    // is the caller's own credential states none.
    let (router, engine) = router_with_engine();
    for (case, index, default, expected) in [
        (
            "a declared authorizer stating its scopes",
            0,
            "Pool",
            vec![POOL_SCOPE.to_string()],
        ),
        ("the caller's own credential", 1, AWS_IAM, Vec::new()),
    ] {
        let stack = format!("sam-auth-scopes-{index}");
        let processed =
            processed_template(&router, &engine, case, &stack, &api_template(default)).await;
        let definition = definition_of(case, &processed, "Web");
        let scopes = requirement_scopes(definition, "/open", "get");
        assert_eq!(
            scopes, expected,
            "case {case:?}: expected the requirement to be reached with {expected:?}, got \
             {definition}"
        );
    }
}

#[tokio::test]
async fn the_requirement_carries_no_more_scopes_than_a_method_can() {
    // The scopes a requirement is reached with are a method's own, and the owning service
    // stores no more of them than its guard on a caller-sized list admits — so a declaration
    // stating more is written up to that many and no further, which is what keeps the written
    // document from being the product of the methods the definition declares and the scopes
    // the authorizer states.
    let case = "an authorizer stating more scopes than a method can carry";
    let stated: Vec<String> = (0..MAX_REQUIREMENT_SCOPES * 2)
        .map(|index| format!("orders/read{index}"))
        .collect();
    let body = template(json!({
        "Resources": {
            "Web": {
                "Type": "AWS::Serverless::Api",
                "Properties": {
                    "StageName": "live",
                    "DefinitionBody": definition_body(),
                    "Auth": {
                        "DefaultAuthorizer": "Pool",
                        "Authorizers": {
                            "Pool": {
                                "UserPoolArn": USER_POOL,
                                "AuthorizationScopes": stated,
                            },
                        },
                    },
                },
            },
        },
    }));
    let (router, engine) = router_with_engine();
    let processed = processed_template(&router, &engine, case, "sam-auth-scope-bound", &body).await;
    let definition = definition_of(case, &processed, "Web");
    let written = requirement_scopes(definition, "/open", "get");
    assert_eq!(
        written.len(),
        MAX_REQUIREMENT_SCOPES,
        "case {case:?}: expected the requirement to carry {MAX_REQUIREMENT_SCOPES} scopes, \
         got {}",
        written.len()
    );
    assert_eq!(
        written.first().map(String::as_str),
        Some("orders/read0"),
        "case {case:?}: expected the scopes the declaration stated, got {written:?}"
    );
}

#[tokio::test]
async fn an_authorizer_the_transform_can_build_no_scheme_from_covers_nothing() {
    // A requirement naming a scheme the document does not declare resolves to nothing, so a
    // default whose declaration states neither a function to call nor a user pool to
    // authorize against is written onto no method at all.
    let case = "a default authorizer whose declaration names nothing to authorize with";
    let body = template(json!({
        "Resources": {
            "Web": {
                "Type": "AWS::Serverless::Api",
                "Properties": {
                    "StageName": "live",
                    "DefinitionBody": definition_body(),
                    "Auth": {
                        "DefaultAuthorizer": "Hollow",
                        "Authorizers": { "Hollow": {} },
                    },
                },
            },
        },
    }));
    let (router, engine) = router_with_engine();
    let processed = processed_template(&router, &engine, case, "sam-auth-hollow", &body).await;
    let definition = definition_of(case, &processed, "Web");
    assert_eq!(
        requirement(definition, "/open", "get"),
        None,
        "case {case:?}: expected the uncovered method to inherit no requirement, got \
         {definition}"
    );
    assert!(
        definition.get("securityDefinitions").is_none(),
        "case {case:?}: expected no security scheme to be declared, got {definition}"
    );
}

#[tokio::test]
async fn an_api_whose_auth_the_macro_cannot_expand_fails_the_change_set() {
    // (case, the api's Auth, the cause the macro reports)
    let rows: &[(&str, Value, &str)] = &[
        (
            "a REQUEST authorizer naming no part of the request to authorize on",
            json!({
                "Authorizers": {
                    "Whole": {
                        "FunctionArn": AUTHORIZER_FUNCTION,
                        "FunctionPayloadType": "REQUEST",
                    },
                },
            }),
            "Resource with id [Web] is invalid. Whole Authorizer must specify Identity with \
             at least one of Headers, QueryStrings, StageVariables, or Context.",
        ),
        (
            "a default authorizer the api does not declare",
            json!({ "DefaultAuthorizer": "Missing" }),
            "Resource with id [Web] is invalid. Unable to set DefaultAuthorizer because \
             'Missing' was not defined in 'Authorizers'",
        ),
    ];
    let (router, engine) = router_with_engine();
    for (index, (case, auth, cause)) in rows.iter().enumerate() {
        let stack = format!("sam-auth-invalid-{index}");
        let body = template(json!({
            "Resources": {
                "Web": {
                    "Type": "AWS::Serverless::Api",
                    "Properties": { "StageName": "live", "Auth": auth },
                },
            },
        }));
        let (code, answer) = call(
            &router,
            "CreateChangeSet",
            &[
                member("StackName", &stack),
                member("ChangeSetName", "plan"),
                member("ChangeSetType", "CREATE"),
                member("TemplateBody", &body),
                member("Capabilities.member.1", "CAPABILITY_AUTO_EXPAND"),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: a transform failure is no bad request, got {code} {answer}"
        );
        settle_the_store(&router, &engine).await;
        let (code, described) = call(
            &router,
            "DescribeChangeSet",
            &[member("StackName", &stack), member("ChangeSetName", "plan")],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: DescribeChangeSet must answer 200, got {code} {described}"
        );
        assert_eq!(
            xml_text(&described, "Status").as_deref(),
            Some(FAILED),
            "case {case:?}: expected the change set to fail, got {described}"
        );
        let expected = format!(
            "Transform {TRANSFORM} failed with: Invalid Serverless Application Specification \
             document. Number of errors found: 1. {cause}"
        );
        assert_eq!(
            xml_text(&described, "StatusReason")
                .map(|reason| xml_unescape(&reason))
                .as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected the macro's own diagnostic {expected:?}, got {described}"
        );
    }
}
