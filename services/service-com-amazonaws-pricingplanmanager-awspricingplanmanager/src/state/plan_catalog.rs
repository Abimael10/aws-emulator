// SPDX-License-Identifier: CC0-1.0

//! The pricing plans this service OFFERS, and how it refuses a request for one it does
//! not.
//!
//! `CreateSubscription$planFamily` is "the pricing plan family to subscribe to, such as
//! `CloudFront`" and `planTier` is "the tier level for the subscription, such as `FREE`,
//! `PRO`, `BUSINESS`, or `PREMIUM`"
//! (`smithy_query(query="shape", shape="CreateSubscriptionInput")`). Both are plain
//! strings in the model, so the boundary validator cannot narrow them: what a caller may
//! subscribe to is a BUSINESS rule, and a family or a tier the service does not offer
//! fails it. That is exactly what the modelled `ValidationException` (HTTP 400, "The
//! request failed a business rule validation") reports, so a request naming a plan that
//! is not on offer is refused here before anything is minted — a subscription to a plan
//! that does not exist would otherwise be stored and reported by every later read.
//!
//! The catalogue is resolved family first, then the tier WITHIN that family's plan: a
//! tier exists only as a level of some plan, so a tier can only be weighed once the plan
//! is known.
//!
//! WHICH plans are on offer is the emulator's own catalogue, not a list the model carries:
//! the `planFamily` doc names CloudFront by example ("such as `CloudFront`") rather than
//! exhaustively, and a plan's CODE is published nowhere in the model at all. One family is
//! documented and the other is read off the resources a subscription may cover
//! ([`ROUTE_53_PLAN_FAMILY`]), and both codes are built the one way
//! ([`FLAT_RATE_PLAN_SUFFIX`]). A family this service comes to offer is added as one more
//! [`PLAN_CATALOG`] row — its published name, its code built that same way, and the tiers
//! it sells — and needs no change to the gate, the refusals or any handler; until it is a
//! row, a request naming it is refused, which is the direction AWS refuses in for a family
//! it does not offer.

// The CloudFront family is named where its documented resource prerequisite lives, so
// the catalogue reads it from there rather than spelling it a second time.
use super::{CLOUDFRONT_PLAN_FAMILY, PLAN_TIERS_ASCENDING};
use crate::ops::types::ValidationException;

/// One pricing plan on offer: the family that names it, the code this service publishes
/// it under, and the tiers it sells.
#[derive(Debug, Clone, Copy)]
pub struct Plan {
    /// The plan family, as this service publishes it. A `planFamily` member names this
    /// value.
    pub family: &'static str,
    /// The code this plan is published under, which is how a refusal about one of its
    /// tiers names the plan. A flat-rate plan's code is the subscribed service's
    /// abbreviation followed by [`FLAT_RATE_PLAN_SUFFIX`].
    pub code: &'static str,
    /// The tiers this plan offers, lowest to highest.
    pub tiers: &'static [&'static str],
}

/// What every flat-rate plan's code ends in: this service "manages FLAT-RATE pricing
/// subscriptions for supported services", and "with a flat-rate pricing subscription,
/// you pay a fixed recurring fee for eligible resources instead of usage-based pricing"
/// (`smithy_query(query="doc",shape="AWSPricingPlanManager")`), so a plan's code is the
/// subscribed service's abbreviation and this suffix.
pub const FLAT_RATE_PLAN_SUFFIX: &str = "_FRP";

/// The code the CloudFront family's flat-rate plan is published under: CloudFront's own
/// abbreviation and [`FLAT_RATE_PLAN_SUFFIX`].
pub const CLOUDFRONT_PLAN_CODE: &str = "CF_FRP";

/// The code the Route 53 family's flat-rate plan is published under, built the same way
/// from that service's abbreviation.
pub const ROUTE_53_PLAN_CODE: &str = "R53_FRP";

/// The other plan family this service offers — the emulator's own catalogue entry, read off
/// the resources a subscription may cover rather than from any statement that the service
/// offers such a plan.
///
/// Source: `CreateSubscriptionInput`'s `resourceArns` doc — "a note: for subscriptions in
/// the CloudFront plan family, the resources must include exactly one Amazon CloudFront
/// distribution and exactly one WAF web ACL. You can also include other supported
/// resources, such as Amazon Route 53 hosted zones, and CloudFront KeyValueStores"
/// (`smithy_query(query="doc",shape="CreateSubscriptionInput",member="resourceArns")`).
/// Route 53 is the one service there whose resources are not CloudFront's own, so it is
/// the second family this emulator offers a flat-rate plan for. See the module header for
/// how a family this service comes to offer is added.
pub const ROUTE_53_PLAN_FAMILY: &str = "Route53";

/// Every plan on offer.
///
/// Each plan sells the published tier ladder ([`PLAN_TIERS_ASCENDING`], from the
/// `planTier` doc), which is what a `planTier` is weighed against once its family has
/// resolved.
pub const PLAN_CATALOG: &[Plan] = &[
    Plan {
        family: CLOUDFRONT_PLAN_FAMILY,
        code: CLOUDFRONT_PLAN_CODE,
        tiers: PLAN_TIERS_ASCENDING,
    },
    Plan {
        family: ROUTE_53_PLAN_FAMILY,
        code: ROUTE_53_PLAN_CODE,
        tiers: PLAN_TIERS_ASCENDING,
    },
];

/// The plan `plan_family` names, or `None` when no plan of that family is on offer.
///
/// The family is matched ignoring ASCII case, the same way the resource-eligibility gate
/// places a request's family ([`super::resource_eligibility`]): the member is a plain
/// string, so the family is identified by its name rather than by an enum the boundary
/// validator would have narrowed.
pub fn plan(plan_family: &str) -> Option<&'static Plan> {
    PLAN_CATALOG
        .iter()
        .find(|plan| plan.family.eq_ignore_ascii_case(plan_family))
}

impl Plan {
    /// Whether this plan offers `tier`.
    ///
    /// The published tiers are upper-case tokens, so they are matched exactly: a tier is
    /// a level this plan sells or it is not one at all.
    pub fn offers_tier(&self, tier: &str) -> bool {
        self.tiers.contains(&tier)
    }
}

/// How this service refuses a `planFamily` no plan is offered for: the modelled
/// `ValidationException` (HTTP 400), naming the family the request carried.
///
/// The variable part is the requested family, copied from the request, so a caller sees
/// the name it sent back.
pub fn invalid_plan_name(plan_family: &str) -> ValidationException {
    ValidationException {
        message: format!("Invalid plan name: {plan_family}"),
        // `resourceId` is "the identifier of the resource that failed validation", and a
        // plan family identifies no resource of this service.
        resource_id: None,
    }
}

/// How this service refuses a `planTier` the resolved plan does not offer: the modelled
/// `ValidationException` (HTTP 400), naming the rejected tier and the CODE of the plan it
/// was asked for.
pub fn invalid_plan_tier(plan: &Plan, plan_tier: &str) -> ValidationException {
    ValidationException {
        message: format!(
            "Invalid plan tier \"{plan_tier}\" for the plan \"{}\"",
            plan.code
        ),
        resource_id: None,
    }
}

/// Resolve the plan a request names, and the tier within it: the gate every operation
/// that takes a `planFamily`/`planTier` pair runs before it writes anything.
///
/// The family resolves first, because a tier is a level of a plan.
pub fn resolve(plan_family: &str, plan_tier: &str) -> Result<&'static Plan, ValidationException> {
    let Some(plan) = plan(plan_family) else {
        return Err(invalid_plan_name(plan_family));
    };
    if !plan.offers_tier(plan_tier) {
        return Err(invalid_plan_tier(plan, plan_tier));
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A family is resolved by name ignoring ASCII case, and nothing outside the
    /// catalogue resolves at all.
    #[test]
    fn only_an_offered_family_resolves() {
        for plan in PLAN_CATALOG {
            for spelling in [
                plan.family.to_string(),
                plan.family.to_ascii_lowercase(),
                plan.family.to_ascii_uppercase(),
            ] {
                let resolved = super::plan(&spelling);
                assert_eq!(
                    resolved.map(|found| found.family),
                    Some(plan.family),
                    "case {spelling:?}: expected the {} plan, got {:?}",
                    plan.family,
                    resolved.map(|found| found.family)
                );
            }
        }
        for absent in [
            "",
            " CloudFront",
            "CloudFront ",
            "CloudFronts",
            "Cloud Front",
            "NotARealPlanFamily",
            "S3",
        ] {
            assert!(
                super::plan(absent).is_none(),
                "case {absent:?}: no plan is offered for this family"
            );
        }
    }

    /// Every plan offers the published ladder and nothing else, and the tier is matched
    /// exactly rather than by case.
    #[test]
    fn a_plan_offers_the_published_tiers_only() {
        for plan in PLAN_CATALOG {
            for tier in PLAN_TIERS_ASCENDING {
                assert!(
                    plan.offers_tier(tier),
                    "case {}/{tier}: the published ladder must be on offer",
                    plan.family
                );
            }
            for absent in ["", "free", "Pro", "ENTERPRISE", "NotARealPlanTier", "FREE "] {
                assert!(
                    !plan.offers_tier(absent),
                    "case {}/{absent:?}: this tier is not on offer",
                    plan.family
                );
            }
        }
    }

    /// The refusals state the fixed text and copy the rejected value from the request;
    /// the tier refusal names the plan as the catalogue publishes it.
    #[test]
    fn the_refusals_name_the_rejected_value() {
        for family in ["NotARealPlanFamily", "s3", "", "CloudFront!"] {
            let refused = resolve(family, "PRO").expect_err("the family is not on offer");
            assert_eq!(
                refused.message,
                format!("Invalid plan name: {family}"),
                "case {family:?}: unexpected message"
            );
            assert_eq!(
                refused.resource_id, None,
                "case {family:?}: a plan family identifies no resource"
            );
        }
        for (family, expected_plan) in [
            ("CloudFront", CLOUDFRONT_PLAN_CODE),
            ("cloudfront", CLOUDFRONT_PLAN_CODE),
            ("Route53", ROUTE_53_PLAN_CODE),
            ("ROUTE53", ROUTE_53_PLAN_CODE),
        ] {
            for tier in ["NotARealPlanTier", "", "pro"] {
                let refused = resolve(family, tier).expect_err("the tier is not on offer");
                assert_eq!(
                    refused.message,
                    format!("Invalid plan tier \"{tier}\" for the plan \"{expected_plan}\""),
                    "case {family:?}/{tier:?}: unexpected message"
                );
            }
            let resolved = resolve(family, "BUSINESS").expect("the tier is on offer");
            assert_eq!(
                resolved.code, expected_plan,
                "case {family:?}: unexpected plan"
            );
        }
    }

    /// Every plan's code is published the one way: the subscribed service's abbreviation
    /// and the flat-rate suffix, and no two plans share a code or a family.
    #[test]
    fn every_plan_code_is_built_the_same_way() {
        for plan in PLAN_CATALOG {
            assert!(
                plan.code.ends_with(FLAT_RATE_PLAN_SUFFIX),
                "case {}: the plan code {:?} must end in {FLAT_RATE_PLAN_SUFFIX}",
                plan.family,
                plan.code
            );
            let abbreviation = plan
                .code
                .strip_suffix(FLAT_RATE_PLAN_SUFFIX)
                .expect("the suffix is there");
            assert!(
                !abbreviation.is_empty()
                    && abbreviation
                        .bytes()
                        .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit()),
                "case {}: {abbreviation:?} must be the service's abbreviation",
                plan.family
            );
            assert_eq!(
                PLAN_CATALOG
                    .iter()
                    .filter(|other| other.code == plan.code || other.family == plan.family)
                    .count(),
                1,
                "case {}: a plan's family and code are its own",
                plan.family
            );
        }
    }
}
