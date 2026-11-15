// SPDX-License-Identifier: CC0-1.0

//! WHO may call this service, driven end to end over the wire (an awsJson1_1 request through
//! the multi-service router into the mounted service), so what is asserted is what a client
//! holding those credentials sees.
//!
//! THE RULE: IAM decides authorization ahead of the service's own code, so every operation
//! evaluates the policies in force on the principal its credentials name against
//! `acm-pca:<OperationName>` on the private CA the request NAMES — under the condition keys
//! the request publishes, which for an issuance is the certificate template it named
//! (`acm-pca:TemplateArn`) — BEFORE it reads or writes anything. A request no statement
//! allows, or one an explicit `Deny` covers, is answered `AccessDeniedException` in IAM's own
//! wording and performs nothing; a request a matching `Allow` covers reaches the operation's
//! own answer.
//!
//! The role, its policies and the session live in the IAM and STS stores of the SAME shared
//! engine, so the documents under test are the ones `iam:PutRolePolicy` stores and the
//! principal a refusal blames is the one `sts:GetCallerIdentity` answers for the key the
//! request signed with.
//!
//! The tables span what the rule generalises over: the condition operator a statement scopes
//! the template with (`StringEquals`, `StringLike`, a `Deny` carrying one) and the template
//! the request names against it; the operation (a read, a list, a tag write, a create, an
//! issuance); the document in force (none, another action, an explicit deny, an allow of
//! exactly the action, of the whole service); and the resource a statement is scoped to (this
//! CA, another CA, every CA).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts;
use smithy_server_runtime::MultiServiceRouter;

use crate::private_ca_lifecycle::{blob, ca_certificate_for, csr_pem, root_ca_request, string, REGION};
use crate::resource_policy::error_message;

/// How long a poll for a due transition may take before the suite gives up. Generous: it
/// bounds a wait for a CONDITION, not a measurement of how long the service takes.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The access key id of the account's own published credential: a key neither STS nor IAM
/// holds a record for, which is the position AWS account root credentials are in. Every
/// set-up call in this suite signs with it.
const ACCOUNT_KEY_ID: &str = "AKIDTESTTESTTEST";

/// The access key id of the assumed-role session the rows below sign with.
const SESSION_KEY_ID: &str = "ASIAEXAMPLESESSION01";

/// The role that session is assumed from, the session's own name, and the name of the inline
/// policy the role carries.
const ROLE: &str = "acm-pca-issuer";
const SESSION: &str = "pca-session";
const POLICY_NAME: &str = "inline";

/// How long the seeded session lasts: an hour, STS's own default.
const SESSION_SECONDS: i32 = 3_600;

/// The templates this suite names. The catalog is the one
/// `IssueCertificateRequest$TemplateArn` publishes ("For a list of TemplateArn values
/// supported by Amazon Web Services Private CA, see Understanding Certificate Templates").
const END_ENTITY_TEMPLATE: &str = "arn:aws:acm-pca:::template/EndEntityCertificate/V1";
const CLIENT_AUTH_TEMPLATE: &str = "arn:aws:acm-pca:::template/EndEntityClientAuthCertificate/V1";
const CODE_SIGNING_TEMPLATE: &str = "arn:aws:acm-pca:::template/CodeSigningCertificate/V1";

/// The condition key an issuance publishes: the ARN of the template the request named.
const TEMPLATE_ARN_KEY: &str = "acm-pca:TemplateArn";

/// A router serving this service beside the IAM and STS services over ONE shared engine, so
/// the policies and the session the cases seed are the ones those services hold.
fn router_with_identity() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_acmpca_acmprivateca::EmulatedService::new(Arc::clone(&engine)),
    );
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
    (router, engine)
}

/// Call one operation of this service signed with `access_key_id`, and return
/// `(status, body)`: the access key id is what names the principal the request speaks for.
async fn call_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let host = format!("acm-pca.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("host", &host)
        .header("x-amz-target", format!("ACMPrivateCA.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential={access_key_id}/20260101/{REGION}/acm-pca/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=deadbeef"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("the request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Call `operation` with the account's own credential, requiring success: how every set-up
/// step in this suite is driven, since that credential holds no IAM policies and this gate
/// does not bound it.
async fn ok_as_account(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, answer) = call_as(router, ACCOUNT_KEY_ID, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} to answer 200 for the account's own credential, \
         got {status} {answer:?}"
    );
    answer
}

/// The principal ARN the seeded session speaks for — what a refusal must name.
fn session_arn() -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{ROLE}/{SESSION}",
        ::engine::emulator_account_id()
    )
}

/// The ARN the account holds the role under, which is the identity whose policies an
/// assumed-role session's request is judged on.
fn role_arn() -> String {
    format!("arn:aws:iam::{}:role/{ROLE}", ::engine::emulator_account_id())
}

/// Record the role and the assumed-role session [`SESSION_KEY_ID`] speaks for, as
/// `iam:CreateRole` and `sts:AssumeRole` record them.
fn given_session(engine: &Arc<engine::InMemoryEngine>) {
    let now = SystemTime::now();
    let account = ::engine::emulator_account_id();
    let role_arn = role_arn();
    let iam_store = iam::service_state(&**engine).expect("the IAM store");
    iam_store.lock().roles.insert(
        iam::name_key(ROLE),
        ::service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::Role {
            path: "/".to_string(),
            role_name: ROLE.to_string(),
            role_id: "AROAEXAMPLEPCAROLE001".to_string(),
            arn: role_arn.clone(),
            create_date: ::aws_smithy_types::DateTime::from_secs(0),
            assume_role_policy_document: None,
            description: None,
            max_session_duration: None,
            permissions_boundary: None,
            tags: None,
            role_last_used: None,
            source_role_template: None,
        },
    );
    let vend = sts::NewSession {
        role_arn: Some(role_arn.clone()),
        ..sts::NewSession::new(
            sts::Identity {
                user_id: sts::role_id(&role_arn),
                account: account.to_string(),
                arn: session_arn(),
            },
            sts::SessionKind::AssumedRole,
            SESSION_SECONDS,
        )
    };
    let store = sts::service_state(&**engine).expect("the STS store");
    store
        .lock()
        .insert(vend.into_session(SESSION_KEY_ID, now))
        .expect("the session is recorded");
}

/// Attach `document` to the role as its inline policy, replacing whatever it carried — what
/// `iam:PutRolePolicy` does. `None` takes every policy off it, as `iam:DeleteRolePolicy`
/// does.
fn given_role_policy(engine: &Arc<engine::InMemoryEngine>, document: Option<&Value>) {
    let store = iam::service_state(&**engine).expect("the IAM store");
    let mut store = store.lock();
    match document {
        None => {
            store.role_inline_policies.remove(&iam::name_key(ROLE));
        }
        Some(document) => {
            store.role_inline_policies.insert(
                iam::name_key(ROLE),
                BTreeMap::from([(
                    iam::name_key(POLICY_NAME),
                    iam::InlinePolicy {
                        policy_name: POLICY_NAME.to_string(),
                        policy_document: document.to_string(),
                    },
                )]),
            );
        }
    }
}

/// A policy allowing `actions` on every resource.
fn allow(actions: Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": "*" }]
    })
}

/// Which of IAM's two refusals a row expects.
#[derive(Debug, Clone, Copy)]
enum Refusal {
    /// The default deny of a request no statement allows.
    DefaultDeny,
    /// The explicit deny of a request a `Deny` statement covers.
    ExplicitDeny,
}

impl Refusal {
    /// The clause IAM closes the refusal with.
    fn reason(self, action: &str) -> String {
        match self {
            Self::DefaultDeny => {
                format!("because no identity-based policy allows the {action} action")
            }
            Self::ExplicitDeny => "with an explicit deny in an identity-based policy".to_string(),
        }
    }
}

/// Assert the answer is IAM's refusal of `action` on `resource` for the seeded session.
fn assert_refused(case: &str, status: u16, answer: &Value, action: &str, resource: &str, refusal: Refusal) {
    assert_eq!(
        status, 403,
        "case {case:?}: expected HTTP 403, got {status} {answer:?}"
    );
    let code = answer
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        code.contains("AccessDeniedException"),
        "case {case:?}: expected an AccessDeniedException, got {code:?} ({answer:?})"
    );
    let message = error_message(answer, case);
    assert_eq!(
        message,
        format!(
            "User: {} is not authorized to perform: {action} on resource: {resource} {}",
            session_arn(),
            refusal.reason(action)
        ),
        "case {case:?}: expected IAM's wording, got {message:?}"
    );
}

/// A private CA driven all the way to ACTIVE with the account's own credential, with the
/// certification request of a subject to present to it.
async fn active_ca(router: &MultiServiceRouter, common_name: &str) -> String {
    let case = common_name;
    let created = ok_as_account(
        router,
        case,
        "CreateCertificateAuthority",
        &root_ca_request(common_name),
    )
    .await;
    let arn = string(&created, "CertificateAuthorityArn", case);
    // Creation settles on the clock, so the suite looks again until the CA is waiting for its
    // certificate rather than waiting a span of its own.
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let described = ok_as_account(
            router,
            case,
            "DescribeCertificateAuthority",
            &json!({"CertificateAuthorityArn": arn}),
        )
        .await;
        let reported = string(&described["CertificateAuthority"], "Status", case);
        if reported == "PENDING_CERTIFICATE" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the CA to wait for its certificate, it still reports \
             {reported:?}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
    let reported = ok_as_account(
        router,
        case,
        "GetCertificateAuthorityCsr",
        &json!({"CertificateAuthorityArn": arn}),
    )
    .await;
    let certificate = ca_certificate_for(&string(&reported, "Csr", case));
    ok_as_account(
        router,
        case,
        "ImportCertificateAuthorityCertificate",
        &json!({"CertificateAuthorityArn": arn, "Certificate": blob(&certificate)}),
    )
    .await;
    arn
}

/// An `IssueCertificate` body for `ca_arn` over `csr` under `template`, inside the window the
/// CA's own certificate holds.
fn issuance(ca_arn: &str, csr: &str, template: Option<&str>) -> Value {
    let mut body = json!({
        "CertificateAuthorityArn": ca_arn,
        "Csr": blob(csr),
        "SigningAlgorithm": "SHA256WITHRSA",
        "Validity": {"Value": 30, "Type": "DAYS"},
    });
    if let Some(template) = template {
        body["TemplateArn"] = json!(template);
    }
    body
}

/// THE RULE for the condition key an issuance publishes: a statement scoped to
/// `acm-pca:TemplateArn` decides the call by the template the request named, so two otherwise
/// identical issuances differing only in that member are decided differently.
#[tokio::test]
async fn an_issuance_is_decided_by_the_template_arn_it_names() {
    let action = "acm-pca:IssueCertificate";
    let (router, engine) = router_with_identity();
    given_session(&engine);
    let ca = active_ca(&router, "conditioned-root").await;
    let csr = csr_pem("a-subject-request");

    // (case, the document in force, the template the issuance names, the refusal it meets)
    let rows: Vec<(&str, Value, Option<&str>, Option<Refusal>)> = vec![
        (
            "the template the allow admits",
            allow_template(action, "StringEquals", json!(END_ENTITY_TEMPLATE)),
            Some(END_ENTITY_TEMPLATE),
            None,
        ),
        (
            "a template the allow does not admit",
            allow_template(action, "StringEquals", json!(END_ENTITY_TEMPLATE)),
            Some(CODE_SIGNING_TEMPLATE),
            Some(Refusal::DefaultDeny),
        ),
        (
            "one of the templates a list of admitted values names",
            allow_template(
                action,
                "StringEquals",
                json!([END_ENTITY_TEMPLATE, CLIENT_AUTH_TEMPLATE]),
            ),
            Some(CLIENT_AUTH_TEMPLATE),
            None,
        ),
        (
            "a template a wildcard pattern admits",
            allow_template(
                action,
                "StringLike",
                json!("arn:aws:acm-pca:::template/EndEntity*"),
            ),
            Some(CLIENT_AUTH_TEMPLATE),
            None,
        ),
        (
            "a template that wildcard pattern does not admit",
            allow_template(
                action,
                "StringLike",
                json!("arn:aws:acm-pca:::template/EndEntity*"),
            ),
            Some(CODE_SIGNING_TEMPLATE),
            Some(Refusal::DefaultDeny),
        ),
        (
            "the template a conditioned deny names, beside an allow of the whole service",
            deny_template(action, json!(CODE_SIGNING_TEMPLATE)),
            Some(CODE_SIGNING_TEMPLATE),
            Some(Refusal::ExplicitDeny),
        ),
        (
            "a template that conditioned deny does not name",
            deny_template(action, json!(CODE_SIGNING_TEMPLATE)),
            Some(END_ENTITY_TEMPLATE),
            None,
        ),
        (
            // A request that names no template publishes no value for the key, so a
            // statement conditioned on it cannot be decided and grants nothing.
            "no template at all against a conditioned allow",
            allow_template(action, "StringEquals", json!(END_ENTITY_TEMPLATE)),
            None,
            Some(Refusal::DefaultDeny),
        ),
        (
            // ... while an unconditioned allow of the action admits it.
            "no template at all against an unconditioned allow",
            allow(json!(action)),
            None,
            None,
        ),
    ];

    for (case, document, template, refused) in &rows {
        given_role_policy(&engine, Some(document));
        let (status, answer) = call_as(
            &router,
            SESSION_KEY_ID,
            "IssueCertificate",
            &issuance(&ca, &csr, *template),
        )
        .await;
        match refused {
            Some(refusal) => {
                assert_refused(case, status, &answer, action, &ca, *refusal);
                assert!(
                    answer.get("CertificateArn").is_none(),
                    "case {case:?}: a refused issuance must mint no certificate, got {answer:?}"
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: an allowed issuance must be served, got {status} {answer:?}"
                );
                let certificate_arn = string(&answer, "CertificateArn", case);
                assert!(
                    certificate_arn.starts_with(&format!("{ca}/certificate/")),
                    "case {case:?}: expected a certificate ARN of {ca:?}, got \
                     {certificate_arn:?}"
                );
            }
        }
    }
}

/// An allow of `action` conditioned on the template, under the IAM condition `operator`.
fn allow_template(action: &str, operator: &str, values: Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": action,
            "Resource": "*",
            "Condition": { operator: { TEMPLATE_ARN_KEY: values } },
        }]
    })
}

/// An allow of the whole service with a DENY of `action` conditioned on the template.
fn deny_template(action: &str, values: Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            { "Effect": "Allow", "Action": "acm-pca:*", "Resource": "*" },
            {
                "Effect": "Deny",
                "Action": action,
                "Resource": "*",
                "Condition": { "StringEquals": { TEMPLATE_ARN_KEY: values } },
            },
        ]
    })
}

/// THE RULE across the operations of this service: each is judged against its OWN action, so
/// a document granting one grants no other, and a refused call performs nothing.
#[tokio::test]
async fn every_operation_is_judged_against_its_own_action() {
    let (router, engine) = router_with_identity();
    given_session(&engine);
    let ca = active_ca(&router, "per-action-root").await;
    let csr = csr_pem("a-per-action-request");

    // (the operation, its request body, the resource the decision is taken against)
    let operations: Vec<(&str, Value, String)> = vec![
        (
            "DescribeCertificateAuthority",
            json!({"CertificateAuthorityArn": ca}),
            ca.clone(),
        ),
        (
            "GetCertificateAuthorityCertificate",
            json!({"CertificateAuthorityArn": ca}),
            ca.clone(),
        ),
        (
            "ListTags",
            json!({"CertificateAuthorityArn": ca}),
            ca.clone(),
        ),
        (
            "TagCertificateAuthority",
            json!({
                "CertificateAuthorityArn": ca,
                "Tags": [{"Key": "owner", "Value": "platform"}],
            }),
            ca.clone(),
        ),
        (
            "IssueCertificate",
            issuance(&ca, &csr, Some(END_ENTITY_TEMPLATE)),
            ca.clone(),
        ),
        // An action that addresses no CA of its own is judged against the resource IAM
        // writes for such a request.
        (
            "ListCertificateAuthorities",
            json!({}),
            "*".to_string(),
        ),
        (
            "CreateCertificateAuthority",
            root_ca_request("another-root"),
            "*".to_string(),
        ),
    ];

    for (operation, body, resource) in &operations {
        let action = format!("acm-pca:{operation}");
        // With no policy at all, IAM's default deny refuses it ...
        given_role_policy(&engine, None);
        let case = format!("{operation} with no policy");
        let (status, answer) = call_as(&router, SESSION_KEY_ID, operation, body).await;
        assert_refused(
            &case,
            status,
            &answer,
            &action,
            resource,
            Refusal::DefaultDeny,
        );

        // ... and so does an allow of every OTHER action of this service: each operation is
        // judged against its own.
        let others: Vec<String> = operations
            .iter()
            .filter(|(other, _, _)| other != operation)
            .map(|(other, _, _)| format!("acm-pca:{other}"))
            .collect();
        given_role_policy(&engine, Some(&allow(json!(others))));
        let case = format!("{operation} with every other action allowed");
        let (status, answer) = call_as(&router, SESSION_KEY_ID, operation, body).await;
        assert_refused(
            &case,
            status,
            &answer,
            &action,
            resource,
            Refusal::DefaultDeny,
        );

        // An allow of exactly this action reaches the operation's own answer.
        given_role_policy(&engine, Some(&allow(json!(action))));
        let case = format!("{operation} allowed");
        let (status, answer) = call_as(&router, SESSION_KEY_ID, operation, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: an allowed request must be served, got {status} {answer:?}"
        );
    }

    // The tag write was refused twice before it was allowed once, and what the CA holds is
    // exactly what the ALLOWED call wrote: a refused call performed nothing.
    let listed = ok_as_account(
        &router,
        "the tags after the refusals",
        "ListTags",
        &json!({"CertificateAuthorityArn": ca}),
    )
    .await;
    assert_eq!(
        listed["Tags"],
        json!([{"Key": "owner", "Value": "platform"}]),
        "case \"the tags after the refusals\": expected only the allowed write's tag, got \
         {listed:?}"
    );
}

/// THE RULE for the resource half of the decision: the call is judged against the CA the
/// request NAMES, so a statement scoped to one CA does not reach another.
#[tokio::test]
async fn a_statement_scoped_to_one_ca_does_not_reach_another() {
    let (router, engine) = router_with_identity();
    given_session(&engine);
    let granted = active_ca(&router, "granted-root").await;
    let withheld = active_ca(&router, "withheld-root").await;
    let action = "acm-pca:DescribeCertificateAuthority";
    given_role_policy(
        &engine,
        Some(&json!({
            "Version": "2012-10-17",
            "Statement": [{ "Effect": "Allow", "Action": action, "Resource": granted }]
        })),
    );

    let case = "the CA the statement names";
    let (status, answer) = call_as(
        &router,
        SESSION_KEY_ID,
        "DescribeCertificateAuthority",
        &json!({"CertificateAuthorityArn": granted}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the described CA to be served, got {status} {answer:?}"
    );
    assert_eq!(
        answer["CertificateAuthority"]["Arn"],
        json!(granted),
        "case {case:?}: expected the CA the request named, got {answer:?}"
    );

    let case = "another CA of the same account";
    let (status, answer) = call_as(
        &router,
        SESSION_KEY_ID,
        "DescribeCertificateAuthority",
        &json!({"CertificateAuthorityArn": withheld}),
    )
    .await;
    assert_refused(
        case,
        status,
        &answer,
        action,
        &withheld,
        Refusal::DefaultDeny,
    );
    assert!(
        answer.get("CertificateAuthority").is_none(),
        "case {case:?}: a refused read must report no CA, got {answer:?}"
    );
}
