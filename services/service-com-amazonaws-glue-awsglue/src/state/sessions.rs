// SPDX-License-Identifier: CC0-1.0

//! The stored interactive-session records and the statements run inside them, with the
//! statuses each moves through.

use super::*;

// The modelled shape the text a cell wrote is reported in, which only this file builds.
use crate::ops::types::StatementOutputData;

/// The `SessionStatus` a session is in the instant its create returns: the remote Spark
/// environment is still being provisioned. One of the modelled `SessionStatus` values
/// (`PROVISIONING`, `READY`, `FAILED`, `TIMEOUT`, `STOPPING`, `STOPPED`, from
/// `smithy_query shape=SessionStatus`).
pub const SESSION_STATUS_PROVISIONING: &str = "PROVISIONING";

/// The `SessionStatus` of a session whose environment is up and can take statements. See
/// [`SESSION_STATUS_PROVISIONING`].
pub const SESSION_STATUS_READY: &str = "READY";

/// The `SessionStatus` of a session its own timeout ended: "the number of minutes when idle
/// before session times out" (`CreateSessionRequest$IdleTimeout`) has passed. See
/// [`SESSION_STATUS_PROVISIONING`].
pub const SESSION_STATUS_TIMEOUT: &str = "TIMEOUT";

/// The `SessionStatus` of a session a stop has begun on and whose environment is still being
/// torn down. See [`SESSION_STATUS_PROVISIONING`].
pub const SESSION_STATUS_STOPPING: &str = "STOPPING";

/// The `SessionStatus` of a session whose environment a stop has taken all the way down: it
/// runs nothing further. See [`SESSION_STATUS_PROVISIONING`].
pub const SESSION_STATUS_STOPPED: &str = "STOPPED";

/// The `SessionStatus` of a session whose environment never came up. See
/// [`SESSION_STATUS_PROVISIONING`].
pub const SESSION_STATUS_FAILED: &str = "FAILED";

/// Whether a session in `status` is still being moved between statuses by the service
/// itself, rather than resting in one only a caller can move it out of.
///
/// Of the modelled `SessionStatus` values (`smithy_query shape=SessionStatus`) exactly two
/// are such a status: the environment is being brought up
/// ([`SESSION_STATUS_PROVISIONING`]) or taken down ([`SESSION_STATUS_STOPPING`]). The other
/// four — `READY`, `FAILED`, `TIMEOUT` and `STOPPED` — are where a session rests.
pub fn session_status_is_transitional(status: &str) -> bool {
    status == SESSION_STATUS_PROVISIONING || status == SESSION_STATUS_STOPPING
}

/// The `SessionStatus` values a session has ENDED in: of the modelled values quoted on
/// [`SESSION_STATUS_PROVISIONING`], the three in which no remote Spark runtime environment
/// exists any more and nothing brings one back — the environment never came up, the session's
/// own timeout ended it, or a stop took it down.
pub const ENDED_SESSION_STATUSES: [&str; 3] = [
    SESSION_STATUS_FAILED,
    SESSION_STATUS_TIMEOUT,
    SESSION_STATUS_STOPPED,
];

/// Whether a session in `status` has ENDED, by [`ENDED_SESSION_STATUSES`] — so its `Id` names
/// no runtime an operation could act on, where a session that is merely transitional
/// ([`session_status_is_transitional`]) still has one on the way up or down.
pub fn session_has_ended(status: &str) -> bool {
    ENDED_SESSION_STATUSES.contains(&status)
}

/// How long a session stays in [`SESSION_STATUS_PROVISIONING`] before the clock owes it
/// [`SESSION_STATUS_READY`].
///
/// CHOSEN, not documented: Glue publishes no duration for provisioning a session, so this is
/// the emulator's own span, the same one a blueprint registration takes
/// ([`BLUEPRINT_SETTLES_AFTER`]) — deliberately not zero, so a caller that reads a session
/// straight after creating it still observes it being provisioned, and the refusals that go
/// with a session that is not ready. The due instant is stored on the record and
/// materialised by whichever later call reads it; nothing sleeps.
pub const SESSION_SETTLES_AFTER: Duration = BLUEPRINT_SETTLES_AFTER;

/// `CreateSessionRequest$Timeout` when the create named none: "The number of minutes before
/// session times out. Default for Spark ETL jobs is 48 hours (2880 minutes)."
pub const DEFAULT_SESSION_TIMEOUT_MINUTES: i32 = 2_880;

/// The Glue version a session has to run on, as `CreateSessionRequest$GlueVersion` states
/// the restriction: "The GlueVersion must be greater than 2.0." A create naming a version
/// that is not is refused rather than stored.
pub const MIN_SESSION_GLUE_VERSION: f64 = 2.0;

/// The `SessionType` a create that named none is provisioned as.
///
/// The model documents no default for the member, and the recorded reads of a session created
/// without one report `LIVY`: of the modelled values (`LIVY`, `SPARK_CONNECT`, from
/// `smithy_query shape=SessionType`) that is the kind this service's own statement operations
/// drive — `RunStatement`, `GetStatement` and `CancelStatement` are that interface — so a
/// create that asks for no particular kind is provisioned as that one, and every read reports
/// the kind the session IS rather than leaving a client to guess it.
pub const DEFAULT_SESSION_TYPE: &str = "LIVY";

/// The worker type a session create that named no capacity is provisioned with: the default
/// worker type, which "maps to 1 DPU" (`CreateSessionRequest$WorkerType`) — the same type a
/// job definition naming no capacity is given ([`DEFAULT_SPARK_WORKER_TYPE`]), and the type
/// the recorded reads of such a session report.
pub const DEFAULT_SESSION_WORKER_TYPE: &str = DEFAULT_SPARK_WORKER_TYPE;

/// How many [`DEFAULT_SESSION_WORKER_TYPE`] workers such a session is provisioned with.
///
/// The model documents no default here either, and the recorded reads of a session created
/// without a capacity report two of them (and the DPU figure they amount to): a remote Spark
/// runtime — "the period in which a remote Spark runtime environment is running" (`Session`)
/// — runs a driver and an executor, so two workers is the smallest session the service brings
/// up, and the figure follows from them through the one per-worker table every capacity goes
/// through ([`dpu_per_worker`]).
pub const DEFAULT_SESSION_WORKERS: i32 = 2;

/// How many seconds one minute of a session's documented timeouts is: both `Timeout` and
/// `IdleTimeout` are stated in minutes.
pub(super) const SECONDS_PER_MINUTE: u64 = 60;

/// Whether `glue_version` fails the restriction quoted on [`MIN_SESSION_GLUE_VERSION`], and
/// the refusal to state when it does.
///
/// A `GlueVersion` this cannot read as a number (the model's pattern `^(\w+\.)+\w+$` is
/// wider than a decimal) fails nothing: the restriction is stated as a version comparison,
/// so a version no comparison can be made against is not refused by it — the same reading
/// [`admits_flex_execution_class`] takes of its own version restriction.
pub fn session_glue_version_defect(glue_version: &str) -> Option<String> {
    let version = glue_version.parse::<f64>().ok()?;
    (version <= MIN_SESSION_GLUE_VERSION)
        .then(|| messages::session_glue_version_too_low(glue_version))
}

/// A stored interactive session: the remote Spark runtime environment a session create
/// provisions, "the period in which a remote Spark runtime environment is running"
/// (`Session`).
///
/// Everything a caller can observe through the modelled `Session` shape is held here — the
/// id it is addressed by, the command, role and capacity the create asked for, when it was
/// created and the status its provisioning has reached — plus the two documented timeouts
/// and the `Tags` the create carried, neither of which that shape reports.
#[derive(Debug, Clone)]
pub struct SessionEntry {
    /// `Session$Id`: "The ID of the session", which is also this record's key in
    /// [`ComAmazonawsGlueAwsglueState::sessions`]. The create chooses it
    /// (`CreateSessionRequest$Id`, "The ID of the session request"), so it is not minted
    /// here.
    pub id: String,
    /// `Session$CreatedOn`: "The time and date when the session was created."
    pub created_on: SystemTime,
    /// The instant this session was last active, which is where the IDLE leg of its timeout
    /// is measured from ([`Self::times_out_at`]). A session nothing has run a statement on
    /// has been idle since it was created.
    pub last_active_on: SystemTime,
    /// One of the modelled `SessionStatus` values: "The session status."
    pub status: String,
    /// The instant the clock owes this session its next status; `None` once it rests in a
    /// status only a caller can move it out of.
    pub settles_at: Option<SystemTime>,
    /// The `SessionStatus` the clock owes this session at [`Self::settles_at`]: the status
    /// its environment is being brought up to, or taken down to.
    pub settles_to: String,
    /// `Session$CompletedOn`: "The date and time that this session is completed", and
    /// `None` while it is still running.
    pub completed_on: Option<SystemTime>,
    /// `Session$Description`: "The description of the session", and `None` when the create
    /// carried none.
    pub description: Option<String>,
    /// `Session$Role`: "The name or Amazon Resource Name (ARN) of the IAM role associated
    /// with the Session", exactly as the create sent it.
    pub role: String,
    /// `Session$Command`: "The command object", as the create described it.
    pub command: SessionCommand,
    /// `Session$DefaultArguments`: "A map array of key-value pairs", which is the EMPTY map
    /// for a create that carried none — the service reports the arguments a statement of
    /// this session runs with, and a session with no arguments runs with none.
    pub default_arguments: HashMap<String, String>,
    /// `Session$Connections`: "The number of connections used for the session."
    pub connections: Option<ConnectionsList>,
    /// The session's capacity in DPUs: what the create named, else what its worker
    /// configuration maps to ([`dpu_per_worker`]), resolved by the one helper every
    /// capacity-carrying request goes through.
    pub max_capacity: Option<f64>,
    /// `Session$NumberOfWorkers`: "The number of workers of a defined `WorkerType` to use
    /// for the session", as the create named it, else the configuration the service
    /// provisions a session that named no capacity with ([`DEFAULT_SESSION_WORKERS`]).
    pub number_of_workers: Option<i32>,
    /// `Session$WorkerType`: "The type of predefined worker that is allocated when a
    /// session runs", as the create named it, else [`DEFAULT_SESSION_WORKER_TYPE`].
    pub worker_type: Option<String>,
    /// `Session$SecurityConfiguration`: "The name of the SecurityConfiguration structure to
    /// be used with the session."
    pub security_configuration: Option<String>,
    /// `Session$GlueVersion`: "The Glue version determines the versions of Apache Spark and
    /// Python that Glue supports", as the create named it. `None` when it named none: the
    /// model documents no default for a session's Glue version, so none is invented here.
    pub glue_version: Option<String>,
    /// `Session$SessionType`: "The type of the session", one of the modelled `SessionType`
    /// values, as the create named it, else [`DEFAULT_SESSION_TYPE`].
    pub session_type: String,
    /// "The number of minutes before session times out"
    /// (`CreateSessionRequest$Timeout`), defaulted by
    /// [`DEFAULT_SESSION_TIMEOUT_MINUTES`] when the create named none.
    ///
    /// The modelled `Session` shape carries NO `Timeout` member, so this is not reported;
    /// it is kept because it bounds how long the session runs and is the documented default
    /// for [`Self::idle_timeout`].
    pub timeout: i32,
    /// `Session$IdleTimeout`: "The number of minutes when idle before the session times
    /// out", which for a create that named none is the value of [`Self::timeout`] ("Default
    /// for Spark ETL jobs is value of Timeout").
    pub idle_timeout: i32,
    /// The tags the session was created with, which the modelled `Session` shape does not
    /// carry.
    ///
    /// Kept because `ListSessions` takes "the tags" to narrow its answer by, so they decide
    /// which sessions a filtered listing reports. NOT yet reachable through the tagging
    /// operations, for the reason [`BlueprintEntry::tags`] gives: [`TaggedResourceKind`]
    /// carries no session variant yet.
    pub tags: HashMap<String, String>,
    /// The statements run on this session, oldest first — which is submission order,
    /// because a statement is appended here by the run that recorded it.
    ///
    /// They live ON the session, as a job's runs live on the job, because every operation
    /// that addresses a statement names its `SessionId` and then the `Id` this session
    /// minted for it ([`Self::next_statement_id`]).
    pub statements: Vec<StatementEntry>,
    /// The `Id` the next statement run on this session is given: one past the last one
    /// minted, counting from [`FIRST_STATEMENT_ID`]. Kept on the session rather than
    /// derived from [`Self::statements`], so an id is never reused once the oldest records
    /// are dropped.
    pub next_statement_id: i32,
}

impl SessionEntry {
    /// Record the session a session create describes, in [`SESSION_STATUS_PROVISIONING`]
    /// with [`SESSION_STATUS_READY`] due [`SESSION_SETTLES_AFTER`] out, and every other
    /// member taken from the request or from the default the model documents for it.
    pub fn from_create_input(input: &CreateSessionInput, now: SystemTime) -> Self {
        let timeout = input.timeout.unwrap_or(DEFAULT_SESSION_TIMEOUT_MINUTES);
        // The worker configuration the create named, else the one the service provisions a
        // session that named no capacity at all with; a create that named a DPU figure
        // asked for that capacity and gets no worker configuration invented for it.
        let (worker_type, number_of_workers) = match (
            input.worker_type.clone(),
            input.number_of_workers,
            input.max_capacity,
        ) {
            (None, None, None) => (
                Some(DEFAULT_SESSION_WORKER_TYPE.to_string()),
                Some(DEFAULT_SESSION_WORKERS),
            ),
            (worker_type, number_of_workers, _) => (worker_type, number_of_workers),
        };
        Self {
            id: input.id.clone(),
            created_on: now,
            last_active_on: now,
            status: SESSION_STATUS_PROVISIONING.to_string(),
            settles_at: Some(now.checked_add(SESSION_SETTLES_AFTER).unwrap_or(now)),
            settles_to: SESSION_STATUS_READY.to_string(),
            completed_on: None,
            description: input.description.clone(),
            role: input.role.clone(),
            command: input.command.clone(),
            default_arguments: input.default_arguments.clone().unwrap_or_default(),
            connections: input.connections.clone(),
            // The same three members a job's capacity is read from, with the same
            // documentation, so they resolve through the same helper.
            max_capacity: capacity_from(
                input.max_capacity,
                0,
                worker_type.as_deref(),
                number_of_workers,
            ),
            number_of_workers,
            worker_type,
            security_configuration: input.security_configuration.clone(),
            glue_version: input.glue_version.clone(),
            session_type: input
                .session_type
                .clone()
                .unwrap_or_else(|| DEFAULT_SESSION_TYPE.to_string()),
            timeout,
            // "Default for Spark ETL jobs is value of Timeout."
            idle_timeout: input.idle_timeout.unwrap_or(timeout),
            tags: input.tags.clone().unwrap_or_default(),
            // Nothing has been run on a session its create has just returned.
            statements: Vec::new(),
            next_statement_id: FIRST_STATEMENT_ID,
        }
    }

    /// The instant this session's own timeouts fall due: the earlier of the two the model
    /// documents — [`Self::timeout`] minutes after it was created ("The number of minutes
    /// before session times out") and [`Self::idle_timeout`] minutes after it was last
    /// active ("The number of minutes when idle before the session times out").
    ///
    /// `None` when neither instant is representable, which leaves the session running.
    pub fn times_out_at(&self) -> Option<SystemTime> {
        let after = |from: SystemTime, minutes: i32| {
            let minutes = u64::try_from(minutes).ok()?;
            from.checked_add(Duration::from_secs(minutes * SECONDS_PER_MINUTE))
        };
        let whole_life = after(self.created_on, self.timeout);
        let idle = after(self.last_active_on, self.idle_timeout);
        match (whole_life, idle) {
            (Some(whole_life), Some(idle)) => Some(whole_life.min(idle)),
            (due, None) | (None, due) => due,
        }
    }

    /// Move this session into the status the clock owed it at `now`, if it owed one.
    ///
    /// Three changes are due by the clock alone, so materialising them on a read reports the
    /// status the clock had already reached and two reads of a settled session agree: the
    /// provisioning that was due to finish at [`Self::settles_at`], the tear-down a stop
    /// began that was due to finish at that same instant ([`Self::stop`]), and the timeout
    /// that was due at [`Self::times_out_at`]. A session already resting in a status nothing
    /// but a caller can move it out of is untouched.
    pub fn settle_if_due(&mut self, now: SystemTime) {
        if let Some(due) = self.settles_at.filter(|due| *due <= now) {
            self.settles_at = None;
            self.status = self.settles_to.clone();
            if self.status == SESSION_STATUS_STOPPED {
                // The session ended when its tear-down fell due, not when somebody read it.
                self.completed_on = Some(due);
            }
        }
        if self.status != SESSION_STATUS_READY {
            return;
        }
        if let Some(due) = self.times_out_at().filter(|due| *due <= now) {
            self.status = SESSION_STATUS_TIMEOUT.to_string();
            // The session ended when the timeout fell due, not when somebody read it.
            self.completed_on = Some(due);
        }
    }

    /// Begin taking this session's environment down, as of `now`.
    ///
    /// The session enters [`SESSION_STATUS_STOPPING`] and the clock owes it
    /// [`SESSION_STATUS_STOPPED`] [`SESSION_SETTLES_AFTER`] out, exactly as a create owes it
    /// [`SESSION_STATUS_READY`]: the span is not zero, so a caller reading straight after the
    /// stop still observes the environment being taken down and the refusals that go with a
    /// session that is not ready.
    ///
    /// A runtime that is being taken down takes no further work, so every statement still in
    /// flight on it is cancelled here ([`StatementEntry::cancel`]) rather than left to settle
    /// on an environment that is no longer running it.
    pub fn stop(&mut self, now: SystemTime) {
        self.status = SESSION_STATUS_STOPPING.to_string();
        self.settles_at = Some(now.checked_add(SESSION_SETTLES_AFTER).unwrap_or(now));
        self.settles_to = SESSION_STATUS_STOPPED.to_string();
        for statement in &mut self.statements {
            statement.cancel(now);
        }
    }

    /// `Session$ExecutionTime`: "The total time the session ran for", in seconds — the span
    /// from the instant it was created to the instant it completed, and 0 for a session that
    /// has not completed, since no time has been accounted to it yet.
    pub(super) fn execution_time(&self) -> f64 {
        self.completed_on
            .and_then(|completed_on| completed_on.duration_since(self.created_on).ok())
            .map(|ran| ran.as_secs_f64())
            .unwrap_or_default()
    }

    /// Rebuild the modelled `Session` shape from what is stored, so every reader of a
    /// session reports the same record.
    pub fn to_session(&self) -> Session {
        let execution_time = self.execution_time();
        Session {
            id: Some(self.id.clone()),
            created_on: Some(to_date_time(self.created_on)),
            status: Some(self.status.clone()),
            // "The error message displayed during the session": a session this store
            // accepted has none.
            error_message: None,
            description: self.description.clone(),
            role: Some(self.role.clone()),
            command: Some(self.command.clone()),
            default_arguments: Some(self.default_arguments.clone()),
            connections: self.connections.clone(),
            // "The code execution progress of the session", which is the model's own
            // default until a statement of this session has run code.
            progress: 0.0,
            max_capacity: self.max_capacity,
            security_configuration: self.security_configuration.clone(),
            glue_version: self.glue_version.clone(),
            number_of_workers: self.number_of_workers,
            worker_type: self.worker_type.clone(),
            completed_on: self.completed_on.map(to_date_time),
            execution_time: Some(execution_time),
            // "The DPUs consumed by the session (formula: ExecutionTime * MaxCapacity)": the
            // time accounted to the session at the capacity it holds, which is none for a
            // session that has not run and none for one holding no DPU figure.
            dpu_seconds: Some(execution_time * self.max_capacity.unwrap_or_default()),
            idle_timeout: Some(self.idle_timeout),
            // "The number of minutes before session times out" (`CreateSession$Timeout`):
            // the span the create named, else the default this store fills in
            // ([`DEFAULT_SESSION_TIMEOUT_MINUTES`]) — the same figure the IDLE leg of the
            // timeout defaults to, and the one this session really ends at
            // ([`Self::times_out_at`]), so a client that reads a session back can confirm
            // the timeout it configured.
            timeout: Some(self.timeout),
            // "The name of an Glue usage profile associated with the session": a session
            // create carries no profile member, so no session this store holds has one.
            profile_name: None,
            session_type: Some(self.session_type.clone()),
        }
    }

    /// Move every statement of this session into the state the clock owed it at `now`, if it
    /// owed one.
    ///
    /// This is the one write a reporting operation makes, for the reason
    /// [`Self::settle_if_due`] gives: a statement was due to leave the state its run put
    /// it in at [`StatementEntry::settles_at`] whether or not anybody called.
    pub fn settle_due_statements(&mut self, now: SystemTime) {
        for statement in &mut self.statements {
            statement.settle_if_due(now);
        }
    }

    /// The statement of this session under `id`, or `None` when it has none.
    ///
    /// Call [`Self::settle_due_statements`] first, or the record answered carries the
    /// state it was in before the clock moved it.
    pub fn statement(&self, id: i32) -> Option<&StatementEntry> {
        self.statements.iter().find(|statement| statement.id == id)
    }

    /// The statement of this session under `id`, to record what its own execution came to on
    /// ([`StatementEntry::record_execution`]).
    pub fn statement_mut(&mut self, id: i32) -> Option<&mut StatementEntry> {
        self.statements
            .iter_mut()
            .find(|statement| statement.id == id)
    }

    /// CLAIM the execution of the next statement of this session that is still awaiting one,
    /// and answer its `Id`.
    ///
    /// A statement whose code nothing has run yet is marked
    /// ([`StatementEntry::awaiting_execution`]), so this is what the execution path asks for
    /// to find the work it owes ([`super::statement_execution`]). The claim is taken under the
    /// store's guard and cleared here, so two calls that find the same statement owed an
    /// execution cannot both run it; a claim nothing performs is given back
    /// ([`Self::release_statement_claim`]).
    pub fn claim_statement_awaiting_execution(&mut self) -> Option<i32> {
        let statement = self
            .statements
            .iter_mut()
            .find(|statement| statement.awaiting_execution)?;
        statement.awaiting_execution = false;
        Some(statement.id)
    }

    /// Give the claim on the execution of the statement `id` back, for an execution nothing
    /// performed: it stays owed, so a later call hands it off.
    pub fn release_statement_claim(&mut self, id: i32) {
        if let Some(statement) = self.statement_mut(id) {
            if !statement.is_terminal() {
                statement.awaiting_execution = true;
            }
        }
    }

    /// Whether a statement of this session has not finished, which is what makes the
    /// session busy: the remote runtime is "currently busy processing another request"
    /// (`SessionBusyException`) until the statement it is running comes to rest.
    ///
    /// Call [`Self::settle_due_statements`] first, or a statement the clock has already
    /// finished still counts as in flight.
    pub fn has_statement_in_flight(&self) -> bool {
        self.statements
            .iter()
            .any(|statement| !statement.is_terminal())
    }

    /// Record `code` as the next statement of this session, and answer the `Id` it was
    /// given — "the Id of the statement that was run" (`RunStatementResponse$Id`).
    ///
    /// Running a statement is what a session is used for, so it is also the activity the
    /// IDLE leg of its timeout is measured from ([`Self::times_out_at`]). At most
    /// [`MAX_STATEMENTS_PER_SESSION`] records are kept, the oldest dropped first, exactly
    /// as a job keeps its runs; the id sequence is unaffected by that drop.
    pub fn run_statement(&mut self, code: &str, now: SystemTime) -> i32 {
        let id = self.next_statement_id;
        self.next_statement_id = id.saturating_add(1);
        self.last_active_on = now;
        self.statements.push(StatementEntry::run(id, code, now));
        if self.statements.len() > MAX_STATEMENTS_PER_SESSION {
            let overflow = self.statements.len() - MAX_STATEMENTS_PER_SESSION;
            self.statements.drain(..overflow);
        }
        id
    }
}

/// The `StatementState` a statement is in the instant its run returns: the session has
/// taken it and nothing has begun actioning it yet. One of the modelled `StatementState`
/// values, whose published list the generated
/// [`crate::ops::types::STATEMENT_STATE_VALUES`] carries (`WAITING`, `RUNNING`,
/// `AVAILABLE`, `CANCELLING`, `CANCELLED`, `ERROR`).
pub const STATEMENT_STATE_WAITING: &str = "WAITING";

/// The `StatementState` of a statement whose code is being run on the session's runtime.
/// See [`STATEMENT_STATE_WAITING`].
pub const STATEMENT_STATE_RUNNING: &str = "RUNNING";

/// The `StatementState` of a statement whose code ran and whose output is readable. See
/// [`STATEMENT_STATE_WAITING`].
pub const STATEMENT_STATE_AVAILABLE: &str = "AVAILABLE";

/// The `StatementState` of a statement a cancel stopped before it ran. See
/// [`STATEMENT_STATE_WAITING`].
pub const STATEMENT_STATE_CANCELLED: &str = "CANCELLED";

/// The `StatementState` of a statement that ended without producing output. See
/// [`STATEMENT_STATE_WAITING`].
pub const STATEMENT_STATE_ERROR: &str = "ERROR";

/// The `StatementState` values a statement never moves out of again: of the modelled
/// enum quoted on [`STATEMENT_STATE_WAITING`], the ones that name a finished statement.
/// The other three (`WAITING`, `RUNNING`, `CANCELLING`) each still owe a further state.
pub const TERMINAL_STATEMENT_STATES: [&str; 3] = [
    STATEMENT_STATE_AVAILABLE,
    STATEMENT_STATE_CANCELLED,
    STATEMENT_STATE_ERROR,
];

/// How long a statement stays in the state its run put it in before the clock owes it its
/// terminal one.
///
/// CHOSEN, not documented: Glue publishes no duration for actioning a statement, so this
/// is the emulator's own span, the same one a session's provisioning takes
/// ([`SESSION_SETTLES_AFTER`]) — deliberately not zero, so a caller that runs a second
/// statement straight after the first still observes the session busy. The due instant is
/// stored on the record and materialised by whichever later call reads it; nothing sleeps.
pub const STATEMENT_SETTLES_AFTER: Duration = SESSION_SETTLES_AFTER;

/// The `Id` the first statement of a session is given, which a session's ids then count
/// up from.
pub const FIRST_STATEMENT_ID: i32 = 1;

/// How many statements one session keeps.
///
/// CHOSEN: Glue publishes no retention for statement records, so this is the emulator's
/// own window, for the reason [`MAX_RUNS_PER_JOB`] gives: it bounds
/// [`SessionEntry::statements`], and with it every per-session walk. The OLDEST record is
/// dropped first, so the statements a caller is most likely to read are kept.
pub const MAX_STATEMENTS_PER_SESSION: usize = 100;

/// One statement run on a session: "the statement or request for a particular action to
/// occur in a session" (`Statement`), as the run that submitted it recorded it and as its
/// own execution left it.
///
/// Everything a caller can observe through the modelled `Statement` shape is held here —
/// the id the session minted for it, the code it was submitted with, the state it has
/// reached, the instants it moved between and what its execution came to.
#[derive(Debug, Clone)]
pub struct StatementEntry {
    /// `Statement$Id`: "The ID of the statement", minted by
    /// [`SessionEntry::run_statement`].
    pub id: i32,
    /// `Statement$Code`: "The execution code of the statement", exactly as the run sent
    /// it.
    pub code: String,
    /// One of the modelled `StatementState` values: "The state while request is actioned."
    pub state: String,
    /// `Statement$StartedOn`: the instant the run recorded this statement.
    pub started_on: SystemTime,
    /// `Statement$CompletedOn`: the instant it reached a terminal state, and `None` until
    /// it has.
    pub completed_on: Option<SystemTime>,
    /// The instant the clock owes this statement its next state; `None` once it has
    /// settled.
    pub settles_at: Option<SystemTime>,
    /// The `StatementState` the clock owes this statement at [`Self::settles_at`].
    pub settles_to: String,
    /// Whether this statement's code still owes an EXECUTION: nothing has run it yet.
    ///
    /// Set by the run that recorded the statement, so a statement nothing has run is found
    /// by [`SessionEntry::claim_statement_awaiting_execution`] and performed by whichever
    /// call finds it owed ([`super::statement_execution`]).
    pub awaiting_execution: bool,
    /// The instant the state this statement's own execution decided falls due, once that
    /// execution has answered and the state the clock owed it first has not been reached yet
    /// ([`Self::record_execution`]).
    pub rests_at: Option<SystemTime>,
    /// What this statement's own execution came to, which is what decides the state it comes
    /// to REST in and the output it then reports; `None` while nothing has run it.
    pub outcome: Option<StatementOutcome>,
}

impl StatementEntry {
    /// Record a statement submitted at `now` under `id`, in [`STATEMENT_STATE_WAITING`]
    /// with [`STATEMENT_STATE_RUNNING`] due [`STATEMENT_SETTLES_AFTER`] out and its code
    /// owing an execution.
    pub fn run(id: i32, code: &str, now: SystemTime) -> Self {
        Self {
            id,
            code: code.to_string(),
            state: STATEMENT_STATE_WAITING.to_string(),
            started_on: now,
            completed_on: None,
            settles_at: Some(now.checked_add(STATEMENT_SETTLES_AFTER).unwrap_or(now)),
            // The session takes the statement and then actions it, so the state the clock
            // owes it next is the one it is in while its code runs; what it comes to REST in
            // is its own execution's answer and nothing else's.
            settles_to: STATEMENT_STATE_RUNNING.to_string(),
            awaiting_execution: true,
            rests_at: None,
            outcome: None,
        }
    }

    /// Whether this statement has finished, by [`TERMINAL_STATEMENT_STATES`].
    pub fn is_terminal(&self) -> bool {
        TERMINAL_STATEMENT_STATES.contains(&self.state.as_str())
    }

    /// Record what this statement's own EXECUTION came to, as of `now`.
    ///
    /// The statement does not go straight to the state that execution decided: it is
    /// [`STATEMENT_STATE_WAITING`] until the span its run owes it passes,
    /// [`STATEMENT_STATE_RUNNING`] from then until one further span after its code answered,
    /// and only then the state that execution came to. So what is recorded here is the state
    /// it comes to REST in and WHEN — one span after `RUNNING` begins, and never before one
    /// span after the execution answered, so code that takes a while keeps its statement
    /// running for as long as it takes — while the `RUNNING` transition the clock already
    /// owed stands. A client polling the statement observes it running whether its code was
    /// quick or slow.
    ///
    /// A statement that has already come to rest is not moved.
    pub fn record_execution(&mut self, outcome: StatementOutcome, now: SystemTime) {
        // Whatever happens below, this statement no longer owes its code an execution: it
        // has had one.
        self.awaiting_execution = false;
        if self.is_terminal() {
            return;
        }
        // When this statement is (or becomes) RUNNING: the instant the clock owes it that
        // state, or the instant it entered it where that has already happened.
        let running_from = self.settles_at.unwrap_or(self.started_on);
        let rests_at = running_from
            .max(now)
            .checked_add(STATEMENT_SETTLES_AFTER)
            .unwrap_or(now);
        self.outcome = Some(outcome);
        if self.settles_at.is_some() {
            // The `RUNNING` transition is still owed, so the rest falls due after it.
            self.rests_at = Some(rests_at);
        } else {
            // The statement is already running: the rest is the next state it is owed.
            self.settles_at = Some(rests_at);
            self.settles_to = self.rests_in().to_string();
        }
    }

    /// End this statement [`STATEMENT_STATE_CANCELLED`] as of `now`: the session's runtime is
    /// no longer running it, so nothing further is owed to it — the state the model names for
    /// "a statement a cancel stopped before it ran".
    ///
    /// Its `Output` is then the one [`Self::output`] builds for a statement whose code nothing
    /// ran: the state it ended in and its execution count, carrying no data and no error,
    /// because there is none to report and none is invented.
    ///
    /// A statement that has already come to rest keeps the state it reached: its code ran, and
    /// its own execution decided what it came to.
    pub fn cancel(&mut self, now: SystemTime) {
        if self.is_terminal() {
            return;
        }
        // The code of a cancelled statement is not run: the execution it owed is owed no
        // longer, so no later call picks it up.
        self.awaiting_execution = false;
        self.settles_at = None;
        self.rests_at = None;
        self.settles_to = STATEMENT_STATE_CANCELLED.to_string();
        self.state = STATEMENT_STATE_CANCELLED.to_string();
        self.completed_on = Some(now);
    }

    /// The `StatementState` this statement comes to REST in: the one its own execution
    /// decided ([`StatementOutcome::statement_state`]).
    ///
    /// A statement whose execution answered nothing at all has none to report, and the
    /// modelled error state is what the service rests such a statement in.
    fn rests_in(&self) -> &'static str {
        self.outcome
            .as_ref()
            .map_or(STATEMENT_STATE_ERROR, StatementOutcome::statement_state)
    }

    /// Move this statement into the state the clock owed it at `now`, if it owed one.
    ///
    /// A statement can be owed MORE than one state at once: one whose code has been executed
    /// owes [`STATEMENT_STATE_RUNNING`] and then the state that execution came to. Every
    /// state already due is materialised, which is the state the clock itself had reached; a
    /// state not yet due stops the walk, so a caller reading inside the span the statement
    /// spends running still sees it running. The instants recorded are the DUE instant, not
    /// the instant somebody read it, so two reads of a settled statement report the same
    /// record.
    pub fn settle_if_due(&mut self, now: SystemTime) {
        while let Some(due) = self.settles_at {
            if due > now {
                return;
            }
            self.settles_at = None;
            self.state = self.settles_to.clone();
            if self.is_terminal() {
                self.completed_on = Some(due);
            }
            // The state this statement's own execution decided follows the one it has just
            // reached, at the instant that execution made it due.
            if let Some(rests_at) = self.rests_at.take() {
                self.settles_at = Some(rests_at);
                self.settles_to = self.rests_in().to_string();
            }
        }
    }

    /// `Statement$Output`: "The output in JSON", which a statement has once it has
    /// finished — and which is what its own execution came to: the text the code wrote, or
    /// the error it raised.
    pub(super) fn output(&self) -> Option<StatementOutput> {
        if !self.is_terminal() {
            return None;
        }
        let (data, error_name, error_value, traceback) = match self.outcome.as_ref() {
            Some(StatementOutcome::Available(text)) => (
                // "The code execution output in text format": what the code itself wrote.
                (!text.is_empty()).then(|| StatementOutputData {
                    text_plain: Some(text.clone()),
                }),
                None,
                None,
                None,
            ),
            Some(StatementOutcome::Errored {
                name,
                value,
                traceback,
            }) => (
                None,
                Some(name.clone()),
                Some(value.clone()),
                (!traceback.is_empty()).then(|| traceback.clone()),
            ),
            // A statement nothing ran reports no output of its own: there is none, and none
            // is invented for it.
            None => (None, None, None, None),
        };
        Some(StatementOutput {
            data,
            // "The execution count of the output": which execution of this session's
            // runtime produced it, which is the position of this statement in the sequence
            // the session mints ([`SessionEntry::next_statement_id`]).
            execution_count: self.id,
            // "The status of the code execution output", which is the state the statement
            // came to rest in.
            status: Some(self.state.clone()),
            error_name,
            error_value,
            traceback,
        })
    }

    /// Rebuild the modelled `Statement` shape from what is stored, so every reader of a
    /// statement reports the same record.
    pub fn to_statement(&self) -> Statement {
        Statement {
            id: self.id,
            code: Some(self.code.clone()),
            state: Some(self.state.clone()),
            output: self.output(),
            // "The code execution progress", which is the model's own default for a
            // statement whose code nothing ran.
            progress: 0.0,
            started_on: to_unix_millis(self.started_on),
            // The model's own default of 0 while the statement has not completed.
            completed_on: self.completed_on.map(to_unix_millis).unwrap_or_default(),
        }
    }
}
