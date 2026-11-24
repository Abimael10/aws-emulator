// SPDX-License-Identifier: CC0-1.0

//! The calculations submitted into this account's notebook sessions.
//!
//! A calculation lives in the [`CalculationRegistry`] of
//! [`ComAmazonawsAthenaAmazonathenaState::calculations`]: `StartCalculationExecution` writes
//! a [`CalculationRecord`] there and `GetCalculationExecutionCode` answers out of that same
//! record, so the code a read reports is the code the submission persisted rather than
//! anything rebuilt from the reading request. `ListCalculationExecutions` reports the same
//! records as the summaries each one projects to ([`CalculationRecord::to_summary`]), in the
//! order they were submitted in ([`CalculationRegistry::session_summaries`]).
//!
//! WHICH SESSION may take one is the session's own lifecycle. "Submits calculations for
//! execution within a session" (`StartCalculationExecution`), and a session takes one while
//! it is running — "IDLE - The session is able to accept a calculation" — so a submission
//! addressed to a session this account holds none of, or to one whose resources are on their
//! way out or gone, is refused in the sentence every session-addressed request this service
//! will not act on is refused with ([`SESSION_NOT_FOUND`]). Whether that session was started
//! in a notebook ([`NOTEBOOK_ID_CONFIG`]) decides which notebook a calculation is a cell of,
//! and not whether the session runs calculations at all: a session started for programmatic
//! access takes them exactly as a notebook's session does.
//!
//! A submitted calculation RUNS: the code block is evaluated by the Python engine this crate
//! embeds ([`crate::state::calculation_engine`]), bounded and contained, and what that run
//! produced is stored on the record as its [`CalculationOutcome`] — the two output streams,
//! saved as objects under the session's working directory ([`calculation_result_objects`]),
//! and the time the host measured the run taking. So a read reports a result and a duration
//! for work that was really performed, and a calculation whose run reported nothing stays
//! RUNNING however long ago it was submitted.
//!
//! The STATES a calculation walks are the clock's: "CREATED - The calculation has been created
//! and is ready to run" is what the submission answers, "RUNNING - The calculation is running"
//! follows ([`CALCULATION_CREATED`]), and the terminal state its outcome names falls due at
//! [`CALCULATION_COMPLETION`] — so every read derives the state from the stored instants
//! rather than a reader having to move the record on.
//!
//! A request to cancel moves it instead: "Requests the cancellation of a calculation"
//! (`StopCalculationExecution`) stores the instant it was asked at
//! ([`CalculationRecord::stop`]), from which the calculation is `CANCELING` — "a request to
//! cancel the calculation has been received and the system is working to stop it" — and then
//! `CANCELED` once that stop is due ([`CALCULATION_CANCELLATION`]). A calculation already in a
//! terminal state takes no cancellation, which is the "succeeds but has no effect" that
//! operation documents.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, SystemTime};

use super::*;

use crate::ops::types::{
    CalculationResult, CalculationStatistics, CalculationStatus, CalculationSummary,
};

/// The `CalculationExecutionState` of a calculation that has been created: "CREATED - The
/// calculation has been created and is ready to run"
/// (`StartCalculationExecutionResponse$State`). One of
/// `CALCULATION_EXECUTION_STATE_VALUES` in [`crate::ops::types`].
pub const CALCULATION_STATE_CREATED: &str = "CREATED";

/// The `CalculationExecutionState` of a calculation whose cancellation has been asked for:
/// "CANCELING - A request to cancel the calculation has been received and the system is
/// working to stop it" (`CalculationStatus$State`). See [`CALCULATION_STATE_CREATED`].
pub const CALCULATION_STATE_CANCELING: &str = "CANCELING";

/// The `CalculationExecutionState` of a calculation that was cancelled: "CANCELED - The
/// calculation is no longer running as the result of a cancel request"
/// (`CalculationStatus$State`). See [`CALCULATION_STATE_CREATED`].
pub const CALCULATION_STATE_CANCELED: &str = "CANCELED";

/// The `CalculationExecutionState` of a calculation that is running: "RUNNING - The
/// calculation is running" (`CalculationStatus$State`). See [`CALCULATION_STATE_CREATED`].
pub const CALCULATION_STATE_RUNNING: &str = "RUNNING";

/// The `CalculationExecutionState` of a calculation that "has completed without error"
/// (`CalculationStatus$State`). See [`CALCULATION_STATE_CREATED`].
pub const CALCULATION_STATE_COMPLETED: &str = "COMPLETED";

/// The `CalculationExecutionState` of a calculation that "failed and is no longer running"
/// (`CalculationStatus$State`). See [`CALCULATION_STATE_CREATED`].
pub const CALCULATION_STATE_FAILED: &str = "FAILED";

/// How long after a calculation is submitted its completion is DUE.
///
/// Athena hands back a calculation in a non-terminal state and the terminal state arrives
/// later, so this is a transition the CLOCK owes: the instant is stored on the record and
/// every read derives the state from it. The span is this emulator's, not a duration AWS
/// publishes (a real calculation takes as long as the Spark job it is), and it is never zero —
/// a caller reading straight back must still observe the transient states, exactly as a
/// submitted query's completion is owed ([`QUERY_EXECUTION_COMPLETION`]).
pub const CALCULATION_COMPLETION: Duration = Duration::from_secs(3);

/// How long a calculation stays [`CALCULATION_STATE_CREATED`] before it is
/// [`CALCULATION_STATE_RUNNING`]: the first half of [`CALCULATION_COMPLETION`], so both
/// non-terminal states this service reports are observable and neither is skipped — the same
/// division a query execution's own two non-terminal states are owed under
/// ([`QUERY_EXECUTION_QUEUED`]).
pub const CALCULATION_CREATED: Duration = Duration::from_millis(1_500);

/// How long after a calculation is asked to stop the cancellation is complete.
///
/// The EMULATOR's own span, the same kind [`SESSION_TERMINATION`] is: AWS publishes no
/// duration for stopping a calculation ("Cancelling a calculation is done on a best effort
/// basis"), and it is never zero, so the documented `CANCELING` state a cancelled calculation
/// passes through is observable rather than skipped.
pub const CALCULATION_CANCELLATION: Duration = Duration::from_secs(3);

/// How many calculations this emulator retains for one account.
///
/// Athena publishes no such limit — a session's calculation history is a service-side
/// record — so this is the EMULATOR's own bound, kept so a long-running process holds a
/// bounded number of code blocks rather than every calculation ever submitted. The OLDEST
/// calculation is dropped once this many are held, exactly as the session registry and the
/// query-execution history are bounded ([`MAX_SESSIONS`], [`MAX_QUERY_EXECUTIONS`]). What a
/// caller can make the registry hold is therefore this count times the modelled `CodeBlock`
/// length (0..68000), whatever the submissions carry.
pub const MAX_CALCULATIONS: usize = 200;

/// One calculation submitted into a session, as this service persists it.
#[derive(Debug, Clone)]
pub struct CalculationRecord {
    /// The `CalculationExecutionId` this calculation was minted under, which is also its
    /// key in the registry.
    pub id: String,
    /// The `SessionId` of the session the calculation was submitted into: "The session ID
    /// that the calculation ran in" (`GetCalculationExecutionResponse$SessionId`).
    pub session_id: String,
    /// The workgroup that session runs in, held so a delete of the workgroup takes the
    /// calculations of its sessions with it.
    pub work_group: String,
    /// The notebook the session serves, which is the notebook this calculation is a cell of
    /// — `None` for a calculation submitted into a session started for programmatic access,
    /// which is a cell of no notebook (see the module docs).
    pub notebook_id: Option<String>,
    /// "A description of the calculation" (`StartCalculationExecutionRequest$Description`),
    /// as the submission carried it.
    pub description: Option<String>,
    /// "A string that contains the code of the calculation"
    /// (`StartCalculationExecutionRequest$CodeBlock`), which is what
    /// `GetCalculationExecutionCode` answers with.
    pub code_block: String,
    /// When the calculation was submitted, reported as `CalculationStatus$
    /// SubmissionDateTime`.
    pub submitted_at: SystemTime,
    /// When this calculation's completion falls DUE — [`CALCULATION_COMPLETION`] after it was
    /// submitted. It is [`CALCULATION_STATE_RUNNING`] until then and terminal from then on.
    pub completion_due_at: SystemTime,
    /// What RUNNING the code block produced, or `None` for a calculation whose run has
    /// reported nothing yet — which keeps it running however long ago it was submitted.
    pub outcome: Option<CalculationOutcome>,
    /// When the calculation was asked to stop, if it was, from which the
    /// [`CALCULATION_CANCELLATION`] span runs.
    pub stop_requested_at: Option<SystemTime>,
    /// "The reason for the calculation state change" (`CalculationStatus$StateChangeReason`),
    /// for a state change this service made on its own rather than at the caller's request:
    /// the cancellation the termination of the calculation's SESSION forces
    /// ([`CALCULATION_CANCELED_WITH_SESSION`]). `None` for every state the clock derives and
    /// for a cancellation the caller asked for itself, neither of which Athena states a reason
    /// for.
    pub state_change_reason: Option<String>,
}

/// What RUNNING one calculation produced: the reading the engine took of the run
/// ([`crate::state::calculation_engine`]), and where the objects it wrote were saved.
///
/// Every field is a reading of work that was really performed: the engine ran the code block,
/// the host measured how long it took, and the two streams were saved to the session's
/// working directory as the objects named here. Nothing in it is derived from the submitted
/// text.
#[derive(Debug, Clone)]
pub struct CalculationOutcome {
    /// Whether the run FAILED: the code raised, or a bound this emulator holds a run to
    /// stopped it.
    pub failed: bool,
    /// How long the host spent running it, which is what the DPU execution time a read
    /// reports is measured as (see [`CalculationRecord::to_statistics`]).
    pub run_time: Duration,
    /// "The Amazon S3 location of the `stdout` file for the calculation"
    /// (`CalculationResult$StdOutS3Uri`), or `None` where the run's output could not be
    /// saved — a session with no working directory to write under.
    pub stdout_uri: Option<String>,
    /// "The Amazon S3 location of the `stderr` error messages file for the calculation"
    /// (`CalculationResult$StdErrorS3Uri`); `None` for the same reason.
    pub stderr_uri: Option<String>,
    /// "The Amazon S3 location of the folder for the calculation results"
    /// (`CalculationResult$ResultS3Uri`), which is the folder the two objects above were
    /// written under; `None` for the same reason.
    pub result_uri: Option<String>,
}

impl CalculationRecord {
    /// The record a submission of `id` into `session_id` at `now` registers: its completion
    /// owed, its run not reported yet, and no cancellation asked for.
    ///
    /// Everything else the submission carried is set by the caller on the record this hands
    /// back, so one calculation is built in one place.
    pub fn submitted(
        id: String,
        session_id: String,
        work_group: String,
        code_block: String,
        now: SystemTime,
    ) -> Self {
        Self {
            id,
            session_id,
            work_group,
            notebook_id: None,
            description: None,
            code_block,
            submitted_at: now,
            completion_due_at: due_at(now, CALCULATION_COMPLETION),
            outcome: None,
            stop_requested_at: None,
            state_change_reason: None,
        }
    }

    /// When this calculation stops being [`CALCULATION_STATE_CREATED`].
    fn running_due_at(&self) -> SystemTime {
        self.submitted_at
            .checked_add(CALCULATION_CREATED)
            .unwrap_or(self.completion_due_at)
    }

    /// The `CalculationExecutionState` this calculation reads as at `now`.
    ///
    /// DERIVED, never written: a calculation walks CREATED, then RUNNING, then the terminal
    /// state its stored outcome names — and stays RUNNING while its run has reported no
    /// outcome at all, because a calculation whose work is not over has not reached a terminal
    /// state however long ago it was submitted. A cancelled one is `CANCELING` until that stop
    /// is due and `CANCELED` from then on. So two reads at the same instant report the same
    /// state, and a calculation nobody polls reaches its state exactly when one that is polled
    /// does.
    pub fn state_at(&self, now: SystemTime) -> &'static str {
        if let Some(requested) = self.stop_requested_at {
            return if now < due_at(requested, CALCULATION_CANCELLATION) {
                CALCULATION_STATE_CANCELING
            } else {
                CALCULATION_STATE_CANCELED
            };
        }
        if now < self.running_due_at() {
            return CALCULATION_STATE_CREATED;
        }
        let Some(outcome) = self.outcome.as_ref() else {
            return CALCULATION_STATE_RUNNING;
        };
        if now < self.completion_due_at {
            return CALCULATION_STATE_RUNNING;
        }
        if outcome.failed {
            CALCULATION_STATE_FAILED
        } else {
            CALCULATION_STATE_COMPLETED
        }
    }

    /// Whether the calculation is in a state it can no longer leave at `now`: "a
    /// `StopCalculationExecution` call on a calculation that is already in a terminal state
    /// […] succeeds but has no effect".
    pub fn is_terminal_at(&self, now: SystemTime) -> bool {
        matches!(
            self.state_at(now),
            CALCULATION_STATE_CANCELED | CALCULATION_STATE_COMPLETED | CALCULATION_STATE_FAILED
        )
    }

    /// When the calculation stopped processing, or `None` while it has not: "the date and time
    /// the calculation completed processing" (`CalculationStatus$CompletionDateTime`), which
    /// is the instant a cancellation finished for one that was cancelled and the instant its
    /// completion fell due for one that ran.
    pub fn completed_at(&self, now: SystemTime) -> Option<SystemTime> {
        if let Some(requested) = self.stop_requested_at {
            let complete = due_at(requested, CALCULATION_CANCELLATION);
            return (now >= complete).then_some(complete);
        }
        (self.outcome.is_some() && now >= self.completion_due_at).then_some(self.completion_due_at)
    }

    /// The `CalculationStatus` a read of this calculation reports at `now`.
    ///
    /// `SubmissionDateTime` is "the date and time the calculation was submitted for
    /// processing", which is the instant the submission persisted, and `CompletionDateTime`
    /// the instant it stopped processing ([`Self::completed_at`]). The `StateChangeReason` is
    /// the one the record holds: AWS publishes no wording for the change a caller's own cancel
    /// makes — the member's own example is a calculation "canceled because the session was
    /// terminated", which is the change this service does state a reason for
    /// ([`CALCULATION_CANCELED_WITH_SESSION`]) — and what a FAILED run reports is the error the
    /// run itself produced, which is the `stderr` object its result names.
    ///
    /// Built on the record rather than in a handler, so every read of a calculation's status
    /// reports the same members out of the same record.
    pub fn to_status(&self, now: SystemTime) -> CalculationStatus {
        CalculationStatus {
            submission_date_time: Some(reported_time(self.submitted_at)),
            completion_date_time: self.completed_at(now).map(reported_time),
            state: Some(self.state_at(now).to_string()),
            state_change_reason: self.state_change_reason.clone(),
        }
    }

    /// The `CalculationResult` a read reports at `now`: "contains result information. This
    /// field is populated only if the calculation is COMPLETED".
    ///
    /// So a calculation that is still running, one that was cancelled and one that failed
    /// report none, and a completed one reports the objects its run really wrote — the two
    /// streams, the folder they are under, and the data format they are in: "the data format
    /// of the calculation result" is the media type the result was saved as, which for the
    /// output of a calculation is the text those objects hold
    /// ([`CALCULATION_OUTPUT_CONTENT_TYPE`]), and the modelled `CalculationResultType` is a
    /// media type (its pattern is `^\w+\/[-+.\w]+$`).
    pub fn to_result(&self, now: SystemTime) -> Option<CalculationResult> {
        if self.state_at(now) != CALCULATION_STATE_COMPLETED {
            return None;
        }
        let outcome = self.outcome.as_ref()?;
        Some(CalculationResult {
            std_out_s3_uri: outcome.stdout_uri.clone(),
            std_error_s3_uri: outcome.stderr_uri.clone(),
            result_s3_uri: outcome.result_uri.clone(),
            result_type: outcome
                .result_uri
                .as_ref()
                .map(|_| CALCULATION_OUTPUT_CONTENT_TYPE.to_string()),
        })
    }

    /// The `CalculationStatistics` a read reports at `now`: "contains information about the
    /// data processing unit (DPU) execution time and progress. This field is populated only
    /// when statistics are available".
    ///
    /// They are available once the run has reported and the calculation is terminal, and what
    /// they carry is the HOST's own measurement of that run: "the data processing unit
    /// execution time in milliseconds for the calculation" is the time the one executor a
    /// session runs on spent on it ([`ExecutorRecord`]), which is the time the engine spent
    /// evaluating the code block. `Progress` is not reported: AWS publishes no vocabulary for
    /// the member, and a run that is over has no progress left to state.
    pub fn to_statistics(&self, now: SystemTime) -> Option<CalculationStatistics> {
        if !self.is_terminal_at(now) {
            return None;
        }
        let outcome = self.outcome.as_ref()?;
        Some(CalculationStatistics {
            dpu_execution_in_millis: i64::try_from(outcome.run_time.as_millis()).ok(),
            progress: None,
        })
    }

    /// This calculation as the `CalculationSummary` a listing reports it as: "Summary
    /// information for a notebook calculation" — the id it was minted under, the description
    /// the submission carried (absent where it carried none, the modelled `DescriptionString`
    /// holding at least one character) and its status.
    pub fn to_summary(&self, now: SystemTime) -> CalculationSummary {
        CalculationSummary {
            calculation_execution_id: Some(self.id.clone()),
            description: self.description.clone(),
            status: Some(self.to_status(now)),
        }
    }

    /// Report what RUNNING this calculation produced, as of `now`.
    ///
    /// The one place a run's outcome is written. The completion is pushed out to
    /// [`CALCULATION_COMPLETION`] after the instant the run REPORTED where that is later than
    /// the instant it was already owed at: the engine runs the code block before the
    /// submission is answered, so a run that took longer than the span the submission owed
    /// would otherwise land a calculation in a terminal state before its caller had the id —
    /// and a caller acting straight after the submission must still find the calculation in
    /// flight, whatever the code block it carried made the host spend.
    pub fn report(&mut self, outcome: CalculationOutcome, now: SystemTime) {
        self.completion_due_at = due_at(now, CALCULATION_COMPLETION).max(self.completion_due_at);
        self.outcome = Some(outcome);
    }

    /// Ask this calculation to stop at `now`, which is what `StopCalculationExecution`
    /// "requests", and answer whether the cancellation was taken on.
    ///
    /// "A `StopCalculationExecution` call on a calculation that is already in a terminal state
    /// (for example, STOPPED, FAILED, or COMPLETED) succeeds but has no effect": such a
    /// calculation keeps the state it reached. One that is still CREATED or RUNNING takes the
    /// cancellation, and a calculation already asked keeps the instant it was FIRST asked at,
    /// so a repeat neither restarts the cancellation nor moves the instant it completed at —
    /// the same no-op a repeated session shutdown is ([`SessionRecord::terminate`]).
    pub fn stop(&mut self, now: SystemTime) -> bool {
        if self.is_terminal_at(now) {
            return false;
        }
        self.stop_requested_at = Some(self.stop_requested_at.unwrap_or(now));
        true
    }

    /// Cancel this calculation at `now` because the SESSION it runs in was terminated, and
    /// answer whether the cancellation was taken on.
    ///
    /// The same cancellation a caller's own stop asks for ([`Self::stop`]) — so a calculation
    /// already in a terminal state keeps the state it reached, and one already asked keeps the
    /// instant it was first asked at — carrying the reason for the change this service made on
    /// its own: "the calculation was canceled because the session was terminated"
    /// (`CalculationStatus$StateChangeReason`).
    pub fn cancel_with_session(&mut self, now: SystemTime) -> bool {
        if !self.stop(now) {
            return false;
        }
        self.state_change_reason = Some(CALCULATION_CANCELED_WITH_SESSION.to_string());
        true
    }

    /// Whether this record is the calculation a repeat of `session_id`, `code_block` and
    /// `description` describes: what an `IdempotencyToken` repeated for the SAME submission
    /// answers with, as against one repeated for another ("If another
    /// StartCalculationExecutionRequest is received, the same response is returned and
    /// another calculation is not created. If a parameter has changed, an error is
    /// returned").
    ///
    /// Every parameter the submission persisted takes part, and the session is one of them:
    /// the same code block submitted into another session is another calculation. The ONE
    /// place that comparison is made, so a parameter added to a submission cannot be weighed
    /// on one path and skipped on another.
    pub fn matches_submission(
        &self,
        session_id: &str,
        code_block: &str,
        description: Option<&str>,
    ) -> bool {
        self.session_id == session_id
            && self.code_block == code_block
            && self.description.as_deref() == description
    }
}

/// Every calculation this account holds, with the idempotency tokens that already minted
/// one.
///
/// A struct of its own rather than loose fields on the store, so the bound above is applied
/// in the ONE place a calculation is written ([`CalculationRegistry::insert`]) and cannot be
/// honoured on one operation's path and skipped on another's.
#[derive(Debug, Default)]
pub struct CalculationRegistry {
    /// The calculations, keyed by `CalculationExecutionId`. A `BTreeMap` so a walk of the
    /// registry is in one stable order.
    calculations: BTreeMap<String, CalculationRecord>,
    /// The ids in submission order, oldest first — the order calculations are dropped in
    /// once [`MAX_CALCULATIONS`] are held, which is why the oldest is taken from the front.
    order: VecDeque<String>,
    /// Which `CalculationExecutionId` each `ClientRequestToken` already minted, so the
    /// documented idempotency is answered from state rather than by submitting a second
    /// calculation.
    request_tokens: BTreeMap<String, String>,
}

impl CalculationRegistry {
    /// The calculation `id` names, or `None` when this account holds none.
    pub fn calculation(&self, id: &str) -> Option<&CalculationRecord> {
        self.calculations.get(id)
    }

    /// The same calculation, for a caller that is going to move it on (a cancellation).
    pub fn calculation_mut(&mut self, id: &str) -> Option<&mut CalculationRecord> {
        self.calculations.get_mut(id)
    }

    /// The calculation a `ClientRequestToken` already minted, or `None` when no submission
    /// carried it.
    pub fn calculation_of_token(&self, token: &str) -> Option<&CalculationRecord> {
        self.calculations.get(self.request_tokens.get(token)?)
    }

    /// How many calculations are held, the count [`MAX_CALCULATIONS`] bounds.
    pub fn len(&self) -> usize {
        self.calculations.len()
    }

    /// Whether no calculation is held at all.
    pub fn is_empty(&self) -> bool {
        self.calculations.is_empty()
    }

    /// Store `record`, under `token` when the submission carried one.
    ///
    /// The oldest calculation is dropped once [`MAX_CALCULATIONS`] are held, together with
    /// any token that named it: a token whose calculation is gone cannot answer with the
    /// same id, so it is not kept pointing at nothing. The ONE place a calculation enters
    /// the registry, so the token index cannot come to name a calculation the map does not
    /// hold.
    pub fn insert(&mut self, record: CalculationRecord, token: Option<String>) {
        let id = record.id.clone();
        // An id the registry already holds is re-stored in place rather than queued a second
        // time, so the order index names exactly the calculations the map holds and the bound
        // below is the count a read reports.
        self.order.retain(|held| held != &id);
        while self.order.len() >= MAX_CALCULATIONS {
            if let Some(oldest) = self.order.pop_front() {
                self.calculations.remove(&oldest);
                self.request_tokens.retain(|_, minted| minted != &oldest);
            }
        }
        if let Some(token) = token {
            self.request_tokens.insert(token, id.clone());
        }
        self.order.push_back(id.clone());
        self.calculations.insert(id, record);
    }

    /// The calculations submitted into the session `session_id`, newest first, as the
    /// summaries a listing reports.
    ///
    /// The order `ListCalculationExecutions` documents: "Lists the calculations that have
    /// been submitted to a session in descending order. Newer calculations are listed first;
    /// older calculations are listed later." Submission order is what this registry's own
    /// index already records, so the walk reads that index backwards rather than re-sorting
    /// the submission instants, which two submissions inside one tick of the clock could not
    /// separate.
    pub fn session_summaries(&self, session_id: &str, now: SystemTime) -> Vec<CalculationSummary> {
        self.order
            .iter()
            .rev()
            .filter_map(|id| self.calculations.get(id))
            .filter(|record| record.session_id == session_id)
            .map(|record| record.to_summary(now))
            .collect()
    }

    /// Remove every calculation submitted into a session of `work_group`, as a delete of
    /// that workgroup does, and answer with how many were removed.
    ///
    /// A calculation is addressed inside the session it ran in, and a session goes with its
    /// workgroup ([`SessionRegistry::remove_work_group`]), so one whose workgroup is gone
    /// could never be read again.
    pub fn remove_work_group(&mut self, work_group: &str) -> usize {
        let removed: Vec<String> = self
            .calculations
            .values()
            .filter(|record| record.work_group == work_group)
            .map(|record| record.id.clone())
            .collect();
        for id in &removed {
            self.calculations.remove(id);
        }
        self.order.retain(|held| !removed.contains(held));
        self.request_tokens
            .retain(|_, minted| !removed.iter().any(|id| id == minted));
        removed.len()
    }

    /// Cancel every calculation of the session `session_id` that is not terminal at `now`, as
    /// the TERMINATION of that session does, and answer with how many were cancelled.
    ///
    /// A calculation runs inside its session, so a session whose resources are being torn down
    /// runs none: the work in flight is cancelled rather than left to settle on its own, which
    /// is the change `CalculationStatus$StateChangeReason` names in its own example. One
    /// already in a terminal state is left exactly as it is
    /// ([`CalculationRecord::cancel_with_session`]), so a repeat of the shutdown changes
    /// nothing.
    pub fn cancel_session(&mut self, session_id: &str, now: SystemTime) -> usize {
        self.calculations
            .values_mut()
            .filter(|record| record.session_id == session_id)
            .filter(|record| !record.is_terminal_at(now))
            .map(|record| usize::from(record.cancel_with_session(now)))
            .sum()
    }
}

/// The cursor a `ListCalculationExecutions` page hands back: the session whose calculations
/// it is listing and the position to resume at.
///
/// Scoped by session for the reason every other cursor of this service is scoped by what its
/// listing was narrowed to: a token minted while listing one session's calculations names no
/// page of another session's listing.
fn calculations_cursor(session_id: &str, position: usize) -> String {
    format!("calculations{CURSOR_SEPARATOR}{session_id}{CURSOR_SEPARATOR}{position}")
}

impl ComAmazonawsAthenaAmazonathenaState {
    /// The `NextToken` a `ListCalculationExecutions` page of session `session_id` resumed at
    /// `position` hands back.
    pub fn calculations_page_token(&self, session_id: &str, position: usize) -> String {
        ::hex::encode(
            self.page_token_key
                .mint(&calculations_cursor(session_id, position)),
        )
    }

    /// The position `token` resumes a calculation listing of session `session_id` at, or
    /// `None` for a token this store did not mint for THAT session's listing.
    pub fn calculations_page_position(&self, session_id: &str, token: &str) -> Option<usize> {
        let cursor = self.page_cursor(token)?;
        cursor_position(&cursor, "calculations", session_id)
    }
}

/// A fresh `CalculationExecutionId`: "The calculation execution UUID"
/// (`GetCalculationExecutionRequest$CalculationExecutionId`), a version-4 UUID in its
/// hyphenated form, which is inside the modelled `CalculationExecutionId` length (1..36).
/// Minted from the maintained crate rather than laid out here.
pub fn new_calculation_execution_id() -> String {
    ::uuid::Uuid::new_v4().as_hyphenated().to_string()
}

/// The name of the object one calculation's standard output is saved as, which is the file
/// `CalculationResult$StdOutS3Uri` names ("the Amazon S3 location of the `stdout` file for the
/// calculation").
const STDOUT_OBJECT: &str = "stdout";

/// The name of the object one calculation's standard error is saved as; see
/// [`STDOUT_OBJECT`].
const STDERR_OBJECT: &str = "stderr";

/// The media type the two output objects of a calculation are saved under, which is also the
/// "data format of the calculation result" a read reports
/// ([`CalculationRecord::to_result`]): what a calculation wrote to a stream is text.
pub const CALCULATION_OUTPUT_CONTENT_TYPE: &str = "text/plain";

/// The folder one calculation's output objects are saved under: the session's working
/// directory, the session, and the calculation itself.
///
/// The ONE place that folder is formed, as [`result_object_uri`] is the one place a query's
/// result object key is formed — so the folder a run WRITES under and the folder a read of
/// the calculation NAMES (`CalculationResult$ResultS3Uri`) can never be two different places.
/// `None` for a session with no working directory to write under, which is the one case a
/// completed calculation reports no result location rather than one that holds nothing.
pub fn calculation_output_folder(
    working_directory: Option<&str>,
    session_id: &str,
    calculation_execution_id: &str,
) -> Option<String> {
    // The working directory names a directory, whether or not it was written with the
    // trailing separator, so what goes under it is joined by exactly one.
    let directory = working_directory?.trim_end_matches('/');
    Some(format!("{directory}/{session_id}/{calculation_execution_id}"))
}

/// What a finished RUN of the calculation `calculation_execution_id` leaves: the objects to
/// save under the session's working directory, and the outcome to store on the record.
///
/// The bodies are the two streams the run produced ([`crate::state::calculation_engine`]) and
/// nothing else, so an object a read names holds what the calculation wrote. A session with no
/// working directory saves nothing — there is nowhere to save it to — and the outcome then
/// names no location, which is what a read of such a calculation reports.
pub fn calculation_result_objects(
    working_directory: Option<&str>,
    session_id: &str,
    calculation_execution_id: &str,
    run: &calculation_engine::Completion,
) -> (Vec<query_results::ResultObject>, CalculationOutcome) {
    let outcome = CalculationOutcome {
        failed: run.failed,
        run_time: run.run_time,
        stdout_uri: None,
        stderr_uri: None,
        result_uri: None,
    };
    let Some(folder) =
        calculation_output_folder(working_directory, session_id, calculation_execution_id)
    else {
        return (Vec::new(), outcome);
    };
    let objects = vec![
        query_results::ResultObject {
            uri: format!("{folder}/{STDOUT_OBJECT}"),
            body: run.stdout.clone().into_bytes(),
            content_type: CALCULATION_OUTPUT_CONTENT_TYPE,
        },
        query_results::ResultObject {
            uri: format!("{folder}/{STDERR_OBJECT}"),
            body: run.stderr.clone().into_bytes(),
            content_type: CALCULATION_OUTPUT_CONTENT_TYPE,
        },
    ];
    let outcome = CalculationOutcome {
        stdout_uri: Some(format!("{folder}/{STDOUT_OBJECT}")),
        stderr_uri: Some(format!("{folder}/{STDERR_OBJECT}")),
        result_uri: Some(folder),
        ..outcome
    };
    (objects, outcome)
}

/// The message a submission that names neither code block, or both, is refused with.
///
/// The rule `StartCalculationExecution` documents: "The request syntax requires the
/// StartCalculationExecutionRequest$CodeBlock parameter or the
/// CalculationConfiguration$CodeBlock parameter, but not both." The sentence names the two
/// members the rule is about and nothing from the request — the code itself is never quoted
/// back — and it is worded here once, the way the other either-or requirement of this
/// service is ([`NOTEBOOK_SOURCE_REQUIRED`]).
pub const CALCULATION_CODE_REQUIRED: &str =
    "Either CodeBlock or CalculationConfiguration.CodeBlock must be specified, but not both.";

/// The message a request addressing a calculation this service will not act on is refused
/// with.
///
/// The SAME sentence a session-addressed request is refused with ([`SESSION_NOT_FOUND`]),
/// named here for the resource this service resolves rather than spelled a second time: a
/// calculation is addressed inside the session it was submitted into — "Submits calculations
/// for execution within a session" — and Athena refuses a request for one it will not act on
/// in fixed text that quotes the id back no more than a session's does.
pub const CALCULATION_NOT_FOUND: &str = SESSION_NOT_FOUND;

/// The message a submission into a session this account DOES hold, but which is not in the
/// state a calculation is taken in, is refused with.
///
/// The rule `SessionState` documents: "IDLE - The session is able to accept a calculation",
/// so a session in any other state accepts none — one still acquiring its resources, one
/// shutting down, one whose resources are gone. Athena states that as the state the session is
/// in beside the state it would have to be in: the fixed text is everything but the state
/// name, which is the session's CURRENT state read from this service's own state
/// ([`SessionRecord::state_at`]), and the state required is always
/// [`SESSION_STATE_IDLE`].
///
/// Distinct from [`SESSION_NOT_FOUND`] on purpose: that sentence is for an id this service
/// will not act on at all, and says nothing about whether anything holds it, while this one
/// names a session the caller does hold and the state it is in — the difference between
/// "start another session" and "you may not address this one".
pub fn session_not_accepting_calculations(state: &str) -> String {
    format!("Session is in the {state} state; needs to be {SESSION_STATE_IDLE} to accept Calculations.")
}

/// The `StateChangeReason` a calculation cancelled by the TERMINATION of its session reports.
///
/// The one change `CalculationStatus$StateChangeReason` documents in its own words — "the
/// reason for the calculation state change (for example, the calculation was canceled because
/// the session was terminated)" — stated as the sentence that reason is, with no part of any
/// request in it: which session it was is what the calculation's own `SessionId` reports.
pub const CALCULATION_CANCELED_WITH_SESSION: &str =
    "The calculation was canceled because the session was terminated.";

/// The refusal a request addressing ONE calculation this service will not act on is
/// answered with: the SAME refusal a session-addressed request takes
/// ([`session_not_authorized`]), named here for the resource the operation resolves — a
/// calculation is addressed inside the session it was submitted into, so Athena states the
/// same class and the same sentence for both.
pub fn calculation_not_authorized<E>() -> crate::scaffolded::Scaffolded<E> {
    session_not_authorized()
}
