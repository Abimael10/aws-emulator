// SPDX-License-Identifier: CC0-1.0

//! Which principal-tag attribute map a `GetCredentialsForIdentity` vend builds its session
//! tags from.
//!
//! RULE: the principal-tag attribute map of an `(IdentityPoolId, IdentityProviderName)` pair
//! is REPLACED by every `SetPrincipalTagAttributeMap` — the call states that pair's whole
//! mapping and reports it back, as `GetPrincipalTagAttributeMap` then does
//! (`smithy_query(query="operation",operation="SetPrincipalTagAttributeMap,GetPrincipalTagAttributeMap")`)
//! — so the map a vend builds its session tags from is the one in force AT THAT VEND, the very
//! map `GetPrincipalTagAttributeMap` reports: a session minted after a replacement carries a
//! tag for every key the NEW map names, taken from the presented token's mapped claim, and
//! carries none for a key the replacement dropped. It holds whoever made the call, so a
//! mapping changed IN PLACE by a CloudFormation update of
//! `AWS::Cognito::IdentityPoolPrincipalTag` — the template form of that same call — governs the
//! next vend exactly as a direct call does.
//!
//! What follows for an ABAC design: an identity-based policy whose resource ARNs carry
//! `${aws:PrincipalTag/<key>}` grants a freshly vended session exactly the prefix the CURRENT
//! map's key resolves to, and nothing at the prefixes of the keys it no longer maps
//! ("`aws:PrincipalTag/tag-key` — the tag attached to the principal making the request", AWS
//! General Reference, *Global condition context keys*).
//!
//! Only a cross-service suite can pin this: the user pool and its signing-in users are Amazon
//! Cognito Identity Provider's, the identity pool and its attribute map Amazon Cognito
//! Identity's, the role and its two documents IAM's, the vended session STS's, the deployment
//! and its in-place update CloudFormation's, and the read is decided by Amazon S3 — six crates
//! over ONE engine, as `core/router/src/lib.rs` mounts them.
//!
//! The rows differ along what the rule generalises over: WHICH tag key the map in force names,
//! how many keys it names (one, two), whether the mapping is a custom one or the DEFAULT
//! (`UseDefaults`) one — whose keys are the documented `client`/`username` pair, taken from the
//! token's `aud` and `sub` claims — who replaced it (a direct call, a CloudFormation in-place
//! update of the declaring resource) and which authentication flow minted the token the mapped
//! claim is read out of (the public one, the admin one).
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on a sized thread.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_cognito_federation_estate::Wire;

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call below is signed for, and so the Region the pools live in.
const REGION: &str = "us-east-1";

/// The emulator's own address, for the unsigned legs: an address, so the `Host` header names
/// no service.
const EMULATOR_HOST: &str = "127.0.0.1:9297";

/// The access key id every signed SETUP call carries — a key no session and no IAM user holds,
/// which speaks for the account itself.
const SETUP_KEY_ID: &str = "AKIDTEST";

/// The awsQuery API version of IAM, the one query service this suite calls directly.
const IAM_API_VERSION: &str = "2010-05-08";

/// The service shapes the two Amazon Cognito models bind their `X-Amz-Target` to.
const IDENTITY_TARGET: &str = "AWSCognitoIdentityService";
const IDP_TARGET: &str = "AWSCognitoIdentityProviderService";

/// The issuer of an identity pool's own tokens, and the `Federated` principal a role's trust
/// policy names to admit one.
const COGNITO_IDENTITY_PROVIDER: &str = "cognito-identity.amazonaws.com";

/// The user-pool attribute every custom mapping below maps onto a session tag, and the three
/// tag keys the ABAC document is written over: the key the first map names, the key a
/// replacement re-points the same attribute at, and the `username` key of Amazon Cognito's
/// DEFAULT mappings (`client` from `aud`, `username` from `sub`, which is what
/// `UseDefaults` configures — `DEFAULT_PRINCIPAL_TAGS` in the owning crate).
const TENANT_ATTRIBUTE: &str = "custom:tenant";
const TENANT_TAG: &str = "tenant";
const ORG_TAG: &str = "org";
const USERNAME_TAG: &str = "username";

/// The name the ABAC document is written under on every role below.
const POLICY_NAME: &str = "tag-scoped-read";

/// The IAM actions this suite is about.
const GET_OBJECT_ACTION: &str = "s3:GetObject";
const TAG_SESSION_ACTION: &str = "sts:TagSession";
const ASSUME_ACTION: &str = "sts:AssumeRoleWithWebIdentity";

/// The code and status S3 refuses an unauthorized read with.
const ACCESS_DENIED: &str = "AccessDenied";

/// The two tenants the rows span.
const TENANT_A: &str = "tenant-a";
const TENANT_B: &str = "tenant-b";

/// The password every user below signs in with.
const PASSWORD: &str = "Passw0rd!2026";

/// Which authentication flow a row's token is minted by: the PUBLIC one a client app sends
/// unsigned (`InitiateAuth`, `USER_PASSWORD_AUTH`), and the ADMIN one a backend sends with its
/// own credential (`AdminInitiateAuth`, `ADMIN_USER_PASSWORD_AUTH`). Both answer an ID token
/// for the same user, so the mapped claim is read out of either.
#[derive(Clone, Copy, Debug)]
enum Flow {
    Public,
    Admin,
}

impl Flow {
    /// The `ExplicitAuthFlows` entries an app client needs for both flows, which is what every
    /// client below declares.
    const CLIENT_FLOWS: [&'static str; 2] = ["ALLOW_USER_PASSWORD_AUTH", "ALLOW_ADMIN_USER_PASSWORD_AUTH"];
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

    /// One element's text with the serializer's escaping undone — the suite's one reader of an
    /// XML answer ([`crate::query_wire::elements`]).
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
        body.push_str(&crate::query_wire::form_encode(name));
        body.push('=');
        body.push_str(&crate::query_wire::form_encode(value));
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

/// The trust policy of a role an identity pool vends sessions of: the `Federated` Cognito
/// Identity principal, the web-identity assume, the `sts:TagSession` permission a TAGGED
/// assume needs beside it, and the `:aud` condition restricting the assume to the users of the
/// pool `audience` resolves to.
fn pool_trust_policy(audience: Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Federated": COGNITO_IDENTITY_PROVIDER},
            "Action": [ASSUME_ACTION, TAG_SESSION_ACTION],
            "Condition": {"StringEquals": {
                format!("{COGNITO_IDENTITY_PROVIDER}:aud"): audience,
            }},
        }],
    })
}

/// The ONE ABAC document in force on the role of every phase: one statement naming no tenant,
/// whose three resource ARNs each carry a different principal-tag variable — so what it grants
/// a session is decided by WHICH tags that session carries, and the document never changes
/// while the phases replace the pool's mapping.
fn tag_scoped_document(bucket: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": GET_OBJECT_ACTION,
            "Resource": [
                prefix_resource(bucket, TENANT_TAG),
                prefix_resource(bucket, ORG_TAG),
                prefix_resource(bucket, USERNAME_TAG),
            ],
        }],
    })
}

/// The resource ARN of the prefix one tag key owns: `<tag key>/${aws:PrincipalTag/<tag key>}/*`.
fn prefix_resource(bucket: &str, tag_key: &str) -> String {
    format!("arn:aws:s3:::{bucket}/{tag_key}/${{aws:PrincipalTag/{tag_key}}}/*")
}

/// The key one object sits at: the prefix a tag key owns, then the value that tag carries.
fn object_key(prefix: &str, value: &str) -> String {
    format!("{prefix}/{value}/obj")
}

/// The bytes one object holds.
fn object_body(key: &str) -> String {
    format!("the object at {key}")
}

/// One signing-in user of a user pool: which tenant its mapped attribute names, the name it
/// signs in under, and the `sub` claim its tokens carry — which the DEFAULT mapping's
/// `username` tag is taken from.
struct User {
    tenant: String,
    username: String,
    sub: String,
}

/// Create one user per tenant in `user_pool_id`, each carrying its tenant in the pool's
/// developer-defined attribute and a permanent password, and read back the `sub` the pool
/// minted for it.
fn create_users(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    user_pool_id: &str,
) -> Vec<User> {
    let mut users = Vec::new();
    for tenant in [TENANT_A, TENANT_B] {
        let username = format!("user-of-{tenant}");
        for (operation, request) in [
            (
                "AdminCreateUser",
                json!({
                    "UserPoolId": user_pool_id,
                    "Username": &username,
                    "UserAttributes": [{"Name": TENANT_ATTRIBUTE, "Value": tenant}],
                }),
            ),
            (
                "AdminSetUserPassword",
                json!({
                    "UserPoolId": user_pool_id,
                    "Username": &username,
                    "Password": PASSWORD,
                    "Permanent": true,
                }),
            ),
        ] {
            let answered = rpc(
                router,
                runtime,
                Some("cognito-idp"),
                &format!("{IDP_TARGET}.{operation}"),
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
        let read = rpc(
            router,
            runtime,
            Some("cognito-idp"),
            &format!("{IDP_TARGET}.AdminGetUser"),
            &json!({"UserPoolId": user_pool_id, "Username": &username}),
        );
        assert_eq!(
            read.status,
            200,
            "setup: AdminGetUser for {tenant:?} must succeed, got {} with {:?}",
            read.status,
            read.text()
        );
        let answered = read.json();
        let sub = answered["UserAttributes"]
            .as_array()
            .unwrap_or_else(|| {
                panic!("setup: AdminGetUser answers {tenant:?}'s attributes: {answered}")
            })
            .iter()
            .find(|attribute| attribute.get("Name") == Some(&json!("sub")))
            .and_then(|attribute| attribute.get("Value"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("setup: AdminGetUser answers {tenant:?}'s sub: {answered}"))
            .to_string();
        users.push(User {
            tenant: tenant.to_string(),
            username,
            sub,
        });
    }
    users
}

/// The bucket and one object per prefix the ABAC document names, for every value a tag could
/// carry: the two tenants under the two tenant-valued prefixes, and each user's `sub` under
/// the prefix the default mapping's key owns. Answers every key written.
fn write_objects(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    bucket: &str,
    users: &[User],
    create_bucket: bool,
) -> Vec<String> {
    if create_bucket {
        let created = s3(router, runtime, SETUP_KEY_ID, "PUT", &format!("/{bucket}"), "");
        assert_eq!(
            created.status,
            200,
            "setup: CreateBucket({bucket:?}) must succeed, got {} with {:?}",
            created.status,
            created.text()
        );
    }
    let mut keys = Vec::new();
    for prefix in [TENANT_TAG, ORG_TAG] {
        for tenant in [TENANT_A, TENANT_B] {
            keys.push(object_key(prefix, tenant));
        }
    }
    for user in users {
        keys.push(object_key(USERNAME_TAG, &user.sub));
    }
    for key in &keys {
        let put = s3(
            router,
            runtime,
            SETUP_KEY_ID,
            "PUT",
            &format!("/{bucket}/{key}"),
            &object_body(key),
        );
        assert_eq!(
            put.status,
            200,
            "setup: PutObject({key:?}) must succeed, got {} with {:?}",
            put.status,
            put.text()
        );
    }
    keys
}

/// The federated estate one vend is made against: which authentication flow mints the token,
/// the user pool and app client that signs the user in, the identity-provider name its tokens
/// are presented under, and the identity pool the vend is made on.
struct Estate {
    flow: Flow,
    user_pool_id: String,
    client_id: String,
    provider_name: String,
    pool_id: String,
}

/// Sign `user` in and vend credentials for its identity of the estate's pool: a FRESH sign-in
/// and a FRESH vend, so what the session carries is decided by the map in force at THIS vend.
/// Answers the access key id the vend handed back.
fn vended_access_key(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    estate: &Estate,
    user: &User,
    case: &str,
) -> String {
    let Estate {
        flow,
        user_pool_id,
        client_id,
        provider_name,
        pool_id,
    } = estate;
    let (operation, signing, request) = match flow {
        Flow::Public => (
            "InitiateAuth",
            None,
            json!({
                "ClientId": client_id,
                "AuthFlow": "USER_PASSWORD_AUTH",
                "AuthParameters": {"USERNAME": &user.username, "PASSWORD": PASSWORD},
            }),
        ),
        Flow::Admin => (
            "AdminInitiateAuth",
            Some("cognito-idp"),
            json!({
                "UserPoolId": user_pool_id,
                "ClientId": client_id,
                "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
                "AuthParameters": {"USERNAME": &user.username, "PASSWORD": PASSWORD},
            }),
        ),
    };
    let signed_in = rpc(
        router,
        runtime,
        signing,
        &format!("{IDP_TARGET}.{operation}"),
        &request,
    );
    assert_eq!(
        signed_in.status,
        200,
        "{case}: the sign-in ({operation}) must be served, got {} with {:?}",
        signed_in.status,
        signed_in.text()
    );
    let id_token = signed_in.json()["AuthenticationResult"]["IdToken"]
        .as_str()
        .unwrap_or_else(|| panic!("{case}: the sign-in issues an IdToken"))
        .to_string();
    let logins = json!({provider_name: &id_token});

    let identity = rpc(
        router,
        runtime,
        None,
        &format!("{IDENTITY_TARGET}.GetId"),
        &json!({"IdentityPoolId": pool_id, "Logins": &logins}),
    );
    assert_eq!(
        identity.status,
        200,
        "{case}: GetId must be served, got {} with {:?}",
        identity.status,
        identity.text()
    );
    let identity_id = identity.json()["IdentityId"]
        .as_str()
        .unwrap_or_else(|| panic!("{case}: GetId reports an IdentityId"))
        .to_string();
    let vended = rpc(
        router,
        runtime,
        None,
        &format!("{IDENTITY_TARGET}.GetCredentialsForIdentity"),
        &json!({"IdentityId": &identity_id, "Logins": &logins}),
    );
    assert_eq!(
        vended.status,
        200,
        "{case}: the vend must be served, got {} with {:?}",
        vended.status,
        vended.text()
    );
    let access_key_id = vended.json()["Credentials"]["AccessKeyId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !access_key_id.is_empty(),
        "{case}: the vend must carry an AccessKeyId, got {:?}",
        vended.text()
    );
    access_key_id
}

/// What the mapping in force makes readable for `user`: one key per tag key the map names,
/// at the prefix that key owns and under the value this user's mapped claim carries — the
/// user's tenant for an attribute-mapped key, its `sub` for the default mapping's `username`.
fn readable_keys(prefixes: &[&str], user: &User) -> Vec<String> {
    prefixes
        .iter()
        .map(|prefix| {
            let value = if *prefix == USERNAME_TAG {
                &user.sub
            } else {
                &user.tenant
            };
            object_key(prefix, value)
        })
        .collect()
}

/// Read every key of `keys` under the vended credential: the ones the session's tags resolve
/// the document's variables to are served the object's bytes, and every other key — another
/// tenant's, and the prefixes of the keys the map no longer names — is refused 403
/// `AccessDenied`.
fn assert_reads(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    access_key_id: &str,
    bucket: &str,
    keys: &[String],
    served: &[String],
    case: &str,
) {
    for key in keys {
        let label = format!("{case} reading {key}");
        let answer = s3(
            router,
            runtime,
            access_key_id,
            "GET",
            &format!("/{bucket}/{key}"),
            "",
        );
        if served.contains(key) {
            assert_eq!(
                (answer.status, answer.text()),
                (200, object_body(key)),
                "{label}: the read must be served the object, got {} with {:?}",
                answer.status,
                answer.text()
            );
        } else {
            assert_eq!(
                answer.status,
                403,
                "{label}: the read must be refused 403, got {} with {:?}",
                answer.status,
                answer.text()
            );
            assert_eq!(
                answer.element("Code").as_deref(),
                Some(ACCESS_DENIED),
                "{label}: expected the code {ACCESS_DENIED}, got {:?}",
                answer.text()
            );
        }
    }
}

/// `GetPrincipalTagAttributeMap` for one pair, which must report the map the call that wrote
/// it reported: the map every vend after it builds its session tags from.
fn assert_mapping_in_force(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    pool_id: &str,
    provider_name: &str,
    expected_tags: &Value,
    expected_use_defaults: bool,
    case: &str,
) {
    let reported = rpc(
        router,
        runtime,
        Some("cognito-identity"),
        &format!("{IDENTITY_TARGET}.GetPrincipalTagAttributeMap"),
        &json!({"IdentityPoolId": pool_id, "IdentityProviderName": provider_name}),
    );
    assert_eq!(
        reported.status,
        200,
        "{case}: GetPrincipalTagAttributeMap must be served, got {} with {:?}",
        reported.status,
        reported.text()
    );
    let answered = reported.json();
    assert_eq!(
        &answered["PrincipalTags"], expected_tags,
        "{case}: the map in force must be the one just written, got {:?}",
        reported.text()
    );
    assert_eq!(
        answered["UseDefaults"],
        json!(expected_use_defaults),
        "{case}: UseDefaults must be {expected_use_defaults}, got {:?}",
        reported.text()
    );
}

/// RULE: every vend builds its session tags from the map in force at that vend, so replacing
/// the map re-points what the ABAC document grants a freshly vended session.
#[test]
fn a_vend_builds_its_session_tags_from_the_map_in_force_at_that_vend() {
    std::thread::Builder::new()
        .name("cognito-live-principal-tag-map".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            const BUCKET: &str = "xsvc-cognito-live-principal-tag-map";
            const ROLE_NAME: &str = "xsvc-cognito-live-map-reader";

            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");

            // The user pool, its app client and one user per tenant.
            let created = rpc(
                &router,
                &runtime,
                Some("cognito-idp"),
                &format!("{IDP_TARGET}.CreateUserPool"),
                &json!({
                    "PoolName": "xsvc-live-map-users",
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
            let user_pool_id = created.json()["UserPool"]["Id"]
                .as_str()
                .unwrap_or_else(|| panic!("setup: CreateUserPool reports the pool's Id"))
                .to_string();
            let client = rpc(
                &router,
                &runtime,
                Some("cognito-idp"),
                &format!("{IDP_TARGET}.CreateUserPoolClient"),
                &json!({
                    "UserPoolId": &user_pool_id,
                    "ClientName": "xsvc-live-map-app",
                    "ExplicitAuthFlows": Flow::CLIENT_FLOWS,
                }),
            );
            assert_eq!(
                client.status,
                200,
                "setup: CreateUserPoolClient must succeed, got {} with {:?}",
                client.status,
                client.text()
            );
            let client_id = client.json()["UserPoolClient"]["ClientId"]
                .as_str()
                .unwrap_or_else(|| panic!("setup: CreateUserPoolClient reports a ClientId"))
                .to_string();
            let provider_name = format!("cognito-idp.{REGION}.amazonaws.com/{user_pool_id}");
            let users = create_users(&router, &runtime, &user_pool_id);

            // The identity pool in front of that user pool.
            let pool = rpc(
                &router,
                &runtime,
                Some("cognito-identity"),
                &format!("{IDENTITY_TARGET}.CreateIdentityPool"),
                &json!({
                    "IdentityPoolName": "xsvc-live-principal-tag-map",
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

            // The role, its trust policy and the one ABAC document in force on it.
            let role = iam(
                &router,
                &runtime,
                "CreateRole",
                &[
                    ("RoleName", ROLE_NAME),
                    (
                        "AssumeRolePolicyDocument",
                        &pool_trust_policy(json!(&pool_id)).to_string(),
                    ),
                ],
            );
            assert_eq!(
                role.status,
                200,
                "setup: CreateRole must succeed, got {} with {:?}",
                role.status,
                role.text()
            );
            let role_arn = role
                .element("Arn")
                .unwrap_or_else(|| panic!("setup: CreateRole reports an Arn"));
            let written = iam(
                &router,
                &runtime,
                "PutRolePolicy",
                &[
                    ("RoleName", ROLE_NAME),
                    ("PolicyName", POLICY_NAME),
                    ("PolicyDocument", &tag_scoped_document(BUCKET).to_string()),
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
                Some("cognito-identity"),
                &format!("{IDENTITY_TARGET}.SetIdentityPoolRoles"),
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

            let keys = write_objects(&router, &runtime, BUCKET, &users, true);
            let estate = Estate {
                flow: Flow::Public,
                user_pool_id: user_pool_id.clone(),
                client_id: client_id.clone(),
                provider_name: provider_name.clone(),
                pool_id: pool_id.clone(),
            };

            // One row: the case, the mapping members to put in force, and the prefixes a
            // session of each user may then read — every other key must be refused.
            let rows: Vec<(&str, Value, Vec<&str>)> = vec![
                (
                    "the first custom mapping, onto the tenant tag",
                    json!({"UseDefaults": false, "PrincipalTags": {TENANT_TAG: TENANT_ATTRIBUTE}}),
                    vec![TENANT_TAG],
                ),
                (
                    "a replacement re-pointing the same attribute at another tag key",
                    json!({"UseDefaults": false, "PrincipalTags": {ORG_TAG: TENANT_ATTRIBUTE}}),
                    vec![ORG_TAG],
                ),
                (
                    "a replacement mapping the attribute onto both keys at once",
                    json!({
                        "UseDefaults": false,
                        "PrincipalTags": {
                            TENANT_TAG: TENANT_ATTRIBUTE,
                            ORG_TAG: TENANT_ATTRIBUTE,
                        },
                    }),
                    vec![TENANT_TAG, ORG_TAG],
                ),
                (
                    "a replacement asking for the default mappings",
                    json!({"UseDefaults": true}),
                    vec![USERNAME_TAG],
                ),
            ];

            for (case, mapping, readable_prefixes) in rows {
                let mut request = json!({
                    "IdentityPoolId": &pool_id,
                    "IdentityProviderName": &provider_name,
                });
                for (member, value) in mapping.as_object().expect("the row states a mapping") {
                    request[member] = value.clone();
                }
                let set = rpc(
                    &router,
                    &runtime,
                    Some("cognito-identity"),
                    &format!("{IDENTITY_TARGET}.SetPrincipalTagAttributeMap"),
                    &request,
                );
                assert_eq!(
                    set.status,
                    200,
                    "case {case:?}: SetPrincipalTagAttributeMap must be served, got {} with {:?}",
                    set.status,
                    set.text()
                );
                let use_defaults = mapping["UseDefaults"] == json!(true);
                assert_mapping_in_force(
                    &router,
                    &runtime,
                    &pool_id,
                    &provider_name,
                    &set.json()["PrincipalTags"],
                    use_defaults,
                    &format!("case {case:?}"),
                );

                for user in &users {
                    let label = format!("case {case:?} / {}", user.tenant);
                    let access_key_id = vended_access_key(&router, &runtime, &estate, user, &label);
                    assert_reads(
                        &router,
                        &runtime,
                        &access_key_id,
                        BUCKET,
                        &keys,
                        &readable_keys(&readable_prefixes, user),
                        &label,
                    );
                }
            }
        })
        .expect("spawn the cognito-live-principal-tag-map thread")
        .join()
        .expect("a vend must build its session tags from the map in force at that vend");
}

/// The logical ids of the deployed estate the second test drives.
const STACK: &str = "xsvc-cognito-cfn-live-map";
const CFN_BUCKET: &str = "xsvc-cognito-cfn-live-map-objects";
const CFN_ROLE: &str = "xsvc-cognito-cfn-live-map-reader";
const USER_POOL_RESOURCE: &str = "UserPool";
const POOL_RESOURCE: &str = "IdentityPool";
const ROLE_RESOURCE: &str = "Reader";
const PRINCIPAL_TAG_RESOURCE: &str = "PrincipalTag";

/// The template the second test deploys and then updates in place: the whole federated estate
/// — the bucket the reads address, the user pool declaring the attribute the mapping reads, an
/// app client of it, the identity pool federating that client, the role carrying the ONE ABAC
/// document, the attachment that binds it, and the principal-tag mapping. `mapped_tag` is the
/// one property the in-place update changes.
fn estate_template(mapped_tag: &str) -> String {
    json!({
        "Resources": {
            "Objects": {
                "Type": "AWS::S3::Bucket",
                "Properties": {"BucketName": CFN_BUCKET},
            },
            USER_POOL_RESOURCE: {
                "Type": "AWS::Cognito::UserPool",
                "Properties": {
                    "UserPoolName": "xsvc-cfn-live-map-users",
                    "Schema": [{
                        "Name": TENANT_TAG,
                        "AttributeDataType": "String",
                        "Mutable": true,
                    }],
                },
            },
            "Client": {
                "Type": "AWS::Cognito::UserPoolClient",
                "Properties": {
                    "UserPoolId": {"Ref": USER_POOL_RESOURCE},
                    "ClientName": "xsvc-cfn-live-map-app",
                    "ExplicitAuthFlows": Flow::CLIENT_FLOWS,
                },
            },
            POOL_RESOURCE: {
                "Type": "AWS::Cognito::IdentityPool",
                "Properties": {
                    "AllowUnauthenticatedIdentities": false,
                    "CognitoIdentityProviders": [{
                        "ProviderName": {"Fn::GetAtt": [USER_POOL_RESOURCE, "ProviderName"]},
                        "ClientId": {"Ref": "Client"},
                    }],
                },
            },
            ROLE_RESOURCE: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": CFN_ROLE,
                    "AssumeRolePolicyDocument": pool_trust_policy(json!({"Ref": POOL_RESOURCE})),
                    "Policies": [{
                        "PolicyName": POLICY_NAME,
                        "PolicyDocument": tag_scoped_document(CFN_BUCKET),
                    }],
                },
            },
            "Attachment": {
                "Type": "AWS::Cognito::IdentityPoolRoleAttachment",
                "Properties": {
                    "IdentityPoolId": {"Ref": POOL_RESOURCE},
                    "Roles": {"authenticated": {"Fn::GetAtt": [ROLE_RESOURCE, "Arn"]}},
                },
            },
            PRINCIPAL_TAG_RESOURCE: {
                "Type": "AWS::Cognito::IdentityPoolPrincipalTag",
                "Properties": {
                    "IdentityPoolId": {"Ref": POOL_RESOURCE},
                    "IdentityProviderName": {
                        "Fn::GetAtt": [USER_POOL_RESOURCE, "ProviderName"],
                    },
                    "UseDefaults": false,
                    "PrincipalTags": {mapped_tag: TENANT_ATTRIBUTE},
                },
            },
        },
        "Outputs": {
            "UserPoolId": {"Value": {"Ref": USER_POOL_RESOURCE}},
            "ClientId": {"Value": {"Ref": "Client"}},
            "PoolId": {"Value": {"Ref": POOL_RESOURCE}},
            "ProviderName": {
                "Value": {"Fn::GetAtt": [USER_POOL_RESOURCE, "ProviderName"]},
            },
            "Mapping": {"Value": {"Ref": PRINCIPAL_TAG_RESOURCE}},
        },
    })
    .to_string()
}

/// One output of the deployed stack, named in the panic of a stack that published none.
fn output(outputs: &std::collections::BTreeMap<String, String>, key: &str, case: &str) -> String {
    outputs
        .get(key)
        .unwrap_or_else(|| panic!("{case}: the stack publishes the output {key:?}: {outputs:?}"))
        .clone()
}

/// RULE, through the template form of the same call: an in-place CloudFormation update of
/// `AWS::Cognito::IdentityPoolPrincipalTag` replaces the mapping the pool had, so the next
/// vend carries the NEW key's tag and no longer the one the update dropped — the identity, the
/// pool, the role and its ABAC document being untouched by the update.
#[test]
fn an_in_place_update_of_the_mapping_governs_the_next_vend() {
    std::thread::Builder::new()
        .name("cognito-cfn-live-principal-tag-map".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("a current-thread runtime");
            let wire = Wire {
                router: &router,
                runtime: &runtime,
            };

            // The estate, deployed with the mapping onto the first tag key.
            let case = "the deployed estate";
            let outputs = wire.deploy(REGION, STACK, &estate_template(TENANT_TAG), case);
            let user_pool_id = output(&outputs, "UserPoolId", case);
            let client_id = output(&outputs, "ClientId", case);
            let pool_id = output(&outputs, "PoolId", case);
            let provider_name = output(&outputs, "ProviderName", case);
            let mapping_id = output(&outputs, "Mapping", case);

            let users = create_users(&router, &runtime, &user_pool_id);
            // The bucket is the stack's, so only its objects are written here.
            let keys = write_objects(&router, &runtime, CFN_BUCKET, &users, false);
            let estate = Estate {
                flow: Flow::Admin,
                user_pool_id: user_pool_id.clone(),
                client_id: client_id.clone(),
                provider_name: provider_name.clone(),
                pool_id: pool_id.clone(),
            };

            // One phase: the case, the tag key the declaration maps the attribute onto, and
            // the prefix a session of each user may then read.
            let phases: [(&str, &str, &str); 3] = [
                ("the create's mapping", TENANT_TAG, TENANT_TAG),
                ("the in-place update's mapping", ORG_TAG, ORG_TAG),
                ("a second in-place update, back to the first key", TENANT_TAG, TENANT_TAG),
            ];
            for (phase, (case, mapped_tag, readable_prefix)) in phases.into_iter().enumerate() {
                if phase > 0 {
                    let updated =
                        wire.update_stack(REGION, STACK, &estate_template(mapped_tag), case);
                    // The update is IN PLACE: the estate the mapping belongs to is the one
                    // that was there, so nothing the vend resolves is a new resource.
                    for (key, expected) in [
                        ("PoolId", &pool_id),
                        ("ProviderName", &provider_name),
                        ("Mapping", &mapping_id),
                    ] {
                        assert_eq!(
                            &output(&updated, key, case),
                            expected,
                            "case {case:?}: the update must keep the estate's {key}"
                        );
                    }
                }
                assert_mapping_in_force(
                    &router,
                    &runtime,
                    &pool_id,
                    &provider_name,
                    &json!({mapped_tag: TENANT_ATTRIBUTE}),
                    false,
                    &format!("case {case:?}"),
                );

                for user in &users {
                    let label = format!("case {case:?} / {}", user.tenant);
                    // The ADMIN flow here, the public one in the sibling test: the mapped
                    // claim is read out of the token either flow minted.
                    let access_key_id = vended_access_key(&router, &runtime, &estate, user, &label);
                    assert_reads(
                        &router,
                        &runtime,
                        &access_key_id,
                        CFN_BUCKET,
                        &keys,
                        &readable_keys(&[readable_prefix], user),
                        &label,
                    );
                }
            }
        })
        .expect("spawn the cognito-cfn-live-principal-tag-map thread")
        .join()
        .expect("an in-place update of the mapping must govern the next vend");
}
