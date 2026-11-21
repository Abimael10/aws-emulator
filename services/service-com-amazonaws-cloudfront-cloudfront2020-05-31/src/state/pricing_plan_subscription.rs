// SPDX-License-Identifier: CC0-1.0

//! Whether a flat-rate PRICING PLAN subscription covers a distribution — the fact that
//! decides whether that distribution's web ACL may come off.
//!
//! `DistributionConfig$WebACLId` is "the WAF web ACL to associate with this distribution",
//! and an empty value names no association at all — which is how a caller removes one. A
//! distribution bought onto a flat-rate pricing plan is the exception: the plan is sold over
//! "exactly one Amazon CloudFront distribution and exactly one WAF web ACL"
//! (`CreateSubscription$resourceArns`), so the protection that pair IS has to stay in place
//! for as long as the subscription does. A write that would clear the stored `WebACLId`, or
//! point it at another web ACL, is therefore refused with the modelled `InvalidArgument`
//! (HTTP 400) while such a subscription covers the distribution, and nothing is stored: the
//! caller cancels the subscription first and then takes the web ACL off.
//!
//! WHICH resources a subscription covers is the fact of the service that SELLS the
//! subscription, so it is read in that service's own store — the one `CreateSubscription`
//! writes and `GetSubscription`/`ListSubscriptions` report — over the SAME shared engine
//! this service's handlers reach their own store through, exactly as the certificate rules
//! read the services that hold certificates ([`super::viewer_certificate`]). This service
//! therefore cannot decide it differently from the service that owns it, and a subscription
//! that service no longer holds stops gating the next write at once.
//!
//! Nothing is refused on evidence that is not there: an emulator assembled without that
//! service establishes nothing about the distribution and gates no write.
//!
//! LOCKING: this takes the other service's guard and holds nothing of this service's while
//! it does, so every caller reads it with NO lock of this store held — the rule
//! [`super::viewer_certificate`] and `crate::service::web_acl` are called under too.

use ::service_com_amazonaws_pricingplanmanager_awspricingplanmanager::state as pricing_plans;

/// The sentence CloudFront refuses a write that would take the web ACL off a subscribed
/// distribution with: both halves are fixed text — the refusal and the rule behind it —
/// naming no value from the request and none from either store. Recorded behaviour.
pub const WEB_ACL_HELD_BY_SUBSCRIPTION_MESSAGE: &str =
    "You can't remove or replace the web ACL for your distribution. Distributions with a \
     pricing plan subscription must have a web ACL resource.";

/// The ARN vendor and resource type of a CloudFront distribution:
/// `arn:aws:cloudfront::<account>:distribution/<id>`, which is how a subscription names one.
const CLOUDFRONT_VENDOR: &str = "cloudfront";
const DISTRIBUTION_RESOURCE_TYPE: &str = "distribution";

/// Whether a pricing-plan subscription covers the distribution `id` names.
///
/// `false` when no subscription names it, and when this emulator holds no pricing plan
/// service to ask at all — which establishes nothing and gates nothing.
///
/// Call it with NO lock of this service's store held: it locks the pricing plan store.
pub fn distribution_covered_by_a_pricing_plan<
    Eng: ::std::marker::Send + ::std::marker::Sync + 'static,
>(
    engine: &Eng,
    id: &str,
) -> bool {
    let Some(state) = pricing_plans::service_state(engine) else {
        return false;
    };
    let mut subscriptions = state.lock();
    // A subscription whose cancellation the clock has already applied covers nothing, so
    // that store is brought up to the clock before it is read — the same materialising of
    // what is already due every one of its own operations makes first.
    subscriptions.apply_due_changes(pricing_plans::now());
    subscriptions
        .subscriptions
        .values()
        .any(|subscription| names_distribution(&subscription.resource_arns, id))
}

/// Whether any of `resource_arns` names the distribution `id` names.
///
/// The ARN is placed by its vendor and resource type rather than compared as text, so a
/// subscription naming the distribution in another partition's or another account's
/// spelling is still naming this distribution.
fn names_distribution(resource_arns: &[String], id: &str) -> bool {
    resource_arns.iter().any(|arn| {
        ::service_common::arn::is_resource_arn_of(arn, CLOUDFRONT_VENDOR)
            && ::service_common::arn::resource_id_of_type(arn, DISTRIBUTION_RESOURCE_TYPE)
                == Some(id)
    })
}

/// `Some(message)` when a write that would leave the distribution's web ACL as `submitted`
/// has to be refused because a pricing-plan subscription covers it, and `None` when it may
/// be stored.
///
/// `covered` is [`distribution_covered_by_a_pricing_plan`]'s answer for the distribution
/// being written, `current` the `WebACLId` the stored configuration carries and `submitted`
/// the one the write would leave behind. A distribution that carries no web ACL has none to
/// keep, and a write that leaves the same one in place changes nothing — so only a REMOVAL
/// (an empty value) or a REPLACEMENT (another web ACL) of an association that is there is
/// refused, which is what the sentence states.
pub fn web_acl_held_by_a_pricing_plan(
    covered: bool,
    current: Option<&str>,
    submitted: Option<&str>,
) -> Option<&'static str> {
    let current = current.unwrap_or_default().trim();
    let submitted = submitted.unwrap_or_default().trim();
    (covered && !current.is_empty() && current != submitted)
        .then_some(WEB_ACL_HELD_BY_SUBSCRIPTION_MESSAGE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ARN forms the owning services mint, which the rows below name.
    const DISTRIBUTION: &str = "arn:aws:cloudfront::000000000000:distribution/E1EXAMPLE";
    const WEB_ACL: &str =
        "arn:aws:wafv2:us-east-1:000000000000:global/webacl/acl/11111111-2222-3333-4444-555555555555";

    /// RULE: a subscription's resource list names a distribution by its id, read out of the
    /// ARN's vendor and resource type — so another resource of the same vendor, another
    /// vendor's ARN and another distribution's id name none of it.
    #[test]
    fn a_subscription_names_a_distribution_by_its_id() {
        // (case, the resources a subscription covers, the distribution id, whether it names it)
        type Row<'a> = (&'a str, &'a [&'a str], &'a str, bool);
        let rows: &[Row<'_>] = &[
            ("the distribution and its web ACL", &[DISTRIBUTION, WEB_ACL], "E1EXAMPLE", true),
            (
                "the distribution in another partition's spelling",
                &["arn:aws-cn:cloudfront::000000000000:distribution/E1EXAMPLE"],
                "E1EXAMPLE",
                true,
            ),
            (
                "another account's distribution of the same id",
                &["arn:aws:cloudfront::111111111111:distribution/E1EXAMPLE"],
                "E1EXAMPLE",
                true,
            ),
            ("another distribution", &[DISTRIBUTION], "E2EXAMPLE", false),
            (
                "another resource of the same vendor",
                &["arn:aws:cloudfront::000000000000:key-value-store/E1EXAMPLE"],
                "E1EXAMPLE",
                false,
            ),
            ("a web ACL alone", &[WEB_ACL], "E1EXAMPLE", false),
            ("no resources at all", &[], "E1EXAMPLE", false),
        ];
        for (case, resources, id, expected) in rows {
            let resources: Vec<String> =
                resources.iter().map(|arn| (*arn).to_string()).collect();
            let actual = names_distribution(&resources, id);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected}, got {actual}"
            );
        }
    }

    /// RULE: while a subscription covers the distribution, a write that would clear or
    /// replace the web ACL it carries is refused; everything else is stored — a
    /// distribution no subscription covers, one that carries no web ACL to keep, and a
    /// write that leaves the same one in place.
    #[test]
    fn only_removing_or_replacing_a_subscribed_distributions_web_acl_is_refused() {
        // (case, whether a subscription covers it, the stored value, the submitted value,
        //  whether the write is refused)
        type Row<'a> = (&'a str, bool, Option<&'a str>, Option<&'a str>, bool);
        let other_acl =
            "arn:aws:wafv2:us-east-1:000000000000:global/webacl/other/22222222-2222-3333-4444-555555555555";
        let rows: &[Row<'_>] = &[
            ("removing it while subscribed", true, Some(WEB_ACL), Some(""), true),
            (
                "leaving the member out altogether while subscribed",
                true,
                Some(WEB_ACL),
                None,
                true,
            ),
            (
                "replacing it with another web ACL while subscribed",
                true,
                Some(WEB_ACL),
                Some(other_acl),
                true,
            ),
            (
                "the same web ACL, spelled with surrounding space",
                true,
                Some(WEB_ACL),
                Some(" \t"),
                true,
            ),
            ("keeping it while subscribed", true, Some(WEB_ACL), Some(WEB_ACL), false),
            (
                "attaching one while subscribed, where there was none",
                true,
                Some(""),
                Some(WEB_ACL),
                false,
            ),
            ("removing it while not subscribed", false, Some(WEB_ACL), Some(""), false),
            (
                "replacing it while not subscribed",
                false,
                Some(WEB_ACL),
                Some(other_acl),
                false,
            ),
            ("no web ACL either way", true, None, None, false),
        ];
        for (case, covered, current, submitted, refused) in rows {
            let actual = web_acl_held_by_a_pricing_plan(*covered, *current, *submitted);
            assert_eq!(
                actual.is_some(),
                *refused,
                "case {case:?}: expected refused == {refused}, got {actual:?}"
            );
            if *refused {
                assert_eq!(
                    actual,
                    Some(WEB_ACL_HELD_BY_SUBSCRIPTION_MESSAGE),
                    "case {case:?}: the refusal states the one fixed sentence"
                );
            }
        }
    }
}
