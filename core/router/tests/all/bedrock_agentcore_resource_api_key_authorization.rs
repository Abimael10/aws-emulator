// SPDX-License-Identifier: CC0-1.0

//! WHO may exchange a workload identity token for a resource's API key: the IAM decision
//! `GetResourceApiKey` takes in front of everything else it does.
//!
//! RULE: an AWS service decides a request at the IAM layer, ahead of the operation, so the
//! AgentCore Identity exchange is authorized against the identity-based policies in force on
//! the principal the request's credential names, on the action
//! `<arn namespace>:<OperationName>` and on the API key credential provider the request's
//! `resourceCredentialProviderName` addresses in this account's token vault — the resource as
//! the plane that OWNS the vault mints its ARN. A call a readable statement allows reaches the
//! operation and is answered the key that provider holds; one no readable statement allows,
//! and one an explicit `Deny` covers, are both `AccessDeniedException` (HTTP 403) naming the
//! principal, the action, that resource and which of IAM's rules withheld the grant. Because
//! the decision is taken first, it is the same for a provider the account holds and for a name
//! it holds none under, and for a live presented token as for a value no issuance stands
//! behind — so a refusal discloses neither whether the provider exists nor whether the
//! credential in the body was good.
//!
//! What this adds to the two suites beside it: `bedrock_agentcore_resource_api_key.rs` pins
//! the exchange itself and `bedrock_agentcore_workload_token_gate.rs` the credential the
//! request BODY carries, both as the account's own credential, which no identity-based policy
//! bounds; neither drives the gate IN FRONT of them. Four crates have to agree for this rule
//! to hold — IAM holds the role and its documents, STS mints the session credential and names
//! its principal, the AgentCore control plane holds the credential provider and Secrets
//! Manager the secret its key lives in, and the data plane takes the decision — so the suite
//! lives here rather than in either AgentCore crate.
//!
//! The rows span what the rule generalises over: what the principal's document states (the
//! exact action on the resource addressed, that action inside a list, every action of the
//! service, the action on another provider of the same kind, another operation of the service
//! only, nothing of the service at all, no document whatsoever, an explicit deny over an
//! allow that would otherwise cover it), and — for the ordering — whether the provider named
//! exists and whether the presented token is one this emulator issued.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The exchange's own reading of an answer, and a token of the identity plane's own form that
// nothing issued: written once, in the suite that pins the exchange.
use crate::bedrock_agentcore_resource_api_key::{
    token_minted_nowhere, Answer, REGION, USER, WORKLOAD,
};
// The IAM and STS fixtures every suite of this crate that drives a role session shares.
use crate::sts_session_credential_refusals::{
    assume_role, create_role, put_role_policy, session_arn, AMBIENT_KEY,
};

/// The account every ARN this emulator mints carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The ARN namespace both AgentCore planes are addressed in, which is also the IAM action
/// namespace of the service (source: smithy_query query="service_info" on either AgentCore
/// model: `arnNamespace`).
const AGENTCORE: &str = "bedrock-agentcore";

/// The action this exchange is decided under: the namespace above and the operation's own
/// name.
const ACTION: &str = "bedrock-agentcore:GetResourceApiKey";

/// `GetResourceApiKey`: `POST /identities/api-key` (source: smithy_query query="operation"
/// operation="GetResourceApiKey": `http`).
const API_KEY_ROUTE: &str = "/identities/api-key";

/// The mint of the identity plane the presented token comes from (source: smithy_query
/// query="operation" operation="GetWorkloadAccessTokenForUserId": `http`).
const TOKEN_ROUTE: &str = "/identities/GetWorkloadAccessTokenForUserId";

/// The control plane's create of an API key credential provider (source: smithy_query
/// query="operation" operation="CreateApiKeyCredentialProvider": `http`).
const CREATE_PROVIDER_ROUTE: &str = "/identities/CreateApiKeyCredentialProvider";

/// The provider the account holds throughout, the key it keeps, and a second provider of the
/// same kind a statement can be scoped to instead. Both names are inside
/// `CredentialProviderName` (len 1..128, pattern `^[a-zA-Z0-9\-_]+$`).
const PROVIDER: &str = "gate-partner-api";
const PROVIDER_KEY: &str = "sk-live-authz-0001";
const OTHER_PROVIDER: &str = "another_partner-api";

/// A name the account holds no provider under, which a refused call must be refused on just
/// the same.
const NO_SUCH_PROVIDER: &str = "no-provider-of-this-name";

/// The code and status AWS refuses a call IAM does not permit with (source: smithy_query
/// query="operation" operation="GetResourceApiKey": `AccessDeniedException`, `@httpError`
/// 403).
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The refusal the exchange answers a provider the account does not hold with (source:
/// smithy_query query="operation" operation="GetResourceApiKey":
/// `ResourceNotFoundException`, `@httpError` 404).
const NOT_FOUND: &str = "ResourceNotFoundException";
const NOT_FOUND_STATUS: u16 = 404;
const PROVIDER_NOT_FOUND_MESSAGE: &str = "Resource Credential Provider not found";

/// The one sentence the plane refuses a presented WORKLOAD IDENTITY token with — the refusal a
/// call that got past the IAM gate with a bad credential in its body earns.
const WORKLOAD_TOKEN_REFUSED_MESSAGE: &str = "Invalid workload access token";

/// The emulator as it is ASSEMBLED, on ONE engine: IAM and STS hold the principal and its
/// documents, the AgentCore control plane the credential provider, Secrets Manager the secret
/// its key lives in, and the AgentCore data plane answers the exchange.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    // The signature is never checked, as in every other suite of this crate: the credential
    // scope is what routes a request and the access key id is what names its caller.
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// One AgentCore `restJson1` call, signed as `access_key_id`; the ROUTE picks which of the two
/// planes answers it, because they share a signing name.
async fn agentcore(
    router: &MultiServiceRouter,
    access_key_id: &str,
    route: &str,
    body: &Value,
) -> Answer {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{AGENTCORE}.{REGION}.amazonaws.com{route}"))
        .header("host", format!("{AGENTCORE}.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{AGENTCORE}/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .header("content-type", "application/json")
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    Answer::of(router.dispatch(request).await)
}

/// `GetResourceApiKey`, presenting `token` for `provider`, signed as `access_key_id`.
async fn resource_api_key(
    router: &MultiServiceRouter,
    access_key_id: &str,
    token: &str,
    provider: &str,
) -> Answer {
    agentcore(
        router,
        access_key_id,
        API_KEY_ROUTE,
        &json!({
            "workloadIdentityToken": token,
            "resourceCredentialProviderName": provider,
        }),
    )
    .await
}

/// Create an API key credential provider whose key the service stores for it, as the
/// account's own credential, and answer the ARN its owner minted for it.
async fn create_provider(router: &MultiServiceRouter, name: &str, api_key: &str) -> String {
    let answer = agentcore(
        router,
        AMBIENT_KEY,
        CREATE_PROVIDER_ROUTE,
        &json!({"name": name, "apiKey": api_key}),
    )
    .await;
    assert_eq!(
        answer.status, 201,
        "setup: the provider {name:?} must be created, got {answer:?}"
    );
    let arn = answer.text("credentialProviderArn");
    assert_eq!(
        arn,
        provider_arn(name),
        "setup: the provider {name:?} must be addressed by its owner's own ARN, got {arn:?}"
    );
    arn
}

/// Mint a workload access token for a user, as the account's own credential, and answer it.
async fn live_token(router: &MultiServiceRouter) -> String {
    let answer = agentcore(
        router,
        AMBIENT_KEY,
        TOKEN_ROUTE,
        &json!({"workloadName": WORKLOAD, "userId": USER}),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "setup: the fixture mint must answer 200, got {answer:?}"
    );
    let token = answer.text("workloadAccessToken");
    assert!(
        !token.is_empty(),
        "setup: the fixture mint must report a token, got {answer:?}"
    );
    token
}

/// The resource a call naming the provider `name` addresses:
/// `arn:aws:bedrock-agentcore:<region>:<account>:token-vault/<tokenVaultId>/\
/// apikeycredentialprovider/<name>`, in this account's default vault (source: the ARN a
/// credential provider's consumer requires, `smithy_query query="shape"
/// shape="ConsentPortalIdpConfig"` of the control plane's model, member
/// `credentialProviderArn`). Built from the NAME alone, so a name the account holds no
/// provider under has one too.
fn provider_arn(name: &str) -> String {
    format!(
        "arn:aws:{AGENTCORE}:{REGION}:{ACCOUNT}:token-vault/default/apikeycredentialprovider/\
         {name}"
    )
}

/// An identity-based document carrying `statements`.
fn document(statements: Vec<Value>) -> String {
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// One statement of a document.
fn statement(effect: &str, actions: &[&str], resource: &str) -> Value {
    json!({"Effect": effect, "Action": actions, "Resource": resource})
}

/// What IAM owes the exchange for one principal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// A statement allows the call, so the exchange is answered the key the provider holds.
    Served,
    /// Nothing readable allows the action: IAM's default deny.
    DefaultDeny,
    /// A statement of the principal's own document denies it.
    ExplicitDeny,
}

/// The sentence a refusal carries, built here from the request rather than from the service's
/// own helper, so the wording has something to be wrong about: IAM names the principal, the
/// action, the resource the request addressed and which of its rules withheld the grant.
fn refusal_message(principal: &str, resource: &str, expected: Expected) -> String {
    let reason = match expected {
        Expected::ExplicitDeny => "with an explicit deny in an identity-based policy".to_string(),
        Expected::DefaultDeny | Expected::Served => {
            format!("because no identity-based policy allows the {ACTION} action")
        }
    };
    format!(
        "User: {principal} is not authorized to perform: {ACTION} on resource: {resource} \
         {reason}"
    )
}

/// Put `document` in force on a fresh role and answer the access key id of a session of it,
/// together with the principal ARN that session speaks as.
async fn principal_holding(
    router: &MultiServiceRouter,
    case: &str,
    role_name: &str,
    document: Option<String>,
) -> (String, String) {
    create_role(router, case, role_name).await;
    if let Some(document) = document {
        put_role_policy(router, case, role_name, "exchange-grants", &document).await;
    }
    let key = assume_role(router, case, role_name, None).await;
    (key, session_arn(role_name))
}

/// Assert one answer of the exchange against what IAM owes the row.
fn assert_decision(
    case: &str,
    answer: &Answer,
    principal: &str,
    resource: &str,
    expected: Expected,
) {
    match expected {
        Expected::Served => {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: a call IAM permits must be served, got {answer:?}"
            );
            assert_eq!(
                answer.text("apiKey"),
                PROVIDER_KEY,
                "case {case:?}: expected the key the provider holds, got {answer:?}"
            );
        }
        Expected::DefaultDeny | Expected::ExplicitDeny => {
            assert_eq!(
                answer.status, ACCESS_DENIED_STATUS,
                "case {case:?}: a call IAM does not permit must be refused \
                 {ACCESS_DENIED_STATUS}, got {answer:?}"
            );
            assert_eq!(
                answer.error_code(),
                ACCESS_DENIED,
                "case {case:?}: expected {ACCESS_DENIED}, got {answer:?}"
            );
            assert_eq!(
                answer.message(),
                refusal_message(principal, resource, expected),
                "case {case:?}: expected IAM's wording, got {answer:?}"
            );
            assert!(
                !answer.message().contains(PROVIDER_KEY),
                "case {case:?}: a refusal must carry no part of the key, got {answer:?}"
            );
        }
    }
}

/// The exchange is served only to a principal the identity-based policies in force on it
/// allow the action on the provider the request names, and refused to every other with IAM's
/// own sentence.
#[tokio::test]
async fn the_exchange_is_served_only_to_a_principal_iam_permits() {
    let router = build_router();
    let addressed = create_provider(&router, PROVIDER, PROVIDER_KEY).await;
    create_provider(&router, OTHER_PROVIDER, "sk-live-authz-0002").await;
    let token = live_token(&router).await;

    let rows: Vec<(&str, &str, Option<String>, Expected)> = vec![
        (
            "an allow of the exact action on the provider addressed",
            "exchange-exact",
            Some(document(vec![statement("Allow", &[ACTION], &addressed)])),
            Expected::Served,
        ),
        (
            "an allow whose action list carries this one beside another",
            "exchange-in-a-list",
            Some(document(vec![statement(
                "Allow",
                &["bedrock-agentcore:GetWorkloadAccessToken", ACTION],
                &addressed,
            )])),
            Expected::Served,
        ),
        (
            "a service-wide allow of every action on every resource",
            "exchange-service-wide",
            Some(document(vec![statement(
                "Allow",
                &["bedrock-agentcore:*"],
                "*",
            )])),
            Expected::Served,
        ),
        (
            "an allow scoped to another provider of the same kind",
            "exchange-other-provider",
            Some(document(vec![statement(
                "Allow",
                &[ACTION],
                &provider_arn(OTHER_PROVIDER),
            )])),
            Expected::DefaultDeny,
        ),
        (
            "an allow of another operation of the same service only",
            "exchange-other-operation",
            Some(document(vec![statement(
                "Allow",
                &["bedrock-agentcore:GetResourceOauth2Token"],
                "*",
            )])),
            Expected::DefaultDeny,
        ),
        (
            "a document stating nothing of this service",
            "exchange-another-service",
            Some(document(vec![statement("Allow", &["s3:GetObject"], "*")])),
            Expected::DefaultDeny,
        ),
        (
            "no document at all",
            "exchange-no-document",
            None,
            Expected::DefaultDeny,
        ),
        (
            "an explicit deny of the action beside a service-wide allow",
            "exchange-denied-action",
            Some(document(vec![
                statement("Allow", &["bedrock-agentcore:*"], "*"),
                statement("Deny", &[ACTION], &addressed),
            ])),
            Expected::ExplicitDeny,
        ),
        (
            "an explicit deny of every action of the service beside an exact allow",
            "exchange-denied-service",
            Some(document(vec![
                statement("Allow", &[ACTION], &addressed),
                statement("Deny", &["bedrock-agentcore:*"], "*"),
            ])),
            Expected::ExplicitDeny,
        ),
    ];

    for (case, role_name, document, expected) in rows {
        let (key, principal) = principal_holding(&router, case, role_name, document).await;
        let answer = resource_api_key(&router, &key, &token, PROVIDER).await;
        assert_decision(case, &answer, &principal, &addressed, expected);
    }
}

/// The decision is taken in FRONT of the operation: a principal no statement allows is
/// refused the same way whether the provider it names exists and whether the token it
/// presents is one this emulator issued, and the refusal names the resource the REQUEST
/// addressed. A principal the policies do allow then reaches the operation's own two gates —
/// the credential in the body first, the provider after it.
#[tokio::test]
async fn iam_decides_before_the_token_and_the_provider_are_resolved() {
    let router = build_router();
    let addressed = create_provider(&router, PROVIDER, PROVIDER_KEY).await;
    let token = live_token(&router).await;
    let nothing_issued = token_minted_nowhere(REGION, Some(USER), 909);

    let (refused_key, refused_principal) = principal_holding(
        &router,
        "a principal nothing allows",
        "exchange-unprivileged",
        None,
    )
    .await;
    for (case, provider, presented) in [
        ("a provider the account holds", PROVIDER, token.as_str()),
        (
            "a name the account holds no provider under",
            NO_SUCH_PROVIDER,
            token.as_str(),
        ),
        (
            "a presented token no issuance stands behind",
            PROVIDER,
            nothing_issued.as_str(),
        ),
        (
            "neither the provider nor the token",
            NO_SUCH_PROVIDER,
            nothing_issued.as_str(),
        ),
    ] {
        let answer = resource_api_key(&router, &refused_key, presented, provider).await;
        assert_decision(
            case,
            &answer,
            &refused_principal,
            &provider_arn(provider),
            Expected::DefaultDeny,
        );
        assert!(
            !answer.message().contains(PROVIDER_NOT_FOUND_MESSAGE)
                && !answer.message().contains(WORKLOAD_TOKEN_REFUSED_MESSAGE),
            "case {case:?}: a call refused at the IAM layer must say nothing about the \
             provider or the presented token, got {answer:?}"
        );
    }

    let (permitted_key, _) = principal_holding(
        &router,
        "a principal the policies allow",
        "exchange-privileged",
        Some(document(vec![statement(
            "Allow",
            &["bedrock-agentcore:*"],
            "*",
        )])),
    )
    .await;

    let answer = resource_api_key(&router, &permitted_key, &nothing_issued, PROVIDER).await;
    assert_eq!(
        answer.status, ACCESS_DENIED_STATUS,
        "case \"past IAM with a token nothing issued\": expected {ACCESS_DENIED_STATUS}, got \
         {answer:?}"
    );
    assert_eq!(
        answer.message(),
        WORKLOAD_TOKEN_REFUSED_MESSAGE,
        "case \"past IAM with a token nothing issued\": expected the token gate's own \
         sentence, got {answer:?}"
    );

    let answer = resource_api_key(&router, &permitted_key, &token, NO_SUCH_PROVIDER).await;
    assert_eq!(
        answer.status, NOT_FOUND_STATUS,
        "case \"past IAM and the token, naming no provider of the account\": expected \
         {NOT_FOUND_STATUS}, got {answer:?}"
    );
    assert_eq!(
        answer.error_code(),
        NOT_FOUND,
        "case \"past IAM and the token, naming no provider of the account\": expected \
         {NOT_FOUND}, got {answer:?}"
    );
    assert_eq!(
        answer.message(),
        PROVIDER_NOT_FOUND_MESSAGE,
        "case \"past IAM and the token, naming no provider of the account\": expected the \
         provider gate's own sentence, got {answer:?}"
    );

    let answer = resource_api_key(&router, &permitted_key, &token, PROVIDER).await;
    assert_eq!(
        answer.status, 200,
        "case \"past both gates\": expected the exchange to be served, got {answer:?}"
    );
    assert_eq!(
        answer.text("apiKey"),
        PROVIDER_KEY,
        "case \"past both gates\": expected the key the provider {addressed:?} holds, got \
         {answer:?}"
    );
}
