// SPDX-License-Identifier: CC0-1.0

//! Two services and one engine: the per-user-pool resource-server count Service Quotas
//! publishes for `cognito-idp` is the limit the Amazon Cognito user pools service
//! ENFORCES.
//!
//! RULE: a user pool holds at most as many resource servers as the account's Service
//! Quotas value for that count allows — `GetAWSDefaultServiceQuota` answers that count as
//! the AWS default of an adjustable quota — and `CreateResourceServer` past it is refused
//! `LimitExceededException`, the error this operation declares for a caller that "exceeds
//! the limit for a requested Amazon Web Services resource". The limit is on the STORED
//! count of ONE pool: another pool admits its own, a refused create stores nothing, and
//! deleting a stored resource server makes room for one more.
//!
//! Only a cross-service suite can pin this: the published quota is the Service Quotas
//! catalog's and the enforcement is the user-pool service's, so a client that reads its
//! own limit before provisioning — the documented way to avoid pinning a default AWS does
//! not guarantee — is told the number that the create it is about to make is judged
//! against. Both services are mounted on ONE engine, exactly as `core/router/src/lib.rs`
//! mounts them.
//!
//! The rows span what the rule generalises over: the create that fills the last slot, the
//! one past it, the same create on a second pool, the create after a delete, and the
//! identifier a refused create must not have stored.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call below is signed for.
const REGION: &str = "us-east-1";

/// The `X-Amz-Target` prefix Service Quotas routes on (that crate's `OPERATIONS.md`).
const QUOTAS_TARGET_PREFIX: &str = "ServiceQuotasV20190624";

/// The `X-Amz-Target` prefix the user pools service routes on (that crate's
/// `OPERATIONS.md`).
const COGNITO_TARGET_PREFIX: &str = "AWSCognitoIdentityProviderService";

/// The quota under test: AWS's `ServiceCode` for Amazon Cognito user pools and the
/// `QuotaCode` it publishes the per-pool resource-server count under.
const SERVICE_CODE: &str = "cognito-idp";
const QUOTA_CODE: &str = "L-7CDAF993";

/// The name AWS publishes that quota under.
const QUOTA_NAME: &str = "Resource servers per user pool";

/// The refusal a create past the quota earns, and the status this service answers a
/// client error with. The sentence states the LIMIT the create was judged against and
/// names nothing of the request, so the number in it is the published count above.
const LIMIT_EXCEEDED: &str = "LimitExceededException";
const REFUSAL_STATUS: u16 = 400;

/// The highest published count this suite will provision up to, so a quota raised beyond
/// anything a fixture needs fails the test instead of filling the store.
const MOST_RESOURCE_SERVERS_PROVISIONED: usize = 200;

/// Both services on one engine.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These callers sign nothing, so they must not be refused for it; the credential
        // scope below is what routes each request and names its Region.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_servicequotas_servicequotasv20190624::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice
                ::EmulatedService::new(Arc::clone(&engine)),
        );
        Self { router }
    }

    /// One `awsJson1_1` call, by target prefix and signing name.
    async fn call(
        &self,
        target_prefix: &str,
        signing_name: &str,
        operation: &str,
        body: &Value,
    ) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("{target_prefix}.{operation}"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
                     aws4_request, SignedHeaders=host;x-amz-target, Signature=0"
                ),
            )
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let parsed = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// One Service Quotas call.
    async fn quotas(&self, operation: &str, body: &Value) -> (u16, Value) {
        self.call(QUOTAS_TARGET_PREFIX, "servicequotas", operation, body)
            .await
    }

    /// One user-pool call.
    async fn cognito(&self, operation: &str, body: &Value) -> (u16, Value) {
        self.call(COGNITO_TARGET_PREFIX, "cognito-idp", operation, body)
            .await
    }

    /// One user-pool call that must succeed.
    async fn cognito_ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, payload) = self.cognito(operation, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {operation} must succeed, got {status} with {payload}"
        );
        payload
    }

    /// A fresh user pool's minted id.
    async fn pool(&self, name: &str) -> String {
        let created = self
            .cognito_ok(name, "CreateUserPool", &json!({ "PoolName": name }))
            .await;
        created["UserPool"]["Id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {name:?}: a pool carries a minted id, got {created}"))
            .to_string()
    }

    /// `CreateResourceServer` for the resource server at `index` in `user_pool_id`. The
    /// friendly name is spelled for `NameType`'s pattern `^[\w\s+=,.@-]+$`, which the
    /// identifier's own form does not satisfy.
    async fn create_resource_server(&self, user_pool_id: &str, index: usize) -> (u16, Value) {
        self.cognito(
            "CreateResourceServer",
            &json!({
                "UserPoolId": user_pool_id,
                "Identifier": identifier(index),
                "Name": format!("resource server {index:03}"),
            }),
        )
        .await
    }
}

/// The modelled error code an `awsJson1_1` error envelope carries in `__type`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .split('#')
        .next_back()
        .unwrap_or("<no __type>")
        .to_string()
}

/// The identifier of the resource server at `index`.
fn identifier(index: usize) -> String {
    format!("https://api-{index:03}.example.test")
}

#[tokio::test]
async fn the_published_resource_server_quota_is_the_limit_the_user_pool_enforces() {
    let emulator = Emulator::new();

    // What a client reads before provisioning: the AWS default of the adjustable quota
    // that counts a pool's resource servers.
    let (status, quota) = emulator
        .quotas(
            "GetAWSDefaultServiceQuota",
            &json!({ "ServiceCode": SERVICE_CODE, "QuotaCode": QUOTA_CODE }),
        )
        .await;
    assert_eq!(
        status, 200,
        "the AWS default of {SERVICE_CODE}/{QUOTA_CODE} must be served, got {status} with {quota}"
    );
    let published = quota["Quota"].clone();
    assert_eq!(
        published["QuotaName"].as_str(),
        Some(QUOTA_NAME),
        "the quota must be the per-pool resource-server count, got {published}"
    );
    assert_eq!(
        published["Adjustable"].as_bool(),
        Some(true),
        "AWS lets an increase be requested for it, got {published}"
    );
    let value = published["Value"]
        .as_f64()
        .unwrap_or_else(|| panic!("the quota carries a numeric Value, got {published}"));
    assert!(
        value >= 1.0 && value <= MOST_RESOURCE_SERVERS_PROVISIONED as f64,
        "the published count must be a provisionable number of resource servers, got {value}"
    );
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let allowed = value as usize;

    // Exactly that many creates are served on one pool.
    let user_pool_id = emulator.pool("quota-pool").await;
    for index in 0..allowed {
        let (status, payload) = emulator
            .create_resource_server(&user_pool_id, index)
            .await;
        assert_eq!(
            status,
            200,
            "case resource server {} of {allowed}: a create inside the quota must be served, \
             got {status} with {payload}",
            index + 1
        );
    }

    // The one past it is refused, with the error the operation declares for a caller
    // past its limit.
    let past = identifier(allowed);
    let (status, payload) = emulator
        .create_resource_server(&user_pool_id, allowed)
        .await;
    assert_eq!(
        status, REFUSAL_STATUS,
        "case the create past the quota: expected {REFUSAL_STATUS}, got {status} with {payload}"
    );
    assert_eq!(
        error_code(&payload),
        LIMIT_EXCEEDED,
        "case the create past the quota: expected {LIMIT_EXCEEDED}, got {payload}"
    );
    let message = payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("<no message>");
    assert_eq!(
        message,
        format!("The limit for user pool resource servers is: {allowed}."),
        "case the create past the quota: the refusal must state the limit it was judged \
         against, got {message:?}"
    );

    // And it stored nothing: the pool holds the quota's worth of resource servers, and
    // the identifier the refused create named is not one of them.
    let (status, described) = emulator
        .cognito(
            "DescribeResourceServer",
            &json!({ "UserPoolId": user_pool_id, "Identifier": past }),
        )
        .await;
    assert_eq!(
        status, REFUSAL_STATUS,
        "case the refused identifier: it must resolve to no resource server, got {status} with \
         {described}"
    );
    assert_eq!(
        error_code(&described),
        "ResourceNotFoundException",
        "case the refused identifier: expected ResourceNotFoundException, got {described}"
    );
    let listed = emulator
        .cognito_ok(
            "the full pool",
            "ListResourceServers",
            &json!({ "UserPoolId": user_pool_id, "MaxResults": 50 }),
        )
        .await;
    let held = listed["ResourceServers"]
        .as_array()
        .unwrap_or_else(|| panic!("a listing carries a ResourceServers list, got {listed}"))
        .len();
    assert_eq!(
        held, allowed,
        "case the full pool: it must hold the quota's worth of resource servers, got {held}"
    );

    // The limit is on ONE pool's stored count: a second pool admits its own.
    let other_pool = emulator.pool("second-quota-pool").await;
    let (status, payload) = emulator.create_resource_server(&other_pool, 0).await;
    assert_eq!(
        status, 200,
        "case another pool: the quota is per pool, so its first create must be served, got \
         {status} with {payload}"
    );

    // ... and on the STORED count rather than on how many creates were made: deleting one
    // makes room for one more.
    emulator
        .cognito_ok(
            "making room",
            "DeleteResourceServer",
            &json!({ "UserPoolId": user_pool_id, "Identifier": identifier(0) }),
        )
        .await;
    let (status, payload) = emulator
        .create_resource_server(&user_pool_id, allowed)
        .await;
    assert_eq!(
        status, 200,
        "case after a delete: the freed slot must admit a create, got {status} with {payload}"
    );
}
