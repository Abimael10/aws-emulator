// SPDX-License-Identifier: CC0-1.0

//! The lifecycle ACTIONS a group's lifecycle hooks hold its instances at, and the events
//! that announce them.
//!
//! # The rule this module implements
//!
//! A lifecycle hook is not a record a client stores and reads back: it is a HOLD. "Create a
//! launch template or launch configuration with a user data script that runs while an
//! instance is in a wait state due to a lifecycle hook. … Create a Lambda function and a
//! rule that allows Amazon EventBridge to invoke your Lambda function when an instance is
//! put into a wait state due to a lifecycle hook. … If you need more time, record the
//! lifecycle action heartbeat to keep the instance in a wait state. If you finish before
//! the timeout period ends, send a callback by using the CompleteLifecycleAction API call"
//! (`smithy_query(query="operation",operation="CompleteLifecycleAction")`).
//!
//! So when a group holds a hook for the transition one of its instances is taking, three
//! things follow, and each is implemented here:
//!
//! * the instance is put into the WAIT STATE of that transition
//!   ([`HeldTransition::wait_state`], one of the model's own `LifecycleState` values) and
//!   stays there: nothing the clock owes moves it on by itself;
//! * the held action is ANNOUNCED on the account's default event bus, under the `source`
//!   this service publishes everything under and the `detail-type` of that transition
//!   ([`HeldTransition::detail_type`]), carrying the group, the instance, the hook, the
//!   transition, the hook's own `NotificationMetadata` and a freshly minted
//!   `LifecycleActionToken` — the token being the only thing a client can complete the
//!   action with, which is why it has to be published;
//! * the hold ENDS when the client passes that token (or the instance id) back through
//!   `CompleteLifecycleAction`, or when the clock times the action out — the hook's
//!   `HeartbeatTimeout` since the hold started or since the last
//!   `RecordLifecycleActionHeartbeat`, and its `GlobalTimeout` since the hold started — at
//!   which point the hook's own `DefaultResult` is what the action resolves to.
//!
//! What the resolved result MEANS is read off the transition: a launch that carries on
//! proceeds into service, a launch that is abandoned gives the instance back to Amazon EC2,
//! and a termination proceeds either way — the hook could delay it, not cancel it.
//!
//! A group holding several hooks for one transition holds the instance at each of them: one
//! token and one event per hook, and the instance proceeds only once every one of them has
//! carried on. Any one of them abandoning the instance ends the wait at once, which is what
//! `ABANDON` means.
//!
//! # Where a hold comes from
//!
//! Nothing here decides WHICH instances move: the one launch path a group has
//! ([`AutoScalingState::launch_into_group`]), the one attach ([`super::attached_instances`])
//! and the one path that takes an instance out of service
//! ([`AutoScalingState::terminate_group_instances`]) each hand their instances to
//! [`AutoScalingState::hold_lifecycle_transition`] — so a hold is owed once per instance per
//! hook however the move was asked for, and a group with no hook for that transition is
//! untouched by any of it.
//!
//! The events are queued and published by the SAME drain and the SAME clock pump as the
//! group's instance lifecycle events ([`super::lifecycle_events`]), for the same reason: a
//! client that waits for a hook's event makes no further Auto Scaling call, so an event
//! published only by the next reader would never be published at all. The queue is bounded
//! ([`MAX_PENDING_LIFECYCLE_ACTION_EVENTS`], oldest dropped first) because every held
//! instance appends to it.

use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

use super::{
    AutoScalingState, InstanceLifecycle, InstanceLifecycleChange, StoredInstance,
    StoredLifecycleHook, LIFECYCLE_ACTION_CONTINUE, LIFECYCLE_PENDING, LIFECYCLE_TERMINATING,
    LIFECYCLE_TRANSITION,
};
use super::lifecycle_events::{instance_arn, publish_events, OutboundEvent};
use super::sns_notifications::insert_text;

/// The transition a hook holds a SCALE-OUT at, from the model's own documentation for
/// `PutLifecycleHookType$LifecycleTransition` ("To create a lifecycle hook for scale-out
/// events, specify autoscaling:EC2_INSTANCE_LAUNCHING").
pub const TRANSITION_LAUNCHING: &str = "autoscaling:EC2_INSTANCE_LAUNCHING";

/// The transition a hook holds a SCALE-IN at, from the same sentence ("To create a lifecycle
/// hook for scale-in events, specify autoscaling:EC2_INSTANCE_TERMINATING").
pub const TRANSITION_TERMINATING: &str = "autoscaling:EC2_INSTANCE_TERMINATING";

/// The `detail-type` the event announcing a held LAUNCH carries.
pub const LAUNCH_LIFECYCLE_ACTION: &str = "EC2 Instance-launch Lifecycle Action";

/// The `detail-type` the event announcing a held TERMINATION carries.
pub const TERMINATE_LIFECYCLE_ACTION: &str = "EC2 Instance-terminate Lifecycle Action";

// The `LifecycleState` values a held transition moves an instance through, spelled
// exactly as the model's `com.amazonaws.autoscaling#LifecycleState` enum does
// (smithy_query(query="shape",shape="LifecycleState")).
/// A launching instance a launch hook is holding.
pub const LIFECYCLE_PENDING_WAIT: &str = "Pending:Wait";
/// A launching instance whose launch hooks have all carried on.
pub const LIFECYCLE_PENDING_PROCEED: &str = "Pending:Proceed";
/// An instance on its way out of the group that a termination hook is holding.
pub const LIFECYCLE_TERMINATING_WAIT: &str = "Terminating:Wait";
/// One whose termination hooks have resolved and whose termination is carrying on.
pub const LIFECYCLE_TERMINATING_PROCEED: &str = "Terminating:Proceed";
/// One whose termination hook was ABANDONED under a group that retains such instances:
/// "set to `retain` to move instances to a retained state … Retained instances don't count
/// toward desired capacity and remain until you call `TerminateInstanceInAutoScalingGroup`"
/// (`RetentionTriggers$TerminateHookAbandon`).
pub const LIFECYCLE_TERMINATING_RETAINED: &str = "Terminating:Retained";

/// How many lifecycle-action events the queue holds at once.
///
/// This module's OWN ceiling, not an AWS quota, and for the same reason as the queue it is
/// drained beside ([`super::lifecycle_events::MAX_PENDING_EVENTS`]): the queue lives in
/// memory and every held instance appends one entry per hook holding it, so without a bound
/// a client scaling without end could grow it without end. The oldest event is dropped
/// first.
pub const MAX_PENDING_LIFECYCLE_ACTION_EVENTS: usize = 4_000;

/// How many RETAINED instances one group keeps at once.
///
/// This module's OWN ceiling, not an AWS quota, and for the same reason as the queue above:
/// an instance a termination hook abandoned under a group whose `TerminateHookAbandon`
/// trigger retains it is released by nothing the clock owes — it "remain[s] until you call
/// `TerminateInstanceInAutoScalingGroup`" — so without a bound a client that keeps abandoning
/// terminations would keep growing the group's own record of its members, and the running
/// Amazon EC2 instances behind them, without end. The OLDEST retained instance is released
/// first ([`AutoScalingState::release_retained_instances_past_the_bound`]), which is what the
/// group would have done with it had the trigger not retained it.
pub const MAX_RETAINED_INSTANCES_PER_GROUP: usize = 100;

/// Which lifecycle transition a hook is holding an instance at.
///
/// The two the service's own catalog admits ([`super::LIFECYCLE_HOOK_TYPES`]), as a type:
/// everything that differs between a held launch and a held termination — the state the
/// instance waits in, the `detail-type` the hold is announced under, and what resolving the
/// action does — is read off this one value rather than off a string comparison at each
/// site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeldTransition {
    /// The instance is being launched into the group.
    Launching,
    /// The instance is being taken out of it.
    Terminating,
}

impl HeldTransition {
    /// The transition `transition` names, or `None` for a value that names neither.
    #[must_use]
    pub fn of(transition: &str) -> Option<Self> {
        match transition {
            TRANSITION_LAUNCHING => Some(Self::Launching),
            TRANSITION_TERMINATING => Some(Self::Terminating),
            _ => None,
        }
    }

    /// The transition this is the wait of, for the step a group records when it moves an
    /// instance: a launch is held at the launching transition and a termination at the
    /// terminating one.
    #[must_use]
    pub fn of_step(kind: InstanceLifecycle) -> Self {
        match kind {
            InstanceLifecycle::Launched => Self::Launching,
            InstanceLifecycle::Terminated => Self::Terminating,
        }
    }

    /// The `LifecycleTransition` this is, as a hook names it and as the event reports it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Launching => TRANSITION_LAUNCHING,
            Self::Terminating => TRANSITION_TERMINATING,
        }
    }

    /// The `detail-type` the event announcing a hold at this transition carries.
    #[must_use]
    pub fn detail_type(self) -> &'static str {
        match self {
            Self::Launching => LAUNCH_LIFECYCLE_ACTION,
            Self::Terminating => TERMINATE_LIFECYCLE_ACTION,
        }
    }

    /// The `LifecycleState` an instance held at this transition is reported in.
    #[must_use]
    pub fn wait_state(self) -> &'static str {
        match self {
            Self::Launching => LIFECYCLE_PENDING_WAIT,
            Self::Terminating => LIFECYCLE_TERMINATING_WAIT,
        }
    }

    /// The `LifecycleState` it is reported in once every hook holding it has resolved and
    /// the transition is carrying on.
    #[must_use]
    pub fn proceed_state(self) -> &'static str {
        match self {
            Self::Launching => LIFECYCLE_PENDING_PROCEED,
            Self::Terminating => LIFECYCLE_TERMINATING_PROCEED,
        }
    }

    /// Whether `instance` is one this transition's hooks can still be put in front of.
    ///
    /// A launch is held while the instance is still coming into service, and a termination
    /// while the instance has not started leaving — an instance already held, or already on
    /// its way out, is not held a second time.
    fn holdable(self, instance: &StoredInstance) -> bool {
        if instance.is_held_by_lifecycle_hook() {
            return false;
        }
        match self {
            Self::Launching => instance.lifecycle_state == LIFECYCLE_PENDING,
            Self::Terminating => !instance.is_leaving_group(),
        }
    }
}

/// The step a group is MID-WAY through for one instance while a hook holds it: the scaling
/// activity it recorded and the event it queued, neither of which is a fact until the hold
/// resolves.
///
/// Kept on the instance so the one place a hold ENDS can settle them
/// ([`AutoScalingState::settle_held_step`]): the activity completes, and the event is
/// published, at the instant the wait ended — and an instance that never came into service
/// has no launch to announce at all.
#[derive(Debug, Clone)]
pub struct HeldStep {
    /// Which step it is, which is the queued event this settles.
    pub kind: InstanceLifecycle,
    /// The scaling activity that step recorded.
    pub activity_id: String,
}

/// One lifecycle action of one instance, held by one hook of its group.
///
/// The instance it belongs to is the record that carries it
/// ([`StoredInstance::pending_lifecycle_actions`]), so an action cannot be stored against an
/// instance that disagrees with it. The hook's own values are copied in when the hold
/// starts: the hold is what that hook said at the instant it took the instance, so a hook a
/// client edits or deletes while an instance waits does not retune or orphan the wait it had
/// already started.
#[derive(Debug, Clone)]
pub struct StoredLifecycleAction {
    /// `LifecycleActionToken`: "a universally unique identifier (UUID) that identifies a
    /// specific lifecycle action associated with an instance"
    /// (`CompleteLifecycleActionType$LifecycleActionToken`), minted once per hold.
    pub token: String,
    /// The hook holding the instance, which is the `LifecycleHookName` a callback names.
    pub hook_name: String,
    /// Which transition it is holding.
    pub transition: HeldTransition,
    /// The hook's `DefaultResult`, which is what the action resolves to if the clock times
    /// it out.
    pub default_result: String,
    /// The hook's `HeartbeatTimeout`, which is how much longer each heartbeat buys.
    pub heartbeat_timeout: Duration,
    /// When the current heartbeat runs out.
    pub heartbeat_due: SystemTime,
    /// When the hook's `GlobalTimeout` runs out — "the maximum time, in seconds, that an
    /// instance can remain in a wait state" — which no heartbeat extends.
    pub global_due: SystemTime,
}

impl StoredLifecycleAction {
    /// The instant this action is timed out at: whichever of its two deadlines comes first.
    #[must_use]
    pub fn timeout_at(&self) -> SystemTime {
        self.heartbeat_due.min(self.global_due)
    }
}

/// One lifecycle-action event waiting to be published.
#[derive(Debug, Clone)]
pub struct PendingLifecycleActionEvent {
    /// Which transition the hold is at, which is what the `detail-type` and the
    /// `LifecycleTransition` in the detail are both read from.
    pub transition: HeldTransition,
    /// The group whose hook is holding the instance.
    pub group_name: String,
    /// That group's ARN, carried in the envelope's `resources`.
    pub group_arn: String,
    /// The Region the group lives in, which is the Region the event is published in.
    pub region: String,
    /// The instance being held.
    pub instance_id: String,
    /// The hook holding it.
    pub hook_name: String,
    /// The token the client has to pass back.
    pub token: String,
    /// The hook's `NotificationMetadata`, when it carries one: "additional information that
    /// you want to include any time Amazon EC2 Auto Scaling sends a message to the
    /// notification target" (`PutLifecycleHookType$NotificationMetadata`).
    pub notification_metadata: Option<String>,
    /// The instant from which the event may be published, which is when the hold started.
    pub due: SystemTime,
}

impl StoredInstance {
    /// Whether a lifecycle hook is holding this instance.
    #[must_use]
    pub fn is_held_by_lifecycle_hook(&self) -> bool {
        !self.pending_lifecycle_actions.is_empty()
    }

    /// The holds on this instance that are NOT at `transition`.
    ///
    /// What a move in another direction has to discard: an instance leaving the group was
    /// waiting at a launch that is now over, so that hold — and the launch it was waiting
    /// for — cannot stand.
    fn holds_other_than(&self, transition: HeldTransition) -> usize {
        self.pending_lifecycle_actions
            .iter()
            .filter(|action| action.transition != transition)
            .count()
    }

    /// Whether this instance is RETAINED: its termination lifecycle hook was abandoned under
    /// a group whose `TerminateHookAbandon` retention trigger keeps such instances, so it is
    /// still a member the group reports and Amazon EC2 still runs it, while it counts towards
    /// no capacity and nothing but `TerminateInstanceInAutoScalingGroup` takes it out.
    #[must_use]
    pub fn is_retained(&self) -> bool {
        self.lifecycle_state == LIFECYCLE_TERMINATING_RETAINED
    }

    /// Whether this instance is on its way OUT of the group — terminating, waiting at a
    /// termination hook, carrying on past one, or being detached.
    ///
    /// Every `LifecycleState` of a terminating instance is the terminating state or a
    /// colon-qualified form of it, so that half is a prefix; the two detached states have no
    /// qualified forms, so they are named exactly. A DETACHED instance leaves the group just
    /// as finally without being terminated (`DetachInstances`: "removes one or more instances
    /// from the specified Auto Scaling group. After the instances are detached, you can
    /// manage them independent of the Auto Scaling group"), so the one reading of "this
    /// instance no longer counts towards the group" covers both: a scale-in that chose it
    /// must not choose it again, and the group's capacity is not met by it.
    #[must_use]
    pub fn is_leaving_group(&self) -> bool {
        self.lifecycle_state.starts_with(LIFECYCLE_TERMINATING)
            || self.lifecycle_state == super::LIFECYCLE_DETACHING
            || self.lifecycle_state == super::LIFECYCLE_DETACHED
    }

    /// The held action of this instance that `token` names, if it holds one.
    fn action_by_token(&self, token: &str) -> Option<&StoredLifecycleAction> {
        self.pending_lifecycle_actions
            .iter()
            .find(|action| action.token == token)
    }
}

/// How one request addresses ONE held lifecycle action.
///
/// `CompleteLifecycleAction` and `RecordLifecycleActionHeartbeat` take the same four
/// members, so the lookup is written once and both doors use it: the hook and the group are
/// required, and the action within them is named by its token or by the instance holding it.
#[derive(Debug, Clone, Copy)]
pub struct LifecycleActionKey<'a> {
    /// `AutoScalingGroupName`.
    pub group_name: &'a str,
    /// `LifecycleHookName`.
    pub hook_name: &'a str,
    /// `LifecycleActionToken`, when the request carries one.
    pub token: Option<&'a str>,
    /// `InstanceId`, when it carries that instead.
    pub instance_id: Option<&'a str>,
}

/// Why a call addressed at a held lifecycle action found none.
#[derive(Debug, PartialEq, Eq)]
pub enum LifecycleActionRefusal {
    /// The request named neither the token nor the instance, so it addresses no action.
    NotAddressed,
    /// No action of this group is held under the identifier the request named, and the
    /// instance it named is waiting at NO lifecycle hook at all — so there is no hook the
    /// callback could have meant.
    NoActiveAction,
    /// The instance the request named IS waiting at a lifecycle hook of this group, but not
    /// at the hook the request named: the hook is the part of the address that matched
    /// nothing, so the refusal names it beside the instance.
    NoActionUnderHook,
}

impl LifecycleActionRefusal {
    /// What the caller is told, for the action the request addressed.
    ///
    /// ONE spelling per situation for both doors, so completing an action and heartbeating
    /// one cannot refuse the same request differently.
    #[must_use]
    pub fn message(&self, key: &LifecycleActionKey<'_>) -> String {
        match self {
            Self::NotAddressed => not_addressed_message(),
            Self::NoActiveAction | Self::NoActionUnderHook => match (key.token, key.instance_id) {
                (Some(token), _) => no_active_action_by_token_message(token),
                (None, Some(instance_id)) => {
                    if *self == Self::NoActionUnderHook {
                        no_active_action_under_hook_message(instance_id, key.hook_name)
                    } else {
                        no_active_action_by_instance_message(instance_id)
                    }
                }
                (None, None) => not_addressed_message(),
            },
        }
    }
}

/// The message a call that names neither identifier is refused with.
///
/// The model makes both `LifecycleActionToken` and `InstanceId` optional while the action a
/// callback is about is one or the other, so the sentence states what the request has to
/// carry and quotes nothing of what it did: it is the same sentence whatever was sent.
#[must_use]
pub fn not_addressed_message() -> String {
    "Either the lifecycle action token or the instance ID must be specified.".to_string()
}

/// The message a call naming a token no action of the group is held under is refused with:
/// fixed text and the one variable part the caller has to correct, the token it sent.
#[must_use]
pub fn no_active_action_by_token_message(token: &str) -> String {
    format!("No active Lifecycle Action found with token {token}")
}

/// The same, for a call that named the instance instead: the one identifier the caller has
/// to correct, the instance it sent. The instance is waiting at no lifecycle hook of the
/// group, so there is no hook to name.
#[must_use]
pub fn no_active_action_by_instance_message(instance_id: &str) -> String {
    format!("No active Lifecycle Action found with instance ID {instance_id}")
}

/// The same, for a call whose instance IS waiting — just not at the hook it named: both
/// identifiers it addressed the action by, because the hook is the half that matched nothing
/// and is what tells a consumed action apart from a hook that holds this instance.
#[must_use]
pub fn no_active_action_under_hook_message(instance_id: &str, hook_name: &str) -> String {
    format!(
        "No active Lifecycle Action found with instance ID {instance_id} and HookName \
         {hook_name}"
    )
}

/// What the activity of an instance a LAUNCH hook abandoned reports as its cause.
///
/// `Activity$Cause` is free text that says what asked for the change, and what asked for
/// this one is the hook's own result: the instance never came into service, so the group
/// gives it back to Amazon EC2. One variable part, the hook that abandoned it.
#[must_use]
pub fn abandoned_launch_cause(at: SystemTime, hook_name: &str) -> String {
    AutoScalingState::reason_cause(
        at,
        &format!(
            "the lifecycle hook {hook_name} abandoned the instance while it was in a wait state, \
             so it was not brought into service"
        ),
    )
}

impl AutoScalingState {
    /// Mint the `LifecycleActionToken` of one hold.
    ///
    /// From this store's own source of opaque resource ids, so no two actions — and no
    /// action and no other resource of this service — are ever minted the same identifier.
    /// The form it mints is the 36-character UUID shape the member is modelled as
    /// (`LifecycleActionToken` is `@length(min: 36, max: 36)`).
    fn mint_lifecycle_action_token(&mut self) -> String {
        self.mint_resource_id()
    }

    /// Hold the instances `instance_ids` names at `transition`, behind every hook of their
    /// group configured for it, and answer the ids that are now held.
    ///
    /// THE ONE place a hold starts, called by each path that moves an instance into or out of
    /// a group. A group with no hook for the transition holds nothing and the call changes
    /// nothing, which is what keeps every group that configures no hook exactly as it was.
    ///
    /// A LAUNCH held here has finished starting: the work a wait state exists for runs on a
    /// started instance, so Amazon EC2 reports the held instance `running` while the hold
    /// lasts ([`super::ec2_instances::finish_starting`]).
    pub(crate) fn hold_lifecycle_transition<Eng: Send + Sync + 'static>(
        &mut self,
        engine: &Eng,
        group_name: &str,
        transition: HeldTransition,
        instance_ids: &[String],
        now: SystemTime,
    ) -> Vec<String> {
        // A move in one direction ends the wait of the other: an instance leaving the group
        // was held at a launch that is over, so that hold goes before this one is taken — or
        // before the move proceeds unheld.
        self.discard_holds_other_than(group_name, transition, instance_ids, now);
        let Some(group) = self.groups.get(group_name) else {
            return Vec::new();
        };
        // The hooks as the group holds them NOW, copied out: the hold is what each said at
        // this instant.
        let hooks: Vec<(String, StoredLifecycleHook)> = group
            .lifecycle_hooks
            .iter()
            .filter(|(_, hook)| hook.lifecycle_transition == transition.name())
            .map(|(name, hook)| (name.clone(), hook.clone()))
            .collect();
        if hooks.is_empty() {
            return Vec::new();
        }
        let group_arn = group.arn.clone();
        let region = group.region.clone();
        let holding: Vec<String> = group
            .instances
            .iter()
            .filter(|instance| {
                instance_ids.contains(&instance.instance_id) && transition.holdable(instance)
            })
            .map(|instance| instance.instance_id.clone())
            .collect();
        if holding.is_empty() {
            return Vec::new();
        }

        let mut held: Vec<(String, Vec<StoredLifecycleAction>)> = Vec::with_capacity(holding.len());
        let mut events: Vec<PendingLifecycleActionEvent> = Vec::new();
        for instance_id in &holding {
            let mut actions = Vec::with_capacity(hooks.len());
            for (hook_name, hook) in &hooks {
                let token = self.mint_lifecycle_action_token();
                let heartbeat = seconds(hook.heartbeat_timeout);
                actions.push(StoredLifecycleAction {
                    token: token.clone(),
                    hook_name: hook_name.clone(),
                    transition,
                    default_result: hook.default_result.clone(),
                    heartbeat_timeout: heartbeat,
                    heartbeat_due: now + heartbeat,
                    global_due: now + seconds(hook.global_timeout()),
                });
                events.push(PendingLifecycleActionEvent {
                    transition,
                    group_name: group_name.to_string(),
                    group_arn: group_arn.clone(),
                    region: region.clone(),
                    instance_id: instance_id.clone(),
                    hook_name: hook_name.clone(),
                    token,
                    notification_metadata: hook.notification_metadata.clone(),
                    due: now,
                });
            }
            held.push((instance_id.clone(), actions));
        }

        if let Some(group) = self.groups.get_mut(group_name) {
            for (instance_id, actions) in held {
                let Some(instance) = group
                    .instances
                    .iter_mut()
                    .find(|instance| instance.instance_id == instance_id)
                else {
                    continue;
                };
                instance.lifecycle_state = transition.wait_state().to_string();
                // Nothing the clock owes moves a held instance on: only the action's own
                // resolution does.
                instance.transition_due = None;
                instance.pending_lifecycle_actions = actions;
            }
        }
        for event in events {
            self.queue_lifecycle_action_event(event);
        }
        if transition == HeldTransition::Launching {
            // The instance a launch hook holds is up: Amazon EC2 has finished starting it,
            // which is what the wait state's own work runs on.
            super::ec2_instances::finish_starting(engine, &holding, now);
        }
        holding
    }

    /// Hold the launches of `group_name` that are STILL IN FLIGHT behind the hooks the group
    /// holds now, and answer whether any instance was taken.
    ///
    /// What a hook stored while a launch is in flight has to act on: an instance the group
    /// still reports [`LIFECYCLE_PENDING`] has not passed the launching transition, so a hook
    /// for that transition takes it exactly as one stored before the launch does.
    ///
    /// Only the LAUNCHING transition has such a point to catch. A launch is complete when the
    /// group admits the instance to service, at the END of the span it reports it `Pending`
    /// for, so a hook arriving inside that span is still in front of it. A termination is
    /// carried out when the instance is shut down, which this service does at the START of
    /// the span it reports `Terminating` for ([`AutoScalingState::terminate_group_instances`]
    /// hands the instance back to Amazon EC2 there) — so by the time an instance is reported
    /// `Terminating` there is no termination left to hold, and a termination hook stored then
    /// acts on the group's next scale-in instead.
    pub(crate) fn hold_launches_in_flight<Eng: Send + Sync + 'static>(
        &mut self,
        engine: &Eng,
        group_name: &str,
        now: SystemTime,
    ) -> bool {
        let Some(group) = self.groups.get(group_name) else {
            return false;
        };
        let in_flight: Vec<String> = group
            .instances
            .iter()
            .filter(|instance| instance.lifecycle_state == LIFECYCLE_PENDING)
            .map(|instance| instance.instance_id.clone())
            .collect();
        if in_flight.is_empty() {
            return false;
        }
        !self
            .hold_lifecycle_transition(engine, group_name, HeldTransition::Launching, &in_flight, now)
            .is_empty()
    }

    /// Queue one lifecycle-action event for publication, keeping the queue bounded.
    fn queue_lifecycle_action_event(&mut self, event: PendingLifecycleActionEvent) {
        while self.pending_lifecycle_action_events.len() >= MAX_PENDING_LIFECYCLE_ACTION_EVENTS {
            self.pending_lifecycle_action_events.pop_front();
            ::tracing::warn!(
                "the queue of Auto Scaling lifecycle-action events waiting to be published to \
                 EventBridge is full at {MAX_PENDING_LIFECYCLE_ACTION_EVENTS}; the oldest event \
                 was dropped"
            );
        }
        self.pending_lifecycle_action_events.push_back(event);
    }

    /// Take at most `limit` of the queued lifecycle-action events the clock has made due at
    /// `now`, oldest first.
    ///
    /// Taking them out under the lock is what stops two concurrent drains from publishing one
    /// event twice, exactly as the instance lifecycle events beside them are taken.
    pub(crate) fn take_due_lifecycle_action_events(
        &mut self,
        now: SystemTime,
        limit: usize,
    ) -> Vec<PendingLifecycleActionEvent> {
        let mut taken = Vec::new();
        let mut kept = VecDeque::with_capacity(self.pending_lifecycle_action_events.len());
        while let Some(event) = self.pending_lifecycle_action_events.pop_front() {
            if taken.len() < limit && event.due <= now {
                taken.push(event);
            } else {
                kept.push_back(event);
            }
        }
        self.pending_lifecycle_action_events = kept;
        taken
    }

    /// Whether any queued lifecycle-action event is already due at `now`.
    pub(crate) fn owes_lifecycle_action_event(&self, now: SystemTime) -> bool {
        self.pending_lifecycle_action_events
            .iter()
            .any(|event| event.due <= now)
    }

    /// Every instant a held lifecycle action of this store is timed out at.
    ///
    /// What the clock owes about the holds, for the pump that waits on the earliest instant
    /// this store owes anything at: a hold nobody completes is resolved by its own deadline,
    /// with no further call from anybody.
    pub(crate) fn lifecycle_action_timeouts(&self) -> Vec<SystemTime> {
        self.groups
            .values()
            .flat_map(|group| group.instances.iter())
            .flat_map(|instance| instance.pending_lifecycle_actions.iter())
            .map(StoredLifecycleAction::timeout_at)
            .chain(
                self.pending_lifecycle_action_events
                    .iter()
                    .map(|event| event.due),
            )
            .collect()
    }

    /// The instance of `group_name` holding the action `key` addresses, and that action's
    /// token.
    ///
    /// The token is answered rather than an index, because resolving one action of an
    /// instance can clear the others: a token identifies the same action however the vector
    /// moved.
    pub(crate) fn locate_lifecycle_action(
        &self,
        key: &LifecycleActionKey<'_>,
    ) -> Result<(String, String), LifecycleActionRefusal> {
        let group = self
            .groups
            .get(key.group_name)
            .ok_or(LifecycleActionRefusal::NoActiveAction)?;
        // The token addresses the action directly; the instance id addresses whichever action
        // of that instance the named hook is holding. A request carrying both is answered on
        // the token, which names exactly one action.
        let found = match (key.token, key.instance_id) {
            (Some(token), _) => group
                .instances
                .iter()
                .filter(|instance| {
                    key.instance_id
                        .is_none_or(|named| named == instance.instance_id)
                })
                .find_map(|instance| {
                    instance
                        .action_by_token(token)
                        .filter(|action| action.hook_name == key.hook_name)
                        .map(|action| (instance.instance_id.clone(), action.token.clone()))
                }),
            (None, Some(instance_id)) => group
                .instances
                .iter()
                .find(|instance| instance.instance_id == instance_id)
                .and_then(|instance| {
                    instance
                        .pending_lifecycle_actions
                        .iter()
                        .find(|action| action.hook_name == key.hook_name)
                        .map(|action| (instance.instance_id.clone(), action.token.clone()))
                }),
            (None, None) => return Err(LifecycleActionRefusal::NotAddressed),
        };
        if let Some(found) = found {
            return Ok(found);
        }
        // Which HALF of the address matched nothing, which is what the refusal says: an
        // instance that is itself waiting at a hook of this group was addressed at the wrong
        // hook, while one waiting at none has no lifecycle action for any hook to hold.
        let waiting = key.instance_id.is_some_and(|instance_id| {
            group
                .instances
                .iter()
                .any(|instance| {
                    instance.instance_id == instance_id && instance.is_held_by_lifecycle_hook()
                })
        });
        Err(if waiting {
            LifecycleActionRefusal::NoActionUnderHook
        } else {
            LifecycleActionRefusal::NoActiveAction
        })
    }

    /// Record a heartbeat against the action `token` of `instance_id`: the hold keeps going
    /// for another `HeartbeatTimeout` from `now`, bounded by the hook's `GlobalTimeout`.
    ///
    /// "Records a heartbeat for the lifecycle action associated with the specified token or
    /// instance. This extends the timeout by the length of time defined using the
    /// PutLifecycleHook API call"
    /// (`smithy_query(query="operation",operation="RecordLifecycleActionHeartbeat")`) — and
    /// the global timeout is "the maximum time, in seconds, that an instance can remain in a
    /// wait state" (`LifecycleHook$GlobalTimeout`), so no run of heartbeats carries the hold
    /// past it.
    pub(crate) fn heartbeat_lifecycle_action(
        &mut self,
        group_name: &str,
        instance_id: &str,
        token: &str,
        now: SystemTime,
    ) {
        let Some(action) = self
            .groups
            .get_mut(group_name)
            .and_then(|group| {
                group
                    .instances
                    .iter_mut()
                    .find(|instance| instance.instance_id == instance_id)
            })
            .and_then(|instance| {
                instance
                    .pending_lifecycle_actions
                    .iter_mut()
                    .find(|action| action.token == token)
            })
        else {
            return;
        };
        action.heartbeat_due = (now + action.heartbeat_timeout).min(action.global_due);
    }

    /// Resolve the action `token` of `instance_id` with `result`, and end the wait when that
    /// result ends it.
    ///
    /// `CONTINUE` retires this one hook's hold; the instance proceeds only once no hook is
    /// still holding it. `ABANDON` ends the wait at once, whatever else is holding it — the
    /// group is not to bring the instance into service, so the remaining holds have nothing
    /// left to wait for. A token this store no longer holds resolves nothing, which is what
    /// makes a second resolution of one action harmless.
    pub(crate) fn resolve_lifecycle_action<Eng: Send + Sync + 'static>(
        &mut self,
        engine: &Eng,
        group_name: &str,
        instance_id: &str,
        token: &str,
        result: &str,
        now: SystemTime,
    ) {
        let Some(instance) = self
            .groups
            .get_mut(group_name)
            .and_then(|group| {
                group
                    .instances
                    .iter_mut()
                    .find(|instance| instance.instance_id == instance_id)
            })
            .filter(|instance| instance.action_by_token(token).is_some())
        else {
            return;
        };
        let Some(position) = instance
            .pending_lifecycle_actions
            .iter()
            .position(|action| action.token == token)
        else {
            return;
        };
        let action = instance.pending_lifecycle_actions.remove(position);
        let abandoned = result != LIFECYCLE_ACTION_CONTINUE;
        if abandoned {
            instance.pending_lifecycle_actions.clear();
        } else if instance.is_held_by_lifecycle_hook() {
            // Another hook of the group is still holding this instance, so the wait goes on.
            return;
        }
        self.end_lifecycle_wait(engine, group_name, instance_id, &action, abandoned, now);
    }

    /// Carry the instance past the wait `action` held it in.
    ///
    /// What that means is read off the transition. A LAUNCH that carried on is ADMITTED: the
    /// instance proceeds and the group reports it in service, with nothing further owed —
    /// the transient span this service owes an instance coming into service
    /// ([`LIFECYCLE_TRANSITION`]) was spent in the wait, so the release is what completes the
    /// launch rather than starting a second span after it. One that was abandoned never comes
    /// into service at all, so the group hands the instance back to Amazon EC2 and records the
    /// step it took with it, through the one path every lifecycle step of a group's instance
    /// is recorded by. A TERMINATION carries on either way — a hook can delay a termination,
    /// not cancel it — and the Amazon EC2 instance is shut down where it lives as the
    /// termination resumes. Both of those START a step of their own, so each takes the span
    /// this service owes that step.
    ///
    /// One exception, and it is the group's own standing instruction: a TERMINATION that was
    /// ABANDONED under a group whose `TerminateHookAbandon` retention trigger is set to
    /// retain does not carry on at all — "set to `retain` to move instances to a retained
    /// state … Retained instances don't count toward desired capacity and remain until you
    /// call `TerminateInstanceInAutoScalingGroup`"
    /// (`RetentionTriggers$TerminateHookAbandon`) — so the instance is RETAINED: the group
    /// keeps reporting it, Amazon EC2 keeps running it, and the termination the hook
    /// abandoned did not happen.
    fn end_lifecycle_wait<Eng: Send + Sync + 'static>(
        &mut self,
        engine: &Eng,
        group_name: &str,
        instance_id: &str,
        action: &StoredLifecycleAction,
        abandoned: bool,
        now: SystemTime,
    ) {
        let admitted = action.transition == HeldTransition::Launching && !abandoned;
        // Whether this group's own retention trigger keeps the instance a termination hook
        // abandoned, which is read off the group as it stands at this instant.
        let retained = abandoned
            && action.transition == HeldTransition::Terminating
            && self.groups.get(group_name).is_some_and(|group| {
                super::group_configuration::retains_abandoned_terminations(
                    group.instance_lifecycle_policy.as_ref(),
                )
            });
        let due = if admitted {
            now
        } else {
            now + LIFECYCLE_TRANSITION
        };
        let group_arn = self
            .groups
            .get(group_name)
            .map(|group| group.arn.clone())
            .unwrap_or_default();
        // The step the group was mid-way through for this instance is a fact now: its
        // activity completes at this instant and the event that reports it is owed here
        // rather than at the deadline the hold stood in for. A launch the hook ABANDONED
        // never came into service, so there is no successful launch to announce at all —
        // and neither did the termination of an instance the group retained instead.
        let happened = !(action.transition == HeldTransition::Launching && abandoned) && !retained;
        self.settle_held_step(group_name, instance_id, happened, due);
        if retained {
            // Nothing the clock owes moves a retained instance: it waits for a caller.
            self.retain_held_instance(engine, group_name, instance_id, now);
            return;
        }
        let retired = match (action.transition, abandoned) {
            (HeldTransition::Launching, false) => {
                self.move_held_instance(group_name, instance_id, LIFECYCLE_PENDING_PROCEED, due);
                false
            }
            (HeldTransition::Launching, true) => {
                self.move_held_instance(group_name, instance_id, LIFECYCLE_TERMINATING, due);
                true
            }
            (HeldTransition::Terminating, _) => {
                self.move_held_instance(
                    group_name,
                    instance_id,
                    LIFECYCLE_TERMINATING_PROCEED,
                    due,
                );
                true
            }
        };
        if !retired {
            return;
        }
        super::ec2_instances::terminate(engine, &[instance_id.to_string()], now);
        if action.transition == HeldTransition::Launching {
            // The group reports the step it took: an instance it launched and then gave back
            // is one that left the group, recorded and announced like every other
            // termination it makes.
            self.record_instance_lifecycle_change(
                engine,
                &InstanceLifecycleChange {
                    kind: InstanceLifecycle::Terminated,
                    group_name,
                    group_arn: &group_arn,
                    instance_id,
                    cause: &abandoned_launch_cause(now, &action.hook_name),
                    start_time: now,
                    due,
                },
            );
        }
    }

    /// Put the instance that was held into `lifecycle_state`, with its transient span owed at
    /// `due` and no hold left on it.
    fn move_held_instance(
        &mut self,
        group_name: &str,
        instance_id: &str,
        lifecycle_state: &str,
        due: SystemTime,
    ) {
        let Some(instance) = self
            .groups
            .get_mut(group_name)
            .and_then(|group| {
                group
                    .instances
                    .iter_mut()
                    .find(|instance| instance.instance_id == instance_id)
            })
        else {
            return;
        };
        instance.pending_lifecycle_actions.clear();
        instance.lifecycle_state = lifecycle_state.to_string();
        instance.transition_due = Some(due);
    }

    /// RETAIN the instance that was held: it is reported
    /// [`LIFECYCLE_TERMINATING_RETAINED`] and NOTHING is owed on it.
    ///
    /// The group's retention trigger is what asked for this, so the termination the hook
    /// abandoned does not resume on the clock: "retained instances … remain until you call
    /// `TerminateInstanceInAutoScalingGroup`" (`RetentionTriggers$TerminateHookAbandon`),
    /// which is why no due instant is left on the instance and the Amazon EC2 instance is
    /// left running where it lives.
    ///
    /// Because nothing the clock owes releases such an instance, the group keeps at most
    /// [`MAX_RETAINED_INSTANCES_PER_GROUP`] of them: past that the OLDEST retained instance
    /// is released here — terminated where it lives and dropped from the group — so a client
    /// that keeps abandoning terminations cannot fill the group's record with instances
    /// nothing will ever come back for.
    fn retain_held_instance<Eng: Send + Sync + 'static>(
        &mut self,
        engine: &Eng,
        group_name: &str,
        instance_id: &str,
        now: SystemTime,
    ) {
        let Some(instance) = self
            .groups
            .get_mut(group_name)
            .and_then(|group| {
                group
                    .instances
                    .iter_mut()
                    .find(|instance| instance.instance_id == instance_id)
            })
        else {
            return;
        };
        instance.pending_lifecycle_actions.clear();
        instance.lifecycle_state = LIFECYCLE_TERMINATING_RETAINED.to_string();
        instance.transition_due = None;
        self.release_retained_instances_past_the_bound(engine, group_name, now);
    }

    /// Keep the retained instances of `group_name` inside
    /// [`MAX_RETAINED_INSTANCES_PER_GROUP`], releasing the oldest ones past it.
    ///
    /// A released instance is terminated where it lives and leaves the group, which is what
    /// the group would have done had the trigger not retained it. Oldest first, by the launch
    /// order the store minted, so the instance a caller retained most recently is the last to
    /// go.
    fn release_retained_instances_past_the_bound<Eng: Send + Sync + 'static>(
        &mut self,
        engine: &Eng,
        group_name: &str,
        now: SystemTime,
    ) {
        let Some(group) = self.groups.get(group_name) else {
            return;
        };
        let mut retained: Vec<(u64, String)> = group
            .instances
            .iter()
            .filter(|instance| instance.is_retained())
            .map(|instance| (instance.launch_sequence, instance.instance_id.clone()))
            .collect();
        let Some(excess) = retained.len().checked_sub(MAX_RETAINED_INSTANCES_PER_GROUP) else {
            return;
        };
        if excess == 0 {
            return;
        }
        retained.sort_unstable();
        let released: Vec<String> = retained
            .into_iter()
            .take(excess)
            .map(|(_, instance_id)| instance_id)
            .collect();
        ::tracing::warn!(
            group = group_name,
            instances = ?released,
            "an Auto Scaling group is holding more instances retained by its TerminateHookAbandon \
             trigger than this emulator keeps at once \
             ({MAX_RETAINED_INSTANCES_PER_GROUP}); the oldest were terminated"
        );
        if let Some(group) = self.groups.get_mut(group_name) {
            group
                .instances
                .retain(|instance| !released.contains(&instance.instance_id));
        }
        super::ec2_instances::terminate(engine, &released, now);
    }

    /// Discard every hold on `instance_ids` that is NOT at `transition`, and settle the step
    /// each of them was waiting at as one that did not happen.
    ///
    /// A move in one direction ends the wait of the other: an instance a launch hook is
    /// still holding when the group takes it out of service never came into service, so its
    /// launch hold goes, its tokens stop resolving and the launch it was waiting for is not
    /// announced. Without this, a hold left behind on an instance another door moved would
    /// keep that instance in the group and, at its own deadline, report it in service again.
    fn discard_holds_other_than(
        &mut self,
        group_name: &str,
        transition: HeldTransition,
        instance_ids: &[String],
        at: SystemTime,
    ) {
        let Some(group) = self.groups.get_mut(group_name) else {
            return;
        };
        let mut discarded: Vec<String> = Vec::new();
        for instance in &mut group.instances {
            if !instance_ids.contains(&instance.instance_id)
                || instance.holds_other_than(transition) == 0
            {
                continue;
            }
            instance
                .pending_lifecycle_actions
                .retain(|action| action.transition == transition);
            discarded.push(instance.instance_id.clone());
        }
        for instance_id in discarded {
            self.settle_held_step(group_name, &instance_id, false, at);
        }
    }

    /// The instant the step `change` reports is owed at while a hook holds the instance at
    /// that step's own transition, and `None` for a step no hook is holding.
    ///
    /// The hold's LATEST deadline: the step is not a fact until every hook holding it has
    /// resolved, and that is the last instant a hold of it can still be running at
    /// ([`Self::settle_held_step`] brings it forward to the instant the wait actually ended).
    pub(crate) fn held_step_deadline(
        &self,
        change: &InstanceLifecycleChange<'_>,
    ) -> Option<SystemTime> {
        let transition = HeldTransition::of_step(change.kind);
        self.groups
            .get(change.group_name)?
            .instances
            .iter()
            .find(|instance| instance.instance_id == change.instance_id)?
            .pending_lifecycle_actions
            .iter()
            .filter(|action| action.transition == transition)
            .map(StoredLifecycleAction::timeout_at)
            .max()
    }

    /// Record, on the instance, the step a hook is holding it at, so the one place the wait
    /// ends can settle the activity and the event that step produced.
    pub(crate) fn remember_held_step(
        &mut self,
        change: &InstanceLifecycleChange<'_>,
        activity_id: &str,
    ) {
        let Some(instance) = self
            .groups
            .get_mut(change.group_name)
            .and_then(|group| {
                group
                    .instances
                    .iter_mut()
                    .find(|instance| instance.instance_id == change.instance_id)
            })
        else {
            return;
        };
        instance.held_step = Some(HeldStep {
            kind: change.kind,
            activity_id: activity_id.to_string(),
        });
    }

    /// Settle the step the instance was held at: its activity completes at `at`, and the
    /// event that reports it is owed at `at` when the step `happened` and dropped when it
    /// did not.
    ///
    /// THE ONE place a deferred step is settled, for every way a wait can end — the callback,
    /// the hook's own deadline, and a move in the other direction — so a step can neither be
    /// announced twice nor stay owed at a deadline that has been overtaken.
    fn settle_held_step(
        &mut self,
        group_name: &str,
        instance_id: &str,
        happened: bool,
        at: SystemTime,
    ) {
        let Some(step) = self
            .groups
            .get_mut(group_name)
            .and_then(|group| {
                group
                    .instances
                    .iter_mut()
                    .find(|instance| instance.instance_id == instance_id)
            })
            .and_then(|instance| instance.held_step.take())
        else {
            return;
        };
        if let Some(activity) = self
            .activities
            .iter_mut()
            .find(|activity| activity.activity_id == step.activity_id)
        {
            activity.completes_at = Some(at);
            // Nothing is holding the step any more, so the activity is no longer paused at
            // a hold: it is under way again until it completes at `at`.
            let milestone = super::ActivityMilestone::InProgress;
            activity.status_code = milestone.status_code().to_string();
            activity.progress = milestone.progress();
        }
        if happened {
            for event in &mut self.pending_lifecycle_events {
                if event.activity_id == step.activity_id {
                    event.due = at;
                }
            }
        } else {
            self.pending_lifecycle_events
                .retain(|event| event.activity_id != step.activity_id);
        }
    }

    /// Resolve every held lifecycle action the clock has timed out at `now`, each with the
    /// hook's own `DefaultResult`.
    ///
    /// Materialised by [`AutoScalingState::materialize_due`] like every other instant this
    /// store owes: a hold that ran out of heartbeats was over at its own deadline whether or
    /// not anybody called, and resolving it here is what keeps an instance from waiting for a
    /// client that never came back.
    pub(crate) fn expire_lifecycle_actions<Eng: Send + Sync + 'static>(
        &mut self,
        engine: &Eng,
        now: SystemTime,
    ) {
        // Collected first, then resolved: resolving one action of an instance can clear the
        // rest, so the set is read once and each entry that is still held is applied.
        let mut timed_out: Vec<(String, String, String, String)> = Vec::new();
        for group in self.groups.values() {
            for instance in &group.instances {
                for action in &instance.pending_lifecycle_actions {
                    if action.timeout_at() <= now {
                        timed_out.push((
                            group.name.clone(),
                            instance.instance_id.clone(),
                            action.token.clone(),
                            action.default_result.clone(),
                        ));
                    }
                }
            }
        }
        for (group_name, instance_id, token, default_result) in timed_out {
            self.resolve_lifecycle_action(
                engine,
                &group_name,
                &instance_id,
                &token,
                &default_result,
                now,
            );
        }
    }
}

/// `seconds` as a duration, for a value the model carries as a count of seconds.
fn seconds(value: i32) -> Duration {
    Duration::from_secs(u64::from(value.max(0).unsigned_abs()))
}

/// The `detail` of a lifecycle-action event: the hold, as the store holds it.
///
/// Every member is read off the queued hold, so the event reports the action a callback can
/// complete and never a second, invented copy of it. `NotificationMetadata` is written only
/// for a hook that carries one, because a hook with none adds nothing to the message.
fn lifecycle_action_detail(event: &PendingLifecycleActionEvent) -> ::serde_json::Value {
    let mut detail = ::serde_json::Map::new();
    insert_text(&mut detail, "LifecycleActionToken", &event.token);
    insert_text(&mut detail, "AutoScalingGroupName", &event.group_name);
    insert_text(&mut detail, "LifecycleHookName", &event.hook_name);
    insert_text(&mut detail, "EC2InstanceId", &event.instance_id);
    insert_text(&mut detail, "LifecycleTransition", event.transition.name());
    if let Some(metadata) = event.notification_metadata.as_deref() {
        insert_text(&mut detail, "NotificationMetadata", metadata);
    }
    ::serde_json::Value::Object(detail)
}

/// Publish `events` onto the account's default event bus, through the one request loop every
/// event of this service goes out by ([`publish_events`]).
///
/// Called with NO store lock held, by the same drain that publishes the group's instance
/// lifecycle events and over the same in-process callback.
pub(crate) async fn publish(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    events: &[PendingLifecycleActionEvent],
) {
    let outbound: Vec<OutboundEvent> = events
        .iter()
        .map(|event| OutboundEvent {
            region: event.region.clone(),
            detail_type: event.transition.detail_type(),
            // The event's `time` is when the instance entered the wait state, not when the
            // queue was drained.
            time: event.due,
            // The event names every resource it is about: the group, and the instance being
            // held.
            resources: vec![
                event.group_arn.clone(),
                instance_arn(&event.region, &event.instance_id),
            ],
            detail: lifecycle_action_detail(event).to_string(),
            subject: event.instance_id.clone(),
        })
        .collect();
    publish_events(callback, &outbound).await;
}
