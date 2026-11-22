// SPDX-License-Identifier: CC0-1.0

//! How a stack records that the operation in flight could NOT be carried out, and the
//! rollback that failure owes.
//!
//! Moved out of [`super`] unchanged so that file stays inside the length a change to this
//! repository may touch: the failure of an operation and the removal or revert it owes are one
//! concern — which resource stopped the operation, which status the stack takes, which
//! resources it goes on reporting, and what the rollback then has to ask the owning services
//! for.

use super::*;

/// Whether a stack-level EVENT reports the failed status a failed transition records.
///
/// CloudFormation's event stream is what the stack DID: a failure the stack rests in is an
/// event of its own, while a failure whose rollback begins at the same instant is reported
/// by that rollback's event instead — the stack moved straight from the operation's
/// `*_IN_PROGRESS` into `ROLLBACK_IN_PROGRESS` / `UPDATE_ROLLBACK_IN_PROGRESS` and never
/// rested in the failed status for an event to report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FailureReport {
    /// The stack rests in the failed status: the event log carries it.
    Rested,
    /// A rollback or a removal supersedes it at the same instant: no event of its own.
    Superseded,
    /// A PRE-DEPLOYMENT VALIDATION failed the operation: that stage ran before anything was
    /// asked of any owning service, so NO resource has an event that could carry the
    /// failure and the stack's own event is the one report of it — whether or not the
    /// rollback it owes begins at this instant. `rests` is whether the stack stays in the
    /// failed status, as it is for the two reports above.
    FailedValidation { rests: bool },
    /// Processing the TEMPLATE failed the operation: the document the stack would have
    /// deployed could not be produced, so nothing was ever asked of any owning service
    /// and no resource has an event that could carry the diagnostic — the stack's own
    /// failed event reports it verbatim, even where the rollback it owes begins at this
    /// same instant. The stack does not rest in the failed status, exactly as
    /// [`FailureReport::Superseded`] does not.
    Diagnosed,
}

impl StoredStack {
    /// Record that the operation in flight could not be carried out, and that the stack
    /// RESTS in the failed status: it takes that status with the reason, the clock is owed
    /// nothing more, and the event log carries the failure.
    ///
    /// This is how CloudFormation reports a stack whose resources an owning service
    /// refused to create or delete and that is left in the failed state
    /// (`CREATE_FAILED` / `DELETE_FAILED`) — the operation
    /// does not stay pending and no terminal status it never reached is reported.
    ///
    /// [`Self::fail_transition_superseded`] is the other report: the same failure, for a
    /// stack that moves into its rollback at this very instant.
    ///
    /// The operation CONCLUDED at `now`, which for a failure an owning service answered is
    /// sooner than the span the walk over the resources was laid out across: the walk is
    /// therefore compressed into the span the operation really took, so what it had done is
    /// recorded in the order it did it, at its own instants, ahead of the failure that ends
    /// it ([`timeline::Timeline::compress_into`]).
    pub fn fail_transition(&mut self, now: SystemTime, failed_status: &str, reason: String) {
        self.record_failure(now, failed_status, reason, FailureReport::Rested);
    }

    /// HOLD the failure a teardown walk reported, where that walk still owes resources a
    /// call: `true` when the failure was held, `false` when the caller is to report it now
    /// ([`Self::fail_transition`]).
    ///
    /// "CloudFormation deletes the resources in reverse order", so a teardown leaves what a
    /// resource still being removed depends on ALONE: a removal the owning service — or a
    /// custom resource's PROVIDER — has only ACCEPTED is not a resource that is gone. Those
    /// resources are asked about once that removal answers, so a teardown reports its own
    /// failure only after it has asked about every resource it still can — a refusal of one
    /// resource holds back only the resources that refused one needs, and a sibling it has
    /// nothing to do with is deleted rather than left behind under the status its create left
    /// it in.
    ///
    /// What is held is the PENDING transition: the operation goes on in its `*_IN_PROGRESS`
    /// status, headed for the same failed status with the same reason
    /// ([`Self::fail_pending_transition`]), and settles into it once the owed pass is through
    /// ([`Self::owes_a_teardown`] is what holds the clock back until then). The refusals are
    /// remembered so that pass asks about none of them a second time
    /// ([`Self::teardown_refusals`]).
    ///
    /// Nothing is held where the walk accepted no removal — there is no answer to wait for,
    /// so the failure is final — nor where every resource left is one the refusals hold back,
    /// which no pass could delete either.
    pub fn hold_teardown_failure(
        &mut self,
        transitions: &[provisioning::ResourceTransition],
        removing: &[provisioning::Stabilising],
        refused: &[String],
        failed_status: &str,
        reason: &str,
    ) -> bool {
        if removing.is_empty() || self.pending.is_none() {
            return false;
        }
        let refusals: BTreeSet<String> = refused.iter().cloned().collect();
        if !self.teardown_owes_resources_past(&refusals) {
            return false;
        }
        // Each resource this pass acted on takes the status it left it in: a teardown that
        // ends in a failure disposes of no record ([`Self::record_resource_deletion_statuses`]),
        // so what became of each one is recorded as the pass that did it reports back.
        self.record_resource_deletion_statuses(transitions, SystemTime::now());
        self.teardown_refusals.extend(refusals);
        self.fail_pending_transition(failed_status, reason.to_string());
        true
    }

    /// Record a failure of the operation in flight that the stack does NOT rest in, because
    /// the removal that failure owes begins at the same instant — a create CloudFormation
    /// rolls back, a create it takes the stack away for, or an update it reverts.
    ///
    /// Everything [`Self::fail_transition`] records is recorded: the stack takes the failed
    /// status with the reason and the attempt is concluded as failed at `now`, which is what
    /// the removal that follows is then opened over. What is NOT recorded is a stack-level
    /// EVENT for that status: the stack moved straight from the operation's
    /// `*_IN_PROGRESS` into the rollback — whose own event carries the failure and the
    /// clause saying the rollback was asked for ([`Self::begin_rollback`]) — so no event
    /// reports a status the stack never rested in. The resources' own failure events are
    /// untouched: WHICH resource failed, and why, is reported on that resource, and a
    /// failed status the stack does rest in (`DisableRollback`, `OnFailure=DO_NOTHING`)
    /// still gets its event from [`Self::fail_transition`].
    pub fn fail_transition_superseded(
        &mut self,
        now: SystemTime,
        failed_status: &str,
        reason: String,
    ) {
        self.record_failure(now, failed_status, reason, FailureReport::Superseded);
    }

    /// Record a failure of the operation in flight that a PRE-DEPLOYMENT VALIDATION reached:
    /// the stage runs before anything is asked of any owning service, so no resource of this
    /// stack has an event that could carry the failure and the stack's own event is the one
    /// report of it.
    ///
    /// Everything the two reports above record is recorded, and `rests` says which of them
    /// this is: a stack that keeps its failed state (`DisableRollback`,
    /// `OnFailure=DO_NOTHING`) rests in the failed status, and one whose rollback begins at
    /// this instant does not. Either way the event is recorded, carrying the detailed status
    /// the model gives a validation failure (`VALIDATION_FAILED`, "template validation failed
    /// because of invalid properties in the template", whose `ResourceStatusReason` is what
    /// failed it) — and the validation's own detail stays on the result event
    /// `DescribeEvents` reports ([`Self::record_validation_failure`]).
    pub fn fail_transition_on_validation(
        &mut self,
        now: SystemTime,
        failed_status: &str,
        reason: String,
        rests: bool,
    ) {
        self.record_failure(
            now,
            failed_status,
            reason,
            FailureReport::FailedValidation { rests },
        );
    }

    /// Record a failure of the operation in flight that PROCESSING THE TEMPLATE reached:
    /// the document the stack would have deployed could not be produced at all — an
    /// `Fn::Transform` CloudFormation could not apply — so nothing was ever asked of any
    /// owning service.
    ///
    /// Everything [`Self::fail_transition_superseded`] records is recorded, and the stack
    /// does not rest in the failed status either: the rollback that failure owes begins at
    /// this same instant. What IS recorded beside it is the stack's own failed EVENT,
    /// carrying the diagnostic verbatim — the report [`Self::fail_transition_on_validation`]
    /// makes for the other stages that run before provisioning, and for the same reason:
    /// no resource of this stack has an event that could carry a failure of the template
    /// itself, so the stack's own event is where a client polling the operation reads it.
    /// The transform's sentence is CloudFormation's own, not a validation's, so the event
    /// carries no detailed status.
    pub fn fail_transition_on_template(
        &mut self,
        now: SystemTime,
        failed_status: &str,
        reason: String,
    ) {
        self.record_failure(now, failed_status, reason, FailureReport::Diagnosed);
    }

    /// The one writer of a failed transition, for every one of the reports above.
    pub(super) fn record_failure(
        &mut self,
        now: SystemTime,
        failed_status: &str,
        reason: String,
        report: FailureReport,
    ) {
        // Whether the stack STAYS in the failed status, which is what decides the resources
        // it goes on reporting, and whether an event of its own reports that status.
        let rests = matches!(
            report,
            FailureReport::Rested | FailureReport::FailedValidation { rests: true }
        );
        // A CREATE whose failed state the stack RESTS in goes on reporting the resources it
        // had made ([`Self::keep_failed_resources`]), and what the walk left each of them in
        // is read off that walk — BEFORE the flush below retires it.
        let creating = self.status == status::CREATE_IN_PROGRESS;
        let walked = if creating && rests {
            self.walked_transitions()
        } else {
            Vec::new()
        };
        self.flush_resource_timeline(now);
        let pending = self.pending.take();
        // An operation that failed waits for nothing: whatever it was waiting at, it is
        // not going to proceed on a signal.
        self.wait_conditions.clear();
        self.stabilisations.clear();
        // ...and a failure HELD for this operation is not reported on top of the one being
        // recorded here: this is the operation's conclusion, so whatever it had reached
        // before belongs to nothing ([`Self::hold_failed_deployment`]). The report of a held
        // failure comes through here too, having taken it first, so nothing is lost.
        self.held_failure = None;
        self.status = failed_status.to_string();
        self.status_reason = Some(reason.clone());
        let operation_id = pending.as_ref().and_then(|p| p.operation_id.clone());
        // The attempt FAILED at this instant, whatever its failure then owes.
        self.conclude_operation(operation_id.as_deref(), failed_status, now);
        // A create that failed is not a create that did nothing: where the failed state
        // STAYS — the caller asked for it to be kept, or nothing follows the failure — the
        // resources the create had already made go on being reported, each in the status
        // the walk left it. A create whose rollback begins at this instant installs nothing,
        // because that rollback removes what it had made.
        if creating && rests {
            self.keep_failed_resources(&walked, now);
        }
        if report == FailureReport::Superseded {
            return;
        }
        self.record_event(
            now,
            failed_status,
            Some(reason),
            operation_id,
            pending.and_then(|p| p.client_request_token),
        );
        // ...and where the PRE-DEPLOYMENT VALIDATION stage is what failed the operation, the
        // event just recorded is the one that reports it: it carries that detailed status
        // ([`super::detailed_status_of`]).
        if matches!(report, FailureReport::FailedValidation { .. }) {
            if let Some(recorded) = self.events.back_mut() {
                recorded.failed_validation = true;
            }
        }
    }

    /// Install the resources a FAILED create made, for a create whose failed state the
    /// caller asked to keep (`OnFailure=DO_NOTHING`, which `DisableRollback` also spells).
    ///
    /// A create that failed is not a create that did nothing: CloudFormation reports every
    /// resource whose creation it STARTED, each in the status the walk left it — the ones
    /// their owning services created as `CREATE_COMPLETE`, and the one a service refused as
    /// `CREATE_FAILED` — which is what a client reads to see how far the deployment got and
    /// which resource stopped it. A create that is ROLLED BACK instead installs nothing,
    /// because the rollback removes what it had made, so this is only called where the failed
    /// state stays.
    ///
    /// Each resource's status is the LAST transition the walk recorded for it
    /// ([`provisioning::ResourceTransition`]), so no status is decided here; a declaration the
    /// walk never reached has no transition and is not reported at all, exactly as
    /// CloudFormation reports a resource only once its creation has started. The pending
    /// deployment itself is LEFT in place, since it is also what the stack's own deletion
    /// deletes ([`Self::current_resources`]).
    ///
    /// That transition's REASON comes with its status: "Success/failure message associated
    /// with the resource" is where the owning service's refusal of the resource survives into
    /// the per-resource views, so the resource whose failure stopped the create reports why it
    /// failed and not merely that it did. A transition with nothing to explain carries no
    /// reason, so a resource in a success status reports none — the member is then absent, as
    /// it is for every other resource whose status speaks for itself.
    pub fn keep_failed_resources(
        &mut self,
        transitions: &[provisioning::ResourceTransition],
        now: SystemTime,
    ) {
        // What the operation's EARLIER passes left each resource in, which is what the event
        // log still says about them: a deployment that WITHHELD a resource until what it reads
        // had published its values is carried out in more than one pass
        // ([`provisioning::deferred`]), and each pass's walk replaces the one before it
        // ([`Self::schedule_resource_events`] flushes it into the log). So the walk in hand is
        // the LAST pass's alone, and without this a failure of that pass would report only the
        // resources IT acted on — dropping every resource the operation really created.
        let recorded = self.recorded_resource_statuses();
        let Some(deployment) = self.pending_deployment.as_ref() else {
            return;
        };
        let finished_at = self.resource_finished_instants();
        let installed: Vec<DeployedResource> = deployment
            .resources
            .iter()
            .filter_map(|resource| {
                let walked = transitions
                    .iter()
                    .rev()
                    .find(|transition| transition.logical_id == resource.logical_id);
                // A resource neither the walk in hand nor the log says anything about is one
                // the operation never reached: nothing was created under it, and
                // CloudFormation reports a resource only once its creation has started.
                let (status, reason, reached_at) = match walked {
                    Some(walked) => (
                        walked.status.clone(),
                        walked.reason.clone(),
                        finished_at.get(&resource.logical_id).copied(),
                    ),
                    None => {
                        let (status, reason, at) = recorded.get(&resource.logical_id)?;
                        (status.clone(), reason.clone(), Some(*at))
                    }
                };
                let mut record =
                    DeployedResource::new(resource.clone(), &status, reached_at.unwrap_or(now));
                record.status_reason = reason;
                Some(record)
            })
            .collect();
        self.resources = installed;
        self.reevaluate_outputs();
    }

    /// The status, the reason and the instant the EVENT LOG last reports for each RESOURCE of
    /// this stack, by logical id.
    ///
    /// The log is what the operation DID ([`Self::schedule_resource_events`]), so it is the
    /// record of a pass whose own walk has since been replaced by a later one's. A transition
    /// still IN PROGRESS is not what became of the resource, for the reason
    /// [`Self::walked_resource_statuses`] skips one, and the stack's own events report no
    /// resource of the template.
    pub(super) fn recorded_resource_statuses(
        &self,
    ) -> BTreeMap<String, (String, Option<String>, SystemTime)> {
        let mut statuses = BTreeMap::new();
        for event in &self.events {
            if event.resource_type == STACK_RESOURCE_TYPE
                || event.resource_status.is_empty()
                || event.resource_status.ends_with(IN_PROGRESS_SUFFIX)
            {
                continue;
            }
            statuses.insert(
                event.logical_resource_id.clone(),
                (
                    event.resource_status.clone(),
                    event.resource_status_reason.clone(),
                    event.timestamp,
                ),
            );
        }
        statuses
    }

    /// The transitions the walk in flight planned for the resources it ACTED ON, oldest
    /// first — the same records that walk was built from
    /// ([`Self::schedule_resource_events`]), read back so a failure that has no transitions
    /// of its own to hand still knows what the walk left each resource in.
    ///
    /// A failure the CLOCK materialises (a resource-signal wait that ran out, a resource an
    /// owning service never got ready) is recorded with the store locked and no walk's answer
    /// beside it, while the handler that drove the walk has those answers in hand. Both leave
    /// the stack in the same state, so both install the kept resources from the same records:
    /// this walk is the one place that still says which resources the operation got to and
    /// what became of each.
    pub(super) fn walked_transitions(&self) -> Vec<provisioning::ResourceTransition> {
        let Some(walk) = self.resource_timeline.as_ref() else {
            return Vec::new();
        };
        walk.steps
            .iter()
            .flat_map(|step| {
                step.events.iter().map(|event| provisioning::ResourceTransition {
                    logical_id: step.logical_id.clone(),
                    resource_type: step.resource_type.clone(),
                    physical_resource_id: event.physical_resource_id.clone(),
                    status: event.status.clone(),
                    reason: event.reason.clone(),
                    resource_properties: event.resource_properties.clone(),
                })
            })
            .collect()
    }

    /// Open the ROLLBACK a failed create owes: the stack moves to `ROLLBACK_IN_PROGRESS`
    /// carrying the reason the create failed, and `ROLLBACK_COMPLETE` becomes due one
    /// [`STATUS_TRANSITION_DELAY`] later.
    ///
    /// "ROLLBACK — Roll back the stack creation. This is the default"
    /// (`CreateStackInput$OnFailure`), so a create whose resources an owning service
    /// refused does not stop at `CREATE_FAILED`: CloudFormation removes what the create
    /// had made and the stack reports the rolled-back status. A request that asked for the
    /// failed state to be kept (`DisableRollback`, `OnFailure=DO_NOTHING`) never reaches
    /// here — the caller said what it wanted left behind.
    ///
    /// Called AFTER [`Self::fail_transition`] recorded the create's own failure, so the
    /// event log carries the failure and then the rollback, in that order. The operation
    /// this rollback belongs to is the create's, which is what the stack already reports as
    /// its last operation, and REMOVING the resources is the caller's to drive
    /// ([`provisioning::delete`]) before the terminal status falls due.
    pub fn begin_rollback(&mut self, now: SystemTime, reason: String) {
        self.status = status::ROLLBACK_IN_PROGRESS.to_string();
        self.status_reason = Some(reason.clone());
        let operation_id = self
            .last_operation()
            .map(|operation| operation.operation_id.clone());
        let client_request_token = self.client_request_token.clone();
        self.record_event(
            now,
            status::ROLLBACK_IN_PROGRESS,
            // What the EVENT says is what the rollback is: the failure that caused it, and
            // that CloudFormation rolled the create back because that is what `OnFailure`
            // asked for — the clause the summary sentence leaves room for
            // ([`rollback_requested`]). The stack's own `StackStatusReason` keeps the
            // failure alone, which is the field a client parses to recover the set of failed
            // logical ids ([`resource_failure_summary`]).
            Some(rollback_requested(&reason)),
            operation_id.clone(),
            client_request_token.clone(),
        );
        self.pending = Some(PendingStatus {
            // The rolled-back stack keeps the reason its create failed for: that is what
            // the stack is in this status for, and a client reading it afterwards has
            // nothing else to go on.
            status: status::ROLLBACK_COMPLETE.to_string(),
            status_reason: Some(reason),
            started_at: now,
            due_at: now + STATUS_TRANSITION_DELAY,
            operation_id,
            client_request_token,
            work_in_flight: false,
        });
    }

    /// Open the ROLLBACK a failed UPDATE owes: the stack moves to
    /// `UPDATE_ROLLBACK_IN_PROGRESS` carrying the reason the update failed, and
    /// `UPDATE_ROLLBACK_COMPLETE` becomes due one [`STATUS_TRANSITION_DELAY`] later.
    ///
    /// CloudFormation "rolls back the stack to the previous stack configuration" when an
    /// update fails: the update's own deployment is ABANDONED here — a failed update
    /// installs nothing, so the stack keeps the resources, outputs and template stage its
    /// last completed operation left — and putting back in the owning services what the
    /// update had already changed there is the caller's to drive
    /// ([`provisioning::reconcile`], against the resources in force) before the terminal
    /// status falls due. A revert an owning service refuses leaves the stack
    /// `UPDATE_ROLLBACK_FAILED` ([`Self::fail_transition`]), which is the state
    /// `ContinueUpdateRollback` resumes from.
    ///
    /// Called AFTER [`Self::fail_transition`] recorded the update's own failure, so the
    /// event log carries the failure and then the rollback, in that order.
    pub fn begin_update_rollback(&mut self, now: SystemTime, reason: String) {
        // A failed update deploys nothing: its pending deployment is what the stack would
        // have reported had it completed, and the rollback is the decision not to.
        self.pending_deployment = None;
        // ...and the stack's OWN record goes back with it: the template, the parameters and
        // the rest of the revision the update replaced are the previous stack
        // configuration, and the values the failed update submitted are discarded with that
        // update ([`Self::previous_configuration`]).
        self.roll_configuration_back();
        self.status = status::UPDATE_ROLLBACK_IN_PROGRESS.to_string();
        self.status_reason = Some(reason.clone());
        let operation_id = self
            .last_operation()
            .map(|operation| operation.operation_id.clone());
        let client_request_token = self.client_request_token.clone();
        self.record_event(
            now,
            status::UPDATE_ROLLBACK_IN_PROGRESS,
            Some(reason.clone()),
            operation_id.clone(),
            client_request_token.clone(),
        );
        self.pending = Some(PendingStatus {
            status: status::UPDATE_ROLLBACK_COMPLETE.to_string(),
            // The rolled-back stack keeps the reason its update failed for: that is what
            // the stack is in this status for.
            status_reason: Some(reason),
            started_at: now,
            due_at: now + STATUS_TRANSITION_DELAY,
            operation_id,
            client_request_token,
            // The rollback's own calls are made by the pass that takes it
            // ([`super::settle_due_rollbacks`]), which holds the transition back itself
            // while they run; until then the stack owes the rollback, which already holds
            // it ([`Self::settle`]).
            work_in_flight: false,
        });
    }

    /// Resume the rollback of a failed update, leaving the resources `skipped` names
    /// exactly as the failed update made them.
    ///
    /// `ContinueUpdateRollback` "continues rolling back a stack from
    /// UPDATE_ROLLBACK_FAILED to UPDATE_ROLLBACK_COMPLETE state", and of the resources the
    /// request names: "CloudFormation sets the status of the specified resources to
    /// UPDATE_COMPLETE and continues to roll back the stack" — so each skipped resource
    /// keeps the configuration it has and takes that status, with the event that says so and
    /// the reason that says WHY it is complete without having been put back
    /// ([`RESOURCE_SKIPPED_DURING_UPDATE_ROLLBACK`]), while the stack moves to
    /// `UPDATE_ROLLBACK_IN_PROGRESS` and reaches `UPDATE_ROLLBACK_COMPLETE` one
    /// [`STATUS_TRANSITION_DELAY`] later.
    pub fn continue_update_rollback(
        &mut self,
        now: SystemTime,
        skipped: &[String],
        client_request_token: Option<String>,
    ) {
        let operation_id = new_operation_id();
        // The resumption is the stack's own operation, of the type the model names for it:
        // "continues rolling back a stack from UPDATE_ROLLBACK_FAILED to
        // UPDATE_ROLLBACK_COMPLETE state".
        self.record_operation(operation_type::CONTINUE_ROLLBACK, &operation_id, now);
        self.client_request_token = client_request_token.clone();
        self.status = status::UPDATE_ROLLBACK_IN_PROGRESS.to_string();
        // A CLIENT asked for this rollback to be resumed, so the event that opens it is
        // `USER_INITIATED` — the reason every stack operation a client starts opens with
        // ([`Self::begin_transition`]), as against the rollback this one resumes, whose own
        // opening event carries the failure that forced it.
        self.status_reason = Some(USER_INITIATED.to_string());
        self.record_event(
            now,
            status::UPDATE_ROLLBACK_IN_PROGRESS,
            Some(USER_INITIATED.to_string()),
            Some(operation_id.clone()),
            client_request_token.clone(),
        );
        for logical_id in skipped {
            let Some(deployed) = self
                .resources
                .iter_mut()
                .find(|deployed| &deployed.resource.logical_id == logical_id)
            else {
                continue;
            };
            deployed.status = resource_status::UPDATE_COMPLETE.to_string();
            deployed.status_timestamp = now;
            deployed.status_reason = Some(RESOURCE_SKIPPED_DURING_UPDATE_ROLLBACK.to_string());
            let logical_resource_id = deployed.resource.logical_id.clone();
            let physical_resource_id = deployed.resource.physical_resource_id.clone();
            let resource_type = deployed.resource.resource_type.clone();
            self.record_resource_event(
                now,
                &logical_resource_id,
                Some(physical_resource_id),
                &resource_type,
                resource_status::UPDATE_COMPLETE,
                Some(RESOURCE_SKIPPED_DURING_UPDATE_ROLLBACK.to_string()),
                None,
                Some(operation_id.clone()),
                client_request_token.clone(),
            );
        }
        self.pending = Some(PendingStatus {
            status: status::UPDATE_ROLLBACK_COMPLETE.to_string(),
            status_reason: None,
            // The resumption's own walk over the resources runs next, with this store
            // unlocked: the stack is not due its terminal status until that walk has
            // reported back.
            work_in_flight: true,
            started_at: now,
            due_at: now + STATUS_TRANSITION_DELAY,
            operation_id: Some(operation_id),
            client_request_token,
        });
    }

    /// The LAST KNOWN STABLE STATE this stack settled in, if it has one.
    ///
    /// Recorded by [`Self::settle`], the one writer of a terminal status, and judged by the
    /// one rule that says which statuses are stable ([`is_stable_status`]): a stack whose
    /// create never completed has no such state, while one whose later operation failed
    /// settled in the status its last completed operation left, which names the
    /// configuration a rollback puts back. Kept on the stack rather than read back off the
    /// event log, which is bounded and evicts oldest-first
    /// ([`MAX_EVENTS_PER_STACK`]) — a busy stack must not forget that it was ever stable.
    pub fn last_stable_status(&self) -> Option<&str> {
        self.last_stable_status.as_deref()
    }

    /// Open the ROLLBACK a client asked for on a stack whose last operation FAILED, and
    /// answer the work it owes the services that own the stack's resources.
    ///
    /// `RollbackStack` "rolls back the specified stack to the last known stable state", and
    /// "will delete a stack if it doesn't contain a last known stable state", so what the
    /// rollback is depends on whether this stack HAS one ([`Self::last_stable_status`]):
    ///
    /// * with none, there is nothing to go back to — the resources the failed operation had
    ///   provisioned are removed again and the stack heads for
    ///   [`status::ROLLBACK_COMPLETE`], exactly as the rollback of a failed create does;
    /// * with one, the configuration in force before the failed operation is put back and
    ///   the stack heads for [`status::UPDATE_ROLLBACK_COMPLETE`] — the plan being the one
    ///   the operation that failed held ([`Self::rollback_plan`], kept for this call where
    ///   that operation preserved its failed state), or, for a stack this service holds no
    ///   plan for, the resources in force declared against themselves, which asks the owning
    ///   services for no change.
    ///
    /// The calls themselves are the caller's to drive with the store unlocked
    /// ([`RequestedRollback`]), before the terminal status falls due; a revert an owning
    /// service refuses leaves the stack `UPDATE_ROLLBACK_FAILED` and a delete it refuses
    /// `ROLLBACK_FAILED`, the same as for a rollback no client asked for.
    pub fn begin_requested_rollback(
        &mut self,
        now: SystemTime,
        operation_id: String,
        client_request_token: String,
    ) -> RequestedRollback {
        // "A unique identifier for this rollback operation that can be used to track the
        // operation's progress and events": the rollback is the stack's own operation, of
        // the type the model names for it, so that is what the stack reports as its last and
        // what the events of this rollback carry.
        self.record_operation(operation_type::ROLLBACK, &operation_id, now);
        // What the operation that failed worked out it would have to put back, read BEFORE
        // the transition below retires it.
        let held = self
            .rollback_plan
            .take()
            .or_else(|| self.unreverted_rollback.clone());
        if self.last_stable_status().is_none() {
            let discarded = self.current_resources();
            self.begin_transition(
                now,
                status::ROLLBACK_IN_PROGRESS,
                status::ROLLBACK_COMPLETE,
                Some(operation_id),
                Some(client_request_token),
            );
            // The caller drives the removals with the store unlocked, so the rollback is not
            // due its terminal status until it has reported back
            // ([`Self::carrying_out_its_deployment`]).
            self.carrying_out_its_deployment();
            return RequestedRollback::Discard(discarded);
        }
        let in_force: Vec<StoredResource> = self
            .resources
            .iter()
            .map(|deployed| deployed.resource.clone())
            .collect();
        let plan = held.unwrap_or_else(|| OwedRollback {
            previous: in_force.clone(),
            applied: in_force,
            // Nothing is known about what any owning service carried out, so nothing is
            // declared again over and above what the records say
            // ([`OwedRollback::part_changed`]).
            part_changed: BTreeSet::new(),
        });
        // The failed operation's own deployment is ABANDONED, as it is for a rollback the
        // failure itself owed ([`Self::begin_update_rollback`]): the stack keeps the
        // resources, outputs and template stage its last completed operation left.
        self.pending_deployment = None;
        // ...and the configuration record of the operation that failed goes back with it, to
        // the revision in force before it — the same restore a rollback the failure itself
        // owed performs, read before the transition below opens this one.
        self.roll_configuration_back();
        self.begin_transition(
            now,
            status::UPDATE_ROLLBACK_IN_PROGRESS,
            status::UPDATE_ROLLBACK_COMPLETE,
            Some(operation_id),
            Some(client_request_token),
        );
        // ...and the revert the caller is about to ask the owning services for holds this
        // rollback's own completion back the same way.
        self.carrying_out_its_deployment();
        // This rollback is now the outstanding one: whatever an earlier one left unreverted
        // is what it is about to ask for again.
        self.unreverted_rollback = None;
        RequestedRollback::Revert(plan)
    }
}
