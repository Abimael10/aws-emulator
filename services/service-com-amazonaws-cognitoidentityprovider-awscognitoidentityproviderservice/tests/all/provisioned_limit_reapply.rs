// SPDX-License-Identifier: CC0-1.0

//! Asking `UpdateProvisionedLimit` for the rate the account ALREADY has in
//! force, driven through the router exactly as an SDK drives it (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.UpdateProvisionedLimit`).
//!
//! The rule under test: `UpdateProvisionedLimit` "sets the provisioned limit for
//! a specific API category. The value must be between the default limit and your
//! account-level maximum limit in Service Quotas" (this service's model), so the
//! only thing asked of a request is that `RequestedLimitValue` land inside the
//! account's range for the limit. A value equal to the rate already in force is
//! inside it: the call is SERVED and answers the limit as it stands, whether a
//! previous call provisioned that rate or it is the rate the category includes at
//! no additional cost — an update that asks for the state a resource is already
//! in is that state, not a bad parameter. Every other rate inside the range —
//! from the included rate up to the account's Service Quotas
//! ceiling — is stored and answered as the limit's `ProvisionedLimitValue`, and
//! a rate outside the range is refused on the bound it missed: above the
//! ceiling with `ServiceQuotaExceededException`, below the included rate with
//! `InvalidParameterException`. Every refused request stores nothing, so the
//! limit a later read reports is the one that was already in force.
//!
//! The suite varies the state the rate in force comes from (the included rate,
//! with nothing provisioned, and a rate a provisioning stored), the width of the
//! account's range (none, so the included rate is both bounds at once, and one a
//! granted quota increase opened), how many times the same value is re-applied,
//! the position of an accepted rate in the range, and the limit itself — every
//! API category this account holds, whose included rates differ. The wire
//! harness is the one `log_delivery_configuration.rs` already builds for this
//! service, reached through the test crate root rather than copied; this suite
//! mounts its own router so it holds the engine the account's quota state lives
//! in.

use serde_json::{json, Value};
use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

use super::log_delivery_configuration::{call, error_code, error_message, ok};
// The two refusal sentences this suite asserts are the ones the sibling suite
// already words, so they have one home in this test crate.
use super::provisioned_limits::{above_ceiling_message, below_included_message};

/// How many times a rate in force is re-applied: more than twice, so the answer
/// is asserted beyond the first repeat.
const REPEATS: usize = 3;

/// How far above a limit's included rate a granted Service Quotas increase raises the
/// account's ceiling, in the rows that open the range.
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

/// The rate a client reconciling a configuration takes from a read: the limit's
/// provisioned value, falling back to the rate it includes when no provisioned
/// value is reported.
fn rate_in_force(case: &str, limit: &Value) -> i32 {
    let reported = limit
        .get("ProvisionedLimitValue")
        .and_then(Value::as_i64)
        .or_else(|| limit.get("FreeLimitValue").and_then(Value::as_i64))
        .unwrap_or_else(|| panic!("case {case:?}: the read must report a rate, got {limit}"));
    i32::try_from(reported)
        .unwrap_or_else(|_| panic!("case {case:?}: {reported} is a modelled rate"))
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

/// The limit as every reader of it renders `category` provisioned at `rate`.
fn rendered(category: &cognito_idp::state::ApiCategory, rate: i32) -> Value {
    json!({
        "LimitDefinition": definition(category.name),
        "ProvisionedLimitValue": rate,
        "FreeLimitValue": category.free_rate,
    })
}

/// Record a granted Service Quotas increase raising this account's ceiling for
/// `category` to `ceiling`, and return the ceiling now in force.
///
/// A Service Quotas increase is the only thing that moves this ceiling and is not an
/// operation of this service, so it is applied to the account's own quota state rather
/// than over the wire.
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

/// Assert that asking `category` for `requested` — the rate already in force — is served
/// and that the limit still reports `expected`.
async fn assert_served_unchanged(
    router: &MultiServiceRouter,
    case: &str,
    category: &cognito_idp::state::ApiCategory,
    requested: i32,
    expected: &Value,
) {
    let (status, payload) = provision(router, category.name, requested).await;
    assert_eq!(
        (status, payload.get("Limit")),
        (200, Some(expected)),
        "case {case:?}: asking for the rate in force ({requested}) is served and answers the \
         limit, got {status} with {payload}"
    );
    let reported = read_back(router, case, category.name).await;
    assert_eq!(
        &reported, expected,
        "case {case:?}: the limit must stand where it stood, got {reported}"
    );
}

#[tokio::test]
async fn the_rate_a_read_reports_is_served_when_it_is_sent_back() {
    // The flow a client converging on a configuration runs: read the limit and
    // send back the rate it reported. That request asks for the state the limit
    // is already in, so it is served and the limit stands where it stood. The
    // rows vary the width of
    // the account's range — none, so the rate in force is both bounds at once,
    // and one a granted increase opened — and run every API category this
    // account holds through both, so the rule is asserted over limits with
    // different included rates rather than over one.
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
            let requested = rate_in_force(&case, &baseline);
            let expected = rendered(category, requested);
            assert_eq!(
                baseline, expected,
                "case {case:?}: the rate in force is what the read reports, got {baseline}"
            );

            // Sending it back more than once: every repeat is answered the same
            // way, so the answer is the rule rather than a one-off.
            for repeat in 1..=REPEATS {
                let case = format!("{case} (apply {repeat} of {REPEATS})");
                assert_served_unchanged(&router, &case, category, requested, &expected).await;
            }
        }
    }
}

#[tokio::test]
async fn the_rate_a_provisioning_stored_is_served_when_it_is_sent_back() {
    // The same rule measured against a rate this account really provisioned
    // rather than against the included one: a granted increase opens the range,
    // a rate inside it is stored, and asking for THAT rate again is served
    // too.
    for category in cognito_idp::state::API_CATEGORIES {
        let (router, engine) = router_and_engine();
        let ceiling = granted_increase(&engine, category, category.free_rate + GRANTED_INCREASE);
        let stored = (category.free_rate + ceiling) / 2;
        let case = format!("{} provisioned at {stored}", category.name);

        let (status, answered) = provision(&router, category.name, stored).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a rate inside the range is stored, got {status} with {answered}"
        );
        let expected = rendered(category, stored);
        assert_eq!(
            answered.get("Limit"),
            Some(&expected),
            "case {case:?}: the update must answer the limit it stored, got {answered}"
        );

        for repeat in 1..=REPEATS {
            let case = format!("{case} (re-applied {repeat} of {REPEATS})");
            assert_served_unchanged(&router, &case, category, stored, &expected).await;
        }

        // The included rate is no longer the rate in force, so asking for it is
        // an adjustment back down and is stored.
        let case = format!("{} released back to its included rate", category.name);
        let (status, answered) = provision(&router, category.name, category.free_rate).await;
        assert_eq!(
            status, 200,
            "case {case:?}: releasing the limit back to its included rate is an adjustment, \
             got {status} with {answered}"
        );
        let released = rendered(category, category.free_rate);
        assert_eq!(
            answered.get("Limit"),
            Some(&released),
            "case {case:?}: the update must answer the limit it stored, got {answered}"
        );
        let reported = read_back(&router, &case, category.name).await;
        assert_eq!(
            reported, released,
            "case {case:?}: the read must render the stored limit, got {reported}"
        );
    }
}

#[tokio::test]
async fn every_other_rate_inside_the_range_is_stored() {
    let (router, engine) = router_and_engine();

    // The rows differ along where in the account's range the rate sits: one step
    // above its floor (the rate the limit includes), the midpoint, the account's
    // ceiling, and back down to the floor. Each row asks for a rate the limit
    // does NOT hold, and the row after it asks for the rate the row before put
    // in force.
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
            let case = format!("{} asked for {requested} ({position})", category.name);
            let expected = rendered(category, requested);

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

            // The rate that was just stored is now the rate in force, so asking
            // for it again is served and moves nothing.
            let case = format!("{case}, re-applied");
            assert_served_unchanged(&router, &case, category, requested, &expected).await;
        }
    }
}

#[tokio::test]
async fn a_rate_outside_the_range_is_refused_on_the_bound_it_missed_and_stores_nothing() {
    // The control for the tests above: the rate already in force is served,
    // while a rate past the account's ceiling is a
    // quota excess and a rate under the limit's included rate is a bad
    // parameter. The rows run over every limit and over both widths of range,
    // and the over-quota rows include a rate far beyond any ceiling as well as
    // the first rate past it.
    for increase in [0, GRANTED_INCREASE] {
        for category in cognito_idp::state::API_CATEGORIES {
            let (router, engine) = router_and_engine();
            let ceiling = if increase > 0 {
                granted_increase(&engine, category, category.free_rate + increase)
            } else {
                category.free_rate
            };

            // The rate the refusals are measured against: where the increase
            // opened the range, a rate this account really provisioned; where it
            // did not, the included rate, which is the only rate the range
            // holds.
            let in_force = if increase > 0 {
                let requested = category.free_rate + 1;
                let (status, answered) = provision(&router, category.name, requested).await;
                assert_eq!(
                    status, 200,
                    "case {:?}: the rate the refusals are measured against is stored first, \
                     got {status} with {answered}",
                    category.name
                );
                requested
            } else {
                category.free_rate
            };
            let expected = rendered(category, in_force);

            let rows = [
                ("the first rate past the ceiling", ceiling + 1),
                // A rate no account's quota reaches, as a client asking for an
                // absurd rate sends it.
                ("a rate far past the ceiling", 2_000_000_000),
            ];
            for (position, requested) in rows {
                let case = format!(
                    "{} asked for {requested} ({position}, ceiling {ceiling})",
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
                        "ServiceQuotaExceededException",
                        above_ceiling_message(i64::from(requested), i64::from(ceiling))
                    ),
                    "case {case:?}: a rate past the ceiling needs a quota increase, got \
                     {status} with {payload}"
                );
                let reported = read_back(&router, &case, category.name).await;
                assert_eq!(
                    reported, expected,
                    "case {case:?}: a refused update must store nothing, got {reported}"
                );
            }

            let requested = category.free_rate - 1;
            let case = format!(
                "{} asked for {requested} (one step below the included rate)",
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
                    below_included_message(i64::from(requested), i64::from(category.free_rate))
                ),
                "case {case:?}: a rate below the included rate is a bad parameter, got {status} \
                 with {payload}"
            );
            let reported = read_back(&router, &case, category.name).await;
            assert_eq!(
                reported, expected,
                "case {case:?}: a refused update must store nothing, got {reported}"
            );
        }
    }
}
