// SPDX-License-Identifier: CC0-1.0

//! Who may make an AWS Signer call, driven end to end over the services that decide it: IAM
//! holds the role and the inline document in force on it, STS mints the session the call is
//! signed with, and Signer resolves that credential and decides the call before it reads or
//! writes anything.
//!
//! RULE under test: a request to AWS Signer is decided against the identity-based policy
//! documents in force on the principal its credential speaks for, on the action
//! `signer:<OperationName>` and the resource the request ADDRESSES — the signing-profile ARN
//! for a call that names a profile (including the profile a `profileVersionArn` belongs to,
//! since a profile version is not a resource IAM addresses on its own), the signing-job ARN
//! for one that names a job, and no resource for one that addresses none. A call an explicit
//! `Deny` covers, and one no readable document allows, are both answered HTTP 403
//! `AccessDeniedException` naming the principal, the action, the resource and which of IAM's
//! rules refused; nothing of the request is performed. The same documents decide the same
//! operation at Signer's DATA-plane endpoint, which publishes the same action namespace.
//!
//! Only a cross-service suite can pin this as a client meets it: the session has to be minted
//! by a real `sts:AssumeRole` of a role a real `iam:CreateRole` holds, under a document a real
//! `iam:PutRolePolicy` stored, and then read back through the answer Signer gives a request
//! signed with it.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The form-encoder every awsQuery suite of this crate shares, and the reader that pulls one
// element's text out of an XML answer: each written ONCE, in the sibling that owns it.
use crate::query_wire::{element, form_encode};

/// The region every request here is signed for and every ARN below carries.
const REGION: &str = "us-west-2";

/// The long-term key every emulator publishes, which is the account's own credential: the
/// fixture is built with it and the role is assumed with it. No IAM document bounds it.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// AWS Signer's SigV4 signing name, its endpoint prefix and its IAM action namespace, which
/// are one string (`smithy_query(query="service_info",
/// model_path="services/service-com-amazonaws-signer-wallabyservice/smithy-model/com-amazonaws-signer-wallabyservice.json")`
/// reads `sigv4Name`, `endpointPrefix` and `arnNamespace` all as `signer`).
const SIGNER: &str = "signer";

/// The endpoint prefix of Signer's DATA plane, whose model publishes AWS Signer's own
/// `signer` action namespace behind its own endpoint
/// (`smithy_query(query="service_info",
/// model_path="services/service-com-amazonaws-signerdata-signerdataplane/smithy-model/com-amazonaws-signerdata-signerdataplane.json")`).
const DATA_SIGNER: &str = "data-signer";

/// `iam`'s and `sts`' own awsQuery API versions, the `Version` each call carries.
const IAM_API_VERSION: &str = "2010-05-08";
const STS_API_VERSION: &str = "2011-06-15";

/// The role the session is a session of, the name of its inline document, and the name the
/// session is minted under — the three values a refusal's principal ARN is built from.
const ROLE: &str = "signer-caller";
const ROLE_POLICY_NAME: &str = "inline";
const SESSION_NAME: &str = "release-1";

/// The signing profile every case addresses, inside `ProfileName`'s own pattern
/// (`^[a-zA-Z0-9_]{2,}$`).
const PROFILE: &str = "release_profile";

/// A second profile of the same account, for the row whose grant is scoped to another
/// resource.
const OTHER_PROFILE: &str = "other_profile";

/// A platform the service publishes, and the ACM certificate ARN a profile on it signs with.
const PLATFORM: &str = "Notation-OCI-SHA384-ECDSA";
const CERTIFICATE_ARN: &str = "arn:aws:acm:us-west-2:000000000000:certificate/abc";

/// The signing job every job-scoped case addresses. No job is started under it: the decision
/// is taken on the ARN the REQUEST carries, before anything it names is resolved, which is
/// what keeps a refused call from telling an unauthorized caller whether the job exists.
const JOB_ID: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

/// The reason clause for a request an explicit `Deny` in one of the principal's own
/// identity-based documents refuses. Written out HERE rather than read from a crate: the
/// wording is the requirement, so a drift in what the services answer fails this suite
/// instead of travelling with it.
const EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// The modelled error shape and `code` an unauthorized call is refused with
/// (`AccessDeniedException`, `httpError` 403).
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The four services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // These cases exercise operation behaviour, not request signing.
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
    router.register_service(
        service_com_amazonaws_signer_wallabyservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_signerdata_signerdataplane::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and whose
/// credential is `access_key_id` — the key a service reads the caller's identity from. The
/// signature is never checked: request authentication is off on this router.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One awsQuery form call to `signing_name`, signed with the account's own credential,
/// reported as `(status, the XML text the wire carried)`.
async fn query(router: &MultiServiceRouter, signing_name: &str, body: String) -> (u16, String) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(AMBIENT_KEY, signing_name))
        .body(bytes::Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One restJson1 call to the endpoint `signing_name` names, signed with `access_key_id`,
/// reported as `(status, body)`.
async fn rest(
    router: &MultiServiceRouter,
    access_key_id: &str,
    signing_name: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let payload = match body {
        Some(value) => {
            bytes::Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
        }
        None => bytes::Bytes::new(),
    };
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", &host)
        .header("authorization", authorization(access_key_id, signing_name));
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(payload)
        .expect("a well-formed restJson1 request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = response.into_body();
    let parsed = if answered.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&answered).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One call to AWS Signer's control plane.
async fn signer(
    router: &MultiServiceRouter,
    access_key_id: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    rest(router, access_key_id, SIGNER, method, path, body).await
}

/// The wire error code of an error response, as the short shape name a client reads.
fn error_code(body: &Value) -> String {
    let raw = body
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    raw.split('#').next_back().unwrap_or(raw).to_string()
}

/// The error body's `message`, under either casing the envelope may carry it in.
fn error_message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// IAM's refusal sentence: who was refused, which action, on which resource, and which of
/// IAM's rules refused it. Written out HERE rather than read from a crate, for the reason
/// [`EXPLICIT_DENY`] gives.
fn not_authorized(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    let on_resource = if resource.is_empty() {
        String::new()
    } else {
        format!(" on resource: {resource}")
    };
    format!("User: {principal} is not authorized to perform: {action}{on_resource} {reason}")
}

/// The reason clause for a request no readable identity-based document allows — IAM's default
/// deny, which names the action nothing granted.
fn no_policy_allows(action: &str) -> String {
    format!("because no identity-based policy allows the {action} action")
}

/// The ARN the account holds the role under.
fn role_arn() -> String {
    format!("arn:aws:iam::{}:role/{ROLE}", ::engine::emulator_account_id())
}

/// The ARN of a signing profile of this account in this region, as Signer mints it: the
/// resource portion carries a leading `/`.
fn profile_arn(name: &str) -> String {
    format!(
        "arn:aws:signer:{REGION}:{}:/signing-profiles/{name}",
        ::engine::emulator_account_id()
    )
}

/// The ARN of a signing job of this account in this region, in the same resource form.
fn job_arn(job_id: &str) -> String {
    format!(
        "arn:aws:signer:{REGION}:{}:/signing-jobs/{job_id}",
        ::engine::emulator_account_id()
    )
}

/// A policy allowing `actions` on `resource`.
fn allow(actions: Value, resource: Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": resource }],
    })
}

/// A policy allowing every action of this service everywhere, with `action` denied on
/// `resource` — the shape of the control a client fences one call off with.
fn allow_all_but(action: &str, resource: Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            { "Effect": "Allow", "Action": format!("{SIGNER}:*"), "Resource": "*" },
            { "Effect": "Deny", "Action": action, "Resource": resource },
        ],
    })
}

/// A role trust policy admitting this account's own principals to `sts:AssumeRole`.
fn trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": format!("arn:aws:iam::{}:root", ::engine::emulator_account_id()) },
            "Action": "sts:AssumeRole",
        }],
    })
}

/// The credential one session was minted under: the access key id a later request signs with,
/// and the ARN `sts:AssumeRole` reports for it — the principal a refusal blames.
struct Session {
    access_key_id: String,
    arn: String,
}

/// Hold the role, through IAM's own `CreateRole`, and mint a session of it through a real
/// `sts:AssumeRole`.
async fn given_session(router: &MultiServiceRouter) -> Session {
    let (status, created) = query(
        router,
        "iam",
        format!(
            "Action=CreateRole&Version={IAM_API_VERSION}&RoleName={ROLE}\
             &AssumeRolePolicyDocument={}",
            form_encode(&trust_policy().to_string())
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup CreateRole({ROLE:?}): expected 200, got {status} {created}"
    );
    assert_eq!(
        element(&created, "Arn"),
        role_arn(),
        "setup CreateRole({ROLE:?}): the role must be held under its own ARN, got {created}"
    );

    let (status, assumed) = query(
        router,
        "sts",
        format!(
            "Action=AssumeRole&Version={STS_API_VERSION}&RoleArn={}\
             &RoleSessionName={SESSION_NAME}",
            form_encode(&role_arn())
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup AssumeRole({ROLE:?}): expected 200, got {status} {assumed}"
    );
    Session {
        access_key_id: element(&assumed, "AccessKeyId"),
        arn: element(&assumed, "Arn"),
    }
}

/// Put `document` in force on the role as its inline policy, replacing whatever it carried,
/// through IAM's own `PutRolePolicy`.
async fn given_role_policy(router: &MultiServiceRouter, document: &Value) {
    let (status, body) = query(
        router,
        "iam",
        format!(
            "Action=PutRolePolicy&Version={IAM_API_VERSION}&RoleName={ROLE}\
             &PolicyName={ROLE_POLICY_NAME}&PolicyDocument={}",
            form_encode(&document.to_string())
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup PutRolePolicy({ROLE:?}): expected 200, got {status} {body}"
    );
}

/// A signing profile of this account, created through `PutSigningProfile` with the account's
/// own credential; the `profileVersionArn` it reports is what a revocation check names.
async fn given_profile(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = signer(
        router,
        AMBIENT_KEY,
        "PUT",
        &format!("/signing-profiles/{name}"),
        Some(&json!({
            "platformId": PLATFORM,
            "signingMaterial": { "certificateArn": CERTIFICATE_ARN },
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup PutSigningProfile({name:?}): expected 200, got {status} {body}"
    );
    body["profileVersionArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("setup PutSigningProfile({name:?}) carried no profileVersionArn: {body}")
        })
        .to_string()
}

/// The query string `GetRevocationStatus` binds its five `@required` members to. A query
/// carries `:` and `/` unescaped, which is how an SDK sends an ARN and an ISO 8601 instant.
fn revocation_query(profile_version_arn: &str) -> String {
    format!(
        "/revocations?signatureTimestamp=2026-01-01T00:00:00Z&platformId={PLATFORM}\
         &profileVersionArn={profile_version_arn}&jobArn={}&certificateHashes={}",
        job_arn(JOB_ID),
        "ab".repeat(96)
    )
}

/// One `GetRevocationStatus` call at the endpoint `signing_name` names, signed with
/// `access_key_id`.
async fn get_revocation_status(
    router: &MultiServiceRouter,
    access_key_id: &str,
    signing_name: &str,
    profile_version_arn: &str,
) -> (u16, Value) {
    rest(
        router,
        access_key_id,
        signing_name,
        "GET",
        &revocation_query(profile_version_arn),
        None,
    )
    .await
}

#[tokio::test]
async fn a_revocation_check_is_decided_against_the_documents_in_force_on_the_session() {
    let router = build_router();
    let profile_version_arn = given_profile(&router, PROFILE).await;
    let other_version_arn = given_profile(&router, OTHER_PROFILE).await;
    let session = given_session(&router).await;
    let action = format!("{SIGNER}:GetRevocationStatus");
    // The resource the call is judged against: a profile VERSION is not a resource IAM
    // addresses, so it is the profile the version belongs to — the version ARN with its
    // trailing segment dropped.
    let resource = profile_arn(PROFILE);
    assert_eq!(
        profile_version_arn
            .rsplit_once('/')
            .map(|(profile, _version)| profile.to_string()),
        Some(resource.clone()),
        "setup: the version ARN must be the profile's ARN with a version appended, got \
         {profile_version_arn:?}"
    );

    // The positive control: the account's own credential is bounded by no IAM document, so the
    // call is served and reports the list it computes.
    let (status, body) = get_revocation_status(&router, AMBIENT_KEY, SIGNER, &profile_version_arn)
        .await;
    assert_eq!(
        status, 200,
        "case \"the account's own credential\": expected 200, got {status} {body}"
    );
    assert!(
        body.get("revokedEntities").and_then(Value::as_array).is_some(),
        "case \"the account's own credential\": expected a revokedEntities list, got {body}"
    );

    // The rows differ in what the document in force says about this action on this resource: a
    // grant covering every action, a grant naming exactly this action and this profile, a
    // grant of another action, a grant scoped to another profile, a broad grant an explicit
    // `Deny` on every resource overrides, and one an explicit `Deny` on exactly this profile
    // overrides. `None` is the row that must still be served — the regression guard that the
    // gate keys off the caller's policy and does not refuse the operation outright.
    let rows: Vec<(&str, Value, Option<String>)> = vec![
        (
            "every action of this service on every resource",
            allow(json!(format!("{SIGNER}:*")), json!("*")),
            None,
        ),
        (
            "this action on exactly this profile",
            allow(json!(action.clone()), json!(resource.clone())),
            None,
        ),
        (
            "another action of this service only",
            allow(json!(format!("{SIGNER}:GetSigningProfile")), json!("*")),
            Some(no_policy_allows(&action)),
        ),
        (
            "every action of this service on another profile",
            allow(
                json!(format!("{SIGNER}:*")),
                json!(profile_arn(OTHER_PROFILE)),
            ),
            Some(no_policy_allows(&action)),
        ),
        (
            "a broad grant an explicit deny on every resource overrides",
            allow_all_but(&action, json!("*")),
            Some(EXPLICIT_DENY.to_string()),
        ),
        (
            "a broad grant an explicit deny on this profile overrides",
            allow_all_but(&action, json!(resource.clone())),
            Some(EXPLICIT_DENY.to_string()),
        ),
        (
            "a grant of every action, once more",
            allow(json!(format!("{SIGNER}:*")), json!("*")),
            None,
        ),
    ];

    for (case, document, refusal) in rows {
        given_role_policy(&router, &document).await;
        let (status, body) =
            get_revocation_status(&router, &session.access_key_id, SIGNER, &profile_version_arn)
                .await;
        match refusal {
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: a call a matching Allow covers expected 200, got {status} \
                     {body}"
                );
                assert!(
                    body.get("revokedEntities")
                        .and_then(Value::as_array)
                        .is_some(),
                    "case {case:?}: a served call must report a revokedEntities list, got {body}"
                );
            }
            Some(reason) => {
                assert_eq!(
                    (status, error_code(&body).as_str()),
                    (403, ACCESS_DENIED),
                    "case {case:?}: a call no document allows must be refused with the modelled \
                     403, got {status} {body}"
                );
                assert_eq!(
                    error_message(&body),
                    not_authorized(&session.arn, &action, &resource, &reason),
                    "case {case:?}: the refusal must name the principal STS minted, the action \
                     asked for, the profile addressed and the rule that refused, got {body}"
                );
            }
        }
    }

    // A grant scoped to the OTHER profile still serves the other profile's own check, so the
    // decision is about the resource the request addressed and not about the operation.
    given_role_policy(
        &router,
        &allow(
            json!(format!("{SIGNER}:*")),
            json!(profile_arn(OTHER_PROFILE)),
        ),
    )
    .await;
    let (status, body) =
        get_revocation_status(&router, &session.access_key_id, SIGNER, &other_version_arn).await;
    assert_eq!(
        status, 200,
        "case \"the profile the grant is scoped to\": expected 200, got {status} {body}"
    );

    // The resource judged is the one the REQUEST addressed, with every field of the ARN it
    // carried — the account included. A version ARN of another account names that account's
    // profile, so a `Deny` scoped to it refuses the call naming it, while the same broad grant
    // still serves a check of this account's own profile.
    let foreign_profile = "arn:aws:signer:us-west-2:111122223333:/signing-profiles/n";
    let foreign_version = format!("{foreign_profile}/aaaaaaaaaa");
    given_role_policy(&router, &allow_all_but(&action, json!(foreign_profile))).await;
    let (status, body) =
        get_revocation_status(&router, &session.access_key_id, SIGNER, &foreign_version).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (403, ACCESS_DENIED),
        "case \"another account's profile version\": expected the modelled 403, got {status} \
         {body}"
    );
    assert_eq!(
        error_message(&body),
        not_authorized(&session.arn, &action, foreign_profile, EXPLICIT_DENY),
        "case \"another account's profile version\": the refusal must name the profile ARN the \
         request addressed, account and all, got {body}"
    );
    let (status, body) =
        get_revocation_status(&router, &session.access_key_id, SIGNER, &profile_version_arn).await;
    assert_eq!(
        status, 200,
        "case \"this account's profile under a deny scoped elsewhere\": expected 200, got \
         {status} {body}"
    );
}

/// One operation's call: how it is addressed, and the resource it is judged against.
struct Call {
    operation: &'static str,
    method: &'static str,
    path: String,
    body: Option<Value>,
    /// The ARN the refusal must name, or the empty string for an action that addresses no
    /// resource of this service.
    resource: String,
}

/// Every operation of AWS Signer whose model declares `AccessDeniedException`
/// (`smithy_query(query="digest",
/// model_path="services/service-com-amazonaws-signer-wallabyservice/smithy-model/com-amazonaws-signer-wallabyservice.json")`),
/// with the resource each one addresses. The three tagging operations model no such error and
/// are not here.
fn every_decided_call(profile_version_arn: &str) -> Vec<Call> {
    let profile = profile_arn(PROFILE);
    let job = job_arn(JOB_ID);
    vec![
        Call {
            operation: "AddProfilePermission",
            method: "POST",
            path: format!("/signing-profiles/{PROFILE}/permissions"),
            body: Some(json!({
                "action": "signer:StartSigningJob",
                "principal": format!("arn:aws:iam::{}:root", ::engine::emulator_account_id()),
                "statementId": "grant-1",
            })),
            resource: profile.clone(),
        },
        Call {
            operation: "CancelSigningProfile",
            method: "DELETE",
            path: format!("/signing-profiles/{PROFILE}"),
            body: None,
            resource: profile.clone(),
        },
        Call {
            operation: "DescribeSigningJob",
            method: "GET",
            path: format!("/signing-jobs/{JOB_ID}"),
            body: None,
            resource: job.clone(),
        },
        Call {
            operation: "GetRevocationStatus",
            method: "GET",
            path: revocation_query(profile_version_arn),
            body: None,
            resource: profile.clone(),
        },
        Call {
            operation: "GetSigningPlatform",
            method: "GET",
            path: format!("/signing-platforms/{PLATFORM}"),
            body: None,
            resource: String::new(),
        },
        Call {
            operation: "GetSigningProfile",
            method: "GET",
            path: format!("/signing-profiles/{PROFILE}"),
            body: None,
            resource: profile.clone(),
        },
        Call {
            operation: "ListProfilePermissions",
            method: "GET",
            path: format!("/signing-profiles/{PROFILE}/permissions"),
            body: None,
            resource: profile.clone(),
        },
        Call {
            operation: "ListSigningJobs",
            method: "GET",
            path: "/signing-jobs".to_string(),
            body: None,
            resource: String::new(),
        },
        Call {
            operation: "ListSigningPlatforms",
            method: "GET",
            path: "/signing-platforms".to_string(),
            body: None,
            resource: String::new(),
        },
        Call {
            operation: "ListSigningProfiles",
            method: "GET",
            path: "/signing-profiles".to_string(),
            body: None,
            resource: String::new(),
        },
        Call {
            operation: "PutSigningProfile",
            method: "PUT",
            path: format!("/signing-profiles/{PROFILE}"),
            body: Some(json!({
                "platformId": PLATFORM,
                "signingMaterial": { "certificateArn": CERTIFICATE_ARN },
            })),
            resource: profile.clone(),
        },
        Call {
            operation: "RemoveProfilePermission",
            method: "DELETE",
            path: format!("/signing-profiles/{PROFILE}/permissions/grant-1?revisionId=0"),
            body: None,
            resource: profile.clone(),
        },
        Call {
            operation: "RevokeSignature",
            method: "PUT",
            path: format!("/signing-jobs/{JOB_ID}/revoke"),
            body: Some(json!({ "reason": "the signing key was compromised" })),
            resource: job,
        },
        Call {
            operation: "RevokeSigningProfile",
            method: "PUT",
            path: format!("/signing-profiles/{PROFILE}/revoke"),
            body: Some(json!({
                "profileVersion": "aaaaaaaaaa",
                "reason": "the profile's material is no longer trusted",
                "effectiveTime": 1_700_000_000,
            })),
            resource: profile.clone(),
        },
        Call {
            operation: "SignPayload",
            method: "POST",
            path: "/signing-jobs/with-payload".to_string(),
            body: Some(json!({
                "profileName": PROFILE,
                // The base64 a restJson1 blob member carries on the wire.
                "payload": "aGVsbG8=",
                "payloadFormat": "application/vnd.cncf.notary.payload.v1+json",
            })),
            resource: profile.clone(),
        },
        Call {
            operation: "StartSigningJob",
            method: "POST",
            path: "/signing-jobs".to_string(),
            body: Some(json!({
                "source": { "s3": { "bucketName": "src", "key": "code.bin", "version": "1" } },
                "destination": { "s3": { "bucketName": "dst" } },
                "profileName": PROFILE,
                "clientRequestToken": "token-1",
            })),
            resource: profile,
        },
    ]
}

#[tokio::test]
async fn every_signer_action_is_decided_on_the_resource_the_request_addresses() {
    let router = build_router();
    let profile_version_arn = given_profile(&router, PROFILE).await;
    let session = given_session(&router).await;

    for call in every_decided_call(&profile_version_arn) {
        let case = call.operation;
        let action = format!("{SIGNER}:{case}");

        // Every action of this service is granted, and exactly this one denied: the refusal
        // has to name THIS action and the resource THIS request addresses.
        given_role_policy(&router, &allow_all_but(&action, json!("*"))).await;
        let (status, body) = signer(
            &router,
            &session.access_key_id,
            call.method,
            &call.path,
            call.body.as_ref(),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (403, ACCESS_DENIED),
            "case {case:?}: an explicitly denied call must be refused with the modelled 403, \
             got {status} {body}"
        );
        assert_eq!(
            error_message(&body),
            not_authorized(&session.arn, &action, &call.resource, EXPLICIT_DENY),
            "case {case:?}: the refusal must name the principal, the action and the resource \
             the request addressed, got {body}"
        );

        // The same call under a grant of every action: whatever the operation then answers
        // about its own input or about the resources it names, it is no longer a 403 — so the
        // gate keys off the caller's policy rather than refusing the operation outright.
        given_role_policy(&router, &allow(json!(format!("{SIGNER}:*")), json!("*"))).await;
        let (status, body) = signer(
            &router,
            &session.access_key_id,
            call.method,
            &call.path,
            call.body.as_ref(),
        )
        .await;
        assert_ne!(
            status, 403,
            "case {case:?}: a call every action is granted for must not be refused 403, got \
             {status} {body}"
        );

        // And with nothing granted at all, IAM's default deny refuses it naming the same
        // action and resource.
        given_role_policy(&router, &allow(json!("s3:GetObject"), json!("*"))).await;
        let (status, body) = signer(
            &router,
            &session.access_key_id,
            call.method,
            &call.path,
            call.body.as_ref(),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (403, ACCESS_DENIED),
            "case {case:?}: a call no document allows must be refused with the modelled 403, \
             got {status} {body}"
        );
        assert_eq!(
            error_message(&body),
            not_authorized(&session.arn, &action, &call.resource, &no_policy_allows(&action)),
            "case {case:?}: the default-deny refusal must name the action nothing granted, got \
             {body}"
        );
    }
}

#[tokio::test]
async fn the_data_plane_endpoint_decides_the_same_call_with_the_same_documents() {
    let router = build_router();
    let profile_version_arn = given_profile(&router, PROFILE).await;
    let session = given_session(&router).await;
    let action = format!("{SIGNER}:GetRevocationStatus");
    let resource = profile_arn(PROFILE);

    // Signer's data plane publishes its own endpoint behind the same `signer` action
    // namespace, and the router resolves a request's service from its credential scope — so a
    // request whose scope names that endpoint is served by it, carrying the same session.
    let (status, body) =
        get_revocation_status(&router, AMBIENT_KEY, DATA_SIGNER, &profile_version_arn).await;
    assert_eq!(
        status, 200,
        "case \"the account's own credential\": expected 200 from the data plane, got {status} \
         {body}"
    );

    given_role_policy(&router, &allow_all_but(&action, json!("*"))).await;
    let (status, body) = get_revocation_status(
        &router,
        &session.access_key_id,
        DATA_SIGNER,
        &profile_version_arn,
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (403, ACCESS_DENIED),
        "case \"an explicit deny\": the data plane must refuse with the modelled 403, got \
         {status} {body}"
    );
    assert_eq!(
        error_message(&body),
        not_authorized(&session.arn, &action, &resource, EXPLICIT_DENY),
        "case \"an explicit deny\": the data plane must word the refusal as the control plane \
         does, got {body}"
    );

    given_role_policy(&router, &allow(json!(format!("{SIGNER}:*")), json!("*"))).await;
    let (status, body) = get_revocation_status(
        &router,
        &session.access_key_id,
        DATA_SIGNER,
        &profile_version_arn,
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"a grant of every action\": expected 200 from the data plane, got {status} {body}"
    );
}
