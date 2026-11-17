// SPDX-License-Identifier: CC0-1.0

//! Re-applying a rate that is already in force through `UpdateProvisionedLimit`,
//! driven through the router exactly as an SDK drives it (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.UpdateProvisionedLimit`).
//!
//! The rule under test: `UpdateProvisionedLimit` "sets the provisioned limit for
//! a specific API category" (this service's model), so the `RequestedLimitValue`
//! has to CHANGE the limit. A request naming the rate the limit already reports
//! as its `ProvisionedLimitValue` sets nothing and is refused with
//! `InvalidParameterException` (HTTP 400) whose one sentence states that the
//! requested value is the same as the current provisioned limit, and the limit a
//! later read reports is the one that was already in force. The comparison is
//! against the CURRENT provisioned value and is taken ahead of the account's
//! Service Quotas ceiling, so a request that is both the rate in force and the
//! account's ceiling is refused as the former. Every other rate in the account's
//! range — "the value must be between the default limit and your account-level
//! maximum limit in Service Quotas" (the same doc) — is still stored.
//!
//! The suite varies the rate's position in the range (its floor, one step in,
//! the midpoint, the account's ceiling), the width of the range (none, so the
//! floor and the ceiling are the same rate, and one a granted quota increase
//! opened), how many times the same value is re-applied, and the limit itself —
//! every API category this account holds, whose included rates differ. The
//! wire harness is the one `log_delivery_configuration.rs` already builds for
//! this service, reached through the test crate root rather than copied; this
//! suite mounts its own router so it holds the engine the account's quota state
//! lives in.

use serde_json::{json, Value};
use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

use super::log_delivery_configuration::{call, error_code, error_message, ok};

/// How many times a rate in force is re-applied where the rule is that every
/// such request is refused: more than twice, so the answer is asserted beyond
/// the first repeat.
const REPEATS: usize = 3;

/// How far above a limit's included rate a granted Service Quotas increase
/// raises the account's ceiling, in the rows that open the range.
const GRANTED_INCREASE: i32 = 500;

/// One emulator with just this service mounted, beside the engine its state —
/// including the account's Service Quotas ceilings — lives in.
fn router_and_engine() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(Arc::clone(&engine)));
    router.set_request_authentication(false);
    (router, engine)
}

/// The definition that identifies `category`'s API rate limit.
fn definition(category: &str) -> Value {
    json!({ "LimitClass": "API_CATEGORY", "Attributes": { "Category": category } })
}

/// The limit `GetProvisionedLimit` reports for `category`.
async fn read_back(router: &MultiServiceRouter, case: &str, category: &str) -> Value {
    let payload = ok(
        router,
        "GetProvisionedLimit",
        &json!({ "LimitDefinition": definition(category) }),
    )
    .await;
    payload
        .get("Limit")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the read must carry a limit: {payload}"))
}

/// Ask for `requested` requests per second on `category`'s limit.
async fn provision(router: &MultiServiceRouter, category: &str, requested: i32) -> (u16, Value) {
    call(
        router,
        "UpdateProvisionedLimit",
        &json!({
            "LimitDefinition": definition(category),
            "RequestedLimitValue": requested,
        }),
    )
    .await
}

/// What a refusal says for a rate the limit already holds: one sentence with no
/// variable part, the rate it is about being the one the request carried.
fn same_as_provisioned_message() -> String {
    "Requested value is the same as the current provisioned limit".to_string()
}

/// Record a granted Service Quotas increase raising this account's ceiling for
/// `category` to `ceiling`, and return the ceiling now in force.
///
/// A Service Quotas increase is the only thing that moves this ceiling and is
/// not an operation of this service, so it is applied to the account's own
/// quota state rather than over the wire.
fn granted_increase(
    engine: &Arc<engine::InMemoryEngine>,
    category: &cognito_idp::state::ApiCategory,
    ceiling: i32,
) -> i32 {
    let service_state = cognito_idp::state::service_state(&**engine).expect("this service's state");
    let mut guard = service_state.lock();
    guard.raise_api_category_quota_ceiling(category, ceiling);
    guard.api_category_quota_ceiling(category)
}

#[tokio::test]
async fn the_rate_a_read_reports_is_refused_when_it_is_sent_back() {
    // The flow a client converging on a configuration runs: read the limit,
    // send back the rate it reported, and be told it is the rate already
    // provisioned. The rows vary the width of the account's range — none, so
    // the rate in force is both bounds at once, and one a granted increase
    // opened — and run every API category this account holds through both, so
    // the rule is asserted over limits with different included rates rather
    // than over one.
    for increase in [0, GRANTED_INCREASE] {
        for category in cognito_idp::state::API_CATEGORIES {
            let (router, engine) = router_and_engine();
            let case = format!(
                "{} re-applied with a ceiling {increase} above its included rate",
                category.name
            );
            if increase > 0 {
                granted_increase(&engine, category, category.free_rate + increase);
            }

            let baseline = read_back(&router, &case, category.name).await;
            let in_force = baseline["ProvisionedLimitValue"]
                .as_i64()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: the read must report a rate in force, got {baseline}")
                });
            let requested = i32::try_from(in_force)
                .unwrap_or_else(|_| panic!("case {case:?}: {in_force} is a modelled rate"));

            // Sending it back more than once: every repeat is refused the same
            // way, so the refusal is the rule rather than a one-off.
            for repeat in 1..=REPEATS {
                let case = format!("{case} (apply {repeat} of {REPEATS})");
                let (status, payload) = provision(&router, category.name, requested).await;
                assert_eq!(
                    (
                        status,
                        error_code(&payload),
                        error_message(&payload).to_string()
                    ),
                    (
                        400,
                        "InvalidParameterException",
                        same_as_provisioned_message()
                    ),
                    "case {case:?}: re-applying the rate in force is refused, got {status} \
                     with {payload}"
                );

                let reported = read_back(&router, &case, category.name).await;
                assert_eq!(
                    reported, baseline,
                    "case {case:?}: a refused update leaves the limit as it was, got {reported}"
                );
            }
        }
    }
}

#[tokio::test]
async fn every_rate_inside_the_range_is_stored_once_and_refused_when_it_is_in_force() {
    let (router, engine) = router_and_engine();

    // The rows differ along where in the account's range the rate sits: its
    // floor (the rate the limit includes, which is the bottom of the accepted
    // range), one step in, the midpoint, and the account's ceiling. Each one is
    // sent twice in a row: the first application is an adjustment and is
    // stored, the second asks for the rate the first put in force — the case
    // this suite is about — and is refused, leaving that rate in place.
    for category in cognito_idp::state::API_CATEGORIES {
        let ceiling = granted_increase(&engine, category, category.free_rate + GRANTED_INCREASE);
        assert_eq!(
            ceiling,
            category.free_rate + GRANTED_INCREASE,
            "case {:?}: the granted increase must be the ceiling in force, got {ceiling}",
            category.name
        );
        let midpoint = (category.free_rate + ceiling) / 2;

        for (position, requested) in [
            ("one step above the floor", category.free_rate + 1),
            ("the midpoint of the range", midpoint),
            ("the account's ceiling", ceiling),
            ("the floor of the range", category.free_rate),
        ] {
            let expected = json!({
                "LimitDefinition": definition(category.name),
                "ProvisionedLimitValue": requested,
                "FreeLimitValue": category.free_rate,
            });

            let case = format!(
                "{} asked for {requested} ({position}, first application)",
                category.name
            );
            let (status, answered) = provision(&router, category.name, requested).await;
            assert_eq!(
                status, 200,
                "case {case:?}: a rate inside the account's range that changes the limit is \
                 stored, got {status} with {answered}"
            );
            assert_eq!(
                answered.get("Limit"),
                Some(&expected),
                "case {case:?}: the update must answer the limit it stored, got {answered}"
            );

            let case = format!(
                "{} asked for {requested} ({position}, repeated application)",
                category.name
            );
            let (status, payload) = provision(&router, category.name, requested).await;
            assert_eq!(
                (
                    status,
                    error_code(&payload),
                    error_message(&payload).to_string()
                ),
                (
                    400,
                    "InvalidParameterException",
                    same_as_provisioned_message()
                ),
                "case {case:?}: the rate now in force is refused, got {status} with {payload}"
            );

            let reported = read_back(&router, &case, category.name).await;
            assert_eq!(
                reported, expected,
                "case {case:?}: the read must render the stored limit, got {reported}"
            );
        }
    }
}
