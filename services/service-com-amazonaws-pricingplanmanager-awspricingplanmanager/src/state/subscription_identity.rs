// SPDX-License-Identifier: CC0-1.0

//! Which members of a `CreateSubscription` request say WHICH subscription it asks for.
//!
//! `clientToken` is "a unique, case-sensitive identifier that you provide to ensure that
//! the request is handled only once. If you send the same request with the same client
//! token, the API returns the original response without creating a duplicate subscription"
//! (`smithy_query(query="doc",shape="CreateSubscriptionInput",member="clientToken")`). The
//! promise is about the SAME request: a token is a safe-retry handle on one request, not a
//! name for whatever the caller sends next. So a replay is answered with the subscription
//! the token minted only when the request is the same one; a token replayed with different
//! parameters is a caller mistake — it asked for one subscription and would be handed
//! another — and is refused with the modelled `ConflictException` (HTTP 409, "The request
//! conflicts with the current state of the resource"), whose `resourceId` names the
//! subscription the token already stands for. Nothing is written on either path.
//!
//! WHICH members identify the subscription a request asks for is the whole of what this
//! module decides: the plan it buys (`planFamily`, `planTier`, `usageLevel`), the resources
//! it covers (`resourceArns`) and whether it is held for approval (`approvalMode`) — every
//! member of `CreateSubscriptionInput` other than the token itself. A record this service
//! stored for a replayed token answers the comparison out of its own fields, so what is
//! compared is the stored subscription rather than a copy of the first request.

use super::{ApprovalMode, SubscriptionRecord};
use crate::ops::types::ConflictException;

/// The members of a `CreateSubscription` request that say which subscription it asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubscriptionIdentity<'request> {
    /// `planFamily`: the plan family the subscription is bought in.
    pub plan_family: &'request str,
    /// `planTier`: the tier within that family.
    pub plan_tier: &'request str,
    /// `usageLevel`: the level within the tier, when the request states one.
    pub usage_level: Option<&'request str>,
    /// `resourceArns`: the resources the subscription covers.
    pub resource_arns: &'request [String],
    /// `approvalMode`, after `CreateSubscription`'s own default has been applied — so a
    /// request that leaves it out asks for the same subscription as one that states the
    /// default.
    pub approval_mode: ApprovalMode,
}

impl SubscriptionIdentity<'_> {
    /// Whether `record` is the subscription this request asks for.
    ///
    /// Every identifying member has to agree. The resources are compared as the SET they
    /// are — `resourceArns` names "the resources to include in the subscription", and a
    /// subscription covers each named resource once however the list was ordered or
    /// repeated — so the same resources named in another order are the same request, while
    /// one more or one fewer resource is a different one.
    pub fn matches(&self, record: &SubscriptionRecord) -> bool {
        self.plan_family == record.plan_family
            && self.plan_tier == record.plan_tier
            && self.usage_level == record.usage_level.as_deref()
            && self.approval_mode == record.approval_mode
            && same_resources(self.resource_arns, &record.resource_arns)
    }
}

/// Whether two resource lists name the same resources, ignoring order and repetition.
fn same_resources(left: &[String], right: &[String]) -> bool {
    distinct(left) == distinct(right)
}

/// The resources `resources` names, each once, in a stable order.
fn distinct(resources: &[String]) -> Vec<&str> {
    let mut names: Vec<&str> = resources.iter().map(String::as_str).collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// How this service refuses a `clientToken` replayed with different parameters: the
/// modelled `ConflictException` (HTTP 409), whose `resourceId` is `@required` and names the
/// subscription `arn` the token already stands for.
///
/// The sentence is fixed text: it states what a `clientToken` is for, so it carries no
/// value from the request and none from the store. Recorded behaviour.
pub fn client_token_parameters_differ(arn: &str) -> ConflictException {
    ConflictException {
        message: "The clientToken matches a previous request but the request parameters \
                  differ. Reuse a clientToken only for an identical request."
            .to_string(),
        resource_id: arn.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ScheduledChangeRecord, Status, FIRST_REVISION};
    use ::aws_smithy_types::DateTime;

    /// The ARN forms the owning services mint, which the rows below name.
    const DISTRIBUTION: &str = "arn:aws:cloudfront::000000000000:distribution/E1EXAMPLE";
    const WEB_ACL: &str =
        "arn:aws:wafv2:us-east-1:000000000000:global/webacl/acl/11111111-2222-3333-4444-555555555555";
    const HOSTED_ZONE: &str = "arn:aws:route53:::hostedzone/Z1EXAMPLE";

    /// The subscription every row is compared against: the `CloudFront` family at the free
    /// tier with no usage level, immediate approval, covering the distribution and the web
    /// ACL.
    fn stored() -> SubscriptionRecord {
        let now = DateTime::from_secs(1_800_000_000);
        SubscriptionRecord {
            arn: "arn:aws:pricingplanmanager::000000000000:subscription/sub_test".to_string(),
            plan_family: "CloudFront".to_string(),
            plan_tier: "FREE".to_string(),
            usage_level: None,
            status: Status::Active,
            status_reason: None,
            resource_arns: vec![DISTRIBUTION.to_string(), WEB_ACL.to_string()],
            scheduled_change: None,
            created_at: now,
            updated_at: now,
            client_token: Some("token".to_string()),
            approval_mode: ApprovalMode::Immediate,
            revision: FIRST_REVISION,
            sync_completes_at: None,
        }
    }

    /// A request over `resources`, otherwise the stored subscription's own parameters.
    fn request<'a>(resources: &'a [String]) -> SubscriptionIdentity<'a> {
        SubscriptionIdentity {
            plan_family: "CloudFront",
            plan_tier: "FREE",
            usage_level: None,
            resource_arns: resources,
            approval_mode: ApprovalMode::Immediate,
        }
    }

    /// RULE: a replay is the same request only when every identifying member agrees; the
    /// rows differ along which member differs, and along the order and repetition of the
    /// resource list, which name the same set.
    #[test]
    fn a_replay_matches_only_when_every_identifying_member_agrees() {
        let record = stored();
        let pair = vec![DISTRIBUTION.to_string(), WEB_ACL.to_string()];
        let reversed = vec![WEB_ACL.to_string(), DISTRIBUTION.to_string()];
        let repeated = vec![
            DISTRIBUTION.to_string(),
            WEB_ACL.to_string(),
            DISTRIBUTION.to_string(),
        ];
        let fewer = vec![DISTRIBUTION.to_string()];
        let more = vec![
            DISTRIBUTION.to_string(),
            WEB_ACL.to_string(),
            HOSTED_ZONE.to_string(),
        ];
        let other = vec![HOSTED_ZONE.to_string(), WEB_ACL.to_string()];

        // (case, the request, whether it asks for the stored subscription)
        let rows: Vec<(&str, SubscriptionIdentity<'_>, bool)> = vec![
            ("the identical request", request(&pair), true),
            ("the same resources, reordered", request(&reversed), true),
            ("the same resources, one repeated", request(&repeated), true),
            ("one resource fewer", request(&fewer), false),
            ("one resource more", request(&more), false),
            ("another resource in place of one", request(&other), false),
            (
                "another plan tier",
                SubscriptionIdentity {
                    plan_tier: "PRO",
                    ..request(&pair)
                },
                false,
            ),
            (
                "another plan family",
                SubscriptionIdentity {
                    plan_family: "Route53",
                    ..request(&pair)
                },
                false,
            ),
            (
                "a usage level the stored one does not carry",
                SubscriptionIdentity {
                    usage_level: Some("DEFAULT"),
                    ..request(&pair)
                },
                false,
            ),
            (
                "another approval mode",
                SubscriptionIdentity {
                    approval_mode: ApprovalMode::Manual,
                    ..request(&pair)
                },
                false,
            ),
        ];

        for (case, identity, expected) in rows {
            let actual = identity.matches(&record);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected matches == {expected}, got {actual} for {identity:?}"
            );
        }
    }

    /// RULE: what the subscription has DONE since it was minted is not part of its
    /// identity — a replay of the token is still the same request once the subscription has
    /// taken a change of its own.
    #[test]
    fn a_replay_matches_a_record_that_has_moved_on() {
        let now = DateTime::from_secs(1_800_000_000);
        let mut record = stored();
        record.status = Status::SyncInProgress;
        record.status_reason = Some("applying".to_string());
        record.scheduled_change = Some(ScheduledChangeRecord::pending_cancellation(now));
        record.touch(now);
        let pair = vec![DISTRIBUTION.to_string(), WEB_ACL.to_string()];
        assert!(
            request(&pair).matches(&record),
            "the stored subscription's own progress must not change which request minted it"
        );
    }

    /// RULE: the refusal states the fixed sentence and names the subscription the token
    /// already stands for.
    #[test]
    fn the_refusal_names_the_subscription_the_token_stands_for() {
        let arn = "arn:aws:pricingplanmanager:us-east-1:000000000000:subscription/sub_1";
        let refusal = client_token_parameters_differ(arn);
        assert_eq!(
            refusal.message,
            "The clientToken matches a previous request but the request parameters differ. \
             Reuse a clientToken only for an identical request.",
            "the sentence is fixed text, got {:?}",
            refusal.message
        );
        assert_eq!(
            refusal.resource_id, arn,
            "the refusal must name the subscription the token stands for, got {:?}",
            refusal.resource_id
        );
    }
}
