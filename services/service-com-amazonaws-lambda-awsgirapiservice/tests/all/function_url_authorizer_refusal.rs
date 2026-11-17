// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the DOCUMENT a function URL's own authorizer refuses a caller with.
//!
//! RULE: a request to a function URL that the URL's authorizer does not admit — the decision
//! on the function's resource-based policy for the endpoint's own action
//! (`lambda:InvokeFunctionUrl`) at the qualifier the URL is attached to — is answered HTTP
//! 403 with `x-amzn-errortype: AccessDeniedException` and the one document AWS words that
//! refusal with: a single `Message` member carrying the word `Forbidden` AND the fixed
//! sentence pointing at the Function-URL authorization documentation. The document is fixed
//! WHOLE: no part of it is taken from the request, the caller, the function or the policy
//! that did not admit it, so every refusal of every request carries the same bytes, and the
//! handler never runs for it.
//!
//! A request the emulator did not AUTHENTICATE is a different refusal and keeps the front
//! door's own shorter sentence: no authorization decision was reached for a caller that
//! presented no credential the emulator could judge, so it is told nothing of authorization.
//! The last test pins that contrast, so neither refusal can drift into the other.
//!
//! The rows differ along what the rule generalises over: the REQUEST (method, path, query,
//! body, the headers it carries, and the same request repeated), the POLICY state that fails
//! to admit the caller (none at all, a grant naming a principal, a grant naming the account,
//! a grant of another action, a grant scoped to the other `AuthType`), and the QUALIFIER the
//! URL is attached to (the unpublished version, and an alias whose own policy carries no
//! grant). Every row drives a real HTTP request through the router against a function whose
//! deployment package RUNS, so a refusal that let the handler run fails here.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::function_url_data_plane::{
    control, given_configured_url, given_function, grant, router_on, send, statement, Answer,
    AUTHORIZER_FORBIDDEN_MESSAGE, FRONT_DOOR_FORBIDDEN_MESSAGE,
};

/// Where the function operations are bound (`POST /2015-03-31/functions`).
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The account id every emulator ARN carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// The member either refusal carries its sentence in.
const MESSAGE_MEMBER: &str = "Message";

/// The class a refusal of this endpoint names in `x-amzn-errortype`.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// See [`ACCESS_DENIED`].
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// The header every answer of a function URL names the request it answered in.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// What the handler answers, so an admitted request is visibly served BY THE FUNCTION and a
/// refused one visibly is not.
const MARKER: &str = "the-url-handler-ran";

/// The action a function URL's own invocation is granted under.
const INVOKE_FUNCTION_URL: &str = "lambda:InvokeFunctionUrl";

/// The action the Lambda API's `Invoke` is granted under, which is NOT the endpoint's.
const INVOKE_FUNCTION: &str = "lambda:InvokeFunction";

/// The handler every function below runs.
fn handler() -> String {
    format!(
        "def lambda_handler(event, context):\n    \
         return {{'statusCode': 200, 'body': '{MARKER}'}}\n"
    )
}

/// The document the authorizer owes a refusal, asserted whole: the status, the class, the one
/// member and its whole sentence, an id for the request it refused, and nothing of the run.
fn assert_authorizer_refusal(case: &str, answer: &Answer) {
    assert_eq!(
        answer.status, 403,
        "case {case:?}: expected HTTP 403, got {}",
        answer.summary()
    );
    assert_eq!(
        answer.header(ERROR_TYPE_HEADER).as_deref(),
        Some(ACCESS_DENIED),
        "case {case:?}: expected the class {ACCESS_DENIED} in {ERROR_TYPE_HEADER}, got {:?}",
        answer.header(ERROR_TYPE_HEADER)
    );
    assert_eq!(
        answer.json(),
        json!({ MESSAGE_MEMBER: AUTHORIZER_FORBIDDEN_MESSAGE }),
        "case {case:?}: expected the authorizer's whole refusal document, got {}",
        answer.text()
    );
    assert!(
        answer
            .header(REQUEST_ID_HEADER)
            .is_some_and(|id| !id.is_empty()),
        "case {case:?}: expected the refusal to name the request it refused, got {:?}",
        answer.header(REQUEST_ID_HEADER)
    );
    assert!(
        !answer.text().contains(MARKER),
        "case {case:?}: expected nothing of the function's own answer, got {}",
        answer.summary()
    );
}

/// One probed request: the refusal's document is the same bytes whatever the request was.
struct Probe<'p> {
    case: &'p str,
    method: &'p str,
    path: &'p str,
    body: &'p [u8],
    headers: &'p [(&'p str, &'p str)],
}

/// RULE: the authorizer's refusal document is fixed whole, so every request the authorizer
/// turns away — whatever method, path, query, body or headers it carried, and however many
/// times the same one is repeated — reads exactly the same document.
#[tokio::test]
async fn an_unadmitted_request_reads_the_authorizers_whole_forbidden_document() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    given_function(&router, "authz-document", &handler()).await;
    let endpoint =
        given_configured_url(&router, "authz-document", None, "NONE", &Value::Null).await;
    // The recorded case: the only grant of the endpoint's action names ONE principal, so no
    // anonymous caller is admitted.
    grant(
        &router,
        "authz-document",
        None,
        &statement(
            "named",
            INVOKE_FUNCTION_URL,
            &format!("arn:aws:iam::{ACCOUNT}:user/someone"),
            "NONE",
        ),
    )
    .await;

    let probes: &[Probe<'_>] = &[
        Probe {
            case: "a bare GET of the root",
            method: "GET",
            path: "/",
            body: b"",
            headers: &[],
        },
        Probe {
            case: "a GET of a deep path with a query",
            method: "GET",
            path: "/orders/42?expand=items&page=2",
            body: b"",
            headers: &[],
        },
        Probe {
            case: "a POST carrying a JSON body",
            method: "POST",
            path: "/orders",
            body: br#"{"quantity":3}"#,
            headers: &[("content-type", "application/json")],
        },
        Probe {
            case: "a PUT carrying bytes and a client header",
            method: "PUT",
            path: "/orders/42",
            body: b"not json at all",
            headers: &[("content-type", "text/plain"), ("x-asked", "something")],
        },
        Probe {
            case: "a DELETE carrying an origin a browser set same-origin",
            method: "DELETE",
            path: "/orders/42",
            body: b"",
            headers: &[
                ("origin", "https://allowed.example"),
                ("sec-fetch-site", "same-origin"),
            ],
        },
        Probe {
            case: "a HEAD of the root",
            method: "HEAD",
            path: "/",
            body: b"",
            headers: &[],
        },
    ];
    for probe in probes {
        let answer = send(
            &router,
            probe.method,
            &endpoint.host,
            probe.path,
            probe.body,
            probe.headers,
        )
        .await;
        assert_authorizer_refusal(probe.case, &answer);
    }

    // The SAME request, three times over: a refusal names a fresh request id and is otherwise
    // the same document, so a client matching on the body reads one answer however often it
    // calls.
    let mut seen: Vec<String> = Vec::new();
    for attempt in 1..=3 {
        let case = format!("consecutive refusal {attempt} of the same request");
        let answer = send(&router, "GET", &endpoint.host, "/probe", &[], &[]).await;
        assert_authorizer_refusal(&case, &answer);
        seen.push(answer.text());
    }
    assert!(
        seen.windows(2).all(|pair| pair[0] == pair[1]),
        "case \"three consecutive refusals\": expected one document every time, got {seen:?}"
    );

    // The same refusal through an emulator that VERIFIES request signatures, as it does when
    // it is started with request authentication on: a request presenting no credential is
    // authenticated by nothing on a `NONE` URL, so it still reaches the authorizer and reads
    // the authorizer's document rather than the front door's.
    let mut authenticating = router_on(&engine);
    authenticating.set_request_authentication(true);
    let answer = send(&authenticating, "GET", &endpoint.host, "/probe", &[], &[]).await;
    assert_authorizer_refusal("an unsigned request while signatures are verified", &answer);
}

/// One policy state the caller is refused under, applied on top of the ones before it.
struct Step<'s> {
    case: &'s str,
    /// The statement added before this probe, or none for the state with no policy at all.
    statement: Option<Value>,
}

/// RULE: the document says nothing of WHY the caller was not admitted, so every policy state
/// that admits no anonymous caller — none at all, a grant naming a principal, a grant naming
/// the account, a grant of another action, a grant scoped to the other `AuthType` — is
/// refused with the same document; the grant that DOES admit it then serves the function's
/// own answer, so the refusals above are not a probe that could never be served.
#[tokio::test]
async fn every_policy_that_admits_nobody_is_refused_with_the_same_document() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    given_function(&router, "authz-policies", &handler()).await;
    let endpoint =
        given_configured_url(&router, "authz-policies", None, "NONE", &Value::Null).await;
    let named_user = format!("arn:aws:iam::{ACCOUNT}:user/someone");

    let steps: Vec<Step<'_>> = vec![
        Step {
            case: "no policy at all",
            statement: None,
        },
        Step {
            case: "the endpoint's action granted to a named IAM user",
            statement: Some(statement("named", INVOKE_FUNCTION_URL, &named_user, "NONE")),
        },
        Step {
            case: "the endpoint's action granted to the account",
            statement: Some(statement("account", INVOKE_FUNCTION_URL, ACCOUNT, "NONE")),
        },
        Step {
            case: "the endpoint's action granted to a service principal",
            statement: Some(statement(
                "service",
                INVOKE_FUNCTION_URL,
                "events.amazonaws.com",
                "NONE",
            )),
        },
        Step {
            case: "the API's own action granted to everyone, which is not the endpoint's",
            statement: Some(statement("api", INVOKE_FUNCTION, "*", "NONE")),
        },
        Step {
            case: "the endpoint's action granted to everyone for the other auth type",
            statement: Some(statement("scoped", INVOKE_FUNCTION_URL, "*", "AWS_IAM")),
        },
    ];
    for step in &steps {
        if let Some(statement) = &step.statement {
            grant(&router, "authz-policies", None, statement).await;
        }
        let answer = send(&router, "GET", &endpoint.host, "/probe", &[], &[]).await;
        assert_authorizer_refusal(step.case, &answer);
    }

    // The grant that admits the anonymous caller: the same probe is now served by the
    // function.
    grant(
        &router,
        "authz-policies",
        None,
        &statement("public", INVOKE_FUNCTION_URL, "*", "NONE"),
    )
    .await;
    let served = send(&router, "GET", &endpoint.host, "/probe", &[], &[]).await;
    assert_eq!(
        (served.status, served.text()),
        (200, MARKER.to_string()),
        "case \"the endpoint's action granted to everyone\": expected the function's own \
         answer, got {}",
        served.summary()
    );
}

/// RULE: the decision is taken on the policy of the QUALIFIER the URL is attached to, and a
/// refusal there carries the same document — the document names nothing of the qualifier
/// either.
#[tokio::test]
async fn a_refusal_on_an_alias_attached_url_carries_the_same_document() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    given_function(&router, "authz-alias", &handler()).await;
    let published = control(
        &router,
        "POST",
        &format!("{FUNCTIONS_URI}/authz-alias/versions"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        published.status, 201,
        "PublishVersion must succeed, got {}",
        published.summary()
    );
    let alias = control(
        &router,
        "POST",
        &format!("{FUNCTIONS_URI}/authz-alias/aliases"),
        &json!({ "Name": "live", "FunctionVersion": "1" }),
    )
    .await;
    assert_eq!(
        alias.status, 201,
        "CreateAlias must succeed, got {}",
        alias.summary()
    );
    let endpoint =
        given_configured_url(&router, "authz-alias", Some("live"), "NONE", &Value::Null).await;
    // A grant made at the FUNCTION level is attached to the function's own ARN, which is not
    // the alias the URL is attached to, so the anonymous caller is still not admitted.
    grant(
        &router,
        "authz-alias",
        None,
        &statement("function-level", INVOKE_FUNCTION_URL, "*", "NONE"),
    )
    .await;
    let answer = send(&router, "GET", &endpoint.host, "/probe", &[], &[]).await;
    assert_authorizer_refusal("a function-level grant on an alias-attached URL", &answer);
}

/// RULE: a caller the emulator did not AUTHENTICATE reached no authorization decision, so it
/// reads the front door's own shorter sentence and not the authorizer's document — the same
/// status and the same class, a different body.
#[tokio::test]
async fn an_unauthenticated_caller_reads_the_front_doors_own_sentence() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    given_function(&router, "authz-unsigned", &handler()).await;
    let restricted =
        given_configured_url(&router, "authz-unsigned", None, "AWS_IAM", &Value::Null).await;
    grant(
        &router,
        "authz-unsigned",
        None,
        &statement("iam", INVOKE_FUNCTION_URL, "*", "AWS_IAM"),
    )
    .await;
    // From here the SAME emulator verifies request signatures, as it does when it is started
    // with request authentication on.
    let mut authenticating = router_on(&engine);
    authenticating.set_request_authentication(true);

    let refused: Answer = send(
        &authenticating,
        "GET",
        &restricted.host,
        "/probe",
        &[],
        &[],
    )
    .await;
    let case = "an unsigned request to an AWS_IAM URL";
    assert_eq!(
        (refused.status, refused.header(ERROR_TYPE_HEADER)),
        (403, Some(ACCESS_DENIED.to_string())),
        "case {case:?}: expected the front door's 403 and its class, got {}",
        refused.summary()
    );
    assert_eq!(
        refused.json(),
        json!({ MESSAGE_MEMBER: FRONT_DOOR_FORBIDDEN_MESSAGE }),
        "case {case:?}: expected the front door's own sentence, got {}",
        refused.text()
    );
    assert!(
        !refused.text().contains(MARKER),
        "case {case:?}: expected nothing of the function's own answer, got {}",
        refused.summary()
    );
}
