// SPDX-License-Identifier: CC0-1.0

//! One account, four services: a long-term IAM access key whose stored `Status` the account
//! has moved off `Active` is not a credential any service accepts.
//!
//! RULE: an access key the account holds authenticates a request only while its stored
//! `Status` is `Active` — `UpdateAccessKey$Status` documents that "Active means that the key
//! can be used for programmatic calls to Amazon Web Services, while Inactive means that the
//! key cannot be used" — so a request signed with a key in any other status resolves to NO
//! principal: it is refused before any operation runs, with the serving service's own
//! unknown-credential error code (`InvalidClientTokenId` on the awsQuery identity services,
//! `UnrecognizedClientException` on an awsJson one) at HTTP 403 and with AWS's fixed sentence
//! for a credential that resolves to nothing, and no policy is consulted at all. The refusal
//! is not destructive: the same key, moved back to `Active`, is accepted again.
//!
//! Only a cross-service suite can pin it: the status lives in the IAM store and each of the
//! other services reads it through IAM's own accessor, so a crate-local suite can never drive
//! `CreateUser` / `CreateAccessKey` / `UpdateAccessKey` end to end into another service's
//! answer. Everything is driven over the wire, so what is under test is the records IAM wrote.
//!
//! The rows span what the rule generalises over: the SERVICE asked (the key's own identity
//! service, the token service, the key-management service and the secret store), the PROTOCOL
//! serving it (awsQuery and awsJson1_1, whose unknown-credential codes and envelopes differ),
//! the OPERATION asked for (two per service, so no row is a single handler's behaviour), and
//! the STATUS the key is moved to — `statusType`'s two non-`Active` values
//! (`smithy_query(query="shape", shape="statusType")` gives `Active`, `Inactive`, `Expired`),
//! each of which is "not usable", plus the move back to `Active` as the positive control.
//!
//! Beside the table, the boundaries: the user's OTHER key keeps working while one of its keys
//! is withdrawn, and a key id the account holds no record of is unaffected — so a refusal is
//! the presented key's own and never an account-wide outage.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

// The form encoder that builds an awsQuery body, and the reader that pulls one element's
// text out of an XML answer, are this test crate's own (`query_wire`).
use crate::query_wire::{form_encode, opt_element};

/// The Region every request here signs for.
const REGION: &str = "us-east-1";

/// The access key id the fixture calls present: a key the account holds no record of, which
/// is the position account root credentials are in — so the fixtures themselves are never
/// what a row refuses.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// How AWS words a credential that resolves to nothing: a fixed sentence carrying nothing
/// from the request.
const UNRESOLVED_CREDENTIAL_MESSAGE: &str = "The security token included in the request is invalid.";

/// The unknown-credential code an awsQuery identity service answers with.
const QUERY_CODE: &str = "InvalidClientTokenId";

/// The unknown-credential code an awsJson service answers with.
const JSON_CODE: &str = "UnrecognizedClientException";

/// The HTTP status a SigV4 authentication refusal carries.
const REFUSED_STATUS: u16 = 403;

/// Which wire a call is made over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wire {
    /// `awsQuery`: a form-encoded `POST /` whose `Action` names the operation.
    Query,
    /// `awsJson1_1`: a JSON `POST /` whose `X-Amz-Target` names it.
    Json,
}

/// One call a row makes.
#[derive(Debug, Clone, Copy)]
struct Call {
    wire: Wire,
    /// The service's signing name: what the credential scope names and what the router
    /// resolves the request to.
    signing_name: &'static str,
    /// The `X-Amz-Target` prefix an awsJson service is addressed by, which is the service
    /// shape's name rather than its signing name (`kms` is addressed as `TrentService`).
    target_prefix: &'static str,
    /// The operation asked for, as the wire names it.
    operation: &'static str,
    /// The body it carries: the smallest page or value the operation admits, as a client
    /// probing its own access asks for.
    body: &'static str,
}

/// The code and the HTTP status the service serving `call` refuses an unresolved credential
/// with.
fn expected_refusal(call: &Call) -> (u16, &'static str, &'static str) {
    let code = match call.wire {
        Wire::Query => QUERY_CODE,
        Wire::Json => JSON_CODE,
    };
    (REFUSED_STATUS, code, UNRESOLVED_CREDENTIAL_MESSAGE)
}

/// The eight reads the rows ask for: two per service, over both protocols.
fn calls() -> Vec<Call> {
    vec![
        Call {
            wire: Wire::Query,
            signing_name: "sts",
            target_prefix: "",
            operation: "GetCallerIdentity",
            body: "Version=2011-06-15&Action=GetCallerIdentity",
        },
        Call {
            wire: Wire::Query,
            signing_name: "sts",
            target_prefix: "",
            operation: "GetSessionToken",
            body: "Version=2011-06-15&Action=GetSessionToken",
        },
        Call {
            wire: Wire::Query,
            signing_name: "iam",
            target_prefix: "",
            operation: "ListUsers",
            body: "Version=2010-05-08&Action=ListUsers&MaxItems=1",
        },
        Call {
            wire: Wire::Query,
            signing_name: "iam",
            target_prefix: "",
            operation: "ListAccessKeys",
            body: "Version=2010-05-08&Action=ListAccessKeys&MaxItems=1",
        },
        Call {
            wire: Wire::Json,
            signing_name: "kms",
            target_prefix: "TrentService",
            operation: "ListKeys",
            body: r#"{"Limit":1}"#,
        },
        Call {
            wire: Wire::Json,
            signing_name: "kms",
            target_prefix: "TrentService",
            operation: "ListAliases",
            body: r#"{"Limit":1}"#,
        },
        Call {
            wire: Wire::Json,
            signing_name: "secretsmanager",
            target_prefix: "secretsmanager",
            operation: "ListSecrets",
            body: r#"{"MaxResults":1}"#,
        },
        Call {
            wire: Wire::Json,
            signing_name: "secretsmanager",
            target_prefix: "secretsmanager",
            operation: "GetRandomPassword",
            body: r#"{"PasswordLength":16}"#,
        },
    ]
}

/// IAM, STS, KMS and Secrets Manager over ONE engine, as `core/router/src/lib.rs` mounts
/// them: the access-key registry `iam:CreateAccessKey` writes and `iam:UpdateAccessKey`
/// updates is the one every other service resolves a presented key against.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The signatures below are not real ones; what names the caller is the access key id in
    // the credential scope, which is what these rows vary.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and whose
/// credential is `access_key_id` — the key a service reads the caller's identity from.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Make `call` presenting `access_key_id`, and answer `(status, body)`.
async fn dispatch(router: &MultiServiceRouter, access_key_id: &str, call: &Call) -> (u16, String) {
    let signing_name = call.signing_name;
    let mut request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(access_key_id, signing_name));
    request = match call.wire {
        Wire::Query => request.header("content-type", "application/x-www-form-urlencoded"),
        Wire::Json => request
            .header("content-type", "application/x-amz-json-1.1")
            .header(
                "x-amz-target",
                format!("{}.{}", call.target_prefix, call.operation),
            ),
    };
    let request = request
        .body(Bytes::from(call.body))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// The error code of a refusal, read out of the envelope the serving protocol wrote it in.
fn error_code(wire: Wire, body: &str) -> String {
    match wire {
        Wire::Query => opt_element(body, "Code").unwrap_or_else(|| "<no Code>".to_string()),
        Wire::Json => {
            let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
            let raw = parsed
                .get("__type")
                .and_then(Value::as_str)
                .unwrap_or("<no __type>")
                .to_string();
            raw.rsplit(['#', '/']).next().unwrap_or(&raw).to_string()
        }
    }
}

/// The message of a refusal, read out of that same envelope.
fn error_message(wire: Wire, body: &str) -> String {
    match wire {
        Wire::Query => opt_element(body, "Message").unwrap_or_else(|| "<no Message>".to_string()),
        Wire::Json => {
            let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
            parsed
                .get("message")
                .or_else(|| parsed.get("Message"))
                .and_then(Value::as_str)
                .unwrap_or("<no message>")
                .to_string()
        }
    }
}

/// One IAM awsQuery fixture call, made as the account's own credential.
async fn iam_call(
    router: &MultiServiceRouter,
    action: &str,
    parameters: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Version=2010-05-08&Action={action}");
    for (name, value) in parameters {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("iam.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(AMBIENT_KEY, "iam"))
        .body(Bytes::from(body))
        .expect("the fixture request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// [`iam_call`], asserted served, for a fixture whose answer carries no credential material.
async fn iam_fixture(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    parameters: &[(&str, &str)],
) {
    let (status, body) = iam_call(router, action, parameters).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture {action} must be served, got {status}: {body}"
    );
}

/// Create a user allowed every action, so no row can be refused for lack of a GRANT, and
/// answer nothing: what the rows are about is the credential, not the permissions.
async fn given_user(router: &MultiServiceRouter, case: &str, user_name: &str) {
    iam_fixture(router, case, "CreateUser", &[("UserName", user_name)]).await;
    iam_fixture(
        router,
        case,
        "PutUserPolicy",
        &[
            ("UserName", user_name),
            ("PolicyName", "allow-the-reads"),
            (
                "PolicyDocument",
                r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"*","Resource":"*"}]}"#,
            ),
        ],
    )
    .await;
}

/// Mint one long-term access key for `user_name` and answer its id.
///
/// This answer CARRIES CREDENTIAL MATERIAL — a minted secret access key — so nothing here
/// quotes the body, not even on a failure: a failure names the case and what it looked for.
async fn mint_access_key(router: &MultiServiceRouter, case: &str, user_name: &str) -> String {
    let (status, body) = iam_call(router, "CreateAccessKey", &[("UserName", user_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture CreateAccessKey must be served, got {status}"
    );
    opt_element(&body, "AccessKeyId").unwrap_or_else(|| {
        panic!("case {case:?}: the minted-credential answer carries no <AccessKeyId> element")
    })
}

/// Move `access_key_id` to `status` through IAM's own API.
async fn set_status(
    router: &MultiServiceRouter,
    case: &str,
    user_name: &str,
    access_key_id: &str,
    status: &str,
) {
    iam_fixture(
        router,
        case,
        "UpdateAccessKey",
        &[
            ("UserName", user_name),
            ("AccessKeyId", access_key_id),
            ("Status", status),
        ],
    )
    .await;
}

/// Assert every call is SERVED for `access_key_id`.
///
/// A served answer may carry credential material (a session `GetSessionToken` minted), so a
/// failure names the case, the call and the status — never the body.
async fn assert_every_call_is_served(
    router: &MultiServiceRouter,
    case: &str,
    access_key_id: &str,
) {
    for call in calls() {
        let (status, _body) = dispatch(router, access_key_id, &call).await;
        assert_eq!(
            status,
            200,
            "case {case:?}: {} {} must be served for an accepted credential, got status {status}",
            call.signing_name,
            call.operation
        );
    }
}

/// Assert every call is REFUSED for `access_key_id`, each in its own service's vocabulary.
async fn assert_every_call_is_refused(
    router: &MultiServiceRouter,
    case: &str,
    access_key_id: &str,
) {
    for call in calls() {
        let (status, body) = dispatch(router, access_key_id, &call).await;
        let actual = (
            status,
            error_code(call.wire, &body),
            error_message(call.wire, &body),
        );
        let (expected_status, expected_code, expected_message) = expected_refusal(&call);
        assert_eq!(
            (
                actual.0,
                actual.1.as_str(),
                actual.2.as_str()
            ),
            (expected_status, expected_code, expected_message),
            "case {case:?}: {} {} must refuse a withdrawn credential with its own \
             unknown-credential code, got {actual:?}",
            call.signing_name,
            call.operation
        );
    }
}

/// Assert no call answers `access_key_id` with the withdrawn-credential refusal.
///
/// Weaker than [`assert_every_call_is_served`] on purpose: what a service answers a key it
/// holds no record of depends on what that caller may do and on what the records it names
/// hold, and the property here is only that the key's status refused nothing.
async fn assert_no_call_is_refused_as_an_unknown_credential(
    router: &MultiServiceRouter,
    case: &str,
    access_key_id: &str,
) {
    for call in calls() {
        let (status, body) = dispatch(router, access_key_id, &call).await;
        let code = error_code(call.wire, &body);
        let (refused_status, refused_code, _) = expected_refusal(&call);
        assert!(
            !(status == refused_status && code == refused_code),
            "case {case:?}: {} {} must not answer the withdrawn-credential refusal, got \
             status {status} and code {code:?}",
            call.signing_name,
            call.operation
        );
    }
}

/// A key the account has moved off `Active` is refused by every service, in that service's
/// own unknown-credential vocabulary, and accepted again once it is `Active`.
#[tokio::test]
async fn a_key_the_account_withdrew_is_refused_by_every_service() {
    let router = build_router();

    // `statusType`'s values, as the model publishes them
    // (`smithy_query(query="shape", shape="statusType")`): `Active` is usable and the other
    // two are not, whichever way the account moved the key.
    let rows: [(&str, &str, bool); 4] = [
        ("a key left Active", "Active", true),
        ("a key deactivated", "Inactive", false),
        ("a key moved back to Active", "Active", true),
        ("a key moved to Expired", "Expired", false),
    ];

    let user_name = "rotating-user";
    given_user(&router, "the fixture user", user_name).await;
    let access_key_id = mint_access_key(&router, "the fixture key", user_name).await;

    // The control: a freshly minted key is `Active`, so every call is served before any
    // status is written at all.
    assert_every_call_is_served(&router, "a freshly minted key", &access_key_id).await;

    for (case, status, usable) in rows {
        set_status(&router, case, user_name, &access_key_id, status).await;
        if usable {
            assert_every_call_is_served(&router, case, &access_key_id).await;
        } else {
            assert_every_call_is_refused(&router, case, &access_key_id).await;
        }
    }
}

/// A withdrawn key is withdrawn by itself: the user's other key keeps working, and a key id
/// the account holds no record of is untouched.
#[tokio::test]
async fn withdrawing_one_key_leaves_every_other_credential_alone() {
    let router = build_router();
    let user_name = "two-key-user";
    given_user(&router, "two keys", user_name).await;
    let first = mint_access_key(&router, "the first key", user_name).await;
    let second = mint_access_key(&router, "the second key", user_name).await;

    set_status(&router, "the first key", user_name, &first, "Inactive").await;

    assert_every_call_is_refused(&router, "the deactivated key", &first).await;
    assert_every_call_is_served(&router, "the user's other key", &second).await;
    assert_no_call_is_refused_as_an_unknown_credential(
        &router,
        "a key the account does not hold",
        AMBIENT_KEY,
    )
    .await;
}
