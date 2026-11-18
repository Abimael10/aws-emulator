// SPDX-License-Identifier: CC0-1.0

//! Serving the retrieval of a parameter ANOTHER service asks for on a ROLE's behalf, which is
//! what a compute service's launch of the workload the value is for turns on.
//!
//! RULE: such a retrieval is decided by three facts about this service's own resource and the
//! key its value is sealed under, and by nothing about the asking service:
//!
//! * `ssm:GetParameters` for THAT ROLE on the parameter the reference names, over the
//!   documents in force on the role — a grant that sits on some other role decides nothing,
//!   and the refusal names the session the role was assumed in;
//! * whether this endpoint produces a value for the reference at all: a name nothing was
//!   written under, a selector the parameter does not carry, an ARN naming another Region's
//!   endpoint and a `SecureString` whose key cannot run the decrypt each produce none, which
//!   is what a batch read reports among its `InvalidParameters` rather than refusing;
//! * for a `SecureString`, the KMS key the value is sealed under: a CUSTOMER managed key's
//!   policy together with the documents in force on the role decides the decrypt, and the
//!   key's own KEY STATE decides it after that — so a role the key does not permit is refused
//!   the same way whatever state the key is in and learns nothing about it. An Amazon Web
//!   Services managed key needs no grant of the account's own, and a `String` is not encrypted
//!   at all — "this flag is ignored for String and StringList parameter types"
//!   (`smithy_query(query="doc",shape="GetParameterRequest",member="WithDecryption")`).
//!
//! The rows differ along what the rule generalises over: the parameter's type and the key that
//! seals it, which of the two grants the role carries and which role carries them, the form of
//! the reference (an ARN, a name, a selector, another Region's endpoint), whether the account
//! holds the parameter, whether a principal is named at all, and the key's own state.
//!
//! Every parameter, key, role and policy below is created through the OWNING service's own
//! API on one shared engine, so what the gate is decided against is what those services
//! persisted.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ssm_amazonssm::state::parameter_retrieval::{
    self, RetrievalPrincipal, RetrievalRefusal,
};

use crate::parameter_store::{ACCOUNT, REGION, TARGET_PREFIX};

/// The IAM actions a retrieval of a `SecureString` needs: this service's batch read, and the
/// decrypt of the key the value is sealed under.
const GET_PARAMETERS: &str = "ssm:GetParameters";
const DECRYPT: &str = "kms:Decrypt";

/// The session the asking service assumed a role in, as it names it for the workload the value
/// is for: the name is the workload's, so the refusal says which one was refused.
const SESSION: &str = "workload-1";

/// A router carrying this service, KMS — which holds the key a `SecureString` is sealed under
/// — and IAM, which holds the roles the retrieval is decided against, on ONE shared engine,
/// beside that engine.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    (router, engine)
}

/// One awsJson1_1 call that must succeed, answered with its body.
async fn json_ok(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20240101/{REGION}/{signing_name}\
                 /aws4_request, SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a valid test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    assert_eq!(
        status, 200,
        "setup: {signing_name} {operation} must succeed, got {status} with {parsed}"
    );
    parsed
}

/// One Systems Manager call that must succeed.
async fn ssm(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    json_ok(router, "ssm", TARGET_PREFIX, operation, body).await
}

/// One KMS call that must succeed.
async fn kms(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    json_ok(router, "kms", "TrentService", operation, body).await
}

/// One awsQuery call to IAM that must succeed.
async fn iam(router: &MultiServiceRouter, action: &str, parameters: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in parameters {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("https://iam.amazonaws.com/")
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20240101/{REGION}/iam/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: iam {action} must succeed, got {status} with {text}"
    );
    text
}

/// Percent-encode one `application/x-www-form-urlencoded` component, leaving only RFC 3986's
/// unreserved characters literal — so a policy document's braces and quotes survive the round
/// trip.
fn form_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// Create the role `name`, trusted by a compute service's own principal, carrying `statements`
/// as its inline policy, and answer its ARN.
async fn role(router: &MultiServiceRouter, name: &str, statements: Vec<Value>) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": ["ecs-tasks.amazonaws.com"] },
            "Action": ["sts:AssumeRole"],
        }],
    })
    .to_string();
    iam(
        router,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    if !statements.is_empty() {
        let document = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
        iam(
            router,
            "PutRolePolicy",
            &[
                ("RoleName", name),
                ("PolicyName", "retrieve"),
                ("PolicyDocument", &document),
            ],
        )
        .await;
    }
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// An `Allow` of `action` on `resource`.
fn allow(action: &str, resource: &str) -> Value {
    json!({ "Effect": "Allow", "Action": [action], "Resource": [resource] })
}

/// The ARN of the parameter `name` names at [`REGION`]'s endpoint.
fn parameter_arn(name: &str) -> String {
    format!("arn:aws:ssm:{REGION}:{ACCOUNT}:parameter{name}")
}

/// The session ARN the retrieval is made in for the role `name`, which is the principal a
/// refusal names.
fn session_arn(name: &str) -> String {
    format!("arn:aws:sts::{ACCOUNT}:assumed-role/{name}/{SESSION}")
}

/// What one verdict IS, so a row states its expectation without restating a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Served,
    NotAuthorized,
    NotAuthorizedByKey,
    Unresolved,
}

/// The verdict `result` carries.
fn verdict(result: &Result<(), RetrievalRefusal>) -> Verdict {
    match result {
        Ok(()) => Verdict::Served,
        Err(RetrievalRefusal::NotAuthorized { .. }) => Verdict::NotAuthorized,
        Err(RetrievalRefusal::NotAuthorizedByKey { .. }) => Verdict::NotAuthorizedByKey,
        Err(RetrievalRefusal::Unresolved { .. }) => Verdict::Unresolved,
    }
}

/// The sentence a refusal carries, empty for a retrieval that was served.
fn message(result: &Result<(), RetrievalRefusal>) -> String {
    match result {
        Ok(()) => String::new(),
        Err(RetrievalRefusal::NotAuthorized { message, .. })
        | Err(RetrievalRefusal::NotAuthorizedByKey { message }) => message.clone(),
        Err(RetrievalRefusal::Unresolved { name }) => name.clone(),
    }
}

/// The names of the parameters and roles the table below is written against.
const SEALED: &str = "/retrieval/sealed";
const PLAIN: &str = "/retrieval/plain";
const GRANTED_ROLE: &str = "retrievalgranted";
const READ_ONLY_ROLE: &str = "retrievalreadonly";
const UNGRANTED_ROLE: &str = "retrievalungranted";

/// RULE: a retrieval is served only when the ROLE it is made as may read the parameter and —
/// for a `SecureString` under a customer managed key — may decrypt it, and only when this
/// endpoint resolves the reference to a version at all.
#[tokio::test]
async fn a_retrieval_is_decided_over_the_role_the_parameter_and_its_key() {
    let (router, engine) = emulator();
    let created = kms(
        &router,
        "CreateKey",
        json!({ "Description": "the key these parameters are sealed under" }),
    )
    .await;
    let key_arn = created["KeyMetadata"]["Arn"]
        .as_str()
        .expect("CreateKey reports the key's Arn")
        .to_string();
    ssm(
        &router,
        "PutParameter",
        json!({
            "Name": SEALED,
            "Value": "sealed-value",
            "Type": "SecureString",
            "KeyId": key_arn,
        }),
    )
    .await;
    ssm(
        &router,
        "PutParameter",
        json!({ "Name": PLAIN, "Value": "plain-value", "Type": "String" }),
    )
    .await;
    // A parameter sealed under the Amazon Web Services managed key of this service, which a
    // put naming no `KeyId` gets.
    const SERVICE_SEALED: &str = "/retrieval/service-sealed";
    ssm(
        &router,
        "PutParameter",
        json!({ "Name": SERVICE_SEALED, "Value": "service-sealed", "Type": "SecureString" }),
    )
    .await;

    let granted = role(
        &router,
        GRANTED_ROLE,
        vec![
            allow(GET_PARAMETERS, &parameter_arn("/retrieval/*")),
            allow(DECRYPT, &key_arn),
        ],
    )
    .await;
    let read_only = role(
        &router,
        READ_ONLY_ROLE,
        vec![allow(GET_PARAMETERS, &parameter_arn("/retrieval/*"))],
    )
    .await;
    let ungranted = role(&router, UNGRANTED_ROLE, Vec::new()).await;

    // (case, the reference, the role the retrieval is made as, the verdict, what the refusal
    // must carry)
    let rows: Vec<(&str, String, Option<&str>, Verdict, String)> = vec![
        (
            "a sealed parameter by ARN, as a role holding both grants",
            parameter_arn(SEALED),
            Some(&granted),
            Verdict::Served,
            String::new(),
        ),
        (
            "the same parameter by NAME, as that same role",
            SEALED.to_string(),
            Some(&granted),
            Verdict::Served,
            String::new(),
        ),
        (
            "a sealed parameter as a role holding the read but not the decrypt",
            parameter_arn(SEALED),
            Some(&read_only),
            Verdict::NotAuthorizedByKey,
            format!(
                "User: {} is not authorized to perform: {DECRYPT} on resource: {key_arn} \
                 because no identity-based policy allows the {DECRYPT} action",
                session_arn(READ_ONLY_ROLE)
            ),
        ),
        (
            "a sealed parameter as a role holding neither grant",
            parameter_arn(SEALED),
            Some(&ungranted),
            Verdict::NotAuthorized,
            format!(
                "User: {} is not authorized to perform: {GET_PARAMETERS} on resource: {} \
                 because no identity-based policy allows the {GET_PARAMETERS} action",
                session_arn(UNGRANTED_ROLE),
                parameter_arn(SEALED)
            ),
        ),
        (
            "a String parameter needs no grant on any key",
            PLAIN.to_string(),
            Some(&read_only),
            Verdict::Served,
            String::new(),
        ),
        (
            "a String parameter still needs the read",
            PLAIN.to_string(),
            Some(&ungranted),
            Verdict::NotAuthorized,
            format!(
                "User: {} is not authorized to perform: {GET_PARAMETERS} on resource: {} \
                 because no identity-based policy allows the {GET_PARAMETERS} action",
                session_arn(UNGRANTED_ROLE),
                parameter_arn(PLAIN)
            ),
        ),
        (
            "a parameter sealed under the Amazon Web Services managed key needs no key grant",
            SERVICE_SEALED.to_string(),
            Some(&read_only),
            Verdict::Served,
            String::new(),
        ),
        (
            "a retrieval naming no principal at all is decided by no policy",
            parameter_arn(SEALED),
            None,
            Verdict::Served,
            String::new(),
        ),
        (
            "a name the account holds no parameter under",
            "/retrieval/absent".to_string(),
            Some(&granted),
            Verdict::Unresolved,
            "/retrieval/absent".to_string(),
        ),
        (
            "a selector naming no version of the parameter",
            format!("{SEALED}:9"),
            Some(&granted),
            Verdict::Unresolved,
            format!("{SEALED}:9"),
        ),
        (
            "an ARN naming another Region's endpoint",
            format!("arn:aws:ssm:eu-central-1:{ACCOUNT}:parameter{SEALED}"),
            Some(&granted),
            Verdict::Unresolved,
            format!("arn:aws:ssm:eu-central-1:{ACCOUNT}:parameter{SEALED}"),
        ),
    ];
    for (case, reference, role_arn, expected, carries) in rows {
        let session = role_arn.map(|arn| {
            session_arn(arn.rsplit('/').next().expect("a role ARN names its role"))
        });
        let principal = role_arn.map(|role_arn| RetrievalPrincipal {
            role_arn,
            session_arn: session.as_deref(),
        });
        let answered = parameter_retrieval::serve_parameter_retrieval(
            &*engine,
            REGION,
            &reference,
            principal,
        );
        let actual = verdict(&answered);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?} ({:?})",
            message(&answered)
        );
        let carried = message(&answered);
        assert!(
            carried.contains(&carries),
            "case {case:?}: expected the refusal to carry {carries:?}, got {carried:?}"
        );
    }
}

/// RULE: a key whose KEY STATE runs no cryptographic operation opens nothing, so a retrieval
/// of a value sealed under it produces no value at all — the answer a batch read reports among
/// its `InvalidParameters` — whatever grants the role carries and whether or not a principal
/// is named; a role the key does not permit is refused by that policy either way, learning
/// nothing about the key's state; and the value is served again once the key is usable.
#[tokio::test]
async fn a_key_whose_state_runs_no_operation_produces_no_value_for_its_parameters() {
    let (router, engine) = emulator();
    let created = kms(&router, "CreateKey", json!({ "Description": "the kill switch" })).await;
    let key_metadata = &created["KeyMetadata"];
    let key_id = key_metadata["KeyId"]
        .as_str()
        .expect("CreateKey reports the key's KeyId")
        .to_string();
    let key_arn = key_metadata["Arn"]
        .as_str()
        .expect("CreateKey reports the key's Arn")
        .to_string();
    const NAME: &str = "/retrieval/kill-switch";
    ssm(
        &router,
        "PutParameter",
        json!({
            "Name": NAME,
            "Value": "sealed-value",
            "Type": "SecureString",
            "KeyId": key_id,
        }),
    )
    .await;
    let granted = role(
        &router,
        "killswitchgranted",
        vec![
            allow(GET_PARAMETERS, &parameter_arn(NAME)),
            allow(DECRYPT, &key_arn),
        ],
    )
    .await;
    let read_only = role(
        &router,
        "killswitchreadonly",
        vec![allow(GET_PARAMETERS, &parameter_arn(NAME))],
    )
    .await;
    let session = session_arn("killswitchgranted");
    let read_only_session = session_arn("killswitchreadonly");
    let as_role = || RetrievalPrincipal {
        role_arn: &granted,
        session_arn: Some(&session),
    };
    let as_read_only = || RetrievalPrincipal {
        role_arn: &read_only,
        session_arn: Some(&read_only_session),
    };

    // (case, the principal the retrieval is made as, the verdict while the key is disabled)
    let principals: Vec<(&str, Option<RetrievalPrincipal<'_>>, Verdict)> = vec![
        (
            "a role holding both grants",
            Some(as_role()),
            Verdict::Unresolved,
        ),
        ("no principal at all", None, Verdict::Unresolved),
        // The key's policy is read BEFORE its state, so a role the key does not permit is
        // refused the same way whatever state the key is in and learns nothing about it.
        (
            "a role the key does not permit",
            Some(as_read_only()),
            Verdict::NotAuthorizedByKey,
        ),
    ];
    kms(&router, "DisableKey", json!({ "KeyId": key_id })).await;
    let described = kms(&router, "DescribeKey", json!({ "KeyId": key_id })).await;
    assert_eq!(
        described["KeyMetadata"]["KeyState"],
        json!("Disabled"),
        "setup: DisableKey must leave the key disabled, got {described}"
    );
    for (case, principal, expected) in &principals {
        let answered =
            parameter_retrieval::serve_parameter_retrieval(&*engine, REGION, NAME, *principal);
        let actual = verdict(&answered);
        assert_eq!(
            actual, *expected,
            "case {case:?}: expected {expected:?} while the key is disabled, got {actual:?} \
             ({:?})",
            message(&answered)
        );
        let carried = message(&answered);
        // A value the key produces nothing for is reported by the NAME the retrieval asked
        // with, the way a batch read reports one; a role the key does not permit is told only
        // that no policy allows the decrypt.
        let expected_text = match expected {
            Verdict::Unresolved => NAME.to_string(),
            _ => format!("is not authorized to perform: {DECRYPT} on resource: {key_arn}"),
        };
        assert!(
            carried.contains(&expected_text),
            "case {case:?}: expected the refusal to carry {expected_text:?}, got {carried:?}"
        );
    }

    kms(&router, "EnableKey", json!({ "KeyId": key_id })).await;
    for (case, principal, while_disabled) in &principals {
        let answered =
            parameter_retrieval::serve_parameter_retrieval(&*engine, REGION, NAME, *principal);
        let actual = verdict(&answered);
        // A usable key serves every retrieval its policy admits; the one it does not admit is
        // refused by that policy either way.
        let expected = match while_disabled {
            Verdict::NotAuthorizedByKey => Verdict::NotAuthorizedByKey,
            _ => Verdict::Served,
        };
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?} once the key is usable again, got {actual:?} \
             ({:?})",
            message(&answered)
        );
    }
}
