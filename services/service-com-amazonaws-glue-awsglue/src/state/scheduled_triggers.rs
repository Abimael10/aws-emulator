// SPDX-License-Identifier: CC0-1.0

//! The firing of a SCHEDULED trigger: when the clock next owes one a firing, and what that
//! firing begins.
//!
//! "A `cron` expression used to specify the schedule […] to run something every day at 12:15
//! UTC, you would specify: `cron(15 12 * * ? *)`" (`Trigger$Schedule`): a `SCHEDULED` trigger
//! fires at the instants its own expression names, and it fires them only while it is ARMED —
//! "set to true to start SCHEDULED and CONDITIONAL triggers when created"
//! (`CreateTriggerRequest$StartOnCreation`) and "starts an existing trigger" (`StartTrigger`)
//! are what arm it, and "stops a specified trigger" (`StopTrigger`) is what disarms it. So an
//! armed trigger's next firing is an instant the CLOCK owes it, exactly as a job run's next
//! state is: the instant is STORED
//! ([`TriggerEntry::scheduled_firing_due_at`]), nothing sleeps for it, and whichever later
//! call reads the runs it begins materialises it — as does the store's own clock pump, so the
//! firing happens with no request waiting on it, which is what a schedule IS.
//!
//! A firing initiates "the actions initiated by this trigger" (`Trigger$Actions`) — each
//! action naming a job begins one run of that job, recorded as the run of this trigger
//! (`JobRun$TriggerName`) — and, for a trigger that belongs to a workflow, begins one run of
//! that workflow, which is what a firing of the EVENT trigger beside it does
//! ([`ComAmazonawsGlueAwsglueState::settle_due_event_batches`]).
//!
//! The expression's own instants are kept, the first one included: the schedule is the
//! caller's own figure, so nothing here brings a firing forward or holds one back. What a
//! firing that fell due while nothing was looking does NOT do is replay: the next firing is
//! the first instant the expression names ahead of the instant it is read at, so a schedule
//! that fell many periods behind catches up to the next instant rather than firing once per
//! instant it missed.

use super::*;

impl TriggerEntry {
    /// Whether this trigger's own schedule is RUNNING: a `SCHEDULED` trigger that is armed.
    ///
    /// Only a `SCHEDULED` trigger carries a schedule the service fires it on — every other
    /// `TriggerType` fires on a caller, a predicate or an event — and only an armed one is
    /// waiting to fire ([`TRIGGER_STATE_ACTIVATED`]), so a trigger a create did not start and
    /// one a stop deactivated are on no schedule at all.
    #[must_use]
    pub fn runs_on_a_schedule(&self) -> bool {
        self.trigger_type == TRIGGER_TYPE_SCHEDULED && self.state == TRIGGER_STATE_ACTIVATED
    }

    /// Re-arm this trigger's schedule as of `now`: the next firing it is owed is the first
    /// instant its expression names ahead of `now`, and NONE where it is on no schedule — a
    /// trigger of another type, one that is not armed, or one whose expression names no
    /// instant ahead at all.
    ///
    /// The ONE place the due instant is written, so every write that can change whether a
    /// trigger is on a schedule — the create that starts it, a start, a stop, and the update
    /// that overwrites its definition — leaves the same answer, and a firing arms the next one
    /// through it too.
    pub fn arm_schedule(&mut self, now: SystemTime) {
        self.scheduled_firing_due_at = if self.runs_on_a_schedule() {
            self.schedule
                .as_deref()
                .and_then(|expression| next_scheduled_firing(expression, now))
        } else {
            None
        };
    }
}

/// The first instant the schedule `expression` names strictly after `after`, or `None` when
/// it names none — because it is not an expression this service reads, or because it names no
/// instant ahead of `after` at all (a `cron` expression naming a year already past).
///
/// The expression is the documented `cron(<six fields>)` form, read through the workspace's
/// ONE reading of that grammar ([`::service_common::schedule_expression::parse_cron`]) rather
/// than a parser written here, so a schedule this fires on is one the write that stored it
/// accepted ([`is_cron_expression`]).
#[must_use]
pub fn next_scheduled_firing(expression: &str, after: SystemTime) -> Option<SystemTime> {
    let body = expression
        .trim()
        .strip_prefix("cron(")
        .and_then(|rest| rest.strip_suffix(')'))?;
    let schedule = ::service_common::schedule_expression::parse_cron(body).ok()?;
    let from: ::chrono::DateTime<::chrono::Utc> = after.into();
    schedule.after(&from).next().map(SystemTime::from)
}

impl ComAmazonawsGlueAwsglueState {
    /// Fire every SCHEDULED trigger of the Region `region` whose next firing the clock has
    /// already made due at `now`, and answer every job run those firings began.
    ///
    /// Each firing was owed at the instant its schedule named whether or not anybody called,
    /// so this is the write a reporting operation performs for it — the same write
    /// [`Self::settle_due_event_batches`] performs for a batch window the clock ran out — and
    /// what it begins is dated at the DUE instant rather than at the instant of the read.
    /// Nothing that was not already due moves, so a caller reading before the instant its
    /// schedule names observes nothing begun.
    ///
    /// The walk is bounded by the triggers this store holds ([`MAX_TRIGGERS`]) and takes them
    /// in name order (the map is ordered), so two reads at one instant fire the same triggers
    /// in the same order.
    pub fn settle_due_scheduled_trigger_firings(
        &mut self,
        region: &str,
        now: SystemTime,
    ) -> Vec<ActionJobRun> {
        let due: Vec<(String, SystemTime)> = self
            .triggers
            .get(region)
            .map(|triggers| {
                triggers
                    .values()
                    .filter(|trigger| trigger.runs_on_a_schedule())
                    .filter_map(|trigger| {
                        trigger
                            .scheduled_firing_due_at
                            .filter(|due| now >= *due)
                            .map(|due| (trigger.name.clone(), due))
                    })
                    .collect()
            })
            .unwrap_or_default();

        let mut begun = Vec::new();
        for (name, due) in due {
            // The next firing is armed BEFORE this one is performed, so a firing is performed
            // once however the walk is re-entered, and it is the first instant the expression
            // names ahead of now rather than of the instant that fell due: a schedule that
            // fell behind catches up instead of replaying what it missed.
            if let Some(trigger) = self.trigger_mut(region, &name) {
                trigger.arm_schedule(now);
            }
            begun.extend(self.fire_scheduled_trigger(region, &name, due));
        }
        begun
    }

    /// The same over EVERY Region this store holds triggers in: what the clock pump applies on
    /// a wake-up, which is not serving a request about one Region.
    pub fn settle_every_due_scheduled_trigger_firing(&mut self, now: SystemTime) {
        let regions: Vec<String> = self.triggers.keys().cloned().collect();
        for region in regions {
            self.settle_due_scheduled_trigger_firings(&region, now);
        }
    }

    /// The earliest instant at which this store owes a SCHEDULED trigger a firing, if any —
    /// one of the instants the clock pump waits for.
    pub(super) fn next_scheduled_trigger_firing_due_at(&self) -> Option<SystemTime> {
        self.triggers
            .values()
            .flat_map(BTreeMap::values)
            .filter(|trigger| trigger.runs_on_a_schedule())
            .filter_map(|trigger| trigger.scheduled_firing_due_at)
            .min()
    }

    /// Perform ONE firing of the trigger `trigger_name` of `region`, at `fired_at`, and answer
    /// every job run it began.
    ///
    /// A firing initiates the trigger's actions through the ONE place a firing's actions are
    /// initiated ([`Self::initiate_trigger_actions`]), so a run a schedule began is counted
    /// against the same ceiling as one a caller started and reported identically by every
    /// later read. A trigger that belongs to a workflow also begins one run of it, through the
    /// ONE place a run of a workflow is begun ([`WorkflowEntry::begin_run`]) — and past the
    /// ceiling that workflow named it begins none, exactly as a caller's own start is refused
    /// there, while the actions the firing initiates are still initiated.
    fn fire_scheduled_trigger(
        &mut self,
        region: &str,
        trigger_name: &str,
        fired_at: SystemTime,
    ) -> Vec<ActionJobRun> {
        // The workflow the firing runs and the actions it initiates, read off the stored
        // definition before anything is borrowed to write to.
        let Some((workflow_name, actions)) = self
            .trigger(region, trigger_name)
            .map(|trigger| (trigger.workflow_name.clone(), trigger.actions.clone()))
        else {
            return Vec::new();
        };
        let total_actions = i32::try_from(actions.len()).unwrap_or(i32::MAX);
        let begun_run = match workflow_name.as_deref() {
            // A firing passes no run properties of its own, so the run carries the workflow's
            // own defaults. A trigger naming a workflow that is gone begins no run rather than
            // failing the firing.
            Some(workflow_name) => self
                .workflow_mut(workflow_name)
                .and_then(|workflow| workflow.begin_run(None, fired_at, total_actions).ok()),
            None => None,
        };
        let begun_job_runs = self.initiate_trigger_actions(region, trigger_name, &actions, fired_at);
        if let (Some(workflow_name), Some(run_id)) =
            (workflow_name.as_deref(), begun_run.as_deref())
        {
            self.record_workflow_run_job_runs(workflow_name, run_id, &begun_job_runs);
        }
        ::tracing::info!(
            trigger = %trigger_name,
            region = %region,
            workflow = workflow_name.as_deref().unwrap_or("none"),
            workflow_run = begun_run.as_deref().unwrap_or("none"),
            job_runs = begun_job_runs.len(),
            "a Glue SCHEDULED trigger fired on its own schedule"
        );
        begun_job_runs
    }
}
