// SPDX-License-Identifier: CC0-1.0

//! What a `ModifySpotFleetRequest` changes about a stored Spot Fleet request: the states a
//! modification runs from, the capacity the fleet holds toward its target, and the one writer
//! that scales a fleet up and down.
//!
//! "Modifies the specified Spot Fleet request. You can only modify a Spot Fleet request of
//! type `maintain`. While the Spot Fleet request is being modified, it is in the `modifying`
//! state. To scale up your Spot Fleet, increase its target capacity … To scale down your Spot
//! Fleet, decrease its target capacity. First, the Spot Fleet cancels any open requests that
//! exceed the new target capacity. You can request that the Spot Fleet terminate Spot
//! Instances until the size of the fleet no longer exceeds the new target capacity.
//! Alternatively, you can request that the Spot Fleet keep the fleet at its current size, but
//! not replace any Spot Instances that are interrupted" (`ModifySpotFleetRequest`) — so the
//! CALL places and releases the Spot Instance requests the new target capacity takes, and what
//! the fleet still owes the clock is its own state word.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every other
//! primitive there.

use super::*;

use crate::ops::types::LaunchTemplateConfig;

/// The `BatchState` members a Spot Fleet request can be MODIFIED from: one that is holding
/// capacity, one that has just been accepted and one whose capacity is already being changed.
/// Every other member is a fleet that has been cancelled or that failed, so there is nothing
/// left to modify.
pub const SPOT_FLEET_MODIFIABLE_STATES: &[&str] = &[
    SPOT_FLEET_STATE_SUBMITTED,
    SPOT_FLEET_STATE_ACTIVE,
    SPOT_FLEET_STATE_MODIFYING,
];

/// Whether a Spot Fleet request in `state` can be modified
/// ([`SPOT_FLEET_MODIFIABLE_STATES`]).
pub fn spot_fleet_request_state_is_modifiable(state: &str) -> bool {
    SPOT_FLEET_MODIFIABLE_STATES.contains(&state)
}

/// The `modify_in_progress` `fleetRequestChange` sub type: "`modify_in_progress` - the EC2
/// Fleet or Spot Fleet request is being modified. The request remains in this state until the
/// modification is fully processed" (`EventInformation$EventSubType`).
pub const EVENT_SUB_TYPE_MODIFY_IN_PROGRESS: &str = "modify_in_progress";

/// The `modify_succeeded` `fleetRequestChange` sub type: "`modify_succeeded` - the EC2 Fleet or
/// Spot Fleet request was modified" (`EventInformation$EventSubType`).
pub const EVENT_SUB_TYPE_MODIFY_SUCCEEDED: &str = "modify_succeeded";

/// The `termination_notified` `instanceChange` sub type: "`termination_notified` - an instance
/// termination notification was sent when a Spot Instance was terminated by Amazon EC2 during
/// scale-down, when the target capacity of the fleet was modified down"
/// (`EventInformation$EventSubType`) — the event a scale-down that terminates an instance
/// records, which is why it is not one of the sub types a cancellation records.
pub const EVENT_SUB_TYPE_TERMINATION_NOTIFIED: &str = "termination_notified";

/// The `noTermination` member of the modelled `ExcessCapacityTerminationPolicy` catalog
/// ([`crate::ops::types::EXCESS_CAPACITY_TERMINATION_POLICY_VALUES`]): "you can request that
/// the Spot Fleet keep the fleet at its current size, but not replace any Spot Instances that
/// are interrupted or that you terminate manually" (`ModifySpotFleetRequest`).
pub const EXCESS_CAPACITY_NO_TERMINATION: &str = "noTermination";

/// The `default` member of the same catalog: the policy under which "the Spot Fleet terminates
/// Spot Instances until the size of the fleet no longer exceeds the new target capacity"
/// (`ModifySpotFleetRequest`), which is what a fleet that names no policy at all gets — the
/// catalog names this member `default` precisely because it is the behaviour in force when
/// nothing else is asked for.
pub const EXCESS_CAPACITY_TERMINATION_DEFAULT: &str = "default";

/// Whether a fleet whose excess-capacity policy is `policy` TERMINATES the running instances a
/// decreased target capacity leaves it holding.
///
/// One reading of the member for every path that scales a fleet down, so the only policy that
/// keeps the instances running is the one that says so.
pub fn spot_fleet_terminates_excess_capacity(policy: Option<&str>) -> bool {
    policy != Some(EXCESS_CAPACITY_NO_TERMINATION)
}

/// The most capacity units ONE Spot Fleet request may be asked for in this emulator.
///
/// "The size of the fleet" (`ModifySpotFleetRequestRequest$TargetCapacity`) is a plain modelled
/// `Integer` with no `@range`, and it is the number every placement this store makes for the
/// fleet is computed from — so a bound on it is what keeps the work and the memory one call asks
/// for from being chosen by a caller's integer, exactly as [`MAX_SPOT_FLEET_POOLS`] bounds the
/// per-pool work one call asks for.
///
/// A chosen ceiling of this store rather than an AWS limit: it is far above the capacity the
/// instances one fleet may place ([`MAX_SPOT_FLEET_SPOT_INSTANCES`]) can provide at any weight a
/// pool realistically carries, so nothing a caller can usefully ask of this emulator is refused
/// by it.
pub const MAX_SPOT_FLEET_TARGET_CAPACITY: i32 = 65_536;

/// Refuse a capacity a request names above what this store holds one Spot Fleet request to
/// ([`MAX_SPOT_FLEET_TARGET_CAPACITY`]), naming the member it was sent under and the value the
/// request sent — the shape every bound this crate puts on an unbounded modelled `Integer` is
/// refused with.
pub fn spot_fleet_capacity_above_ceiling<E>(member: &str, value: i32) -> Scaffolded<E> {
    Scaffolded::invalid_parameter_value(format!(
        "Invalid value '{value}' for {member}. This emulator holds one Spot Fleet request to at \
         most {MAX_SPOT_FLEET_TARGET_CAPACITY} capacity units"
    ))
}

/// Refuse a modification of a Spot Fleet request whose type is not
/// [`FLEET_TYPE_MAINTAIN`]: HTTP 400 `UnsupportedOperation`, with EC2's own sentence for the
/// restriction ("You can only modify a Spot Fleet request of type `maintain`",
/// `ModifySpotFleetRequest`), which carries no variable part.
pub fn spot_fleet_request_type_not_modifiable<E>() -> Scaffolded<E> {
    unsupported_operation("You can only modify a Spot Fleet request of type maintain.")
}

/// Refuse a modification of a Spot Fleet request that is not in a state a modification runs
/// from ([`SPOT_FLEET_MODIFIABLE_STATES`]): the state refusal every operation whose only
/// obstacle is the state of the resource it names shares ([`incorrect_state_of`]), naming the
/// fleet from the request and the `BatchState` from the store.
pub fn spot_fleet_request_not_modifiable<E>(request_id: &str, state: &str) -> Scaffolded<E> {
    incorrect_state_of("Spot fleet request", request_id, state)
}

/// Whether one placement of a Spot Fleet request still provides the fleet capacity: its Spot
/// Instance request is open or active, and the instance it launched — if it has launched one —
/// is one the instance store still holds un-terminated.
///
/// The ONE reading of "the fleet is holding this placement", so what a scale-up adds to and
/// what a scale-down releases from are the same set. A request that was cancelled, or whose
/// validity ran out, holds nothing for the fleet, and neither does one whose instance is gone.
pub fn spot_fleet_placement_is_held(
    placement: &SpotFleetPlacement,
    requests: &BTreeMap<String, SpotInstanceRequestRecord>,
    instances: &HashMap<String, InstanceRecord>,
) -> bool {
    let Some(request) = requests.get(&placement.spot_instance_request_id) else {
        return false;
    };
    let live = matches!(
        request.view.state.as_deref(),
        Some(SPOT_REQUEST_STATE_OPEN) | Some(SPOT_REQUEST_STATE_ACTIVE)
    );
    if !live {
        return false;
    }
    match request.view.instance_id.as_deref() {
        None => true,
        Some(instance_id) => instances
            .get(instance_id)
            .is_some_and(|instance| instance.state != InstanceStateName::Terminated),
    }
}

/// The capacity one Spot Fleet's `placements` currently HOLD toward its target capacity: the
/// weighted capacity of every placement the fleet is still holding
/// ([`spot_fleet_placement_is_held`]).
///
/// Not the same question as [`spot_fleet_fulfilled_capacity`], which counts the instances the
/// account still RUNS for this fleet whatever state their requests are in: a fleet whose
/// requests are still open HOLDS that capacity and has not had it FULFILLED yet, while a fleet
/// whose requests were cancelled with their instances left running has FULFILLED capacity it no
/// longer holds. That is why a scale-up adds nothing for capacity already placed and a
/// scale-down cancels those open requests first, and why neither derivation can be expressed as
/// the other.
pub fn spot_fleet_held_capacity(
    placements: &[SpotFleetPlacement],
    requests: &BTreeMap<String, SpotInstanceRequestRecord>,
    instances: &HashMap<String, InstanceRecord>,
) -> f64 {
    placements
        .iter()
        .filter(|placement| spot_fleet_placement_is_held(placement, requests, instances))
        .map(|placement| placement.weighted_capacity)
        // Folded from a POSITIVE zero rather than summed, for the reason
        // [`spot_fleet_fulfilled_capacity`] is: `Sum` for `f64` folds from a negative zero.
        .fold(0.0, |held, capacity| held + capacity)
}

/// What a `ModifySpotFleetRequest` changes about a stored Spot Fleet request, as its handler
/// has already validated it.
///
/// Every member is `None` for a request that leaves that part of the fleet as stored: "the
/// size of the fleet", "the number of On-Demand Instances in the fleet", "the launch template
/// and overrides" and whether excess capacity is terminated are four separate modifications,
/// and a request names any subset of them.
#[derive(Debug, Clone, Default)]
pub struct SpotFleetRequestModification {
    /// "The size of the fleet" (`ModifySpotFleetRequestRequest$TargetCapacity`).
    pub target_capacity: Option<i32>,
    /// "The number of On-Demand Instances in the fleet" (`$OnDemandTargetCapacity`).
    pub on_demand_target_capacity: Option<i32>,
    /// "Indicates whether running instances should be terminated if the target capacity of the
    /// Spot Fleet request is decreased below the current size of the Spot Fleet"
    /// (`$ExcessCapacityTerminationPolicy`), as a member of the modelled catalog rather than
    /// as the request's own text.
    pub excess_capacity_termination_policy: Option<&'static str>,
    /// "Reserved" (`$Context`): carried onto the fleet's stored configuration, so nothing a
    /// caller set is dropped and a describe reports what was sent.
    pub context: Option<String>,
    /// "The launch template and overrides" (`$LaunchTemplateConfigs`) as the request gave them,
    /// which is what the fleet's configuration then reports.
    pub launch_template_configs: Option<Vec<LaunchTemplateConfig>>,
    /// The Spot pools those launch template configurations resolve to
    /// ([`spot_fleet_template_pools`]), which is where the fleet places capacity from now on.
    /// Set exactly when `launch_template_configs` is.
    pub pools: Option<Vec<SpotFleetPoolPlacement>>,
}

impl SpotFleetRequestRecord {
    /// Whether a modification runs from the state this fleet is in
    /// ([`spot_fleet_request_state_is_modifiable`]).
    pub fn is_modifiable(&self) -> bool {
        spot_fleet_request_state_is_modifiable(self.state())
    }

    /// "The type of request" this fleet was created as
    /// (`SpotFleetRequestConfigData$Type`), out of [`FLEET_TYPE_VALUES`].
    pub fn fleet_type(&self) -> &str {
        self.config()
            .and_then(|config| config.r#type.as_deref())
            .unwrap_or_default()
    }

    /// Put this fleet in the state a modification runs through: "while the Spot Fleet request
    /// is being modified, it is in the `modifying` state" (`ModifySpotFleetRequest`), reaching
    /// `active` again one [`TRANSITION_DELAY`] later.
    ///
    /// Both halves are one event of the fleet's history — the modification that started
    /// (`modify_in_progress`) and the one that completed (`modify_succeeded`) — so the sub type
    /// the clock-owed move records is carried beside it rather than taken from the state word,
    /// which is what every other move of this fleet records.
    pub(super) fn begin_modification(&mut self, at: SystemTime) {
        self.view.spot_fleet_request_state = Some(SPOT_FLEET_STATE_MODIFYING.to_string());
        self.history.push(SpotFleetHistoryEvent::fleet_request_change(
            EVENT_SUB_TYPE_MODIFY_IN_PROGRESS,
            at,
        ));
        self.pending = Some(SpotFleetRequestTransition::starting_now(
            SPOT_FLEET_STATE_ACTIVE,
        ));
        self.pending_event_sub_type = Some(EVENT_SUB_TYPE_MODIFY_SUCCEEDED);
    }
}

impl ComAmazonawsEc2Amazonec2State {
    /// Apply a `ModifySpotFleetRequest` to the Spot Fleet request stored under `fleet_id`.
    ///
    /// The one way a stored Spot Fleet request is modified, so what a modification changes
    /// about it — its configuration, the Spot Instance requests it holds, the state it reports
    /// and the move the clock then owes it — cannot be applied two ways.
    ///
    /// The CALL places and releases capacity: "to scale up your Spot Fleet, increase its target
    /// capacity. The Spot Fleet launches the additional Spot Instances", and "to scale down your
    /// Spot Fleet, decrease its target capacity. First, the Spot Fleet cancels any open requests
    /// that exceed the new target capacity" (`ModifySpotFleetRequest`) — so the requests are
    /// facts of this call rather than of a later instant. What the fleet still owes the clock is
    /// its own STATE word: it is answered in the non-terminal `modifying` and reaches `active`
    /// one [`TRANSITION_DELAY`] later.
    ///
    /// Answers whether the fleet was modified: `false` when the id names no fleet of the
    /// account or names one in a state a modification does not run from
    /// ([`SPOT_FLEET_MODIFIABLE_STATES`]). A caller resolves the fleet, its type and its state
    /// first and words those refusals itself; this guard is what keeps the store from being
    /// written behind them.
    pub fn modify_spot_fleet_request(
        &mut self,
        fleet_id: &str,
        change: &SpotFleetRequestModification,
    ) -> bool {
        let Some(record) = self.spot_fleet_requests.get(fleet_id) else {
            return false;
        };
        if !record.is_modifiable() {
            return false;
        }
        let tags = record.view.tags.clone().unwrap_or_default();
        let mut config = record.config().cloned().unwrap_or_default();
        let charged_before = spot_fleet_request_payload_bytes(&config, &tags);
        let region = record.region.clone();
        let spot_request_type = record.spot_request_type;
        let interruption_behavior = record.interruption_behavior;
        let pools = change
            .pools
            .clone()
            .unwrap_or_else(|| record.pools.clone());
        let held = spot_fleet_held_capacity(
            &record.placements,
            &self.spot_instance_requests,
            &self.instances,
        );

        // The configuration as the modification leaves it: every member the request named, and
        // every other one exactly as the fleet was stored with it.
        if let Some(target_capacity) = change.target_capacity {
            config.target_capacity = target_capacity;
        }
        if let Some(on_demand_target_capacity) = change.on_demand_target_capacity {
            config.on_demand_target_capacity = Some(on_demand_target_capacity);
        }
        if let Some(policy) = change.excess_capacity_termination_policy {
            config.excess_capacity_termination_policy = Some(policy.to_string());
        }
        if let Some(context) = change.context.clone() {
            config.context = Some(context);
        }
        if let Some(launch_template_configs) = change.launch_template_configs.clone() {
            config.launch_template_configs = Some(launch_template_configs);
        }
        let charged_after = spot_fleet_request_payload_bytes(&config, &tags);

        // "Amazon EC2 calculates the difference between the total capacity and On-Demand
        // capacity, and launches the difference as Spot capacity" (`RequestSpotFleet`), so what
        // the fleet's Spot Instance requests cover is the target less the On-Demand target.
        let spot_target = f64::from(
            config
                .target_capacity
                .saturating_sub(config.on_demand_target_capacity.unwrap_or(0)),
        );
        let terminate_excess = spot_fleet_terminates_excess_capacity(
            config.excess_capacity_termination_policy.as_deref(),
        );
        let at = SystemTime::now();

        let Some(record) = self.spot_fleet_requests.get_mut(fleet_id) else {
            return false;
        };
        record.view.spot_fleet_request_config = Some(config.clone());
        if change.pools.is_some() {
            record.pools = pools.clone();
        }
        record.begin_modification(at);
        // The configuration a modification stores is caller-chosen text like the one the create
        // stored, so what it ADDS to that charge is charged here; a modification that shortens
        // it refunds nothing, which is the same conservative reading every other writer of this
        // store takes.
        self.add_stored_bytes(charged_after.saturating_sub(charged_before));

        if spot_target > held {
            self.place_spot_fleet_scale_up(
                fleet_id,
                &config,
                &pools,
                spot_target - held,
                &region,
                spot_request_type,
                interruption_behavior,
            );
        } else if spot_target < held {
            self.release_spot_fleet_capacity(fleet_id, spot_target, terminate_excess, at);
        }

        // Read the fulfilled capacity and the activity status back through the one derivation of
        // them, so the modification leaves the fleet reporting what a describe in the same
        // instant reports.
        self.materialize_due_spot_fleet_requests(Instant::now());
        true
    }

    /// Place the additional Spot Instance requests a Spot Fleet needs to cover `extra` more
    /// capacity units, in the pools it places capacity in.
    ///
    /// "To scale up your Spot Fleet, increase its target capacity. The Spot Fleet launches the
    /// additional Spot Instances according to the allocation strategy for the Spot Fleet
    /// request" (`ModifySpotFleetRequest`) — the same arithmetic and the same writer the create
    /// places its own capacity through ([`Self::place_spot_fleet_capacity`]), so a fleet that
    /// was created at a capacity and one that was scaled up to it hold the same requests.
    ///
    /// Bounded by what this store can still hold: a fleet whose new target capacity needs more
    /// instances than that places what it can and keeps reporting its capacity as not yet
    /// fulfilled, which is what AWS reports for a fleet below its target.
    #[allow(clippy::too_many_arguments)]
    fn place_spot_fleet_scale_up(
        &mut self,
        fleet_id: &str,
        config: &SpotFleetRequestConfigData,
        pools: &[SpotFleetPoolPlacement],
        extra: f64,
        region: &str,
        spot_request_type: &'static str,
        interruption_behavior: &'static str,
    ) {
        let held_placements = self
            .spot_fleet_requests
            .get(fleet_id)
            .map_or(0, |record| {
                record
                    .placements
                    .iter()
                    .filter(|placement| {
                        spot_fleet_placement_is_held(
                            placement,
                            &self.spot_instance_requests,
                            &self.instances,
                        )
                    })
                    .count()
            });
        // The caller-chosen bytes one more request of the largest pool would retain, by the one
        // definition of that charge, so the byte ceiling bounds this placement as it bounds a
        // create's.
        let per_request = pools
            .iter()
            .map(|pool| {
                let probe = SpotInstanceRequest {
                    launch_specification: Some(pool.launch.clone()),
                    spot_price: pool.spot_price.clone(),
                    ..SpotInstanceRequest::default()
                };
                spot_instance_request_payload_bytes(&probe, None)
            })
            .max()
            .unwrap_or(0);
        let byte_room = match per_request {
            0 => MAX_SPOT_FLEET_SPOT_INSTANCES,
            bytes => MAX_STORED_BYTES.saturating_sub(self.stored_bytes()) / bytes,
        };
        let room = MAX_SPOT_FLEET_SPOT_INSTANCES
            .saturating_sub(held_placements)
            .min(MAX_STORED_SPOT_INSTANCE_REQUESTS.saturating_sub(self.spot_instance_requests.len()))
            .min(MAX_STORED_INSTANCES.saturating_sub(self.instances.len()))
            .min(byte_room);
        if room == 0 || pools.is_empty() {
            return;
        }
        let weights: Vec<f64> = pools.iter().map(|pool| pool.weighted_capacity).collect();
        let counts = spot_fleet_placement_counts(
            &weights,
            extra,
            config.allocation_strategy.as_deref()
                == Some(SPOT_FLEET_ALLOCATION_STRATEGY_DIVERSIFIED),
            room,
        );
        let mut placing: Vec<SpotFleetPoolPlacement> = pools.to_vec();
        for (pool, count) in placing.iter_mut().zip(&counts) {
            pool.count = *count;
        }
        if placing.iter().all(|pool| pool.count == 0) {
            return;
        }
        let placed = self.place_spot_fleet_capacity(&SpotFleetRequestSpec {
            region: region.to_string(),
            config: config.clone(),
            // The fleet's own tags were assigned when it was created and are not re-applied by
            // a modification; nothing tags the requests this places either.
            tags: Vec::new(),
            pools: placing,
            spot_request_type,
            interruption_behavior,
        });
        if let Some(record) = self.spot_fleet_requests.get_mut(fleet_id) {
            record.placements.extend(placed);
        }
    }

    /// Give up the capacity a Spot Fleet holds above `spot_target` units.
    ///
    /// "First, the Spot Fleet cancels any open requests that exceed the new target capacity" —
    /// so the requests that have launched nothing go first, and only then, when
    /// `terminate_excess` says so, does the fleet "terminate Spot Instances until the size of
    /// the fleet no longer exceeds the new target capacity"; otherwise it "keeps the fleet at
    /// its current size, but does not replace any Spot Instances that are interrupted"
    /// (`ModifySpotFleetRequest`), so its running instances are left alone.
    ///
    /// Within each of those two passes the fleet gives up the capacity it placed LAST first:
    /// "if the allocation strategy is `lowestPrice`, the Spot Fleet terminates the instances
    /// with the highest price per unit", which this emulator holds no Spot price history to tell
    /// apart, so the order is the reverse of the one the capacity was placed in — the same order
    /// every other member of the configuration is read in.
    fn release_spot_fleet_capacity(
        &mut self,
        fleet_id: &str,
        spot_target: f64,
        terminate_excess: bool,
        at: SystemTime,
    ) {
        let Some(record) = self.spot_fleet_requests.get(fleet_id) else {
            return;
        };
        let placements = record.placements.clone();
        let mut held = spot_fleet_held_capacity(
            &placements,
            &self.spot_instance_requests,
            &self.instances,
        );
        for placement in placements.iter().rev() {
            if held <= spot_target {
                break;
            }
            if !spot_fleet_placement_is_held(
                placement,
                &self.spot_instance_requests,
                &self.instances,
            ) {
                continue;
            }
            let launched = self
                .spot_instance_requests
                .get(&placement.spot_instance_request_id)
                .is_some_and(|request| request.view.instance_id.is_some());
            if launched {
                continue;
            }
            // Cancelled through the ONE writer of a Spot Instance request's cancellation, so a
            // request the fleet gave up and one a caller cancelled end the same way — and the
            // fleet launches nothing more for it.
            self.cancel_spot_instance_request(&placement.spot_instance_request_id, false, at);
            held -= placement.weighted_capacity;
        }
        if !terminate_excess {
            return;
        }
        for placement in placements.iter().rev() {
            if held <= spot_target {
                break;
            }
            if !spot_fleet_placement_is_held(
                placement,
                &self.spot_instance_requests,
                &self.instances,
            ) {
                continue;
            }
            let Some(instance_id) = self
                .spot_instance_requests
                .get(&placement.spot_instance_request_id)
                .and_then(|request| request.view.instance_id.clone())
            else {
                continue;
            };
            // "An instance termination notification was sent when a Spot Instance was
            // terminated by Amazon EC2 during scale-down, when the target capacity of the fleet
            // was modified down" — one event of the fleet's history, at the instant the
            // scale-down was asked for.
            self.record_spot_fleet_history_event(
                fleet_id,
                SpotFleetHistoryEvent::instance_change(
                    EVENT_SUB_TYPE_TERMINATION_NOTIFIED,
                    &instance_id,
                    at,
                ),
            );
            self.cancel_spot_instance_request(&placement.spot_instance_request_id, true, at);
            self.begin_instance_termination(&instance_id, at);
            held -= placement.weighted_capacity;
        }
    }
}
