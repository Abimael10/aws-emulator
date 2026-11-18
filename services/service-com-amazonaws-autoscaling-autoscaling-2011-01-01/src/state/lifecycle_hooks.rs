// SPDX-License-Identifier: CC0-1.0

//! The lifecycle hooks of an Auto Scaling group: the group's own named collection,
//! and the rules every writer of it shares.
//!
//! A lifecycle hook is a NAMED CHILD of one Auto Scaling group, so it is stored on
//! [`StoredGroup`] itself ([`StoredGroup::lifecycle_hooks`]): one collection, keyed
//! by the hook's name, that `PutLifecycleHook` upserts into, `DescribeLifecycleHooks`
//! projects from, `DeleteLifecycleHook` removes from, and the group's own delete takes
//! with the group. Nothing in this crate holds a second copy.
//!
//! # The upsert
//!
//! `PutLifecycleHook` "creates OR updates a lifecycle hook"
//! (`smithy_query(query="operation",operation="PutLifecycleHook")`), and the model
//! documents the update as PER MEMBER: `LifecycleTransition` and `RoleARN` are
//! "Required for new lifecycle hooks, but optional when updating existing hooks", and
//! an empty `NotificationTargetARN` "overrides the current ARN". So a request carries
//! three states for each member, which [`LifecycleHookPut`] keeps apart: absent (keep
//! what is stored, or take the documented default on create), present (overwrite), and
//! the empty notification target (clear the stored one). The documented defaults are
//! applied ON CREATE only, so an update that names one member cannot silently reset
//! another to its default.

use std::collections::BTreeMap;

use super::StoredGroup;

/// The most lifecycle hooks one Auto Scaling group may hold.
///
/// From the model's own documentation for `PutLifecycleHook`: "If you exceed your
/// maximum limit of lifecycle hooks, which by default is 50 per Auto Scaling group,
/// the call fails." The member carries no modelled bound, so the handler checks it and
/// answers the operation's modelled `LimitExceededFault` past it.
pub const MAX_LIFECYCLE_HOOKS_PER_GROUP: usize = 50;

/// `HeartbeatTimeout` when a request that CREATES a hook omits it.
///
/// From the model's own documentation for `PutLifecycleHookType$HeartbeatTimeout`:
/// "The default value is 3600 seconds (1 hour)."
pub const DEFAULT_HEARTBEAT_TIMEOUT: i32 = 3600;

/// The smallest `HeartbeatTimeout` the service accepts, from the same documentation:
/// "The range is from 30 to 7200 seconds."
pub const MIN_HEARTBEAT_TIMEOUT: i32 = 30;

/// The largest `HeartbeatTimeout` the service accepts (same documentation).
pub const MAX_HEARTBEAT_TIMEOUT: i32 = 7200;

/// The ceiling on a hook's `GlobalTimeout`.
///
/// From the model's own documentation for `LifecycleHook$GlobalTimeout`: "The maximum
/// time, in seconds, that an instance can remain in a wait state. The maximum is 172800
/// seconds (48 hours) or 100 times HeartbeatTimeout, whichever is smaller."
pub const MAX_GLOBAL_TIMEOUT: i32 = 172_800;

/// How many heartbeat timeouts a `GlobalTimeout` spans before [`MAX_GLOBAL_TIMEOUT`]
/// caps it (same documentation: "100 times HeartbeatTimeout").
pub const GLOBAL_TIMEOUT_HEARTBEATS: i32 = 100;

/// `DefaultResult` when a request that CREATES a hook omits it.
///
/// From the model's own documentation for `PutLifecycleHookType$DefaultResult`: "The
/// default value is ABANDON."
pub const DEFAULT_LIFECYCLE_ACTION_RESULT: &str = "ABANDON";

/// The vendors a lifecycle hook's notification target may be a resource of.
///
/// The model documents the member as the ARN of a target "that Amazon EC2 Auto Scaling
/// uses to notify you when an instance is in a wait state for the lifecycle hook. You
/// can specify either an Amazon SNS topic or an Amazon SQS queue"
/// (`smithy_query(query="doc",shape="PutLifecycleHookType",member="NotificationTargetARN")`),
/// so the target is a resource of a service Amazon EC2 Auto Scaling publishes a
/// notification to, and the vendor field of the ARN is what says which. The refusal
/// below states this set, so a caller is told which vendors it may choose from.
pub const NOTIFICATION_TARGET_VENDORS: &[&str] = &["sqs", "sns", "lambda"];

/// The result that lets the transition a lifecycle hook holds an instance at CARRY ON,
/// from the same documentation ("Valid values: CONTINUE | ABANDON").
pub const LIFECYCLE_ACTION_CONTINUE: &str = "CONTINUE";

/// The results a lifecycle hook may default to, from the same documentation: "Valid
/// values: CONTINUE | ABANDON".
pub const LIFECYCLE_ACTION_RESULTS: &[&str] =
    &[LIFECYCLE_ACTION_CONTINUE, DEFAULT_LIFECYCLE_ACTION_RESULT];

/// One lifecycle hook of a group, as it is stored.
///
/// The name is the key of [`StoredGroup::lifecycle_hooks`] rather than a field, so a
/// hook cannot be stored under a name that disagrees with its own.
#[derive(Debug, Clone)]
pub struct StoredLifecycleHook {
    /// `LifecycleTransition`: the transition the hook acts on. Required to create a
    /// hook, so every stored hook has one.
    pub lifecycle_transition: String,
    /// `NotificationTargetARN`, absent for a hook with no notification target — which
    /// is what an empty value in a request leaves behind.
    pub notification_target_arn: Option<String>,
    /// `RoleARN`.
    pub role_arn: Option<String>,
    /// `NotificationMetadata`.
    pub notification_metadata: Option<String>,
    /// `HeartbeatTimeout`, as requested or [`DEFAULT_HEARTBEAT_TIMEOUT`].
    pub heartbeat_timeout: i32,
    /// `DefaultResult`, as requested or [`DEFAULT_LIFECYCLE_ACTION_RESULT`].
    pub default_result: String,
}

impl StoredLifecycleHook {
    /// `GlobalTimeout`: a value DERIVED from the hook's own heartbeat timeout rather
    /// than one a caller supplies — "172800 seconds (48 hours) or 100 times
    /// HeartbeatTimeout, whichever is smaller".
    pub fn global_timeout(&self) -> i32 {
        self.heartbeat_timeout
            .saturating_mul(GLOBAL_TIMEOUT_HEARTBEATS)
            .min(MAX_GLOBAL_TIMEOUT)
    }
}

/// The hook members ONE request carries, each `Some` only where the request named it.
///
/// This is what keeps an omitted member apart from a supplied one, which is the whole
/// of the update rule: see this module's own documentation.
#[derive(Debug, Clone, Default)]
pub struct LifecycleHookPut {
    pub lifecycle_transition: Option<String>,
    pub notification_target_arn: Option<String>,
    pub role_arn: Option<String>,
    pub notification_metadata: Option<String>,
    pub heartbeat_timeout: Option<i32>,
    pub default_result: Option<String>,
}

/// Why a lifecycle hook could not be stored, for the operation to answer the error
/// class each situation belongs to.
#[derive(Debug, PartialEq, Eq)]
pub enum LifecycleHookRefusal {
    /// The request creates a hook but names no transition for it to act on.
    TransitionMissing,
    /// The transition the request names is not one a hook can be created for. The
    /// refusal states the member and the admissible transitions, so what the request
    /// named is not carried.
    UnknownTransition,
    /// The default result the request names is not one a hook can take. Stated the same
    /// way, so what the request named is not carried either.
    UnknownResult,
    /// The notification target the request names is a resource of a vendor this service
    /// does not publish notifications to; carries that vendor, which the refusal reports.
    UnsupportedNotificationTargetVendor(String),
    /// The heartbeat timeout the request names is outside the documented range. The
    /// refusal states the member and both documented bounds, so what the request named is
    /// not carried.
    HeartbeatOutOfRange,
    /// The group already holds [`MAX_LIFECYCLE_HOOKS_PER_GROUP`] hooks and this request
    /// would create one more.
    LimitExceeded,
}

impl LifecycleHookRefusal {
    /// What the caller is told, for the hook the request named.
    ///
    /// ONE spelling per situation for every writer of a hook — the operation that
    /// upserts one and the group create that carries specifications — so two of them
    /// cannot refuse the same request differently. The hook's name is the only part
    /// taken from the request where the sentence does not quote the offending value.
    pub fn message(&self, hook_name: &str) -> String {
        match self {
            Self::TransitionMissing => transition_missing_message(hook_name),
            Self::UnknownTransition => unknown_transition_message(),
            Self::UnknownResult => unknown_result_message(),
            Self::UnsupportedNotificationTargetVendor(vendor) => {
                unsupported_notification_target_message(vendor)
            }
            Self::HeartbeatOutOfRange => heartbeat_timeout_violation_message(),
            Self::LimitExceeded => too_many_hooks_message(hook_name),
        }
    }
}

impl StoredGroup {
    /// The hook the group holds under `name`, if it holds one.
    pub fn lifecycle_hook(&self, name: &str) -> Option<&StoredLifecycleHook> {
        self.lifecycle_hooks.get(name)
    }

    /// Create or update the hook `name` of this group, per the update rule in this
    /// module's own documentation.
    pub fn put_lifecycle_hook(
        &mut self,
        name: &str,
        put: &LifecycleHookPut,
    ) -> Result<(), LifecycleHookRefusal> {
        put_lifecycle_hook(&mut self.lifecycle_hooks, name, put)
    }

    /// Remove the hook `name` from this group, answering whether it held one.
    pub fn delete_lifecycle_hook(&mut self, name: &str) -> bool {
        self.lifecycle_hooks.remove(name).is_some()
    }
}

/// The lifecycle hooks of ONE group, keyed by `LifecycleHookName`.
pub type LifecycleHooks = BTreeMap<String, StoredLifecycleHook>;

/// Create or update the hook `name` in `hooks`, per the update rule in this module's
/// own documentation.
///
/// Takes the COLLECTION rather than the group, so a group's hooks can be assembled
/// before the group itself exists — which is what lets `CreateAutoScalingGroup` take
/// its lifecycle hook specifications before it allocates or stores anything.
///
/// Every rule is checked BEFORE the first member is written, so a refused request
/// leaves the hook — and the rest of the collection — exactly as it was.
pub fn put_lifecycle_hook(
    hooks: &mut LifecycleHooks,
    name: &str,
    put: &LifecycleHookPut,
) -> Result<(), LifecycleHookRefusal> {
    validate_lifecycle_hook_members(put)?;

    match hooks.get_mut(name) {
        // An UPDATE: every member the request omits keeps the value the hook already
        // carries, and the empty notification target clears the stored one.
        Some(stored) => {
            if let Some(transition) = put.lifecycle_transition.clone() {
                stored.lifecycle_transition = transition;
            }
            if let Some(target) = put.notification_target_arn.as_deref() {
                stored.notification_target_arn = (!target.is_empty()).then(|| target.to_string());
            }
            if let Some(role) = put.role_arn.clone() {
                stored.role_arn = Some(role);
            }
            if let Some(metadata) = put.notification_metadata.clone() {
                stored.notification_metadata = Some(metadata);
            }
            if let Some(timeout) = put.heartbeat_timeout {
                stored.heartbeat_timeout = timeout;
            }
            if let Some(result) = put.default_result.clone() {
                stored.default_result = result;
            }
            Ok(())
        }
        // A CREATE: the transition is required, the group's own limit applies, and the
        // members the request omits take their documented defaults.
        None => {
            let Some(transition) = put.lifecycle_transition.clone() else {
                return Err(LifecycleHookRefusal::TransitionMissing);
            };
            if hooks.len() >= MAX_LIFECYCLE_HOOKS_PER_GROUP {
                return Err(LifecycleHookRefusal::LimitExceeded);
            }
            hooks.insert(
                name.to_string(),
                StoredLifecycleHook {
                    lifecycle_transition: transition,
                    notification_target_arn: put
                        .notification_target_arn
                        .as_deref()
                        .filter(|target| !target.is_empty())
                        .map(str::to_string),
                    role_arn: put.role_arn.clone(),
                    notification_metadata: put.notification_metadata.clone(),
                    heartbeat_timeout: put.heartbeat_timeout.unwrap_or(DEFAULT_HEARTBEAT_TIMEOUT),
                    default_result: put
                        .default_result
                        .clone()
                        .unwrap_or_else(|| DEFAULT_LIFECYCLE_ACTION_RESULT.to_string()),
                },
            );
            Ok(())
        }
    }
}

/// The values a request carries for ONE hook, checked against the rules that are about
/// the REQUEST rather than about the hook the group holds: the transition and the
/// default result against their documented value sets, the notification target against
/// the vendors this service publishes to, and the heartbeat timeout against its
/// documented range.
///
/// Separate from [`put_lifecycle_hook`] because a request's own values are checked
/// BEFORE the resources it names are resolved, so an operation that resolves the group
/// first still refuses an unusable value with the value's own refusal; the upsert calls
/// it too, so a writer that has the group in hand (the group create's hook
/// specifications) cannot skip a rule.
pub fn validate_lifecycle_hook_members(
    put: &LifecycleHookPut,
) -> Result<(), LifecycleHookRefusal> {
    if let Some(transition) = put.lifecycle_transition.as_deref() {
        if !super::LIFECYCLE_HOOK_TYPES.contains(&transition) {
            return Err(LifecycleHookRefusal::UnknownTransition);
        }
    }
    if let Some(result) = put.default_result.as_deref() {
        if !LIFECYCLE_ACTION_RESULTS.contains(&result) {
            return Err(LifecycleHookRefusal::UnknownResult);
        }
    }
    if let Some(vendor) = unsupported_notification_target_vendor(
        put.notification_target_arn.as_deref().unwrap_or_default(),
    ) {
        return Err(LifecycleHookRefusal::UnsupportedNotificationTargetVendor(
            vendor.to_string(),
        ));
    }
    if let Some(timeout) = put.heartbeat_timeout {
        if !(MIN_HEARTBEAT_TIMEOUT..=MAX_HEARTBEAT_TIMEOUT).contains(&timeout) {
            return Err(LifecycleHookRefusal::HeartbeatOutOfRange);
        }
    }
    Ok(())
}

/// The vendor of `target` when it is a resource of a vendor this service cannot notify,
/// and `None` when the target is one it can.
///
/// The empty target names no resource at all — it is how a request CLEARS a stored one —
/// and a value this service cannot read an ARN's vendor field out of
/// ([`::service_common::arn::vendor`]) names no vendor to report, so neither is refused
/// here.
pub fn unsupported_notification_target_vendor(target: &str) -> Option<&str> {
    ::service_common::arn::vendor(target)
        .filter(|vendor| !NOTIFICATION_TARGET_VENDORS.contains(vendor))
}

/// The message a request naming a member value outside the set that member admits is
/// refused with: the member as the MODEL names it, the admissible values in the order
/// the service publishes them, and no part of the request — the sentence states what
/// the member admits, which is the same whatever the request carried.
///
/// THE ONE SHAPE for every lifecycle-hook member whose value set this service checks
/// itself, because the model types those members as plain strings
/// (`smithy_query(query="shape",shape="LifecycleTransition,LifecycleActionResult")`) and
/// so carries no enumeration for the boundary validator to refuse them against.
fn outside_value_set_message(member: &str, values: &[&str]) -> String {
    format!(
        "'{member}' must be one of: {}",
        super::joined_values(values.iter().map(::std::string::ToString::to_string))
    )
}

/// The message a request naming a transition no lifecycle hook can be created for is
/// refused with.
pub fn unknown_transition_message() -> String {
    outside_value_set_message("LifecycleTransition", super::LIFECYCLE_HOOK_TYPES)
}

/// The message a request naming a default result outside the documented pair is refused
/// with, in the same shape.
pub fn unknown_result_message() -> String {
    outside_value_set_message("DefaultResult", LIFECYCLE_ACTION_RESULTS)
}

/// The message a call that completes a held lifecycle action with a result outside that
/// same pair is refused with, in the same shape.
///
/// The member is the one `CompleteLifecycleAction` carries the result in — "the action for
/// the group to take. You can specify either CONTINUE or ABANDON"
/// (`smithy_query(query="doc",shape="CompleteLifecycleActionType",member="LifecycleActionResult")`)
/// — and the admissible values are the pair a hook may default to, since both state the
/// same outcome of the same wait.
pub fn unknown_lifecycle_action_result_message() -> String {
    outside_value_set_message("LifecycleActionResult", LIFECYCLE_ACTION_RESULTS)
}

/// The message a request whose notification target is a resource of a vendor this
/// service does not publish to is refused with.
///
/// Three sentences of fixed text around ONE variable part — the vendor field of the ARN
/// the request submitted: the member as the model names it, the vendor that cannot be
/// notified, and the vendors that can ([`NOTIFICATION_TARGET_VENDORS`]).
pub fn unsupported_notification_target_message(vendor: &str) -> String {
    format!(
        "The value in 'NotificationTargetARN' is invalid. The specified vendor '{vendor}' is not \
         valid. Supported vendors: {}",
        super::joined_values(
            NOTIFICATION_TARGET_VENDORS
                .iter()
                .map(::std::string::ToString::to_string)
        )
    )
}

/// The message a call addressed at a lifecycle hook this service holds none of is
/// refused with: the hook is a named child of a group, so the sentence names BOTH
/// identifiers the request addressed it by, and it is the same whether the group holds
/// no hook of that name or no group answers to the name at all — either way there is no
/// such hook.
///
/// Two variable parts, both quoted and both taken from the request: the hook's name and
/// the group's. The rest, punctuation included, is fixed text.
pub fn absent_lifecycle_hook_message(hook_name: &str, group_name: &str) -> String {
    format!("No Lifecycle Hook found with name '{hook_name}' for group '{group_name}'")
}

/// The message a request whose `HeartbeatTimeout` is outside the documented range is
/// refused with: the member as the model names it and the range it has to be inside,
/// both bounds from the model's own documentation ("The range is from 30 to 7200
/// seconds"). Fixed text — the sentence states what the member admits, so no part of the
/// request appears in it.
pub fn heartbeat_timeout_violation_message() -> String {
    format!(
        "HeartbeatTimeout must be between {MIN_HEARTBEAT_TIMEOUT} and {MAX_HEARTBEAT_TIMEOUT} \
         seconds"
    )
}

/// The message a request that would create a hook past
/// [`MAX_LIFECYCLE_HOOKS_PER_GROUP`] is refused with, which quotes the limit the caller
/// has to come back under — the shape this service reports every per-group ceiling in.
pub fn too_many_hooks_message(name: &str) -> String {
    format!(
        "The lifecycle hook {name} exceeds the limit of {MAX_LIFECYCLE_HOOKS_PER_GROUP} lifecycle \
         hooks per Auto Scaling group"
    )
}

/// The message a request that creates a hook without naming a transition is refused
/// with: the model documents `LifecycleTransition` as "Required for new lifecycle hooks,
/// but optional when updating existing hooks", so the refusal says which of the two the
/// request is.
pub fn transition_missing_message(name: &str) -> String {
    format!(
        "The LifecycleTransition is required to create the lifecycle hook {name}, and may be \
         omitted only when updating one that already exists"
    )
}
