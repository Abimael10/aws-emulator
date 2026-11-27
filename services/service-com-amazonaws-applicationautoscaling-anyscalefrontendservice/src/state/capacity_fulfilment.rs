// SPDX-License-Identifier: CC0-1.0

//! FULFILMENT of a capacity movement: when the service that owns the scaled resource has
//! APPLIED the capacity this service set on it.
//!
//! A capacity movement is two things, not one. This service DECIDES a capacity and sets it
//! on the resource, which is what the movement's activity records the instant it happens
//! ("Setting desired count to 3."); the owning service then has to take that capacity on,
//! which is what the same activity reports it is still waiting for ("Successfully set
//! desired count to 3. Waiting for change to be fulfilled by ecs.") until it has
//! ("Change successfully fulfilled by ecs."). So a movement is non-terminal for the span this
//! service gives it ([`super::ACTIVITY_FULFILMENT`]) and reaches `Successful` once the capacity
//! it set is the capacity the owning service holds for the scaled property.
//!
//! What gets fulfilled is the CHANGE, not the work the changed capacity implies. A scalable
//! dimension "consists of the service namespace, resource type, and scaling property", and the
//! property this one names is the task count of an ECS service
//! (`RegisterScalableTarget$ScalableDimension`: "ecs:service:DesiredCount - The task count of an
//! ECS service") — `Service$desiredCount`, "the desired number of instantiations of the task
//! definition to keep running on the service". So the owning service has fulfilled the change
//! once it HOLDS that property at the capacity the movement set, which is the one thing this
//! service asked it for.
//!
//! What that service then does to honour the count is its own scheduling, and no part of the
//! change: `Service$runningCount`, "the number of tasks in the cluster that are in the RUNNING
//! state", catches the desired count up on that service's own clock, long after the change was
//! taken on. A movement that waited for it would report the change as still on its way for as
//! long as the placement took — and for ever on an estate with nowhere to place a task, which
//! is a cluster whose scheduler has no capacity rather than a change Amazon ECS has not taken.
//! Recorded behaviour says the same: a scaling activity of an ECS service carries an `EndTime`
//! about a second after its `StartTime`, well before any task the new count asks for could have
//! reached `RUNNING`.
//!
//! Which capacity the owning service holds is a fact only that service has, so it is READ
//! there, once per pass ([`settle_activity_fulfilments`]), for the movements whose own span has
//! already elapsed — never before, because the capacity is written into the owning service's
//! store after the activity is recorded
//! ([`super::policy_execution::settle_policy_executions`]), so a reading taken at the recording
//! instant would be of the capacity the resource held BEFORE.
//!
//! One arm per namespace whose owning service holds the scaled capacity in a store this
//! emulator can read: Amazon ECS does. For a namespace whose capacity cannot be read back that
//! way there is nothing to observe, and the movement is fulfilled by the span alone.

use std::collections::BTreeSet;
use std::time::SystemTime;

use super::{
    service_state, ComAmazonawsApplicationautoscalingAnyscalefrontendserviceState, TargetKey,
};

/// The `ecs` namespace, whose owning service holds the capacity `ecs:service:DesiredCount`
/// scales as `Service$desiredCount`.
const ECS_NAMESPACE: &str = "ecs";

/// Whether a movement of a target in this namespace has its fulfilment CONFIRMED in the
/// owning service rather than by this service's own span alone.
///
/// Namespace by namespace, because it is a property of the owning service and not of any one
/// request: a service whose stored capacity this emulator can read back is one whose
/// confirmation can be taken, and the next such namespace is a new arm of
/// [`owner_has_fulfilled`] plus a row here.
pub fn owner_confirms_fulfilment(service_namespace: &str) -> bool {
    service_namespace == ECS_NAMESPACE
}

/// How many activities ONE pass asks the owning service about.
///
/// The pass's OWN bound, the sibling of `policy_execution::MAX_EXECUTIONS_PER_PASS` and for
/// the same reason: how many movements are awaiting confirmation is the caller's choice, so
/// the work of a pass is fixed here. Nothing starves — the activities are taken oldest first,
/// so what this pass leaves out is what the next pass takes.
const MAX_CONFIRMATIONS_PER_PASS: usize = 32;

/// One movement this pass has to ask the owning service about.
#[derive(Debug, Clone)]
struct Awaiting {
    /// The activity, by the id it is reported under.
    activity_id: String,
    target: TargetKey,
    /// The capacity the movement set, which is the capacity the owning service has to have in
    /// effect.
    capacity: i32,
}

/// Confirm, for every capacity movement of `region` still waiting on the service that owns
/// the scaled resource, whether that service has taken the capacity on — and end the activity
/// at the instant it has.
///
/// Call it with NO store lock held: the activities are collected under this service's lock,
/// the owning service's store is read with it released, and the confirmations are written
/// under it again.
///
/// This is work the CLOCK owes and not any request's: the capacity was applied in the owning
/// service at an instant of its own, whether or not anybody called this service afterwards,
/// so a reporting operation may materialise it.
pub fn settle_activity_fulfilments<Eng: Send + Sync + 'static>(engine: &Eng, region: &str) {
    let Some(state) = service_state(engine) else {
        return;
    };
    let now = SystemTime::now();
    let awaiting = awaiting_confirmation(&state.lock(), region, now);
    if awaiting.is_empty() {
        return;
    }
    // The owning services' OWN clocks first, once for the whole pass: what each of them owes
    // the resources this pass asks about fell due at an instant of that service's own, so a
    // reading taken without materialising it would report a capacity already in effect as
    // outstanding.
    settle_owner_clocks(engine, &awaiting);
    let confirmed: BTreeSet<String> = awaiting
        .into_iter()
        .filter(|movement| owner_has_fulfilled(engine, &movement.target, movement.capacity))
        .map(|movement| movement.activity_id)
        .collect();
    if confirmed.is_empty() {
        return;
    }
    confirm_fulfilled(&mut state.lock(), &confirmed, SystemTime::now().max(now));
}

/// Materialise what the clock owes in each service that owns a resource this pass asks about,
/// one pass per owning service rather than one per movement.
///
/// Taken with NO store lock held, this service's or the owning service's: each pass takes the
/// locks it needs itself.
fn settle_owner_clocks<Eng: Send + Sync + 'static>(engine: &Eng, awaiting: &[Awaiting]) {
    use ::service_com_amazonaws_ecs_amazonec2containerservicev20141113 as ecs;

    let asks_ecs = awaiting
        .iter()
        .any(|movement| movement.target.service_namespace == ECS_NAMESPACE);
    if asks_ecs && ecs::state::service_state(engine).is_some() {
        ecs::state::advance_task_clock(engine, SystemTime::now());
    }
}

/// The movements of `region` whose fulfilment the owning service has still to confirm, and
/// whose own span has already elapsed.
fn awaiting_confirmation(
    store: &ComAmazonawsApplicationautoscalingAnyscalefrontendserviceState,
    region: &str,
    now: SystemTime,
) -> Vec<Awaiting> {
    store
        .activities
        .iter()
        .filter(|activity| activity.awaiting_owner && activity.target.region == region)
        .filter(|activity| activity.fulfilled_at.is_none_or(|due| due <= now))
        .take(MAX_CONFIRMATIONS_PER_PASS)
        .map(|activity| Awaiting {
            activity_id: activity.activity_id.clone(),
            target: activity.target.clone(),
            capacity: activity.capacity,
        })
        .collect()
}

/// End each confirmed movement at `at`: the capacity it set is the one the owning service
/// holds, so the activity reaches the terminal status it was recorded with and its `EndTime`
/// is that instant.
fn confirm_fulfilled(
    store: &mut ComAmazonawsApplicationautoscalingAnyscalefrontendserviceState,
    confirmed: &BTreeSet<String>,
    at: SystemTime,
) {
    for activity in store
        .activities
        .iter_mut()
        .filter(|activity| confirmed.contains(&activity.activity_id))
    {
        activity.awaiting_owner = false;
        // Never earlier than the instant the movement started, so an activity cannot report
        // having ended before it began.
        activity.fulfilled_at = Some(at.max(activity.start_time));
    }
}

/// Whether the service that OWNS the scaled resource has applied `capacity` to it — or holds
/// nothing this movement can wait on, which is as far as the waiting can go.
///
/// One arm per namespace whose owning service holds the scaled capacity in a store this
/// emulator can read; see the module docs.
fn owner_has_fulfilled<Eng: Send + Sync + 'static>(
    engine: &Eng,
    target: &TargetKey,
    capacity: i32,
) -> bool {
    match target.service_namespace.as_str() {
        ECS_NAMESPACE => ecs_has_fulfilled(engine, &target.resource_id, capacity),
        _ => true,
    }
}

/// Whether the ECS service an `ecs` `ResourceId` names HOLDS the task count a movement set on
/// it, which is the property `ecs:service:DesiredCount` scales: `Service$desiredCount`, "the
/// desired number of instantiations of the task definition to keep running on the service".
///
/// A count the service does not hold yet is a change still on its way: the movement is queued
/// for the owning service and delivered with this service's lock released
/// (`super::policy_execution::deliver_pending_capacity`), so the count is read back here rather
/// than assumed from the queueing. A LATER movement of the same target, which leaves the
/// service driving towards ITS capacity instead, resolves this activity as overridden rather
/// than fulfilled
/// (`super::ComAmazonawsApplicationautoscalingAnyscalefrontendserviceState::override_unfulfilled_activities`).
///
/// What the scheduler then does to honour that count — placing or stopping the tasks it asks
/// for, which is what `Service$runningCount` reports — is the owning service's own work and no
/// part of the change this movement made; see the module docs.
///
/// What the ECS clock has already made due is materialised by the pass before the count is read
/// ([`settle_owner_clocks`]): it was owed at an instant of that service's own, whether or not
/// anybody called it, so reading without taking it would report a change already taken on as
/// outstanding.
///
/// There is nothing to wait on — so the movement is as fulfilled as it will get — where that
/// count says nothing about this movement at all: a resource id that names no ECS service, an
/// engine that holds no ECS store, and a service that is no longer active.
fn ecs_has_fulfilled<Eng: Send + Sync + 'static>(
    engine: &Eng,
    resource_id: &str,
    capacity: i32,
) -> bool {
    use ::service_com_amazonaws_ecs_amazonec2containerservicev20141113 as ecs;

    let Some((cluster, service_name)) = super::split_ecs_resource_id(resource_id) else {
        return true;
    };
    let Some(store) = ecs::state::service_state(engine) else {
        return true;
    };
    let store = store.lock();
    let Some(desired_count) = store
        .services
        .get(&(cluster.to_string(), service_name.to_string()))
        .filter(|service| service.is_active())
        .map(|service| service.desired_count)
    else {
        return true;
    };
    desired_count == capacity
}
