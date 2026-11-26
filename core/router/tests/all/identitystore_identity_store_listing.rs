// SPDX-License-Identifier: CC0-1.0

//! The identity stores a caller has are the stores of the IAM Identity Center instances
//! the account holds — asserted across BOTH services, assembled by the production
//! [`MultiServiceRouter`] the way `core/router/src/lib.rs` assembles them.
//!
//! The rule under test: an identity store is not a resource any API creates, it comes with
//! an Identity Center instance and is that instance's `InstanceMetadata$IdentityStoreId`,
//! so the set `identitystore:ListIdentityStores` reports is the set the service that OWNS
//! instances answers — and it is the SAME set the resolution every store-addressed
//! operation goes through admits, so a store the listing leaves out is one those operations
//! refuse. While Identity Center has decided nothing about the account's enablement, the
//! account's stores are not knowable from instances and the listing reports the stores this
//! emulator holds a record of.
//!
//! Only a cross-service suite can pin the two halves agree: the authority is the other
//! service's own reading of its instances, so a suite that mounts one service can assert
//! what a listing reports but never that it FOLLOWS the instances the account holds. The
//! rest of the rule — which forms of an `IdentityStoreId` a store is reported under, the
//! page sizes, the cursors — is
//! `services/service-com-amazonaws-identitystore-awsidentitystore/tests/all/caller_named_identity_stores.rs`.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The `X-Amz-Target` prefix of the identity store's `awsJson1_1` wire (its crate's
/// OPERATIONS.md, "Wire protocol").
const TARGET_PREFIX: &str = "AWSIdentityStore";

/// The `X-Amz-Target` prefix of IAM Identity Center's own wire.
const IDENTITY_CENTER_TARGET_PREFIX: &str = "SWBExternalService";

/// The identity store IAM Identity Center provisioned for the instance `sso-admin` seeds,
/// which is the one its `ListInstances` points a client at. Read from that crate's own
/// constant rather than restated, so the two services cannot drift apart here.
const SEED_STORE: &str =
    service_com_amazonaws_identitystore_awsidentitystore::state::SEED_IDENTITY_STORE_ID;

/// The access key id these calls are signed with. Signatures are not what this suite is
/// about, but the credential scope is what routes a request to its service.
const AKID: &str = "AKIDLISTIDENTITYSTORES";

/// The identity store AND IAM Identity Center on ONE engine, as `core/router/src/lib.rs`
/// mounts them: the stores the identity store serves are the stores that service's own
/// instances have.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signatures are not what this suite is about; the request still carries a SigV4
    // credential scope, which is what routes it to its service.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_identitystore_awsidentitystore::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_ssoadmin_swbexternalservice::EmulatedService::new(engine),
    );
    router
}

/// One `awsJson1_1` call: the HTTP status, the wire error code and the parsed body.
async fn call_service(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: Value,
) -> (u16, String, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("host", format!("{signing_name}.us-east-1.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={AKID}/20260101/us-east-1\
                 /{signing_name}/aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let header_code = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    let code = if header_code.is_empty() {
        parsed
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    } else {
        header_code
    };
    (status, code, parsed)
}

/// An identity-store call that must answer 200, as its body.
async fn served(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    let (status, code, parsed) =
        call_service(router, "identitystore", TARGET_PREFIX, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} to answer 200, got {status} {code} with {parsed}"
    );
    parsed
}

/// An identity-store call that must be refused with `status` and an error code naming
/// `error`.
async fn refused(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
    status: u16,
    error: &str,
    case: &str,
) -> Value {
    let (actual_status, code, parsed) =
        call_service(router, "identitystore", TARGET_PREFIX, operation, body).await;
    assert_eq!(
        actual_status, status,
        "case {case:?}: expected {operation} to answer {status} {error}, \
         got {actual_status} {code} with {parsed}"
    );
    assert!(
        code.ends_with(error),
        "case {case:?}: expected {operation} to answer {error}, got {code:?} with {parsed}"
    );
    parsed
}

/// One Identity Center call that must answer 200, as its body.
async fn identity_center(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
    case: &str,
) -> Value {
    let (status, code, parsed) =
        call_service(router, "sso", IDENTITY_CENTER_TARGET_PREFIX, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected Identity Center's {operation} to answer 200, \
         got {status} {code} with {parsed}"
    );
    parsed
}

/// Every identity store the listing reports, walked to the end: the ids in the order the
/// pages reported them, each checked to carry the ARN derived from its own id.
async fn listed_stores(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let mut collected: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    // One page per store is the most a walk can need; the loop is bounded so a listing
    // that never finishes fails the case instead of hanging it.
    for page in 1..=16 {
        let mut request = json!({ "MaxResults": 1 });
        if let Some(token) = token.clone() {
            request["NextToken"] = json!(token);
        }
        let row = format!("{case}, page {page}");
        let answer = served(router, "ListIdentityStores", request, &row).await;
        let entries = answer["IdentityStores"].as_array().cloned().unwrap_or_else(|| {
            panic!("case {row:?}: expected the required IdentityStores list, got {answer}")
        });
        for entry in &entries {
            let identity_store_id = entry["IdentityStoreId"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {row:?}: expected a required IdentityStoreId, got {entry}")
                })
                .to_string();
            let expected_arn = format!(
                "arn:aws:identitystore::{}:identitystore/{identity_store_id}",
                engine::emulator_account_id()
            );
            assert_eq!(
                entry["IdentityStoreArn"].as_str(),
                Some(expected_arn.as_str()),
                "case {row:?}: expected the ARN of {identity_store_id:?}, got {entry}"
            );
            collected.push(identity_store_id);
        }
        match answer["NextToken"].as_str() {
            None => return collected,
            Some(next) => token = Some(next.to_string()),
        }
    }
    panic!("case {case:?}: the paged walk did not finish: {collected:?}")
}

/// `CreateGroup` in the store `identity_store_id` names, which is what makes this emulator
/// hold a record of that store.
async fn write_to_store(
    router: &MultiServiceRouter,
    identity_store_id: &str,
    display_name: &str,
    case: &str,
) {
    served(
        router,
        "CreateGroup",
        json!({ "IdentityStoreId": identity_store_id, "DisplayName": display_name }),
        case,
    )
    .await;
}

/// Once IAM Identity Center has answered which identity stores the account holds, the
/// listing is that set — and the resolution every store-addressed operation goes through
/// admits exactly the stores the listing reports.
#[tokio::test]
async fn the_identity_stores_listed_are_the_stores_identity_center_answers() {
    let router = build_router();

    // While nothing is decided, the store a caller's own environment names is served, so
    // this emulator comes to hold a record of it — and reports it.
    let caller_store = "d-00000000f8";
    write_to_store(
        &router,
        caller_store,
        "group-in-the-callers-store",
        "an undecided account",
    )
    .await;
    let mut listed = listed_stores(&router, "an undecided account").await;
    listed.sort();
    assert_eq!(
        listed,
        {
            let mut held = vec![caller_store.to_string(), SEED_STORE.to_string()];
            held.sort();
            held
        },
        "case \"an undecided account\": expected the two stores this emulator holds"
    );

    // A client reads the account's identity store the way AWS gives it one: Identity
    // Center's own listing of its instances. Asking also settles the account's enablement,
    // so from here on the stores the account has are knowable.
    let instances = identity_center(&router, "ListInstances", json!({}), "a decided account").await;
    let mut account_stores: Vec<String> = instances["Instances"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|instance| instance["IdentityStoreId"].as_str().map(str::to_string))
        .collect();
    account_stores.sort();
    assert!(
        !account_stores.is_empty(),
        "case \"a decided account\": expected Identity Center to report an instance, \
         got {instances}"
    );

    let listed = listed_stores(&router, "a decided account").await;
    assert_eq!(
        listed, account_stores,
        "case \"a decided account\": expected the stores of the account's instances, \
         got {listed:?}"
    );
    assert!(
        !listed.contains(&caller_store.to_string()),
        "case \"a decided account\": expected a store the account does not have to be \
         absent, got {listed:?}"
    );

    // The listing and that resolution agree, in both directions: a store the listing
    // leaves out is refused, and every store it reports is served.
    refused(
        &router,
        "ListGroups",
        json!({ "IdentityStoreId": caller_store }),
        400,
        "ResourceNotFoundException",
        "a store the listing leaves out",
    )
    .await;
    for identity_store_id in &listed {
        served(
            &router,
            "ListGroups",
            json!({ "IdentityStoreId": identity_store_id }),
            &format!("the listed store {identity_store_id}"),
        )
        .await;
    }
}
