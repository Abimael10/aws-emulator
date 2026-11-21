// SPDX-License-Identifier: CC0-1.0

//! Which `UpdateProvisionedLimit` requests the no-op refusal fires on, driven
//! through the router exactly as an SDK drives it (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.<Op>`).
//!
//! The rule under test: `UpdateProvisionedLimit` "sets the provisioned limit for
//! a specific API category. The value must be between the default limit and your
//! account-level maximum limit in Service Quotas" (this service's model), so
//! every rate from the limit's default rate — the one it reports as
//! `FreeLimitValue`, "the rate included at no additional cost" — up to the
//! account's Service Quotas ceiling is an accepted write. The one request that
//! adjusts nothing, and is refused as a malformed parameter with a sentence that
//! quotes no value back, is a repeat of the rate THIS ACCOUNT provisioned for
//! that limit: the gate is keyed on the account's own stored adjustment, not on
//! the rate a read happens to report, because a limit the account has adjusted
//! nothing on reports the category's default rate, which the documentation above
//! makes the bottom of the accepted range. A refused request stores nothing.
//!
//! The suite asserts that predicate as a MATRIX, so both directions of the gate
//! are pinned in one place: the rows are every state the limit can be in before
//! the probe (nothing provisioned, and each position of the range provisioned)
//! crossed with every rate the probe can ask for (each position of the range),
//! over every API category this account holds — whose default rates differ. A
//! cell is refused exactly when the rate asked for is the rate the account
//! itself put in force, and stored otherwise; every cell reads the limit back.
//!
//! The harness — the router that holds the account's quota state, the two calls,
//! the rendering of a limit and the granted Service Quotas increase — is the one
//! `provisioned_limit_reapply.rs` already builds for these operations, and the
//! refusal sentence is the one `provisioned_limits.rs` words, both reached
//! through the test crate root rather than copied.

use serde_json::Value;
use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;
use smithy_server_runtime::MultiServiceRouter;

use super::log_delivery_configuration::{error_code, error_message};
use super::provisioned_limit_reapply::{
    granted_increase, provision, read_back, rendered, router_and_engine, GRANTED_INCREASE,
};
use super::provisioned_limits::already_in_force_message;

/// What this account has put in force for a limit before the probe runs.
#[derive(Debug, Clone, Copy)]
enum Standing {
    /// Nothing: the account has adjusted this limit at all, so the rate it
    /// reports in force is the category's default rate.
    Unadjusted,
    /// A rate this account provisioned, which the limit now reports in force.
    Provisioned(i32),
}

impl Standing {
    /// The rate the limit reports in force in this state.
    fn rate_in_force(self, default_rate: i32) -> i32 {
        match self {
            Self::Unadjusted => default_rate,
            Self::Provisioned(rate) => rate,
        }
    }

    /// The rate this account itself set, if any: the only rate a request can
    /// repeat without adjusting anything.
    fn provisioned(self) -> Option<i32> {
        match self {
            Self::Unadjusted => None,
            Self::Provisioned(rate) => Some(rate),
        }
    }
}

/// The positions of the account's range for a limit whose default rate is
/// `default_rate` and whose ceiling is `ceiling`, each named by the position it
/// occupies: the two bounds, one step inside the floor and the midpoint.
fn positions(default_rate: i32, ceiling: i32) -> Vec<(&'static str, i32)> {
    vec![
        ("the floor of the range (the default rate)", default_rate),
        ("one step above the floor", default_rate + 1),
        ("the midpoint of the range", (default_rate + ceiling) / 2),
        ("the account's ceiling", ceiling),
    ]
}

/// Assert that `requested` is stored for `category` and read back as the limit.
async fn assert_stored(
    router: &MultiServiceRouter,
    case: &str,
    category: &cognito_idp::state::ApiCategory,
    requested: i32,
) {
    let expected = rendered(category, requested);
    let (status, answered) = provision(router, category.name, requested).await;
    assert_eq!(
        status, 200,
        "case {case:?}: a rate the account has not provisioned is an accepted write, got \
         {status} with {answered}"
    );
    assert_eq!(
        answered.get("Limit"),
        Some(&expected),
        "case {case:?}: the update must answer the limit it stored, got {answered}"
    );
    let reported = read_back(router, case, category.name).await;
    assert_eq!(
        reported, expected,
        "case {case:?}: the read must render the stored limit, got {reported}"
    );
}

/// Assert that `requested` is refused as a rate that adjusts nothing and that
/// the limit still reports `expected`.
async fn assert_refused(
    router: &MultiServiceRouter,
    case: &str,
    category: &cognito_idp::state::ApiCategory,
    requested: i32,
    expected: &Value,
) {
    let (status, payload) = provision(router, category.name, requested).await;
    assert_eq!(
        (
            status,
            error_code(&payload),
            error_message(&payload).to_string()
        ),
        (400, "InvalidParameterException", already_in_force_message()),
        "case {case:?}: asking for the rate this account provisioned ({requested}) adjusts \
         nothing, got {status} with {payload}"
    );
    let reported = read_back(router, case, category.name).await;
    assert_eq!(
        &reported, expected,
        "case {case:?}: a refused update must store nothing, got {reported}"
    );
}

#[tokio::test]
async fn the_refusal_fires_exactly_on_a_repeat_of_the_rate_the_account_provisioned() {
    for category in cognito_idp::state::API_CATEGORIES {
        // The account's range is opened by a granted Service Quotas increase —
        // the only thing that moves that ceiling — so that positions strictly
        // inside the range exist for every limit.
        let ceiling = category.free_rate + GRANTED_INCREASE;
        let range = positions(category.free_rate, ceiling);

        let mut standings = vec![Standing::Unadjusted];
        standings.extend(range.iter().map(|(_, rate)| Standing::Provisioned(*rate)));

        for standing in standings {
            for (position, requested) in &range {
                let (router, engine) = router_and_engine();
                let granted = granted_increase(&engine, category, ceiling);
                assert_eq!(
                    granted, ceiling,
                    "case {:?}: the granted increase must be the ceiling in force, got {granted}",
                    category.name
                );

                let case = format!(
                    "{} standing at {standing:?} asked for {requested} ({position}, ceiling \
                     {ceiling})",
                    category.name
                );

                // Bring the limit into the state this row measures the probe
                // against: the account provisions that rate over the wire, so
                // the state the probe sees is one this API really produced.
                if let Some(rate) = standing.provisioned() {
                    assert_stored(&router, &format!("{case} (standing set up)"), category, rate)
                        .await;
                }

                let in_force = standing.rate_in_force(category.free_rate);
                let reported = read_back(&router, &case, category.name).await;
                assert_eq!(
                    reported,
                    rendered(category, in_force),
                    "case {case:?}: the rate in force before the probe is {in_force}, got \
                     {reported}"
                );

                // The predicate itself: only a repeat of the rate the ACCOUNT
                // set adjusts nothing. The rate a read reports for a limit the
                // account has not adjusted is the category's default rate, the
                // documented bottom of the range, so asking for it is a write.
                if standing.provisioned() == Some(*requested) {
                    assert_refused(&router, &case, category, *requested, &reported).await;
                } else {
                    assert_stored(&router, &case, category, *requested).await;
                }
            }
        }
    }
}

#[tokio::test]
async fn reading_the_limit_never_changes_which_rate_the_account_may_provision() {
    // The gate reads the account's own stored adjustment, so reading the limit —
    // any number of times — neither puts a rate in force nor turns the default
    // rate into one the account provisioned: the write that follows the reads is
    // still accepted, and only the repeat after it is refused.
    const READS: usize = 3;

    for category in cognito_idp::state::API_CATEGORIES {
        let (router, _engine) = router_and_engine();
        let case = format!("{} read {READS} times before the write", category.name);

        let unadjusted = rendered(category, category.free_rate);
        for read in 1..=READS {
            let reported = read_back(&router, &format!("{case} (read {read})"), category.name).await;
            assert_eq!(
                reported, unadjusted,
                "case {case:?}: read {read} must report the default rate in force, got {reported}"
            );
        }

        assert_stored(&router, &case, category, category.free_rate).await;
        assert_refused(
            &router,
            &format!("{case}, re-applied"),
            category,
            category.free_rate,
            &unadjusted,
        )
        .await;
    }
}
