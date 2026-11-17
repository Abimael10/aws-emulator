// SPDX-License-Identifier: CC0-1.0

//! Re-applying a rate that is already in force through `UpdateProvisionedLimit`,
//! driven through the router exactly as an SDK drives it (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.UpdateProvisionedLimit`).
//!
//! The rule under test: `UpdateProvisionedLimit` stores and reports EVERY
//! `RequestedLimitValue` inside the account's range for the identified limit —
//! "the value must be between the default limit and your account-level maximum
//! limit in Service Quotas" (`UpdateProvisionedLimit` in this service's model),
//! so from the limit's `FreeLimitValue` up to the account's Service Quotas
//! ceiling, both included — and refuses only a value outside it. A value equal
//! to the rate the limit already reports as in force is inside the range, so
//! the write is accepted and answers 200 with the limit it stored: setting a
//! limit to the rate it already holds is the same write made twice, not an
//! error, and a caller that re-applies the value a read just reported
//! converges.
//!
//! The suite varies the rate's position in the range (its floor, one step in,
//! the midpoint, the account's ceiling), the width of the range (none, so the
//! floor and the ceiling are the same rate, and one a granted quota increase
//! opened), how many times the same value is applied, and the limit itself —
//! every API category this account holds, whose included rates differ. The
//! wire harness is the one `log_delivery_configuration.rs` already builds for
//! this service, reached through the test crate root rather than copied; this
//! suite mounts its own router so it holds the engine the account's quota state
//! lives in.

use serde_json::{json, Value};
use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

use super::log_delivery_configuration::{call, ok};

/// How many times a rate is applied where the rule is idempotence: more than
/// twice, so the answer is asserted to be the same beyond the first repeat.
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
async fn the_rate_a_read_reports_is_accepted_again_and_answers_the_same_limit() {
    // The flow a client converging on a configuration runs: read the limit,
    // send back the rate it reported, and expect the limit it asked for. The
    // rows vary the width of the account's range — none, so the rate in force
    // is both bounds at once, and one a granted increase opened — and run every
    // API category this account holds through both, so the rule is asserted
    // over limits with different included rates rather than over one.
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

            // Applying it more than once: every repeat answers the same limit,
            // so the write is idempotent rather than accepted only the first
            // time.
            for repeat in 1..=REPEATS {
                let case = format!("{case} (apply {repeat} of {REPEATS})");
                let (status, answered) = provision(&router, category.name, requested).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: re-applying the rate in force is accepted, got {status} \
                     with {answered}"
                );
                assert_eq!(
                    answered.get("Limit"),
                    Some(&baseline),
                    "case {case:?}: the answer must be the limit the read reported, got {answered}"
                );
                assert_eq!(
                    answered["Limit"]["ProvisionedLimitValue"].as_i64(),
                    Some(in_force),
                    "case {case:?}: the answered rate must be the one asked for, got {answered}"
                );

                let reported = read_back(&router, &case, category.name).await;
                assert_eq!(
                    reported, baseline,
                    "case {case:?}: the limit a later read reports is the rate that was \
                     applied, got {reported}"
                );
            }
        }
    }
}

#[tokio::test]
async fn every_rate_inside_the_range_is_stored_whether_or_not_it_is_in_force() {
    let (router, engine) = router_and_engine();

    // The rows differ along where in the account's range the rate sits: its
    // floor (the rate the limit includes, which is the bottom of the accepted
    // range), one step in, the midpoint, and the account's ceiling. Each one is
    // applied twice in a row, so the second application asks for the rate the
    // first put in force — the case this suite is about — and both are expected
    // to answer the same limit.
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
            ("the floor of the range", category.free_rate),
            ("one step above the floor", category.free_rate + 1),
            ("the midpoint of the range", midpoint),
            ("the account's ceiling", ceiling),
        ] {
            for application in ["first", "repeated"] {
                let case = format!(
                    "{} asked for {requested} ({position}, {application} application)",
                    category.name
                );
                let expected = json!({
                    "LimitDefinition": definition(category.name),
                    "ProvisionedLimitValue": requested,
                    "FreeLimitValue": category.free_rate,
                });
                let (status, answered) = provision(&router, category.name, requested).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: a rate inside the account's range is stored, got {status} \
                     with {answered}"
                );
                assert_eq!(
                    answered.get("Limit"),
                    Some(&expected),
                    "case {case:?}: the update must answer the limit it stored, got {answered}"
                );

                let reported = read_back(&router, &case, category.name).await;
                assert_eq!(
                    reported, expected,
                    "case {case:?}: the read must render the stored limit, got {reported}"
                );
            }
        }
    }
}
