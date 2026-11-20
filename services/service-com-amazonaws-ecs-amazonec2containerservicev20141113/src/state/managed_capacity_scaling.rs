// SPDX-License-Identifier: CC0-1.0

//! ECS MANAGED SCALING: the capacity Amazon ECS itself acquires for a capacity provider
//! whose Auto Scaling group it manages the scaling of.
//!
//! # The rule this module implements
//!
//! `ManagedScaling`'s own documentation: "When managed scaling is turned on, Amazon ECS
//! manages the scale-in and scale-out actions of the Auto Scaling group"; "If managed
//! scaling is off, the user must manage the scaling of the Auto Scaling group". So a cluster
//! that holds tasks it cannot place — tasks routed through such a provider, parked while the
//! capacity to run them is acquired ([`awaits_provisioned_capacity`]) — is a cluster Amazon
//! ECS answers by ADDING capacity: it raises the group's desired capacity, the group launches
//! the instances, their agents register them into the cluster
//! ([`super::ec2_capacity::settle_ec2_capacity`]) and the parked tasks are placed from there
//! ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::place_tasks_awaiting_capacity`]).
//!
//! HOW FAR it raises the capacity is the provider's own configuration:
//! `ManagedScaling$minimumScalingStepSize` is "the minimum number of Amazon EC2 instances
//! that Amazon ECS will scale out at one time" and `maximumScalingStepSize` "the maximum
//! number ... that Amazon ECS will scale out at one time", so the step is the shortfall
//! brought inside those two bounds; the group's own `MaxSize` is the ceiling over everything,
//! because an Auto Scaling group never holds a capacity outside its own bounds. A group whose
//! capacity already covers the step is left exactly as it is, so one shortfall is answered
//! once however many passes look at it.
//!
//! What is NOT done here is the scale-IN half: a group this emulator never raised holds the
//! capacity its owner gave it, and the tasks a cluster stops say nothing about which of its
//! instances their owner wants kept.
//!
//! # Why the clock owes it
//!
//! The scaling action is Amazon ECS's own, taken with no call from anybody: a client that
//! creates a service through a managed-scaling provider makes no further request and waits
//! for the capacity to appear. So the action is owed by the CLOCK — recorded as due one
//! [`MANAGED_SCALING_SPAN`] after the instant the cluster first held a task it could not
//! place ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::note_managed_capacity_demand`],
//! run by the clock pass) and taken by the pass that materialises what is due
//! ([`settle_managed_capacity_scaling`], run by every operation's prologue and by the clock
//! pump, [`advance_task_clock`]). The span is never zero, so a caller reading straight after
//! the create still sees the parked task on a group at the capacity its owner left it at.
//!
//! LOCK ORDER: the demand is read out of this service's store and the store's lock RELEASED
//! before the Auto Scaling store's is taken, and nothing of this service is reached while
//! that one is held — the same contract [`auto_scaling_group_binding`] has, which is what
//! keeps a lock cycle between the two stores unreachable.

use super::*;

use ::service_com_amazonaws_autoscaling_autoscaling_2011_01_01::state as auto_scaling;

/// How long after a cluster first holds a task it cannot place Amazon ECS's managed scaling
/// has acted on the capacity provider's group.
///
/// EMULATOR-CHOSEN — AWS publishes no duration for the reaction, which on the real service is
/// a CloudWatch metric's alarm period — and deliberately non-zero, so a caller that reads the
/// group straight after the placement still sees the capacity its owner left it at.
pub const MANAGED_SCALING_SPAN: Duration = Duration::from_secs(3);

/// The reason an Auto Scaling capacity change this pass makes records, which the group's own
/// scaling activity reports as its `Cause`.
fn managed_scaling_reason(provider_name: &str) -> String {
    format!(
        "a scaling activity was initiated by Amazon ECS managed scaling of capacity provider \
         {provider_name}"
    )
}

/// What Amazon ECS managed scaling owes ONE `(cluster, capacity provider)` pair: how much
/// capacity the cluster is short of, when the action falls due, and what it was last taken
/// for.
#[derive(Debug, Clone)]
pub struct ManagedCapacityDemand {
    /// How many tasks of the cluster are waiting for this provider's capacity.
    pub waiting_tasks: i32,
    /// The instant the scaling action falls due at, which is the instant it happens at.
    pub due_at: SystemTime,
    /// The `waiting_tasks` the last action was taken for, and `None` while none has been:
    /// what keeps one shortfall answered once however many passes look at it.
    pub acted_for: Option<i32>,
}

/// One scaling action this pass is to take: the group to raise, and the numbers that decide
/// how far ([`ManagedCapacityDemand`] and the provider's own managed-scaling block).
#[derive(Debug, Clone)]
pub struct ManagedCapacityScaleOut {
    /// The cluster whose tasks are waiting, and the provider they are routed through: the key
    /// the demand is recorded under.
    pub cluster_name: String,
    pub provider_name: String,
    /// The Auto Scaling group the provider binds onto, by NAME — the key that service holds
    /// its groups under ([`auto_scaling_group_key`]).
    pub group_name: String,
    /// How many tasks are waiting for the capacity.
    pub waiting_tasks: i32,
    /// `ManagedScaling$minimumScalingStepSize` and `maximumScalingStepSize` of the provider,
    /// which bound one scale-out.
    pub minimum_step: i32,
    pub maximum_step: i32,
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// Record what managed scaling is owed as of `now`: for every `(cluster, provider)` pair
    /// whose cluster holds tasks parked on that provider's capacity, the shortfall and the
    /// instant the action falls due.
    ///
    /// Part of the clock pass ([`Self::advance_clock`]), where the parking itself happens: a
    /// pair whose tasks have all been placed (or stopped) owes nothing, so its demand is
    /// dropped — and a pair whose shortfall has GROWN is owed another action, due one span
    /// from the instant it grew.
    pub(super) fn note_managed_capacity_demand(&mut self, now: SystemTime) {
        let waiting = self.tasks_awaiting_managed_capacity();
        self.managed_capacity_demands
            .retain(|key, _| waiting.contains_key(key));
        for (key, waiting_tasks) in waiting {
            match self.managed_capacity_demands.get_mut(&key) {
                Some(demand) if demand.waiting_tasks == waiting_tasks => {}
                Some(demand) => {
                    demand.waiting_tasks = waiting_tasks;
                    demand.due_at = now + MANAGED_SCALING_SPAN;
                    demand.acted_for = None;
                }
                None => {
                    self.managed_capacity_demands.insert(
                        key,
                        ManagedCapacityDemand {
                            waiting_tasks,
                            due_at: now + MANAGED_SCALING_SPAN,
                            acted_for: None,
                        },
                    );
                }
            }
        }
    }

    /// How many tasks each `(cluster, provider)` pair holds that are waiting for the
    /// provider's capacity to be acquired: the parked tasks on their way UP
    /// ([`TaskRecord::is_awaiting_placement`]) whose provider is one whose group Amazon ECS
    /// manages the scaling of ([`needs_registered_capacity_now`]).
    fn tasks_awaiting_managed_capacity(&self) -> HashMap<(String, String), i32> {
        let mut waiting: HashMap<(String, String), i32> = HashMap::new();
        for task in self.tasks.values() {
            if task.desired_status != STATUS_RUNNING || !task.is_awaiting_placement() {
                continue;
            }
            let Some(provider_name) = task.capacity_provider_name.as_deref() else {
                continue;
            };
            let Some(provider) = self.capacity_providers.get(provider_name) else {
                continue;
            };
            if !self.scales_its_own_capacity(provider) {
                continue;
            }
            *waiting
                .entry((task.cluster_name.clone(), provider_name.to_string()))
                .or_default() += 1;
        }
        waiting
    }

    /// Whether `provider` is one whose Auto Scaling group Amazon ECS drives the capacity of:
    /// a provider over a group of the caller's with managed scaling turned ON
    /// ([`needs_registered_capacity_now`], the same reading that decides whether a placement
    /// through it waits for capacity at all).
    fn scales_its_own_capacity(&self, provider: &CapacityProviderRecord) -> bool {
        provider.auto_scaling_group_provider.is_some() && !needs_registered_capacity_now(provider)
    }

    /// The earliest instant at which managed scaling owes an action, if any — an instant the
    /// clock pump waits for, exactly as it waits for a task's own transitions.
    ///
    /// A demand the action has already been taken for is owed nothing more, so it keeps no
    /// pump awake: the capacity is coming, and what happens when it arrives is the
    /// registration's own pass.
    pub fn next_managed_capacity_due_at(&self) -> Option<SystemTime> {
        self.managed_capacity_demands
            .values()
            .filter(|demand| demand.acted_for != Some(demand.waiting_tasks))
            .map(|demand| demand.due_at)
            .min()
    }

    /// The scaling actions due at `now`: one per `(cluster, provider)` pair whose demand has
    /// fallen due and whose action has not been taken yet.
    ///
    /// Read under this store's lock and answered as owned values, because the action itself
    /// is taken against the Auto Scaling store (see the module's LOCK ORDER note).
    pub fn due_managed_capacity_scale_outs(
        &self,
        now: SystemTime,
    ) -> Vec<ManagedCapacityScaleOut> {
        self.managed_capacity_demands
            .iter()
            .filter(|(_, demand)| {
                demand.due_at <= now && demand.acted_for != Some(demand.waiting_tasks)
            })
            .filter_map(|((cluster_name, provider_name), demand)| {
                let provider = self.capacity_providers.get(provider_name)?;
                let group = provider.auto_scaling_group_provider.as_ref()?;
                let scaling = group.managed_scaling.as_ref();
                Some(ManagedCapacityScaleOut {
                    cluster_name: cluster_name.clone(),
                    provider_name: provider_name.clone(),
                    group_name: auto_scaling_group_key(&group.auto_scaling_group_arn).to_string(),
                    waiting_tasks: demand.waiting_tasks,
                    minimum_step: scaling
                        .and_then(|scaling| scaling.minimum_scaling_step_size)
                        .unwrap_or(MANAGED_SCALING_DEFAULT_MINIMUM_STEP_SIZE),
                    maximum_step: scaling
                        .and_then(|scaling| scaling.maximum_scaling_step_size)
                        .unwrap_or(MANAGED_SCALING_DEFAULT_MAXIMUM_STEP_SIZE),
                })
            })
            .collect()
    }

    /// Record that the scaling action for each named pair has been taken, so the same
    /// shortfall is not answered twice.
    pub fn record_managed_capacity_actions(&mut self, taken: &[ManagedCapacityScaleOut]) {
        for action in taken {
            let key = (action.cluster_name.clone(), action.provider_name.clone());
            if let Some(demand) = self.managed_capacity_demands.get_mut(&key) {
                demand.acted_for = Some(action.waiting_tasks);
            }
        }
    }
}

/// The capacity a scale-out asks the group for: the capacity it holds now
/// (`desired_capacity`) plus the step the shortfall comes to under the provider's own bounds,
/// held at the group's own `max_size`.
///
/// Answers `None` when the group is already at (or past) that capacity — a group whose owner
/// has given it the capacity already, and one whose maximum leaves no room, are each a group
/// this action changes nothing about.
fn scaled_capacity(
    action: &ManagedCapacityScaleOut,
    desired_capacity: i32,
    max_size: i32,
) -> Option<i32> {
    let minimum_step = action.minimum_step.max(1);
    let maximum_step = action.maximum_step.max(minimum_step);
    let step = action.waiting_tasks.clamp(minimum_step, maximum_step);
    let target = desired_capacity.saturating_add(step).min(max_size);
    (target > desired_capacity).then_some(target)
}

/// Take every managed-scaling action the clock has made due at `now`: raise the desired
/// capacity of each group whose provider's cluster holds tasks it cannot place.
///
/// The capacity change goes through Amazon EC2 Auto Scaling's OWN convergence
/// ([`auto_scaling::ComAmazonawsAutoscalingAutoscaling20110101State::apply_capacity`]), so the
/// instances it launches are the account's own Amazon EC2 instances, each with its scaling
/// activity recorded — exactly as a `SetDesiredCapacity` of the same group would leave it.
///
/// Called with NO guard of this service's store held (see the module's LOCK ORDER note). An
/// engine carrying no Auto Scaling store holds no group to raise, so nothing is owed.
pub fn settle_managed_capacity_scaling<Eng: Send + Sync + 'static>(engine: &Eng, now: SystemTime) {
    let Some(state) = service_state(engine) else {
        return;
    };
    let due = state.lock().due_managed_capacity_scale_outs(now);
    if due.is_empty() {
        return;
    }
    let Some(auto_scaling_state) = auto_scaling::service_state(engine) else {
        return;
    };
    {
        // Under the Auto Scaling store's lock alone: the groups are raised through that
        // service's own convergence, which reaches the Amazon EC2 store and never this one.
        let mut store = auto_scaling_state.lock();
        for action in &due {
            let Some((from, target)) = store.groups.get(&action.group_name).and_then(|group| {
                scaled_capacity(action, group.desired_capacity, group.max_size)
                    .map(|target| (group.desired_capacity, target))
            }) else {
                continue;
            };
            let cause = auto_scaling::ComAmazonawsAutoscalingAutoscaling20110101State::capacity_change_cause(
                now,
                &managed_scaling_reason(&action.provider_name),
                from,
                target,
            );
            store.apply_capacity(engine, &action.group_name, target, &cause, now);
        }
    }
    // Recorded whether or not a group moved: the action has been TAKEN for this shortfall,
    // and a group that is already at the capacity it asks for is not asked again.
    state.lock().record_managed_capacity_actions(&due);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A group as Amazon EC2 Auto Scaling holds it, read as the two numbers the step
    /// calculation takes: the capacity it holds now and the maximum it may hold.
    #[derive(Debug, Clone, Copy)]
    struct Held {
        desired_capacity: i32,
        max_size: i32,
    }

    fn group(desired_capacity: i32, max_size: i32) -> Held {
        Held {
            desired_capacity,
            max_size,
        }
    }

    /// One scale-out of `waiting` tasks under the two step bounds.
    fn action(waiting_tasks: i32, minimum_step: i32, maximum_step: i32) -> ManagedCapacityScaleOut {
        ManagedCapacityScaleOut {
            cluster_name: "a-cluster".to_string(),
            provider_name: "a-provider".to_string(),
            group_name: "a-group".to_string(),
            waiting_tasks,
            minimum_step,
            maximum_step,
        }
    }

    /// One row of the table below: what the shortfall and the bounds are, and the capacity
    /// the scale-out asks for.
    #[derive(Debug, Clone, Copy)]
    struct Case {
        case: &'static str,
        waiting_tasks: i32,
        minimum_step: i32,
        maximum_step: i32,
        held: Held,
        expected: Option<i32>,
    }

    /// The capacity a scale-out asks for: the shortfall, under the provider's own step bounds
    /// and the group's own maximum.
    #[test]
    fn the_scaled_capacity_is_the_shortfall_inside_every_bound() {
        let rows: &[Case] = &[
            Case {
                case: "one task on an empty group: one instance",
                waiting_tasks: 1,
                minimum_step: 1,
                maximum_step: 10000,
                held: group(0, 2),
                expected: Some(1),
            },
            Case {
                case: "three tasks: one instance each",
                waiting_tasks: 3,
                minimum_step: 1,
                maximum_step: 10000,
                held: group(0, 4),
                expected: Some(3),
            },
            Case {
                case: "the minimum step is a floor under one task",
                waiting_tasks: 1,
                minimum_step: 2,
                maximum_step: 10000,
                held: group(0, 4),
                expected: Some(2),
            },
            Case {
                case: "the maximum step is a ceiling over the shortfall",
                waiting_tasks: 5,
                minimum_step: 1,
                maximum_step: 2,
                held: group(0, 9),
                expected: Some(2),
            },
            Case {
                case: "the group's own maximum holds the capacity",
                waiting_tasks: 5,
                minimum_step: 1,
                maximum_step: 10000,
                held: group(0, 2),
                expected: Some(2),
            },
            Case {
                case: "a group already at its maximum is not asked again",
                waiting_tasks: 1,
                minimum_step: 1,
                maximum_step: 10000,
                held: group(2, 2),
                expected: None,
            },
            Case {
                case: "the step is added to the capacity the group already holds",
                waiting_tasks: 2,
                minimum_step: 1,
                maximum_step: 10000,
                held: group(1, 10),
                expected: Some(3),
            },
        ];
        for row in rows {
            let asked = scaled_capacity(
                &action(row.waiting_tasks, row.minimum_step, row.maximum_step),
                row.held.desired_capacity,
                row.held.max_size,
            );
            let case = row.case;
            let expected = row.expected;
            assert_eq!(
                asked, expected,
                "case {case:?}: expected the scale-out to ask for {expected:?}, got {asked:?}"
            );
        }
    }
}
