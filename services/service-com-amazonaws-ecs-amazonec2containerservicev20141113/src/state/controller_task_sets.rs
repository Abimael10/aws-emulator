// SPDX-License-Identifier: CC0-1.0

//! The task sets of a service whose deployments a controller OTHER than the ECS scheduler
//! drives: the one Amazon ECS creates for it itself, and the one a controller adds later.
//!
//! # The rule this module implements
//!
//! `CreateService` "Runs and maintains your desired number of tasks from a specified task
//! definition", and only ONE deployment controller takes that work off Amazon ECS: "When
//! creating a service that uses the `EXTERNAL` deployment controller, you can specify only
//! parameters that aren't controlled at the task set level. The only required parameter is
//! the service name. You control your services using the `CreateTaskSet`". The CodeDeploy
//! controller is described the other way round — "CodeDeploy installs an updated version of
//! the application as a new replacement task set and reroutes production traffic from the
//! ORIGINAL application task set to the replacement task set" — so the original task set is
//! one Amazon ECS has, and a client that creates such a service and polls it watches
//! `runningCount` reach `desiredCount` without creating anything else.
//!
//! So the task set a CodeDeploy-controlled service starts with is minted by the create
//! itself, out of the service's OWN configuration — its task definition, its launch
//! selection, its networking and the `loadBalancers` binding such a service is required to
//! carry — and it is `PRIMARY`, because `TaskSet$status` PRIMARY is "The task set is serving
//! production traffic" and the original application task set is what serves it. An
//! `EXTERNAL`-controlled service is left with none: its task sets are the ones its caller
//! creates.
//!
//! # One shape for a controller's task set
//!
//! The create's own task set and the one a controller adds to the service afterwards are both
//! minted here ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::add_controller_task_set`]),
//! so the task set Amazon ECS makes for a service and the one a deployment controller asks it
//! for differ only in what was asked for: the same record shape, the same `scale` (the whole
//! of the service's count, which is what a controller's task set is placed for), the same
//! steady-state confirmation the clock owes it ([`TRANSITION_SPAN`]) and the same scheduler
//! pass placing its tasks
//! ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::scale_task_set`]).
//!
//! `CreateTaskSet` mints a CALLER's own task set rather than a controller's, and keeps its own
//! record of what that caller asked for — the `scale` the request carried, its `clientToken`
//! and its tags, and the launch selection resolved against the cluster's default strategy —
//! so it builds its own record beside this one; what the two share is the scheduler pass and
//! the scale/stability/placement rules each is held to.

use super::*;

/// What a deployment controller asks Amazon ECS for when it adds a task set to a service.
///
/// Every member is the one the `TaskSet` shape carries for it, so the record this builds is
/// the one `CreateTaskSet` would have built for the same request.
#[derive(Debug, Clone)]
pub struct RequestedTaskSet {
    /// The task definition the task set's tasks run, as a reference — an ARN, a
    /// `family:revision` or a family, which "If a revision isn't specified, the latest
    /// ACTIVE revision is used".
    pub task_definition: String,
    /// `TaskSet$status`: [`TASK_SET_PRIMARY`] for a task set that serves production
    /// traffic, [`TASK_SET_ACTIVE`] for one that does not.
    pub status: String,
    /// The target groups and container ports the task set's tasks are registered into.
    pub load_balancers: Vec<LoadBalancer>,
    /// The networking its tasks are placed with, taken from the service when the controller
    /// states none.
    pub network_configuration: Option<NetworkConfiguration>,
    /// The launch type its tasks are placed under; see [`Self::network_configuration`].
    pub launch_type: Option<String>,
    /// The capacity providers its tasks are placed through; see
    /// [`Self::network_configuration`].
    pub capacity_provider_strategy: Vec<CapacityProviderStrategyItem>,
    /// The platform version its tasks run; see [`Self::network_configuration`].
    pub platform_version: Option<String>,
    /// `TaskSet$externalId`, which identifies the task set to whatever created it.
    pub external_id: Option<String>,
}

impl RequestedTaskSet {
    /// The task set a CodeDeploy-controlled service's own create asks for: the service's
    /// configuration, serving production traffic.
    ///
    /// Nothing is invented — the revision, the binding, the networking and the launch
    /// selection are the ones the stored service carries — so the original application task
    /// set runs exactly what the service was created to run.
    fn of_service(service: &ServiceRecord) -> Option<Self> {
        Some(Self {
            task_definition: service.task_definition_arn.clone()?,
            status: TASK_SET_PRIMARY.to_string(),
            load_balancers: service.load_balancers.clone(),
            network_configuration: service.network_configuration.clone(),
            launch_type: service.launch_type.clone(),
            capacity_provider_strategy: service.capacity_provider_strategy.clone(),
            platform_version: service.platform_version.clone(),
            external_id: None,
        })
    }
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// Create the task set a service of the CodeDeploy controller starts with, and place its
    /// tasks.
    ///
    /// Called by `CreateService` beside the scheduler's own pass, and a no-op for every
    /// other controller: the ECS scheduler places a service's tasks itself
    /// ([`ServiceRecord::places_tasks`]), and an `EXTERNAL`-controlled service's task sets
    /// are its caller's to create.
    pub fn place_original_task_set(
        &mut self,
        key: &(String, String),
        region: &str,
        now: SystemTime,
    ) -> Result<(), TaskCapacityExceeded> {
        let Some(service) = self.services.get(key) else {
            return Ok(());
        };
        if service.controller() != DEPLOYMENT_CONTROLLER_CODE_DEPLOY {
            return Ok(());
        }
        // Only a service's FIRST task set is Amazon ECS's own: one that already holds a task
        // set is one a controller is already deploying through.
        if !self.service_task_sets(&key.0, &key.1).is_empty() {
            return Ok(());
        }
        let Some(requested) = RequestedTaskSet::of_service(service) else {
            return Ok(());
        };
        self.add_controller_task_set(key, &requested, region, now)?;
        Ok(())
    }

    /// Add the task set `requested` to the service `key` names, place its tasks, and answer
    /// its id.
    ///
    /// The record is built as `CreateTaskSet` builds a caller's own — the same ARN form, the
    /// same steady-state confirmation owed by the clock and the same scheduler pass placing
    /// its tasks — with the `scale` a task set placed for the whole of the service takes,
    /// which is what `TaskSet$computedDesiredCount` multiplies.
    ///
    /// `None` when there is nothing to create a task set from: no such service, a service
    /// the ECS scheduler places the tasks of, one that is not active, one already holding as
    /// many task sets as a service may hold ([`MAX_TASK_SETS_PER_SERVICE`]), or a task
    /// definition reference this store holds no usable revision for. `Err` is
    /// [`Self::scale_task_set`]'s: the placement would push this emulator past
    /// [`MAX_TASKS`], and the task set is not kept.
    pub fn add_controller_task_set(
        &mut self,
        key: &(String, String),
        requested: &RequestedTaskSet,
        region: &str,
        now: SystemTime,
    ) -> Result<Option<String>, TaskCapacityExceeded> {
        let Some(service) = self.services.get(key).cloned() else {
            return Ok(None);
        };
        if service.places_tasks() || !service.is_active() {
            return Ok(None);
        }
        if self.service_task_sets(&key.0, &key.1).len() >= MAX_TASK_SETS_PER_SERVICE {
            return Ok(None);
        }
        let Some((family, revision)) = parse_task_definition_reference(&requested.task_definition)
        else {
            return Ok(None);
        };
        let Ok(definition) = self.usable_revision(region, &family, revision).cloned() else {
            return Ok(None);
        };
        let created_at = reported_instant(now);
        let id = self.next_task_set_id();
        let record = TaskSetRecord {
            arn: task_set_arn(region, &key.0, &key.1, &id),
            id,
            cluster_name: key.0.clone(),
            cluster_arn: service.cluster_arn.clone(),
            service_name: key.1.clone(),
            // The task set Amazon ECS mints for a service of its own accord names that
            // service by the service's OWN ARN, cluster segment and all, while a task set a
            // deployment controller created names it by the cluster-less form
            // ([`task_set_service_arn`]). `TaskSet$serviceArn` is documented as nothing more
            // than "The Amazon Resource Name (ARN) of the service the task set exists in",
            // so which form each carries is recorded behavior — which is why the ARN a task
            // set reports is STORED on its own record rather than derived when it is read.
            service_arn: service.arn.clone(),
            external_id: requested.external_id.clone(),
            status: requested.status.clone(),
            task_definition_arn: definition.arn.clone(),
            task_definition: (
                region.to_string(),
                definition.family.clone(),
                definition.revision,
            ),
            // The launch selection, the platform version and the networking the controller
            // asked for, and the SERVICE's own where it asked for none: a replacement task
            // set runs where the service runs unless it was told otherwise.
            launch_type: requested
                .launch_type
                .clone()
                .or_else(|| service.launch_type.clone()),
            capacity_provider_strategy: if requested.capacity_provider_strategy.is_empty() {
                service.capacity_provider_strategy.clone()
            } else {
                requested.capacity_provider_strategy.clone()
            },
            platform_version: requested
                .platform_version
                .clone()
                .or_else(|| service.platform_version.clone()),
            network_configuration: defaulted_network_configuration(
                requested
                    .network_configuration
                    .as_ref()
                    .or(service.network_configuration.as_ref()),
            ),
            load_balancers: requested.load_balancers.clone(),
            service_registries: service.service_registries.clone(),
            // "A floating-point percentage of your desired number of tasks to place and keep
            // running in the task set": a task set placed for the whole of the service takes
            // the whole of that count.
            scale: stored_scale(Some(&Scale {
                value: MAX_SCALE_PERCENT,
                unit: Some(SCALE_UNIT_PERCENT.to_string()),
            })),
            tags: Vec::new(),
            client_token: None,
            // A task set that has just been placed has not converged on what it wants: the
            // scheduler confirms its steady state a span later.
            stable_due_at: Some(now + TRANSITION_SPAN),
            removal_due_at: None,
            created_at,
            updated_at: created_at,
        };
        let task_set_key = record.key();
        let id = record.id.clone();
        self.task_sets.insert(task_set_key.clone(), record);
        // The service now HAS a deployment of its own, which deleting its task sets is what
        // takes away again.
        if let Some(stored) = self.services.get_mut(key) {
            stored.held_task_set = true;
        }
        if let Err(exceeded) = self.scale_task_set(&task_set_key, &definition, region, now) {
            self.task_sets.remove(&task_set_key);
            return Err(exceeded);
        }
        Ok(Some(id))
    }

    /// Keep every task set of every service at the count it wants, as of `now`.
    ///
    /// The scheduler's steady-state work for a service whose tasks come from a task set,
    /// exactly as [`Self::maintain_service_task_counts`] is for one whose tasks come from
    /// the service itself: `TaskSet$computedDesiredCount` "is calculated by multiplying the
    /// service's `desiredCount` by the task set's `scale` percentage", so a task set is
    /// short of what it wants whenever a task of it stopped or the service's desired count
    /// moved — and placing that shortfall was owed from the instant it arose, whether or not
    /// anybody called.
    ///
    /// A task set being drained away by a `DeleteTaskSet` is left alone: its tasks "are being
    /// stopped, and their corresponding targets are being deregistered from their target
    /// group", so nothing is placed back into it.
    pub(super) fn maintain_task_set_counts(&mut self, now: SystemTime) {
        let keys: Vec<(String, String, String)> = self
            .task_sets
            .values()
            .filter(|task_set| !task_set.is_deleted() && task_set.status != TASK_SET_DRAINING)
            .map(TaskSetRecord::key)
            .collect();
        for key in keys {
            let active = self
                .services
                .get(&(key.0.clone(), key.1.clone()))
                .is_some_and(ServiceRecord::is_active);
            if !active {
                continue;
            }
            let Some(revision) = self.task_sets.get(&key).map(|set| set.task_definition.clone())
            else {
                continue;
            };
            let region = revision.0.clone();
            let Some(definition) = self.task_definitions.get(&revision).cloned() else {
                continue;
            };
            let _ = self.scale_task_set(&key, &definition, &region, now);
        }
    }
}
