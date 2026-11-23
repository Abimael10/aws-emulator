// SPDX-License-Identifier: CC0-1.0

//! IAM authorization of this service's operations, over the restJson1 wire.
//!
//! The rule under test: every operation of this service is a signed call, and such a call
//! is served only to a caller whose identity-based IAM policies allow THAT operation's own
//! action (`ssm-guiconnect:<OperationName>`) on the resource the request is for — the
//! account's EC2 instance space in the Region the request was signed for, the resource an
//! IAM statement for this API scopes a grant with. Every other caller is refused with the
//! modelled `AccessDeniedException` (HTTP 403) BEFORE the operation reads or writes
//! anything, so a refused caller learns nothing about what the account holds and a refused
//! mutation changes nothing. The refusal states IAM's own sentence, which is what keeps a
//! missing grant distinguishable from the account-level just-in-time node access gate —
//! the other cause of a 403 here, and a completely different remediation.
//!
//! The decision is the real one: the principals below are made with IAM's and STS's OWN
//! operations over the same engine (`CreateUser` + `PutUserPolicy` + `CreateAccessKey`),
//! so the documents this service reads are the ones IAM wrote, and the evaluation is
//! `service_common::iam_authorization`, shared with every other service that authorizes.
//!
//! It is asserted over TABLES that differ along what the rule generalises over: the
//! OPERATION asked for (all three the model binds, each with its own action — a read, a
//! write and a delete), the kind of principal (the account's own published credential and
//! an IAM user), and what its policies say (nothing, an `Allow` for this action on this
//! resource, a wildcard `Allow`, an `Allow` for another action of the namespace, an
//! `Allow` on another Region's resource, and an explicit `Deny` beside a wildcard
//! `Allow`).

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ssmguiconnect_ssmguiconnect::EmulatedService;

/// The Region every call below is signed for, so the resource the decisions are made
/// about is the one this Region's instances carry.
pub(crate) const REGION: &str = "us-east-1";

/// This service's SigV4 signing name, which is also the namespace its actions are
/// published under (`smithy_query(query="service_info")`: `sigv4Name` and `arnNamespace`
/// are both `ssm-guiconnect`).
pub(crate) const SIGNING_NAME: &str = "ssm-guiconnect";

/// The three operations the model binds (`smithy_query(query="digest")`).
pub(crate) const UPDATE: &str = "UpdateConnectionRecordingPreferences";
pub(crate) const GET: &str = "GetConnectionRecordingPreferences";
pub(crate) const DELETE: &str = "DeleteConnectionRecordingPreferences";

/// The access-key id standing for the account's own published credential: neither STS nor
/// IAM holds a record for it, so it resolves to the account's ambient identity, which
/// holds no IAM policies and is allowed — the position AWS account root credentials are
/// in.
pub(crate) const ACCOUNT_KEY: &str = "EMULATORTESTKEY12345";

/// A `KMSKeyArn` inside its modelled `@length(1, 2048)`.
pub(crate) const KEY_ARN: &str =
    "arn:aws:kms:us-east-1:123456789012:key/1a2b3c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d";

/// The header AWS names the request a response answered in. Written out here rather than
/// read from the crate, so the assertion does not merely compare the implementation with
/// itself.
pub(crate) const REQUEST_ID_HEADER: &str = "x-amzn-RequestId";

/// The account-level just-in-time node access gate's own denial sentence — the OTHER
/// cause of a 403 from this service, which an IAM denial must not be worded as. Written
/// out here for the same reason the header above is.
pub(crate) const JIT_DENIAL: &str = "Just-in-time node access is not enabled. You must set \
     up just-in-time node access to record RDP connections.";

/// One response, reduced to what a client branches on.
#[derive(Debug)]
pub(crate) struct Reply {
    pub(crate) status: u16,
    /// The `__type` of a restJson1 error body, shortened to the shape name, or the empty
    /// string on a success.
    pub(crate) code: String,
    /// The `message` of an error body, or the empty string when it carries none.
    pub(crate) message: String,
    /// The request id the response named, read case-insensitively the way HTTP and an SDK
    /// read a header.
    pub(crate) request_id: Option<String>,
    pub(crate) body: Value,
}

/// A principal a call is signed as: the access key id it presents, and the ARN a refusal
/// must blame it by — what `sts:GetCallerIdentity` reports for that key.
pub(crate) struct Principal {
    pub(crate) access_key_id: String,
    pub(crate) arn: String,
}

/// The emulator these suites drive: one shared engine carrying this service beside the IAM
/// and STS services the principals and their policies are made with, and the Systems
/// Manager service the account's just-in-time node access enrollment lives in.
pub(crate) struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    pub(crate) fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite signs nothing: the `Authorization` header carries only the credential
        // scope, which is where a service reads the caller's access key id from.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        register_systems_manager(&mut router, &engine);
        Self { router }
    }

    /// Enroll (or un-enroll) the account in just-in-time node access in [`REGION`], over
    /// the wire — see [`enroll`].
    pub(crate) async fn enroll(&self, enrolled: bool) {
        enroll(&self.router, REGION, enrolled).await;
    }

    /// Dispatch one request as it stands and answer the whole response, so a suite can read
    /// the headers it carries.
    pub(crate) async fn respond(
        &self,
        request: http::Request<bytes::Bytes>,
    ) -> http::Response<bytes::Bytes> {
        self.router.dispatch(request).await
    }

    /// Dispatch one request as it stands, and report the status, the request id it named
    /// and its body as text — the raw path, for an answer no modelled operation produced.
    pub(crate) async fn dispatch(
        &self,
        request: http::Request<bytes::Bytes>,
    ) -> (u16, Option<String>, String) {
        let response = self.respond(request).await;
        let status = response.status().as_u16();
        let request_id = response
            .headers()
            .get(REQUEST_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = String::from_utf8_lossy(&response.into_body()).to_string();
        (status, request_id, body)
    }

    /// One call to `op` of this service, with `access_key_id` in the SigV4 credential
    /// scope, in `region`.
    pub(crate) async fn call_in(
        &self,
        region: &str,
        access_key_id: &str,
        op: &str,
        body: &Value,
    ) -> Reply {
        let (status, request_id, text) = self
            .dispatch(request(region, access_key_id, op, body))
            .await;
        let body: Value = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::Null)
        };
        Reply {
            status,
            code: error_code(&body),
            message: error_message(&body),
            request_id,
            body,
        }
    }

    /// One call to `op` in [`REGION`].
    pub(crate) async fn call(&self, access_key_id: &str, op: &str, body: &Value) -> Reply {
        self.call_in(REGION, access_key_id, op, body).await
    }

    /// One awsQuery call to IAM, signed with the account's own credential.
    async fn iam(&self, action: &str, params: &[(&str, String)]) -> String {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in params {
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
                    "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}/\
                     iam/aws4_request, SignedHeaders=host, Signature=0"
                ),
            )
            .body(bytes::Bytes::from(body))
            .expect("a well-formed awsQuery request");
        let (status, _, text) = self.dispatch(request).await;
        assert_eq!(
            status, 200,
            "case {action:?}: the setup call must succeed, got {status} with {text}"
        );
        text
    }

    /// Create an IAM user carrying `policy` as its inline policy (when one is given), mint
    /// a long-term access key for it, and answer that key id with the user's own ARN.
    pub(crate) async fn user(&self, user_name: &str, policy: Option<&str>) -> Principal {
        let created = self
            .iam("CreateUser", &[("UserName", user_name.to_string())])
            .await;
        let arn = xml_element(&created, "Arn")
            .unwrap_or_else(|| panic!("CreateUser answers the user's Arn, got {created}"));
        if let Some(policy) = policy {
            self.iam(
                "PutUserPolicy",
                &[
                    ("UserName", user_name.to_string()),
                    ("PolicyName", "inline".to_string()),
                    ("PolicyDocument", policy.to_string()),
                ],
            )
            .await;
        }
        let minted = self
            .iam("CreateAccessKey", &[("UserName", user_name.to_string())])
            .await;
        Principal {
            access_key_id: xml_element(&minted, "AccessKeyId")
                .unwrap_or_else(|| panic!("CreateAccessKey answers an AccessKeyId, got {minted}")),
            arn,
        }
    }
}

/// How the account is enrolled in just-in-time node access: the enrollment IS an
/// access-request approval policy, held as a Systems Manager document of an
/// approval-policy type, so a client stands an enrolled account up by creating that
/// document and withdraws the enrollment by deleting it. These helpers do exactly that,
/// over the wire, so nothing a suite sets up is reachable only from in-process Rust.
///
/// The name is the test's own: nothing addresses the policy by name — it is its PRESENCE in
/// the Region that is the enrollment.
pub(crate) const APPROVAL_POLICY_NAME: &str = "ConnectionRecordingApprovalPolicy";

/// The `DocumentType` of an approval policy that decides the requests it covers without an
/// approver acting, one of Systems Manager's modelled document types
/// (`smithy_query(query="shape",shape="DocumentType",
/// model_path="services/service-com-amazonaws-ssm-amazonssm/smithy-model/com-amazonaws-ssm-amazonssm.json")`).
pub(crate) const AUTO_APPROVAL_POLICY: &str = "AutoApprovalPolicy";

/// The `DocumentType` of an approval policy that leaves each request to an approver, from
/// the same modelled value set. Either type is an enrollment.
pub(crate) const MANUAL_APPROVAL_POLICY: &str = "ManualApprovalPolicy";

/// A document content Systems Manager accepts for a policy document: a JSON document
/// declaring the schema version every SSM document carries.
const APPROVAL_POLICY_CONTENT: &str =
    r#"{"schemaVersion":"1.0","description":"who may approve a node access request"}"#;

/// Systems Manager's awsJson1_1 target prefix — the `x-amz-target` prefix its own suites
/// address it by.
const SSM_TARGET_PREFIX: &str = "AmazonSSM";

/// Systems Manager's SigV4 signing name, which is what routes a request to it.
const SSM_SIGNING_NAME: &str = "ssm";

/// Mount Systems Manager on `router` over the same engine, so a suite can enroll the
/// account the way a client does.
pub(crate) fn register_systems_manager(
    router: &mut MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
) {
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        Arc::clone(engine),
    ));
}

/// One awsJson1_1 call to Systems Manager, signed for `region` with the account's own
/// credential, answering `(status, body-as-text)`.
pub(crate) async fn systems_manager(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{SSM_TARGET_PREFIX}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{region}/\
                 {SSM_SIGNING_NAME}/aws4_request, SignedHeaders=host;x-amz-target, Signature=0"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("serialize the request body"),
        ))
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(&response.into_body()).to_string(),
    )
}

/// Create an SSM document of `document_type` named `name` in `region`, through Systems
/// Manager's own `CreateDocument` — which, for an approval-policy type, is what enrolls the
/// account in just-in-time node access there.
pub(crate) async fn create_ssm_document(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    document_type: &str,
) -> (u16, String) {
    systems_manager(
        router,
        region,
        "CreateDocument",
        &json!({
            "Name": name,
            "DocumentType": document_type,
            "Content": APPROVAL_POLICY_CONTENT,
        }),
    )
    .await
}

/// Delete the document named `name` in `region` — what withdraws an enrollment the
/// approval policy of that name stood for.
pub(crate) async fn delete_ssm_document(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
) -> (u16, String) {
    systems_manager(router, region, "DeleteDocument", &json!({ "Name": name })).await
}

/// Bring the account to the enrollment `enrolled` names in `region`, through Systems
/// Manager's own API: enrolled means it holds the approval policy, un-enrolled means it
/// holds none. Idempotent, so a case may ask for the state it is already in.
pub(crate) async fn enroll(router: &MultiServiceRouter, region: &str, enrolled: bool) {
    if enrolled {
        let (status, body) =
            create_ssm_document(router, region, APPROVAL_POLICY_NAME, AUTO_APPROVAL_POLICY).await;
        // An account that already holds the policy is already enrolled, which the create
        // reports as a document of that name existing; either way the Region holds an
        // approval policy once this returns, which is what "enrolled" is.
        assert!(
            status == 200 || body.contains("DocumentAlreadyExists"),
            "case \"enroll {region}\": creating the approval policy must either create it or \
             report that it is already there, got {status} with {body}"
        );
    } else {
        // An account that holds no policy is already un-enrolled, which the delete reports
        // as a document that does not exist; either way the Region holds no approval policy
        // once this returns, which is what "not enrolled" is.
        let (status, body) = delete_ssm_document(router, region, APPROVAL_POLICY_NAME).await;
        assert!(
            status == 200 || body.contains("InvalidDocument"),
            "case \"un-enroll {region}\": deleting the approval policy must either remove it or \
             report that there is none, got {status} with {body}"
        );
    }
}

/// A restJson1 request for `op`, routed by its SigV4 credential scope the way every signed
/// AWS SDK request is.
pub(crate) fn request(
    region: &str,
    access_key_id: &str,
    op: &str,
    body: &Value,
) -> http::Request<bytes::Bytes> {
    http::Request::builder()
        .method("POST")
        .uri(format!("https://{SIGNING_NAME}.{region}.amazonaws.com/{op}"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{region}/\
                 {SIGNING_NAME}/aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("serialize the request body"),
        ))
        .expect("build the request")
}

/// The `__type` of a restJson1 error body, shortened to the shape name.
pub(crate) fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit(['#', '/'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The `message` of an error body, under either casing a protocol spells it with.
pub(crate) fn error_message(body: &Value) -> String {
    for key in ["message", "Message"] {
        if let Some(text) = body.get(key).and_then(Value::as_str) {
            return text.to_string();
        }
    }
    String::new()
}

/// Percent-encode one `application/x-www-form-urlencoded` component, leaving only RFC
/// 3986's unreserved characters literal — so a policy document's braces and quotes survive
/// the round trip.
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

/// The text of the first `<name>…</name>` element of an awsQuery response.
fn xml_element(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

/// A well-formed `ConnectionRecordingPreferences` body.
pub(crate) fn preferences(owner: &str, bucket: &str, key_arn: &str) -> Value {
    json!({
        "RecordingDestinations": { "S3Buckets": [{ "BucketOwner": owner, "BucketName": bucket }] },
        "KMSKeyArn": key_arn,
    })
}

/// The request body of a well-formed Update carrying `token`.
pub(crate) fn update_body(token: &str) -> Value {
    json!({
        "ConnectionRecordingPreferences": preferences("123456789012", "rdp-recordings", KEY_ARN),
        "ClientToken": token,
    })
}

/// A policy document allowing `action` on `resource`.
fn allow(action: &str, resource: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Action\":\"{action}\",\"Resource\":\"{resource}\"}}]}}"
    )
}

/// A policy document allowing every action of this service on everything, and explicitly
/// denying `denied`.
fn allow_all_but(denied: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[\
         {{\"Effect\":\"Allow\",\"Action\":\"{SIGNING_NAME}:*\",\"Resource\":\"*\"}},\
         {{\"Effect\":\"Deny\",\"Action\":\"{denied}\",\"Resource\":\"*\"}}]}}"
    )
}

/// The IAM action an operation of this service is performed under.
pub(crate) fn action_of(operation: &str) -> String {
    format!("{SIGNING_NAME}:{operation}")
}

/// The resource a decision for this service is taken against in `region`: the account's
/// EC2 instance space. Spelled out here rather than read from the crate so the assertion
/// does not compare the implementation with itself.
pub(crate) fn instances_of(region: &str) -> String {
    format!(
        "arn:aws:ec2:{region}:{}:instance/*",
        ::engine::emulator_account_id()
    )
}

/// IAM's own denial sentence for a principal no identity-based policy grants the action
/// to. Its fixed text is written out here; every variable part comes from the request and
/// the signing identity.
pub(crate) fn no_policy_allows(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         because no identity-based policy allows the {action} action"
    )
}

/// The body the three operations are called with below: Update needs its required member,
/// Get takes `smithy.api#Unit` and Delete takes an optional token.
fn body_for(operation: &str, token: &str) -> Value {
    if operation == UPDATE {
        update_body(token)
    } else if operation == DELETE {
        json!({ "ClientToken": token })
    } else {
        json!({})
    }
}

/// RULE: a principal whose identity-based policies allow none of this service's actions is
/// refused the modelled `AccessDeniedException` (403), with IAM's own sentence, on EVERY
/// operation — and the refusal discloses nothing about the account's state.
#[tokio::test]
async fn a_principal_with_no_policy_is_refused_every_operation() {
    for (index, operation) in [GET, UPDATE, DELETE].into_iter().enumerate() {
        let emulator = Emulator::new();
        // The account IS enrolled, so the only thing that can deny the call is the
        // caller's own lack of a grant.
        emulator.enroll(true).await;
        let principal = emulator.user(&format!("noperm-{index}"), None).await;
        let reply = emulator
            .call(
                &principal.access_key_id,
                operation,
                &body_for(operation, "no-policy"),
            )
            .await;
        assert_eq!(
            reply.status, 403,
            "case {operation:?}: expected 403 for a principal with no policy, got {reply:?}"
        );
        assert_eq!(
            reply.code, "AccessDeniedException",
            "case {operation:?}: expected AccessDeniedException, got {reply:?}"
        );
        let expected = no_policy_allows(
            &principal.arn,
            &action_of(operation),
            &instances_of(REGION),
        );
        assert_eq!(
            reply.message, expected,
            "case {operation:?}: expected {expected:?}, got {:?}",
            reply.message
        );
        // The refusal is the ONLY thing the caller learns: nothing of the singleton's
        // state reaches the body.
        assert!(
            !reply.message.contains(JIT_DENIAL),
            "case {operation:?}: the IAM denial must not be worded as the account gate, got {:?}",
            reply.message
        );
    }
}

/// RULE: what a document SAYS decides the call — the same request, from principals whose
/// policies differ, is served or refused accordingly, and a refusal names the rule that
/// withheld the grant.
///
/// Rows differ along the document: nothing at all, this action on this resource, every
/// action of the namespace on everything, another action of the namespace, this action on
/// another Region's resource, and an explicit `Deny` beside a wildcard `Allow`.
#[tokio::test]
async fn a_grant_is_read_for_the_action_and_the_resource_the_request_is_for() {
    let emulator = Emulator::new();
    emulator.enroll(true).await;
    let action = action_of(DELETE);
    let resource = instances_of(REGION);

    // (case, the inline policy, whether the call is served)
    let rows: [(&str, Option<String>, bool); 6] = [
        ("no policy at all", None, false),
        (
            "this action on this resource",
            Some(allow(&action, &resource)),
            true,
        ),
        (
            "every action of the namespace on everything",
            Some(allow(&format!("{SIGNING_NAME}:*"), "*")),
            true,
        ),
        (
            "another action of the namespace",
            Some(allow(&action_of(GET), &resource)),
            false,
        ),
        (
            "this action on another Region's resource",
            Some(allow(&action, &instances_of("eu-west-3"))),
            false,
        ),
        (
            "an explicit deny beside a wildcard allow",
            Some(allow_all_but(&action)),
            false,
        ),
    ];

    for (index, (case, policy, served)) in rows.into_iter().enumerate() {
        let principal = emulator
            .user(&format!("grant-{index}"), policy.as_deref())
            .await;
        let reply = emulator
            .call(
                &principal.access_key_id,
                DELETE,
                &json!({ "ClientToken": format!("grant-{index}") }),
            )
            .await;
        if served {
            assert_eq!(
                reply.status, 200,
                "case {case:?}: expected the call to be served, got {reply:?}"
            );
            continue;
        }
        assert_eq!(
            reply.status, 403,
            "case {case:?}: expected 403, got {reply:?}"
        );
        assert_eq!(
            reply.code, "AccessDeniedException",
            "case {case:?}: expected AccessDeniedException, got {reply:?}"
        );
        let opening = format!(
            "User: {} is not authorized to perform: {action} on resource: {resource} ",
            principal.arn
        );
        assert!(
            reply.message.starts_with(&opening),
            "case {case:?}: expected the refusal to open {opening:?}, got {:?}",
            reply.message
        );
        let reason = reply.message[opening.len()..].to_string();
        let expected_reason = if case == "an explicit deny beside a wildcard allow" {
            "with an explicit deny in an identity-based policy".to_string()
        } else {
            format!("because no identity-based policy allows the {action} action")
        };
        assert_eq!(
            reason, expected_reason,
            "case {case:?}: expected the reason {expected_reason:?}, got {reason:?}"
        );
    }
}

/// RULE: granting the policy restores the call — the SAME request a principal with no
/// policy is refused is answered, for a principal that holds the grant, exactly as the
/// account's own credential is answered. Which is also the proof that each operation's own
/// outcome is reached only once the caller is authorized.
#[tokio::test]
async fn the_grant_restores_each_operations_own_outcome() {
    let emulator = Emulator::new();
    // NOT enrolled, so Update's own answer is the account gate's 403 — which is what the
    // permissioned principal and the account credential must BOTH get, and which must not
    // be what the unpermissioned principal gets.
    let granted = emulator
        .user("granted", Some(&allow(&format!("{SIGNING_NAME}:*"), "*")))
        .await;
    let refused = emulator.user("refused", None).await;

    // (case, operation, the status an authorized caller gets, the code it gets)
    let rows: [(&str, &str, u16, &str); 3] = [
        ("the read, with nothing configured", GET, 404, "ResourceNotFoundException"),
        ("the write, on an unenrolled account", UPDATE, 403, "AccessDeniedException"),
        ("the delete, with nothing configured", DELETE, 200, ""),
    ];

    for (case, operation, status, code) in rows {
        for (who, key) in [
            ("the account's own credential", ACCOUNT_KEY),
            ("a principal holding the grant", granted.access_key_id.as_str()),
        ] {
            let reply = emulator
                .call(key, operation, &body_for(operation, "authorized"))
                .await;
            assert_eq!(
                reply.status, status,
                "case {case:?} as {who:?}: expected {status}, got {reply:?}"
            );
            assert_eq!(
                reply.code, code,
                "case {case:?} as {who:?}: expected code {code:?}, got {reply:?}"
            );
        }
        // The unpermissioned principal gets the IAM refusal instead, whatever this
        // operation's own answer would have been.
        let denied = emulator
            .call(
                &refused.access_key_id,
                operation,
                &body_for(operation, "unauthorized"),
            )
            .await;
        assert_eq!(
            denied.status, 403,
            "case {case:?} as an unpermissioned principal: expected 403, got {denied:?}"
        );
        assert_eq!(
            denied.message,
            no_policy_allows(&refused.arn, &action_of(operation), &instances_of(REGION)),
            "case {case:?} as an unpermissioned principal: expected the IAM denial, got {denied:?}"
        );
    }
}

/// RULE: the two causes of a 403 here are told apart by their message — a missing IAM
/// grant states IAM's sentence, the account's enrollment gate states the feature's. The
/// same Update, from two principals on the same unenrolled account, gets one 403 each and
/// they do not share a message.
#[tokio::test]
async fn the_iam_denial_and_the_account_gate_do_not_share_a_message() {
    let emulator = Emulator::new();
    let granted = emulator
        .user("jit-granted", Some(&allow(&format!("{SIGNING_NAME}:*"), "*")))
        .await;
    let refused = emulator.user("jit-refused", None).await;

    let by_gate = emulator
        .call(&granted.access_key_id, UPDATE, &update_body("gate"))
        .await;
    let by_iam = emulator
        .call(&refused.access_key_id, UPDATE, &update_body("iam"))
        .await;

    for (case, reply) in [("the account gate", &by_gate), ("the IAM gate", &by_iam)] {
        assert_eq!(
            reply.status, 403,
            "case {case:?}: expected 403, got {reply:?}"
        );
        assert_eq!(
            reply.code, "AccessDeniedException",
            "case {case:?}: expected AccessDeniedException, got {reply:?}"
        );
    }
    assert_eq!(
        by_gate.message,
        JIT_DENIAL,
        "case \"the account gate\": expected the feature gate's own sentence, got {by_gate:?}"
    );
    assert_eq!(
        by_iam.message,
        no_policy_allows(&refused.arn, &action_of(UPDATE), &instances_of(REGION)),
        "case \"the IAM gate\": expected IAM's own sentence, got {by_iam:?}"
    );
    assert_ne!(
        by_gate.message, by_iam.message,
        "the two denial causes must not share a message, got {:?}",
        by_gate.message
    );
}

/// RULE: the decision comes BEFORE the operation touches anything, so an unauthorized
/// caller cannot tell a configured account from an unconfigured one, and an unauthorized
/// write or delete changes nothing a later authorized read sees.
#[tokio::test]
async fn a_refused_call_discloses_nothing_and_changes_nothing() {
    let emulator = Emulator::new();
    emulator.enroll(true).await;
    let refused = emulator.user("silent", None).await;

    // Configured by the account's own credential, so there IS state to disclose.
    let configured = emulator
        .call(ACCOUNT_KEY, UPDATE, &update_body("configured"))
        .await;
    assert_eq!(
        configured.status, 200,
        "case \"the setup write\": expected it to succeed, got {configured:?}"
    );

    // (case, operation, the body it is called with)
    let rows: [(&str, &str, Value); 3] = [
        ("the read", GET, json!({})),
        ("the write", UPDATE, update_body("intruder")),
        ("the delete", DELETE, json!({ "ClientToken": "intruder" })),
    ];
    for (case, operation, body) in rows {
        let reply = emulator
            .call(&refused.access_key_id, operation, &body)
            .await;
        assert_eq!(
            reply.status, 403,
            "case {case:?}: expected 403, got {reply:?}"
        );
        // Nothing of the stored record reaches the refusal.
        for disclosed in ["rdp-recordings", "123456789012", KEY_ARN, "configured"] {
            assert!(
                !reply.message.contains(disclosed),
                "case {case:?}: the refusal must disclose nothing of the record, got {:?}",
                reply.message
            );
        }
        assert!(
            reply.body.get("ConnectionRecordingPreferences").is_none(),
            "case {case:?}: the refusal must carry no preferences, got {:?}",
            reply.body
        );
    }

    // The record the refused calls could not touch is still exactly what was written.
    let read_back = emulator.call(ACCOUNT_KEY, GET, &json!({})).await;
    assert_eq!(
        read_back.status, 200,
        "case \"the read back\": expected the stored record, got {read_back:?}"
    );
    assert_eq!(
        read_back.body["ConnectionRecordingPreferences"]["RecordingDestinations"]["S3Buckets"][0]
            ["BucketName"]
            .as_str(),
        Some("rdp-recordings"),
        "case \"the read back\": expected the stored bucket, got {read_back:?}"
    );
    assert_eq!(
        read_back.body["ClientToken"].as_str(),
        Some("configured"),
        "case \"the read back\": expected the token the surviving write carried, got {read_back:?}"
    );
}

/// RULE: the resource a decision is taken against carries the REGION the request was
/// signed for, so a grant scoped to one Region's instances serves that Region's calls and
/// no other's.
#[tokio::test]
async fn a_region_scoped_grant_serves_only_that_regions_calls() {
    let emulator = Emulator::new();
    let scoped = emulator
        .user(
            "region-scoped",
            Some(&allow(&action_of(DELETE), &instances_of(REGION))),
        )
        .await;

    // (case, the region the call is signed for, whether the grant covers it)
    let rows: [(&str, &str, bool); 3] = [
        ("the Region the grant names", REGION, true),
        ("another Region", "eu-west-3", false),
        ("a third Region", "ap-south-1", false),
    ];
    for (case, region, covered) in rows {
        let reply = emulator
            .call_in(
                region,
                &scoped.access_key_id,
                DELETE,
                &json!({ "ClientToken": format!("region-{region}") }),
            )
            .await;
        let expected = if covered { 200 } else { 403 };
        assert_eq!(
            reply.status, expected,
            "case {case:?}: expected {expected}, got {reply:?}"
        );
        if !covered {
            assert_eq!(
                reply.message,
                no_policy_allows(&scoped.arn, &action_of(DELETE), &instances_of(region)),
                "case {case:?}: expected the refusal to name that Region's resource, got {reply:?}"
            );
        }
    }
}
