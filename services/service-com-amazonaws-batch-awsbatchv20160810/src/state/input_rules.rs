// SPDX-License-Identifier: CC0-1.0

//! The rules AWS Batch holds a REQUEST to before it resolves anything: the pattern an
//! identifier it reads as a name must match, the ARN syntax an identity it composes must
//! have, and the capacity a `capacityLimits` entry must allocate.
//!
//! Each of them is a boundary the service applies AHEAD of the lookup an operation would
//! otherwise do, so a value outside it is refused rather than resolved — and a reference
//! that resolves to nothing stays the separate, later refusal
//! ([`super::does_not_exist_sentence`]) or, for an operation whose post-state already
//! holds, the empty success. The rules live here, once, because the same sentence is
//! answered by every operation that reads the same kind of value: one rule, one wording,
//! whichever handler applies it.
//!
//! Three of them publish a sentence the model does not state, so each constant below says
//! what the recorded behaviour of that refusal class is, and the wording is the service's,
//! never one call's values.

use super::*;

/// What every ARN starts with, and therefore how a member documented as "the name or ARN"
/// of a resource tells the two forms apart.
///
/// The coarsest reading of the ARN form, and the only one such a member needs: whether the
/// value is a NAME or an ARN. What makes an ARN well formed is `service_common`'s single
/// parse ([`is_well_formed_arn`]).
pub const ARN_PREFIX: &str = "arn:";

/// The name pattern AWS Batch publishes when it refuses an identifier outside it, spelled
/// exactly as the service spells it in that refusal.
///
/// The members this governs (`CancelJob$jobId`, `TerminateJob$jobId`,
/// `DeregisterJobDefinition$jobDefinition`, `DeleteConsumableResource$consumableResource`)
/// are each a plain `String` with no `@pattern` trait, so the framework admits anything
/// and the rule is applied in the handler; unlike the name refusals of the creates
/// ([`super::name_pattern_detail`]), this class of refusal states the pattern itself.
pub const RESOURCE_NAME_PATTERN: &str = "[a-zA-Z_0-9-]{1,128}";

/// The longest identifier [`RESOURCE_NAME_PATTERN`] admits, as that pattern spells it.
pub const MAX_IDENTIFIER_NAME_LEN: usize = 128;

/// How AWS Batch words the refusal of an identifier outside [`RESOURCE_NAME_PATTERN`]: the
/// value as the request submitted it, then the pattern it did not match.
///
/// The identifier is echoed verbatim because it is the only thing the caller can fix, and
/// the pattern is published here (unlike the create-time name refusals) because the
/// service refuses the value before it has any resource to talk about.
pub fn unsupported_name_message(identifier: &str) -> String {
    format!("Provided name {identifier} is not supported pattern{RESOURCE_NAME_PATTERN}")
}

/// The refusal an identifier an operation resolves BY NAME draws when it is outside
/// [`RESOURCE_NAME_PATTERN`], and `None` for one inside it.
///
/// `identifier` is the value as submitted — what the refusal echoes — and `name` is the
/// name the service reads out of it, which is the identifier itself for a member that is
/// only ever a name and the name part for one that may carry a revision
/// ([`super::job_definition_reference_name`]). A value in the ARN form is a well-formed
/// identifier of the OTHER form every such member documents, so it is not held to the name
/// pattern.
///
/// This is what keeps a malformed identifier from reaching a lookup that would miss and
/// report the operation's empty success: a cancel, a deregister or a delete of a name no
/// resource could ever carry is a defect of the REQUEST, not a post-state that already
/// holds.
pub fn unsupported_identifier(identifier: &str, name: &str) -> Option<ClientException> {
    if identifier.starts_with(ARN_PREFIX)
        || is_valid_resource_name(name, MAX_IDENTIFIER_NAME_LEN)
    {
        return None;
    }
    Some(client_exception(unsupported_name_message(identifier)))
}

/// How the ARN layer words the refusal of a value it cannot read as an ARN: the value,
/// then that it is not one.
///
/// The value is the ARN the SERVICE composed from the request, which is what makes the
/// refusal actionable — the caller supplied a part of it.
pub fn not_a_valid_arn_message(arn: &str) -> String {
    format!("{arn} is not a valid AmazonResourceName (ARN)")
}

/// Whether `value` is a syntactically well-formed ARN: the six colon-separated fields
/// `service_common`'s single ARN parse requires (`arn`, a non-empty partition and vendor,
/// a Region, an account and a non-empty resource), with an account field that is either
/// empty — as a partition-global or account-less resource's ARN is written — or an AWS
/// account id.
///
/// The parse and the account-id form are `service_common::arn`'s, so this service cannot
/// disagree with another about what an ARN is; what is local is only which of its fields
/// this refusal requires.
///
/// This is SYNTAX only: an ARN that is well formed and names nothing is a different
/// failure, reported by the lookup that misses it.
pub fn is_well_formed_arn(value: &str) -> bool {
    ::service_common::arn::parse(value).is_some_and(|arn| {
        arn.account.is_empty() || ::service_common::arn::is_account_id(arn.account)
    })
}

/// The identity a `CreateQuotaShare` composes out of its request: the `jobQueue` the share
/// hangs off, then the share's own name under it.
///
/// A quota share is named UNDER its queue ([`super::quota_share_arn`]), so a request that
/// refers to the queue by ARN composes the share's own ARN, and the composed value is what
/// the ARN layer reads before this service resolves the queue.
pub fn composed_quota_share_identity(job_queue: &str, quota_share_name: &str) -> String {
    format!("{job_queue}/quota-share/{quota_share_name}")
}

/// The refusal a `CreateQuotaShare` draws when the identity it composes
/// ([`composed_quota_share_identity`]) is in the ARN form but is not a well-formed ARN,
/// and `None` when the queue is referred to by name or by an ARN that is well formed.
///
/// A malformed ARN is an EARLIER boundary than a well-formed reference that names no
/// queue: the value never reaches this service's lookup, so it is refused by the ARN layer
/// under that layer's own code ([`crate::scaffolded::FRONTEND_REFUSAL_CODE`], HTTP 400)
/// rather than with the `ClientException` a queue that is not there draws. A client
/// branching on the two tells "fix the request" from "create the queue and retry" by the
/// code alone.
///
/// Generic over the operation's modeled error enum, so the handler returns it through its
/// own error channel.
pub fn malformed_quota_share_identity<E>(
    job_queue: &str,
    quota_share_name: &str,
) -> Option<crate::scaffolded::Scaffolded<E>> {
    let identity = composed_quota_share_identity(job_queue, quota_share_name);
    (job_queue.starts_with(ARN_PREFIX) && !is_well_formed_arn(&identity)).then(|| {
        crate::scaffolded::Scaffolded::frontend_refusal(not_a_valid_arn_message(&identity))
    })
}

/// How AWS Batch words the refusal of a `CreateQuotaShare` naming a share the queue
/// already carries: the share's name and the queue as the request spells it.
///
/// Both variable parts come from the REQUEST — the queue slot echoes the reference given,
/// a name staying a name and an ARN staying an ARN — so a caller reconciling shares can
/// tell WHICH name collided on WHICH queue, which the store's own duplicate-key detail
/// ([`super::OBJECT_ALREADY_EXISTS`], the wording the older resource families answer) does
/// not say. Recorded for this operation bare: this class carries no frontend envelope.
pub fn quota_share_exists_message(quota_share_name: &str, job_queue: &str) -> String {
    format!("QuotaShare with name {quota_share_name} already exists for job queue {job_queue}")
}

/// What AWS Batch answers a quota-share `capacityLimits` that is present and allocates
/// nothing.
///
/// The member is `@required` and carries the capacity the share IS, so an empty list
/// identifies no share this service can hold — the same refusal whether the list arrives
/// on the create or replaces the stored one on the update.
pub const CAPACITY_LIMITS_REQUIRED: &str = "capacityLimits is required and cannot be empty.";

/// What AWS Batch answers a multi-node `nodeProperties` whose `mainNode` is not below its
/// `numNodes`.
///
/// `NodeProperties$mainNode` is "the node index for the main node of a multi-node parallel
/// job. This node index value must be fewer than the number of nodes", and neither member
/// carries a `@range` or a cross-member constraint, so the rule is applied in the handler.
/// The sentence is FIXED: it states which index broke which bound, and carries neither a
/// member path nor either submitted value — a refusal of this class names no value of the
/// request.
pub const MAIN_NODE_OUT_OF_RANGE: &str =
    "Main node index is out of range, must be less than numNodes.";

/// How AWS Batch words the refusal of a submit onto a job queue whose state is not
/// `ENABLED`.
///
/// `JQState`: "If the job queue state is `DISABLED`, new jobs can't be added to the queue,
/// but jobs already in the queue can finish." The sentence states that PRECONDITION rather
/// than the state the queue happens to be in, and its one variable part is the ARN the
/// request's name-or-ARN reference resolved to — the same `<Kind> <reference> <condition>.`
/// shape this plane's lookup miss carries ([`super::not_found_exception`]).
///
/// Shared by every submit that admits work onto a queue — the job plane's `SubmitJob` and
/// the service-job plane's `SubmitServiceJob` — because the precondition is the queue's,
/// not the operation's.
pub fn job_queue_not_enabled_message(job_queue_arn: &str) -> String {
    format!("JobQueue {job_queue_arn} not ENABLED.")
}

/// How AWS Batch words the refusal of a submit that names no `quotaShareName` onto a job
/// queue whose scheduling policy carries a quota share.
///
/// `SubmitServiceJob$quotaShareName`: "Don't specify this parameter if the job queue
/// doesn't have a quota share scheduling policy. If the job queue has a quota share
/// scheduling policy, then this parameter must be specified." The member is an optional
/// unconstrained `String`, so the requirement is CONDITIONAL on the queue the request names
/// and is decided by the service layer rather than at the boundary — which is why the
/// sentence is stated BARE, as this plane's other state-derived refusals are, and why it
/// names the QUEUE it is about the way they name it ([`job_queue_not_enabled_message`],
/// [`super::not_found_exception`]): the kind, then the ARN the request's reference resolved
/// to.
///
/// The requirement itself is the model's own, restated as the service states a rule: the
/// member that must be given, and the queue that requires it.
pub fn quota_share_name_required_message(job_queue_arn: &str) -> String {
    format!("quotaShareName must be specified for JobQueue {job_queue_arn}.")
}

/// The smallest capacity a `capacityLimits` entry may allocate.
///
/// Neither `QuotaShareCapacityLimit$maxCapacity` nor `CapacityLimit$maxCapacity` carries a
/// `@range` trait, so the bound is the service's: a non-positive allocation would seed the
/// capacity arithmetic a placement is decided by with a quantity no resource can satisfy,
/// and both recordings of such a request are refusals.
pub const MIN_MAX_CAPACITY: i32 = 1;

/// What AWS Batch answers a QUOTA SHARE whose `capacityLimits` carries a non-positive
/// `maxCapacity`.
pub const QUOTA_SHARE_MAX_CAPACITY_POSITIVE: &str = "maxCapacity must be greater than 0.";

/// What AWS Batch answers a SERVICE ENVIRONMENT whose `capacityLimits` carries a
/// non-positive `maxCapacity`.
///
/// The same rule as [`QUOTA_SHARE_MAX_CAPACITY_POSITIVE`], worded as the service
/// environment plane words it: the two planes are recorded stating this bound in their own
/// sentence, so each keeps its own.
pub const SERVICE_ENVIRONMENT_MAX_CAPACITY_POSITIVE: &str = "Max capacity must be more than 0.";

/// The detail a quota share's `capacityLimits` is refused with, or `None` when every entry
/// allocates capacity the share can hold.
///
/// Shared by `CreateQuotaShare` and `UpdateQuotaShare`, which hold the same list shape to
/// the same rule: an update's list REPLACES the stored one, so a list the create would not
/// accept is not a list the update may store either.
pub fn quota_share_capacity_limits_detail(
    limits: &[QuotaShareCapacityLimit],
) -> Option<&'static str> {
    if limits.is_empty() {
        return Some(CAPACITY_LIMITS_REQUIRED);
    }
    limits
        .iter()
        .any(|limit| limit.max_capacity < MIN_MAX_CAPACITY)
        .then_some(QUOTA_SHARE_MAX_CAPACITY_POSITIVE)
}

/// The detail a service environment's `capacityLimits` is refused with, or `None` when
/// every entry that states a `maxCapacity` allocates capacity the environment can hold.
///
/// `CapacityLimit$maxCapacity` is optional, so an entry that states none states no
/// capacity to hold to the bound. Shared by `CreateServiceEnvironment` and
/// `UpdateServiceEnvironment` for the reason the quota-share rule is shared.
pub fn service_environment_capacity_limits_detail(
    limits: &[CapacityLimit],
) -> Option<&'static str> {
    limits
        .iter()
        .any(|limit| {
            limit
                .max_capacity
                .is_some_and(|capacity| capacity < MIN_MAX_CAPACITY)
        })
        .then_some(SERVICE_ENVIRONMENT_MAX_CAPACITY_POSITIVE)
}
