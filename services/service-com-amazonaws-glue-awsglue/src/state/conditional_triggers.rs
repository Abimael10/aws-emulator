// SPDX-License-Identifier: CC0-1.0

//! The GATES of a workflow run: the CONDITIONAL triggers of a workflow, whether the job runs
//! one of its runs began have satisfied one of their predicates, and the actions a firing adds
//! to that run.
//!
//! "Defines the predicate of the trigger, which determines when it fires" (`Predicate`): a
//! `CONDITIONAL` trigger carries one, each of its conditions names a component and the state
//! that component has to have reached, and the trigger "fires when its predicate is satisfied"
//! — so inside a workflow run, such a trigger is the gate between one stage of the run and the
//! next. The firing is owed to the run by the CLOCK, at the instant the states its conditions
//! name were reached, so it is materialised by whichever later call reads the run
//! ([`ComAmazonawsGlueAwsglueState::settle_due_workflow_runs`]) and nothing sleeps for it.

use super::*;

/// The `Logical` of a predicate every one of whose conditions has to hold. One of the two
/// modelled `Logical` values (`AND`, `ANY`), and the way a predicate that names none is read:
/// `Logical` is "an optional field if only one condition is listed" (`Predicate$Logical`), and
/// over one condition the two agree.
pub const LOGICAL_AND: &str = "AND";

/// The `Logical` of a predicate ANY ONE of whose conditions is enough for: the other modelled
/// `Logical` value. See [`LOGICAL_AND`].
pub const LOGICAL_ANY: &str = "ANY";

/// The `JobRunState` values a trigger's condition can watch a job's runs for.
///
/// "Currently, the only job states that a trigger can listen for are `SUCCEEDED`, `STOPPED`,
/// `FAILED`, and `TIMEOUT`" (`Condition$State`) — the terminal states of a run, which is what
/// makes a predicate a gate between two stages rather than a race with one. A condition naming
/// any other state, or naming none at all, names nothing this service listens for, so no run
/// satisfies it.
pub const CONDITION_JOB_RUN_STATES: [&str; 4] = [
    JOB_RUN_STATE_SUCCEEDED,
    JOB_RUN_STATE_STOPPED,
    JOB_RUN_STATE_FAILED,
    JOB_RUN_STATE_TIMEOUT,
];

/// One CONDITIONAL trigger of a workflow, copied out of the store before a run is written to:
/// the trigger's name, the predicate that decides when it fires, and the actions firing it
/// initiates.
#[derive(Debug, Clone)]
struct ConditionalGate {
    /// `Trigger$Name`, which is also what the run records as having fired
    /// ([`WorkflowRunEntry::fired_triggers`]) and what the runs the firing begins report as
    /// their `JobRun$TriggerName`.
    name: String,
    /// `Trigger$Predicate`: "the predicate of this trigger, which defines when it will fire."
    predicate: Option<Predicate>,
    /// `Trigger$Actions`: "the actions initiated by this trigger."
    actions: Vec<Action>,
}

impl ComAmazonawsGlueAwsglueState {
    /// Fire, inside each unfinished run of the workflow `workflow_name`, every CONDITIONAL
    /// trigger of that workflow whose predicate the run's own job runs have satisfied by `now`.
    ///
    /// A `CONDITIONAL` trigger "fires when its predicate is satisfied"
    /// (`CreateTriggerRequest$Predicate`), and the conditions of that predicate are the states
    /// of the workflow's own components — so the firing belongs to the RUN whose job runs
    /// reached those states: the actions it initiates are actions of that run, counted in its
    /// `WorkflowRunStatistics` and reported under the `JOB` nodes of its graph, and the run
    /// comes to rest only once they have too
    /// ([`ComAmazonawsGlueAwsglueState::settle_due_workflow_runs`]).
    ///
    /// A gate fires AT MOST ONCE per run ([`WorkflowRunEntry::record_trigger_firing`]), so a
    /// later read does not initiate its actions a second time, and it fires at the instant its
    /// predicate was satisfied rather than at the instant of the read: that firing was owed
    /// whether or not anybody called, and the run it begins therefore starts at or after the
    /// run that satisfied the condition finished.
    ///
    /// Only a gate that is ARMED fires ([`TriggerEntry::watches_a_predicate`]): a trigger the
    /// create did not start, and one a stop deactivated, is watching nothing. A run that has
    /// already finished gains no action, and the gates are taken in name order (the map is
    /// ordered), so one store reaches one outcome however often it is read.
    ///
    /// The walk is over this Region's triggers, bounded by [`MAX_TRIGGERS`], for each run of
    /// the workflow, bounded by [`MAX_RUNS_PER_WORKFLOW`].
    pub fn fire_satisfied_conditional_triggers(
        &mut self,
        region: &str,
        workflow_name: &str,
        now: SystemTime,
    ) {
        let gates = self.conditional_gates(region, workflow_name);
        if gates.is_empty() {
            return;
        }
        // The runs a gate can still fire inside, resolved before anything is written: a run
        // that has come to rest initiates nothing further.
        let open_runs: Vec<String> = self
            .workflow(workflow_name)
            .into_iter()
            .flat_map(|workflow| workflow.runs.iter())
            .filter(|run| !run.is_terminal())
            .map(|run| run.id.clone())
            .collect();

        for run_id in open_runs {
            for gate in &gates {
                // The run is re-read for every gate, because a gate that fired has added
                // actions to it and a later gate's predicate may be satisfied by them.
                let Some(run) = self
                    .workflow(workflow_name)
                    .and_then(|workflow| workflow.run(&run_id))
                else {
                    continue;
                };
                if run.has_fired_trigger(&gate.name) {
                    continue;
                }
                let Some(satisfied_at) =
                    self.predicate_satisfied_at(region, run, gate.predicate.as_ref())
                else {
                    continue;
                };
                // The firing was owed at the instant the predicate was satisfied, which is in
                // the past for every read that comes after it.
                let fired_at = satisfied_at.min(now);
                let actions = i32::try_from(gate.actions.len()).unwrap_or(i32::MAX);
                // The firing is recorded on the run BEFORE its actions are initiated, so a
                // gate cannot fire twice however the walk is re-entered, and "total number of
                // Actions in the workflow run" grows by what this gate initiates.
                if let Some(run) = self
                    .workflow_mut(workflow_name)
                    .and_then(|workflow| workflow.run_mut(&run_id))
                {
                    run.record_trigger_firing(&gate.name, actions);
                }
                // "The actions initiated by this trigger": initiated exactly as a firing a
                // caller or the event ingress causes initiates them, so each action naming a
                // job begins a run of it carrying the runs that satisfied the predicate as its
                // `PredecessorRuns`.
                let begun =
                    self.initiate_trigger_actions(region, &gate.name, &gate.actions, fired_at);
                self.record_workflow_run_job_runs(workflow_name, &run_id, &begun);
                ::tracing::info!(
                    trigger = %gate.name,
                    workflow = %workflow_name,
                    run = %run_id,
                    job_runs = begun.len(),
                    "a Glue CONDITIONAL trigger's predicate was satisfied, so it initiated its actions"
                );
            }
        }
    }

    /// Every ARMED `CONDITIONAL` trigger of the workflow `workflow_name` in the Region
    /// `region`, in name order, copied out of the store.
    ///
    /// A trigger belongs to the workflow its `WorkflowName` names and is served only through
    /// the endpoint that created it, so these are the gates a run of that workflow can pass
    /// through — the counterpart, for a run already under way, of the entry point a run begins
    /// at ([`ComAmazonawsGlueAwsglueState::starting_trigger`]).
    fn conditional_gates(&self, region: &str, workflow_name: &str) -> Vec<ConditionalGate> {
        self.triggers
            .get(region)
            .into_iter()
            .flat_map(BTreeMap::values)
            .filter(|trigger| {
                trigger.workflow_name.as_deref() == Some(workflow_name)
                    && trigger.watches_a_predicate()
            })
            .map(|trigger| ConditionalGate {
                name: trigger.name.clone(),
                predicate: trigger.predicate.clone(),
                actions: trigger.actions.clone(),
            })
            .collect()
    }

    /// The instant the predicate `predicate` was satisfied by the job runs the workflow run
    /// `run` began, or `None` while it is not satisfied.
    ///
    /// "A list of the conditions that determine when the trigger will fire"
    /// (`Predicate$Conditions`) are read under the predicate's own `Logical`: [`LOGICAL_ANY`]
    /// is satisfied by any ONE of them, so it was satisfied as soon as the first was — the
    /// EARLIEST instant one held — while [`LOGICAL_AND`], which is also how a predicate naming
    /// no `Logical` is read, needs every one of them, so it was satisfied only once the last
    /// did, at the LATEST of those instants. A predicate with no condition at all determines
    /// nothing and is satisfied by nothing.
    fn predicate_satisfied_at(
        &self,
        region: &str,
        run: &WorkflowRunEntry,
        predicate: Option<&Predicate>,
    ) -> Option<SystemTime> {
        let conditions = predicate?.conditions.as_deref().unwrap_or_default();
        if conditions.is_empty() {
            return None;
        }
        let satisfied: Vec<Option<SystemTime>> = conditions
            .iter()
            .map(|condition| self.condition_satisfied_at(region, run, condition))
            .collect();
        if predicate.and_then(|predicate| predicate.logical.as_deref()) == Some(LOGICAL_ANY) {
            satisfied.into_iter().flatten().min()
        } else {
            satisfied
                .into_iter()
                .collect::<Option<Vec<SystemTime>>>()?
                .into_iter()
                .max()
        }
    }

    /// The instant the condition `condition` was first satisfied by the job runs the workflow
    /// run `run` began, or `None` while none of them satisfies it.
    ///
    /// "The name of the job whose `JobRuns` this condition applies to, and on which this
    /// trigger waits" (`Condition$JobName`) is matched against the runs THIS workflow run
    /// began of that job, because the condition is part of that run's own dependency chain: a
    /// run of the same job that another workflow run — or a caller's own `StartJobRun` — began
    /// is no part of it. The instant is the earliest at which one of those runs reached the
    /// state the condition names, which is when the run that satisfied it finished.
    ///
    /// A condition naming a CRAWLER waits on a crawl rather than on a job run, and this crate
    /// records no crawl of a workflow run ([`CrawlerEntry`] holds none), so such a condition is
    /// satisfied by nothing here — it blocks a [`LOGICAL_AND`] predicate and contributes
    /// nothing to a [`LOGICAL_ANY`] one.
    fn condition_satisfied_at(
        &self,
        region: &str,
        run: &WorkflowRunEntry,
        condition: &Condition,
    ) -> Option<SystemTime> {
        let job_name = condition.job_name.as_deref()?;
        let watched = condition
            .state
            .as_deref()
            .filter(|state| CONDITION_JOB_RUN_STATES.contains(state))?;
        let job = self.job(region, job_name)?;
        run.action_job_runs
            .iter()
            .filter(|begun| begun.job_name == job_name)
            .filter_map(|begun| job.run(&begun.run_id))
            .filter(|job_run| job_run.state == watched)
            .map(|job_run| job_run.completed_on.unwrap_or(job_run.last_modified_on))
            .min()
    }
}
