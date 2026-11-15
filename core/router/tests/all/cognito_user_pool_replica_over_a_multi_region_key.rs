// SPDX-License-Identifier: CC0-1.0

//! A multi-Region user pool replicated over a multi-Region KMS key, driven end to
//! end across the two services that own the pieces: KMS mints the multi-Region
//! customer-managed key and replicates it into the second Region, and Amazon
//! Cognito replicates the user pool that is encrypted under it.
//!
//! The rule under test: **a user pool encrypted under a customer-managed
//! multi-Region key can be driven through its whole replica lifecycle, and every
//! leg is observable through `ListUserPoolReplicas` in EITHER Region of the pool
//! — a created replica settles `INACTIVE`, `UpdateUserPoolReplica` moves it
//! between `ACTIVE` and `INACTIVE`, `DeleteUserPoolReplica` removes it so the
//! Region is no longer listed, and an operation naming the removed replica is
//! refused.** `ListUserPoolReplicas` "lists all replicas for a user pool,
//! including both primary and secondary replicas" and `UpdateUserPoolReplica`
//! "can be made in both primary and secondary regions of the user pool"
//! (`smithy_query(query="doc",shape="UpdateUserPoolReplica")`), so neither the
//! listing nor the status it reports depends on which of the pool's Regions the
//! caller addressed.
//!
//! Two crates have to agree for that to hold (KMS owns the key the pool names and
//! the replica of that key, Amazon Cognito owns the pool and its replicas), so the
//! suite lives here rather than in either of them.
//!
//! Every wait looks again for the CONDITION — the status the clock owes — rather
//! than for a duration, so no leg's outcome depends on how fast the host runs.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region the pool and the key's primary live in, and the Region both are
/// replicated to.
const PRIMARY_REGION: &str = "us-east-1";
const REPLICA_REGION: &str = "us-west-2";

/// The `ReplicaStatusType` values a secondary replica moves through
/// (`smithy_query(query="shape",shape="ReplicaStatusType")`).
const CREATING: &str = "CREATING";
const ACTIVE: &str = "ACTIVE";
const INACTIVE: &str = "INACTIVE";
const DELETING: &str = "DELETING";

/// The `KeyState` a usable KMS key reports.
const KEY_STATE_ENABLED: &str = "Enabled";

/// How long a leg may take before the suite calls it stuck. Generous: the spans
/// the emulator owes these transitions are far shorter, and only the condition
/// decides when a wait ends.
const BUDGET: Duration = Duration::from_secs(60);

/// How long a wait pauses before looking again.
const LOOK_AGAIN: Duration = Duration::from_millis(20);

/// The emulator with the two services this suite exercises, mounted on ONE engine
/// exactly as `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_kms_trentservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in
/// `region`, which is what routes the request and names the Region it was made
/// in. The signature is never checked: request authentication is off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one awsJson1_1 operation of `service` in `region`, through the
/// `X-Amz-Target` prefix `target`.
async fn call(
    router: &MultiServiceRouter,
    service: &str,
    target: &str,
    region: &str,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target}.{operation}"))
        .header("authorization", authorization(service, region))
        .body(Bytes::from(
            serde_json::to_vec(request).expect("the request serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
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

/// Call one KMS operation in `region`.
async fn kms(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    call(
        router,
        "kms",
        "TrentService",
        region,
        operation,
        request,
    )
    .await
}

/// Call one Amazon Cognito user pools operation in `region`.
async fn idp(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    call(
        router,
        "cognito-idp",
        "AWSCognitoIdentityProviderService",
        region,
        operation,
        request,
    )
    .await
}

/// Call `operation`, requiring success, and return its answer.
async fn ok(
    router: &MultiServiceRouter,
    region: &str,
    service: Service,
    operation: &str,
    request: &Value,
) -> Value {
    let (status, payload) = match service {
        Service::Kms => kms(router, region, operation, request).await,
        Service::Idp => idp(router, region, operation, request).await,
    };
    assert_eq!(
        status, 200,
        "{operation} in {region} must succeed, got {status} with {payload}"
    );
    payload
}

/// Which service a call goes to.
#[derive(Debug, Clone, Copy)]
enum Service {
    Kms,
    Idp,
}

/// The `__type` of an error body, which is the wire error code.
fn error_code(payload: &Value) -> &str {
    payload
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
}

/// The `message` of an error body.
fn error_message(payload: &Value) -> &str {
    payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
}

/// The status `ListUserPoolReplicas` in `asked_in` reports for the replica in
/// `region`, or `None` when that listing holds no entry for the Region.
async fn listed_status(
    router: &MultiServiceRouter,
    asked_in: &str,
    user_pool_id: &str,
    region: &str,
) -> Option<String> {
    let listed = ok(
        router,
        asked_in,
        Service::Idp,
        "ListUserPoolReplicas",
        &json!({ "UserPoolId": user_pool_id }),
    )
    .await;
    let entries = listed["UserPoolReplicas"]
        .as_array()
        .unwrap_or_else(|| panic!("ListUserPoolReplicas answers a list, got {listed}"))
        .clone();
    let found = entries
        .into_iter()
        .find(|entry| entry["RegionName"] == json!(region))?;
    Some(
        found["Status"]
            .as_str()
            .unwrap_or_else(|| panic!("a listed replica carries a status: {found}"))
            .to_string(),
    )
}

/// Look again until the listing in EVERY Region of the pool reports `want` for
/// the replica in `region` — `None` meaning the Region is no longer listed.
async fn wait_for_status(
    router: &MultiServiceRouter,
    case: &str,
    user_pool_id: &str,
    region: &str,
    want: Option<&str>,
) {
    for asked_in in [PRIMARY_REGION, REPLICA_REGION] {
        let deadline = Instant::now() + BUDGET;
        loop {
            let seen = listed_status(router, asked_in, user_pool_id, region).await;
            if seen.as_deref() == want {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the listing in {asked_in} never reported {want:?} for the \
                 replica in {region}, last saw {seen:?}"
            );
            tokio::time::sleep(LOOK_AGAIN).await;
        }
    }
}

#[tokio::test]
async fn a_pool_under_a_multi_region_key_is_replicated_end_to_end() {
    let router = build_router();
    let case = "a PLUS pool under a replicated customer-managed key";

    // The prerequisite: a multi-Region key, replicated into the Region the pool
    // will be replicated to. "You must use a multi-region key to enable
    // multi-region replication for a user pool"
    // (`smithy_query(query="doc",shape="KeyConfigurationType",member="KeyType")`).
    let key = ok(
        &router,
        PRIMARY_REGION,
        Service::Kms,
        "CreateKey",
        &json!({ "MultiRegion": true, "Description": "the pool's key" }),
    )
    .await;
    let key_id = key["KeyMetadata"]["KeyId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateKey mints a key id, got {key}"))
        .to_string();
    let key_arn = key["KeyMetadata"]["Arn"]
        .as_str()
        .expect("CreateKey reports the key ARN")
        .to_string();
    assert_eq!(
        key["KeyMetadata"]["MultiRegion"].as_bool(),
        Some(true),
        "case {case:?}: the key must be a multi-Region key, got {key}"
    );
    let replicated = ok(
        &router,
        PRIMARY_REGION,
        Service::Kms,
        "ReplicateKey",
        &json!({ "KeyId": key_id, "ReplicaRegion": REPLICA_REGION }),
    )
    .await;
    assert_eq!(
        replicated["ReplicaKeyMetadata"]["MultiRegionConfiguration"]["MultiRegionKeyType"].as_str(),
        Some("REPLICA"),
        "case {case:?}: the replicated key is a replica of the set, got {replicated}"
    );

    // The replica key settles into the state a usable key reports, which its OWN
    // Region's endpoint is what serves.
    let deadline = Instant::now() + BUDGET;
    loop {
        let described = ok(
            &router,
            REPLICA_REGION,
            Service::Kms,
            "DescribeKey",
            &json!({ "KeyId": key_id }),
        )
        .await;
        let state = described["KeyMetadata"]["KeyState"].as_str().map(str::to_string);
        if state.as_deref() == Some(KEY_STATE_ENABLED) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the replica key in {REPLICA_REGION} never reported \
             {KEY_STATE_ENABLED}, last saw {state:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }

    // The pool, encrypted under that key.
    let pool = ok(
        &router,
        PRIMARY_REGION,
        Service::Idp,
        "CreateUserPool",
        &json!({
            "PoolName": "replicated-pool",
            "UserPoolTier": "PLUS",
            "KeyConfiguration": {
                "KeyType": "CUSTOMER_MANAGED_KEY",
                "KmsKeyArn": key_arn,
            },
        }),
    )
    .await;
    let user_pool_id = pool["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateUserPool mints an id, got {pool}"))
        .to_string();

    // The create answers with the replica it stored, which the clock still owes
    // its activation.
    let created = ok(
        &router,
        PRIMARY_REGION,
        Service::Idp,
        "CreateUserPoolReplica",
        &json!({ "UserPoolId": user_pool_id, "RegionName": REPLICA_REGION }),
    )
    .await;
    assert_eq!(
        created["UserPoolReplica"]["Status"].as_str(),
        Some(CREATING),
        "case {case:?}: the create must answer the replica it stored, got {created}"
    );

    // Every leg is readable in BOTH of the pool's Regions.
    wait_for_status(&router, case, &user_pool_id, REPLICA_REGION, Some(INACTIVE)).await;
    for asked in [ACTIVE, INACTIVE] {
        let updated = ok(
            &router,
            PRIMARY_REGION,
            Service::Idp,
            "UpdateUserPoolReplica",
            &json!({
                "UserPoolId": user_pool_id,
                "RegionName": REPLICA_REGION,
                "Status": asked,
            }),
        )
        .await;
        assert_eq!(
            updated["UserPoolReplica"]["Status"].as_str(),
            Some(asked),
            "case {case:?}: the update must answer the status it stored, got {updated}"
        );
        wait_for_status(&router, case, &user_pool_id, REPLICA_REGION, Some(asked)).await;
    }

    let deleted = ok(
        &router,
        PRIMARY_REGION,
        Service::Idp,
        "DeleteUserPoolReplica",
        &json!({ "UserPoolId": user_pool_id, "RegionName": REPLICA_REGION }),
    )
    .await;
    assert_eq!(
        deleted["UserPoolReplica"]["Status"].as_str(),
        Some(DELETING),
        "case {case:?}: the delete must answer the replica it is removing, got {deleted}"
    );
    wait_for_status(&router, case, &user_pool_id, REPLICA_REGION, None).await;

    // The Region holds no replica any more, so the operations that address one
    // are refused rather than reviving it.
    let (status, payload) = idp(
        &router,
        PRIMARY_REGION,
        "UpdateUserPoolReplica",
        &json!({
            "UserPoolId": user_pool_id,
            "RegionName": REPLICA_REGION,
            "Status": ACTIVE,
        }),
    )
    .await;
    assert_eq!(
        (status, error_code(&payload), error_message(&payload)),
        (
            400,
            "ResourceNotFoundException",
            "Replica not found in the requested region"
        ),
        "case {case:?}: a removed replica is not found, got {status} with {payload}"
    );
}
