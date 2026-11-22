// SPDX-License-Identifier: CC0-1.0

//! One cluster's instance GROUPS: the lifecycle of a persisted group and the wire shape
//! `ListInstanceGroups` and the cluster reads project from it.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and reached
//! from it like every other primitive there.

use super::*;

impl InstanceGroupRecord {
    /// Persist a submitted `InstanceGroupConfig` under `id`.
    ///
    /// A group EMR has just accepted is `PROVISIONING` (`InstanceGroupState`) — its
    /// Amazon EC2 instances are not up yet — and the clock owes it the move to
    /// `RUNNING`. A policy the configuration carries is `ATTACHING` for a span of its
    /// own.
    pub fn from_config(id: String, config: InstanceGroupConfig, now: SystemTime) -> Self {
        let policy_attaching_due_at = config
            .auto_scaling_policy
            .as_ref()
            .map(|_| now + TRANSITION_SPAN);
        Self {
            id,
            config,
            state: instance_group_state::PROVISIONING.to_string(),
            state_change_reason: None,
            creation_date_time: now,
            ready_date_time: None,
            end_date_time: None,
            ready_due_at: Some(now + TRANSITION_SPAN),
            policy_attaching_due_at,
            change_due_at: None,
            resizing_from_count: None,
            shrink_policy: None,
            configurations_version: None,
            pending_configurations_version: None,
            last_successfully_applied_configurations: None,
            last_successfully_applied_configurations_version: None,
        }
    }

    /// Start the resize to `target` instances a modification asked for.
    ///
    /// "ModifyInstanceGroups modifies the number of nodes … The input parameters include
    /// the new target instance count for the group" (`ModifyInstanceGroups` doc, Smithy
    /// model): the target is what the group requests from here on, which is what a later
    /// read reports as its `RequestedInstanceCount`. A group whose instances are already
    /// up is `RESIZING` until the clock makes the new count due, and goes on reporting the
    /// instances it already runs while it is; one still provisioning stays
    /// `PROVISIONING` — it is yet to acquire anything, and what it acquires is the new
    /// target.
    pub fn begin_resize(&mut self, target: i32, now: SystemTime) {
        if instance_group_state::UP.contains(&self.state.as_str()) {
            self.resizing_from_count = Some(self.running_instance_count());
            self.state = instance_group_state::RESIZING.to_string();
            self.change_due_at = Some(now + TRANSITION_SPAN);
        }
        self.config.instance_count = target;
    }

    /// Start the reconfiguration a modification asked for, merging `configurations` into
    /// the ones this group holds when `merge` is set and replacing them otherwise.
    ///
    /// "Type of reconfiguration requested. Valid values are MERGE and OVERWRITE."
    /// (`InstanceGroupModifyConfig$ReconfigurationType` doc.) A merge keeps every
    /// classification the group holds that the request does not name again, in the order
    /// it held them, so what the group requests is the two specifications merged; an
    /// overwrite is the submitted specification alone. Either way the requested
    /// specification moves to a new version, and that version only becomes the last
    /// SUCCESSFULLY APPLIED one when the clock makes the reconfiguration applied.
    pub fn begin_reconfiguration(
        &mut self,
        configurations: Vec<Configuration>,
        merge: bool,
        now: SystemTime,
    ) {
        let requested = if merge {
            let mut merged = self.config.configurations.clone().unwrap_or_default();
            for configuration in configurations {
                match merged
                    .iter_mut()
                    .find(|held| held.classification == configuration.classification)
                {
                    Some(held) => *held = configuration,
                    None => merged.push(configuration),
                }
            }
            merged
        } else {
            configurations
        };
        self.config.configurations = Some(requested);
        let version = self.configurations_version.unwrap_or(0).saturating_add(1);
        self.configurations_version = Some(version);
        self.pending_configurations_version = Some(version);
        if instance_group_state::UP.contains(&self.state.as_str())
            && self.state != instance_group_state::RESIZING
        {
            self.state = instance_group_state::RECONFIGURING.to_string();
        }
        self.change_due_at = Some(now + TRANSITION_SPAN);
    }

    /// Attach `policy` to this group, replacing whatever policy it carried before.
    ///
    /// "Creates or updates an automatic scaling policy for a core instance group or task
    /// instance group" (`PutAutoScalingPolicy` doc, Smithy model): the submitted policy
    /// is what the group carries from here on. A policy EMR has just accepted is still
    /// `ATTACHING` (`AutoScalingPolicyState`), and the clock owes it the move to
    /// `ATTACHED`.
    pub fn attach_auto_scaling_policy(&mut self, policy: AutoScalingPolicy, now: SystemTime) {
        self.config.auto_scaling_policy = Some(policy);
        self.policy_attaching_due_at = Some(now + TRANSITION_SPAN);
    }

    /// Take the automatic scaling policy off this group, reporting whether it carried
    /// one.
    ///
    /// "Removes an automatic scaling policy from a specified instance group"
    /// (`RemoveAutoScalingPolicy` doc, Smithy model): the group carries no policy from
    /// here on, so the attach the clock may still have owed it goes with it. `false` says
    /// the group carried no policy, which is what leaves the caller nothing to remove —
    /// and in that case the record is left exactly as it was, so a refused call writes
    /// nothing at all.
    pub fn remove_auto_scaling_policy(&mut self) -> bool {
        if self.config.auto_scaling_policy.take().is_none() {
            return false;
        }
        self.policy_attaching_due_at = None;
        true
    }

    /// Materialise the transitions the clock already made due, and nothing else: a group
    /// that was `PROVISIONING` is `RUNNING` once its span has passed, a group that was
    /// `RESIZING` or `RECONFIGURING` is `RUNNING` once the modification's span has (with
    /// the configurations it requested now the ones last successfully applied), and a
    /// policy that was `ATTACHING` is `ATTACHED` once its own has — each owed at that
    /// instant whether or not anybody looked.
    pub fn advance_to(&mut self, now: SystemTime) {
        if let Some(due) = self.policy_attaching_due_at {
            if now >= due {
                self.policy_attaching_due_at = None;
            }
        }
        if let Some(due) = self.change_due_at {
            if now >= due {
                self.change_due_at = None;
                self.resizing_from_count = None;
                if let Some(version) = self.pending_configurations_version.take() {
                    self.last_successfully_applied_configurations =
                        self.config.configurations.clone();
                    self.last_successfully_applied_configurations_version = Some(version);
                }
                if self.state == instance_group_state::RESIZING
                    || self.state == instance_group_state::RECONFIGURING
                {
                    self.state = instance_group_state::RUNNING.to_string();
                }
            }
        }
        if self.state != instance_group_state::PROVISIONING {
            return;
        }
        if let Some(due) = self.ready_due_at {
            if now >= due {
                self.state = instance_group_state::RUNNING.to_string();
                self.ready_date_time = Some(due);
                self.ready_due_at = None;
            }
        }
    }

    /// End this group at `at`, because its cluster reached `TERMINATED`.
    pub fn end_at(&mut self, at: SystemTime) {
        if self.state == instance_group_state::ENDED {
            return;
        }
        self.state = instance_group_state::ENDED.to_string();
        self.state_change_reason = Some(StateChangeReasonRecord {
            code: instance_group_state_change_reason_code::CLUSTER_TERMINATED.to_string(),
            message: Some("The cluster was terminated.".to_string()),
        });
        self.ready_due_at = None;
        self.policy_attaching_due_at = None;
        self.change_due_at = None;
        self.resizing_from_count = None;
        self.end_date_time = Some(at);
    }

    /// "The marketplace to provision instances for this group. Valid values are
    /// ON_DEMAND or SPOT." (`InstanceGroup$Market` doc.) A configuration that names no
    /// market provisions On-Demand instances, which is Amazon EMR's default market.
    pub(super) fn market(&self) -> &str {
        self.config
            .market
            .as_deref()
            .filter(|market| !market.is_empty())
            .unwrap_or(market_type::ON_DEMAND)
    }

    /// "The number of instances currently running in this instance group."
    /// (`InstanceGroup$RunningInstanceCount` doc.) None are running while the group is
    /// still provisioning them or once it has ended; a group that is up runs the target
    /// number it requested, and one part way through a resize goes on running the count
    /// it was already up at until the new target comes due.
    pub(super) fn running_instance_count(&self) -> i32 {
        if !instance_group_state::UP.contains(&self.state.as_str()) {
            return 0;
        }
        self.resizing_from_count
            .unwrap_or(self.config.instance_count)
            .max(0)
    }

    /// The wire `AutoScalingPolicyDescription` for the automatic scaling policy this
    /// group carries — the one its configuration was given, or the one the last
    /// `PutAutoScalingPolicy` attached — and `None` when it carries none.
    ///
    /// The constraints and rules are the ones that were submitted. The status is where
    /// the clock has got to: a policy whose attach span has not passed is `ATTACHING`,
    /// and one past it is `ATTACHED` (`AutoScalingPolicyState`).
    pub fn to_auto_scaling_policy(&self) -> Option<AutoScalingPolicyDescription> {
        let policy = self.config.auto_scaling_policy.as_ref()?;
        let state = if self.policy_attaching_due_at.is_some() {
            auto_scaling_policy_state::ATTACHING
        } else {
            auto_scaling_policy_state::ATTACHED
        };
        Some(AutoScalingPolicyDescription {
            status: Some(AutoScalingPolicyStatus {
                state: Some(state.to_string()),
                // Nothing moved this policy's state by request or by failure, so there
                // is no reason to report beside it.
                state_change_reason: None,
            }),
            constraints: Some(policy.constraints.clone()),
            rules: optional_list(&policy.rules),
        })
    }

    /// The wire `InstanceGroupStatus` for this record.
    pub fn to_status(&self) -> InstanceGroupStatus {
        InstanceGroupStatus {
            state: Some(self.state.clone()),
            state_change_reason: self.state_change_reason.as_ref().map(|reason| {
                InstanceGroupStateChangeReason {
                    code: Some(reason.code.clone()),
                    message: reason.message.clone(),
                }
            }),
            timeline: Some(InstanceGroupTimeline {
                creation_date_time: Some(timestamp(self.creation_date_time)),
                ready_date_time: self.ready_date_time.map(timestamp),
                end_date_time: self.end_date_time.map(timestamp),
            }),
        }
    }

    /// The wire `InstanceGroup` for this record — what `ListInstanceGroups` reports.
    pub fn to_instance_group(&self) -> InstanceGroup {
        InstanceGroup {
            id: Some(self.id.clone()),
            name: self.config.name.clone(),
            market: Some(self.market().to_string()),
            // "The role of the instance group in the cluster"
            // (`InstanceGroupConfig$InstanceRole`) IS the group's type: both carry
            // MASTER, CORE or TASK.
            instance_group_type: Some(self.config.instance_role.clone()),
            bid_price: self.config.bid_price.clone(),
            instance_type: Some(self.config.instance_type.clone()),
            requested_instance_count: Some(self.config.instance_count),
            running_instance_count: Some(self.running_instance_count()),
            status: Some(self.to_status()),
            // Served with the property map every configuration carries; an empty list is
            // left absent for the reason [`instance_type_specification`] states.
            configurations: self
                .config
                .configurations
                .as_deref()
                .map(served_configurations)
                .filter(|configurations| !configurations.is_empty()),
            // The configuration versions are what a RECONFIGURATION moves ("the version
            // number of the requested configuration specification"), so a group nothing
            // has reconfigured carries none, and the ones last SUCCESSFULLY APPLIED are
            // the specification of the last reconfiguration the clock has made applied —
            // not the one still in flight.
            configurations_version: self.configurations_version,
            last_successfully_applied_configurations: self
                .last_successfully_applied_configurations
                .as_deref()
                .map(served_configurations)
                .filter(|configurations| !configurations.is_empty()),
            last_successfully_applied_configurations_version: self
                .last_successfully_applied_configurations_version,
            ebs_block_devices: optional_list(&ebs_block_devices(
                self.config.ebs_configuration.as_ref(),
            )),
            ebs_optimized: self
                .config
                .ebs_configuration
                .as_ref()
                .and_then(|configuration| configuration.ebs_optimized),
            // "Policy for customizing shrink operations" — a shrink policy arrives with a
            // `ModifyInstanceGroups` resize, so a group nobody has resized carries none.
            shrink_policy: self.shrink_policy.clone(),
            auto_scaling_policy: self.to_auto_scaling_policy(),
            custom_ami_id: self.config.custom_ami_id.clone(),
        }
    }
}
