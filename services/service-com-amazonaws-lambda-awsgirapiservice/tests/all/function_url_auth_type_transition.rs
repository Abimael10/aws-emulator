// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH CALLER one function URL admits as its CONFIGURATION and its
//! function's POLICY are changed under it.
//!
//! RULE: every request to a function URL is authorized at the endpoint's own front door,
//! against the function's resource-based policy as it stands AT THAT MOMENT for the qualifier
//! the URL is attached to, under the endpoint's own action (`lambda:InvokeFunctionUrl`) and
//! with the condition keys such a request publishes a value for — the `AuthType` of the URL it
//! arrived at, and that it came through a URL at all. `AuthType` decides only whether the
//! emulator AUTHENTICATES the request ("`NONE` – Bypass IAM authentication to create a public
//! endpoint"); it never grants. So a request nothing authenticated speaks for nobody, the only
//! `Principal` element that can name it is the wildcard, and a policy whose grants all name a
//! principal admits it nowhere: it is refused `403` with `x-amzn-errortype:
//! AccessDeniedException` and the authorizer's own fixed document, before the function runs.
//! Because the decision is taken per request off the stored configuration and the stored
//! policy, `UpdateFunctionUrlConfig`, `AddPermission` and `RemovePermission` each change which
//! callers the SAME published address admits, with no re-create and no new address.
//!
//! The steps below walk one endpoint through those changes, probing it after each with the two
//! callers the rule distinguishes, so what is asserted is the decision in force rather than
//! the one the create started with. They differ along what the rule generalises over: the
//! `AuthType` in force (`NONE`, `AWS_IAM`, and back), which principal form the grants name (a
//! named role ARN, the wildcard), which `FunctionUrlAuthType` a grant is scoped to (the one in
//! force and the other one), which action it grants (the endpoint's own and the API's), and
//! whether the statement is added or taken away.
//!
//! A verified credential is a separate caller throughout: the one long-term credential this
//! emulator publishes speaks for the account's own identity, which holds no IAM policies of
//! its own — the position account root credentials are in — so it is served whatever the
//! policy says, and what the steps vary is the ANONYMOUS caller's admission. The signed leg is
//! probed at every step anyway, because the contract has two halves: the unsigned caller must
//! read the refusal, and the same endpoint must still SERVE the caller it admits.
//!
//! Every probe drives a real HTTP request through a router that VERIFIES request signatures —
//! as the emulator does when it is started with request authentication on — against a function
//! whose deployment package RUNS, so a refusal that let the handler run, and an admission that
//! answered without it, both fail here.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_url_data_plane::{
    control, given_configured_url, given_function, grant, router_on, send, statement, Answer,
    Endpoint, AUTHORIZER_FORBIDDEN_MESSAGE, FRONT_DOOR_FORBIDDEN_MESSAGE,
};

/// Where the function operations are bound (`POST /2015-03-31/functions`), which the policy
/// operations hang off.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// Where the function-URL configuration operations are bound
/// (`smithy_query(operation="GetFunctionUrlConfig")`: `/2021-10-31/functions/{FunctionName}/url`).
const URL_CONFIG_URI: &str = "/2021-10-31/functions";

/// The account id every emulator ARN carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// The Region this service's endpoint names, which a credential is signed for.
const REGION: &str = "us-east-1";

/// The signing name the emulator verifies a request to this service under.
const SIGNING_NAME: &str = "lambda";

/// The path within the function URL every probe below asks for.
const PROBE_PATH: &str = "/probe";

/// What the handler answers, so a request served BY THE FUNCTION is distinguishable from one
/// the endpoint answered itself.
const MARKER: &str = "the-url-handler-ran";

/// The action a function URL's own invocation is granted under, which the IAM Service
/// Authorization Reference publishes as this endpoint's own.
const INVOKE_FUNCTION_URL: &str = "lambda:InvokeFunctionUrl";

/// The action the Lambda API's `Invoke` is granted under, which is NOT the endpoint's.
const INVOKE_FUNCTION: &str = "lambda:InvokeFunction";

/// The member either refusal carries its sentence in.
const MESSAGE_MEMBER: &str = "Message";

/// The class a refusal of this endpoint names in `x-amzn-errortype`.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// See [`ACCESS_DENIED`].
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// The handler the function below runs.
fn handler() -> String {
    format!(
        "def lambda_handler(event, context):\n    \
         return {{'statusCode': 200, 'body': '{MARKER}'}}\n"
    )
}

/// The principal the narrowly scoped policy names: ONE role of this account, and never the
/// wildcard — the policy a client keeps a `NONE` URL private with.
fn named_role() -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/url-caller")
}

/// What the endpoint must answer one probe with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The function's own answer: the request was admitted and the handler RAN.
    Served,
    /// The URL authorizer's own `403` document: an authorization decision was taken on the
    /// function's policy and admitted nobody.
    AuthorizerRefusal,
    /// The front door's own shorter `403`: nothing authenticated the caller, so no
    /// authorization decision was reached for it at all.
    FrontDoorRefusal,
}

/// Which caller made a probe, for the failure messages.
const ANONYMOUS_CALLER: &str = "the caller presenting no credential";
/// See [`ANONYMOUS_CALLER`].
const SIGNED_CALLER: &str = "the caller presenting a verified credential";

/// One step: the change a client makes to the endpoint or to the policy, and what each of the
/// two callers must then read.
struct Step {
    case: &'static str,
    /// The `AuthType` an `UpdateFunctionUrlConfig` puts in force before this step's probes, or
    /// `None` for a step that changes no configuration.
    configured: Option<&'static str>,
    /// The statements `AddPermission` adds before this step's probes.
    granted: Vec<Value>,
    /// The statement ids `RemovePermission` takes away before this step's probes.
    revoked: Vec<&'static str>,
    /// What the endpoint must answer a request presenting no credential.
    unsigned: Expected,
    /// What it must answer the same request correctly signed.
    signed: Expected,
}

/// The `AuthType` the URL is created with, which the first step probes.
const CREATED_AUTH_TYPE: &str = "NONE";

/// The steps, in the order one client applies them.
fn steps() -> Vec<Step> {
    vec![
        Step {
            // The recorded case: a `NONE` URL whose only grants name this caller's own
            // principal, with no statement naming the wildcard.
            case: "NONE, granted only to a named principal",
            configured: None,
            granted: vec![
                statement("sUrl", INVOKE_FUNCTION_URL, &named_role(), "NONE"),
                json!({
                    "StatementId": "sInvoke",
                    "Action": INVOKE_FUNCTION,
                    "Principal": named_role(),
                    "InvokedViaFunctionUrl": true,
                }),
            ],
            revoked: Vec::new(),
            unsigned: Expected::AuthorizerRefusal,
            signed: Expected::Served,
        },
        Step {
            case: "NONE, the endpoint's action then granted to everyone for the type in force",
            configured: None,
            granted: vec![statement("sPublic", INVOKE_FUNCTION_URL, "*", "NONE")],
            revoked: Vec::new(),
            unsigned: Expected::Served,
            signed: Expected::Served,
        },
        Step {
            case: "NONE, that wildcard grant taken away again",
            configured: None,
            granted: Vec::new(),
            revoked: vec!["sPublic"],
            unsigned: Expected::AuthorizerRefusal,
            signed: Expected::Served,
        },
        Step {
            case: "switched to AWS_IAM, the named principal granted for that type",
            configured: Some("AWS_IAM"),
            granted: vec![statement("sIam", INVOKE_FUNCTION_URL, &named_role(), "AWS_IAM")],
            revoked: Vec::new(),
            // An `AWS_IAM` URL is "restrict[ed] to authenticated users only", so a request
            // presenting no credential reaches no authorization decision at all.
            unsigned: Expected::FrontDoorRefusal,
            signed: Expected::Served,
        },
        Step {
            case: "switched back to NONE while the only wildcard grant is scoped to AWS_IAM",
            configured: Some("NONE"),
            granted: vec![statement("sPublicIam", INVOKE_FUNCTION_URL, "*", "AWS_IAM")],
            revoked: Vec::new(),
            unsigned: Expected::AuthorizerRefusal,
            signed: Expected::Served,
        },
        Step {
            case: "NONE, the API's own action granted to everyone, which is not the endpoint's",
            configured: None,
            granted: vec![statement("sApi", INVOKE_FUNCTION, "*", "NONE")],
            revoked: Vec::new(),
            unsigned: Expected::AuthorizerRefusal,
            signed: Expected::Served,
        },
        Step {
            case: "NONE, the endpoint's action re-granted to everyone for the type in force",
            configured: None,
            granted: vec![statement("sPublicNone", INVOKE_FUNCTION_URL, "*", "NONE")],
            revoked: Vec::new(),
            unsigned: Expected::Served,
            signed: Expected::Served,
        },
    ]
}

/// A router over the same engine that VERIFIES every request signature, as the emulator does
/// when it is started with request authentication on.
fn authenticating_router(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = router_on(engine);
    router.set_request_authentication(true);
    router
}

/// `UpdateFunctionUrlConfig(AuthType=auth_type)` on `function`, stating that member alone.
async fn update_auth_type(router: &MultiServiceRouter, function: &str, auth_type: &str) {
    let updated = control(
        router,
        "PUT",
        &format!("{URL_CONFIG_URI}/{function}/url"),
        &json!({ "AuthType": auth_type }),
    )
    .await;
    assert_eq!(
        updated.status,
        200,
        "UpdateFunctionUrlConfig(AuthType={auth_type:?}) must succeed, got {}",
        updated.summary()
    );
}

/// `RemovePermission(StatementId=sid)` on `function`.
async fn revoke(router: &MultiServiceRouter, function: &str, sid: &str) {
    let removed = control(
        router,
        "DELETE",
        &format!("{FUNCTIONS_URI}/{function}/policy/{sid}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        removed.status,
        204,
        "RemovePermission({sid:?}) must succeed, got {}",
        removed.summary()
    );
}

/// What `GetFunctionUrlConfig` reports for `function`: the address the URL is published at and
/// the `AuthType` in force, read back where a client reads them.
async fn reported_config(router: &MultiServiceRouter, function: &str) -> (String, String) {
    let read = control(
        router,
        "GET",
        &format!("{URL_CONFIG_URI}/{function}/url"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "GetFunctionUrlConfig on {function:?} must succeed, got {}",
        read.summary()
    );
    let member = |name: &str| -> String {
        read.json()
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                panic!(
                    "GetFunctionUrlConfig must report {name}, got {}",
                    read.summary()
                )
            })
    };
    (member("FunctionUrl"), member("AuthType"))
}

/// The headers one SigV4-signed request to `authority` travels with: the signature the
/// workspace's own signer computed over that exact request with the credential this emulator
/// publishes, which is the recipe the emulator's verifier re-signs with.
async fn signature_for(authority: &str, method: &str, target: &str) -> Vec<(String, String)> {
    let keys = ::emulator_server::request_authentication::SigV4Authenticator::for_this_process();
    let mut request = ::http::Request::builder()
        .method(method)
        .uri(format!("https://{authority}{target}"))
        .body(Vec::new())
        .expect("the test request must build");
    ::service_common::in_process_sigv4::sign_in_process_request_as(
        &mut request,
        SIGNING_NAME,
        REGION,
        &keys.access_key_id,
        &keys.secret_access_key,
    )
    .await
    .expect("the test request must sign");
    request
        .headers()
        .iter()
        .filter(|(name, _)| name.as_str() != "host")
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// One request to `endpoint` presenting NO credential: no `Authorization`, no `X-Amz-Date`, no
/// session token. This is the request a stock unsigned client sends.
async fn unsigned_probe(router: &MultiServiceRouter, endpoint: &Endpoint) -> Answer {
    send(router, "GET", &endpoint.host, PROBE_PATH, &[], &[]).await
}

/// The same request, correctly signed with the credential this emulator publishes.
async fn signed_probe(router: &MultiServiceRouter, endpoint: &Endpoint) -> Answer {
    let carried = signature_for(&endpoint.host, "GET", PROBE_PATH).await;
    let headers: Vec<(&str, &str)> = carried
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    send(router, "GET", &endpoint.host, PROBE_PATH, &[], &headers).await
}

/// What `expected` requires of `answer`, asserted whole: the status, the failure class, the
/// body, and whether the handler ran for it.
fn assert_answer(case: &str, caller: &str, expected: Expected, answer: &Answer) {
    let sentence = match expected {
        Expected::Served => {
            assert_eq!(
                answer.status,
                200,
                "case {case:?} / {caller}: expected the function's own answer, got {}",
                answer.summary()
            );
            assert_eq!(
                answer.text(),
                MARKER,
                "case {case:?} / {caller}: expected the handler's body {MARKER:?}, got {}",
                answer.summary()
            );
            assert_eq!(
                answer.header(ERROR_TYPE_HEADER),
                None,
                "case {case:?} / {caller}: expected no failure class at all, got {:?}",
                answer.header(ERROR_TYPE_HEADER)
            );
            return;
        }
        Expected::AuthorizerRefusal => AUTHORIZER_FORBIDDEN_MESSAGE,
        Expected::FrontDoorRefusal => FRONT_DOOR_FORBIDDEN_MESSAGE,
    };
    assert_eq!(
        answer.status,
        403,
        "case {case:?} / {caller}: expected HTTP 403, got {}",
        answer.summary()
    );
    assert_eq!(
        answer.header(ERROR_TYPE_HEADER).as_deref(),
        Some(ACCESS_DENIED),
        "case {case:?} / {caller}: expected the class {ACCESS_DENIED} in {ERROR_TYPE_HEADER}, \
         got {:?}",
        answer.header(ERROR_TYPE_HEADER)
    );
    assert_eq!(
        answer.json(),
        json!({ MESSAGE_MEMBER: sentence }),
        "case {case:?} / {caller}: expected the refusal document {:?}, got {}",
        json!({ MESSAGE_MEMBER: sentence }).to_string(),
        answer.text()
    );
    assert!(
        !answer.text().contains(MARKER),
        "case {case:?} / {caller}: expected nothing of the function's own answer, got {}",
        answer.summary()
    );
}

/// RULE: the admission decision is taken per request against the configuration and the policy
/// as they stand, so one published address admits a different set of callers after every
/// configuration update and every grant added or taken away — and an anonymous caller is
/// admitted exactly while a statement names the wildcard principal for the `AuthType` in
/// force.
#[tokio::test]
async fn one_endpoint_admits_the_callers_its_current_config_and_policy_name() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    // The resources are built through a router that verifies nothing, as the other suites of
    // this plane build them; every probe goes through one that verifies each signature.
    let setup = router_on(&engine);
    let function = "url-auth-transition";
    given_function(&setup, function, &handler()).await;
    let endpoint =
        given_configured_url(&setup, function, None, CREATED_AUTH_TYPE, &Value::Null).await;
    let router = authenticating_router(&engine);

    let published = endpoint.host.clone();
    let mut in_force = CREATED_AUTH_TYPE.to_string();
    for step in steps() {
        if let Some(configured) = step.configured {
            update_auth_type(&setup, function, configured).await;
            in_force = configured.to_string();
        }
        for statement in &step.granted {
            grant(&setup, function, None, statement).await;
        }
        for sid in &step.revoked {
            revoke(&setup, function, sid).await;
        }

        // The configuration a client reads back names the type now in force at the SAME
        // address: an update re-decides who is admitted, and mints no new endpoint.
        let (reported_url, reported_auth_type) = reported_config(&setup, function).await;
        assert_eq!(
            reported_auth_type, in_force,
            "case {:?}: expected GetFunctionUrlConfig to report AuthType {in_force:?}, got {:?}",
            step.case, reported_auth_type
        );
        assert!(
            reported_url.contains(&published),
            "case {:?}: expected the published address {published:?} to be unchanged, got {:?}",
            step.case,
            reported_url
        );

        let anonymous = unsigned_probe(&router, &endpoint).await;
        assert_answer(step.case, ANONYMOUS_CALLER, step.unsigned, &anonymous);
        let credentialled = signed_probe(&router, &endpoint).await;
        assert_answer(step.case, SIGNED_CALLER, step.signed, &credentialled);
    }
}

/// How many times each probe below is repeated: a client polling for the refusal reads the
/// same answer every time, so a handful of consecutive calls is what shows the decision is not
/// a one-off.
const REPEATS: usize = 3;

/// RULE, read as an invariant: the decision is a pure READ of the configuration and the
/// policy, so probing the endpoint does not change what a later caller reads — the same
/// request repeated against one unchanged state answers the same document every time, for the
/// caller it refuses and for the caller it serves alike.
#[tokio::test]
async fn repeated_probes_of_one_unchanged_state_answer_the_same() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let setup = router_on(&engine);
    let function = "url-auth-repeated";
    given_function(&setup, function, &handler()).await;
    let endpoint =
        given_configured_url(&setup, function, None, CREATED_AUTH_TYPE, &Value::Null).await;
    // The narrowly scoped policy of the recorded case: one named principal, no wildcard.
    grant(
        &setup,
        function,
        None,
        &statement("sUrl", INVOKE_FUNCTION_URL, &named_role(), "NONE"),
    )
    .await;
    let router = authenticating_router(&engine);

    let mut refused: Vec<String> = Vec::new();
    let mut served: Vec<String> = Vec::new();
    for attempt in 1..=REPEATS {
        let case = format!("attempt {attempt} of {REPEATS} against one unchanged state");
        let anonymous = unsigned_probe(&router, &endpoint).await;
        assert_answer(
            &case,
            ANONYMOUS_CALLER,
            Expected::AuthorizerRefusal,
            &anonymous,
        );
        refused.push(anonymous.text());
        let credentialled = signed_probe(&router, &endpoint).await;
        assert_answer(&case, SIGNED_CALLER, Expected::Served, &credentialled);
        served.push(credentialled.text());
    }
    assert!(
        refused.windows(2).all(|pair| pair[0] == pair[1]),
        "case \"consecutive refusals of one state\": expected one document every time, got \
         {refused:?}"
    );
    assert!(
        served.windows(2).all(|pair| pair[0] == pair[1]),
        "case \"consecutive admissions of one state\": expected one answer every time, got \
         {served:?}"
    );
}
