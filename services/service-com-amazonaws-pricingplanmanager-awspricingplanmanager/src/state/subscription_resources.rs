// SPDX-License-Identifier: CC0-1.0

//! Which resource set a stored subscription may hold, and how a change to it is refused.
//!
//! A subscription's `resourceArns` is not a free-form list: it is `@required` with
//! `@length(min: 1, max: 10)` on every shape that carries it
//! (`smithy_query(query="shape", shape="Subscription,ResourceArns")`), so a subscription
//! always covers at least one resource, and for one plan family the composition is
//! documented too — "For subscriptions in the CloudFront plan family, the associated
//! resources must always include exactly one Amazon CloudFront distribution and exactly
//! one WAF web ACL. You cannot remove these required resources."
//! (`smithy_query(query="doc", shape="DisassociateResourcesFromSubscription")`).
//!
//! So a request that changes the set is weighed against the set it would LEAVE BEHIND
//! rather than against the resources it names — a subscription covering two resources of a
//! required kind may lose one of them, because what is left still satisfies the family's
//! composition, and a subscription may grow only while the set it would end up with still
//! fits the modelled list. It is admitted only when the subscription still covers a
//! resource of every kind its plan family requires and at least one resource at all,
//! and only while the subscription's own state admits a change — "The subscription must be
//! in an active state that is not pending other changes"
//! (`smithy_query(query="doc", shape="AssociateResourcesToSubscription")`), and
//! "`SYNC_IN_PROGRESS` — A change is being applied to the subscription. Wait for the
//! operation to complete before making additional changes."
//! (`smithy_query(query="doc", shape="Status")`). A request the record's own pending change
//! refuses is the modelled `ConflictException` (HTTP 409), the one `UpdateSubscription`
//! already answers; everything else is the modelled business-rule refusal
//! `ValidationException` (HTTP 400, "The request failed a business rule validation … or the
//! subscription might not be in the required state for this operation"). Either way the
//! stored set is left alone.
//!
//! Which ARN names a CloudFront distribution and which names a WAF web ACL is read
//! through [`super::resource_eligibility`]'s own classification, so this crate places a
//! resource ARN in exactly one place.

use super::resource_eligibility::{distribution_id, names_a_web_acl, CLOUDFRONT_PLAN_FAMILY};
use super::{scheduled_change_pending, Status, SubscriptionRecord, MAX_SUBSCRIPTION_RESOURCES};
use crate::ops::types::{ConflictException, ValidationException};

/// The status in which a subscription's resource set may be changed: the one in which
/// "resources are covered by flat-rate pricing" (`smithy_query(query="doc",
/// shape="Status")`). A subscription waiting for approval, applying a change, or failed
/// is not in the state such a request requires.
pub const RESOURCE_CHANGE_STATUS: Status = Status::Active;

/// A kind of resource a plan family requires every one of its subscriptions to cover.
///
/// The CloudFront family names two: "the associated resources must always include exactly
/// one Amazon CloudFront distribution and exactly one WAF web ACL". Which ARN names which
/// is read through [`super::resource_eligibility`]'s own classification, so this crate
/// places a resource ARN in exactly one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequiredKind {
    /// An Amazon CloudFront distribution.
    Distribution,
    /// A WAF web ACL.
    WebAcl,
}

/// The kind of required resource `resource_arn` names, or `None` for one that is not of a
/// required kind at all — a resource a subscription may also cover ("Amazon Route 53
/// hosted zones, and CloudFront KeyValueStores").
fn required_kind(resource_arn: &str) -> Option<RequiredKind> {
    if distribution_id(resource_arn).is_some() {
        return Some(RequiredKind::Distribution);
    }
    names_a_web_acl(resource_arn).then_some(RequiredKind::WebAcl)
}

/// The kinds of resource `plan_family` requires a subscription to cover at all times.
///
/// Only the CloudFront family documents a composition; a family with no documented one
/// requires no particular kind, and a subscription of it is held only to the rule that it
/// covers at least one resource.
fn required_kinds(plan_family: &str) -> &'static [RequiredKind] {
    if plan_family.eq_ignore_ascii_case(CLOUDFRONT_PLAN_FAMILY) {
        &[RequiredKind::Distribution, RequiredKind::WebAcl]
    } else {
        &[]
    }
}

/// Whether `plan_family` requires every one of its subscriptions to cover a resource of
/// the kind `resource_arn` names.
///
/// Says what the resource IS, not whether this particular subscription may lose it: that
/// depends on what the subscription would be left covering, which is
/// [`plan_resource_removal`]'s question.
pub fn is_required_resource(plan_family: &str, resource_arn: &str) -> bool {
    required_kind(resource_arn)
        .is_some_and(|kind| required_kinds(plan_family).contains(&kind))
}

/// The kinds `plan_family` requires that NO resource in `resource_arns` provides, in the
/// order the family names them; empty when the set satisfies the family's composition.
fn unsatisfied_kinds(plan_family: &str, resource_arns: &[String]) -> Vec<RequiredKind> {
    required_kinds(plan_family)
        .iter()
        .copied()
        .filter(|kind| {
            !resource_arns
                .iter()
                .any(|held| required_kind(held) == Some(*kind))
        })
        .collect()
}

/// How this service states a removal its plan family's composition forbids: the rule the
/// plan family imposes, as the operation documents it, and then the resources the request
/// named that it keeps on.
///
/// The two sentences are the operation's own documented note — "For subscriptions in the
/// CloudFront plan family, the associated resources must always include exactly one Amazon
/// CloudFront distribution and exactly one WAF web ACL. You cannot remove these required
/// resources." (`smithy_query(query="doc",
/// shape="DisassociateResourcesFromSubscription")`) — and the only variable part is the
/// required ARNs THE REQUEST named, in the order it named them, so a caller can match each
/// one against the list it sent.
pub fn required_resources_message(required: &[String]) -> String {
    format!(
        "For subscriptions in the {CLOUDFRONT_PLAN_FAMILY} plan family, the associated \
         resources must always include exactly one Amazon CloudFront distribution and \
         exactly one WAF web ACL. You cannot remove these required resources: {}.",
        required.join(", ")
    )
}

/// Why a subscription's resource set could not be changed as a request asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceChangeRefusal {
    /// The subscription already has a change scheduled on it, so another one cannot be
    /// stacked on top — the conflict every mutator of this service reports.
    ChangePending,
    /// The subscription's status does not admit a change to its resources.
    Status(Status),
    /// The request named a resource the subscription does not cover, so there is nothing
    /// of that name to remove.
    NotAssociated(String),
    /// The request named a resource the subscription already covers, so there is nothing
    /// of that name to add.
    AlreadyAssociated(String),
    /// The request named a resource ANOTHER subscription already covers, so this one cannot
    /// take it on.
    UsedByAnotherSubscription(String),
    /// The request named resources the subscription's plan family requires it keep.
    Required(Vec<String>),
    /// The change would leave the subscription covering no resource at all.
    LeavesNoResources,
    /// The change would leave the subscription covering more resources than one
    /// subscription may hold: it covers `covered` and the request adds `adding`.
    TooManyResources { covered: usize, adding: usize },
}

/// The modelled error a [`ResourceChangeRefusal`] is answered with: the conflict for a
/// request the record's own pending state refuses, the business-rule failure for a request
/// whose resources or whose timing the rule refuses.
#[derive(Debug)]
pub enum ResourceChangeError {
    /// `ConflictException`, HTTP 409.
    Conflict(ConflictException),
    /// `ValidationException`, HTTP 400.
    Validation(ValidationException),
}

impl ResourceChangeRefusal {
    /// The modelled error for a request against the subscription `arn` names.
    ///
    /// `resourceId` is "the identifier of the resource that failed validation", which is
    /// the offending resource when exactly one of them failed and the subscription itself
    /// when the failure is about its own state or about its set as a whole.
    pub fn into_modelled(self, arn: &str) -> ResourceChangeError {
        match self {
            Self::ChangePending => {
                ResourceChangeError::Conflict(scheduled_change_pending(arn))
            }
            Self::Status(status) => ResourceChangeError::Validation(ValidationException {
                message: format!(
                    "Cannot change the resources of subscription {arn}: the subscription \
                     is {}; its resources can be changed only while it is {}.",
                    status.wire(),
                    RESOURCE_CHANGE_STATUS.wire()
                ),
                resource_id: Some(arn.to_string()),
            }),
            Self::NotAssociated(resource) => {
                ResourceChangeError::Validation(ValidationException {
                    message: format!(
                        "Cannot remove {resource} from subscription {arn}: the resource is \
                         not associated with this subscription."
                    ),
                    resource_id: Some(resource),
                })
            }
            Self::AlreadyAssociated(resource) => {
                ResourceChangeError::Validation(ValidationException {
                    message: format!(
                        "Cannot add {resource} to subscription {arn}: the resource is \
                         already associated with this subscription."
                    ),
                    resource_id: Some(resource),
                })
            }
            // The same rule `CreateSubscription` refuses a request under
            // ([`resource_already_subscribed`]), stated for the operation that would add
            // the resource to a subscription it is not on: the tail is the rule itself, so
            // it is worded as AWS words it there.
            Self::UsedByAnotherSubscription(resource) => {
                ResourceChangeError::Validation(ValidationException {
                    message: format!(
                        "Cannot add {resource} to subscription {arn}: resource must belong \
                         to your account and not be used in another subscription."
                    ),
                    resource_id: Some(resource),
                })
            }
            Self::Required(required) => ResourceChangeError::Validation(ValidationException {
                message: required_resources_message(&required),
                resource_id: match required.as_slice() {
                    [only] => Some(only.clone()),
                    _ => None,
                },
            }),
            Self::LeavesNoResources => ResourceChangeError::Validation(ValidationException {
                message: format!(
                    "Cannot remove every resource from subscription {arn}: a subscription \
                     must always include at least one resource."
                ),
                resource_id: Some(arn.to_string()),
            }),
            Self::TooManyResources { covered, adding } => {
                ResourceChangeError::Validation(ValidationException {
                    message: format!(
                        "Cannot change the resources of subscription {arn}: it covers \
                         {covered} and the request adds {adding}; a subscription must \
                         include at most {MAX_SUBSCRIPTION_RESOURCES} resources."
                    ),
                    resource_id: Some(arn.to_string()),
                })
            }
        }
    }
}

/// The resource set `record` is left with once the resources `requested` names are
/// removed, or why that removal is refused.
///
/// Decides nothing from the request's shape: the answer is about the set the change would
/// leave behind, so the same rule holds for one resource or for ten, named in any order,
/// and the caller writes the returned set back only on the success arm — a refused call
/// changes nothing.
pub fn plan_resource_removal(
    record: &SubscriptionRecord,
    requested: &[String],
) -> Result<Vec<String>, ResourceChangeRefusal> {
    if record.status != RESOURCE_CHANGE_STATUS {
        return Err(ResourceChangeRefusal::Status(record.status));
    }
    // "The subscription must be in an active state that is not pending other changes" — a
    // subscription with a downgrade or a cancellation already scheduled is pending one.
    if record.scheduled_change.is_some() {
        return Err(ResourceChangeRefusal::ChangePending);
    }
    // In the order the request named them, so a caller reading the refusal sees its own
    // list's entries in its own order.
    for resource in requested {
        if !record.resource_arns.iter().any(|held| held == resource) {
            return Err(ResourceChangeRefusal::NotAssociated(resource.clone()));
        }
    }
    let retained: Vec<String> = record
        .resource_arns
        .iter()
        .filter(|held| !requested.iter().any(|removed| removed == *held))
        .cloned()
        .collect();
    // The composition is a rule about the SET the change leaves behind, so what decides
    // the request is which required kinds that set no longer provides — not which kind
    // each named resource happens to be. The resources named in the refusal are the ones
    // the request would have taken the last of.
    let unsatisfied = unsatisfied_kinds(&record.plan_family, &retained);
    if !unsatisfied.is_empty() {
        let required: Vec<String> = requested
            .iter()
            .filter(|resource| {
                required_kind(resource).is_some_and(|kind| unsatisfied.contains(&kind))
            })
            .cloned()
            .collect();
        return Err(ResourceChangeRefusal::Required(required));
    }
    // A plan family with no documented composition is still held to the one rule every
    // subscription keeps: `resourceArns` is `@required` with `@length(min: 1)`.
    if retained.is_empty() {
        return Err(ResourceChangeRefusal::LeavesNoResources);
    }
    Ok(retained)
}

/// The resource set `record` is left with once the resources `requested` names are added,
/// or why that addition is refused.
///
/// The mirror of [`plan_resource_removal`], and the same shape of answer: what decides the
/// request is the set it would leave the subscription covering, so the rule holds for one
/// resource or for ten, named in any order, and the caller writes the returned set back
/// only on the success arm. A resource the subscription already covers is nothing this call
/// can add, exactly as one it does not cover is nothing a removal can take off; a resource
/// the request names twice names one resource.
///
/// Whether a resource may join this subscription's PLAN is a different question, and one
/// that may need another service's answer: [`super::resource_eligibility`] owns it.
pub fn plan_resource_addition(
    record: &SubscriptionRecord,
    requested: &[String],
) -> Result<Vec<String>, ResourceChangeRefusal> {
    if record.status != RESOURCE_CHANGE_STATUS {
        return Err(ResourceChangeRefusal::Status(record.status));
    }
    // "The subscription must be in an active state that is not pending other changes" — a
    // subscription with a downgrade or a cancellation already scheduled is pending one.
    if record.scheduled_change.is_some() {
        return Err(ResourceChangeRefusal::ChangePending);
    }
    // In the order the request named them, so a caller reading the refusal sees its own
    // list's entries in its own order.
    for resource in requested {
        if record.resource_arns.iter().any(|held| held == resource) {
            return Err(ResourceChangeRefusal::AlreadyAssociated(resource.clone()));
        }
    }
    let mut covering = record.resource_arns.clone();
    for resource in requested {
        if !covering.iter().any(|held| held == resource) {
            covering.push(resource.clone());
        }
    }
    // `resourceArns` is `@length(max: 10)` on the shape a subscription is REPORTED through,
    // so a set larger than that is not a subscription this service could answer with.
    if covering.len() > MAX_SUBSCRIPTION_RESOURCES {
        return Err(ResourceChangeRefusal::TooManyResources {
            covered: record.resource_arns.len(),
            adding: covering.len() - record.resource_arns.len(),
        });
    }
    Ok(covering)
}

/// A resource some stored subscription already covers, and the subscription covering it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoveredResource {
    /// The resource, as the request naming it spelled it.
    pub resource_arn: String,
    /// The ARN of the subscription that covers it.
    pub subscription_arn: String,
}

/// The first resource in `requested` that a subscription other than `subscription` already
/// covers, or `None` when no stored subscription covers any of them.
///
/// A resource is on at most ONE pricing plan: the business-rule failure this service models
/// is documented "the specified resource might already be associated with another
/// subscription" (`smithy_query(query="shape", shape="ValidationException")`), so a
/// subscription may be sold over a resource only while no other subscription covers it. A
/// stored subscription covers exactly the resources its own `resourceArns` names, whatever
/// status it is in and whatever change is pending on it — a subscription whose cancellation
/// has not taken effect yet is still the one covering them — so the question is answered
/// from the stored sets alone.
///
/// `subscription`, when given, is the subscription the request is ABOUT, which is never
/// "another" one: a request that names a resource its own subscription already covers is a
/// different refusal ([`ResourceChangeRefusal::AlreadyAssociated`]).
///
/// The answer is the FIRST resource the request named that is taken, in the request's own
/// order, so a caller reading the refusal sees its own list's first offending entry.
pub fn resource_in_another_subscription<'store>(
    stored: impl IntoIterator<Item = &'store SubscriptionRecord>,
    requested: &[String],
    subscription: Option<&str>,
) -> Option<CoveredResource> {
    let mut found: Option<(usize, CoveredResource)> = None;
    for record in stored {
        if subscription == Some(record.arn.as_str()) {
            continue;
        }
        for (position, resource) in requested.iter().enumerate() {
            let covered = record.resource_arns.iter().any(|held| held == resource);
            if covered && found.as_ref().is_none_or(|(at, _)| position < *at) {
                found = Some((
                    position,
                    CoveredResource {
                        resource_arn: resource.clone(),
                        subscription_arn: record.arn.clone(),
                    },
                ));
            }
        }
    }
    found.map(|(_position, covered)| covered)
}

/// How `CreateSubscription` states a resource another subscription already covers: the
/// modelled `ValidationException` (HTTP 400, "The request failed a business rule
/// validation").
///
/// The fixed text states the rule the request broke; the only variable part is the
/// offending resource ARN THE REQUEST named, so a caller can match it against the list it
/// sent. `resourceId` is "the identifier of the resource that failed validation", which is
/// that same resource. Recorded behaviour.
pub fn resource_already_subscribed(resource_arn: &str) -> ValidationException {
    ValidationException {
        message: format!(
            "Cannot create subscription with {resource_arn}: resource must belong to your \
             account and not be used in another subscription."
        ),
        resource_id: Some(resource_arn.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::aws_smithy_types::DateTime;

    /// A stored subscription of `plan_family` covering `resources`, in `status`, with a
    /// change already scheduled on it when `scheduled`.
    fn record(
        plan_family: &str,
        resources: &[&str],
        status: Status,
        scheduled: bool,
    ) -> SubscriptionRecord {
        let now = DateTime::from_secs(1_800_000_000);
        SubscriptionRecord {
            arn: "arn:aws:pricingplanmanager::000000000000:subscription:sub_test".to_string(),
            plan_family: plan_family.to_string(),
            plan_tier: "FREE".to_string(),
            usage_level: None,
            status,
            status_reason: None,
            resource_arns: resources.iter().map(|arn| (*arn).to_string()).collect(),
            scheduled_change: scheduled
                .then(|| super::super::ScheduledChangeRecord::pending_cancellation(now)),
            created_at: now,
            updated_at: now,
            client_token: None,
            approval_mode: super::super::ApprovalMode::Immediate,
            revision: super::super::FIRST_REVISION,
            sync_completes_at: None,
        }
    }

    /// The ARN forms each owning service mints, used as the rows below.
    const DISTRIBUTION: &str = "arn:aws:cloudfront::000000000000:distribution/E1EXAMPLE";
    const WEB_ACL: &str =
        "arn:aws:wafv2:us-east-1:000000000000:global/webacl/acl/11111111-2222-3333-4444-555555555555";
    const HOSTED_ZONE: &str = "arn:aws:route53:::hostedzone/Z1EXAMPLE";
    const KEY_VALUE_STORE: &str = "arn:aws:cloudfront::000000000000:key-value-store/kvs-1";
    const SECOND_DISTRIBUTION: &str = "arn:aws:cloudfront::000000000000:distribution/E2EXAMPLE";

    /// Which resources a plan family requires a subscription to keep: for the CloudFront
    /// family its distribution and its web ACL, and nothing else anywhere.
    ///
    /// The family name is matched ignoring ASCII case, because `planFamily` is a plain
    /// string in the model rather than an enum.
    #[test]
    fn only_the_cloudfront_pair_is_required() {
        // (case, plan family, resource, required)
        let rows: [(&str, &str, &str, bool); 8] = [
            ("the distribution", "CloudFront", DISTRIBUTION, true),
            ("the web ACL", "CloudFront", WEB_ACL, true),
            ("a lower-cased family", "cloudfront", DISTRIBUTION, true),
            ("a hosted zone", "CloudFront", HOSTED_ZONE, false),
            ("a key value store", "CloudFront", KEY_VALUE_STORE, false),
            ("another family's distribution", "Route53", DISTRIBUTION, false),
            ("another family's web ACL", "Route53", WEB_ACL, false),
            ("another family's hosted zone", "Route53", HOSTED_ZONE, false),
        ];
        for (case, plan_family, resource, expected) in rows {
            let actual = is_required_resource(plan_family, resource);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected is_required_resource == {expected}, got {actual}"
            );
        }
    }

    /// A removal is weighed against the set it leaves behind: the optional resources come
    /// off, the plan family's required ones do not, the set never empties, a resource the
    /// subscription does not cover is refused, and a subscription that is not active, or
    /// that is already applying or pending a change of its own, refuses the change
    /// outright.
    #[test]
    fn a_removal_is_weighed_against_the_set_it_leaves_behind() {
        struct Case {
            label: &'static str,
            plan_family: &'static str,
            held: &'static [&'static str],
            status: Status,
            /// Whether the subscription already has a change scheduled on it.
            scheduled: bool,
            removed: &'static [&'static str],
            expected: Result<&'static [&'static str], ResourceChangeRefusal>,
        }

        let cases = [
            Case {
                label: "an optional resource of the CloudFront pair",
                plan_family: "CloudFront",
                held: &[DISTRIBUTION, WEB_ACL, HOSTED_ZONE],
                status: Status::Active,
                scheduled: false,
                removed: &[HOSTED_ZONE],
                expected: Ok(&[DISTRIBUTION, WEB_ACL]),
            },
            Case {
                label: "several optional resources at once",
                plan_family: "CloudFront",
                held: &[DISTRIBUTION, WEB_ACL, HOSTED_ZONE, KEY_VALUE_STORE],
                status: Status::Active,
                scheduled: false,
                removed: &[KEY_VALUE_STORE, HOSTED_ZONE],
                expected: Ok(&[DISTRIBUTION, WEB_ACL]),
            },
            Case {
                label: "the required distribution",
                plan_family: "CloudFront",
                held: &[DISTRIBUTION, WEB_ACL, HOSTED_ZONE],
                status: Status::Active,
                scheduled: false,
                removed: &[DISTRIBUTION],
                expected: Err(ResourceChangeRefusal::Required(vec![
                    DISTRIBUTION.to_string()
                ])),
            },
            Case {
                label: "the required web ACL, named after an optional resource",
                plan_family: "CloudFront",
                held: &[DISTRIBUTION, WEB_ACL, HOSTED_ZONE],
                status: Status::Active,
                scheduled: false,
                removed: &[HOSTED_ZONE, WEB_ACL],
                expected: Err(ResourceChangeRefusal::Required(vec![WEB_ACL.to_string()])),
            },
            Case {
                label: "both required resources at once",
                plan_family: "CloudFront",
                held: &[DISTRIBUTION, WEB_ACL, HOSTED_ZONE],
                status: Status::Active,
                scheduled: false,
                removed: &[DISTRIBUTION, WEB_ACL],
                expected: Err(ResourceChangeRefusal::Required(vec![
                    DISTRIBUTION.to_string(),
                    WEB_ACL.to_string(),
                ])),
            },
            Case {
                label: "a resource the subscription does not cover",
                plan_family: "CloudFront",
                held: &[DISTRIBUTION, WEB_ACL],
                status: Status::Active,
                scheduled: false,
                removed: &[HOSTED_ZONE],
                expected: Err(ResourceChangeRefusal::NotAssociated(
                    HOSTED_ZONE.to_string(),
                )),
            },
            Case {
                label: "the only resource of a family with no required composition",
                plan_family: "Route53",
                held: &[HOSTED_ZONE],
                status: Status::Active,
                scheduled: false,
                removed: &[HOSTED_ZONE],
                expected: Err(ResourceChangeRefusal::LeavesNoResources),
            },
            Case {
                label: "every resource of a family with no required composition",
                plan_family: "Route53",
                held: &[HOSTED_ZONE, KEY_VALUE_STORE],
                status: Status::Active,
                scheduled: false,
                removed: &[HOSTED_ZONE, KEY_VALUE_STORE],
                expected: Err(ResourceChangeRefusal::LeavesNoResources),
            },
            Case {
                label: "one of two resources of a family with no required composition",
                plan_family: "Route53",
                held: &[HOSTED_ZONE, KEY_VALUE_STORE],
                status: Status::Active,
                scheduled: false,
                removed: &[KEY_VALUE_STORE],
                expected: Ok(&[HOSTED_ZONE]),
            },
            Case {
                label: "a subscription waiting for approval",
                plan_family: "CloudFront",
                held: &[DISTRIBUTION, WEB_ACL, HOSTED_ZONE],
                status: Status::PendingApproval,
                scheduled: false,
                removed: &[HOSTED_ZONE],
                expected: Err(ResourceChangeRefusal::Status(Status::PendingApproval)),
            },
            Case {
                label: "a subscription applying a change",
                plan_family: "CloudFront",
                held: &[DISTRIBUTION, WEB_ACL, HOSTED_ZONE],
                status: Status::SyncInProgress,
                scheduled: false,
                removed: &[HOSTED_ZONE],
                expected: Err(ResourceChangeRefusal::Status(Status::SyncInProgress)),
            },
            Case {
                label: "a failed subscription",
                plan_family: "CloudFront",
                held: &[DISTRIBUTION, WEB_ACL, HOSTED_ZONE],
                status: Status::Failed,
                scheduled: false,
                removed: &[HOSTED_ZONE],
                expected: Err(ResourceChangeRefusal::Status(Status::Failed)),
            },
            Case {
                label: "one of two distributions a CloudFront subscription holds",
                plan_family: "CloudFront",
                held: &[DISTRIBUTION, SECOND_DISTRIBUTION, WEB_ACL],
                status: Status::Active,
                scheduled: false,
                removed: &[DISTRIBUTION],
                expected: Ok(&[SECOND_DISTRIBUTION, WEB_ACL]),
            },
            Case {
                label: "both distributions a CloudFront subscription holds",
                plan_family: "CloudFront",
                held: &[DISTRIBUTION, SECOND_DISTRIBUTION, WEB_ACL],
                status: Status::Active,
                scheduled: false,
                removed: &[DISTRIBUTION, SECOND_DISTRIBUTION],
                expected: Err(ResourceChangeRefusal::Required(vec![
                    DISTRIBUTION.to_string(),
                    SECOND_DISTRIBUTION.to_string(),
                ])),
            },
            Case {
                label: "a subscription with a change already scheduled",
                plan_family: "CloudFront",
                held: &[DISTRIBUTION, WEB_ACL, HOSTED_ZONE],
                status: Status::Active,
                scheduled: true,
                removed: &[HOSTED_ZONE],
                expected: Err(ResourceChangeRefusal::ChangePending),
            },
        ];

        for case in cases {
            let subject = record(case.plan_family, case.held, case.status, case.scheduled);
            let requested: Vec<String> =
                case.removed.iter().map(|arn| (*arn).to_string()).collect();
            let actual = plan_resource_removal(&subject, &requested);
            let expected = case
                .expected
                .map(|retained| retained.iter().map(|arn| (*arn).to_string()).collect());
            assert_eq!(
                actual, expected,
                "case {:?}: expected {expected:?}, got {actual:?}",
                case.label
            );
        }
    }

    /// An addition is weighed against the set it would leave the subscription covering:
    /// the named resources go on the end in request order, a repeat inside one request
    /// names one resource, a resource the subscription already covers is nothing to add,
    /// the set never grows past the modelled maximum, and a subscription that is not
    /// active, or that is pending a change of its own, refuses the change outright.
    #[test]
    fn an_addition_is_weighed_against_the_set_it_would_leave_covered() {
        struct Case {
            label: &'static str,
            plan_family: &'static str,
            held: &'static [&'static str],
            status: Status,
            /// Whether the subscription already has a change scheduled on it.
            scheduled: bool,
            added: &'static [&'static str],
            expected: Result<&'static [&'static str], ResourceChangeRefusal>,
        }

        /// A stored set of exactly [`MAX_SUBSCRIPTION_RESOURCES`] resources, and one of
        /// one short of it, so the rows below walk the modelled bound.
        const FULL: &[&str] = &[
            DISTRIBUTION,
            WEB_ACL,
            "arn:aws:route53:::hostedzone/Z1",
            "arn:aws:route53:::hostedzone/Z2",
            "arn:aws:route53:::hostedzone/Z3",
            "arn:aws:route53:::hostedzone/Z4",
            "arn:aws:route53:::hostedzone/Z5",
            "arn:aws:route53:::hostedzone/Z6",
            "arn:aws:route53:::hostedzone/Z7",
            "arn:aws:route53:::hostedzone/Z8",
        ];
        const ALMOST_FULL: &[&str] = &[
            DISTRIBUTION,
            WEB_ACL,
            "arn:aws:route53:::hostedzone/Z1",
            "arn:aws:route53:::hostedzone/Z2",
            "arn:aws:route53:::hostedzone/Z3",
            "arn:aws:route53:::hostedzone/Z4",
            "arn:aws:route53:::hostedzone/Z5",
            "arn:aws:route53:::hostedzone/Z6",
            "arn:aws:route53:::hostedzone/Z7",
        ];

        let cases = [
            Case {
                label: "one optional resource joins the CloudFront pair",
                plan_family: "CloudFront",
                held: &[DISTRIBUTION, WEB_ACL],
                status: Status::Active,
                scheduled: false,
                added: &[HOSTED_ZONE],
                expected: Ok(&[DISTRIBUTION, WEB_ACL, HOSTED_ZONE]),
            },
            Case {
                label: "two resources at once, in the order the request names them",
                plan_family: "CloudFront",
                held: &[DISTRIBUTION, WEB_ACL],
                status: Status::Active,
                scheduled: false,
                added: &[KEY_VALUE_STORE, HOSTED_ZONE],
                expected: Ok(&[DISTRIBUTION, WEB_ACL, KEY_VALUE_STORE, HOSTED_ZONE]),
            },
            Case {
                label: "a second distribution of a CloudFront subscription",
                plan_family: "CloudFront",
                held: &[DISTRIBUTION, WEB_ACL],
                status: Status::Active,
                scheduled: false,
                added: &[SECOND_DISTRIBUTION],
                expected: Ok(&[DISTRIBUTION, WEB_ACL, SECOND_DISTRIBUTION]),
            },
            Case {
                label: "a resource named twice names one resource",
                plan_family: "Route53",
                held: &[HOSTED_ZONE],
                status: Status::Active,
                scheduled: false,
                added: &[KEY_VALUE_STORE, KEY_VALUE_STORE],
                expected: Ok(&[HOSTED_ZONE, KEY_VALUE_STORE]),
            },
            Case {
                label: "a resource the subscription already covers",
                plan_family: "CloudFront",
                held: &[DISTRIBUTION, WEB_ACL],
                status: Status::Active,
                scheduled: false,
                added: &[WEB_ACL],
                expected: Err(ResourceChangeRefusal::AlreadyAssociated(
                    WEB_ACL.to_string(),
                )),
            },
            Case {
                label: "an already covered resource named behind a new one",
                plan_family: "CloudFront",
                held: &[DISTRIBUTION, WEB_ACL],
                status: Status::Active,
                scheduled: false,
                added: &[HOSTED_ZONE, DISTRIBUTION],
                expected: Err(ResourceChangeRefusal::AlreadyAssociated(
                    DISTRIBUTION.to_string(),
                )),
            },
            Case {
                label: "the last resource the subscription has room for",
                plan_family: "CloudFront",
                held: ALMOST_FULL,
                status: Status::Active,
                scheduled: false,
                added: &[KEY_VALUE_STORE],
                expected: Ok(&[
                    DISTRIBUTION,
                    WEB_ACL,
                    "arn:aws:route53:::hostedzone/Z1",
                    "arn:aws:route53:::hostedzone/Z2",
                    "arn:aws:route53:::hostedzone/Z3",
                    "arn:aws:route53:::hostedzone/Z4",
                    "arn:aws:route53:::hostedzone/Z5",
                    "arn:aws:route53:::hostedzone/Z6",
                    "arn:aws:route53:::hostedzone/Z7",
                    KEY_VALUE_STORE,
                ]),
            },
            Case {
                label: "one resource past the modelled maximum",
                plan_family: "CloudFront",
                held: FULL,
                status: Status::Active,
                scheduled: false,
                added: &[KEY_VALUE_STORE],
                expected: Err(ResourceChangeRefusal::TooManyResources {
                    covered: MAX_SUBSCRIPTION_RESOURCES,
                    adding: 1,
                }),
            },
            Case {
                label: "two resources where there is room for one",
                plan_family: "CloudFront",
                held: ALMOST_FULL,
                status: Status::Active,
                scheduled: false,
                added: &[KEY_VALUE_STORE, HOSTED_ZONE],
                expected: Err(ResourceChangeRefusal::TooManyResources {
                    covered: MAX_SUBSCRIPTION_RESOURCES - 1,
                    adding: 2,
                }),
            },
            Case {
                label: "a subscription waiting for approval",
                plan_family: "CloudFront",
                held: &[DISTRIBUTION, WEB_ACL],
                status: Status::PendingApproval,
                scheduled: false,
                added: &[HOSTED_ZONE],
                expected: Err(ResourceChangeRefusal::Status(Status::PendingApproval)),
            },
            Case {
                label: "a subscription applying a change",
                plan_family: "CloudFront",
                held: &[DISTRIBUTION, WEB_ACL],
                status: Status::SyncInProgress,
                scheduled: false,
                added: &[HOSTED_ZONE],
                expected: Err(ResourceChangeRefusal::Status(Status::SyncInProgress)),
            },
            Case {
                label: "a failed subscription",
                plan_family: "CloudFront",
                held: &[DISTRIBUTION, WEB_ACL],
                status: Status::Failed,
                scheduled: false,
                added: &[HOSTED_ZONE],
                expected: Err(ResourceChangeRefusal::Status(Status::Failed)),
            },
            Case {
                label: "a subscription with a change already scheduled",
                plan_family: "CloudFront",
                held: &[DISTRIBUTION, WEB_ACL],
                status: Status::Active,
                scheduled: true,
                added: &[HOSTED_ZONE],
                expected: Err(ResourceChangeRefusal::ChangePending),
            },
        ];

        for case in cases {
            let subject = record(case.plan_family, case.held, case.status, case.scheduled);
            let requested: Vec<String> = case.added.iter().map(|arn| (*arn).to_string()).collect();
            let actual = plan_resource_addition(&subject, &requested);
            let expected = case
                .expected
                .map(|covering| covering.iter().map(|arn| (*arn).to_string()).collect());
            assert_eq!(
                actual, expected,
                "case {:?}: expected {expected:?}, got {actual:?}",
                case.label
            );
        }
    }

    /// A stored subscription under `arn` covering `resources`.
    fn covering(arn: &str, resources: &[&str]) -> SubscriptionRecord {
        let mut stored = record("CloudFront", resources, Status::Active, false);
        stored.arn = arn.to_string();
        stored
    }

    /// RULE: a resource is covered by at most one subscription, so the resources a request
    /// names are weighed against every stored subscription's own set — whatever status it
    /// is in — and the first resource the request named that is taken is the answer.
    ///
    /// The rows differ along which stored subscription holds the resource, the kind of
    /// resource, where in the request's list it sits, how many are taken, and whether the
    /// subscription holding it is the one the request is about.
    #[test]
    fn a_resource_is_covered_by_at_most_one_subscription() {
        const FIRST: &str = "arn:aws:pricingplanmanager::000000000000:subscription/sub_first";
        const SECOND: &str = "arn:aws:pricingplanmanager::000000000000:subscription/sub_second";
        let mut pending = covering(SECOND, &[HOSTED_ZONE]);
        pending.status = Status::PendingApproval;
        let stored = vec![covering(FIRST, &[DISTRIBUTION, WEB_ACL]), pending];

        // (case, the resources the request names, the subscription it is about, the
        //  resource and the holder it is refused for)
        type Row<'a> = (
            &'a str,
            &'a [&'a str],
            Option<&'a str>,
            Option<(&'a str, &'a str)>,
        );
        let rows: &[Row<'_>] = &[
            (
                "a resource no subscription covers",
                &[SECOND_DISTRIBUTION, KEY_VALUE_STORE],
                None,
                None,
            ),
            (
                "the distribution an active subscription covers",
                &[DISTRIBUTION, SECOND_DISTRIBUTION],
                None,
                Some((DISTRIBUTION, FIRST)),
            ),
            (
                "a web ACL an active subscription covers",
                &[SECOND_DISTRIBUTION, WEB_ACL],
                None,
                Some((WEB_ACL, FIRST)),
            ),
            (
                "a resource a subscription waiting for approval covers",
                &[HOSTED_ZONE],
                None,
                Some((HOSTED_ZONE, SECOND)),
            ),
            (
                "two taken resources: the first the request named answers",
                &[WEB_ACL, HOSTED_ZONE],
                None,
                Some((WEB_ACL, FIRST)),
            ),
            (
                "the holder is the subscription the request is about",
                &[DISTRIBUTION, WEB_ACL],
                Some(FIRST),
                None,
            ),
            (
                "another subscription still holds it",
                &[DISTRIBUTION, HOSTED_ZONE],
                Some(FIRST),
                Some((HOSTED_ZONE, SECOND)),
            ),
            ("no resources at all", &[], None, None),
        ];

        for (case, requested, subscription, expected) in rows {
            let requested: Vec<String> =
                requested.iter().map(|arn| (*arn).to_string()).collect();
            let actual = resource_in_another_subscription(&stored, &requested, *subscription);
            let expected = expected.map(|(resource, holder)| CoveredResource {
                resource_arn: resource.to_string(),
                subscription_arn: holder.to_string(),
            });
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the refusal states the fixed text and the offending resource ARN the request
    /// named, and names that resource as the one that failed validation.
    #[test]
    fn the_refusal_names_the_resource_the_request_asked_for() {
        for resource in [DISTRIBUTION, WEB_ACL, HOSTED_ZONE] {
            let refusal = resource_already_subscribed(resource);
            assert_eq!(
                refusal.message,
                format!(
                    "Cannot create subscription with {resource}: resource must belong to \
                     your account and not be used in another subscription."
                ),
                "case {resource:?}: got {:?}",
                refusal.message
            );
            assert_eq!(
                refusal.resource_id.as_deref(),
                Some(resource),
                "case {resource:?}: got {:?}",
                refusal.resource_id
            );
        }
    }
}
