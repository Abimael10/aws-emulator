// SPDX-License-Identifier: CC0-1.0

//! The ROLLING UPDATE's two bounds: how many tasks the scheduler may start before it stops
//! the tasks they replace, and how few it may leave running while it does.
//!
//! # The rule this module implements
//!
//! `DeploymentConfiguration` states "optional deployment parameters that control how many
//! tasks run during a deployment and the ordering of stopping and starting tasks", and the
//! two percentages bound the SAME count from opposite ends:
//!
//! * `maximumPercent` is "an upper limit on the number of your service's tasks that are
//!   allowed in the `RUNNING` or `PENDING` state during a deployment, as a percentage of the
//!   `desiredCount` (rounded down to the nearest integer)", and what that upper limit BUYS
//!   is the ordering: "if your service ... has a `desiredCount` of four tasks and a
//!   `maximumPercent` value of 200%, the scheduler may start four new tasks before stopping
//!   the four older tasks";
//! * `minimumHealthyPercent` is "a lower limit on the number of your service's tasks that
//!   must remain in the `RUNNING` state during a deployment, as a percentage of the
//!   `desiredCount` (rounded up to the nearest integer)" — so a task being replaced is
//!   stopped only while the tasks left running still meet that floor, and "if any tasks are
//!   unhealthy and if `maximumPercent` doesn't allow the Amazon ECS scheduler to start
//!   replacement tasks, the scheduler stops the unhealthy tasks one-by-one — using the
//!   `minimumHealthyPercent` as a constraint — to clear up capacity to launch replacement
//!   tasks".
//!
//! Both bounds are therefore about ONE scheduler pass over a service that holds tasks it is
//! replacing: the pass starts as many tasks of the service's current deployment as the
//! ceiling leaves room for, and stops as many of the tasks being replaced as the floor
//! leaves room for. Neither bound constrains a service that is not replacing anything: a
//! service that already holds the task set it wants is at its desired count, which both
//! percentages are a percentage OF.
//!
//! # The two steps of the replacement, and the span between them
//!
//! A replacement and the retirement it allows are two STEPS of the rollout, and the
//! scheduler takes them one after the other — which is what a client watching a rolling
//! update observes:
//!
//! * a task the scheduler stopped to make room is followed by its replacement one step
//!   later. The `minimumHealthyPercent` doc: the scheduler "stops the unhealthy tasks
//!   one-by-one — using the `minimumHealthyPercent` as a constraint — to clear up capacity
//!   to launch replacement tasks" — so a ceiling that leaves no surge room takes the
//!   service BELOW its desired count while the capacity it cleared is being filled
//!   ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::awaiting_cleared_capacity`]);
//! * a task whose replacement has come up is not retired in the same instant. The same doc:
//!   "the service scheduler will wait ... after a task reaches a `RUNNING` state before the
//!   task is counted towards the minimum healthy percent total" — so the retirement the
//!   floor now leaves room for is owed one step later
//!   ([`ServiceRecord::retirement_due_at`]), and a caller polling through the rollout sees
//!   the service ABOVE its desired count while the two overlap, which is the surge the
//!   ceiling was there to allow.
//!
//! [`ROLLOUT_OVERLAP_SPAN`] and [`ROLLOUT_STEP_SPAN`] are this emulator's own spans for
//! those two steps, stored and compared and never slept on, exactly as every other
//! clock-owed transition here is.
//!
//! # Why one pass does not finish the rollout
//!
//! A task the pass starts is `PENDING` and reaches `RUNNING` on its own clock, so the room
//! the floor gives for stopping the tasks it replaces does not exist until it does. That is
//! what makes the rollout a sequence of passes rather than one write: the scheduler's
//! steady-state pass ([`super::service_steady_state`]) runs this computation again at every
//! instant the clock materialises, so the replacements start first, the floor holds while
//! they come up, and the tasks they replace are stopped once they have.

use super::*;

/// How long a replacement and the task it replaces OVERLAP before the retirement the floor
/// now leaves room for is carried out.
///
/// EMULATOR-CHOSEN. Amazon ECS documents a wait here ("the service scheduler will wait ...
/// after a task reaches a `RUNNING` state before the task is counted towards the minimum
/// healthy percent total") but not one this emulator could hold a caller to, so this is a
/// span of its own — comfortably longer than the steps a replacement itself takes to come
/// up, so the reading the ordering produces (the service above its desired count while the
/// two overlap) is observable to a client polling the service rather than collapsing into
/// the instant one scheduler pass runs in.
pub const ROLLOUT_OVERLAP_SPAN: Duration = Duration::from_secs(5);

/// How long the capacity a retirement CLEARED stays empty before the replacement it was
/// cleared for is started.
///
/// EMULATOR-CHOSEN, for the same reason and in the same spirit as
/// [`ROLLOUT_OVERLAP_SPAN`]: "the scheduler stops the unhealthy tasks one-by-one — using
/// the `minimumHealthyPercent` as a constraint — to clear up capacity to launch replacement
/// tasks", so the launch follows the stop rather than happening in the same instant, and a
/// client polling a rollout with no surge room observes the service below its desired count
/// while the capacity is filled.
pub const ROLLOUT_STEP_SPAN: Duration = Duration::from_secs(2);

/// What ONE scheduler pass over a service that is replacing tasks may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RollingUpdateRoom {
    /// How many tasks of the service's CURRENT deployment the pass may start — the
    /// shortfall against the desired count, held to what the ceiling leaves room for.
    pub placements: usize,
    /// How many of the RUNNING tasks being replaced it may stop — what the floor leaves
    /// room for. A task being replaced that is not running yet counts against neither
    /// bound's reading of a RUNNING task, so stopping it is not held to this.
    pub running_stops: usize,
    /// Whether the service is holding MORE tasks than it wants — the surge the ceiling
    /// allowed, which is the replacements running beside the tasks they replace. What tells
    /// the two orderings apart: a rollout with surge room has an overlap to wait out before
    /// it retires anything ([`ROLLOUT_OVERLAP_SPAN`]), while one whose ceiling left none is
    /// stopping tasks "to clear up capacity to launch replacement tasks" and has nothing to
    /// wait for.
    pub overlapping: bool,
}

/// What a service's task set COUNTS, as the two bounds read it.
#[derive(Debug, Clone, Copy, Default)]
pub struct TaskSetCounts {
    /// How many tasks of the service's current deployment it still wants running.
    pub current: usize,
    /// How many tasks it holds in all — the current deployment's and the ones being
    /// replaced — that are still wanted running, which is the count `maximumPercent`
    /// bounds ("`RUNNING` or `PENDING`").
    pub wanted: usize,
    /// How many of those have reached `RUNNING`, which is the count `minimumHealthyPercent`
    /// bounds.
    pub running: usize,
}

/// The lower limit on the tasks that must stay `RUNNING` during a deployment, as a COUNT:
/// `minimumHealthyPercent` "as a percentage of the `desiredCount` (rounded up to the
/// nearest integer)".
///
/// A configuration naming no floor is one whose floor has already been defaulted
/// ([`deployment_configuration`]); read as no floor at all here, so this answers the same
/// count whichever call stored the configuration.
pub fn minimum_healthy_tasks(configuration: &DeploymentConfiguration, desired: i32) -> i32 {
    let percent = configuration.minimum_healthy_percent.unwrap_or_default();
    if desired <= 0 || percent <= 0 {
        return 0;
    }
    // Rounded UP: the member's own words, and the direction that keeps the floor a floor —
    // half of three tasks is two tasks that have to stay running, not one.
    let scaled = i64::from(desired) * i64::from(percent) + 99;
    i32::try_from(scaled / 100).unwrap_or(i32::MAX)
}

/// The upper limit on the tasks allowed `RUNNING` or `PENDING` during a deployment, as a
/// COUNT: `maximumPercent` "as a percentage of the `desiredCount` (rounded down to the
/// nearest integer)".
///
/// A configuration naming no ceiling leaves the scheduler the desired count itself: the
/// count a service wants is always allowed to run, which is why the member's own range
/// starts at 100% ([`MIN_MAXIMUM_PERCENT`]).
pub fn maximum_tasks(configuration: &DeploymentConfiguration, desired: i32) -> i32 {
    let Some(percent) = configuration.maximum_percent else {
        return desired.max(0);
    };
    if desired <= 0 {
        return 0;
    }
    // Rounded DOWN: the member's own words.
    let scaled = i64::from(desired) * i64::from(percent.max(0)) / 100;
    i32::try_from(scaled).unwrap_or(i32::MAX)
}

/// The room ONE scheduler pass has, under the deployment configuration `configuration`, for
/// a service whose `desired_count` is `desired` and whose task set counts `counts`.
///
/// The ceiling bounds what may be STARTED and the floor what may be STOPPED, each against
/// the task set as the pass found it: that is what makes the replacements start before the
/// tasks they replace are stopped, and what keeps the tasks left running at the floor until
/// they have.
pub fn rolling_update_room(
    configuration: &DeploymentConfiguration,
    desired: i32,
    counts: TaskSetCounts,
) -> RollingUpdateRoom {
    let wanted = usize::try_from(desired).unwrap_or(0);
    let shortfall = wanted.saturating_sub(counts.current);
    let ceiling = usize::try_from(maximum_tasks(configuration, desired)).unwrap_or(0);
    let floor = usize::try_from(minimum_healthy_tasks(configuration, desired)).unwrap_or(0);
    RollingUpdateRoom {
        placements: shortfall.min(ceiling.saturating_sub(counts.wanted)),
        running_stops: counts.running.saturating_sub(floor),
        overlapping: counts.wanted > wanted,
    }
}

/// What one scheduler pass does about the retirement the floor now leaves room for, and
/// what the service is left owing.
///
/// `due` is the instant stored back on the service ([`ServiceRecord::retirement_due_at`]):
/// where the replacements are overlapping the tasks they replace, the retirement is ARMED
/// when the pass first finds room for it and CARRIED OUT one [`ROLLOUT_OVERLAP_SPAN`]
/// later, so the two overlap for that span however long the replacement itself took to come
/// up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetirementStep {
    /// Whether this pass retires the tasks the floor leaves room to retire.
    pub retire: bool,
    /// What the service owes afterwards: the instant the retirement falls due, and `None`
    /// when nothing is owed (there is nothing to retire, or this pass has just done it).
    pub due: Option<SystemTime>,
}

/// The retirement step a pass takes, given the `room` the floor leaves it, what the service
/// already owed (`owed`) and the instant `now`.
pub fn retirement_step(
    room: RollingUpdateRoom,
    owed: Option<SystemTime>,
    now: SystemTime,
) -> RetirementStep {
    if room.running_stops == 0 {
        // Nothing is being retired, so nothing is owed: the next replacement that comes up
        // arms its own step.
        return RetirementStep {
            retire: false,
            due: None,
        };
    }
    if !room.overlapping {
        // There is no overlap to wait out: the ceiling left no room to start a replacement
        // above the desired count, so this stop is one the scheduler makes "to clear up
        // capacity to launch replacement tasks", and it is the REPLACEMENT that follows it
        // a step later
        // ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::awaiting_cleared_capacity`]).
        return RetirementStep {
            retire: true,
            due: None,
        };
    }
    match owed {
        // The step has fallen due: the tasks the floor leaves room for are retired, and the
        // retirement after this one arms afresh.
        Some(due) if due <= now => RetirementStep {
            retire: true,
            due: None,
        },
        // Armed and not due yet: the tasks being replaced keep running, which is what makes
        // the overlap observable.
        Some(due) => RetirementStep {
            retire: false,
            due: Some(due),
        },
        // The first pass that finds room arms the step rather than taking it.
        None => RetirementStep {
            retire: false,
            due: Some(now + ROLLOUT_OVERLAP_SPAN),
        },
    }
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// The earliest instant at which a rolling update's next step falls due, or `None` when
    /// none is owed.
    ///
    /// One of the instants the clock pump waits for: a rollout takes one step at a time, and
    /// the step after the one a pass just took — the retirement a service has armed
    /// ([`ROLLOUT_OVERLAP_SPAN`]), and the replacement owed for capacity a retirement
    /// cleared ([`ROLLOUT_STEP_SPAN`]) — is owed whether or not anybody calls again.
    ///
    /// The retirement is the instant the service itself stores (cleared when it is carried
    /// out, so it cannot wake the pump twice). The replacement is read off the retired task,
    /// which keeps the instant it was stopped at for good, so only one still in the FUTURE
    /// is answered: a pass has already taken the step of one that is past.
    pub(super) fn next_scheduler_step_due_at(&self, now: SystemTime) -> Option<SystemTime> {
        let scheduling: Vec<&ServiceRecord> = self
            .services
            .values()
            .filter(|service| {
                service.is_active()
                    && service.places_tasks()
                    && service.deployment_rollout_failure.is_none()
            })
            .collect();
        if scheduling.is_empty() {
            return None;
        }
        let armed = scheduling
            .iter()
            .filter_map(|service| service.retirement_due_at)
            .min();
        let clearing = self
            .tasks
            .values()
            .filter(|task| {
                task.desired_status == STATUS_STOPPED
                    && task.stop_code.as_deref() == Some(STOP_CODE_SERVICE_SCHEDULER)
                    && scheduling.iter().any(|service| {
                        task.cluster_name == service.cluster_name
                            && task.service_name.as_deref() == Some(service.name.as_str())
                            && task.started_by.as_deref() != Some(service.deployment_id.as_str())
                    })
            })
            .filter_map(|task| {
                task.stopping_at
                    .and_then(|at| SystemTime::try_from(at).ok())
                    .map(|at| at + ROLLOUT_STEP_SPAN)
            })
            .filter(|due| *due > now)
            .min();
        [armed, clearing].into_iter().flatten().min()
    }

    /// Whether the scheduler has just cleared capacity for `service` and so owes the
    /// replacement that capacity was cleared for, as of `now`.
    ///
    /// "The scheduler stops the unhealthy tasks one-by-one — using the
    /// `minimumHealthyPercent` as a constraint — to clear up capacity to launch replacement
    /// tasks": the launch FOLLOWS the stop that made room for it, by one
    /// [`ROLLOUT_STEP_SPAN`], so a ceiling that leaves no surge room takes the service below
    /// its desired count for that span instead of swapping a task in the same instant.
    ///
    /// Read off the tasks themselves — a task of a SUPERSEDED deployment the scheduler asked
    /// to stop that recently — so nothing has to be remembered beside them.
    pub(super) fn awaiting_cleared_capacity(
        &self,
        service: &ServiceRecord,
        now: SystemTime,
    ) -> bool {
        self.tasks.values().any(|task| {
            task.cluster_name == service.cluster_name
                && task.service_name.as_deref() == Some(service.name.as_str())
                && task.started_by.as_deref() != Some(service.deployment_id.as_str())
                && task.desired_status == STATUS_STOPPED
                && task.stop_code.as_deref() == Some(STOP_CODE_SERVICE_SCHEDULER)
                && task
                    .stopping_at
                    .and_then(|at| SystemTime::try_from(at).ok())
                    .is_some_and(|at| at + ROLLOUT_STEP_SPAN > now)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The configuration a row states, with nothing else of a deployment's in it.
    fn percentages(minimum: Option<i32>, maximum: Option<i32>) -> DeploymentConfiguration {
        DeploymentConfiguration {
            minimum_healthy_percent: minimum,
            maximum_percent: maximum,
            ..DeploymentConfiguration::default()
        }
    }

    #[test]
    fn a_percentage_of_the_desired_count_rounds_the_way_its_member_documents() {
        // Each row is a count the two members' own rounding directions decide: the floor
        // rounds up, the ceiling rounds down, so three tasks at half are two that stay and
        // four that may run.
        let rows: [(i32, i32, i32, i32, i32); 6] = [
            (2, 100, 200, 2, 4),
            (3, 50, 150, 2, 4),
            (1, 100, 150, 1, 1),
            (4, 0, 100, 0, 4),
            (0, 100, 200, 0, 0),
            (7, 75, 133, 6, 9),
        ];
        for (desired, minimum, maximum, floor, ceiling) in rows {
            let configuration = percentages(Some(minimum), Some(maximum));
            let case = format!("desired {desired}, {minimum}%..{maximum}%");
            assert_eq!(
                minimum_healthy_tasks(&configuration, desired),
                floor,
                "case {case:?}: expected a floor of {floor}, got {}",
                minimum_healthy_tasks(&configuration, desired)
            );
            assert_eq!(
                maximum_tasks(&configuration, desired),
                ceiling,
                "case {case:?}: expected a ceiling of {ceiling}, got {}",
                maximum_tasks(&configuration, desired)
            );
        }
    }

    #[test]
    fn the_ceiling_bounds_the_placements_and_the_floor_the_stops() {
        let configuration = percentages(Some(100), Some(200));
        // Two old tasks running, none of the new deployment yet: the ceiling leaves room
        // for both replacements and the floor leaves no room to stop anything.
        let start = rolling_update_room(
            &configuration,
            2,
            TaskSetCounts {
                current: 0,
                wanted: 2,
                running: 2,
            },
        );
        assert_eq!(
            start,
            RollingUpdateRoom {
                placements: 2,
                running_stops: 0,
                overlapping: false
            },
            "case \"floor at the desired count\": expected both replacements and no stop, got {start:?}"
        );
        // Once the replacements are running the floor is met twice over, so both of the
        // tasks they replace may stop.
        let settled = rolling_update_room(
            &configuration,
            2,
            TaskSetCounts {
                current: 2,
                wanted: 4,
                running: 4,
            },
        );
        assert_eq!(
            settled,
            RollingUpdateRoom {
                placements: 0,
                running_stops: 2,
                overlapping: true
            },
            "case \"replacements running\": expected no placement and two stops, got {settled:?}"
        );
    }

    #[test]
    fn a_retirement_waits_out_the_overlap_and_a_stop_that_clears_capacity_does_not() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let overlapping = RollingUpdateRoom {
            placements: 0,
            running_stops: 1,
            overlapping: true,
        };
        // The first pass that finds room ARMS the retirement, the pass before it falls due
        // retires nothing, and the pass after it retires.
        let armed = retirement_step(overlapping, None, now);
        assert_eq!(
            armed,
            RetirementStep {
                retire: false,
                due: Some(now + ROLLOUT_OVERLAP_SPAN)
            },
            "case \"first pass with room\": expected the retirement to be armed, got {armed:?}"
        );
        let waiting = retirement_step(overlapping, armed.due, now + Duration::from_millis(1));
        assert_eq!(
            waiting,
            RetirementStep {
                retire: false,
                due: armed.due
            },
            "case \"inside the overlap\": expected the task to keep running, got {waiting:?}"
        );
        let taken = retirement_step(overlapping, armed.due, now + ROLLOUT_OVERLAP_SPAN);
        assert_eq!(
            taken,
            RetirementStep {
                retire: true,
                due: None
            },
            "case \"overlap over\": expected the retirement to be taken, got {taken:?}"
        );
        // A stop that CLEARS capacity has no overlap to wait out, and a pass with no room
        // retires nothing and owes nothing.
        let clearing = retirement_step(
            RollingUpdateRoom {
                placements: 0,
                running_stops: 1,
                overlapping: false,
            },
            None,
            now,
        );
        assert_eq!(
            clearing,
            RetirementStep {
                retire: true,
                due: None
            },
            "case \"no surge room\": expected the stop that clears capacity to be made at \
             once, got {clearing:?}"
        );
        let idle = retirement_step(
            RollingUpdateRoom {
                placements: 1,
                running_stops: 0,
                overlapping: true,
            },
            Some(now),
            now,
        );
        assert_eq!(
            idle,
            RetirementStep {
                retire: false,
                due: None
            },
            "case \"nothing to retire\": expected nothing owed, got {idle:?}"
        );
    }
}
