// SPDX-License-Identifier: CC0-1.0

//! What a developer-authenticated Amazon Cognito Identity token's PRINCIPAL TAGS decide about
//! the Amazon S3 reads a multi-tenant role document writes over those tags.
//!
//! RULE: the principal tags an Amazon Cognito Identity issuance records on the token it mints
//! are attached as SESSION TAGS to the session `sts:AssumeRoleWithWebIdentity` exchanges that
//! token for, so for EVERY key the issuance recorded — and for no key it did not —
//! `aws:PrincipalTag/<key>` holds that key's value while the request the vended credential
//! signs is authorized ("`aws:PrincipalTag/tag-key` — the tag attached to the principal making
//! the request", AWS General Reference, *Global condition context keys*). A key the session
//! carries a value for is therefore answered in BOTH places a document names one: substituted
//! into an element written as the policy variable `${aws:PrincipalTag/<key>}` ("you can use
//! policy variables in the Resource element", AWS IAM User Guide, *IAM policy elements:
//! variables and tags*), and compared in a `Condition` clause; a key the session carries no
//! value for substitutes nowhere, so an element naming it matches nothing and grants nothing.
//! Attaching the tags needs `sts:TagSession` on the role's trust policy beside the assume
//! ("To pass session tags, the principal must have the `sts:TagSession` permission",
//! `AssumeRole$Tags`).
//!
//! The session the exchange minted is a session of the role under the name the exchange asked
//! for, which is what `sts:GetCallerIdentity` answers for the vended credential ("Returns
//! details about the IAM user or role whose credentials are used to call the operation"), and
//! an issuance for a pool that no longer exists is refused `ResourceNotFoundException` naming
//! that pool — the two legs a run that stops at the first ABAC-gated read never reaches.
//!
//! The rows differ along what the rule generalises over: HOW MANY pairs the issuance recorded
//! (none at all, one, three), WHICH key they are recorded under (the one the `Resource` element
//! is written over, the one the `Condition` clause is written over, both, neither), and which
//! VALUE each names — read against a set of objects spanning the prefix each tenant owns, a
//! prefix whose name merely starts with another tenant's, and the one object the `Condition`
//! clause gates.
//!
//! Only a cross-service suite can pin this: the pool and the tagged token are Amazon Cognito
//! Identity's, the exchange, the session and the caller identity are STS's, the role, its trust
//! policy and its inline document are IAM's, and the read decisions are Amazon S3's — four
//! crates over ONE engine, as `core/router/src/lib.rs` mounts them.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on a sized thread.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode, opt_element};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call below is signed for, and so the Region the pool lives in.
const REGION: &str = "us-east-1";

/// The emulator's own address, for the UNSIGNED exchange: an address, so the `Host` header
/// names no service — the shape a request from a client holding no Amazon Web Services
/// credential has, which is how the basic (classic) flow's exchange is sent.
const EMULATOR_HOST: &str = "127.0.0.1:9297";

/// The access key id every signed SETUP call carries — a key no session and no IAM user holds,
/// which speaks for the account itself.
const SETUP_KEY_ID: &str = "AKIDTEST";

/// The awsQuery API versions IAM and STS are called under.
const IAM_API_VERSION: &str = "2010-05-08";
const STS_API_VERSION: &str = "2011-06-15";

/// The developer provider the pool is configured with, which is the `Logins` key every
/// issuance below mints its token under.
const DEVELOPER_PROVIDER: &str = "login.xsvc-tenants.example";

/// The bucket every read below addresses.
const BUCKET: &str = "xsvc-cognito-tenant-prefix";

/// The tag key the `Resource` element of the tenant grant is written over, as the policy
/// variable `${aws:PrincipalTag/tenant}`.
const TENANT_TAG: &str = "tenant";

/// The tag key the `Condition` clause of the shared grant is written over.
const CLEARANCE_TAG: &str = "clearance";

/// A third key one issuance records beside the other two, which no statement names at all.
const SEAT_TAG: &str = "seat";

/// The two tenants the rows span, and the clearance values: the one the shared grant admits
/// and one it does not.
const TENANT_A: &str = "tenant-a";
const TENANT_B: &str = "tenant-b";
const ADMITTED_CLEARANCE: &str = "high";
const OTHER_CLEARANCE: &str = "low";

/// The role every exchange below assumes, and the name its inline document is written under.
const ROLE_NAME: &str = "xsvc-cognito-tenant-reader";
const POLICY_NAME: &str = "abac-tenant-prefix";

/// The object the `Condition`-gated statement names, which sits under no tenant's prefix.
const SHARED_KEY: &str = "shared.txt";

/// Every object the rows read: one under each tenant's own prefix, one under a prefix whose
/// name merely STARTS with the first tenant's (so a grant over `<tenant>/*` is not a grant over
/// everything spelled with that prefix), and the one object the `Condition` clause gates.
const OBJECTS: &[&str] = &[
    "tenant-a/doc.txt",
    "tenant-b/doc.txt",
    "tenant-ab/doc.txt",
    SHARED_KEY,
];

/// The code and status Amazon S3 refuses an unauthorized read with.
const ACCESS_DENIED: &str = "AccessDenied";

/// The refusal a call naming a pool this emulator does not hold is answered with.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The status that refusal travels with: Amazon Cognito Identity's awsJson1_1 refusals carry
/// the service's client-error status, not the 404 the shape's `@httpError` annotation names —
/// the rule the owning crate's `wire_error_envelope` suite pins for every operation that can
/// refuse that way.
const CLIENT_ERROR_STATUS: u16 = 400;

/// One issuance the rows mint a session from: what it is labelled by, the developer user the
/// backend authenticated, and the `PrincipalTags` the request records (`None` for a request
/// carrying the member not at all).
struct Mint {
    label: &'static str,
    developer_user: &'static str,
    principal_tags: Option<&'static [(&'static str, &'static str)]>,
}

/// The issuances the rows span: one pair under the key the `Resource` element names, the same
/// under the other tenant, three pairs naming both keys the document is written over plus one
/// no statement names, one pair under the `Condition` key alone, one naming a value that clause
/// does not admit, and an issuance recording no pair at all.
const MINTS: &[Mint] = &[
    Mint {
        label: "one pair, the first tenant",
        developer_user: "user-of-tenant-a",
        principal_tags: Some(&[(TENANT_TAG, TENANT_A)]),
    },
    Mint {
        label: "one pair, the second tenant",
        developer_user: "user-of-tenant-b",
        principal_tags: Some(&[(TENANT_TAG, TENANT_B)]),
    },
    Mint {
        label: "three pairs, a tenant and an admitted clearance",
        developer_user: "user-of-tenant-a-cleared",
        principal_tags: Some(&[
            (TENANT_TAG, TENANT_A),
            (CLEARANCE_TAG, ADMITTED_CLEARANCE),
            (SEAT_TAG, "7"),
        ]),
    },
    Mint {
        label: "one pair, an admitted clearance and no tenant",
        developer_user: "user-of-clearance-only",
        principal_tags: Some(&[(CLEARANCE_TAG, ADMITTED_CLEARANCE)]),
    },
    Mint {
        label: "one pair, a clearance the clause does not admit",
        developer_user: "user-of-other-clearance",
        principal_tags: Some(&[(CLEARANCE_TAG, OTHER_CLEARANCE)]),
    },
    Mint {
        label: "no PrincipalTags member at all",
        developer_user: "user-of-no-tags",
        principal_tags: None,
    },
];

impl Mint {
    /// The value this issuance records for `key`, or `None` for a key it records no pair
    /// under — which is what the session then carries no value for.
    fn tag(&self, key: &str) -> Option<&'static str> {
        self.principal_tags?
            .iter()
            .find(|(recorded, _)| *recorded == key)
            .map(|(_, value)| *value)
    }

    /// The `PrincipalTags` member of the issuance request, absent for a row that records none.
    fn member(&self) -> Option<Value> {
        let pairs = self.principal_tags?;
        let mut map = Map::new();
        for (key, value) in pairs {
            map.insert((*key).to_string(), Value::String((*value).to_string()));
        }
        Some(Value::Object(map))
    }

    /// The session name the exchange below asks for — one per issuance, so the ARN each
    /// session is named by is its own.
    fn session_name(&self) -> String {
        format!("sess-{}", self.developer_user)
    }

    /// Whether the ONE document in force authorizes this session's read of `key`: the tenant
    /// grant reaches exactly the objects under the prefix the session's own `tenant` tag names,
    /// and the shared grant the one object its clause names, for a session whose `clearance`
    /// tag holds the value that clause admits. A session carrying no value for a key leaves the
    /// element naming it matching nothing.
    fn authorizes(&self, key: &str) -> bool {
        let by_prefix = self
            .tag(TENANT_TAG)
            .is_some_and(|tenant| key.starts_with(&format!("{tenant}/")));
        let by_clearance = key == SHARED_KEY && self.tag(CLEARANCE_TAG) == Some(ADMITTED_CLEARANCE);
        by_prefix || by_clearance
    }
}

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

    /// One element's text of an XML answer — a refusal's `<Message>` quotes the resource, and
    /// those quotes travel escaped.
    fn element(&self, name: &str) -> Option<String> {
        opt_element(&self.text(), name)
    }

    /// The refusal an awsJson1_1 answer states: the shape it names and the message it carries.
    fn refusal(&self) -> (String, String) {
        let body = self.json();
        let named = body["__type"].as_str().unwrap_or_default().to_string();
        let message = body["message"]
            .as_str()
            .or_else(|| body["Message"].as_str())
            .unwrap_or_default()
            .to_string();
        (named, message)
    }
}

/// How a request is signed: with a SigV4 credential scope naming a service and the key id it is
/// signed with, or with no credential scope at all — which is how an SDK sends an operation
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

/// One IAM call, signed with the account's own credential.
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

/// One Amazon S3 call on the path-style address, signed with `access_key_id`.
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

/// The trust policy of the role every token below is exchanged for: the `Federated` Cognito
/// Identity principal, the web-identity assume, the `sts:TagSession` permission a TAGGED assume
/// needs beside it, and the `:aud` condition restricting the assume to this pool.
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

/// The ONE document in force on that role, naming no tenant and no clearance of its own: a
/// statement whose `Resource` is written as the tenant tag's policy VARIABLE, so what it
/// reaches is the prefix the caller's own tag names, and a statement over the shared object
/// whose `Condition` compares the caller's clearance tag with the value it admits.
fn tenant_scoped_document() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Sid": "TenantPrefix",
                "Effect": "Allow",
                "Action": "s3:GetObject",
                "Resource": format!("arn:aws:s3:::{BUCKET}/${{aws:PrincipalTag/{TENANT_TAG}}}/*"),
            },
            {
                "Sid": "SharedUnderClearance",
                "Effect": "Allow",
                "Action": "s3:GetObject",
                "Resource": format!("arn:aws:s3:::{BUCKET}/{SHARED_KEY}"),
                "Condition": {"StringEquals": {
                    format!("aws:PrincipalTag/{CLEARANCE_TAG}"): ADMITTED_CLEARANCE,
                }},
            },
        ],
    })
    .to_string()
}

/// The bytes one object holds.
fn object_body(key: &str) -> String {
    format!("the bytes of {key}")
}

/// The ARN of an object of [`BUCKET`], as a refusal names it.
fn object_arn(key: &str) -> String {
    format!("arn:aws:s3:::{BUCKET}/{key}")
}

/// One session the rows read with: the access key id it signs with and the assumed-role ARN a
/// refusal names it by.
struct Session {
    access_key_id: String,
    principal: String,
}

/// Mint one developer-authenticated session for `mint`: `GetOpenIdTokenForDeveloperIdentity`
/// recording the row's `PrincipalTags` (signed with the account's own credential, as a
/// developer backend calls it), then the UNSIGNED `sts:AssumeRoleWithWebIdentity` that
/// exchanges the token it issued for a session of `role_arn`.
fn developer_session(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    pool_id: &str,
    role_arn: &str,
    mint: &Mint,
) -> Session {
    let label = mint.label;
    let mut request = json!({
        "IdentityPoolId": pool_id,
        "Logins": {DEVELOPER_PROVIDER: mint.developer_user},
    });
    if let Some(member) = mint.member() {
        request["PrincipalTags"] = member;
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
        .unwrap_or_else(|| {
            panic!(
                "case {label:?}: the issuance must report a Token, got {:?}",
                issued.text()
            )
        })
        .to_string();

    // The basic (classic) flow's exchange: a client holding no Amazon Web Services credential
    // presents the token it was issued and gets a session of the role back.
    let session_name = mint.session_name();
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
    let principal = format!(
        "arn:aws:sts::{}:assumed-role/{ROLE_NAME}/{session_name}",
        ::engine::EMULATOR_ACCOUNT_ID
    );
    Session {
        access_key_id,
        principal,
    }
}

#[test]
fn a_developer_tokens_principal_tags_scope_every_read_its_role_document_writes_over_them() {
    std::thread::Builder::new()
        .name("cognito-developer-token-tenant-prefix".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");

            // The bucket and one object per address the rows read, written with the account's
            // own credential.
            let created = s3(
                &router,
                &runtime,
                SETUP_KEY_ID,
                "PUT",
                &format!("/{BUCKET}"),
                "",
            );
            assert_eq!(
                created.status,
                200,
                "setup: CreateBucket({BUCKET:?}) must succeed, got {} with {:?}",
                created.status,
                created.text()
            );
            for key in OBJECTS {
                let written = s3(
                    &router,
                    &runtime,
                    SETUP_KEY_ID,
                    "PUT",
                    &format!("/{BUCKET}/{key}"),
                    &object_body(key),
                );
                assert_eq!(
                    written.status,
                    200,
                    "setup: PutObject({key:?}) must succeed, got {} with {:?}",
                    written.status,
                    written.text()
                );
            }

            // The pool: a developer provider, which is what every issuance below mints its
            // token under.
            let pool = rpc(
                &router,
                &runtime,
                as_setup("cognito-identity"),
                "AWSCognitoIdentityService.CreateIdentityPool",
                &json!({
                    "IdentityPoolName": "xsvc-tenant-prefix-pool",
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

            // The role, its trust policy and the ONE document in force on it.
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

            for mint in MINTS {
                let label = mint.label;
                let session = developer_session(&router, &runtime, &pool_id, &role_arn, mint);

                // The session the exchange minted is a session of THAT role under the name the
                // exchange asked for, which is what the token service answers for the vended
                // credential.
                let caller = query(
                    &router,
                    &runtime,
                    Signing::As {
                        signing_name: "sts",
                        access_key_id: &session.access_key_id,
                    },
                    "GetCallerIdentity",
                    STS_API_VERSION,
                    &[],
                );
                assert_eq!(
                    caller.status,
                    200,
                    "case {label:?}: GetCallerIdentity under the vended credential must be \
                     served, got {} with {:?}",
                    caller.status,
                    caller.text()
                );
                let reported = element(&caller.text(), "Arn");
                assert_eq!(
                    reported, session.principal,
                    "case {label:?}: expected the caller {:?}, got {reported:?}",
                    session.principal
                );
                let last_segment = reported.rsplit('/').next().unwrap_or_default();
                assert_eq!(
                    last_segment,
                    mint.session_name(),
                    "case {label:?}: expected the caller ARN to end in the session name {:?}, \
                     got {last_segment:?}",
                    mint.session_name()
                );

                // Every object, read by this session: the document reaches exactly what the
                // session's OWN tags resolve its two statements to.
                for key in OBJECTS {
                    let case = format!("{label} reading {key}");
                    let answer = s3(
                        &router,
                        &runtime,
                        &session.access_key_id,
                        "GET",
                        &format!("/{BUCKET}/{key}"),
                        "",
                    );
                    if mint.authorizes(key) {
                        assert_eq!(
                            (answer.status, answer.text()),
                            (200, object_body(key)),
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
                        stated.contains(&session.principal) && stated.contains(&object_arn(key)),
                        "case {case:?}: the refusal must name the session {:?} and the object \
                         {:?}, got {stated:?}",
                        session.principal,
                        object_arn(key)
                    );
                }
            }

            // The pool is the audience every issuance named, so once it is gone a further
            // issuance names nothing: the two reads a run stopping at the first gated read
            // never reaches.
            let deleted = rpc(
                &router,
                &runtime,
                as_setup("cognito-identity"),
                "AWSCognitoIdentityService.DeleteIdentityPool",
                &json!({"IdentityPoolId": &pool_id}),
            );
            assert_eq!(
                deleted.status,
                200,
                "case the pool is deleted: DeleteIdentityPool must be served, got {} with {:?}",
                deleted.status,
                deleted.text()
            );
            let absent = format!("IdentityPool '{pool_id}' not found.");
            let after: [(&str, Value); 2] = [
                (
                    "AWSCognitoIdentityService.DescribeIdentityPool",
                    json!({"IdentityPoolId": &pool_id}),
                ),
                (
                    "AWSCognitoIdentityService.GetOpenIdTokenForDeveloperIdentity",
                    json!({
                        "IdentityPoolId": &pool_id,
                        "Logins": {DEVELOPER_PROVIDER: "user-after-deletion"},
                        "PrincipalTags": {TENANT_TAG: TENANT_A},
                    }),
                ),
            ];
            for (target, request) in &after {
                let answer = rpc(
                    &router,
                    &runtime,
                    as_setup("cognito-identity"),
                    target,
                    request,
                );
                assert_eq!(
                    answer.status,
                    CLIENT_ERROR_STATUS,
                    "case {target:?} on the deleted pool: expected HTTP {CLIENT_ERROR_STATUS}, \
                     got {} with {:?}",
                    answer.status,
                    answer.text()
                );
                let (named, message) = answer.refusal();
                assert!(
                    named.ends_with(RESOURCE_NOT_FOUND),
                    "case {target:?} on the deleted pool: expected {RESOURCE_NOT_FOUND}, got \
                     {named:?}"
                );
                assert_eq!(
                    message, absent,
                    "case {target:?} on the deleted pool: expected the message {absent:?}, got \
                     {message:?}"
                );
            }
        })
        .expect("spawn the cognito-developer-token-tenant-prefix thread")
        .join()
        .expect(
            "a developer token's principal tags must scope every read its role document writes \
             over them",
        );
}
