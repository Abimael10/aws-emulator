// SPDX-License-Identifier: CC0-1.0

//! The sessions this account's workgroups run, and the notebook a notebook session was
//! started in.
//!
//! A session lives in the [`SessionRegistry`] of
//! [`ComAmazonawsAthenaAmazonathenaState::sessions`]: the request that starts one writes a
//! [`SessionRecord`] there and `ListNotebookSessions` answers out of those same records, so
//! every session a listing reports is one some earlier write persisted rather than
//! anything derived from the reading request.
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

use crate::ops::types::NotebookSessionSummary;

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
/// `ListNotebookSessions` names them: it "Lists, in descending order, the sessions that
/// have been created in a notebook that are in an active state like CREATING, CREATED,
/// IDLE or BUSY" — so a session in any other state (one shutting down, one whose resources
/// are gone, or one that ended badly) is not part of a notebook's session listing.
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

/// How long after a session is started its resources are DUE.
///
/// Athena hands back a session in a non-terminal state — `StartSession` answers with a
/// `State` of its own — and the resources arrive later, so this is a transition the CLOCK
/// owes: the instant is stored on the record and every read derives the state from it. The
/// span is this emulator's, not a duration AWS publishes (a real session takes as long as
/// its coordinator takes to come up), and it is never zero — a caller reading straight back
/// must still observe the documented CREATING state.
pub const SESSION_CREATION: Duration = Duration::from_secs(3);

/// How long after a session is asked to shut down the shutdown is complete: the same kind
/// of emulator-chosen span as [`SESSION_CREATION`], so the documented TERMINATING state a
/// session passes through is observable rather than skipped.
pub const SESSION_TERMINATION: Duration = Duration::from_secs(3);

/// How many sessions this emulator retains for one account.
///
/// The EMULATOR's own bound, with no counterpart in the model: a terminated session stays
/// readable, so without a bound a sequence of starts would grow the emulator's heap
/// without limit. The OLDEST session is dropped once this many are held, exactly as the
/// query-execution history is bounded.
pub const MAX_SESSIONS: usize = 1000;

/// One session, as this service persists it.
///
/// The state is NOT stored: it follows from the stored instants ([`Self::state_at`]), so a
/// read derives it from the clock instead of a reader having to move the record on.
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
}

impl SessionRecord {
    /// The record a start of `id` in `work_group` at `now` registers: its resources not
    /// acquired yet, and no shutdown asked for.
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
            created_at: now,
            resources_due_at: due_at(now, SESSION_CREATION),
            terminate_requested_at: None,
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

    /// Ask the session to shut down at `now`: the shutdown is complete
    /// [`SESSION_TERMINATION`] later, so the session is [`SESSION_STATE_TERMINATING`]
    /// until then. A session already asked keeps the instant it was first asked at — a
    /// repeat does not restart the shutdown.
    pub fn terminate(&mut self, now: SystemTime) {
        self.terminate_requested_at = Some(self.terminate_requested_at.unwrap_or(now));
    }

    /// The record as the modelled `NotebookSessionSummary` a notebook's session listing
    /// reports: "the notebook session ID and notebook session creation time".
    pub fn to_notebook_session_summary(&self) -> NotebookSessionSummary {
        NotebookSessionSummary {
            session_id: Some(self.id.clone()),
            creation_time: Some(::aws_smithy_types::DateTime::from(self.created_at)),
        }
    }
}

/// Every session this account holds.
///
/// A struct of its own rather than loose fields on the store, so the bound above is applied
/// in the ONE place a session is written and the newest-first order a listing reports is
/// derived in the ONE place a session is looked up, and neither can be honoured on one
/// operation's path and skipped on another's.
#[derive(Debug, Default)]
pub struct SessionRegistry {
    /// The sessions, keyed by `SessionId`. A `BTreeMap` so a walk of the registry is in one
    /// stable order.
    sessions: BTreeMap<String, SessionRecord>,
    /// The ids of [`Self::sessions`] in the order they were started, oldest first — the
    /// order a notebook's listing reports reversed, and the order sessions are dropped in
    /// once [`MAX_SESSIONS`] are held.
    order: Vec<String>,
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
    /// a bounded number of records rather than every session the account ever started.
    pub fn insert(&mut self, record: SessionRecord) {
        while self.order.len() >= MAX_SESSIONS {
            let oldest = self.order.remove(0);
            self.sessions.remove(&oldest);
        }
        let id = record.id.clone();
        self.order.retain(|held| held != &id);
        self.order.push(id.clone());
        self.sessions.insert(id, record);
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
}

/// A fresh `SessionId`: a version-4 UUID in its hyphenated, lower-case form, well inside
/// the modelled `SessionId` length (1..256). Minted from the maintained crate rather than
/// laid out here.
pub fn new_session_id() -> String {
    ::uuid::Uuid::new_v4().as_hyphenated().to_string()
}

/// The cursor a `ListNotebookSessions` page hands back: the notebook it listed the sessions
/// of and the position to resume at.
fn notebook_sessions_cursor(notebook_id: &str, position: usize) -> String {
    format!("notebooksessions{CURSOR_SEPARATOR}{notebook_id}{CURSOR_SEPARATOR}{position}")
}

impl ComAmazonawsAthenaAmazonathenaState {
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

/// `now` plus `span`, or `now` when that instant is not representable — so a clock far
/// enough in the future to overflow yields a transition that is already due rather than a
/// panic.
fn due_at(now: SystemTime, span: Duration) -> SystemTime {
    now.checked_add(span).unwrap_or(now)
}
