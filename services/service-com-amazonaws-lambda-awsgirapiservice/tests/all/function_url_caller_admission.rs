// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH CALLER a function URL whose `AuthType` is `NONE` admits.
//!
//! The rule under test: a request that arrives at a function URL is an invocation of the
//! function, decided at the IAM layer under the endpoint's own action
//! (`lambda:InvokeFunctionUrl`) against the function's resource-based policy for the
//! qualifier the URL is attached to. `AuthType=NONE` "bypass[es] IAM authentication to create
//! a public endpoint", so WHO the request is decided as follows from the credential it
//! presented, which is the same fact the emulator decided its AUTHENTICATION by:
//!
//! * a request presenting NO credential is authenticated by nothing and speaks for nobody, so
//!   the only `Principal` element that can name it is the wildcard — a policy whose grants all
//!   name a principal, a grant of another action, a grant scoped to the other `AuthType`, and
//!   no policy at all each admit it nowhere, and it is refused the URL authorizer's own `403`
//!   document with nothing of the function run for it;
//! * a request PRESENTING a credential is served by the mounting that declares the SigV4
//!   scheme, so the emulator verifies that credential and a signature it cannot verify is
//!   turned away before the endpoint decides anything; a verified credential then speaks for
//!   the principal it belongs to — the account's own, which holds no IAM policies of its own,
//!   for the one credential this emulator publishes — and is served.
//!
//! The rows span what the rule generalises over: whether the request carries a credential,
//! which principal form the grant names (the wildcard, an IAM user ARN, an account id, a
//! service principal), which ACTION it grants, which `AuthType` the grant is scoped to, and
//! whether the URL is attached to the unpublished version or to an alias — where the policy of
//! THAT qualifier is the one that decides. The admission table runs against a router that
//! verifies no signature, so its rows isolate the POLICY decision; the credential's own
//! verification is pinned by the suite's last test, against a router that does verify.
//!
//! Every row drives a real HTTP request through the router against a function whose deployment
//! package RUNS, so a refusal that let the handler run, or an admission that answered without
//! it, fails here.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_url_data_plane::{
    control, given_configured_url, given_function, grant, router_on, send, statement, Answer,
    Endpoint,
};

/// Where the function operations are bound (`POST /2015-03-31/functions`).
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The account id every emulator ARN carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// A SigV4 credential scope naming this service's signing name (`lambda`) in `us-east-1`:
/// what a request that PRESENTS a credential carries. Nothing in it is verified — the harness
/// turns request authentication off — so what the rows below vary is whether a credential is
/// presented at all.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=emulator-test-key/20260101/us-east-1/lambda/aws4_request, \
     SignedHeaders=host, Signature=not-verified";

/// The body the URL's own AUTHORIZER answers a caller the authorization decision does not
/// admit with: the one sentence this target states once
/// ([`crate::function_url_data_plane::AUTHORIZER_FORBIDDEN_MESSAGE`]), in the member the
/// refusal carries it in. The document and the class of request it covers are asserted in
/// `function_url_authorizer_refusal`.
fn authorizer_forbidden_body() -> String {
    json!({ "Message": crate::function_url_data_plane::AUTHORIZER_FORBIDDEN_MESSAGE }).to_string()
}

/// The body Lambda's own front door answers a caller it did not AUTHENTICATE with, from the
/// sentence this target states once
/// ([`crate::function_url_data_plane::FRONT_DOOR_FORBIDDEN_MESSAGE`]).
fn front_door_forbidden_body() -> String {
    json!({ "Message": crate::function_url_data_plane::FRONT_DOOR_FORBIDDEN_MESSAGE }).to_string()
}

/// The class that refusal names in `x-amzn-ErrorType`: the common AWS access error, which is
/// what AWS answers a function URL request no policy admits.
const FORBIDDEN_ERROR_TYPE: &str = "AccessDeniedException";

/// The header every answer of a function URL names the request it answered in.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// What the handler every function below runs answers, so an admitted request is visibly
/// served BY THE FUNCTION and a refused one visibly is not.
const SERVED_BODY: &str = "served by the function";

/// The handler every function below runs.
fn handler() -> String {
    format!(
        "def lambda_handler(event, context):\n    \
         return {{'statusCode': 200, 'body': '{SERVED_BODY}'}}\n"
    )
}

/// The action a function URL's own invocation is granted under.
const INVOKE_FUNCTION_URL: &str = "lambda:InvokeFunctionUrl";

/// The action the Lambda API's `Invoke` is granted under, which is NOT the endpoint's.
const INVOKE_FUNCTION: &str = "lambda:InvokeFunction";

/// One request to `endpoint`, carrying a credential or carrying none.
async fn request(
    router: &MultiServiceRouter,
    endpoint: &Endpoint,
    credentialled: bool,
) -> Answer {
    let headers: Vec<(&str, &str)> = if credentialled {
        vec![("authorization", AUTHORIZATION), ("x-amz-date", "20260101T000000Z")]
    } else {
        Vec::new()
    };
    send(router, "GET", &endpoint.host, "/probe", &[], &headers).await
}

/// One row of the admission table.
struct Row {
    case: &'static str,
    /// The function the row stands up, so each row is decided on its own policy.
    function: &'static str,
    /// The statement the function's policy carries, or none at all.
    statement: Option<Value>,
    /// Whether the request presents a credential.
    credentialled: bool,
    /// The status the endpoint answers.
    status: u16,
}

/// RULE: a `NONE` function URL admits exactly the caller the function's resource-based policy
/// allows the endpoint's own action for, and refuses every other caller with the URL
/// authorizer's own `403` document and nothing of the function run.
#[tokio::test]
async fn a_none_endpoint_admits_the_caller_its_policy_names() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let named_user = format!("arn:aws:iam::{ACCOUNT}:user/someone");
    let rows: Vec<Row> = vec![
        Row {
            case: "no policy at all, unsigned",
            function: "admit-none",
            statement: None,
            credentialled: false,
            status: 403,
        },
        Row {
            case: "the endpoint's action granted to everyone, unsigned",
            function: "admit-public",
            statement: Some(statement("public", INVOKE_FUNCTION_URL, "*", "NONE")),
            credentialled: false,
            status: 200,
        },
        Row {
            case: "the endpoint's action granted to a named IAM user, unsigned",
            function: "admit-named-user",
            statement: Some(statement("named", INVOKE_FUNCTION_URL, &named_user, "NONE")),
            credentialled: false,
            status: 403,
        },
        Row {
            case: "the endpoint's action granted to the account, unsigned",
            function: "admit-account",
            statement: Some(statement("account", INVOKE_FUNCTION_URL, ACCOUNT, "NONE")),
            credentialled: false,
            status: 403,
        },
        Row {
            case: "the endpoint's action granted to a service principal, unsigned",
            function: "admit-service",
            statement: Some(statement(
                "service",
                INVOKE_FUNCTION_URL,
                "events.amazonaws.com",
                "NONE",
            )),
            credentialled: false,
            status: 403,
        },
        Row {
            case: "the API action granted to everyone, which is not the endpoint's, unsigned",
            function: "admit-wrong-action",
            statement: Some(statement("api", INVOKE_FUNCTION, "*", "NONE")),
            credentialled: false,
            status: 403,
        },
        Row {
            case: "the endpoint's action granted to everyone for the OTHER auth type, unsigned",
            function: "admit-wrong-auth-type",
            statement: Some(statement("scoped", INVOKE_FUNCTION_URL, "*", "AWS_IAM")),
            credentialled: false,
            status: 403,
        },
        Row {
            case: "no policy at all, presenting the account's own credential",
            function: "admit-signed-ungranted",
            statement: None,
            credentialled: true,
            status: 200,
        },
        Row {
            case: "granted to another principal, presenting the account's own credential",
            function: "admit-signed-named",
            statement: Some(statement("named", INVOKE_FUNCTION_URL, &named_user, "NONE")),
            credentialled: true,
            status: 200,
        },
    ];

    for row in &rows {
        given_function(&router, row.function, &handler()).await;
        let endpoint = given_configured_url(&router, row.function, None, "NONE", &Value::Null)
            .await;
        if let Some(statement) = &row.statement {
            grant(&router, row.function, None, statement).await;
        }
        let answer = request(&router, &endpoint, row.credentialled).await;
        assert_eq!(
            answer.status, row.status,
            "case {:?}: expected HTTP {}, got {}",
            row.case,
            row.status,
            answer.summary()
        );
        if row.status == 200 {
            assert_eq!(
                answer.text(),
                SERVED_BODY,
                "case {:?}: expected the function's own answer, got {}",
                row.case,
                answer.summary()
            );
        } else {
            assert_eq!(
                answer.text(),
                authorizer_forbidden_body(),
                "case {:?}: expected the endpoint's refusal and nothing of the function, got {}",
                row.case,
                answer.summary()
            );
            // The refusal names the class of the failure and the request it refused, as
            // every other answer of this endpoint names one.
            assert_eq!(
                answer.header("x-amzn-errortype").as_deref(),
                Some(FORBIDDEN_ERROR_TYPE),
                "case {:?}: expected the refusal to name {FORBIDDEN_ERROR_TYPE}, got {:?}",
                row.case,
                answer.header("x-amzn-errortype")
            );
            assert!(
                answer
                    .header(REQUEST_ID_HEADER)
                    .is_some_and(|id| !id.is_empty()),
                "case {:?}: expected the refusal to name the request it refused, got {:?}",
                row.case,
                answer.header(REQUEST_ID_HEADER)
            );
        }
    }
}

/// RULE: the policy that decides is the one of the QUALIFIER the URL is attached to, so a URL
/// on an alias is admitted by the alias's own grant and not by one made at the function level.
#[tokio::test]
async fn the_policy_of_the_qualifier_the_url_is_attached_to_decides() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    given_function(&router, "admit-alias", &handler()).await;
    let published = control(
        &router,
        "POST",
        &format!("{FUNCTIONS_URI}/admit-alias/versions"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        published.status,
        201,
        "PublishVersion must succeed, got {}",
        published.summary()
    );
    let alias = control(
        &router,
        "POST",
        &format!("{FUNCTIONS_URI}/admit-alias/aliases"),
        &json!({ "Name": "live", "FunctionVersion": "1" }),
    )
    .await;
    assert_eq!(
        alias.status,
        201,
        "CreateAlias must succeed, got {}",
        alias.summary()
    );
    let endpoint =
        given_configured_url(&router, "admit-alias", Some("live"), "NONE", &Value::Null).await;

    // A grant made at the FUNCTION level is attached to the function's own ARN, which the
    // alias the URL is attached to is not: the anonymous caller is still refused.
    grant(
        &router,
        "admit-alias",
        None,
        &statement("function-level", INVOKE_FUNCTION_URL, "*", "NONE"),
    )
    .await;
    let refused = request(&router, &endpoint, false).await;
    assert_eq!(
        (refused.status, refused.text()),
        (403, authorizer_forbidden_body()),
        "case \"a function-level grant on an alias-attached URL\": expected the refusal, got {}",
        refused.summary()
    );

    // The same grant made on the ALIAS admits it.
    grant(
        &router,
        "admit-alias",
        Some("live"),
        &statement("alias-level", INVOKE_FUNCTION_URL, "*", "NONE"),
    )
    .await;
    let served = request(&router, &endpoint, false).await;
    assert_eq!(
        (served.status, served.text()),
        (200, SERVED_BODY.to_string()),
        "case \"the alias's own grant\": expected the function's answer, got {}",
        served.summary()
    );
}

/// RULE: a credential presented to a `NONE` endpoint is one the emulator VERIFIES — the
/// mounting serving such a request declares the SigV4 scheme — so a signature it cannot
/// verify is turned away before the endpoint decides anything, while the same endpoint still
/// serves a request that presents no credential at all while its policy grants the invocation
/// to everyone.
#[tokio::test]
async fn a_credential_presented_to_a_public_endpoint_is_verified() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    // The resources are built through a router that verifies nothing, as every other suite
    // builds them; the probes below go through one that verifies every signature.
    let router = router_on(&engine);
    given_function(&router, "admit-verified", &handler()).await;
    let endpoint =
        given_configured_url(&router, "admit-verified", None, "NONE", &Value::Null).await;
    grant(
        &router,
        "admit-verified",
        None,
        &statement("public", INVOKE_FUNCTION_URL, "*", "NONE"),
    )
    .await;

    let mut authenticating = router_on(&engine);
    authenticating.set_request_authentication(true);

    // A credential the emulator cannot verify: the request is turned away, and nothing of the
    // function is answered for it — even though the endpoint's own policy admits everyone.
    let unverifiable = send(
        &authenticating,
        "GET",
        &endpoint.host,
        "/probe",
        &[],
        &[
            ("authorization", AUTHORIZATION),
            ("x-amz-date", "20260101T000000Z"),
        ],
    )
    .await;
    assert_eq!(
        (unverifiable.status, unverifiable.text()),
        (403, front_door_forbidden_body()),
        "case \"a credential that does not verify\": expected the front door's refusal, got {}",
        unverifiable.summary()
    );

    // And the same endpoint, reached with no credential at all, is served by the function:
    // what admits that caller is the policy's grant to everyone.
    let served = send(&authenticating, "GET", &endpoint.host, "/probe", &[], &[]).await;
    assert_eq!(
        (served.status, served.text()),
        (200, SERVED_BODY.to_string()),
        "case \"no credential at all\": expected the function's own answer, got {}",
        served.summary()
    );
}
