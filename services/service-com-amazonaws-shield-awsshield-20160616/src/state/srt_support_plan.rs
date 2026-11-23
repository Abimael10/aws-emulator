// SPDX-License-Identifier: CC0-1.0

//! Whether the account may use the services of the Shield Response Team (SRT) at all:
//! the Amazon Web Services Support plan it holds.
//!
//! `AssociateDRTRole` states that precondition in its own documentation
//! (`smithy_query(query="operation",operation="AssociateDRTRole")`): "To use the services
//! of the SRT and make an AssociateDRTRole request, you must be subscribed to the Business
//! Support plan or the Enterprise Support plan." It is a fact about the ACCOUNT and not
//! about Shield Advanced: a Support plan is bought from Amazon Web Services Support, which
//! this emulator models in its own service crate, and an operator grants one out of band
//! (`SUPPORT_PLAN=enterprise`, that crate's
//! [`state::SUPPORT_PLAN_ENV`](::service_com_amazonaws_support_awssupport_20130415::state::SUPPORT_PLAN_ENV)).
//! So the question is put to the Support store's OWN field rather than kept as a second
//! copy here, and one plan serves every service gated on it. Nothing is cached: a plan
//! granted a moment ago counts at once.
//!
//! The plan decides whether such a request can EVER work, so it is judged ahead of
//! everything about Shield Advanced: an account whose plan admits no SRT access is refused
//! with the modelled `InvalidOperationException` (HTTP 400) and
//! [`SUPPORT_PLAN_INVALID_MESSAGE`], which tells a caller to change its plan, where the
//! account-level `ResourceNotFoundException` tells it to subscribe to Shield Advanced and
//! retry.
//!
//! A store this emulator cannot read at all — an engine that is not the in-memory one — is
//! EVIDENCE OF NOTHING and so refuses nothing, the convention every cross-service read of
//! this crate follows ([`super::target_resources`]).

use ::service_com_amazonaws_support_awssupport_20130415::state as support;

/// How this service states that the account's Support plan does not admit the services of
/// the Shield Response Team. Fixed text in full: what it reports is a fact about the
/// account's plan, so no member, identifier or value of the request appears in it.
pub const SUPPORT_PLAN_INVALID_MESSAGE: &str = "Operation is invalid for your AWS Support plan.";

/// The Support plans that admit the services of the SRT, each in the form
/// [`support::normalize_support_plan`] produces.
///
/// Source: the documentation of the operations that require it, carried on their headers in
/// `src/ops/` — "To use the services of the SRT and make an AssociateDRTRole request, you
/// must be subscribed to the Business Support plan or the Enterprise Support plan." Those
/// two tiers, and nothing else: every plan the sentence does not name — Basic, Developer,
/// and the Enterprise On-Ramp tier — does NOT admit them. This table is therefore Shield's
/// own and deliberately NARROWER than the gate the Support API puts on its own operations
/// (that service's `state::QUALIFYING_SUPPORT_PLANS`, which its documentation extends to
/// the On-Ramp tier): what is shared with that crate is the account's plan and the reading
/// of a plan NAME, not the entitlement, which each service's own documentation states.
///
/// Each of the two tiers is listed under the spellings its name is recorded in, the word
/// `Support` included, so a tier that entitles the account to the SRT is not refused over
/// how an operator wrote it down.
///
/// Compared through [`plan_admits_srt`] rather than against this list directly.
pub const SRT_SUPPORT_PLANS: &[&str] = &[
    "business",
    "businesssupport",
    "businesssupport+",
    "enterprise",
    "enterprisesupport",
];

/// Whether an account holding `plan` may use the services of the SRT, i.e. whether `plan`
/// names one of the [`SRT_SUPPORT_PLANS`]. The name is read the way the Support service
/// reads one, so a plan spelled `"Enterprise Support"`, `"enterprise_support"` or
/// `"AWS Enterprise Support"` is one plan.
pub fn plan_admits_srt(plan: &str) -> bool {
    SRT_SUPPORT_PLANS.contains(&support::normalize_support_plan(plan).as_str())
}

/// Whether the account may use the services of the SRT, as the Support store states its
/// plan. False for an account holding no plan at all, which is what a new Amazon Web
/// Services account has, and false for one on a plan that does not admit them.
///
/// `true` for a store this emulator cannot read, which is evidence of nothing.
pub fn srt_access_permitted<Eng: Send + Sync + 'static>(engine: &Eng) -> bool {
    match support::service_state(engine) {
        Some(state) => state
            .lock()
            .support_plan
            .as_deref()
            .is_some_and(plan_admits_srt),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plan admits the SRT when it is one of the Business or Enterprise tiers, under
    /// whichever spelling it is recorded in, and not otherwise.
    #[test]
    fn the_business_and_enterprise_tiers_are_the_plans_that_admit_the_srt() {
        // (the plan as an operator records it, whether it admits the SRT)
        let rows: &[(&str, bool)] = &[
            ("Business", true),
            ("business", true),
            ("AWS Business Support", true),
            ("Business Support+", true),
            ("Enterprise", true),
            ("Enterprise Support", true),
            ("enterprise_support", true),
            // A tier the operation's sentence does not name.
            ("Enterprise On-Ramp", false),
            ("Developer", false),
            ("Basic", false),
            ("AWS Basic Support", false),
            ("", false),
            ("enterprise-ish", false),
        ];

        for (plan, admits) in rows {
            assert_eq!(
                plan_admits_srt(plan),
                *admits,
                "case {plan:?}: expected plan_admits_srt == {admits}, got {}",
                plan_admits_srt(plan)
            );
        }
    }

    /// Every entry of the table is already in the form the comparison reduces a recorded
    /// plan name to, so no entry is unreachable.
    #[test]
    fn every_admitted_plan_is_written_in_the_form_the_comparison_produces() {
        for plan in SRT_SUPPORT_PLANS {
            assert_eq!(
                support::normalize_support_plan(plan).as_str(),
                *plan,
                "case {plan:?}: every entry must already be in normalized form"
            );
        }
    }
}
