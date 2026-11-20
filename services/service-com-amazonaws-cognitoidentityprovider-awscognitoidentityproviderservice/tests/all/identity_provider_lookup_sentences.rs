// SPDX-License-Identifier: CC0-1.0

//! Which sentence an unresolved identity-provider lookup carries, driven through the
//! router exactly as an SDK drives it (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.<Op>`).
//!
//! The rules under test:
//!
//! * A `ProviderName` the named pool does not hold is refused with
//!   `ResourceNotFoundException`
//!   (`smithy_query(query="operation",operation="UpdateIdentityProvider")`), and WHICH
//!   sentence that refusal carries belongs to the operation answering it. The operation
//!   that MODIFIES a stored provider names the pool as the user pool it is, while every
//!   operation that RESOLVES a provider against a pool — the read, the delete, and the
//!   read by identifier — names the pool as the tenant it resolved in. Both sentences
//!   take their two variable parts, the name the request asked by and the pool it asked
//!   in, from the request, so the two forms stay distinct whatever the provider and the
//!   pool are and however the name came to be absent.
//! * A `UserPoolId` no create minted is a different lookup, and every one of those
//!   operations reports it the same way: the pool is resolved before the provider, so
//!   the provider the request named never reaches the sentence.
//!
//! The rows span what those rules generalise over — the form and length of the name
//! asked by, the pool asked in, how the name came to be absent (never created, created
//! in another pool, created here and deleted), the form of the identifier asked by, and
//! the operation asking. Every assertion names its case and reports the actual value,
//! and every table is followed by reads proving the refusals changed nothing.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

/// A request's SigV4 credential scope is what routes it to this service. The signature
/// is never checked here: the suite turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/cognito-idp/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// The endpoint every request of this suite arrives at.
const ENDPOINT_HOST: &str = "cognito-idp.us-east-1.amazonaws.com";

/// Ids satisfying `UserPoolIdType`'s pattern `^[\w-]+_[0-9a-zA-Z]+$` and length 1..55
/// (`smithy_query(query="shape",shape="UserPoolIdType")`) that no create minted, so a
/// lookup is what fails rather than a modelled constraint: two Regions, a numeric suffix
/// and an upper-case one.
const ABSENT_POOLS: [&str; 3] = [
    "us-east-1_absentpool",
    "eu-west-2_0000000000",
    "us-west-2_ZZZZZZZZZ",
];

/// One emulator with just this service mounted.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(
        service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice::EmulatedService::new(engine),
    );
    router.set_request_authentication(false);
    router
}

/// Call `operation` with `body` and return its status and JSON response body.
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", ENDPOINT_HOST)
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AWSCognitoIdentityProviderService.{operation}"),
        )
        .header("authorization", AUTHORIZATION)
        .body(Bytes::from(body.to_string()))
        .expect("the request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or_else(|error| {
            panic!(
                "{operation} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(response.body())
            )
        })
    };
    (status, payload)
}

/// Call `operation`, requiring success, and return its response body.
async fn ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, payload) = call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must succeed, got {status} with {payload}"
    );
    payload
}

/// Call `operation`, requiring a refused `ResourceNotFoundException`, and return the
/// sentence it carried.
async fn not_found(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> String {
    let (status, payload) = call(router, operation, body).await;
    assert_eq!(
        (status, error_code(&payload)),
        (400, "ResourceNotFoundException"),
        "case {case:?}: expected a 400 ResourceNotFoundException, got {status} with {payload}"
    );
    error_message(&payload).to_string()
}

/// The `__type` of an error body, which is the wire error code.
fn error_code(payload: &Value) -> &str {
    payload
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
}

/// The `message` of an error body, under either casing the protocol admits.
fn error_message(payload: &Value) -> &str {
    payload
        .get("message")
        .or_else(|| payload.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
}

/// Create a pool named `pool_name` and return its minted id.
async fn pool(router: &MultiServiceRouter, pool_name: &str) -> String {
    let created = ok(
        router,
        pool_name,
        "CreateUserPool",
        &json!({ "PoolName": pool_name }),
    )
    .await;
    created["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {pool_name:?}: a minted pool id, got {created}"))
        .to_string()
}

/// Store an OIDC provider named `provider_name` in `user_pool_id`, carrying
/// `identifiers` as its `IdpIdentifiers`.
async fn create_provider(
    router: &MultiServiceRouter,
    case: &str,
    user_pool_id: &str,
    provider_name: &str,
    identifiers: &[&str],
) {
    let created = ok(
        router,
        case,
        "CreateIdentityProvider",
        &json!({
            "UserPoolId": user_pool_id,
            "ProviderName": provider_name,
            "ProviderType": "OIDC",
            "ProviderDetails": {
                "attributes_request_method": "GET",
                "attributes_url": "https://auth.example.com/userInfo",
                "authorize_scopes": "openid profile email",
                "authorize_url": "https://auth.example.com/authorize",
                "client_id": "1example23456789",
                "client_secret": "provider-app-client-secret",
                "jwks_uri": "https://auth.example.com/.well-known/jwks.json",
                "oidc_issuer": "https://auth.example.com",
                "token_url": "https://auth.example.com/token",
            },
            "IdpIdentifiers": identifiers,
        }),
    )
    .await;
    assert_eq!(
        created["IdentityProvider"]["ProviderName"],
        json!(provider_name),
        "case {case:?}: the create must store the name it was given, got {created}"
    );
}

/// The `ProviderName`s `user_pool_id` holds, in the order the service served them.
async fn stored_names(router: &MultiServiceRouter, case: &str, user_pool_id: &str) -> Vec<String> {
    let listed = ok(
        router,
        case,
        "ListIdentityProviders",
        &json!({ "UserPoolId": user_pool_id, "MaxResults": 60 }),
    )
    .await;
    listed["Providers"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: Providers must be a list, got {listed}"))
        .iter()
        .map(|entry| {
            entry["ProviderName"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a listed provider is named, got {entry}"))
                .to_string()
        })
        .collect()
}

/// A name a request asks a provider by.
struct Asked {
    /// The `ProviderName` the request carries.
    name: String,
    /// Whether a create can also STORE it: `CreateIdentityProvider` admits a narrower
    /// form of the name than the lookup members do, so a name only a lookup admits is
    /// exercised on the ways that store nothing.
    creatable: bool,
}

/// Names spanning `ProviderNameType` — length 1..32, pattern
/// `^[\p{L}\p{M}\p{S}\p{N}\p{P}\p{Z}]+$`
/// (`smithy_query(query="shape",shape="ProviderNameType")`): the shortest the length
/// admits, a name of the form a create carries, one of the social provider names the
/// service reserves, a punctuation-heavy one, and one at the published maximum length 32.
fn asked_names() -> Vec<Asked> {
    [
        ("a", false),
        ("AuthExampleOIDC", true),
        ("SignInWithApple", true),
        ("Idp.2-x", true),
        ("Provider_with.every:char/", false),
    ]
    .into_iter()
    .map(|(name, creatable)| Asked {
        name: name.to_string(),
        creatable,
    })
    .chain(::std::iter::once(Asked {
        name: "P".repeat(32),
        creatable: true,
    }))
    .collect()
}

/// How the name a request asks by comes to be one the pool it names does not hold.
#[derive(Debug, Clone, Copy)]
enum Absent {
    /// No create ever stored the name, in any pool.
    NeverCreated,
    /// A live provider of ANOTHER pool: the name resolves, the pairing does not.
    InAnotherPool,
    /// Stored in this pool and then deleted, which is the race a client loses when a
    /// delete lands before its update.
    CreatedAndDeleted,
}

#[tokio::test]
async fn an_absent_provider_is_reported_with_the_sentence_its_own_operation_words() {
    let router = router();
    let asked_in = pool(&router, "idp-sentence-pool").await;
    let elsewhere = pool(&router, "idp-sentence-other-pool").await;
    // A provider the asked-in pool DOES hold, so no refusal below can be the pool
    // holding none.
    create_provider(&router, "a live provider", &asked_in, "LiveOIDC", &[]).await;

    for Asked { name, creatable } in asked_names() {
        for absent in [
            Absent::NeverCreated,
            Absent::InAnotherPool,
            Absent::CreatedAndDeleted,
        ] {
            // A way that has to store the name first is only open to a name a create
            // admits; the sentences are asserted for every name on the way that stores
            // nothing.
            if !creatable && !matches!(absent, Absent::NeverCreated) {
                continue;
            }
            let case = format!("{absent:?}: the name {name:?} asked for in {asked_in}");
            match absent {
                Absent::NeverCreated => {}
                Absent::InAnotherPool => {
                    create_provider(&router, &case, &elsewhere, &name, &[]).await;
                }
                Absent::CreatedAndDeleted => {
                    create_provider(&router, &case, &asked_in, &name, &[]).await;
                    ok(
                        &router,
                        &case,
                        "DeleteIdentityProvider",
                        &json!({ "UserPoolId": asked_in, "ProviderName": name }),
                    )
                    .await;
                }
            }

            // The operation that MODIFIES a stored provider names the pool as the user
            // pool it is.
            let written = not_found(
                &router,
                &case,
                "UpdateIdentityProvider",
                &json!({
                    "UserPoolId": asked_in,
                    "ProviderName": name,
                    "AttributeMapping": { "email": "email_final" },
                }),
            )
            .await;
            let expected_written =
                format!("Identity provider {name} in User Pool {asked_in} does not exist.");
            assert_eq!(
                written, expected_written,
                "case {case:?}: UpdateIdentityProvider must report {expected_written:?}, \
                 got {written:?}"
            );

            // Every operation that RESOLVES a provider against a pool names the pool as
            // the tenant it resolved in, and that sentence is not the one above.
            let expected_resolved =
                format!("Identity provider {name} for tenantId {asked_in} does not exist.");
            for operation in ["DescribeIdentityProvider", "DeleteIdentityProvider"] {
                let case = format!("{case} by {operation}");
                let resolved = not_found(
                    &router,
                    &case,
                    operation,
                    &json!({ "UserPoolId": asked_in, "ProviderName": name }),
                )
                .await;
                assert_eq!(
                    resolved, expected_resolved,
                    "case {case:?}: {operation} must report {expected_resolved:?}, \
                     got {resolved:?}"
                );
                assert_ne!(
                    resolved, written,
                    "case {case:?}: the resolve path and the write path must keep their \
                     own sentences, got {resolved:?} from both"
                );
            }
        }
    }

    // None of those refusals changed anything: the asked-in pool holds exactly the one
    // provider its create stored — no refused update minted a provider and no refused
    // delete removed one — while every name created in the other pool is still there.
    let held = stored_names(&router, "after the refusals", &asked_in).await;
    assert_eq!(
        held,
        vec!["LiveOIDC".to_string()],
        "the refusals must leave the pool holding only its live provider, got {held:?}"
    );
    let mut held_elsewhere = stored_names(&router, "after the refusals", &elsewhere).await;
    held_elsewhere.sort();
    let mut created_elsewhere: Vec<String> = asked_names()
        .into_iter()
        .filter(|asked| asked.creatable)
        .map(|asked| asked.name)
        .collect();
    created_elsewhere.sort();
    assert_eq!(
        held_elsewhere, created_elsewhere,
        "the other pool must still hold every name created in it, got {held_elsewhere:?}"
    );

    // And the live provider is still updatable, so the refusals are the lookup failing
    // rather than this operation refusing everything: the update is observable through a
    // later read.
    let updated = ok(
        &router,
        "the live provider",
        "UpdateIdentityProvider",
        &json!({
            "UserPoolId": asked_in,
            "ProviderName": "LiveOIDC",
            "AttributeMapping": { "email": "email_final" },
        }),
    )
    .await;
    assert_eq!(
        updated["IdentityProvider"]["AttributeMapping"]["email"],
        json!("email_final"),
        "the live provider must take the submitted mapping, got {updated}"
    );
    let read = ok(
        &router,
        "the live provider",
        "DescribeIdentityProvider",
        &json!({ "UserPoolId": asked_in, "ProviderName": "LiveOIDC" }),
    )
    .await;
    assert_eq!(
        read["IdentityProvider"]["AttributeMapping"]["email"],
        json!("email_final"),
        "the stored provider must read back with the submitted mapping, got {read}"
    );
}

#[tokio::test]
async fn a_pool_no_create_minted_is_reported_the_same_way_by_every_asking_operation() {
    // Every operation addressed by a pool and one provider of it, with the members each
    // carries beside `UserPoolId` so the POOL lookup is what the request fails on.
    let operations: &[(&str, Value)] = &[
        (
            "UpdateIdentityProvider",
            json!({
                "ProviderName": "AuthExampleOIDC",
                "AttributeMapping": { "email": "email_final" },
            }),
        ),
        (
            "DescribeIdentityProvider",
            json!({ "ProviderName": "AuthExampleOIDC" }),
        ),
        (
            "DeleteIdentityProvider",
            json!({ "ProviderName": "AuthExampleOIDC" }),
        ),
        (
            "GetIdentityProviderByIdentifier",
            json!({ "IdpIdentifier": "auth.example.com" }),
        ),
        ("ListIdentityProviders", json!({})),
    ];

    let router = router();
    // One pool that DOES exist, holding a provider, so a refusal cannot be the store
    // being empty.
    let live_pool = pool(&router, "absent-pool-live").await;
    create_provider(
        &router,
        "a live provider",
        &live_pool,
        "AuthExampleOIDC",
        &["auth.example.com"],
    )
    .await;

    for absent in ABSENT_POOLS {
        for (operation, beside) in operations {
            let case = format!("{operation} against the pool {absent}");
            let mut body = Map::new();
            body.insert("UserPoolId".to_string(), json!(absent));
            for (member, value) in beside.as_object().expect("the members are an object") {
                body.insert(member.clone(), value.clone());
            }
            let message = not_found(&router, &case, operation, &Value::Object(body)).await;
            let expected = format!("User pool {absent} does not exist.");
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
            // The pool is resolved first, so neither the provider nor the identifier the
            // request carried reaches the sentence.
            assert!(
                !message.contains("AuthExampleOIDC") && !message.contains("auth.example.com"),
                "case {case:?}: an unresolved pool is reported without what it was asked \
                 for, got {message:?}"
            );
        }
    }

    // Every one of those was a refusal: the stored pool still holds its provider, and
    // reads it back by name and by identifier.
    for (case, operation, body) in [
        (
            "by name",
            "DescribeIdentityProvider",
            json!({ "UserPoolId": live_pool, "ProviderName": "AuthExampleOIDC" }),
        ),
        (
            "by identifier",
            "GetIdentityProviderByIdentifier",
            json!({ "UserPoolId": live_pool, "IdpIdentifier": "auth.example.com" }),
        ),
    ] {
        let read = ok(&router, case, operation, &body).await;
        assert_eq!(
            read["IdentityProvider"]["ProviderName"],
            json!("AuthExampleOIDC"),
            "case {case:?}: the stored provider must read back, got {read}"
        );
    }
}

#[tokio::test]
async fn an_identifier_no_provider_carries_is_reported_as_the_resolve_path_words_it() {
    // Identifiers spanning `IdpIdentifierType` — length 1..40, pattern
    // `^[\w\s+=.@-]+$` (`smithy_query(query="shape",shape="IdpIdentifierType")`): the
    // shortest the length admits, a domain-form one, one carrying every punctuation the
    // pattern allows, and one at the published maximum length 40.
    let asked: Vec<String> = vec![
        "i".to_string(),
        "idp.example.org".to_string(),
        "team+group=one.two@example-corp".to_string(),
        "i".repeat(40),
    ];

    let router = router();
    let user_pool_id = pool(&router, "idp-identifier-pool").await;
    // A provider carrying one identifier, so a refusal is the identifier failing to
    // resolve rather than the pool carrying no identifiers at all.
    create_provider(
        &router,
        "a live provider",
        &user_pool_id,
        "AuthExampleOIDC",
        &["auth.example.com"],
    )
    .await;

    for identifier in &asked {
        let case = format!("the identifier {identifier:?} no provider of {user_pool_id} carries");
        let message = not_found(
            &router,
            &case,
            "GetIdentityProviderByIdentifier",
            &json!({ "UserPoolId": user_pool_id, "IdpIdentifier": identifier }),
        )
        .await;
        // The resolve path names what the request looked the provider up BY, which here
        // is the identifier, beside the pool it resolved in.
        let expected =
            format!("Identity provider {identifier} for tenantId {user_pool_id} does not exist.");
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }

    // The stored identifier still resolves, so the refusals are the lookup failing
    // rather than this operation refusing everything.
    let read = ok(
        &router,
        "the stored identifier",
        "GetIdentityProviderByIdentifier",
        &json!({ "UserPoolId": user_pool_id, "IdpIdentifier": "auth.example.com" }),
    )
    .await;
    assert_eq!(
        read["IdentityProvider"]["ProviderName"],
        json!("AuthExampleOIDC"),
        "the stored identifier must resolve to the provider carrying it, got {read}"
    );
}
