// SPDX-License-Identifier: CC0-1.0

//! `GetProvisionedLimit` and `UpdateProvisionedLimit` over the wire, driven
//! through the router exactly as an SDK drives them (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.<Op>`).
//!
//! The rule under test: an account holds one request-rate limit per API
//! category, and a limit is identified by its class and the attributes of that
//! class (`LimitClass` `API_CATEGORY` plus the `Category` attribute).
//! `GetProvisionedLimit` reports the rate in force for the identified limit
//! beside the rate that category includes at no additional cost — the included
//! rate itself while nothing has adjusted the limit — and changes nothing.
//! `UpdateProvisionedLimit` stores EVERY rate in the account's range that
//! ADJUSTS the limit, the range running from the limit's included rate up to
//! the account's Service Quotas ceiling for that limit. The rate the limit
//! already reports as in force — whether a previous provisioning stored it or
//! it is the rate the category includes — adjusts nothing and is refused as
//! such, and so is every rate OUTSIDE the range: below the included rate as a
//! malformed parameter, above the ceiling as a quota excess naming the rate
//! asked for and the ceiling it passed. A refused request stores nothing.
//!
//! Stated plainly, because it decides how the tests below reach a provisioned
//! rate DIFFERENT from the one in force: that ceiling is a Service Quotas value
//! of the ACCOUNT, and while no increase has raised it, it IS the limit's
//! included rate — so a quota increase is what opens the range above the rate in
//! force. A granted increase is recorded in this service's own account state
//! (`State::raise_api_category_quota_ceiling`), which is what the write tests
//! below do before they provision, so each one really stores a rate the limit
//! did not hold and reads it back.
//!
//! Each table row names the case it asserts and every assertion reports the
//! actual value. The wire harness (the `call`/`ok` pair and the error readers)
//! is the one `log_delivery_configuration.rs` already builds for this service,
//! reached through the test crate root rather than copied; this suite mounts its
//! own router so it holds the engine the account's quota state lives in.

use serde_json::{json, Value};
use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

use super::log_delivery_configuration::{call, error_code, error_message, ok};

/// One API rate limit this account holds: the `Category` attribute that
/// identifies it and the rate it includes at no additional cost, in requests
/// per second.
///
/// Hand-written from the request-rate quotas Amazon Cognito publishes per API
/// category (Amazon Cognito Developer Guide, "Quotas in Amazon Cognito"); the
/// model names `UserAuthentication` and `UserCreation` on
/// `LimitDefinitionType.Attributes`
/// (`smithy_query(query="doc",shape="LimitDefinitionType",member="Attributes")`)
/// and `UserFederation` on `UpdateProvisionedLimit`, and enumerates no category
/// value at all, so the service's own list is what identifies a limit. This
/// table is a second copy of that list, so that a change to the service's
/// catalogue (`API_CATEGORIES` in `src/state.rs`) has to be deliberate:
/// `the_catalogue_and_this_suites_table_hold_the_same_limits` below detects
/// drift between the two, while the behavioural coverage is the rest of this
/// suite running over every row.
struct Limit {
    category: &'static str,
    included: i64,
}

/// Every limit this account holds, including categories no issue cited. Their
/// included rates differ, which is what the rule generalises over.
const LIMITS: &[Limit] = &[
    Limit {
        category: "UserAuthentication",
        included: 120,
    },
    Limit {
        category: "UserFederation",
        included: 25,
    },
    Limit {
        category: "UserCreation",
        included: 50,
    },
    Limit {
        category: "UserRead",
        included: 120,
    },
    Limit {
        category: "UserUpdate",
        included: 25,
    },
    Limit {
        category: "UserAccountRecovery",
        included: 30,
    },
    Limit {
        category: "UserResourceRead",
        included: 50,
    },
];

/// How far above a limit's included rate the suite raises the account's Service
/// Quotas ceiling for it before provisioning: wide enough that a rate strictly
/// inside the range exists, and the same for every category so the range's width
/// is not what a row varies.
const GRANTED_INCREASE: i64 = 500;

/// One emulator with just this service mounted, beside the engine its state —
/// including the account's Service Quotas ceilings — lives in.
fn router_and_engine() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(Arc::clone(&engine)));
    router.set_request_authentication(false);
    (router, engine)
}

/// Record a granted Service Quotas increase raising this account's ceiling for
/// `limit` to `ceiling`, and return the ceiling now in force.
///
/// What a Service Quotas increase does to the account, which is the only thing
/// that moves this ceiling: it is not an operation of this service, so it is
/// applied to the account's own quota state rather than over the wire.
fn granted_increase(engine: &Arc<engine::InMemoryEngine>, limit: &Limit, ceiling: i64) -> i64 {
    let category = cognito_idp::state::API_CATEGORIES
        .iter()
        .find(|candidate| candidate.name == limit.category)
        .unwrap_or_else(|| panic!("{} is one of this service's API categories", limit.category));
    let service_state =
        cognito_idp::state::service_state(&**engine).expect("this service's state");
    let mut guard = service_state.lock();
    guard.raise_api_category_quota_ceiling(
        category,
        i32::try_from(ceiling).expect("a ceiling within the modelled rate type"),
    );
    i64::from(guard.api_category_quota_ceiling(category))
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
async fn provision(router: &MultiServiceRouter, category: &str, requested: i64) -> (u16, Value) {
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

/// The limit a reader renders for `category` once `provisioned` is in force.
fn rendered(limit: &Limit, provisioned: i64) -> Value {
    json!({
        "LimitDefinition": definition(limit.category),
        "ProvisionedLimitValue": provisioned,
        "FreeLimitValue": limit.included,
    })
}

#[tokio::test]
async fn a_limit_nothing_has_adjusted_reports_its_included_rate() {
    let (router, _engine) = router_and_engine();

    for limit in LIMITS {
        let case = format!("a fresh account's {} limit", limit.category);
        let reported = read_back(&router, &case, limit.category).await;
        assert_eq!(
            reported,
            rendered(limit, limit.included),
            "case {case:?}: an unadjusted limit reports the rate it includes as the rate in \
             force, got {reported}"
        );

        // The read is a read: a second one answers the same limit.
        let again = read_back(&router, &case, limit.category).await;
        assert_eq!(
            again, reported,
            "case {case:?}: reading twice must answer the same limit, got {again}"
        );
    }
}

#[tokio::test]
async fn an_adjusted_rate_is_what_every_reader_renders() {
    let (router, engine) = router_and_engine();

    for limit in LIMITS {
        // A granted quota increase opens the range above the rate in force, so
        // every row below asks for a rate the limit did NOT hold and the
        // read-back afterwards can only answer it if the update stored it.
        let ceiling = granted_increase(&engine, limit, limit.included + GRANTED_INCREASE);
        assert_eq!(
            ceiling,
            limit.included + GRANTED_INCREASE,
            "case {:?}: the granted increase must be the ceiling in force, got {ceiling}",
            limit.category
        );

        // The rows differ along where in the account's range the rate sits:
        // both bounds, rates inside it, and a release back to the included
        // rate. Every row asks for a rate the limit does NOT hold, which is
        // what an adjustment is.
        let midpoint = (limit.included + ceiling) / 2;
        let rows: Vec<(String, i64)> = vec![
            ("one above the included rate".to_string(), limit.included + 1),
            ("a rate inside the range".to_string(), midpoint),
            ("one above that rate".to_string(), midpoint + 1),
            ("the account's ceiling".to_string(), ceiling),
            ("back down to the included rate".to_string(), limit.included),
        ];

        for (position, requested) in rows {
            let case = format!("{} provisioned at {requested} ({position})", limit.category);
            let (status, answered) = provision(&router, limit.category, requested).await;
            assert_eq!(
                status, 200,
                "case {case:?}: a rate inside the account's range is stored, got {status} \
                 with {answered}"
            );
            assert_eq!(
                answered.get("Limit"),
                Some(&rendered(limit, requested)),
                "case {case:?}: the update must answer the limit it stored, got {answered}"
            );

            // The stored rate is what the reader renders, so the adjustment
            // round-trips rather than living in the response alone.
            let reported = read_back(&router, &case, limit.category).await;
            assert_eq!(
                reported,
                rendered(limit, requested),
                "case {case:?}: the read must render the stored limit, got {reported}"
            );
        }
    }
}

#[tokio::test]
async fn each_limit_holds_its_own_provisioned_rate() {
    let (router, engine) = router_and_engine();

    // Two limits with different included rates. The quota increase is granted
    // for the FIRST one only, so the rate it is provisioned at is above the
    // second limit's own ceiling.
    let (first, second) = (&LIMITS[0], &LIMITS[1]);
    let ceiling = granted_increase(&engine, first, first.included + GRANTED_INCREASE);
    let requested = ceiling;

    let (status, answered) = provision(&router, first.category, requested).await;
    assert_eq!(
        status, 200,
        "case {:?}: provisioning one limit succeeds, got {status} with {answered}",
        first.category
    );

    let case = format!(
        "{} after {} was provisioned",
        second.category, first.category
    );
    let reported = read_back(&router, &case, second.category).await;
    assert_eq!(
        reported,
        rendered(second, second.included),
        "case {case:?}: provisioning another limit must leave this one at its included rate, \
         got {reported}"
    );

    // The increase was granted for the first limit alone, so the same rate on
    // the second limit is above ITS ceiling and refused rather than carried
    // over.
    let case = format!("{} asked for {}'s rate", second.category, first.category);
    let (status, payload) = provision(&router, second.category, requested).await;
    assert_eq!(
        (
            status,
            error_code(&payload),
            error_message(&payload).to_string()
        ),
        (
            400,
            "ServiceQuotaExceededException",
            above_ceiling_message(requested, second.included)
        ),
        "case {case:?}: each limit has its own ceiling, got {status} with {payload}"
    );

    let case = format!("{} itself", first.category);
    let reported = read_back(&router, &case, first.category).await;
    assert_eq!(
        reported,
        rendered(first, requested),
        "case {case:?}: the provisioned limit reports its own rate, got {reported}"
    );
    assert_ne!(
        requested, first.included,
        "case {case:?}: the rate asserted above must differ from the one a fresh limit reports"
    );
}

#[tokio::test]
async fn a_rate_outside_the_accounts_range_is_refused_and_stores_nothing() {
    let (router, engine) = router_and_engine();

    for limit in LIMITS {
        // The account's ceiling for this limit, raised by a granted increase so
        // that the baseline below is a rate the limit did not already hold and
        // the ceiling the refusals name is not the included rate either.
        let ceiling = granted_increase(&engine, limit, limit.included + GRANTED_INCREASE);

        // Each row is a rate outside the account's range for this limit, with
        // the error the bound it missed carries: below the included rate is a
        // malformed parameter, above the account's Service Quotas ceiling needs
        // a quota increase. Each bound is covered one step past it, plus no
        // rate at all, a negative rate, and a rate far above the ceiling.
        let rows: Vec<(String, i64, &str, String)> = vec![
            (
                "one below the included rate".to_string(),
                limit.included - 1,
                "InvalidParameterException",
                below_included_message(limit.included - 1, limit.included),
            ),
            (
                "no rate at all".to_string(),
                0,
                "InvalidParameterException",
                below_included_message(0, limit.included),
            ),
            (
                "a negative rate".to_string(),
                -1,
                "InvalidParameterException",
                below_included_message(-1, limit.included),
            ),
            (
                "one above the account's ceiling".to_string(),
                ceiling + 1,
                "ServiceQuotaExceededException",
                above_ceiling_message(ceiling + 1, ceiling),
            ),
            (
                "far above the account's ceiling".to_string(),
                2_000_000_000,
                "ServiceQuotaExceededException",
                above_ceiling_message(2_000_000_000, ceiling),
            ),
        ];

        // A rate in force that a refusal has to leave alone, and one no fresh
        // limit reports, so the reads below are about the write and not about
        // an untouched default.
        let stored = limit.included + 1;
        let (status, answered) = provision(&router, limit.category, stored).await;
        assert_eq!(
            status, 200,
            "case {:?}: the rate every refusal must leave alone is stored first, got {status} \
             with {answered}",
            limit.category
        );
        let before = read_back(
            &router,
            &format!("{} before any refusal", limit.category),
            limit.category,
        )
        .await;
        assert_eq!(
            before,
            rendered(limit, stored),
            "case {:?}: the baseline every refusal is measured against must be the stored \
             rate rather than the included one, got {before}",
            limit.category
        );

        for (position, requested, expected_code, expected_message) in rows {
            let case = format!("{} asked for {requested} ({position})", limit.category);
            let (status, payload) = provision(&router, limit.category, requested).await;
            assert_eq!(
                (status, error_code(&payload)),
                (400, expected_code),
                "case {case:?}: expected a 400 {expected_code}, got {status} with {payload}"
            );
            let message = error_message(&payload);
            assert_eq!(
                message, expected_message,
                "case {case:?}: expected {expected_message:?}, got {message:?}"
            );

            // The refusal is judged before anything is written, so the limit
            // still reports the rate it held.
            let after = read_back(&router, &case, limit.category).await;
            assert_eq!(
                after, before,
                "case {case:?}: a refused update must leave the limit unchanged, got {after}"
            );
        }
    }
}

#[tokio::test]
async fn a_rate_under_the_floor_is_refused_naming_the_rate_asked_for_and_the_free_limit() {
    let (router, engine) = router_and_engine();

    for limit in LIMITS {
        // The floor of the range is the rate the limit INCLUDES, which every
        // reader of the limit reports as its `FreeLimitValue` — "the default
        // (free) limit value, in requests per second (RPS). This is the rate
        // included at no additional cost" (this service's model) — so the bound
        // the refusal has to name is read off the limit itself rather than
        // taken from this suite's table alone.
        let published = read_back(
            &router,
            &format!("{} before anything is provisioned", limit.category),
            limit.category,
        )
        .await;
        let free = published
            .get("FreeLimitValue")
            .and_then(Value::as_i64)
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: a limit must report its free value: {published}",
                    limit.category
                )
            });

        // A granted increase opens the range, so a rate ABOVE the floor can be
        // put in force: the floor a refusal names is the limit's free value and
        // not the rate the account currently holds, which this makes visible by
        // asking for a sub-floor rate while a HIGHER rate is provisioned.
        granted_increase(&engine, limit, limit.included + GRANTED_INCREASE);
        let in_force = free + 2;
        let (status, answered) = provision(&router, limit.category, in_force).await;
        assert_eq!(
            status, 200,
            "case {:?}: the rate in force while the sub-floor rows run is stored first, got \
             {status} with {answered}",
            limit.category
        );

        // Each row is a rate under the floor, differing along how far under it
        // reaches: one step, and the lowest rate the modelled integer holds.
        // The rates in between — no rate at all and a negative one — are the
        // rows `a_rate_outside_the_accounts_range_is_refused_and_stores_nothing`
        // above already drives over every category, so they are not repeated.
        for (position, requested) in [
            ("one step under the free limit", free - 1),
            (
                "the lowest rate the modelled integer holds",
                i64::from(i32::MIN),
            ),
        ] {
            let case = format!("{} asked for {requested} ({position})", limit.category);
            let (status, payload) = provision(&router, limit.category, requested).await;
            assert_eq!(
                (status, error_code(&payload)),
                (400, "InvalidParameterException"),
                "case {case:?}: expected a 400 InvalidParameterException, got {status} \
                 with {payload}"
            );
            let message = error_message(&payload);
            let expected = below_included_message(requested, free);
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );

            // A caller that reads the bound out of the sentence gets the
            // limit's free value, so the sentence ends with that bound and
            // nothing after it.
            let stated_bound = message
                .rsplit(' ')
                .next()
                .and_then(|word| word.parse::<i64>().ok());
            assert_eq!(
                stated_bound,
                Some(free),
                "case {case:?}: the refusal must end with the free limit {free}, got {message:?}"
            );

            // The refusal is judged before anything is written, so the rate in
            // force is the one the account already held.
            let after = read_back(&router, &case, limit.category).await;
            assert_eq!(
                after,
                rendered(limit, in_force),
                "case {case:?}: a refused update must leave the limit unchanged, got {after}"
            );
        }
    }
}

#[tokio::test]
async fn an_account_with_no_increase_has_the_included_rate_as_its_ceiling() {
    let (router, _engine) = router_and_engine();

    // No increase is granted anywhere in this test, so each limit's ceiling is
    // the value Service Quotas starts its quota at — the rate the limit
    // includes. The rows differ along how far past that ceiling the request
    // reaches: one step, a round number, and a value far beyond any rate.
    for limit in LIMITS {
        for (position, requested) in [
            ("one step past it", limit.included + 1),
            ("ten times it", limit.included * 10),
            ("far beyond any rate", 2_000_000_000),
        ] {
            let case = format!("{} asked for {requested} ({position})", limit.category);
            let (status, payload) = provision(&router, limit.category, requested).await;
            assert_eq!(
                (
                    status,
                    error_code(&payload),
                    error_message(&payload).to_string()
                ),
                (
                    400,
                    "ServiceQuotaExceededException",
                    above_ceiling_message(requested, limit.included)
                ),
                "case {case:?}: with no increase granted the ceiling is the included rate, \
                 got {status} with {payload}"
            );

            // The refusal wrote nothing: the limit still reports the rate it
            // held.
            let reported = read_back(&router, &case, limit.category).await;
            assert_eq!(
                reported,
                rendered(limit, limit.included),
                "case {case:?}: a refused update must store nothing, got {reported}"
            );
        }

        // The ceiling itself is not PAST the ceiling, so the refusals above are
        // about passing it rather than about the operation refusing everything.
        // While no increase has been granted that ceiling is both bounds of the
        // range at once AND the rate in force, so asking for it adjusts nothing
        // and is refused for being unchanged rather than for missing a bound.
        let case = format!("{} asked for its ceiling", limit.category);
        let (status, payload) = provision(&router, limit.category, limit.included).await;
        assert_eq!(
            (
                status,
                error_code(&payload),
                error_message(&payload).to_string()
            ),
            (
                400,
                "InvalidParameterException",
                already_in_force_message()
            ),
            "case {case:?}: the rate at both bounds of the range is the rate in force, got \
             {status} with {payload}"
        );
        let reported = read_back(&router, &case, limit.category).await;
        assert_eq!(
            reported,
            rendered(limit, limit.included),
            "case {case:?}: a refused update must store nothing, got {reported}"
        );
    }
}

#[tokio::test]
async fn the_rate_a_read_reports_is_refused_when_it_is_re_applied() {
    // The two states a limit reports a rate in force from, which is what the
    // rows vary: one nothing has provisioned, where the rate in force is the
    // rate the category includes, and one a provisioning stored a rate on,
    // where it is that stored rate. Either way the value a read REPORTS is the
    // rate already in force, so an update asking for it again adjusts nothing
    // and is refused with the limit left where it stood. Every limit this
    // account holds is driven through both, so the rule is asserted over
    // categories with different included rates rather than over one. The
    // neighbouring rate is the control: it IS an adjustment and is stored.
    for provisioned_first in [false, true] {
        let (router, engine) = router_and_engine();
        let state = if provisioned_first {
            "after a provisioning stored a rate"
        } else {
            "with nothing provisioned"
        };

        for limit in LIMITS {
            let case = format!("{} {state}", limit.category);

            if provisioned_first {
                // A granted increase opens the range, so a rate the limit did
                // not hold can be stored and become the rate in force.
                let ceiling = granted_increase(&engine, limit, limit.included + GRANTED_INCREASE);
                let requested = (limit.included + ceiling) / 2;
                let (status, answered) = provision(&router, limit.category, requested).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the rate the re-apply below is measured against is stored \
                     first, got {status} with {answered}"
                );
            }

            let reported = read_back(&router, &case, limit.category).await;
            let in_force = reported["ProvisionedLimitValue"].as_i64().unwrap_or_else(|| {
                panic!("case {case:?}: the read must report a rate in force, got {reported}")
            });

            let (status, payload) = provision(&router, limit.category, in_force).await;
            assert_eq!(
                (
                    status,
                    error_code(&payload),
                    error_message(&payload).to_string()
                ),
                (
                    400,
                    "InvalidParameterException",
                    already_in_force_message()
                ),
                "case {case:?}: re-applying the reported rate {in_force} adjusts nothing, \
                 got {status} with {payload}"
            );

            // The refusal wrote nothing, so the limit still reports that rate.
            let after = read_back(&router, &case, limit.category).await;
            assert_eq!(
                after, reported,
                "case {case:?}: the limit must stand at {in_force}, got {after}"
            );

            if provisioned_first {
                // The next rate up is a rate the limit does NOT hold, and the
                // increase put it inside the account's range, so the refusal
                // above is about the value being unchanged rather than about
                // the operation refusing every write.
                let (status, answered) = provision(&router, limit.category, in_force + 1).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the next rate up is still an adjustment, got {status} \
                     with {answered}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_definition_that_identifies_no_limit_is_refused_by_both_operations() {
    let (router, _engine) = router_and_engine();

    // A definition that identifies no limit is a definition the request has to
    // correct, whichever part of it names nothing: a category value the service
    // publishes no limit of, and attributes that name no category at all, carry
    // the same class and differ only in the sentence.
    let rows: Vec<(String, Value, u16, &str, String)> = vec![
        (
            "a category the service publishes no limit of".to_string(),
            definition("UserDeletion"),
            400,
            "InvalidParameterException",
            unknown_category_message("UserDeletion"),
        ),
        (
            "a category differing only in case".to_string(),
            definition("usercreation"),
            400,
            "InvalidParameterException",
            unknown_category_message("usercreation"),
        ),
        (
            "the empty category".to_string(),
            definition(""),
            400,
            "InvalidParameterException",
            unknown_category_message(""),
        ),
        (
            "attributes that name no category".to_string(),
            json!({ "LimitClass": "API_CATEGORY", "Attributes": {} }),
            400,
            "InvalidParameterException",
            missing_attribute_message(),
        ),
        (
            "attributes naming something else".to_string(),
            json!({
                "LimitClass": "API_CATEGORY",
                "Attributes": { "UserPoolId": "us-east-1_example" },
            }),
            400,
            "InvalidParameterException",
            missing_attribute_message(),
        ),
    ];

    for (position, limit_definition, expected_status, expected_code, expected_message) in rows {
        for operation in ["GetProvisionedLimit", "UpdateProvisionedLimit"] {
            let case = format!("{operation} with {position}");
            let mut request = json!({ "LimitDefinition": limit_definition });
            if operation == "UpdateProvisionedLimit" {
                request["RequestedLimitValue"] = json!(1_000);
            }
            let (status, payload) = call(&router, operation, &request).await;
            assert_eq!(
                (status, error_code(&payload)),
                (expected_status, expected_code),
                "case {case:?}: expected a {expected_status} {expected_code}, got {status} \
                 with {payload}"
            );
            let message = error_message(&payload);
            assert_eq!(
                message, expected_message,
                "case {case:?}: expected {expected_message:?}, got {message:?}"
            );
        }
    }
}

#[test]
fn the_catalogue_and_this_suites_table_hold_the_same_limits() {
    // Drift detection, not behaviour: the limits this account holds are the
    // categories Amazon Cognito publishes a request-rate quota for, and this
    // suite drives every row of its own table over the wire — so a row the
    // service's catalogue does not hold, or an entry of the catalogue no row
    // covers, means the two copies have drifted apart and the coverage below is
    // no longer the coverage it reads as.
    let tabled: Vec<(&str, i64)> = LIMITS
        .iter()
        .map(|limit| (limit.category, limit.included))
        .collect();
    let catalogued: Vec<(&str, i64)> = cognito_idp::state::API_CATEGORIES
        .iter()
        .map(|category| (category.name, i64::from(category.free_rate)))
        .collect();
    assert_eq!(
        catalogued, tabled,
        "case \"the account's API rate limits\": the catalogue and this suite's table must hold \
         the same categories with the same included rates, got {catalogued:?}"
    );
}

#[tokio::test]
async fn a_category_neighbouring_a_published_one_identifies_no_limit() {
    let (router, _engine) = router_and_engine();

    // A category is a published value itself rather than something that
    // resembles one: a published name with another word appended, the same name
    // in another case, and one with surrounding space each identify no limit, so
    // neither operation resolves them through the catalogue. The rows are built
    // around a published category the issue cited and one it did not, so the
    // rule is asserted on more than a single name.
    let mut rows: Vec<(String, String)> = Vec::new();
    for published in ["UserResourceRead", "UserAccountRecovery"] {
        rows.push((
            format!("{published} with another word appended"),
            format!("{published}Write"),
        ));
        rows.push((
            format!("{published} in upper case"),
            published.to_uppercase(),
        ));
        rows.push((
            format!("{published} surrounded by space"),
            format!(" {published} "),
        ));
    }
    rows.push((
        "a plausible but unpublished category".to_string(),
        "UserResourceDeletion".to_string(),
    ));

    for (position, category) in rows {
        for operation in ["GetProvisionedLimit", "UpdateProvisionedLimit"] {
            let case = format!("{operation} with {position}");
            let mut request = json!({ "LimitDefinition": definition(&category) });
            if operation == "UpdateProvisionedLimit" {
                request["RequestedLimitValue"] = json!(1_000);
            }
            let (status, payload) = call(&router, operation, &request).await;
            assert_eq!(
                (status, error_code(&payload)),
                (400, "InvalidParameterException"),
                "case {case:?}: expected a 400 InvalidParameterException, got {status} \
                 with {payload}"
            );
            let message = error_message(&payload);
            let expected = unknown_category_message(&category);
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
        }
    }
}

/// What a refusal says for a rate that is already the rate in force: fixed text
/// in full, naming neither the rate asked for nor the one it matched.
///
/// Shared with the sibling suite that drives the same refusal
/// (`provisioned_limit_reapply.rs`), so the sentence has one home in this test
/// crate.
pub fn already_in_force_message() -> String {
    "Requested value is the same as the current provisioned limit".to_string()
}

/// What a refusal says for a rate below the limit's included rate: the rate
/// that was asked for and the included rate it missed, which the limit reports
/// as its `FreeLimitValue`.
///
/// Shared with the sibling suite that drives the same refusal
/// (`provisioned_limit_reapply.rs`), so the sentence has one home in this test
/// crate.
pub fn below_included_message(requested: i64, included: i64) -> String {
    format!("Requested value {requested} is below the free limit of {included}")
}

/// What a refusal says for a rate above the account's Service Quotas ceiling
/// for the limit: the rate that was asked for and the ceiling it passed, and
/// nothing else.
///
/// Shared with the sibling suite that drives the same refusal
/// (`provisioned_limit_reapply.rs`), so the sentence has one home in this test
/// crate.
pub fn above_ceiling_message(requested: i64, ceiling: i64) -> String {
    format!("Requested value {requested} exceeds Service Quota ceiling of {ceiling}")
}

/// What a refusal says for a category value the service publishes no limit of:
/// the class the definition looked it up under and the value itself.
fn unknown_category_message(category: &str) -> String {
    format!("Unknown API_CATEGORY category: {category}")
}

/// What a refusal says for a definition that names no category.
fn missing_attribute_message() -> String {
    "The limit definition must include the attribute Category to identify the API_CATEGORY \
     limit it means."
        .to_string()
}
