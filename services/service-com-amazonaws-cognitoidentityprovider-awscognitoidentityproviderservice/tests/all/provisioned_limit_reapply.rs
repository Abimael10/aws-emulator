// SPDX-License-Identifier: CC0-1.0

//! Asking `UpdateProvisionedLimit` for the rate the account ALREADY has in
//! force, driven through the router exactly as an SDK drives it (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.UpdateProvisionedLimit`).
//!
//! The rule under test: `UpdateProvisionedLimit` "sets the provisioned limit for
//! a specific API category. The value must be between the default limit and your
//! account-level maximum limit in Service Quotas" (this service's model), so what
//! decides a request is the account's RANGE for the identified limit and nothing
//! else — every `RequestedLimitValue` from the limit's default (free) rate up to
//! the account's ceiling is stored and answered as the limit's
//! `ProvisionedLimitValue`, the rate the limit already reports included, whether
//! an earlier call provisioned it or it is the rate the category includes at no
//! additional cost. Applying a rate is therefore IDEMPOTENT: the flow a client
//! reconciling a configuration runs — read the limit, write the rate it reported
//! back — answers that rate rather than a refusal, however many times it repeats.
//! Only a rate outside the range is refused, above the account's Service Quotas
//! ceiling with `ServiceQuotaExceededException` and below the default rate with
//! `InvalidParameterException`, and a refused request stores nothing.
//!
//! The suite varies the rate's position in the range (its floor, one step in, the
//! midpoint, the account's ceiling), the width of the range (none, so the floor
//! and the ceiling are the same rate, and one a granted quota increase opened),
//! whether a provisioning stored the rate first or it is the included rate, how
//! many times the same value is re-applied, and the limit itself — every API
//! category this account holds, whose included rates differ. The wire harness is
//! the one `log_delivery_configuration.rs` already builds for this service,
//! reached through the test crate root rather than copied; this suite mounts its
//! own router so it holds the engine the account's quota state lives in.

use serde_json::{json, Value};
use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

use super::log_delivery_configuration::{call, error_code, error_message, ok};
// The two refusal sentences this suite's control asserts are the ones the
// sibling suite already words, so they have one home in this test crate.
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

#[tokio::test]
async fn the_rate_a_read_reports_is_stored_when_it_is_sent_back() {
    // The flow a client converging on a configuration runs: read the limit,
    // send back the rate it reported, and be answered that rate. The rows vary
    // the width of the account's range — none, so the rate in force is both
    // bounds at once, and one a granted increase opened — and run every API
    // category this account holds through both, so the rule is asserted over
    // limits with different included rates rather than over one.
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

            // Sending it back more than once: every repeat stores the same rate
            // and answers it, so the write is idempotent rather than accepted
            // once.
            for repeat in 1..=REPEATS {
                let case = format!("{case} (apply {repeat} of {REPEATS})");
                let (status, answered) = provision(&router, category.name, requested).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: re-applying the rate in force is an ordinary write, got \
                     {status} with {answered}"
                );
                assert_eq!(
                    answered.get("Limit"),
                    Some(&expected),
                    "case {case:?}: the update must answer the limit provisioned at \
                     {requested}, got {answered}"
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

#[tokio::test]
async fn every_rate_inside_the_range_is_stored_however_often_it_is_applied() {
    let (router, engine) = router_and_engine();

    // The rows differ along where in the account's range the rate sits: its
    // floor (the rate the limit includes), one step in, the midpoint, and the
    // account's ceiling. Each one is sent REPEATS times in a row: the first
    // application is an adjustment, every later one asks for the rate the
    // previous put in force — the case this suite is about — and is stored just
    // the same.
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
            let expected = rendered(category, requested);

            for repeat in 1..=REPEATS {
                let case = format!(
                    "{} asked for {requested} ({position}, apply {repeat} of {REPEATS})",
                    category.name
                );
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

#[tokio::test]
async fn only_a_rate_outside_the_range_is_refused_and_stores_nothing() {
    // The control for the two tests above: the operation is not accepting
    // everything. A rate past the account's ceiling is a quota excess and a
    // rate under the limit's included rate is a bad parameter, and neither
    // changes the rate a later read reports. The rows run over every limit and
    // over both widths of range, and the over-quota rows include a rate far
    // beyond any ceiling as well as the first rate past it.
    for increase in [0, GRANTED_INCREASE] {
        for category in cognito_idp::state::API_CATEGORIES {
            let (router, engine) = router_and_engine();
            let ceiling = if increase > 0 {
                granted_increase(&engine, category, category.free_rate + increase)
            } else {
                category.free_rate
            };

            // The rate in force is first applied through the API, so the
            // refusals below are measured against a limit this account really
            // provisioned.
            let in_force = ceiling;
            let (status, answered) = provision(&router, category.name, in_force).await;
            assert_eq!(
                status, 200,
                "case {:?}: the rate the refusals are measured against is stored first, got \
                 {status} with {answered}",
                category.name
            );
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
