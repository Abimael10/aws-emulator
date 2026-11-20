// SPDX-License-Identifier: CC0-1.0

//! The instances a caller asks a group to launch OUTRIGHT, beside the capacity changes
//! that launch them to converge on a desired capacity.
//!
//! `LaunchInstances` "launches a specified number of instances in an Auto Scaling group"
//! (`smithy_query(query="operation",operation="LaunchInstances")`), so what it launches are
//! instances OF THE GROUP: they come from the group's own launch source through the one
//! launch path every instance of a group is launched through
//! ([`AutoScalingState::launch_into_group`]), they are the account's own Amazon EC2
//! instances ([`super::ec2_instances`]), and the group's desired capacity rises by the
//! number launched — a group holds the instances its capacity says it holds, so a launch
//! that left the capacity where it was would leave the group holding instances it was never
//! asked for.
//!
//! The launch is reported back per PLACEMENT and instance type (`InstanceCollection`),
//! which is what tells the instances of one launch apart, and a unit of capacity nothing
//! was placed for is reported as an error of that same placement (`LaunchInstancesError`)
//! rather than as an instance that does not exist — the same shortfall the failed scaling
//! activity of the launch records, worded identically, so the two agree.
//!
//! # One request, one launch
//!
//! `ClientToken` is "a unique, case-sensitive identifier to ensure idempotency of the
//! request" (`LaunchInstancesRequest$ClientToken`), so the answer a token's first request
//! produced is stored ([`StoredLaunchRequest`]) and answered again for a repeat of that
//! token: the repeat launches nothing. A token already used for OTHER parameters is a
//! different request under one token, which is what `IdempotentParameterMismatchError`
//! states.

use std::time::SystemTime;

use crate::ops::types::{InstanceCollection, LaunchInstancesError2};

use super::{
    ec2_instances, failed_launch_description, zone_id_of, AutoScalingState, BlockedLaunch,
    GroupLaunch, GroupLaunchOutcome, InstanceLifecycle, InstanceLifecycleChange,
    LaunchDestination, PendingCapacity, LAUNCH_FAILED_SENTENCE, LIFECYCLE_TRANSITION,
    PROCESS_LAUNCH, UNPLACED_INSTANCE_MESSAGE,
};

/// The most instances ONE launch places.
///
/// From the model's own documentation for `LaunchInstancesRequest$RequestedCapacity`:
/// "although this value can exceed 100 for instance weights, the actual instance count is
/// limited to 100 instances per launch". The capacity a request asks for is therefore not
/// refused past this; it is the INSTANCE COUNT that stops here.
pub const MAX_INSTANCES_PER_LAUNCH: i32 = 100;

/// How many client tokens this store remembers the answer of.
///
/// A BOUND ON THIS PROCESS, not a value AWS documents, in the shape every other
/// collection of this store is bounded in ([`super::MAX_RETAINED_ACTIVITIES`]): each
/// remembered token holds the answer its request produced, so a run of launches under
/// fresh tokens cannot grow this store without end. The oldest token gives way first, and
/// a repeat of a token this store no longer holds is a launch of its own.
pub const MAX_RETAINED_LAUNCH_REQUESTS: usize = 1_000;

/// The `RetryStrategy` that retries the units a launch could not place: "increase desired
/// capacity and retry with group configuration"
/// (`smithy_query(query="doc",shape="LaunchInstancesRequest",member="RetryStrategy")`).
pub const RETRY_WITH_GROUP_CONFIGURATION: &str = "retry-with-group-configuration";

/// The `RetryStrategy` that retries nothing — the default, "no async retry" (the same
/// member's documentation).
pub const RETRY_STRATEGY_NONE: &str = "none";

/// The `RetryStrategy` values a request may name, from the model's own `RetryStrategy`
/// enum (`smithy_query(query="shape",shape="RetryStrategy")`).
pub const RETRY_STRATEGIES: &[&str] = &[RETRY_WITH_GROUP_CONFIGURATION, RETRY_STRATEGY_NONE];

/// `LaunchInstancesError$ErrorCode` for a unit of capacity the ACCOUNT could not place:
/// Amazon EC2's own code for capacity it could not provide, which is what a shortfall of
/// the account's own capacity is.
pub const INSUFFICIENT_CAPACITY_CODE: &str = "InsufficientInstanceCapacity";

/// `LaunchInstancesError$ErrorCode` for a launch something about the GROUP stopped
/// outright — an attachment Elastic Load Balancing v2 can take no registration into, a
/// launch source the account's stores cannot satisfy, a suspended `Launch` process.
///
/// The code this service answers a request it cannot satisfy with
/// ([`crate::scaffolded::Scaffolded::validation_error`]), carried here per failed
/// placement because the launch, not the request, is what could not be made.
pub const LAUNCH_BLOCKED_CODE: &str = "ValidationError";

/// What asked for the capacity a launch request adds, as the activities it records state
/// it: reads as the subject of [`AutoScalingState::capacity_change_cause`], the way a
/// manual termination's reason does ([`super::TERMINATE_INSTANCE_REASON`]).
pub const LAUNCH_INSTANCES_REASON: &str = "a user request explicitly launched instances in the \
                                           group";

/// What asked for the capacity the ASYNCHRONOUS retry of a launch request adds, for a
/// request that asked for one: the retry is a capacity change of the group, made from the
/// group's own configuration rather than from the placement the launch named.
pub const RETRY_LAUNCH_REASON: &str = "a launch request is being retried with the group \
                                       configuration";

/// What the error of a unit a group's suspended `Launch` process stopped reports.
///
/// "If you suspend `Launch`, your Auto Scaling group does not add instances"
/// (`SuspendProcesses`' own documentation): a launch request adds instances, so a group
/// that has suspended that process places none — and says so where every other launch
/// failure of this crate says why, ending with the sentence they all end with
/// ([`LAUNCH_FAILED_SENTENCE`]).
#[must_use]
pub fn launch_suspended_message() -> String {
    format!(
        "The {PROCESS_LAUNCH} process of the Auto Scaling group is suspended, so no instance \
         was added. {LAUNCH_FAILED_SENTENCE}"
    )
}

/// Where a launch request places its instances, when it names a placement narrower than
/// the group's own configuration.
///
/// The request states it as Availability Zones, as Availability Zone IDs or as subnets, and
/// the three are resolved to ONE placement before this is built: the zones the instances go
/// in, and the subnets of those zones the launch may use. Empty zones are no placement of
/// the caller's own, so the group's own configuration is what the launch then places from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestedPlacement {
    /// The Availability Zones the instances are placed in — a subset of the group's own.
    pub zones: Vec<String>,
    /// The subnets the launch places into, as the comma-separated value every other reader
    /// of a group's subnets takes ([`super::ec2_subnets::subnet_ids`]); `None` for a
    /// request that named no subnet, whose instances are placed by zone.
    pub vpc_zone_identifier: Option<String>,
}

/// What a caller asked a group to launch: how many instances, where, and whether the units
/// it cannot place are retried from the group's configuration afterwards.
#[derive(Debug)]
pub struct RequestedLaunch<'a> {
    /// The group the instances are launched into.
    pub group_name: &'a str,
    /// How many instances to launch, already bounded by [`MAX_INSTANCES_PER_LAUNCH`] and
    /// by the room the group's `MaxSize` leaves.
    pub count: usize,
    /// The placement the request named, when it named one narrower than the group's.
    pub placement: Option<&'a RequestedPlacement>,
    /// Whether the request asked for the units this launch cannot place to be retried
    /// asynchronously from the group's own configuration
    /// ([`RETRY_WITH_GROUP_CONFIGURATION`]).
    pub retry_with_group_configuration: bool,
}

/// What one launch request produced: the instances it placed, grouped the way the API
/// reports them, and the shortfall it could not place.
#[derive(Debug, Clone, Default)]
pub struct RequestedLaunchOutcome {
    /// The instances launched, one entry per placement and instance type they were
    /// launched with, in launch order.
    pub instances: Vec<InstanceCollection>,
    /// The placements a unit of the capacity asked for was not placed in, one entry per
    /// placement, carrying why.
    pub errors: Vec<LaunchInstancesError2>,
}

/// The answer one client token produced, so a repeat of that token is answered with it
/// rather than launching a second time.
#[derive(Debug, Clone)]
pub struct StoredLaunchRequest {
    /// The token the request carried.
    pub client_token: String,
    /// The parameters it carried, as one value: a repeat carrying others is a different
    /// request under one token.
    pub fingerprint: String,
    /// What that request answered.
    pub outcome: RequestedLaunchOutcome,
}

impl AutoScalingState {
    /// What a request under `client_token` already answered, when this store still
    /// remembers one.
    #[must_use]
    pub fn launch_request(&self, client_token: &str) -> Option<&StoredLaunchRequest> {
        self.launch_requests
            .iter()
            .find(|request| request.client_token == client_token)
    }

    /// Remember what a launch request answered, keeping the remembered tokens bounded:
    /// the oldest gives way first ([`MAX_RETAINED_LAUNCH_REQUESTS`]).
    pub fn record_launch_request(&mut self, request: StoredLaunchRequest) {
        self.launch_requests.push(request);
        if self.launch_requests.len() > MAX_RETAINED_LAUNCH_REQUESTS {
            let excess = self.launch_requests.len() - MAX_RETAINED_LAUNCH_REQUESTS;
            self.launch_requests.drain(..excess);
        }
    }

    /// Launch what a caller asked for into the group, raise the group's desired capacity by
    /// the instances it placed, and answer them as the API reports them.
    ///
    /// The instances are the account's OWN Amazon EC2 instances, launched from the group's
    /// launch source through the one launch path every instance of a group is launched
    /// through, so every id this answers resolves in the account's Amazon EC2 state. Each
    /// launch records its own scaling activity and the lifecycle event that reports it, the
    /// way a capacity change's launches do, and each unit nothing was placed for records a
    /// failed activity instead of an instance.
    ///
    /// A group that has suspended its `Launch` process places nothing: "if you suspend
    /// Launch, your Auto Scaling group does not add instances" — the whole of what the
    /// launch did is then the shortfall it reports.
    ///
    /// Does nothing for a name no group answers to; the caller refuses such a request
    /// before reaching here.
    pub fn launch_requested_instances<Eng: Send + Sync + 'static>(
        &mut self,
        engine: &Eng,
        request: &RequestedLaunch<'_>,
        now: SystemTime,
    ) -> RequestedLaunchOutcome {
        let Some(group) = self.groups.get(request.group_name) else {
            return RequestedLaunchOutcome::default();
        };
        let arn = group.arn.clone();
        let desired = group.desired_capacity;
        let max_size = group.max_size;
        let launch_suspended = group.is_process_suspended(PROCESS_LAUNCH);
        let due = now + LIFECYCLE_TRANSITION;

        let launch = if launch_suspended {
            GroupLaunchOutcome::unplaced(
                request.count,
                Some(BlockedLaunch::failed(launch_suspended_message())),
            )
        } else {
            self.launch_into_group(
                engine,
                &GroupLaunch {
                    group_name: request.group_name,
                    count: request.count,
                    launched_at: now,
                    due,
                    // A launch request launches from the group's own launch source: it
                    // states WHERE to launch, never what to launch.
                    desired_configuration: None,
                    destination: LaunchDestination::Group,
                    placement: request.placement,
                },
            )
        };
        let launched_ids = launch.launched_ids.clone();
        let unplaced = launch.unplaced;
        let unplaced_message = launch.unplaced_message(UNPLACED_INSTANCE_MESSAGE);
        let unplaced_status_code = launch.unplaced_status_code();
        let error_code = if launch.blocked.is_some() {
            LAUNCH_BLOCKED_CODE
        } else {
            INSUFFICIENT_CAPACITY_CODE
        };

        // The group holds the instances its capacity says it holds, so the capacity rises by
        // the instances this launch actually placed — never by the ones it could not.
        //
        // The ceiling is never below the floor: a capacity is a count, so the highest the
        // group may be left at is its own `MaxSize` or the capacity it already holds,
        // whichever is greater, and never below zero.
        let placed = i32::try_from(launched_ids.len()).unwrap_or(i32::MAX);
        let ceiling = max_size.max(desired).max(0);
        let new_desired = desired.saturating_add(placed).clamp(0, ceiling);
        let cause =
            AutoScalingState::capacity_change_cause(now, LAUNCH_INSTANCES_REASON, desired, new_desired);
        // The units this launch could not place are retried from the group's own
        // configuration when the request asked for that, and the retry is ASYNCHRONOUS: the
        // capacity it adds falls due on the clock, which is what applies it and launches
        // from the whole of the group's configuration rather than from the placement this
        // request named.
        let retried = if request.retry_with_group_configuration && unplaced > 0 {
            let shortfall = i32::try_from(unplaced).unwrap_or(i32::MAX);
            let ceiling = max_size.max(new_desired).max(0);
            Some(new_desired.saturating_add(shortfall).clamp(0, ceiling))
        } else {
            None
        };
        if let Some(group) = self.groups.get_mut(request.group_name) {
            group.desired_capacity = new_desired;
            // The group's cooldown is left where it is: it is "the amount of time, in
            // seconds, between one scaling activity ending and another one starting due to
            // simple scaling policies" (`CreateAutoScalingGroupType$DefaultCooldown`), and a
            // launch request is neither a simple scaling policy nor a call that carries a
            // cooldown to honour.
            //
            // Only a launch that owes a retry queues one: a capacity change some other
            // caller already left pending on this group is still owed, so it is left where
            // it is rather than discarded by a launch that has nothing to retry.
            if let Some(desired_capacity) = retried {
                group.pending_capacity = Some(PendingCapacity {
                    desired_capacity,
                    due,
                    cause: AutoScalingState::capacity_change_cause(
                        now,
                        RETRY_LAUNCH_REASON,
                        new_desired,
                        desired_capacity,
                    ),
                    // A retried launch leaves the group's own `DefaultCooldown` in force:
                    // nothing of its own overrides it.
                    cooldown_override: None,
                });
            }
        }

        for instance_id in &launched_ids {
            self.record_instance_lifecycle_change(
                engine,
                &InstanceLifecycleChange {
                    kind: InstanceLifecycle::Launched,
                    group_name: request.group_name,
                    group_arn: &arn,
                    instance_id,
                    cause: &cause,
                    start_time: now,
                    due,
                },
            );
        }
        for _ in 0..unplaced {
            self.record_failed_activity(
                request.group_name,
                &arn,
                failed_launch_description(&unplaced_message),
                unplaced_message.clone(),
                unplaced_status_code,
                cause.clone(),
                now,
            );
        }

        RequestedLaunchOutcome {
            instances: self.launched_collections(engine, request.group_name, &launched_ids),
            errors: if unplaced > 0 {
                vec![self.shortfall_error(
                    engine,
                    request.group_name,
                    request.placement,
                    error_code,
                    &unplaced_message,
                )]
            } else {
                Vec::new()
            },
        }
    }

    /// The instances `launched_ids` names, grouped as the API reports them: one entry per
    /// instance type and placement, in launch order.
    ///
    /// Every value comes from the record of the instance itself — the type Amazon EC2
    /// launched it as and the zone the group placed it in, with the subnet read from the
    /// account's own Amazon EC2 store ([`ec2_instances::subnet_of`]), where the placement
    /// is EC2's own fact — so the collection reports what was launched rather than what was
    /// asked for. `MarketType` is left unfilled: neither this service nor the account's
    /// Amazon EC2 record of the instance holds one, the way `WeightedCapacity` is left
    /// unfilled on an instance ([`super::StoredInstance::to_instance`]).
    fn launched_collections<Eng: Send + Sync + 'static>(
        &self,
        engine: &Eng,
        group_name: &str,
        launched_ids: &[String],
    ) -> Vec<InstanceCollection> {
        let Some(group) = self.groups.get(group_name) else {
            return Vec::new();
        };
        let mut collections: Vec<InstanceCollection> = Vec::new();
        for instance_id in launched_ids {
            let Some(instance) = group
                .instances
                .iter()
                .find(|instance| instance.instance_id == *instance_id)
            else {
                continue;
            };
            let subnet_id = ec2_instances::subnet_of(engine, instance_id);
            let instance_type = instance.instance_type.clone();
            let zone = instance.availability_zone.clone();
            if let Some(collection) = collections.iter_mut().find(|collection| {
                collection.instance_type == instance_type
                    && collection.subnet_id == subnet_id
                    && collection.availability_zone.as_deref() == Some(zone.as_str())
            }) {
                collection
                    .instance_ids
                    .get_or_insert_with(Vec::new)
                    .push(instance_id.clone());
                continue;
            }
            collections.push(InstanceCollection {
                instance_type,
                market_type: None,
                subnet_id,
                availability_zone: Some(zone.clone()),
                availability_zone_id: zone_id_of(&zone),
                instance_ids: Some(vec![instance_id.clone()]),
            });
        }
        collections
    }

    /// The error a launch's shortfall reports: the placement the units were asked for, the
    /// code for what stopped them and the sentence the failed activity of the same launch
    /// records.
    ///
    /// The placement is the one the REQUEST named, since that is where the instances were
    /// asked for, and the group's own when the request named none. The instance type is the
    /// one the group would have launched them as — the type its launch source names,
    /// resolved against the account's own Amazon EC2 launch templates — because there is no
    /// launched instance to read it off.
    fn shortfall_error<Eng: Send + Sync + 'static>(
        &self,
        engine: &Eng,
        group_name: &str,
        placement: Option<&RequestedPlacement>,
        error_code: &str,
        message: &str,
    ) -> LaunchInstancesError2 {
        let group = self.groups.get(group_name);
        let zone = placement
            .and_then(|placement| placement.zones.first().cloned())
            .or_else(|| group.and_then(|group| group.availability_zones.first().cloned()));
        let subnets = placement
            .and_then(|placement| placement.vpc_zone_identifier.clone())
            .or_else(|| group.and_then(|group| group.vpc_zone_identifier.clone()));
        let subnet_id = subnets.as_deref().and_then(|value| {
            super::ec2_subnets::subnet_ids(value)
                .first()
                .map(|id| (*id).to_string())
        });
        let instance_type = group.and_then(|group| {
            super::declared_instance_types(group.mixed_instances_policy.as_ref())
                .into_iter()
                .next()
                .or_else(|| {
                    super::launch_template_of(
                        group.launch_template.as_ref(),
                        group.mixed_instances_policy.as_ref(),
                    )
                    .and_then(|specification| {
                        ec2_instances::launch_template_data(engine, specification)
                    })
                    .and_then(|data| data.instance_type)
                })
        });
        LaunchInstancesError2 {
            instance_type,
            market_type: None,
            subnet_id,
            availability_zone_id: zone.as_deref().and_then(zone_id_of),
            availability_zone: zone,
            error_code: Some(error_code.to_string()),
            error_message: Some(message.to_string()),
        }
    }
}
