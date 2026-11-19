// SPDX-License-Identifier: CC0-1.0

//! What a developer-authenticated Cognito token's PRINCIPAL TAGS decide about an Amazon S3
//! read the role's own document gates on the OBJECT's tags.
//!
//! RULE: the principal tags an Amazon Cognito Identity issuance records on the token it mints
//! are attached as SESSION TAGS to the session `sts:AssumeRoleWithWebIdentity` exchanges that
//! token for, so `aws:PrincipalTag/<key>` holds them for every request the vended credential
//! signs ("`aws:PrincipalTag/tag-key` — the tag attached to the principal making the request",
//! AWS General Reference, *Global condition context keys*); and a decision Amazon S3 takes
//! over an EXISTING object answers one `s3:ExistingObjectTag/<key>` per tag that object
//! carries ("`s3:ExistingObjectTag/<key>` … enables you to verify that an existing object tag
//! has the specific tag key and value", Amazon S3 User Guide, *Actions, resources, and
//! condition keys for Amazon S3*). So the ABAC document an attribute-based design writes —
//! ONE statement comparing the object's tag with the caller's own,
//! `StringEquals {"s3:ExistingObjectTag/<key>": "${aws:PrincipalTag/<key>}"}` — grants each
//! session exactly the objects whose tag matches the tag its token carried, and nothing else:
//! a session whose tag names another value, an object carrying no such tag, a session carrying
//! no tag at all and a session tagged under another key are all refused by the same document.
//! Attaching the tags needs `sts:TagSession` on the role's trust policy beside the assume
//! ("To pass session tags, the principal must have the `sts:TagSession` permission",
//! `AssumeRole$Tags`).
//!
//! Only a cross-service suite can pin this: the pool and the tagged token are Amazon Cognito
//! Identity's, the exchange and the session are STS's, the role, its trust policy and its
//! inline ABAC document are IAM's, and the decision is Amazon S3's — four crates over ONE
//! engine, as `core/router/src/lib.rs` mounts them.
//!
//! The rows differ along what the rule generalises over: WHICH value the session's tag names,
//! which tag the object read carries (the same value, another value, no tag at all, and a tag
//! set carrying more than one pair), whether the session carries a tag at all, and whether it
//! carries it under the key the document is written over.
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

/// The emulator's own address, for the UNSIGNED legs: an address, so the `Host` header names
/// no service — the shape a request from a client holding no Amazon Web Services credential
/// has, which is how the basic (classic) flow's exchange is sent.
const EMULATOR_HOST: &str = "127.0.0.1:9297";

/// The access key id every signed SETUP call carries — a key no session and no IAM user holds,
/// which speaks for the account itself.
const SETUP_KEY_ID: &str = "AKIDTEST";

/// The awsQuery API versions IAM and STS are called under.
const IAM_API_VERSION: &str = "2010-05-08";
const STS_API_VERSION: &str = "2011-06-15";

/// The developer provider the pool is configured with, which is the `Logins` key the
/// developer-authenticated issuance below mints its token under.
const DEVELOPER_PROVIDER: &str = "login.xsvc-departments.example";

/// The bucket every read below addresses.
const BUCKET: &str = "xsvc-cognito-existing-object-tag";

/// The tag key both halves of the ABAC comparison are written over: the object's tag and the
/// session's own.
const DEPARTMENT_TAG: &str = "department";

/// A tag key the document is NOT written over, which one session carries instead.
const OTHER_TAG: &str = "team";

/// The two tag values the rows span.
const ENGINEERING: &str = "engineering";
const FINANCE: &str = "finance";

/// The role every exchange below assumes, and the name its inline ABAC document is written
/// under.
const ROLE_NAME: &str = "xsvc-cognito-department-reader";
const POLICY_NAME: &str = "abac-s3-department";

/// The code and status S3 refuses an unauthorized read with.
const ACCESS_DENIED: &str = "AccessDenied";

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

/// One awsQuery call — IAM's and STS's wire form — with every component percent-encoded so a
/// policy document and a web identity token travel verbatim.
fn query(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    signing: Signing<'_>,
    action: &str,
    version: &str,
    members: &[(&str, &str)],
) -> Answer {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    dispatch(
        router,
        runtime,
        signing,
        "POST",
        "/",
        &[("content-type", "application/x-www-form-urlencoded")],
        bytes::Bytes::from(body),
    )
}

/// One IAM call.
fn iam(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    action: &str,
    members: &[(&str, &str)],
) -> Answer {
    query(
        router,
        runtime,
        as_setup("iam"),
        action,
        IAM_API_VERSION,
        members,
    )
}

/// One S3 call on the path-style address, signed with `access_key_id`, carrying `headers`
/// beside the body.
fn s3(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    access_key_id: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Answer {
    let mut all: Vec<(&str, &str)> = vec![("content-type", "application/xml")];
    all.extend_from_slice(headers);
    dispatch(
        router,
        runtime,
        Signing::As {
            signing_name: "s3",
            access_key_id,
        },
        method,
        path,
        &all,
        bytes::Bytes::from(body.to_owned()),
    )
}

/// The trust policy of the role the tokens below are exchanged for: the `Federated` Cognito
/// Identity principal, the web-identity assume, the `sts:TagSession` permission a TAGGED
/// assume needs beside it, and the `:aud` condition restricting the assume to this pool.
fn pool_trust_policy(pool_id: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Federated": "cognito-identity.amazonaws.com"},
            "Action": ["sts:AssumeRoleWithWebIdentity", "sts:TagSession"],
            "Condition": {"StringEquals": {"cognito-identity.amazonaws.com:aud": pool_id}},
        }],
    })
    .to_string()
}

/// The ABAC document in force on that role: ONE statement, naming no department, comparing the
/// OBJECT's tag with the CALLER's own session tag — so what it grants is decided per session
/// and per object by those two tags alone.
fn department_scoped_document() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": "s3:GetObject",
            "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
            "Condition": {"StringEquals": {
                format!("s3:ExistingObjectTag/{DEPARTMENT_TAG}"):
                    format!("${{aws:PrincipalTag/{DEPARTMENT_TAG}}}"),
            }},
        }],
    })
    .to_string()
}

/// One object the rows read: the key it sits at, the tag set it was written with as the
/// `x-amz-tagging` header carries it, and the department its tag names (`None` for an object
/// carrying no such tag).
struct StoredObject {
    key: &'static str,
    tagging: &'static str,
    department: Option<&'static str>,
}

/// The objects the rows read: one per department, one carrying NO tag at all, and one whose tag
/// set carries more than one pair — so the family is answered per tag rather than for a
/// single-tag object alone.
const OBJECTS: &[StoredObject] = &[
    StoredObject {
        key: "engineering-secret.txt",
        tagging: "department=engineering",
        department: Some(ENGINEERING),
    },
    StoredObject {
        key: "finance-secret.txt",
        tagging: "department=finance",
        department: Some(FINANCE),
    },
    StoredObject {
        key: "untagged-secret.txt",
        tagging: "",
        department: None,
    },
    StoredObject {
        key: "engineering-gold.txt",
        tagging: "tier=gold&department=engineering",
        department: Some(ENGINEERING),
    },
];

/// The bytes one object holds.
fn object_body(key: &str) -> String {
    format!("the bytes of {key}")
}

/// The ARN of an object of [`BUCKET`], as a refusal names it.
fn object_arn(key: &str) -> String {
    format!("arn:aws:s3:::{BUCKET}/{key}")
}

/// One session the rows read with: what it is labelled by, the access key id it signs with,
/// the assumed-role ARN a refusal names it by, and the department its own session tag names
/// (`None` for a session carrying no tag under the document's key).
struct Session {
    label: String,
    access_key_id: String,
    principal: String,
    department: Option<&'static str>,
}

/// Mint one developer-authenticated session: `GetOpenIdTokenForDeveloperIdentity` asking for
/// `principal_tags` (signed with the account's own credential, as a developer backend calls
/// it), then the UNSIGNED `sts:AssumeRoleWithWebIdentity` that exchanges the token it issued
/// for a session of `role_arn`.
#[allow(clippy::too_many_arguments)]
fn developer_session(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    pool_id: &str,
    role_arn: &str,
    label: &str,
    developer_user: &str,
    principal_tags: Option<Value>,
    department: Option<&'static str>,
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
    let token = issued.json()["Token"]
        .as_str()
        .unwrap_or_else(|| panic!("case {label:?}: the issuance reports a Token"))
        .to_string();

    // The basic (classic) flow's exchange: a client holding no Amazon Web Services credential
    // presents the token it was issued and gets a session of the role back.
    let session_name = format!("sess-{developer_user}");
    let exchanged = query(
        router,
        runtime,
        Signing::Anonymous,
        "AssumeRoleWithWebIdentity",
        STS_API_VERSION,
        &[
            ("RoleArn", role_arn),
            ("RoleSessionName", &session_name),
            ("WebIdentityToken", &token),
        ],
    );
    assert_eq!(
        exchanged.status,
        200,
        "case {label:?}: the web-identity exchange must be served, got {} with {:?}",
        exchanged.status,
        exchanged.text()
    );
    let body = exchanged.text();
    let access_key_id = element(&body, "AccessKeyId");
    assert!(
        !access_key_id.is_empty(),
        "case {label:?}: the exchange must carry an AccessKeyId, got {body:?}"
    );
    // The session the exchange minted is a session of THAT role under the name the request
    // asked for, which is what the assumed-role ARN names it by.
    let principal = format!(
        "arn:aws:sts::{}:assumed-role/{ROLE_NAME}/{session_name}",
        ::engine::EMULATOR_ACCOUNT_ID
    );
    assert_eq!(
        element(&body, "Arn"),
        principal,
        "case {label:?}: the exchange must report the assumed-role session {principal:?}, got \
         {body:?}"
    );
    Session {
        label: label.to_string(),
        access_key_id,
        principal,
        department,
    }
}

#[test]
fn a_developer_tokens_principal_tag_decides_an_object_tag_gated_read() {
    std::thread::Builder::new()
        .name("cognito-developer-token-object-tag".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");

            // The bucket and one object per tag set, written with the account's own credential.
            let created = s3(
                &router,
                &runtime,
                SETUP_KEY_ID,
                "PUT",
                &format!("/{BUCKET}"),
                &[],
                "",
            );
            assert_eq!(
                created.status,
                200,
                "setup: CreateBucket({BUCKET:?}) must succeed, got {} with {:?}",
                created.status,
                created.text()
            );
            for held in OBJECTS {
                let tagging: &[(&str, &str)] = if held.tagging.is_empty() {
                    &[]
                } else {
                    &[("x-amz-tagging", held.tagging)]
                };
                let written = s3(
                    &router,
                    &runtime,
                    SETUP_KEY_ID,
                    "PUT",
                    &format!("/{BUCKET}/{}", held.key),
                    tagging,
                    &object_body(held.key),
                );
                assert_eq!(
                    written.status,
                    200,
                    "setup: PutObject({:?}, tagging {:?}) must succeed, got {} with {:?}",
                    held.key,
                    held.tagging,
                    written.status,
                    written.text()
                );
            }

            // The pool: a developer provider, which is what the issuances below mint their
            // tokens under.
            let pool = rpc(
                &router,
                &runtime,
                as_setup("cognito-identity"),
                "AWSCognitoIdentityService.CreateIdentityPool",
                &json!({
                    "IdentityPoolName": "xsvc-existing-object-tag-departments",
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
                    ("PolicyDocument", &department_scoped_document()),
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

            // One session per tag the rows span: each department, a session asking for no
            // principal tag at all, and one tagged under a key the document is not written
            // over — the last two carry no value for `aws:PrincipalTag/department`.
            let sessions = [
                developer_session(
                    &router,
                    &runtime,
                    &pool_id,
                    &role_arn,
                    "tagged engineering",
                    "user-of-engineering",
                    Some(json!({DEPARTMENT_TAG: ENGINEERING})),
                    Some(ENGINEERING),
                ),
                developer_session(
                    &router,
                    &runtime,
                    &pool_id,
                    &role_arn,
                    "tagged finance",
                    "user-of-finance",
                    Some(json!({DEPARTMENT_TAG: FINANCE})),
                    Some(FINANCE),
                ),
                developer_session(
                    &router,
                    &runtime,
                    &pool_id,
                    &role_arn,
                    "carrying no principal tag",
                    "user-of-no-department",
                    None,
                    None,
                ),
                developer_session(
                    &router,
                    &runtime,
                    &pool_id,
                    &role_arn,
                    "tagged under another key",
                    "user-of-another-key",
                    Some(json!({OTHER_TAG: ENGINEERING})),
                    None,
                ),
            ];

            // Every session reads every object: the one document serves exactly the objects
            // whose own tag equals the session's, and refuses every other — another
            // department's object, an object carrying no such tag, and every read by a session
            // that carries no value for the key the document compares.
            for session in &sessions {
                for held in OBJECTS {
                    let served = session.department.is_some()
                        && session.department == held.department;
                    let case = format!("{} reading {}", session.label, held.key);
                    let answer = s3(
                        &router,
                        &runtime,
                        &session.access_key_id,
                        "GET",
                        &format!("/{BUCKET}/{}", held.key),
                        &[],
                        "",
                    );
                    if served {
                        assert_eq!(
                            (answer.status, answer.text()),
                            (200, object_body(held.key)),
                            "case {case:?}: the read must be served the object, got {} with {:?}",
                            answer.status,
                            answer.text()
                        );
                        continue;
                    }
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
                            && stated.contains(&object_arn(held.key)),
                        "case {case:?}: the refusal must name the session {:?} and the object \
                         {:?}, got {stated:?}",
                        session.principal,
                        object_arn(held.key)
                    );
                }
            }
        })
        .expect("spawn the cognito-developer-token-object-tag thread")
        .join()
        .expect(
            "a developer token's principal tag must decide an object-tag gated read",
        );
}
