// SPDX-License-Identifier: CC0-1.0

//! The sessions this account's workgroups run, the executors each session runs on, and the
//! notebook a notebook session was started in.
//!
//! A session lives in the [`SessionRegistry`] of
//! [`ComAmazonawsAthenaAmazonathenaState::sessions`]: `StartSession` writes a
//! [`SessionRecord`] there and `GetSession`, `GetSessionStatus`, `ListSessions`,
//! `ListExecutors` and `ListNotebookSessions` all answer out of those same records, so every
//! value a read reports is one the start persisted or one derived from the clock — never
//! anything rebuilt from the reading request.
//!
//! A session HAS a lifecycle the clock owes, and `SessionStatus$State` documents it:
//! "CREATING - The session is being started, including acquiring resources", "IDLE - The
//! session is able to accept a calculation", "TERMINATING - The session is in the process
//! of shutting down", "TERMINATED - The session and its resources are no longer running".
//! So a start stores WHEN those resources are due ([`SessionRecord::resources_due_at`]) and
//! every read derives the state from that instant ([`SessionRecord::state_at`]) rather than
//! a reader having to move the record on; a shutdown is owed the same kind of span, which
//! is what makes the documented TERMINATING state observable instead of skipped.
//!
//! The EXECUTORS are the session's own resources rather than a second registry: a session
//! runs on a coordinator ("A coordinator is a special executor that orchestrates processing
//! work and manages other executors in a notebook session", `EngineConfiguration$
//! CoordinatorDpuSize`), which is started with the session and comes up over the same span,
//! so `ListExecutors` reports what the start provisioned and each executor's state follows
//! its session's own.
//!
//! Those four are the states the stored instants can yield. The others the enum carries
//! follow from something else: BUSY from a calculation running ON the session ("the session
//! is processing another task and is unable to accept a calculation"), and CREATED, DEGRADED
//! and FAILED from outcomes nothing in this service produces — so a session a calculation
//! was submitted into still reads IDLE, because nothing in this service RUNS that
//! calculation (see [`CalculationRegistry`]), and a session is BUSY only while one is
//! running on it. [`ACTIVE_SESSION_STATES`] still names the four states
//! `ListNotebookSessions` documents, because the filter a listing applies is the documented
//! one rather than a list of what this projection happens to emit.
//!
//! WHICH NOTEBOOK a session belongs to is carried by the request that starts it rather than
//! by the session's own identity. `EngineConfiguration$AdditionalConfigs` documents the
//! binding: "To specify an Athena notebook that the Jupyter server will download and serve,
//! specify a value for the StartSessionRequest$NotebookVersion field, and then add a key
//! named NotebookId to AdditionalConfigs that has the value of the Athena notebook ID" — so
//! [`NOTEBOOK_ID_CONFIG`] is the key that binds a session to a notebook, and a session
//! started without it belongs to no notebook and is reported by no notebook's listing.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use super::*;

use crate::ops::types::{
    EngineConfiguration, EngineVersion, ExecutorsSummary, MonitoringConfiguration,
    NotebookSessionSummary, SessionConfiguration, SessionStatus, SessionSummary,
};

/// The `SessionState` of a session whose resources are still being acquired: "CREATING -
/// The session is being started, including acquiring resources"
/// (`SessionStatus$State`). One of `SESSION_STATE_VALUES` in [`crate::ops::types`].
pub const SESSION_STATE_CREATING: &str = "CREATING";
/// The `SessionState` of a session that has been started; see
/// [`SESSION_STATE_CREATING`].
pub const SESSION_STATE_CREATED: &str = "CREATED";
/// The `SessionState` of a started session that can take a calculation: "IDLE - The
/// session is able to accept a calculation"; see [`SESSION_STATE_CREATING`].
pub const SESSION_STATE_IDLE: &str = "IDLE";
/// The `SessionState` of a session running a calculation: "BUSY - The session is
/// processing another task and is unable to accept a calculation"; see
/// [`SESSION_STATE_CREATING`].
pub const SESSION_STATE_BUSY: &str = "BUSY";
/// The `SessionState` of a session that is shutting down; see
/// [`SESSION_STATE_CREATING`].
pub const SESSION_STATE_TERMINATING: &str = "TERMINATING";
/// The `SessionState` of a session whose resources are gone: "TERMINATED - The session
/// and its resources are no longer running"; see [`SESSION_STATE_CREATING`].
pub const SESSION_STATE_TERMINATED: &str = "TERMINATED";

/// The session states that count as ACTIVE.
///
/// The two session listings name them in the same words: `ListSessions` "Lists the sessions
/// in a workgroup that are in an active state like CREATING, CREATED, IDLE, or BUSY" and
/// `ListNotebookSessions` "Lists, in descending order, the sessions that have been created
/// in a notebook that are in an active state like CREATING, CREATED, IDLE or BUSY" — so a
/// session in any other state (one shutting down, one whose resources are gone, or one that
/// ended badly) is part of neither listing, though `GetSession` still reads it.
pub const ACTIVE_SESSION_STATES: &[&str] = &[
    SESSION_STATE_CREATING,
    SESSION_STATE_CREATED,
    SESSION_STATE_IDLE,
    SESSION_STATE_BUSY,
];

/// The `EngineConfiguration$AdditionalConfigs` key that binds a session to a notebook: the
/// key that documentation names, "a key named `NotebookId` to `AdditionalConfigs` that has
/// the value of the Athena notebook ID".
pub const NOTEBOOK_ID_CONFIG: &str = "NotebookId";

/// The `ExecutorState` of an executor whose resources are still being acquired: "`CREATING`
/// - The executor is being started, including acquiring resources" (`ExecutorsSummary$
/// ExecutorState`). One of `EXECUTOR_STATE_VALUES` in [`crate::ops::types`].
pub const EXECUTOR_STATE_CREATING: &str = "CREATING";
/// The `ExecutorState` of an executor the session has registered: "`REGISTERED` - The
/// executor has been registered", which is the state an executor backing a running session
/// is in. See [`EXECUTOR_STATE_CREATING`].
pub const EXECUTOR_STATE_REGISTERED: &str = "REGISTERED";
/// The `ExecutorState` of an executor that is shutting down with its session:
/// "`TERMINATING` - The executor is in the process of shutting down". See
/// [`EXECUTOR_STATE_CREATING`].
pub const EXECUTOR_STATE_TERMINATING: &str = "TERMINATING";
/// The `ExecutorState` of an executor that is gone: "`TERMINATED` - The executor is no
/// longer running". See [`EXECUTOR_STATE_CREATING`].
pub const EXECUTOR_STATE_TERMINATED: &str = "TERMINATED";

/// The `ExecutorType` of the one executor a session is started with: the coordinator, "a
/// special executor that orchestrates processing work and manages other executors in a
/// notebook session" (`EngineConfiguration$CoordinatorDpuSize`). One of
/// `EXECUTOR_TYPE_VALUES` in [`crate::ops::types`].
pub const EXECUTOR_TYPE_COORDINATOR: &str = "COORDINATOR";

/// How long after a session is started its resources are DUE.
///
/// Athena hands back a session in a non-terminal state — `StartSession` answers with a
/// `State` of its own, and "the session is ready when it reaches an IDLE state" — and the
/// resources arrive later, so this is a transition the CLOCK owes: the instant is stored on
/// the record and every read derives the state from it. The span is this emulator's, not a
/// duration AWS publishes (a real session takes as long as its coordinator takes to come
/// up), and it is never zero — a caller reading straight back must still observe the
/// documented CREATING state.
pub const SESSION_CREATION: Duration = Duration::from_secs(3);

/// How long after a session is asked to shut down the shutdown is complete: the same kind
/// of emulator-chosen span as [`SESSION_CREATION`], so the documented TERMINATING state a
/// session passes through is observable rather than skipped.
pub const SESSION_TERMINATION: Duration = Duration::from_secs(3);

/// The session idle timeout a start that names none is given, in minutes.
///
/// CHOSEN by this emulator: `StartSessionRequest$SessionIdleTimeoutInMinutes` publishes no
/// default, only the range the model carries (1 to 480), so a session that names none is
/// given a timeout well inside that range rather than none at all — a session has an idle
/// timeout in Athena whether or not the request set one, and `SessionConfiguration` reports
/// it under both the names that shape carries. What the timeout IS to this emulator is the
/// configuration the session runs under; see [`SessionRecord::idle_timeout_minutes`].
pub const DEFAULT_SESSION_IDLE_TIMEOUT_MINUTES: i32 = 20;

/// The number of data processing units a coordinator runs on when the request names none:
/// "The default is 1" (`EngineConfiguration$CoordinatorDpuSize`), which is also the only
/// value the modelled range (1..1) admits.
pub const DEFAULT_COORDINATOR_DPU_SIZE: i32 = 1;

/// The number of data processing units an executor runs on when the request names none: "The
/// default number of DPUs to use for executors. […] The default is 1"
/// (`EngineConfiguration$DefaultExecutorDpuSize`).
pub const DEFAULT_EXECUTOR_DPU_SIZE: i32 = 1;

/// The largest number of data processing units a session may run concurrently when the
/// request names none: the `@default` the model carries on
/// `EngineConfiguration$MaxConcurrentDpus`.
pub const DEFAULT_MAX_CONCURRENT_DPUS: i32 = 20;

/// How many sessions this emulator retains for one account.
///
/// The EMULATOR's own bound, with no counterpart in the model: a terminated session stays
/// readable, so without a bound a sequence of starts would grow the emulator's heap
/// without limit. The OLDEST session is dropped once this many are held, exactly as the
/// query-execution history is bounded.
pub const MAX_SESSIONS: usize = 1000;

/// One executor a session runs on, as this service persists it.
///
/// Neither its state nor its termination instant is stored: both follow from the session's
/// own instants (see [`SessionRecord::executor_summaries`]), an executor of a session being
/// started with it and torn down with it.
#[derive(Debug, Clone)]
pub struct ExecutorRecord {
    /// The executor's id, reported as `ExecutorsSummary$ExecutorId` ("The UUID of the
    /// executor").
    pub id: String,
    /// An `ExecutorType` value: [`EXECUTOR_TYPE_COORDINATOR`] for the executor a session is
    /// started with.
    pub executor_type: String,
    /// How many data processing units this executor runs on, reported as
    /// `ExecutorsSummary$ExecutorSize`.
    pub size_dpus: i32,
}

/// One session, as this service persists it.
///
/// Everything the request that started the session carried is held here, so a read of the
/// session reports what that start wrote rather than anything rebuilt from the reading
/// request. The state is NOT stored: it follows from the stored instants
/// ([`Self::state_at`]), so a read derives it from the clock instead of a reader having to
/// move the record on.
#[derive(Debug, Clone)]
pub struct SessionRecord {
    /// The `SessionId` this session was minted under, which is also its key in the
    /// registry.
    pub id: String,
    /// The workgroup the session runs in — "The workgroup to which the session belongs"
    /// (`StartSessionRequest$WorkGroup`).
    pub work_group: String,
    /// The notebook this session was started in, for a start that bound it to one (see
    /// [`NOTEBOOK_ID_CONFIG`]). `None` for a session started for programmatic access,
    /// which belongs to no notebook.
    pub notebook_id: Option<String>,
    /// "The notebook version" the start carried (`StartSessionRequest$NotebookVersion`),
    /// reported back as `GetSessionResponse$NotebookVersion`.
    pub notebook_version: Option<String>,
    /// "The session description" the start carried
    /// (`StartSessionRequest$Description`).
    pub description: Option<String>,
    /// "The engine version used by the session (for example, PySpark engine version 3)"
    /// (`GetSessionResponse$EngineVersion`): the version the workgroup had SELECTED when
    /// the session was started, so a later change to the workgroup does not rewrite the
    /// version a running session was started on.
    pub engine_version: Option<String>,
    /// The DPU configuration and parameter mappings the start carried
    /// (`StartSessionRequest$EngineConfiguration`, which the model makes required).
    pub engine_configuration: Option<EngineConfiguration>,
    /// The log-delivery settings the start carried
    /// (`StartSessionRequest$MonitoringConfiguration`).
    pub monitoring_configuration: Option<MonitoringConfiguration>,
    /// "The workgroup configuration information used by the session"
    /// (`GetSessionResponse$SessionConfiguration`), resolved once when the session was
    /// started from the request and the workgroup it runs in.
    pub configuration: SessionConfiguration,
    /// The `ExecutionRole` the START carried, as it was sent — `None` where the request
    /// named none and the session therefore runs under the workgroup's.
    ///
    /// Kept BESIDE the resolved [`Self::configuration`] because the two answer different
    /// questions: that one is what the session runs under, and this one is what the request
    /// asked for, which is what [`Self::same_parameters`] compares.
    pub requested_execution_role: Option<String>,
    /// The tags the session carries: the ones the start listed, and the workgroup's own
    /// where the start asked for them to be copied
    /// (`StartSessionRequest$CopyWorkGroupTags`).
    pub tags: Vec<Tag>,
    /// The `Tags` the START listed, as they were sent. Kept for the same reason as
    /// [`Self::requested_execution_role`]: the tags the session CARRIES are [`Self::tags`],
    /// and these are the ones the request asked for.
    pub requested_tags: Vec<Tag>,
    /// Whether the start asked for the workgroup's tags to be copied onto the session.
    pub copy_work_group_tags: Option<bool>,
    /// The `ClientRequestToken` the start carried, if any: the token a repeat of that same
    /// request is answered with this session under ([`SessionRegistry::session_of_token`]).
    pub client_request_token: Option<String>,
    /// When the session was started, reported as `SessionStatus$StartDateTime` and as
    /// `NotebookSessionSummary$CreationTime` ("The time when the notebook session was
    /// created").
    pub created_at: SystemTime,
    /// When acquiring the session's resources is due to be finished —
    /// [`SESSION_CREATION`] after it was started. The session is
    /// [`SESSION_STATE_CREATING`] until then and [`SESSION_STATE_IDLE`] from then on.
    pub resources_due_at: SystemTime,
    /// When the session was asked to shut down, if it was, from which the
    /// [`SESSION_TERMINATION`] span runs.
    pub terminate_requested_at: Option<SystemTime>,
    /// The executors the start provisioned: the coordinator the session runs on.
    pub executors: Vec<ExecutorRecord>,
}

impl SessionRecord {
    /// The record a start of `id` in `work_group` at `now` registers: its resources not
    /// acquired yet, no shutdown asked for, and the coordinator it runs on provisioned.
    ///
    /// Everything the starting request carried beyond the workgroup and the notebook is set
    /// by the caller on the record this hands back, so one session is built in one place
    /// whether the start named those parameters or not.
    pub fn started(
        id: String,
        work_group: String,
        notebook_id: Option<String>,
        now: SystemTime,
    ) -> Self {
        Self {
            id,
            work_group,
            notebook_id,
            notebook_version: None,
            description: None,
            engine_version: None,
            engine_configuration: None,
            monitoring_configuration: None,
            configuration: SessionConfiguration::default(),
            requested_execution_role: None,
            tags: Vec::new(),
            requested_tags: Vec::new(),
            copy_work_group_tags: None,
            client_request_token: None,
            created_at: now,
            resources_due_at: due_at(now, SESSION_CREATION),
            terminate_requested_at: None,
            executors: vec![ExecutorRecord {
                id: new_executor_id(),
                executor_type: EXECUTOR_TYPE_COORDINATOR.to_string(),
                size_dpus: DEFAULT_COORDINATOR_DPU_SIZE,
            }],
        }
    }

    /// Whether `other` describes the SAME session this record does: every parameter the
    /// request that started it carried, and nothing the service decided itself (the id, the
    /// instants, the token).
    ///
    /// `StartSessionRequest$ClientRequestToken` is what needs the comparison: "If another
    /// StartSessionRequest is received, the same response is returned and another session is
    /// not created. If a parameter has changed, an error is returned."
    ///
    /// The parameters are the REQUEST's, so what the WORKGROUP contributed to the session is
    /// not among them: a workgroup updated between two identical starts changes what the
    /// second session would run under, and changes nothing about what that request asked for.
    pub fn same_parameters(&self, other: &Self) -> bool {
        self.work_group == other.work_group
            && self.notebook_id == other.notebook_id
            && self.notebook_version == other.notebook_version
            && self.description == other.description
            && self.engine_configuration == other.engine_configuration
            && self.monitoring_configuration == other.monitoring_configuration
            && self.requested_execution_role == other.requested_execution_role
            && self.configuration.session_idle_timeout_in_minutes
                == other.configuration.session_idle_timeout_in_minutes
            && self.requested_tags == other.requested_tags
            && self.copy_work_group_tags == other.copy_work_group_tags
    }

    /// When the shutdown this session was asked for is complete, for a session that was
    /// asked: [`SESSION_TERMINATION`] after the request, which is also the instant
    /// `SessionStatus$EndDateTime` reports ("The date and time that the session ended").
    pub fn ended_at(&self) -> Option<SystemTime> {
        self.terminate_requested_at
            .map(|requested| due_at(requested, SESSION_TERMINATION))
    }

    /// The instant this session last entered the state it is in at `now`.
    ///
    /// Derived from the same instants [`Self::state_at`] reads, so "the most recent date and
    /// time that the session was modified" (`SessionStatus$LastModifiedDateTime`) is the
    /// instant of the transition a reader is observing rather than the instant it read.
    pub fn state_changed_at(&self, now: SystemTime) -> SystemTime {
        if let Some(requested) = self.terminate_requested_at {
            let ended = due_at(requested, SESSION_TERMINATION);
            return if now < ended { requested } else { ended };
        }
        if now < self.resources_due_at {
            self.created_at
        } else {
            self.resources_due_at
        }
    }

    /// When the session became idle, or `None` while it is not: "The date and time starting
    /// at which the session became idle. Can be empty if the session is not currently idle"
    /// (`SessionStatus$IdleSinceDateTime`). A session is idle from the instant its resources
    /// were acquired.
    pub fn idle_since(&self, now: SystemTime) -> Option<SystemTime> {
        (self.state_at(now) == SESSION_STATE_IDLE).then_some(self.resources_due_at)
    }

    /// The record's `SessionStatus` at `now`: the state the clock puts the session in and
    /// the instants that state follows from.
    ///
    /// The one place a session's status is projected, so `GetSession`, a status read and a
    /// listing all report the same status for the same session at the same instant.
    pub fn to_session_status(&self, now: SystemTime) -> SessionStatus {
        let state = self.state_at(now);
        SessionStatus {
            start_date_time: Some(reported_time(self.created_at)),
            last_modified_date_time: Some(reported_time(self.state_changed_at(now))),
            // Reported only once the shutdown is complete: a session still shutting down
            // has not ended yet, and one nobody asked to shut down has no end.
            end_date_time: (state == SESSION_STATE_TERMINATED)
                .then(|| self.ended_at().map(reported_time))
                .flatten(),
            idle_since_date_time: self.idle_since(now).map(reported_time),
            state: Some(state.to_string()),
            // "The reason for the session state change": Athena states one for a state it
            // moved the session to on its own, and every state this service derives is one
            // the start or the caller's own shutdown asked for.
            state_change_reason: None,
        }
    }

    /// The `SessionState` this session is in at `now`.
    ///
    /// Derived, never written: two reads at the same instant report the same state, and a
    /// session nobody reads becomes idle exactly when one that is polled does.
    pub fn state_at(&self, now: SystemTime) -> &'static str {
        if let Some(requested) = self.terminate_requested_at {
            return if now < due_at(requested, SESSION_TERMINATION) {
                SESSION_STATE_TERMINATING
            } else {
                SESSION_STATE_TERMINATED
            };
        }
        if now < self.resources_due_at {
            SESSION_STATE_CREATING
        } else {
            SESSION_STATE_IDLE
        }
    }

    /// Whether the session is in one of the [`ACTIVE_SESSION_STATES`] at `now`.
    pub fn is_active_at(&self, now: SystemTime) -> bool {
        ACTIVE_SESSION_STATES.contains(&self.state_at(now))
    }

    /// Whether the session and its resources are no longer running at `now`.
    pub fn is_terminated_at(&self, now: SystemTime) -> bool {
        self.state_at(now) == SESSION_STATE_TERMINATED
    }

    /// The record as the modelled `SessionSummary` a workgroup's session listing reports it
    /// by, as of `now`.
    ///
    /// Every member is projected from this same record, and the status through the ONE place
    /// a session's status is projected ([`Self::to_session_status`]), so the listing reports
    /// no value a detailed read would not. The engine version travels as the `EngineVersion`
    /// shape here where `GetSession` reports the string, so the version the session was
    /// started on is resolved into that shape the way a workgroup's selection is
    /// ([`resolved_engine_version`]).
    pub fn to_summary(&self, now: SystemTime) -> SessionSummary {
        SessionSummary {
            session_id: Some(self.id.clone()),
            description: self.description.clone(),
            engine_version: Some(resolved_engine_version(Some(&EngineVersion {
                selected_engine_version: self.engine_version.clone(),
                // Read only, and ignored on the way in: the resolution decides it.
                effective_engine_version: None,
                // Decided by the resolution too, from the version it resolves to.
                category: None,
            }))),
            notebook_version: self.notebook_version.clone(),
            status: Some(self.to_session_status(now)),
        }
    }

    /// The record as the modelled `NotebookSessionSummary` a notebook's session listing
    /// reports: "the notebook session ID and notebook session creation time".
    pub fn to_notebook_session_summary(&self) -> NotebookSessionSummary {
        NotebookSessionSummary {
            session_id: Some(self.id.clone()),
            creation_time: Some(reported_time(self.created_at)),
        }
    }

    /// The session's executors as the modelled `ExecutorsSummary` entries `ListExecutors`
    /// reports, as of `now`, in the order they were provisioned.
    ///
    /// An executor is started with its session and torn down with it, so its state follows
    /// the session's: it is being acquired while the session is, registered while the session
    /// runs, and shutting down or gone with it. Its termination instant is the session's own
    /// end, and is reported only once it has one.
    ///
    /// The mapping is TOTAL over the session states, including the one `ListExecutors` does
    /// not reach today: that operation refuses a session whose resources "are no longer
    /// running" rather than listing them, so a `TERMINATED` executor is what this projects
    /// for any other caller of it.
    pub fn executor_summaries(&self, now: SystemTime) -> Vec<ExecutorsSummary> {
        let state = match self.state_at(now) {
            SESSION_STATE_CREATING => EXECUTOR_STATE_CREATING,
            SESSION_STATE_TERMINATING => EXECUTOR_STATE_TERMINATING,
            SESSION_STATE_TERMINATED => EXECUTOR_STATE_TERMINATED,
            _ => EXECUTOR_STATE_REGISTERED,
        };
        let terminated = self
            .is_terminated_at(now)
            .then(|| self.ended_at())
            .flatten()
            .map(epoch_millis);
        self.executors
            .iter()
            .map(|executor| ExecutorsSummary {
                executor_id: executor.id.clone(),
                executor_type: Some(executor.executor_type.clone()),
                start_date_time: Some(epoch_millis(self.created_at)),
                termination_date_time: terminated,
                executor_state: Some(state.to_string()),
                executor_size: Some(i64::from(executor.size_dpus)),
            })
            .collect()
    }

    /// Ask the session to shut down at `now`: the shutdown is complete
    /// [`SESSION_TERMINATION`] later, so the session is [`SESSION_STATE_TERMINATING`]
    /// until then. A session already asked keeps the instant it was first asked at — a
    /// repeat does not restart the shutdown, which is the no-op `TerminateSession`
    /// documents for a session that is already inactive.
    pub fn terminate(&mut self, now: SystemTime) {
        self.terminate_requested_at = Some(self.terminate_requested_at.unwrap_or(now));
    }
}

/// Every session this account holds, and the start tokens that already minted one.
///
/// A struct of its own rather than loose fields on the store, so the bound above is applied
/// in the ONE place a session is written ([`SessionRegistry::insert`]) and the newest-first
/// order a listing reports is derived in the ONE place sessions are looked up, and neither
/// can be honoured on one operation's path and skipped on another's.
#[derive(Debug, Default)]
pub struct SessionRegistry {
    /// The sessions, keyed by `SessionId`. A `BTreeMap` so a walk of the registry is in one
    /// stable order.
    sessions: BTreeMap<String, SessionRecord>,
    /// The ids of [`Self::sessions`] in the order they were started, oldest first — the
    /// order a listing reports reversed ("Newer sessions are listed first; older sessions
    /// are listed later"), and the order sessions are dropped in once [`MAX_SESSIONS`] are
    /// held.
    order: Vec<String>,
    /// The session each `ClientRequestToken` minted, so a repeat of the request that
    /// carried it is answered with that same session rather than starting another.
    of_token: BTreeMap<String, String>,
}

impl SessionRegistry {
    /// The session `id` names, or `None` when this account holds none.
    pub fn session(&self, id: &str) -> Option<&SessionRecord> {
        self.sessions.get(id)
    }

    /// The same session, for a caller that is going to move it on (a shutdown).
    pub fn session_mut(&mut self, id: &str) -> Option<&mut SessionRecord> {
        self.sessions.get_mut(id)
    }

    /// The session the `ClientRequestToken` `token` already minted, or `None` when no start
    /// carried it.
    pub fn session_of_token(&self, token: &str) -> Option<&SessionRecord> {
        self.of_token
            .get(token)
            .and_then(|id| self.sessions.get(id))
    }

    /// How many sessions are held, the count [`MAX_SESSIONS`] bounds.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Whether no session is held at all.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Store `record`. The one place a session enters the registry.
    ///
    /// The oldest session is dropped once [`MAX_SESSIONS`] are held, so the registry holds
    /// a bounded number of records rather than every session the account ever started, and
    /// a dropped session's `ClientRequestToken` stops resolving with it.
    pub fn insert(&mut self, record: SessionRecord) {
        while self.order.len() >= MAX_SESSIONS {
            let oldest = self.order.remove(0);
            if let Some(dropped) = self.sessions.remove(&oldest) {
                if let Some(token) = dropped.client_request_token {
                    self.of_token.remove(&token);
                }
            }
        }
        let id = record.id.clone();
        if let Some(token) = &record.client_request_token {
            self.of_token.insert(token.clone(), id.clone());
        }
        self.order.retain(|held| held != &id);
        self.order.push(id.clone());
        self.sessions.insert(id, record);
    }

    /// The sessions of `work_group`, newest first — the order `ListSessions` reports.
    pub fn sessions_in(&self, work_group: &str) -> Vec<&SessionRecord> {
        self.order
            .iter()
            .rev()
            .filter_map(|id| self.sessions.get(id))
            .filter(|record| record.work_group == work_group)
            .collect()
    }

    /// The ACTIVE sessions of the notebook `notebook_id`, newest first, as the modelled
    /// `NotebookSessionSummary` entries a listing reports.
    ///
    /// The order and the filter `ListNotebookSessions` documents: "Lists, in descending
    /// order, the sessions that have been created in a notebook that are in an active state
    /// like CREATING, CREATED, IDLE or BUSY. Newer sessions are listed first; older
    /// sessions are listed later." Sessions started in the same instant are reported
    /// most-recently-started first, so the order is total whatever the clock's resolution.
    pub fn notebook_sessions_at(
        &self,
        notebook_id: &str,
        now: SystemTime,
    ) -> Vec<NotebookSessionSummary> {
        let mut of_notebook: Vec<&SessionRecord> = self
            .order
            .iter()
            .rev()
            .filter_map(|id| self.sessions.get(id))
            .filter(|record| record.notebook_id.as_deref() == Some(notebook_id))
            .filter(|record| record.is_active_at(now))
            .collect();
        // A STABLE sort, over a walk that is already newest-started first, so records
        // sharing a creation instant keep that order.
        of_notebook.sort_by_key(|record| ::std::cmp::Reverse(record.created_at));
        of_notebook
            .iter()
            .map(|record| record.to_notebook_session_summary())
            .collect()
    }

    /// Remove every session of `work_group`, as a delete of that workgroup does, and answer
    /// with how many were removed.
    ///
    /// A session is addressed by its `SessionId` alone, but it BELONGS to the workgroup it was
    /// started in — the workgroup whose engine configuration and execution role it runs under
    /// — so one whose workgroup is gone could never be started again or listed, and keeping it
    /// would hand a workgroup later created under the same name sessions nobody started in it.
    pub fn remove_work_group(&mut self, work_group: &str) -> usize {
        let removed: Vec<String> = self
            .sessions
            .values()
            .filter(|record| record.work_group == work_group)
            .map(|record| record.id.clone())
            .collect();
        for id in &removed {
            self.remove(id);
        }
        removed.len()
    }

    /// Drop the session `id` names, with the start token that named it: a token whose session
    /// is gone cannot answer with the same id, so it is not kept pointing at nothing.
    fn remove(&mut self, id: &str) -> Option<SessionRecord> {
        let removed = self.sessions.remove(id)?;
        self.order.retain(|held| held != id);
        self.of_token.retain(|_, minted| minted != id);
        Some(removed)
    }
}

/// A fresh `SessionId`: a version-4 UUID in its hyphenated, lower-case form, well inside
/// the modelled `SessionId` length (1..256). Minted from the maintained crate rather than
/// laid out here.
pub fn new_session_id() -> String {
    ::uuid::Uuid::new_v4().as_hyphenated().to_string()
}

/// A fresh `ExecutorId`: the UUID `ExecutorsSummary$ExecutorId` documents ("The UUID of the
/// executor").
pub fn new_executor_id() -> String {
    ::uuid::Uuid::new_v4().as_hyphenated().to_string()
}

/// The cursor a `ListSessions` page hands back: the workgroup it is listing the sessions of
/// and the position to resume at.
fn sessions_cursor(work_group: &str, position: usize) -> String {
    format!("sessions{CURSOR_SEPARATOR}{work_group}{CURSOR_SEPARATOR}{position}")
}

/// The cursor a `ListExecutors` page hands back: the session whose executors it is reporting
/// and the position to resume at.
fn executors_cursor(session_id: &str, position: usize) -> String {
    format!("executors{CURSOR_SEPARATOR}{session_id}{CURSOR_SEPARATOR}{position}")
}

/// The `SessionConfiguration` a session started in `work_group` runs under: "the workgroup
/// configuration information used by the session" (`GetSessionResponse$SessionConfiguration`).
///
/// Resolved ONCE, when the session is started, and stored on the record, so a later change
/// to the workgroup does not rewrite the configuration a running session was started under.
/// The execution role is the one the start named, or the workgroup's own where it named none
/// ("The ARN of the execution role used to access user resources for Spark sessions",
/// `WorkGroupConfiguration$ExecutionRole`); the working directory and the encryption
/// settings are the workgroup's result configuration, which is where a session's notebook
/// information is stored.
pub fn session_configuration(
    work_group: &WorkGroupRecord,
    execution_role: Option<String>,
    session_idle_timeout_in_minutes: Option<i32>,
) -> SessionConfiguration {
    let results = work_group.configuration.result_configuration.as_ref();
    SessionConfiguration {
        execution_role: execution_role.or_else(|| work_group.configuration.execution_role.clone()),
        working_directory: results.and_then(|results| results.output_location.clone()),
        // "The idle timeout in seconds for the session", which is the minutes the request
        // asked for expressed in the unit this member carries.
        idle_timeout_seconds: session_idle_timeout_in_minutes
            .map(|minutes| i64::from(minutes) * SECONDS_PER_MINUTE),
        session_idle_timeout_in_minutes,
        encryption_configuration: results
            .and_then(|results| results.encryption_configuration.clone()),
    }
}

/// Seconds in a minute, for the one place a session's idle timeout is restated in the unit
/// `SessionConfiguration$IdleTimeoutSeconds` carries.
const SECONDS_PER_MINUTE: i64 = 60;

/// The tags a session started in `work_group` carries: the ones the start listed, over the
/// workgroup's own where the start asked for those to be copied.
///
/// `StartSessionRequest$CopyWorkGroupTags` is documented "Copies the tags from the Workgroup
/// to the Session", so a start that asks for it begins from the workgroup's tags; a key the
/// request lists itself takes the request's value, the way any tag write resolves a key the
/// resource already carries ([`write_tags`]).
pub fn session_tags(
    work_group: &WorkGroupRecord,
    requested: Vec<Tag>,
    copy_work_group_tags: bool,
) -> Vec<Tag> {
    let mut held = if copy_work_group_tags {
        work_group.tags.clone()
    } else {
        Vec::new()
    };
    write_tags(&mut held, requested);
    held
}

/// The `SessionId` a start bound the session to a notebook with, out of the engine
/// configuration it carried.
///
/// `EngineConfiguration$AdditionalConfigs` documents the binding: "add a key named
/// NotebookId to AdditionalConfigs that has the value of the Athena notebook ID" (see
/// [`NOTEBOOK_ID_CONFIG`]). A start carrying no such key belongs to no notebook.
pub fn notebook_of_engine_configuration(
    engine_configuration: Option<&EngineConfiguration>,
) -> Option<String> {
    engine_configuration
        .and_then(|configuration| configuration.additional_configs.as_ref())
        .and_then(|configs| configs.get(NOTEBOOK_ID_CONFIG))
        .cloned()
}

/// The message a session-addressed request this service will not act on is refused with.
///
/// FIXED text with no variable part: the `SessionId` the request named is deliberately not
/// quoted back, so a refusal discloses nothing about which ids name sessions. It is the
/// sentence Athena refuses such a request with, shared by every operation of this service
/// that addresses ONE session, so the same request reads the same way whichever of them was
/// called — whether the id resolved to no session of this account at all, or resolved to a
/// session this service will not perform the request's work in (one whose resources are
/// gone, or one serving no notebook for a calculation to be a cell of).
pub const SESSION_NOT_FOUND: &str = "Not authorized to make this request.";

/// The message a start that names a notebook version without the notebook it belongs to is
/// refused with: the requirement `StartSessionRequest$NotebookVersion` states, "If you
/// specify a value for NotebookVersion, you must also specify a value for NotebookId",
/// which travels in `EngineConfiguration$AdditionalConfigs` under
/// [`NOTEBOOK_ID_CONFIG`].
pub const NOTEBOOK_ID_REQUIRED: &str =
    "If you specify a value for NotebookVersion, you must also specify a value for NotebookId.";

/// The cursor a `ListNotebookSessions` page hands back: the notebook it listed the sessions
/// of and the position to resume at.
fn notebook_sessions_cursor(notebook_id: &str, position: usize) -> String {
    format!("notebooksessions{CURSOR_SEPARATOR}{notebook_id}{CURSOR_SEPARATOR}{position}")
}

impl ComAmazonawsAthenaAmazonathenaState {
    /// The `NextToken` a `ListSessions` page of `work_group` resumed at `position` hands back.
    pub fn sessions_page_token(&self, work_group: &str, position: usize) -> String {
        ::hex::encode(
            self.page_token_key
                .mint(&sessions_cursor(work_group, position)),
        )
    }

    /// The position `token` resumes a session listing of `work_group` at, or `None` for a
    /// token this store did not mint for THAT workgroup.
    pub fn sessions_page_position(&self, work_group: &str, token: &str) -> Option<usize> {
        let cursor = self.page_cursor(token)?;
        cursor_position(&cursor, "sessions", work_group)
    }

    /// The `NextToken` a `ListExecutors` page of session `session_id` resumed at `position`
    /// hands back.
    pub fn executors_page_token(&self, session_id: &str, position: usize) -> String {
        ::hex::encode(
            self.page_token_key
                .mint(&executors_cursor(session_id, position)),
        )
    }

    /// The position `token` resumes an executor listing of session `session_id` at, or `None`
    /// for a token this store did not mint for THAT session.
    pub fn executors_page_position(&self, session_id: &str, token: &str) -> Option<usize> {
        let cursor = self.page_cursor(token)?;
        cursor_position(&cursor, "executors", session_id)
    }

    /// The `NextToken` a `ListNotebookSessions` page of `notebook_id` resumed at `position`
    /// hands back.
    pub fn notebook_sessions_page_token(&self, notebook_id: &str, position: usize) -> String {
        ::hex::encode(
            self.page_token_key
                .mint(&notebook_sessions_cursor(notebook_id, position)),
        )
    }

    /// The position `token` resumes a listing of `notebook_id`'s sessions at, or `None` for
    /// a token this store did not mint for THAT notebook's listing.
    pub fn notebook_sessions_page_position(
        &self,
        notebook_id: &str,
        token: &str,
    ) -> Option<usize> {
        let cursor = self.page_cursor(token)?;
        cursor_position(&cursor, "notebooksessions", notebook_id)
    }
}

/// `at` as the count of milliseconds since the Unix epoch, which is how
/// `ExecutorsSummary$StartDateTime` and `$TerminationDateTime` carry an instant (a `Long`).
fn epoch_millis(at: SystemTime) -> i64 {
    millis_between(SystemTime::UNIX_EPOCH, at)
}

/// `now` plus `span`, or `now` when that instant is not representable — so a clock far
/// enough in the future to overflow yields a transition that is already due rather than a
/// panic.
fn due_at(now: SystemTime, span: Duration) -> SystemTime {
    now.checked_add(span).unwrap_or(now)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session started in `work_group` at `started_at`, the record every case below moves
    /// the clock over.
    fn session(work_group: &str, started_at: SystemTime) -> SessionRecord {
        SessionRecord::started(
            new_session_id(),
            work_group.to_string(),
            None,
            started_at,
        )
    }

    /// RULE: a session's state follows the clock — it is `CREATING` until its resources are
    /// due up and `IDLE` from then on — and no read has to write to move it on.
    #[test]
    fn the_state_a_session_reports_follows_the_clock() {
        let started_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let record = session("spark-wg", started_at);
        let cases = [
            ("at the start", Duration::ZERO, SESSION_STATE_CREATING),
            (
                "a moment before the resources are up",
                SESSION_CREATION - Duration::from_millis(1),
                SESSION_STATE_CREATING,
            ),
            (
                "the instant they are up",
                SESSION_CREATION,
                SESSION_STATE_IDLE,
            ),
            (
                "long after they are up",
                SESSION_CREATION * 1_000,
                SESSION_STATE_IDLE,
            ),
        ];
        for (case, elapsed, expected) in cases {
            let now = started_at + elapsed;
            let reached = record.state_at(now);
            assert_eq!(
                reached, expected,
                "case {case:?}: expected {expected}, got {reached}"
            );
            assert_eq!(
                record.idle_since(now).is_some(),
                expected == SESSION_STATE_IDLE,
                "case {case:?}: an idle instant is reported exactly while the session is \
                 idle, got {:?}",
                record.idle_since(now)
            );
        }
    }

    /// RULE: a shutdown is a span of the clock too — the session reports `TERMINATING` until
    /// the teardown is due and `TERMINATED` from then on, its end instant is that teardown's,
    /// and a repeated shutdown does not restart it.
    #[test]
    fn a_requested_shutdown_passes_through_terminating() {
        let started_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let mut record = session("spark-wg", started_at);
        let asked_at = started_at + SESSION_CREATION;
        record.terminate(asked_at);
        // A repeat keeps the instant the first call stored, so the session ends when that
        // first teardown is due rather than later.
        record.terminate(asked_at + SESSION_TERMINATION * 10);
        let cases = [
            (
                "the instant the teardown was asked for",
                Duration::ZERO,
                SESSION_STATE_TERMINATING,
                false,
            ),
            (
                "a moment before it is due",
                SESSION_TERMINATION - Duration::from_millis(1),
                SESSION_STATE_TERMINATING,
                false,
            ),
            (
                "the instant it is due",
                SESSION_TERMINATION,
                SESSION_STATE_TERMINATED,
                true,
            ),
        ];
        for (case, elapsed, expected, ended) in cases {
            let now = asked_at + elapsed;
            let reached = record.state_at(now);
            assert_eq!(
                reached, expected,
                "case {case:?}: expected {expected}, got {reached}"
            );
            // The end instant is the teardown's own, so it is the SAME instant before and
            // after the clock reaches it; what changes at that instant is whether the
            // session has ended, which is the state asserted above and the instant
            // `SessionStatus$EndDateTime` is then reported from.
            assert_eq!(
                record.is_terminated_at(now),
                ended,
                "case {case:?}: expected the session to have ended: {ended}, got the state \
                 {reached}"
            );
            assert_eq!(
                record.ended_at(),
                Some(asked_at + SESSION_TERMINATION),
                "case {case:?}: expected the end instant to be the first teardown's, got {:?}",
                record.ended_at()
            );
            assert!(
                !record.is_active_at(now),
                "case {case:?}: a session that was asked to shut down is in no active state"
            );
        }
    }

    /// RULE: the state of a session's executor follows its session's own at every instant —
    /// acquiring while the session is, registered while it runs, shutting down and then gone
    /// with it — and its termination instant is the session's own end.
    #[test]
    fn an_executors_state_follows_its_sessions() {
        let started_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let live = session("spark-wg", started_at);
        // The same session once a shutdown has been asked for: a record carries ONE
        // shutdown instant, so the states either side of it are read off the record as it
        // stood then.
        let asked_at = started_at + SESSION_CREATION * 2;
        let mut shutting_down = live.clone();
        shutting_down.terminate(asked_at);
        let cases = [
            (
                "while the session acquires its resources",
                &live,
                started_at,
                EXECUTOR_STATE_CREATING,
                false,
            ),
            (
                "once the session is ready",
                &live,
                started_at + SESSION_CREATION,
                EXECUTOR_STATE_REGISTERED,
                false,
            ),
            (
                "while the session shuts down",
                &shutting_down,
                asked_at,
                EXECUTOR_STATE_TERMINATING,
                false,
            ),
            (
                "once the session is gone",
                &shutting_down,
                asked_at + SESSION_TERMINATION,
                EXECUTOR_STATE_TERMINATED,
                true,
            ),
        ];
        for (case, record, now, expected, terminated) in cases {
            let summaries = record.executor_summaries(now);
            assert_eq!(
                summaries.len(),
                1,
                "case {case:?}: expected the coordinator alone, got {summaries:?}"
            );
            let coordinator = summaries.first().expect("one executor");
            assert_eq!(
                coordinator.executor_state.as_deref(),
                Some(expected),
                "case {case:?}: expected {expected}, got {:?}",
                coordinator.executor_state
            );
            assert_eq!(
                coordinator.executor_type.as_deref(),
                Some(EXECUTOR_TYPE_COORDINATOR),
                "case {case:?}: the executor a start provisions is the coordinator, got {:?}",
                coordinator.executor_type
            );
            assert_eq!(
                coordinator.termination_date_time.is_some(),
                terminated,
                "case {case:?}: expected a termination instant to be reported: {terminated}, \
                 got {:?}",
                coordinator.termination_date_time
            );
        }
    }

    /// RULE: a workgroup's sessions are walked newest first, and only its own.
    #[test]
    fn a_workgroups_sessions_are_walked_newest_first() {
        let started_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let mut registry = SessionRegistry::default();
        let mut ids = Vec::new();
        for index in 0..3u32 {
            let record = session("spark-wg", started_at + Duration::from_secs(index.into()));
            ids.push(record.id.clone());
            registry.insert(record);
        }
        // Another workgroup's session is never in this workgroup's walk.
        registry.insert(session("another-wg", started_at));

        let walked: Vec<&str> = registry
            .sessions_in("spark-wg")
            .into_iter()
            .map(|record| record.id.as_str())
            .collect();
        let newest_first: Vec<&str> = ids.iter().rev().map(String::as_str).collect();
        assert_eq!(
            walked, newest_first,
            "case \"three sessions of one workgroup\": expected the newest first, got {walked:?}"
        );
        assert_eq!(
            registry.len(),
            4,
            "case \"four sessions across two workgroups\": expected all four to be held"
        );
    }

    /// RULE: a start token answers with the session it minted, and the record it points at
    /// goes with the session when the workgroup is deleted.
    #[test]
    fn a_start_token_resolves_the_session_it_minted() {
        let started_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let mut registry = SessionRegistry::default();
        let mut record = session("spark-wg", started_at);
        record.client_request_token = Some("a-token".to_string());
        let id = record.id.clone();
        registry.insert(record);
        assert_eq!(
            registry.session_of_token("a-token").map(|held| &held.id),
            Some(&id),
            "case \"the token the start carried\": expected the session it minted"
        );
        assert!(
            registry.session_of_token("another-token").is_none(),
            "case \"a token no start carried\": expected no session"
        );
        assert_eq!(
            registry.remove_work_group("spark-wg"),
            1,
            "case \"deleting the workgroup\": expected its one session to go with it"
        );
        assert!(
            registry.session_of_token("a-token").is_none() && registry.is_empty(),
            "case \"after the workgroup was deleted\": expected neither the session nor its \
             token to be held"
        );
    }

    /// RULE: the notebook an engine configuration binds its session to is the value of its
    /// `NotebookId` additional config, and nothing else binds one.
    #[test]
    fn the_notebook_a_start_binds_comes_from_its_additional_configs() {
        let with = |entries: &[(&str, &str)]| EngineConfiguration {
            additional_configs: Some(
                entries
                    .iter()
                    .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                    .collect(),
            ),
            ..EngineConfiguration::default()
        };
        let cases = [
            (
                "the documented key",
                with(&[(NOTEBOOK_ID_CONFIG, "nb-1")]),
                Some("nb-1".to_string()),
            ),
            (
                "the documented key beside others",
                with(&[("Other", "x"), (NOTEBOOK_ID_CONFIG, "nb-2")]),
                Some("nb-2".to_string()),
            ),
            ("another key alone", with(&[("NotebookName", "nb-3")]), None),
            ("no additional configs at all", with(&[]), None),
        ];
        for (case, configuration, expected) in cases {
            let bound = notebook_of_engine_configuration(Some(&configuration));
            assert_eq!(
                bound, expected,
                "case {case:?}: expected {expected:?}, got {bound:?}"
            );
        }
    }
}
