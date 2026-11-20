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
//! TIER it is bought at, which is what the service's own refusal states — "not eligible
//! for this subscription tier"
//! ([`ineligible_resources_message`]). That same note says a subscription may also cover
//! "other supported
//! resources, such as Amazon Route 53 hosted zones, and CloudFront KeyValueStores", and
//! those two are not covered alike: the free tier — the lowest of the published tiers
//! "`FREE`, `PRO`, `BUSINESS`, or `PREMIUM`"
//! (`smithy_query(query="doc",shape="Subscription",member="planTier")`) — carries a
//! hosted zone but not a KeyValueStore, which AWS covers only from a paid tier up. So a
//! KeyValueStore named for a free-tier CloudFront subscription is refused with that same
//! message, while every tier that is not the free one covers it.
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

/// How this service states a request whose resources are not eligible for the plan it
/// asks for, as AWS words that refusal: the fixed sentence, then the resources that
/// failed it, then the full stop.
///
/// The variable part is the ineligible ARNs THE REQUEST named, in the order it named
/// them, so a caller can match each one against the list it sent.
pub fn ineligible_resources_message(ineligible: &[String]) -> String {
    format!(
        "The following resources are not eligible for this subscription tier: {}.",
        ineligible.join(", ")
    )
}

/// The resources among `resource_arns` that are NOT eligible for the plan
/// `plan_family`/`plan_tier` names, in the order the request named them; empty when every
/// named resource is eligible, and when nothing of the kind could be established (see the
/// module docs).
///
/// `region` is the Region the request was signed for, which is the one the owning service
/// is reached in.
pub async fn ineligible_resources(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    plan_family: &str,
    plan_tier: &str,
    resource_arns: &[String],
) -> Vec<String> {
    // Only the CloudFront family's resources are documented, so only within it is a
    // resource placed at all: every other family establishes nothing and refuses nothing.
    if !plan_family.eq_ignore_ascii_case(CLOUDFRONT_PLAN_FAMILY) {
        return Vec::new();
    }
    // The web ACLs the SAME request names: the association a distribution must carry to
    // be eligible is one of these, because that is the pair the subscription covers.
    let named_web_acls: Vec<&str> = resource_arns
        .iter()
        .map(String::as_str)
        .filter(|arn| names_a_web_acl(arn))
        .collect();

    let mut ineligible: Vec<String> = Vec::new();
    for resource_arn in resource_arns {
        // What the TIER covers needs no other service to answer.
        if !covered_by_plan_tier(plan_tier, resource_arn) {
            ineligible.push(resource_arn.clone());
            continue;
        }
        let Some(id) = distribution_id(resource_arn) else {
            continue;
        };
        if protected_by_a_named_web_acl(ctx, region, id, &named_web_acls).await == Some(false) {
            ineligible.push(resource_arn.clone());
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

    /// The refusal states the fixed sentence, the ineligible resources in the order the
    /// request named them, and the full stop.
    #[test]
    fn the_refusal_lists_the_ineligible_resources() {
        let one = ["arn:aws:cloudfront::000000000000:distribution/E1".to_string()];
        assert_eq!(
            ineligible_resources_message(&one),
            "The following resources are not eligible for this subscription tier: \
             arn:aws:cloudfront::000000000000:distribution/E1.",
            "one ineligible resource must be stated on its own"
        );
        let two = [
            "arn:aws:cloudfront::000000000000:distribution/E1".to_string(),
            "arn:aws:cloudfront::000000000000:distribution/E2".to_string(),
        ];
        assert_eq!(
            ineligible_resources_message(&two),
            "The following resources are not eligible for this subscription tier: \
             arn:aws:cloudfront::000000000000:distribution/E1, \
             arn:aws:cloudfront::000000000000:distribution/E2.",
            "every ineligible resource must be stated, in the order the request named them"
        );
    }
}
