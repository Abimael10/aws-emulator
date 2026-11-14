// SPDX-License-Identifier: CC0-1.0

//! What a Cognito-vended session's PRINCIPAL TAGS decide about a later Amazon S3 read.
//!
//! RULE: an identity pool that maps principal tags performs its exchange as a tagged
//! `sts:AssumeRoleWithWebIdentity`, so the pairs it attaches are what
//! `aws:PrincipalTag/<key>` holds for every request the vended credential signs
//! ("`aws:PrincipalTag/tag-key` — the tag attached to the principal making the request", AWS
//! General Reference, *Global condition context keys*). An identity-based policy written over
//! that key — the `${aws:PrincipalTag/<key>}` variable an ABAC design puts in a resource ARN —
//! therefore resolves to the value THIS session carries: each session reaches the resources
//! its own tag names and is refused the ones another session's tag names, under one document
//! that names no tenant at all. Attaching those tags needs `sts:TagSession` on the role's
//! trust policy beside the assume ("To pass session tags, the principal must have the
//! `sts:TagSession` permission", `AssumeRole$Tags`).
//!
//! Only a cross-service suite can pin this: the role, its trust policy and its inline ABAC
//! document are written by `iam:CreateRole`/`iam:PutRolePolicy`, the pool and its principal-tag
//! mapping by Amazon Cognito Identity, the tagged session is minted by the vend through STS,
//! and the decision is taken by Amazon S3 — four crates over ONE engine, as
//! `core/router/src/lib.rs` mounts them.
//!
//! The rows differ along what the rule generalises over: WHICH tenant the session's tag names,
//! which prefix the read addresses (its own, another tenant's, a prefix no tenant owns), and a
//! session carrying NO tag at all, which the same document grants nothing — the developer
//! exchange that asks for no principal tag.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on a sized thread.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call below is signed for, and so the Region the pool lives in.
const REGION: &str = "us-east-1";

/// The emulator's own address, for the unsigned vend leg: an address, so the `Host` header
/// names no service and the call carries its service in `X-Amz-Target` alone.
const EMULATOR_HOST: &str = "127.0.0.1:9297";

/// The access key id every signed SETUP call carries — a key no session and no IAM user holds,
/// which speaks for the account itself.
const SETUP_KEY_ID: &str = "AKIDTEST";

/// The IAM API version every awsQuery call to IAM carries.
const IAM_API_VERSION: &str = "2010-05-08";

/// The identity-provider name a login carrying a token THIS service minted is handed over
/// under (`GetCredentialsForIdentity`'s own documentation).
const COGNITO_IDENTITY_PROVIDER: &str = "cognito-identity.amazonaws.com";

/// The developer provider the pool is configured with, which is the `Logins` key the
/// developer-authenticated exchange mints its token under.
const DEVELOPER_PROVIDER: &str = "login.xsvc-tenants.example";

/// The bucket every read below addresses, and the tag key the ABAC document is written over.
const BUCKET: &str = "xsvc-cognito-principal-tag";
const TENANT_TAG: &str = "tenant";

/// The role every vend below assumes, and the name its inline ABAC document is written under.
const ROLE_NAME: &str = "xsvc-cognito-tenant-reader";
const POLICY_NAME: &str = "tenant-scoped-read";

/// The IAM action a read of an object's current version is authorized under.
const GET_OBJECT_ACTION: &str = "s3:GetObject";

/// The code and status S3 refuses an unauthorized read with.
const ACCESS_DENIED: &str = "AccessDenied";

/// The two tenants the rows span, and a prefix no tenant's tag names.
const TENANT_A: &str = "tenant-a";
const TENANT_B: &str = "tenant-b";
const UNOWNED: &str = "shared";

/// What one call answered.
struct Answer {
    status: u16,
    body: bytes::Bytes,
}

impl Answer {
    fn json(&self) -> Value {
        if self.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&self.body).unwrap_or(Value::Null)
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// One element's text with the serializer's escaping undone — a refusal's `<Message>`
    /// quotes the resource, and those quotes travel as `&quot;`.
    fn element(&self, name: &str) -> Option<String> {
        elements(&self.text(), name).into_iter().next()
    }
}

/// How a request is signed: with a SigV4 credential scope naming a service and the key id it
/// is signed with, or with no credential scope at all — which is how an SDK sends an operation
/// whose model lists no auth scheme.
#[derive(Clone, Copy, Debug)]
enum Signing<'a> {
    As {
        signing_name: &'a str,
        access_key_id: &'a str,
    },
    Anonymous,
}

/// A setup call's signing, for the service whose signing name is `signing_name`.
fn as_setup(signing_name: &str) -> Signing<'_> {
    Signing::As {
        signing_name,
        access_key_id: SETUP_KEY_ID,
    }
}

/// Dispatch one request, addressed and signed as `signing` says.
fn dispatch(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    signing: Signing<'_>,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: bytes::Bytes,
) -> Answer {
    let host = match signing {
        Signing::As { signing_name, .. } => format!("{signing_name}.{REGION}.amazonaws.com"),
        Signing::Anonymous => EMULATOR_HOST.to_string(),
    };
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host.clone());
    if let Signing::As {
        signing_name,
        access_key_id,
    } = signing
    {
        // The signature is never checked: this router runs with authentication off. What the
        // scope carries is the key id a handler reads the calling principal from.
        builder = builder.header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential={access_key_id}/20260101/{REGION}/{signing_name}/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        );
    }
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(body).expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        body,
    }
}

/// One awsJson1_1 call, signed as `signing` says.
fn rpc(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    signing: Signing<'_>,
    target: &str,
    body: &Value,
) -> Answer {
    dispatch(
        router,
        runtime,
        signing,
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", target),
        ],
        bytes::Bytes::from(body.to_string()),
    )
}

/// One awsQuery call to IAM, with every component percent-encoded so a policy document travels
/// verbatim.
fn iam(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    action: &str,
    members: &[(&str, &str)],
) -> Answer {
    let mut body = format!("Action={action}&Version={IAM_API_VERSION}");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    dispatch(
        router,
        runtime,
        as_setup("iam"),
        "POST",
        "/",
        &[("content-type", "application/x-www-form-urlencoded")],
        bytes::Bytes::from(body),
    )
}

/// One S3 call on the path-style address, signed with `access_key_id`.
fn s3(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    access_key_id: &str,
    method: &str,
    path: &str,
    body: &str,
) -> Answer {
    dispatch(
        router,
        runtime,
        Signing::As {
            signing_name: "s3",
            access_key_id,
        },
        method,
        path,
        &[("content-type", "application/xml")],
        bytes::Bytes::from(body.to_owned()),
    )
}

/// The trust policy of the role the pool vends sessions of: the `Federated` Cognito Identity
/// principal, the web-identity assume, the `sts:TagSession` permission a TAGGED assume needs
/// beside it, and the `:aud` condition restricting the assume to this one pool's users.
fn pool_trust_policy(pool_id: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Federated": COGNITO_IDENTITY_PROVIDER},
            "Action": ["sts:AssumeRoleWithWebIdentity", "sts:TagSession"],
            "Condition": {"StringEquals": {
                format!("{COGNITO_IDENTITY_PROVIDER}:aud"): pool_id,
            }},
        }],
    })
    .to_string()
}

/// The ABAC document in force on that role: ONE statement, naming no tenant, whose resource
/// ARN carries the principal-tag variable — so what it grants is decided per session by the
/// tag that session carries.
fn tenant_scoped_document() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": GET_OBJECT_ACTION,
            "Resource": format!("arn:aws:s3:::{BUCKET}/org/${{aws:PrincipalTag/{TENANT_TAG}}}/*"),
        }],
    })
    .to_string()
}

/// The key one tenant's object sits at.
fn object_key(tenant: &str) -> String {
    format!("org/{tenant}/obj")
}

/// The ARN of an object of [`BUCKET`], as a refusal names it.
fn object_arn(key: &str) -> String {
    format!("arn:aws:s3:::{BUCKET}/{key}")
}

/// The bytes one tenant's object holds.
fn object_body(tenant: &str) -> String {
    format!("the object of {tenant}")
}

/// The access key id and the principal of one vended session.
struct Session {
    /// What the session's tag names, for the case label.
    label: String,
    access_key_id: String,
    /// The assumed-role session ARN a refusal names this session by.
    principal: String,
}

/// Mint one developer-authenticated session of `pool_id`: the backend's
/// `GetOpenIdTokenForDeveloperIdentity` (signed with the account's own credential, as a
/// developer backend calls it), asking for `principal_tags`, followed by the unsigned
/// `GetCredentialsForIdentity` that exchanges the token for credentials.
fn developer_session(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    pool_id: &str,
    label: &str,
    developer_user: &str,
    principal_tags: Option<Value>,
) -> Session {
    let mut request = json!({
        "IdentityPoolId": pool_id,
        "Logins": {DEVELOPER_PROVIDER: developer_user},
    });
    if let Some(principal_tags) = principal_tags {
        request["PrincipalTags"] = principal_tags;
    }
    let issued = rpc(
        router,
        runtime,
        as_setup("cognito-identity"),
        "AWSCognitoIdentityService.GetOpenIdTokenForDeveloperIdentity",
        &request,
    );
    assert_eq!(
        issued.status,
        200,
        "case {label:?}: GetOpenIdTokenForDeveloperIdentity must be served, got {} with {:?}",
        issued.status,
        issued.text()
    );
    let body = issued.json();
    let identity_id = body["IdentityId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {label:?}: the issuance reports an IdentityId"))
        .to_string();
    let token = body["Token"]
        .as_str()
        .unwrap_or_else(|| panic!("case {label:?}: the issuance reports a Token"))
        .to_string();

    let vended = rpc(
        router,
        runtime,
        Signing::Anonymous,
        "AWSCognitoIdentityService.GetCredentialsForIdentity",
        &json!({
            "IdentityId": &identity_id,
            "Logins": {COGNITO_IDENTITY_PROVIDER: token},
        }),
    );
    assert_eq!(
        vended.status,
        200,
        "case {label:?}: the vend must be served, got {} with {:?}",
        vended.status,
        vended.text()
    );
    let access_key_id = vended.json()["Credentials"]["AccessKeyId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !access_key_id.is_empty(),
        "case {label:?}: the vend must carry an AccessKeyId, got {:?}",
        vended.text()
    );
    Session {
        label: label.to_string(),
        access_key_id,
        principal: format!(
            "arn:aws:sts::{}:assumed-role/{ROLE_NAME}/CognitoIdentityCredentials",
            ::engine::EMULATOR_ACCOUNT_ID
        ),
    }
}

/// What one read must be answered with.
#[derive(Clone, Copy, Debug)]
enum Expected {
    /// The object's bytes.
    Served,
    /// 403 `AccessDenied`, the document granting this session nothing at that key.
    Denied,
}

#[test]
fn a_vended_sessions_principal_tag_scopes_its_object_reads() {
    std::thread::Builder::new()
        .name("cognito-vended-session-principal-tag".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");

            // The bucket and one object per prefix, written with the account's own credential.
            let created = s3(&router, &runtime, SETUP_KEY_ID, "PUT", &format!("/{BUCKET}"), "");
            assert_eq!(
                created.status,
                200,
                "setup: CreateBucket({BUCKET:?}) must succeed, got {} with {:?}",
                created.status,
                created.text()
            );
            for tenant in [TENANT_A, TENANT_B, UNOWNED] {
                let key = object_key(tenant);
                let written = s3(
                    &router,
                    &runtime,
                    SETUP_KEY_ID,
                    "PUT",
                    &format!("/{BUCKET}/{key}"),
                    &object_body(tenant),
                );
                assert_eq!(
                    written.status,
                    200,
                    "setup: PutObject({key:?}) must succeed, got {} with {:?}",
                    written.status,
                    written.text()
                );
            }

            // The pool: a developer provider, which is what the developer-authenticated
            // exchange below mints its token under.
            let pool = rpc(
                &router,
                &runtime,
                as_setup("cognito-identity"),
                "AWSCognitoIdentityService.CreateIdentityPool",
                &json!({
                    "IdentityPoolName": "xsvc-principal-tag-tenants",
                    "AllowUnauthenticatedIdentities": false,
                    "DeveloperProviderName": DEVELOPER_PROVIDER,
                }),
            );
            assert_eq!(
                pool.status,
                200,
                "setup: CreateIdentityPool must succeed, got {} with {:?}",
                pool.status,
                pool.text()
            );
            let pool_id = pool.json()["IdentityPoolId"]
                .as_str()
                .unwrap_or_else(|| panic!("setup: CreateIdentityPool reports an IdentityPoolId"))
                .to_string();

            // The role, its trust policy and the ONE ABAC document in force on it.
            let role = iam(
                &router,
                &runtime,
                "CreateRole",
                &[
                    ("RoleName", ROLE_NAME),
                    ("AssumeRolePolicyDocument", &pool_trust_policy(&pool_id)),
                ],
            );
            assert_eq!(
                role.status,
                200,
                "setup: CreateRole must succeed, got {} with {:?}",
                role.status,
                role.text()
            );
            let role_arn = element(&role.text(), "Arn");
            let written = iam(
                &router,
                &runtime,
                "PutRolePolicy",
                &[
                    ("RoleName", ROLE_NAME),
                    ("PolicyName", POLICY_NAME),
                    ("PolicyDocument", &tenant_scoped_document()),
                ],
            );
            assert_eq!(
                written.status,
                200,
                "setup: PutRolePolicy must succeed, got {} with {:?}",
                written.status,
                written.text()
            );
            let configured = rpc(
                &router,
                &runtime,
                as_setup("cognito-identity"),
                "AWSCognitoIdentityService.SetIdentityPoolRoles",
                &json!({
                    "IdentityPoolId": &pool_id,
                    "Roles": {"authenticated": &role_arn},
                }),
            );
            assert_eq!(
                configured.status,
                200,
                "setup: SetIdentityPoolRoles must succeed, got {} with {:?}",
                configured.status,
                configured.text()
            );

            // One session per tenant, plus one asking for no principal tag at all.
            let sessions: Vec<(Session, Option<&str>)> = vec![
                (
                    developer_session(
                        &router,
                        &runtime,
                        &pool_id,
                        TENANT_A,
                        "user-of-tenant-a",
                        Some(json!({TENANT_TAG: TENANT_A})),
                    ),
                    Some(TENANT_A),
                ),
                (
                    developer_session(
                        &router,
                        &runtime,
                        &pool_id,
                        TENANT_B,
                        "user-of-tenant-b",
                        Some(json!({TENANT_TAG: TENANT_B})),
                    ),
                    Some(TENANT_B),
                ),
                (
                    developer_session(
                        &router,
                        &runtime,
                        &pool_id,
                        "no principal tag",
                        "user-of-no-tenant",
                        None,
                    ),
                    None,
                ),
            ];

            // Every session reads every prefix: its own tenant's object is served, and every
            // other prefix — another tenant's and the one no tag names — is refused, under the
            // one document that names no tenant. A session carrying no tag reaches nothing.
            for (session, tenant) in &sessions {
                for key_tenant in [TENANT_A, TENANT_B, UNOWNED] {
                    let expected = if *tenant == Some(key_tenant) {
                        Expected::Served
                    } else {
                        Expected::Denied
                    };
                    let key = object_key(key_tenant);
                    let case = format!("{} reading {key}", session.label);
                    let answer = s3(
                        &router,
                        &runtime,
                        &session.access_key_id,
                        "GET",
                        &format!("/{BUCKET}/{key}"),
                        "",
                    );
                    match expected {
                        Expected::Served => {
                            assert_eq!(
                                (answer.status, answer.text()),
                                (200, object_body(key_tenant)),
                                "case {case:?}: the read must be served the object, got {} with \
                                 {:?}",
                                answer.status,
                                answer.text()
                            );
                        }
                        Expected::Denied => {
                            assert_eq!(
                                answer.status,
                                403,
                                "case {case:?}: the read must be refused 403, got {} with {:?}",
                                answer.status,
                                answer.text()
                            );
                            assert_eq!(
                                answer.element("Code").as_deref(),
                                Some(ACCESS_DENIED),
                                "case {case:?}: expected the code {ACCESS_DENIED}, got {:?}",
                                answer.text()
                            );
                            let stated = answer.element("Message").unwrap_or_default();
                            assert!(
                                stated.contains(&session.principal)
                                    && stated.contains(&object_arn(&key)),
                                "case {case:?}: the refusal must name the vended principal {:?} \
                                 and the object {:?}, got {stated:?}",
                                session.principal,
                                object_arn(&key)
                            );
                        }
                    }
                }
            }
        })
        .expect("spawn the cognito-principal-tag thread")
        .join()
        .expect("a vended session's principal tag must scope its object reads");
}
