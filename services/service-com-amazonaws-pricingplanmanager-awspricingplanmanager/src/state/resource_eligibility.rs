// SPDX-License-Identifier: CC0-1.0

//! Whether the resources a subscription names are ELIGIBLE for the PLAN it asks for —
//! its plan family and its plan tier.
//!
//! Two questions are answered here, and a resource has to pass both.
//!
//! The first is about the state ANOTHER service holds.
//! `CreateSubscription$resourceArns` names "the resources to include in the
//! subscription", and for one plan family the member documents what those resources are:
//! "For subscriptions in the CloudFront plan family, the resources must include exactly
//! one Amazon CloudFront distribution and exactly one WAF web ACL"
//! (`smithy_query(query="doc",shape="CreateSubscriptionInput",member="resourceArns")`).
//! A flat-rate CloudFront plan is bought over a distribution WAF is already protecting,
//! so the pair a request names has to be a REAL pair: the named distribution's own web
//! ACL association — a fact only CloudFront holds — must be the web ACL the same request
//! names. A subscription over a distribution that web ACL does not front would report
//! cover the client does not have, so such a request is refused through the modelled
//! business-rule error (`ValidationException`, HTTP 400, "The request failed a business
//! rule validation") and nothing is stored.
//!
//! The second needs nobody: within that same family, what a plan covers depends on the
//! TIER it is bought at. That same note says a subscription may also cover "other supported
//! resources, such as Amazon Route 53 hosted zones, and CloudFront KeyValueStores", and
//! those two are not covered alike: the free tier — the lowest of the published tiers
//! "`FREE`, `PRO`, `BUSINESS`, or `PREMIUM`"
//! (`smithy_query(query="doc",shape="Subscription",member="planTier")`) — carries a
//! hosted zone but not a KeyValueStore, which AWS covers only from a paid tier up. So a
//! KeyValueStore named for a free-tier CloudFront subscription is refused, while every
//! tier that is not the free one covers it.
//!
//! The two questions are refused in DIFFERENT words, because their remedies are opposite.
//! A resource the plan tier does not cover is a statement about the plan — the resource set
//! and the tier are incompatible, and the caller either drops the resource or buys a tier
//! that covers it ([`resources_incompatible_with_plan_tier_message`]), so the sentence
//! names the tier and no resource. A resource whose prerequisite association is absent is a
//! statement about those resources — the caller must fix each one, so the sentence names
//! them ([`prerequisite_unmet_message`]). Both travel in the same modelled business-rule
//! error, so the message is the only thing that tells a client which rule it broke, and
//! [`IneligibleResources::refusal`] is the one place either is worded.
//!
//! CloudFront is asked the way the framework documents a handler reaching another
//! emulated service ([`smithy_server_runtime::OperationContext::callback`]): an AWS SDK
//! client over the callback's in-process HTTP client and credentials, so nothing here
//! opens a socket or leaves the host, and the caller's own `Authorization` header is
//! never forwarded. `GetDistribution` is the read that reports a distribution's current
//! configuration, which is where its web ACL association lives. The only
//! caller-influenced value that reaches the client's AUTHORITY is the signing Region, and
//! it is narrowed before it gets there ([`service_common::arn::region_label`]), so no
//! request can re-point the client at a host of its own choosing: it reaches this
//! emulator's own CloudFront and nothing else.
//!
//! A refusal that needs an owning service is GROUNDED in what that service answered, so
//! everything else establishes nothing and refuses nothing: a resource of another vendor
//! (for which this service documents no prerequisite), a plan family with no documented
//! prerequisite, and an emulator assembled without a CloudFront to ask at all.

/// The plan family whose resources carry a documented prerequisite: "The pricing plan
/// family to subscribe to, such as `CloudFront`"
/// (`smithy_query(query="doc",shape="CreateSubscriptionInput",member="planFamily")`).
///
/// `planFamily` is a plain string in the model, so the family is identified by its name
/// ignoring ASCII case rather than by an enum the boundary validator would have checked.
pub const CLOUDFRONT_PLAN_FAMILY: &str = "CloudFront";

/// The ARN vendor of a CloudFront resource, and the resource type of a distribution and
/// of a KeyValueStore: `arn:aws:cloudfront::<account>:distribution/<id>` and
/// `arn:aws:cloudfront::<account>:key-value-store/<id>`.
const CLOUDFRONT_VENDOR: &str = "cloudfront";
const DISTRIBUTION_RESOURCE_TYPE: &str = "distribution";
const KEY_VALUE_STORE_RESOURCE_TYPE: &str = "key-value-store";

/// The ARN vendor of a WAF web ACL, and the segment its resource field carries:
/// `arn:aws:wafv2:<region>:<account>:<global|regional>/webacl/<name>/<id>`, so the
/// namespace a web ACL lives in opens the resource field and the kind follows it.
const WAFV2_VENDOR: &str = "wafv2";
const WEB_ACL_SEGMENT: &str = "/webacl/";

/// How this service states resources the plan TIER does not cover, as AWS words that
/// refusal: one fixed sentence whose only variable part is the tier the subscription is on,
/// quoted.
///
/// The refusal is about the resource SET against the plan, not about any one resource, so
/// it names no resource ARN: the remedy it states is to change the set or the tier.
pub fn resources_incompatible_with_plan_tier_message(plan_tier: &str) -> String {
    format!(
        "Resources are incompatible with plan tier '{plan_tier}': modify resources or \
         select a different tier."
    )
}

/// How this service states resources whose documented prerequisite — the association the
/// service that OWNS them must already hold — is absent: the fixed sentence, then the
/// resources that failed it, then the full stop.
///
/// The variable part is the ineligible ARNs THE REQUEST named, in the order it named
/// them, so a caller can match each one against the list it sent and fix it.
pub fn prerequisite_unmet_message(ineligible: &[String]) -> String {
    format!(
        "The following resources are not eligible for this subscription tier: {}.",
        ineligible.join(", ")
    )
}

/// The resources a request named that are NOT eligible for the plan it asks for, split by
/// WHICH of the two questions refused them, in the order the request named them.
///
/// Both halves empty is an eligible request — and also a request about which nothing of
/// the kind could be established (see the module docs).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IneligibleResources {
    /// The resources the plan TIER does not cover.
    pub outside_plan_tier: Vec<String>,
    /// The resources whose documented prerequisite association the owning service does not
    /// hold.
    pub prerequisite_unmet: Vec<String>,
}

impl IneligibleResources {
    /// Whether every named resource is eligible.
    pub fn is_empty(&self) -> bool {
        self.outside_plan_tier.is_empty() && self.prerequisite_unmet.is_empty()
    }

    /// The same finding narrowed to the resources `named` lists, keeping their order.
    ///
    /// An operation that weighs the set a subscription WOULD cover asks about resources the
    /// subscription already carries as well, and those were admitted when they joined: only
    /// what THIS request names can be refused.
    pub fn only_named(&self, named: &[String]) -> Self {
        let retain = |found: &Vec<String>| -> Vec<String> {
            found
                .iter()
                .filter(|arn| named.contains(arn))
                .cloned()
                .collect()
        };
        Self {
            outside_plan_tier: retain(&self.outside_plan_tier),
            prerequisite_unmet: retain(&self.prerequisite_unmet),
        }
    }

    /// The modelled business-rule refusal for this finding, worded for the question that
    /// refused it, or `None` when nothing was refused.
    ///
    /// `plan_tier` is the tier the request asks for, which is what a tier refusal states.
    /// A request both questions refuse is answered with the plan-tier sentence: the tier
    /// does not cover the set at all, so no prerequisite the caller could fix would admit
    /// it.
    pub fn refusal(&self, plan_tier: &str) -> Option<crate::ops::types::ValidationException> {
        if !self.outside_plan_tier.is_empty() {
            return Some(crate::ops::types::ValidationException {
                message: resources_incompatible_with_plan_tier_message(plan_tier),
                // "The identifier of the resource that failed validation" — and what failed
                // here is the set against the tier rather than one resource, which is why
                // AWS's sentence names none either.
                resource_id: None,
            });
        }
        if self.prerequisite_unmet.is_empty() {
            return None;
        }
        Some(crate::ops::types::ValidationException {
            message: prerequisite_unmet_message(&self.prerequisite_unmet),
            // "The identifier of the resource that failed validation" — which is a single
            // resource only when exactly one failed; the message carries the whole list
            // either way.
            resource_id: match self.prerequisite_unmet.as_slice() {
                [only] => Some(only.clone()),
                _ => None,
            },
        })
    }
}

/// Which of the resources in `resource_arns` are not eligible for the plan
/// `plan_family`/`plan_tier` names, and which question refused each.
///
/// `region` is the Region the request was signed for, which is the one the owning service
/// is reached in.
pub async fn ineligible_resources(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    plan_family: &str,
    plan_tier: &str,
    resource_arns: &[String],
) -> IneligibleResources {
    let mut ineligible = IneligibleResources::default();
    // Only the CloudFront family's resources are documented, so only within it is a
    // resource placed at all: every other family establishes nothing and refuses nothing.
    if !plan_family.eq_ignore_ascii_case(CLOUDFRONT_PLAN_FAMILY) {
        return ineligible;
    }
    // The web ACLs the SAME request names: the association a distribution must carry to
    // be eligible is one of these, because that is the pair the subscription covers.
    let named_web_acls: Vec<&str> = resource_arns
        .iter()
        .map(String::as_str)
        .filter(|arn| names_a_web_acl(arn))
        .collect();

    for resource_arn in resource_arns {
        // What the TIER covers needs no other service to answer.
        if !covered_by_plan_tier(plan_tier, resource_arn) {
            ineligible.outside_plan_tier.push(resource_arn.clone());
            continue;
        }
        let Some(id) = distribution_id(resource_arn) else {
            continue;
        };
        if protected_by_a_named_web_acl(ctx, region, id, &named_web_acls).await == Some(false) {
            ineligible.prerequisite_unmet.push(resource_arn.clone());
        }
    }
    ineligible
}

/// Whether the plan tier `plan_tier` names covers the resource `value` names, within the
/// plan family whose resources the model documents.
///
/// Only one resource kind is placed by the tier (see the module docs): a CloudFront
/// KeyValueStore, which the free tier does not carry and every paid tier does. A tier
/// outside the published ladder is not the free one, so it covers what a paid tier covers.
fn covered_by_plan_tier(plan_tier: &str, value: &str) -> bool {
    !super::is_free_plan_tier(plan_tier) || !names_a_key_value_store(value)
}

/// Whether the ARN `value` names a CloudFront KeyValueStore.
///
/// An ARN whose id segment is empty names no store, so it is not one either.
fn names_a_key_value_store(value: &str) -> bool {
    ::service_common::arn::is_resource_arn_of(value, CLOUDFRONT_VENDOR)
        && ::service_common::arn::resource_id_of_type(value, KEY_VALUE_STORE_RESOURCE_TYPE)
            .is_some_and(|id| !id.is_empty())
}

/// The distribution id the ARN `value` names, or `None` when it names no CloudFront
/// distribution.
///
/// An ARN whose id segment is empty names no distribution to ask about, so it is not one
/// either.
pub(super) fn distribution_id(value: &str) -> Option<&str> {
    if !::service_common::arn::is_resource_arn_of(value, CLOUDFRONT_VENDOR) {
        return None;
    }
    ::service_common::arn::resource_id_of_type(value, DISTRIBUTION_RESOURCE_TYPE)
        .filter(|id| !id.is_empty())
}

/// Whether the ARN `value` names a WAF web ACL, of either namespace.
pub(super) fn names_a_web_acl(value: &str) -> bool {
    ::service_common::arn::resource(value)
        .is_some_and(|resource| resource.contains(WEB_ACL_SEGMENT))
        && ::service_common::arn::is_resource_arn_of(value, WAFV2_VENDOR)
}

/// Whether the distribution `id` names is fronted by one of the web ACLs
/// `named_web_acls` names, as CloudFront reports its configuration — and `None` when
/// CloudFront answered nothing about it, in which case nothing is established and nothing
/// is refused.
///
/// A distribution CloudFront reports it does not hold is not protected by anything, which
/// is an answer: its ARN names no resource a plan can cover.
async fn protected_by_a_named_web_acl(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    id: &str,
    named_web_acls: &[&str],
) -> Option<bool> {
    // Which Region the distribution is read in decides nothing — the emulator serves one
    // CloudFront, a global service — so a Region field outside the label grammar falls
    // back to the default rather than putting caller text in the client's authority.
    let region = ::service_common::arn::region_label(region).unwrap_or(super::DEFAULT_REGION);
    match cloudfront_client(ctx, region)
        .get_distribution()
        .id(id)
        .send()
        .await
    {
        Ok(read) => {
            // The association IS the stored configuration's `WebACLId` — "the ID of the
            // WAF web ACL that is associated with this distribution" — and an empty value
            // is CloudFront reporting no association at all.
            let associated = read
                .distribution()
                .and_then(::aws_sdk_cloudfront::types::Distribution::distribution_config)
                .and_then(::aws_sdk_cloudfront::types::DistributionConfig::web_acl_id)
                .unwrap_or_default();
            Some(!associated.is_empty() && named_web_acls.contains(&associated))
        }
        Err(error) if reported_absent(&error) => Some(false),
        // Any other answer is this emulator failing to ask rather than CloudFront
        // reporting a configuration, so it establishes nothing — and it is worth a
        // warning, because a deployment with no CloudFront to ask is one where this gate
        // does not run. The id is rendered in its escaped `Debug` form, so a value
        // carrying a newline cannot forge a log line.
        Err(error) => {
            ::tracing::warn!(
                distribution = ?id,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "a subscription's resource could not be resolved against CloudFront"
            );
            None
        }
    }
}

/// Whether a `GetDistribution` failure is CloudFront reporting that it holds no
/// distribution under the id, as opposed to the call not reaching CloudFront at all.
fn reported_absent(
    error: &::aws_sdk_cloudfront::error::SdkError<
        ::aws_sdk_cloudfront::operation::get_distribution::GetDistributionError,
        ::aws_smithy_runtime_api::client::orchestrator::HttpResponse,
    >,
) -> bool {
    error.as_service_error().is_some_and(
        ::aws_sdk_cloudfront::operation::get_distribution::GetDistributionError::is_no_such_distribution,
    )
}

/// A CloudFront client over the in-process callback into this same emulator, addressed to
/// `region`'s endpoint.
///
/// The transport is in-process, so there is nothing to retry, time out or watch for a
/// stalled stream — each would need a sleep implementation this client is not given — and
/// the credentials the callback hands over are used directly rather than cached. They are
/// the emulator's own and only sign this in-process call.
fn cloudfront_client(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> ::aws_sdk_cloudfront::Client {
    let config = ::aws_sdk_cloudfront::Config::builder()
        .behavior_version(::aws_sdk_cloudfront::config::BehaviorVersion::latest())
        .region(::aws_sdk_cloudfront::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://{CLOUDFRONT_VENDOR}.{region}.amazonaws.com"))
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .http_client(ctx.callback().http_client())
        .retry_config(::aws_sdk_cloudfront::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_cloudfront::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_cloudfront::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_cloudfront::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_cloudfront::Client::from_conf(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Which ARNs name a CloudFront distribution to resolve, which name a WAF web
    /// ACL the association is compared against, and which name a CloudFront
    /// KeyValueStore the tier places. The forms come from the ARN each service
    /// mints: `arn:aws:cloudfront::<account>:distribution/<id>`,
    /// `arn:aws:cloudfront::<account>:key-value-store/<id>` and
    /// `arn:aws:wafv2:<region>:<account>:<global|regional>/webacl/<name>/<id>`.
    #[test]
    fn resource_arns_are_placed_by_vendor_and_resource_type() {
        // (case, ARN, the distribution id it names, whether it names a web ACL,
        //  whether it names a KeyValueStore)
        let rows: [(&str, &str, Option<&str>, bool, bool); 11] = [
            (
                "a distribution",
                "arn:aws:cloudfront::000000000000:distribution/E1EXAMPLE1EXAMPLE",
                Some("E1EXAMPLE1EXAMPLE"),
                false,
                false,
            ),
            (
                "a distribution of another partition",
                "arn:aws-cn:cloudfront::000000000000:distribution/E2EXAMPLE2EXAMPLE",
                Some("E2EXAMPLE2EXAMPLE"),
                false,
                false,
            ),
            (
                "a distribution ARN naming no id",
                "arn:aws:cloudfront::000000000000:distribution/",
                None,
                false,
                false,
            ),
            (
                "a KeyValueStore",
                "arn:aws:cloudfront::000000000000:key-value-store/kvs-1",
                None,
                false,
                true,
            ),
            (
                "a KeyValueStore of another partition",
                "arn:aws-us-gov:cloudfront::000000000000:key-value-store/\
                 11111111-2222-3333-4444-555555555555",
                None,
                false,
                true,
            ),
            (
                "a KeyValueStore ARN naming no id",
                "arn:aws:cloudfront::000000000000:key-value-store/",
                None,
                false,
                false,
            ),
            (
                "a global web ACL",
                "arn:aws:wafv2:us-east-1:000000000000:global/webacl/acl/11111111-2222-3333-4444-555555555555",
                None,
                true,
                false,
            ),
            (
                "a regional web ACL",
                "arn:aws:wafv2:eu-west-1:000000000000:regional/webacl/acl/11111111-2222-3333-4444-555555555555",
                None,
                true,
                false,
            ),
            (
                "another WAF resource",
                "arn:aws:wafv2:us-east-1:000000000000:global/ipset/set/11111111-2222-3333-4444-555555555555",
                None,
                false,
                false,
            ),
            (
                "a Route 53 hosted zone",
                "arn:aws:route53:::hostedzone/Z1EXAMPLE",
                None,
                false,
                false,
            ),
            (
                "not an ARN at all",
                "distribution/E1EXAMPLE1EXAMPLE",
                None,
                false,
                false,
            ),
        ];

        for (case, arn, expected_id, expected_web_acl, expected_store) in rows {
            let actual_id = distribution_id(arn);
            assert_eq!(
                actual_id, expected_id,
                "case {case:?}: expected the distribution id {expected_id:?}, got {actual_id:?}"
            );
            let actual_web_acl = names_a_web_acl(arn);
            assert_eq!(
                actual_web_acl, expected_web_acl,
                "case {case:?}: expected names_a_web_acl == {expected_web_acl}, \
                 got {actual_web_acl}"
            );
            let actual_store = names_a_key_value_store(arn);
            assert_eq!(
                actual_store, expected_store,
                "case {case:?}: expected names_a_key_value_store == {expected_store}, \
                 got {actual_store}"
            );
        }
    }

    /// A KeyValueStore is covered by every tier but the free one; every other resource
    /// kind is covered by every tier, including a tier the service docs do not publish.
    #[test]
    fn the_free_tier_is_the_only_one_that_does_not_cover_a_key_value_store() {
        let store = "arn:aws:cloudfront::000000000000:key-value-store/kvs-1";
        let others = [
            "arn:aws:cloudfront::000000000000:distribution/E1EXAMPLE1EXAMPLE",
            "arn:aws:wafv2:us-east-1:000000000000:global/webacl/acl/11111111-2222-3333-4444-555555555555",
            "arn:aws:route53:::hostedzone/Z1EXAMPLE",
        ];
        // Every published tier, plus one the docs do not publish.
        let tiers: Vec<&str> = super::super::PLAN_TIERS_ASCENDING
            .iter()
            .copied()
            .chain(["ENTERPRISE"])
            .collect();

        for tier in tiers {
            let expected = tier != super::super::FREE_PLAN_TIER;
            let actual = covered_by_plan_tier(tier, store);
            assert_eq!(
                actual, expected,
                "case {tier:?}: expected a KeyValueStore to be covered == {expected}, \
                 got {actual}"
            );
            for other in others {
                assert!(
                    covered_by_plan_tier(tier, other),
                    "case {tier:?}/{other:?}: every tier covers a resource the tier does \
                     not place"
                );
            }
        }
    }

    /// Each refusal class is worded for its own question: the tier refusal states the tier
    /// the subscription is on and names no resource, while the prerequisite refusal states
    /// the resources in the order the request named them. The class that answers a finding
    /// refused by both is the tier one.
    #[test]
    fn each_refusal_class_is_worded_for_its_own_question() {
        let first = "arn:aws:cloudfront::000000000000:distribution/E1".to_string();
        let second = "arn:aws:cloudfront::000000000000:distribution/E2".to_string();
        let store = "arn:aws:cloudfront::000000000000:key-value-store/kvs-1".to_string();

        let nothing = IneligibleResources::default();
        assert!(nothing.is_empty(), "an empty finding refuses nothing");
        assert!(
            nothing.refusal(super::super::FREE_PLAN_TIER).is_none(),
            "an empty finding must produce no refusal"
        );

        // The tier class, over every published tier: the sentence quotes the tier it is
        // given and carries no ARN.
        for tier in super::super::PLAN_TIERS_ASCENDING {
            let finding = IneligibleResources {
                outside_plan_tier: vec![store.clone()],
                prerequisite_unmet: Vec::new(),
            };
            let refusal = finding
                .refusal(tier)
                .expect("a resource outside the tier must be refused");
            assert_eq!(
                refusal.message,
                format!(
                    "Resources are incompatible with plan tier '{tier}': modify resources \
                     or select a different tier."
                ),
                "case {tier:?}: unexpected tier-refusal message {:?}",
                refusal.message
            );
            assert!(
                !refusal.message.contains(&store),
                "case {tier:?}: the tier refusal must name no resource, got {:?}",
                refusal.message
            );
            assert_eq!(
                refusal.resource_id, None,
                "case {tier:?}: the tier refusal identifies no single resource, got {:?}",
                refusal.resource_id
            );
        }

        // The prerequisite class: one resource, then several, in the order they were named.
        let one = IneligibleResources {
            outside_plan_tier: Vec::new(),
            prerequisite_unmet: vec![first.clone()],
        };
        let refusal = one.refusal("PRO").expect("a prerequisite failure is refused");
        assert_eq!(
            refusal.message,
            format!("The following resources are not eligible for this subscription tier: {first}."),
            "one ineligible resource must be stated on its own, got {:?}",
            refusal.message
        );
        assert_eq!(
            refusal.resource_id,
            Some(first.clone()),
            "a single prerequisite failure names the resource that failed, got {:?}",
            refusal.resource_id
        );
        let two = IneligibleResources {
            outside_plan_tier: Vec::new(),
            prerequisite_unmet: vec![first.clone(), second.clone()],
        };
        let refusal = two.refusal("PRO").expect("a prerequisite failure is refused");
        assert_eq!(
            refusal.message,
            format!(
                "The following resources are not eligible for this subscription tier: \
                 {first}, {second}."
            ),
            "every ineligible resource must be stated, in the order the request named them, \
             got {:?}",
            refusal.message
        );
        assert_eq!(
            refusal.resource_id, None,
            "several prerequisite failures identify no single resource, got {:?}",
            refusal.resource_id
        );

        // Both questions at once: the tier sentence answers, because no prerequisite the
        // caller could fix would admit the set on this tier.
        let both = IneligibleResources {
            outside_plan_tier: vec![store.clone()],
            prerequisite_unmet: vec![first.clone()],
        };
        let refusal = both
            .refusal(super::super::FREE_PLAN_TIER)
            .expect("a finding of either class is refused");
        assert_eq!(
            refusal.message,
            resources_incompatible_with_plan_tier_message(super::super::FREE_PLAN_TIER),
            "a finding of both classes must be worded as the tier refusal, got {:?}",
            refusal.message
        );
    }

    /// A finding narrowed to the resources one request named drops every resource it did
    /// not, in either class, and keeps the rest in order.
    #[test]
    fn a_finding_narrowed_to_one_requests_resources_keeps_only_those() {
        let held = "arn:aws:cloudfront::000000000000:distribution/E1".to_string();
        let added = "arn:aws:cloudfront::000000000000:distribution/E2".to_string();
        let store = "arn:aws:cloudfront::000000000000:key-value-store/kvs-1".to_string();
        let finding = IneligibleResources {
            outside_plan_tier: vec![store.clone()],
            prerequisite_unmet: vec![held.clone(), added.clone()],
        };

        let narrowed = finding.only_named(&[added.clone(), store.clone()]);
        assert_eq!(
            narrowed,
            IneligibleResources {
                outside_plan_tier: vec![store.clone()],
                prerequisite_unmet: vec![added.clone()],
            },
            "only the resources the request named may be refused, got {narrowed:?}"
        );
        let narrowed = finding.only_named(std::slice::from_ref(&held));
        assert_eq!(
            narrowed,
            IneligibleResources {
                outside_plan_tier: Vec::new(),
                prerequisite_unmet: vec![held.clone()],
            },
            "a request naming none of the other findings must not carry them, got {narrowed:?}"
        );
        assert!(
            finding.only_named(&[]).is_empty(),
            "a request naming nothing refuses nothing"
        );
    }
}
