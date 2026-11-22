// SPDX-License-Identifier: CC0-1.0

//! WHICH Region of a multi-Region user pool serves `GetSigningCertificate`.
//!
//! The RULE under test: a user pool is a regional resource that reaches another
//! Region only as a replica of itself, and the replica's role decides what that
//! Region serves — "the primary replica supports all end user and administrator
//! operations", while "the secondary replica supports a limited set of end user
//! and administrator operations. Generally, only administrator operations that
//! set configurations specific to the replica, and only end-user operations that
//! do not create or change attributes of a user are supported"
//! (`smithy_query(query="doc",shape="UserPoolReplicaType",member="Role")`). The
//! signing certificate is the POOL's own configuration rather than anything
//! specific to one replica, so the operation is served in the Region holding the
//! pool's primary replica alone:
//!
//! * in the pool's own Region it answers the pool's certificate;
//! * in a Region holding a secondary replica of that pool it is refused with
//!   `OperationNotEnabledException`, which "is thrown when an operation is not
//!   available in the current region ... when attempting to perform operations
//!   that are not supported in secondary replica regions"
//!   (`smithy_query(query="doc",shape="OperationNotEnabledException")`), HTTP
//!   400, and the fixed sentence of that error class;
//! * in a Region the pool holds no replica in, no pool of that id exists there
//!   at all, so the request is refused as an unresolved user pool.
//!
//! The rows differ along what the rule generalises over: the Region the pool is
//! primary in, how many Regions it is replicated to and which, the pool's
//! feature plan, and which Regions it was never replicated to. A further test
//! drives the refusal through the replica's own lifecycle — a Region that holds
//! a replica still coming up, and the same Region once the replica is gone — so
//! the answer follows the STORED replicas rather than any fixed list of Regions.
//!
//! Every wait here looks again for the CONDITION a listing reports rather than
//! for a duration, so no row's outcome depends on how fast the host runs.

use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use cognito_idp::state::{
    ESSENTIALS_USER_POOL_TIER, PLUS_USER_POOL_TIER, REPLICA_STATUS_INACTIVE,
};
use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

use super::saml_signing_certificate::{call, error_code, error_message, ok, parse, router};

/// The whole message `OperationNotEnabledException` carries for an operation the
/// Region a request reached does not serve: fixed text for this error class,
/// naming neither the Region nor the pool.
const NOT_ENABLED_MESSAGE: &str = "This operation is not enabled in this region for this User Pool";

/// A `KeyConfiguration.KmsKeyArn`-shaped ARN (`EncryptionKeyArnType`, len
/// 20..2048) standing for the multi-Region customer-managed key a replicated
/// pool has to be encrypted under: "you must use a multi-region key to enable
/// multi-region replication for a user pool"
/// (`smithy_query(query="doc",shape="KeyConfigurationType",member="KeyType")`).
const MULTI_REGION_KEY_ARN: &str = "arn:aws:kms:us-east-1:123456789012:key/multi-region-example";

/// How long a leg of a replica's lifecycle may take before the suite calls it
/// stuck. Generous: the span the emulator owes a transition is far shorter, and
/// only the condition decides when a wait ends.
const BUDGET: Duration = Duration::from_secs(60);

/// How long a wait pauses before looking again.
const LOOK_AGAIN: Duration = Duration::from_millis(20);

/// One row: the Region the pool is created in, the plan it is on, the Regions it
/// is replicated to, and Regions it is never replicated to.
struct Case {
    case: &'static str,
    primary_region: &'static str,
    tier: &'static str,
    replica_regions: &'static [&'static str],
    unreplicated_regions: &'static [&'static str],
}

/// The table. The rows span the pool's own Region (four Regions, of three
/// partitions' geographies), the feature plans that include multi-Region
/// replication (`UserPoolTierType`'s `ESSENTIALS` and `PLUS`), one and several
/// secondary Regions per pool, and both a pool with no secondary replica at all
/// and Regions no pool here was replicated to.
const CASES: &[Case] = &[
    Case {
        case: "a PLUS pool replicated to one Region",
        primary_region: "us-east-1",
        tier: PLUS_USER_POOL_TIER,
        replica_regions: &["us-west-2"],
        unreplicated_regions: &["eu-west-1"],
    },
    Case {
        case: "an ESSENTIALS pool replicated to two Regions",
        primary_region: "eu-west-1",
        tier: ESSENTIALS_USER_POOL_TIER,
        replica_regions: &["eu-central-1", "ap-southeast-1"],
        unreplicated_regions: &["us-east-1", "sa-east-1"],
    },
    Case {
        case: "a PLUS pool whose own Region is outside the Regions it replicates to",
        primary_region: "ap-southeast-2",
        tier: PLUS_USER_POOL_TIER,
        replica_regions: &["ap-northeast-1"],
        unreplicated_regions: &["us-west-2", "eu-north-1"],
    },
    Case {
        case: "a pool replicated nowhere, so every other Region holds no pool of its id",
        primary_region: "us-west-2",
        tier: ESSENTIALS_USER_POOL_TIER,
        replica_regions: &[],
        unreplicated_regions: &["us-east-1", "eu-central-1"],
    },
];

/// Create a pool in `region` on `tier`, encrypted under a multi-Region
/// customer-managed key so it can be replicated, and return its minted
/// `UserPoolId`.
async fn create_pool(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    tier: &str,
    name: &str,
) -> String {
    let mut body = Map::new();
    body.insert("PoolName".to_string(), json!(name));
    body.insert("UserPoolTier".to_string(), json!(tier));
    body.insert(
        "KeyConfiguration".to_string(),
        json!({ "KeyType": "CUSTOMER_MANAGED_KEY", "KmsKeyArn": MULTI_REGION_KEY_ARN }),
    );
    let created = ok(router, region, "CreateUserPool", &Value::Object(body)).await;
    created["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateUserPool must mint an id, got {created}"))
        .to_string()
}

/// Replicate `user_pool_id` to `replica_region`, from the pool's own Region.
async fn create_replica(
    router: &MultiServiceRouter,
    primary_region: &str,
    user_pool_id: &str,
    replica_region: &str,
) {
    ok(
        router,
        primary_region,
        "CreateUserPoolReplica",
        &json!({ "UserPoolId": user_pool_id, "RegionName": replica_region }),
    )
    .await;
}

/// The status `ListUserPoolReplicas` reports for `region`, or `None` when the
/// listing holds no entry for that Region at all.
async fn listed_status(
    router: &MultiServiceRouter,
    primary_region: &str,
    user_pool_id: &str,
    region: &str,
) -> Option<String> {
    let listed = ok(
        router,
        primary_region,
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
            .unwrap_or_else(|| panic!("a listed replica carries a status, got {found}"))
            .to_string(),
    )
}

/// Look again until `ListUserPoolReplicas` reports `want` for `region` — `None`
/// meaning the listing holds no entry for it. The wait ends on the CONDITION;
/// the budget is only there so a stuck lifecycle fails instead of hanging.
async fn wait_for(
    router: &MultiServiceRouter,
    case: &str,
    primary_region: &str,
    user_pool_id: &str,
    region: &str,
    want: Option<&str>,
) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let status = listed_status(router, primary_region, user_pool_id, region).await;
        if status.as_deref() == want {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the replica in {region} never reached {want:?}, last saw {status:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Assert that `region` refuses `GetSigningCertificate` for `user_pool_id` as an
/// operation it does not serve: HTTP 400 `OperationNotEnabledException` with the
/// fixed sentence of that error class.
async fn assert_not_enabled(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    user_pool_id: &str,
) {
    let (status, payload) = call(
        router,
        region,
        "GetSigningCertificate",
        &json!({ "UserPoolId": user_pool_id }),
    )
    .await;
    assert_eq!(
        (
            status,
            error_code(&payload),
            error_message(&payload).to_string()
        ),
        (
            400,
            "OperationNotEnabledException",
            NOT_ENABLED_MESSAGE.to_string()
        ),
        "case {case:?}: {region} holds a secondary replica of {user_pool_id}, which does not \
         serve this operation, got {status} with {payload}"
    );
}

/// Assert that `region` refuses `GetSigningCertificate` for `user_pool_id` as an
/// unresolved user pool: no pool of that id exists in a Region the pool was
/// never replicated to.
async fn assert_pool_absent(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    user_pool_id: &str,
) {
    let (status, payload) = call(
        router,
        region,
        "GetSigningCertificate",
        &json!({ "UserPoolId": user_pool_id }),
    )
    .await;
    let expected = format!("User pool {user_pool_id} does not exist.");
    assert_eq!(
        (
            status,
            error_code(&payload),
            error_message(&payload).to_string()
        ),
        (400, "ResourceNotFoundException", expected),
        "case {case:?}: {region} holds no replica of {user_pool_id}, so the pool does not exist \
         there, got {status} with {payload}"
    );
}

// ---------------------------------------------------------------------------
// RULE: the certificate is served in the Region holding the pool's PRIMARY
// replica alone — a Region holding a secondary replica does not serve the
// operation, and a Region the pool was never replicated to holds no such pool.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_certificate_is_served_in_the_pools_primary_region_alone() {
    for case in CASES {
        let router = router();
        let user_pool_id = create_pool(
            &router,
            case.case,
            case.primary_region,
            case.tier,
            "signing-certificate-region-scope",
        )
        .await;
        for region in case.replica_regions {
            create_replica(&router, case.primary_region, &user_pool_id, region).await;
        }

        // The pool's own Region serves the certificate, and what it serves is a
        // real X.509 certificate rather than a placeholder.
        let served = {
            let answered = ok(
                &router,
                case.primary_region,
                "GetSigningCertificate",
                &json!({ "UserPoolId": user_pool_id }),
            )
            .await;
            answered["Certificate"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: the primary Region must answer a certificate, got {answered}",
                        case.case
                    )
                })
                .to_string()
        };
        parse(case.case, &served);

        for region in case.replica_regions {
            assert_not_enabled(&router, case.case, region, &user_pool_id).await;
        }
        for region in case.unreplicated_regions {
            assert_pool_absent(&router, case.case, region, &user_pool_id).await;
        }

        // The refusals changed nothing: the primary Region still answers the one
        // certificate the pool was issued.
        let again = ok(
            &router,
            case.primary_region,
            "GetSigningCertificate",
            &json!({ "UserPoolId": user_pool_id }),
        )
        .await;
        assert_eq!(
            again["Certificate"],
            json!(served),
            "case {:?}: the primary Region must still answer the pool's certificate after the \
             refusals, got {again}",
            case.case
        );
    }
}

// ---------------------------------------------------------------------------
// RULE: the answer follows the STORED replicas — a Region answers the refusal
// of an operation it does not serve from the moment the pool holds a replica
// there, and answers as a Region holding no such pool once the replica is gone.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_regions_answer_follows_the_replica_it_holds() {
    let case = "a replica created and then deleted again";
    let primary_region = "us-east-1";
    let replica_region = "eu-central-1";
    let router = router();
    let user_pool_id = create_pool(
        &router,
        case,
        primary_region,
        PLUS_USER_POOL_TIER,
        "signing-certificate-replica-lifecycle",
    )
    .await;

    // Before anything is replicated, the other Region holds no pool of this id.
    assert_pool_absent(&router, case, replica_region, &user_pool_id).await;

    // From the moment the replica is stored — while it is still coming up — that
    // Region answers the refusal of an operation it does not serve rather than a
    // certificate.
    create_replica(&router, primary_region, &user_pool_id, replica_region).await;
    assert_not_enabled(&router, case, replica_region, &user_pool_id).await;

    // And once the clock has settled the replica, which is the status a delete
    // is served from ("you can only delete replicas that are in the INACTIVE
    // status", `smithy_query(query="doc",shape="DeleteUserPoolReplica")`).
    wait_for(
        &router,
        case,
        primary_region,
        &user_pool_id,
        replica_region,
        Some(REPLICA_STATUS_INACTIVE),
    )
    .await;
    assert_not_enabled(&router, case, replica_region, &user_pool_id).await;

    // With the replica taken away, the Region holds no pool of this id again.
    ok(
        &router,
        primary_region,
        "DeleteUserPoolReplica",
        &json!({ "UserPoolId": user_pool_id, "RegionName": replica_region }),
    )
    .await;
    wait_for(
        &router,
        case,
        primary_region,
        &user_pool_id,
        replica_region,
        None,
    )
    .await;
    assert_pool_absent(&router, case, replica_region, &user_pool_id).await;

    // Through all of it, the pool's own Region kept serving its certificate.
    let answered = ok(
        &router,
        primary_region,
        "GetSigningCertificate",
        &json!({ "UserPoolId": user_pool_id }),
    )
    .await;
    let certificate = answered["Certificate"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the primary Region answers a certificate, got {answered}"));
    parse(case, certificate);
}
