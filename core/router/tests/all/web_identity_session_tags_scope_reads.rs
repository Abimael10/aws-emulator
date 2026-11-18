// SPDX-License-Identifier: CC0-1.0

//! What the PRINCIPAL TAGS a web identity token carries decide about the session it is
//! exchanged for, and about that session's later Amazon S3 reads.
//!
//! RULE: `sts:AssumeRoleWithWebIdentity` takes no tag member of its own, so the session tags
//! it attaches are the principal tags the PRESENTED TOKEN carries — for a token this
//! emulator issued, the ones its issuance recorded: the `PrincipalTags` a
//! `GetOpenIdTokenForDeveloperIdentity` asked for (its own member, which "enables you to
//! reference user attributes in your IAM permissions policy"), and, for a provider login, the
//! tags the pool's own `SetPrincipalTagAttributeMap` maps that provider's attributes onto.
//! Those pairs are what
//! `aws:PrincipalTag/<key>` holds for every request the vended credential signs
//! ("`aws:PrincipalTag/tag-key` — the tag attached to the principal making the request", AWS
//! General Reference, *Global condition context keys*), so an identity-based policy written
//! over that key — the `${aws:PrincipalTag/<key>}` variable an ABAC design puts in a resource
//! ARN — resolves to the value THIS session carries, and a token carrying no tag is granted
//! nothing by it.
//!
//! RULE: attaching them needs one grant more than the assume — "To pass session tags, the
//! principal must have the `sts:TagSession` permission" (`AssumeRole$Tags`, the STS model) —
//! so a role whose trust policy admits only the web-identity assume refuses an exchange of a
//! TAGGED token, naming that action, while an untagged token is still exchanged.
//!
//! Only a cross-service suite can pin this: the role, its trust policy and its ABAC document
//! are written by `iam:CreateRole`/`iam:PutRolePolicy`, the tagged token is minted by Amazon
//! Cognito Identity, the exchange is served by STS, and the read is decided by Amazon S3 —
//! four crates over ONE engine, as `core/router/src/lib.rs` mounts them.
//!
//! The rows differ along what the rules generalise over: how many tags the token carries
//! (none, one, several), which value the tag carries, which prefix the read addresses (the
//! one the tag names, another tag's, one no tag names) and whether the role's trust policy
//! grants the tag-passing permission.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on a sized thread.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call below is signed for.
const REGION: &str = "us-east-1";

/// The emulator's own address, for the anonymous legs: an address, so the `Host` header names
/// no service.
const EMULATOR_HOST: &str = "127.0.0.1:9297";

/// The access key id every signed SETUP call carries — a key no session and no IAM user
/// holds, which speaks for the account itself.
const SETUP_KEY_ID: &str = "AKIDTEST";

/// The awsQuery API versions of the two query services driven below.
const IAM_API_VERSION: &str = "2010-05-08";
const STS_API_VERSION: &str = "2011-06-15";

/// The issuer of every token an identity pool mints, and the `Federated` principal a role's
/// trust policy names to admit one.
const COGNITO_IDENTITY_PROVIDER: &str = "cognito-identity.amazonaws.com";

/// The developer provider the pool is configured with, which is the `Logins` key the
/// developer-authenticated issuance below mints its token under.
const DEVELOPER_PROVIDER: &str = "login.xsvc-webidentity-tags.example";

/// The bucket every read addresses, and the tag key the ABAC document is written over.
const BUCKET: &str = "xsvc-web-identity-session-tags";
const TENANT_TAG: &str = "tenant";

/// A second tag key, carried beside the first by one row: a tag the document says nothing
/// about must not change what the document grants.
const DEPARTMENT_TAG: &str = "department";

/// The role a tagged exchange assumes, the role whose trust policy omits the tag-passing
/// grant, and the name the ABAC document is written under.
const TAGGING_ROLE: &str = "xsvc-web-identity-tenant-reader";
const UNTAGGING_ROLE: &str = "xsvc-web-identity-no-tag-session";
const POLICY_NAME: &str = "tenant-scoped-read";

/// The IAM actions this suite is about.
const GET_OBJECT_ACTION: &str = "s3:GetObject";
const TAG_SESSION_ACTION: &str = "sts:TagSession";

/// The code AWS refuses an unauthorized call with, in both services' protocols.
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

    /// The text of the first `<name>…</name>` element of an XML body, with the serializer's
    /// escaping undone — the suite's one reader of an awsQuery answer
    /// ([`crate::query_wire::elements`]), since a refusal's `<Message>` quotes the resource
    /// and those quotes travel escaped.
    fn element(&self, name: &str) -> Option<String> {
        crate::query_wire::elements(&self.text(), name)
            .into_iter()
            .next()
    }
}

/// How a request is signed: for the service whose signing name it names, with the key id it
/// carries, or with no credential scope at all — which is how an SDK sends an operation whose
/// model lists no auth scheme.
#[derive(Clone, Copy, Debug)]
enum Signing<'a> {
    As {
        signing_name: &'a str,
        access_key_id: &'a str,
    },
    Anonymous,
}

/// A SETUP call's signing: the account's own key id, for `signing_name`.
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

/// One awsJson1_1 call to `target`, signed for `signing_name` or unsigned.
fn rpc(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    signing_name: Option<&str>,
    target: &str,
    body: &Value,
) -> Answer {
    dispatch(
        router,
        runtime,
        signing_name.map_or(Signing::Anonymous, as_setup),
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", target),
        ],
        bytes::Bytes::from(body.to_string()),
    )
}

/// One awsQuery call — IAM's and STS's wire form — signed for `signing_name` or unsigned,
/// with every component percent-encoded so a policy document and a compact JWS travel
/// verbatim.
fn query(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    signing_name: Option<&str>,
    action: &str,
    version: &str,
    members: &[(&str, &str)],
) -> Answer {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in members {
        body.push('&');
        body.push_str(&crate::query_wire::form_encode(name));
        body.push('=');
        body.push_str(&crate::query_wire::form_encode(value));
    }
    dispatch(
        router,
        runtime,
        signing_name.map_or(Signing::Anonymous, as_setup),
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

/// The trust policy of a role an identity pool's tokens are exchanged against: the
/// `Federated` Cognito Identity principal, the web-identity assume, the `:aud` condition
/// restricting it to this one pool's users, and — when `tag_session` — the `sts:TagSession`
/// permission a TAGGED exchange needs beside the assume.
fn trust_policy(pool_id: &str, tag_session: bool) -> String {
    let actions = if tag_session {
        json!(["sts:AssumeRoleWithWebIdentity", TAG_SESSION_ACTION])
    } else {
        json!("sts:AssumeRoleWithWebIdentity")
    };
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Federated": COGNITO_IDENTITY_PROVIDER},
            "Action": actions,
            "Condition": {"StringEquals": {
                format!("{COGNITO_IDENTITY_PROVIDER}:aud"): pool_id,
            }},
        }],
    })
    .to_string()
}

/// The ABAC document in force on both roles: ONE statement, naming no tenant, whose resource
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

/// `iam:CreateRole` with `trust` as its trust policy, plus `document` in force on it as the
/// role's one inline policy. Answers the role's ARN.
fn create_role_with(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    role_name: &str,
    trust: &str,
    document: &str,
) -> String {
    let created = query(
        router,
        runtime,
        Some("iam"),
        "CreateRole",
        IAM_API_VERSION,
        &[("RoleName", role_name), ("AssumeRolePolicyDocument", trust)],
    );
    assert_eq!(
        created.status,
        200,
        "setup: CreateRole({role_name:?}) must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    let role_arn = created
        .element("Arn")
        .unwrap_or_else(|| panic!("setup: CreateRole({role_name:?}) reports an Arn"));
    let written = query(
        router,
        runtime,
        Some("iam"),
        "PutRolePolicy",
        IAM_API_VERSION,
        &[
            ("RoleName", role_name),
            ("PolicyName", POLICY_NAME),
            ("PolicyDocument", document),
        ],
    );
    assert_eq!(
        written.status,
        200,
        "setup: PutRolePolicy({role_name:?}) must succeed, got {} with {:?}",
        written.status,
        written.text()
    );
    role_arn
}

/// [`create_role_with`] carrying the tenant-scoped ABAC document, which is the one in force
/// on every role the prefix-scoped rows are driven against.
fn create_role(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    role_name: &str,
    trust: &str,
) -> String {
    create_role_with(router, runtime, role_name, trust, &tenant_scoped_document())
}

/// The key one tenant's object sits at, and the bytes it holds.
fn object_key(tenant: &str) -> String {
    format!("org/{tenant}/obj")
}

fn object_body(tenant: &str) -> String {
    format!("the object of {tenant}")
}

/// The ARN of an object of [`BUCKET`], as a refusal names it.
fn object_arn(key: &str) -> String {
    format!("arn:aws:s3:::{BUCKET}/{key}")
}

/// Mint one developer-authenticated web identity token of `pool_id`, asking for
/// `principal_tags` (a request naming none mints an untagged token).
fn developer_token(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    pool_id: &str,
    case: &str,
    developer_user: &str,
    principal_tags: Option<Value>,
) -> String {
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
        Some("cognito-identity"),
        "AWSCognitoIdentityService.GetOpenIdTokenForDeveloperIdentity",
        &request,
    );
    assert_eq!(
        issued.status,
        200,
        "case {case:?}: GetOpenIdTokenForDeveloperIdentity must be served, got {} with {:?}",
        issued.status,
        issued.text()
    );
    issued.json()["Token"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the issuance reports a Token"))
        .to_string()
}

/// An UNSIGNED `sts:AssumeRoleWithWebIdentity`, the way the basic (classic) flow's client —
/// which holds no Amazon Web Services credential — makes it.
fn assume_with_web_identity(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    role_arn: &str,
    session_name: &str,
    token: &str,
) -> Answer {
    query(
        router,
        runtime,
        None,
        "AssumeRoleWithWebIdentity",
        STS_API_VERSION,
        &[
            ("RoleArn", role_arn),
            ("RoleSessionName", session_name),
            ("WebIdentityToken", token),
        ],
    )
}

/// What one read must be answered with.
#[derive(Clone, Copy, Debug)]
enum Expected {
    /// The object's bytes.
    Served,
    /// 403 `AccessDenied`: the document grants this session nothing at that key.
    Denied,
}

/// The bucket, its three objects and the pool every row below is driven against.
struct Estate {
    pool_id: String,
    tagging_role: String,
    untagging_role: String,
}

fn provision(router: &MultiServiceRouter, runtime: &tokio::runtime::Runtime) -> Estate {
    let created = s3(router, runtime, SETUP_KEY_ID, "PUT", &format!("/{BUCKET}"), "");
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
            router,
            runtime,
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

    let pool = rpc(
        router,
        runtime,
        Some("cognito-identity"),
        "AWSCognitoIdentityService.CreateIdentityPool",
        &json!({
            "IdentityPoolName": "xsvc-web-identity-tag-tenants",
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

    let tagging_role = create_role(
        router,
        runtime,
        TAGGING_ROLE,
        &trust_policy(&pool_id, true),
    );
    let untagging_role = create_role(
        router,
        runtime,
        UNTAGGING_ROLE,
        &trust_policy(&pool_id, false),
    );
    Estate {
        pool_id,
        tagging_role,
        untagging_role,
    }
}

/// The tags the exchanged token carries scope what the session may read, under one ABAC
/// document that names no tenant.
#[test]
fn the_tags_a_web_identity_token_carries_scope_its_sessions_object_reads() {
    std::thread::Builder::new()
        .name("web-identity-session-tags".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");
            let estate = provision(&router, &runtime);

            // One row: the case, the developer user the token is minted for, the
            // `PrincipalTags` the issuance asks for, and the tenant whose prefix the session
            // may read (`None` for a session no tag names a prefix for).
            let rows: Vec<(&str, &str, Option<Value>, Option<&str>)> = vec![
                (
                    "one tag naming the first tenant",
                    "user-of-tenant-a",
                    Some(json!({TENANT_TAG: TENANT_A})),
                    Some(TENANT_A),
                ),
                (
                    "one tag naming the other tenant",
                    "user-of-tenant-b",
                    Some(json!({TENANT_TAG: TENANT_B})),
                    Some(TENANT_B),
                ),
                (
                    "two tags, one of them the tenant",
                    "user-of-two-tags",
                    Some(json!({TENANT_TAG: TENANT_A, DEPARTMENT_TAG: "engineering"})),
                    Some(TENANT_A),
                ),
                (
                    "a tag the document says nothing about",
                    "user-of-another-tag",
                    Some(json!({DEPARTMENT_TAG: "engineering"})),
                    None,
                ),
                (
                    "a tag naming a prefix no object of a tenant sits under",
                    "user-of-no-prefix",
                    Some(json!({TENANT_TAG: UNOWNED})),
                    Some(UNOWNED),
                ),
                ("no tag at all", "user-of-no-tag", None, None),
            ];

            for (case, developer_user, principal_tags, readable) in rows {
                let token = developer_token(
                    &router,
                    &runtime,
                    &estate.pool_id,
                    case,
                    developer_user,
                    principal_tags,
                );
                let session_name = format!("session-{developer_user}");
                let exchanged = assume_with_web_identity(
                    &router,
                    &runtime,
                    &estate.tagging_role,
                    &session_name,
                    &token,
                );
                assert_eq!(
                    exchanged.status,
                    200,
                    "case {case:?}: the exchange must be served, got {} with {:?}",
                    exchanged.status,
                    exchanged.text()
                );
                let access_key_id = exchanged
                    .element("AccessKeyId")
                    .unwrap_or_else(|| panic!("case {case:?}: the exchange reports an AccessKeyId"));
                let principal = format!(
                    "arn:aws:sts::{}:assumed-role/{TAGGING_ROLE}/{session_name}",
                    ::engine::EMULATOR_ACCOUNT_ID
                );

                for key_tenant in [TENANT_A, TENANT_B, UNOWNED] {
                    let expected = if readable == Some(key_tenant) {
                        Expected::Served
                    } else {
                        Expected::Denied
                    };
                    let key = object_key(key_tenant);
                    let case = format!("{case} reading {key}");
                    let answer =
                        s3(&router, &runtime, &access_key_id, "GET", &format!("/{BUCKET}/{key}"), "");
                    match expected {
                        Expected::Served => assert_eq!(
                            (answer.status, answer.text()),
                            (200, object_body(key_tenant)),
                            "case {case:?}: the read must be served the object, got {} with {:?}",
                            answer.status,
                            answer.text()
                        ),
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
                                stated.contains(&principal) && stated.contains(&object_arn(&key)),
                                "case {case:?}: the refusal must name the vended principal {:?} \
                                 and the object {:?}, got {stated:?}",
                                principal,
                                object_arn(&key)
                            );
                        }
                    }
                }
            }
        })
        .expect("spawn the web-identity-session-tags thread")
        .join()
        .expect("the tags a token carries must scope its session's reads");
}

/// A role whose trust policy admits only the assume cannot be assumed WITH the tags a token
/// carries: the tagged exchange is refused naming `sts:TagSession`, and an untagged one of
/// the same token's pool is still exchanged.
#[test]
fn a_tagged_web_identity_exchange_needs_the_tag_session_grant() {
    std::thread::Builder::new()
        .name("web-identity-tag-session-grant".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");
            let estate = provision(&router, &runtime);

            // One row: the case, the tags the token carries, whether the role's trust policy
            // grants the tag-passing permission, and whether the exchange is served.
            let rows: Vec<(&str, Option<Value>, bool, bool)> = vec![
                (
                    "a tagged token against a role granting the tag-passing permission",
                    Some(json!({TENANT_TAG: TENANT_A})),
                    true,
                    true,
                ),
                (
                    "several tags against the same role",
                    Some(json!({TENANT_TAG: TENANT_B, DEPARTMENT_TAG: "engineering"})),
                    true,
                    true,
                ),
                (
                    "a tagged token against a role granting only the assume",
                    Some(json!({TENANT_TAG: TENANT_A})),
                    false,
                    false,
                ),
                (
                    "several tags against the role granting only the assume",
                    Some(json!({TENANT_TAG: TENANT_B, DEPARTMENT_TAG: "engineering"})),
                    false,
                    false,
                ),
                (
                    "an untagged token against the role granting only the assume",
                    None,
                    false,
                    true,
                ),
                (
                    "an untagged token against the role granting both",
                    None,
                    true,
                    true,
                ),
            ];

            for (index, (case, principal_tags, tag_session, served)) in rows.into_iter().enumerate()
            {
                let developer_user = format!("user-{index}");
                let token = developer_token(
                    &router,
                    &runtime,
                    &estate.pool_id,
                    case,
                    &developer_user,
                    principal_tags,
                );
                let role_arn = if tag_session {
                    &estate.tagging_role
                } else {
                    &estate.untagging_role
                };
                let exchanged = assume_with_web_identity(
                    &router,
                    &runtime,
                    role_arn,
                    &format!("session-{index}"),
                    &token,
                );
                if served {
                    assert_eq!(
                        exchanged.status,
                        200,
                        "case {case:?}: the exchange must be served, got {} with {:?}",
                        exchanged.status,
                        exchanged.text()
                    );
                    assert!(
                        exchanged.element("AccessKeyId").is_some(),
                        "case {case:?}: the exchange must vend a credential, got {:?}",
                        exchanged.text()
                    );
                } else {
                    assert_eq!(
                        exchanged.status,
                        403,
                        "case {case:?}: the exchange must be refused 403, got {} with {:?}",
                        exchanged.status,
                        exchanged.text()
                    );
                    assert_eq!(
                        exchanged.element("Code").as_deref(),
                        Some(ACCESS_DENIED),
                        "case {case:?}: expected the code {ACCESS_DENIED}, got {:?}",
                        exchanged.text()
                    );
                    let stated = exchanged.element("Message").unwrap_or_default();
                    assert!(
                        stated.contains(TAG_SESSION_ACTION),
                        "case {case:?}: the refusal must name {TAG_SESSION_ACTION}, got \
                         {stated:?}"
                    );
                    assert!(
                        exchanged.element("AccessKeyId").is_none(),
                        "case {case:?}: a refused exchange must vend no credential, got {:?}",
                        exchanged.text()
                    );
                }
            }
        })
        .expect("spawn the web-identity-tag-session-grant thread")
        .join()
        .expect("a tagged web identity exchange must need the tag-session grant");
}

/// The other place a token's principal tags come from: the pool's own
/// `SetPrincipalTagAttributeMap`, which maps one of the signing-in user's attributes onto a
/// tag key. The token the classic flow mints for such a login carries the mapped pair, so the
/// session the exchange mints is scoped by the USER's attribute — and a user whose attribute
/// names another tenant reaches only that tenant's prefix.
#[test]
fn a_pools_mapped_user_attribute_becomes_the_exchanged_sessions_session_tag() {
    std::thread::Builder::new()
        .name("web-identity-mapped-attribute-tags".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");
            // The bucket and its objects, which this leg reads too; its own identity pool and
            // role are created below, because the login it federates is a USER POOL's — which
            // is what a principal-tag ATTRIBUTE MAP is written over.
            let estate = provision(&router, &runtime);
            let _ = &estate.untagging_role;

            let idp = "AWSCognitoIdentityProviderService";
            let user_pool_id = {
                let created = rpc(
                    &router,
                    &runtime,
                    Some("cognito-idp"),
                    &format!("{idp}.CreateUserPool"),
                    &json!({
                        "PoolName": "xsvc-web-identity-tag-users",
                        "Schema": [{
                            "Name": TENANT_TAG,
                            "AttributeDataType": "String",
                            "Mutable": true,
                        }],
                    }),
                );
                assert_eq!(
                    created.status,
                    200,
                    "setup: CreateUserPool must succeed, got {} with {:?}",
                    created.status,
                    created.text()
                );
                created.json()["UserPool"]["Id"]
                    .as_str()
                    .unwrap_or_else(|| panic!("setup: CreateUserPool reports the pool's Id"))
                    .to_string()
            };
            let client_id = {
                let created = rpc(
                    &router,
                    &runtime,
                    Some("cognito-idp"),
                    &format!("{idp}.CreateUserPoolClient"),
                    &json!({
                        "UserPoolId": &user_pool_id,
                        "ClientName": "xsvc-web-identity-tag-app",
                        "ExplicitAuthFlows": ["ALLOW_USER_PASSWORD_AUTH"],
                    }),
                );
                assert_eq!(
                    created.status,
                    200,
                    "setup: CreateUserPoolClient must succeed, got {} with {:?}",
                    created.status,
                    created.text()
                );
                created.json()["UserPoolClient"]["ClientId"]
                    .as_str()
                    .unwrap_or_else(|| panic!("setup: CreateUserPoolClient reports a ClientId"))
                    .to_string()
            };
            let provider_name = format!("cognito-idp.{REGION}.amazonaws.com/{user_pool_id}");

            // One user per tenant, each carrying the tenant in the pool's custom attribute.
            let password = "Passw0rd!2026";
            for tenant in [TENANT_A, TENANT_B] {
                for (operation, request) in [
                    (
                        "AdminCreateUser",
                        json!({
                            "UserPoolId": &user_pool_id,
                            "Username": format!("user-of-{tenant}"),
                            "UserAttributes": [{
                                "Name": format!("custom:{TENANT_TAG}"),
                                "Value": tenant,
                            }],
                        }),
                    ),
                    (
                        "AdminSetUserPassword",
                        json!({
                            "UserPoolId": &user_pool_id,
                            "Username": format!("user-of-{tenant}"),
                            "Password": password,
                            "Permanent": true,
                        }),
                    ),
                ] {
                    let answered = rpc(
                        &router,
                        &runtime,
                        Some("cognito-idp"),
                        &format!("{idp}.{operation}"),
                        &request,
                    );
                    assert_eq!(
                        answered.status,
                        200,
                        "setup: {operation} for {tenant:?} must succeed, got {} with {:?}",
                        answered.status,
                        answered.text()
                    );
                }
            }

            // The identity pool in front of that user pool, its principal-tag attribute map,
            // and a role of its own carrying the same ABAC document.
            let pool = rpc(
                &router,
                &runtime,
                Some("cognito-identity"),
                "AWSCognitoIdentityService.CreateIdentityPool",
                &json!({
                    "IdentityPoolName": "xsvc-web-identity-mapped-tags",
                    "AllowUnauthenticatedIdentities": false,
                    "CognitoIdentityProviders": [
                        {"ProviderName": &provider_name, "ClientId": &client_id},
                    ],
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
            let mapped = rpc(
                &router,
                &runtime,
                Some("cognito-identity"),
                "AWSCognitoIdentityService.SetPrincipalTagAttributeMap",
                &json!({
                    "IdentityPoolId": &pool_id,
                    "IdentityProviderName": &provider_name,
                    "UseDefaults": false,
                    "PrincipalTags": {TENANT_TAG: format!("custom:{TENANT_TAG}")},
                }),
            );
            assert_eq!(
                mapped.status,
                200,
                "setup: SetPrincipalTagAttributeMap must succeed, got {} with {:?}",
                mapped.status,
                mapped.text()
            );
            let role_arn = create_role(
                &router,
                &runtime,
                "xsvc-web-identity-mapped-reader",
                &trust_policy(&pool_id, true),
            );

            // One row: the case, the user signing in, and the tenant whose prefix the session
            // its token mints may read.
            let rows: Vec<(&str, &str, &str)> = vec![
                ("the user of the first tenant", TENANT_A, TENANT_A),
                ("the user of the other tenant", TENANT_B, TENANT_B),
            ];

            for (case, tenant, readable) in rows {
                let signed_in = rpc(
                    &router,
                    &runtime,
                    None,
                    &format!("{idp}.InitiateAuth"),
                    &json!({
                        "ClientId": &client_id,
                        "AuthFlow": "USER_PASSWORD_AUTH",
                        "AuthParameters": {
                            "USERNAME": format!("user-of-{tenant}"),
                            "PASSWORD": password,
                        },
                    }),
                );
                assert_eq!(
                    signed_in.status,
                    200,
                    "case {case:?}: the sign-in must be served, got {} with {:?}",
                    signed_in.status,
                    signed_in.text()
                );
                let id_token = signed_in.json()["AuthenticationResult"]["IdToken"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: the sign-in must issue an IdToken")
                    })
                    .to_string();
                let logins = json!({&provider_name: &id_token});

                let identity = rpc(
                    &router,
                    &runtime,
                    None,
                    "AWSCognitoIdentityService.GetId",
                    &json!({"IdentityPoolId": &pool_id, "Logins": &logins}),
                );
                assert_eq!(
                    identity.status,
                    200,
                    "case {case:?}: GetId must be served, got {} with {:?}",
                    identity.status,
                    identity.text()
                );
                let identity_id = identity.json()["IdentityId"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: GetId reports an IdentityId"))
                    .to_string();
                let issued = rpc(
                    &router,
                    &runtime,
                    None,
                    "AWSCognitoIdentityService.GetOpenIdToken",
                    &json!({"IdentityId": &identity_id, "Logins": &logins}),
                );
                assert_eq!(
                    issued.status,
                    200,
                    "case {case:?}: GetOpenIdToken must be served, got {} with {:?}",
                    issued.status,
                    issued.text()
                );
                let token = issued.json()["Token"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: GetOpenIdToken reports a Token"))
                    .to_string();

                let session_name = format!("mapped-{tenant}");
                let exchanged = assume_with_web_identity(
                    &router,
                    &runtime,
                    &role_arn,
                    &session_name,
                    &token,
                );
                assert_eq!(
                    exchanged.status,
                    200,
                    "case {case:?}: the exchange must be served, got {} with {:?}",
                    exchanged.status,
                    exchanged.text()
                );
                let access_key_id = exchanged
                    .element("AccessKeyId")
                    .unwrap_or_else(|| panic!("case {case:?}: the exchange reports an AccessKeyId"));

                for key_tenant in [TENANT_A, TENANT_B, UNOWNED] {
                    let key = object_key(key_tenant);
                    let case = format!("{case} reading {key}");
                    let answer =
                        s3(&router, &runtime, &access_key_id, "GET", &format!("/{BUCKET}/{key}"), "");
                    if key_tenant == readable {
                        assert_eq!(
                            (answer.status, answer.text()),
                            (200, object_body(key_tenant)),
                            "case {case:?}: the read must be served the object, got {} with {:?}",
                            answer.status,
                            answer.text()
                        );
                    } else {
                        assert_eq!(
                            answer.status,
                            403,
                            "case {case:?}: the read must be refused 403, got {} with {:?}",
                            answer.status,
                            answer.text()
                        );
                    }
                }
            }
        })
        .expect("spawn the web-identity-mapped-attribute-tags thread")
        .join()
        .expect("a pool's mapped user attribute must become the session's tag");
}

/// The tag key an identity-based CONDITION is written over, and the two values the rows span.
const DEPARTMENT_ADMITTED: &str = "engineering";
const DEPARTMENT_REFUSED: &str = "finance";

/// The key the condition-gated rows read, at the bucket's root: what the tag decides there is
/// the CONDITION, not the resource the statement names.
const GUARDED_KEY: &str = "secret.txt";

/// An identity-based document granting the read of every object of the bucket, but only to a
/// session whose `department` tag names [`DEPARTMENT_ADMITTED`] — the condition-key form of an
/// ABAC policy, beside [`tenant_scoped_document`]'s policy-variable form.
fn department_guarded_document() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": GET_OBJECT_ACTION,
            "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
            "Condition": {"StringEquals": {
                format!("aws:PrincipalTag/{DEPARTMENT_TAG}"): DEPARTMENT_ADMITTED,
            }},
        }],
    })
    .to_string()
}

/// An identity-based document granting nothing about [`BUCKET`]: the position a role is in
/// when the only grant that can reach it is the BUCKET's own policy.
fn grants_another_bucket_document() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": GET_OBJECT_ACTION,
            "Resource": format!("arn:aws:s3:::{BUCKET}-elsewhere/*"),
        }],
    })
    .to_string()
}

/// The BUCKET's own policy: it grants the read to the sessions of `role_arn`, under the same
/// condition on the session's `department` tag.
fn department_guarded_bucket_policy(role_arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"AWS": role_arn},
            "Action": GET_OBJECT_ACTION,
            "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
            "Condition": {"StringEquals": {
                format!("aws:PrincipalTag/{DEPARTMENT_TAG}"): DEPARTMENT_ADMITTED,
            }},
        }],
    })
    .to_string()
}

/// The same rule read along the CONDITION-KEY axis, in both documents that can grant a read:
/// a statement conditioned on `aws:PrincipalTag/<key>` is decided by the tag the exchanged
/// token carried, whether that statement lives in the role's own policy or in the bucket's.
#[test]
fn a_condition_on_the_tokens_tag_decides_both_documents_that_can_grant() {
    std::thread::Builder::new()
        .name("web-identity-tag-conditioned-grants".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");
            let estate = provision(&router, &runtime);

            // The one object the condition-gated rows read, at the bucket's root.
            let written = s3(
                &router,
                &runtime,
                SETUP_KEY_ID,
                "PUT",
                &format!("/{BUCKET}/{GUARDED_KEY}"),
                "the guarded object",
            );
            assert_eq!(
                written.status,
                200,
                "setup: PutObject({GUARDED_KEY:?}) must succeed, got {} with {:?}",
                written.status,
                written.text()
            );

            // Two roles of the same pool: one whose OWN document carries the conditioned
            // grant, and one that grants nothing here, for which only the bucket's policy can.
            let identity_granted = create_role_with(
                &router,
                &runtime,
                "xsvc-web-identity-department-reader",
                &trust_policy(&estate.pool_id, true),
                &department_guarded_document(),
            );
            let bucket_granted = create_role_with(
                &router,
                &runtime,
                "xsvc-web-identity-bucket-policy-reader",
                &trust_policy(&estate.pool_id, true),
                &grants_another_bucket_document(),
            );
            let policy = s3(
                &router,
                &runtime,
                SETUP_KEY_ID,
                "PUT",
                &format!("/{BUCKET}?policy"),
                &department_guarded_bucket_policy(&bucket_granted),
            );
            assert_eq!(
                policy.status,
                204,
                "setup: PutBucketPolicy must succeed, got {} with {:?}",
                policy.status,
                policy.text()
            );

            // One row: the case, the tags the token carries, which role is assumed, and
            // whether the read of the guarded key is served.
            let rows: Vec<(&str, Option<Value>, &String, bool)> = vec![
                (
                    "the admitted tag value, granted by the role's own document",
                    Some(json!({DEPARTMENT_TAG: DEPARTMENT_ADMITTED})),
                    &identity_granted,
                    true,
                ),
                (
                    "the admitted value beside another tag",
                    Some(json!({DEPARTMENT_TAG: DEPARTMENT_ADMITTED, TENANT_TAG: TENANT_A})),
                    &identity_granted,
                    true,
                ),
                (
                    "the other tag value",
                    Some(json!({DEPARTMENT_TAG: DEPARTMENT_REFUSED})),
                    &identity_granted,
                    false,
                ),
                (
                    "no tag at all",
                    None,
                    &identity_granted,
                    false,
                ),
                (
                    "a tag under another key",
                    Some(json!({TENANT_TAG: TENANT_A})),
                    &identity_granted,
                    false,
                ),
                (
                    "the admitted tag value, granted by the bucket's own policy",
                    Some(json!({DEPARTMENT_TAG: DEPARTMENT_ADMITTED})),
                    &bucket_granted,
                    true,
                ),
                (
                    "the other tag value against the bucket's policy",
                    Some(json!({DEPARTMENT_TAG: DEPARTMENT_REFUSED})),
                    &bucket_granted,
                    false,
                ),
            ];

            for (index, (case, principal_tags, role_arn, served)) in
                rows.into_iter().enumerate()
            {
                let developer_user = format!("guarded-user-{index}");
                let token = developer_token(
                    &router,
                    &runtime,
                    &estate.pool_id,
                    case,
                    &developer_user,
                    principal_tags,
                );
                let exchanged = assume_with_web_identity(
                    &router,
                    &runtime,
                    role_arn,
                    &format!("guarded-{index}"),
                    &token,
                );
                assert_eq!(
                    exchanged.status,
                    200,
                    "case {case:?}: the exchange must be served, got {} with {:?}",
                    exchanged.status,
                    exchanged.text()
                );
                let access_key_id = exchanged
                    .element("AccessKeyId")
                    .unwrap_or_else(|| panic!("case {case:?}: the exchange reports an AccessKeyId"));
                let answer = s3(
                    &router,
                    &runtime,
                    &access_key_id,
                    "GET",
                    &format!("/{BUCKET}/{GUARDED_KEY}"),
                    "",
                );
                if served {
                    assert_eq!(
                        (answer.status, answer.text()),
                        (200, "the guarded object".to_string()),
                        "case {case:?}: the read must be served the object, got {} with {:?}",
                        answer.status,
                        answer.text()
                    );
                } else {
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
                        stated.contains(&object_arn(GUARDED_KEY)),
                        "case {case:?}: the refusal must name the object {:?}, got {stated:?}",
                        object_arn(GUARDED_KEY)
                    );
                }
            }
        })
        .expect("spawn the web-identity-tag-conditioned-grants thread")
        .join()
        .expect("a condition on the token's tag must decide both documents that can grant");
}
