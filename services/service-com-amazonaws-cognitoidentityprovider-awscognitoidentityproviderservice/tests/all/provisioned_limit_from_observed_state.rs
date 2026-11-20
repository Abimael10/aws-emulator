// SPDX-License-Identifier: CC0-1.0

//! Writing an account's provisioned API rate limit from the state a read of it
//! reported, driven through the router exactly as an SDK drives it (`POST /`
//! with `X-Amz-Target: AWSCognitoIdentityProviderService.<Op>`).
//!
//! The rule under test: `UpdateProvisionedLimit` "sets the provisioned limit for
//! a specific API category. The value must be between the default limit and your
//! account-level maximum limit in Service Quotas" (this service's model), so
//! every rate inside the account's range for the limit is an accepted write —
//! the rate `GetProvisionedLimit` just reported as in force included, because a
//! limit the account has provisioned nothing for holds no value of the account's
//! to collide with. The answer carries the required `Limit` structure
//! (`LimitDefinition`, `ProvisionedLimitValue` and `FreeLimitValue` are all
//! required members of `LimitType`) with the requested rate as the provisioned
//! value, and a read afterwards renders the same limit. The control rows are the
//! two refusals that remain: a repeat of a rate the account itself provisioned
//! adjusts nothing, and a rate past the account's Service Quotas ceiling is a
//! quota excess; neither stores anything.
//!
//! The rows vary the limit (every API category this account holds, whose
//! included rates differ) and the width of the account's range (none, so the
//! reported rate is both bounds at once, and one a granted quota increase
//! opened). The harness — the router, the two calls, the rendering of a limit
//! and the granted increase — is the one `provisioned_limit_reapply.rs` already
//! builds for these operations, and the refusal sentences are the ones
//! `provisioned_limits.rs` words, both reached through the test crate root
//! rather than copied; the positions inside the range are that suite's table.

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

use super::log_delivery_configuration::{error_code, error_message};
use super::provisioned_limit_reapply::{
    assert_stored, granted_increase, provision, rate_in_force, read_back, rendered,
    router_and_engine, GRANTED_INCREASE,
};
use super::provisioned_limits::{above_ceiling_message, already_in_force_message};

#[tokio::test]
async fn the_rate_a_read_reports_is_written_back_for_every_limit() {
    // The apply configuration tooling, retries and reconciliation loops
    // perform: read the limit, then write back exactly the rate the read
    // reported. The rows vary the limit and the width of the account's range,
    // so the written rate is at both bounds at once in half of them and
    // strictly inside the range in the others.
    for increase in [0, GRANTED_INCREASE] {
        for category in cognito_idp::state::API_CATEGORIES {
            let (router, engine) = router_and_engine();
            let case = format!(
                "{} written back with a ceiling {increase} above its included rate",
                category.name
            );
            if increase > 0 {
                granted_increase(&engine, category, category.free_rate + increase);
            }

            let reported = read_back(&router, &case, category.name).await;
            let observed = rate_in_force(&case, &reported);
            assert_eq!(
                observed, category.free_rate,
                "case {case:?}: a limit nothing has provisioned reports the rate it includes, \
                 got {reported}"
            );

            // The write the issue is about: the required `Limit` comes back
            // carrying the requested rate, and the read renders the same limit.
            assert_stored(&router, &case, category, observed).await;
        }
    }
}

#[tokio::test]
async fn only_a_rate_past_the_ceiling_or_a_repeat_of_the_accounts_own_is_refused() {
    // The control for the rows above: writing the reported rate back is
    // accepted, so the refusals this operation does make are the ones about
    // repeating the account's own value and about the account's quota. The rows
    // run over every limit, with the ceiling the account's own default (no
    // increase granted) so the rate past it is the first rate above the written
    // one.
    for category in cognito_idp::state::API_CATEGORIES {
        let (router, _engine) = router_and_engine();
        let case = format!("{} on a fresh account", category.name);

        let reported = read_back(&router, &case, category.name).await;
        let observed = rate_in_force(&case, &reported);
        assert_stored(&router, &case, category, observed).await;

        // Repeating the rate the account has now provisioned adjusts nothing it
        // set.
        let case = format!("{} asked for {observed} a second time", category.name);
        let (status, payload) = provision(&router, category.name, observed).await;
        assert_eq!(
            (
                status,
                error_code(&payload),
                error_message(&payload).to_string()
            ),
            (400, "InvalidParameterException", already_in_force_message()),
            "case {case:?}: repeating the account's own rate adjusts nothing, got {status} \
             with {payload}"
        );
        let after = read_back(&router, &case, category.name).await;
        assert_eq!(
            after,
            rendered(category, observed),
            "case {case:?}: a refused update must store nothing, got {after}"
        );

        // A rate past the account's ceiling needs a quota increase, whatever the
        // limit reports in force. The rows differ along how far past it reaches.
        for (position, requested) in [
            ("the first rate past the ceiling", category.free_rate + 1),
            ("a rate far past the ceiling", 2_000_000_000),
        ] {
            let case = format!("{} asked for {requested} ({position})", category.name);
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
                    above_ceiling_message(i64::from(requested), i64::from(category.free_rate))
                ),
                "case {case:?}: a rate past the ceiling is a quota excess, got {status} \
                 with {payload}"
            );
            let after = read_back(&router, &case, category.name).await;
            assert_eq!(
                after,
                rendered(category, observed),
                "case {case:?}: a refused update must store nothing, got {after}"
            );
        }
    }
}
