// SPDX-License-Identifier: CC0-1.0

//! The calculations submitted into this account's notebook sessions.
//!
//! A calculation lives in the [`CalculationRegistry`] of
//! [`ComAmazonawsAthenaAmazonathenaState::calculations`]: `StartCalculationExecution` writes
//! a [`CalculationRecord`] there and `GetCalculationExecutionCode` answers out of that same
//! record, so the code a read reports is the code the submission persisted rather than
//! anything rebuilt from the reading request.
//!
//! WHICH SESSION may take one is the session's own notebook. "Submits calculations for
//! execution within a session" (`StartCalculationExecution`) and the code block it carries
//! is "a string that contains the code of the calculation" — the code of a cell of the
//! notebook the session's Jupyter server serves, which is the notebook the START of the
//! session bound it to ([`NOTEBOOK_ID_CONFIG`]). A session started for no notebook serves
//! none ([`crate::state::notebook_url`] signs no address for it either), so it runs no
//! calculation, and a submission addressed to one is refused in the sentence every
//! session-addressed request this service will not act on is refused with
//! ([`SESSION_NOT_FOUND`]).
//!
//! A submitted calculation is stored COMPLETE in the state its submission reached:
//! "CREATED - The calculation has been created and is ready to run"
//! ([`CALCULATION_STATE_CREATED`]). Nothing here moves it on, because nothing here runs it:
//! the Spark engine that would execute the code block is not part of this emulator yet, so
//! no record claims a run, a result or a duration for work nothing performed.

use std::collections::{BTreeMap, VecDeque};
use std::time::SystemTime;

use super::*;

/// The `CalculationExecutionState` of a calculation that has been created: "CREATED - The
/// calculation has been created and is ready to run"
/// (`StartCalculationExecutionResponse$State`). One of
/// `CALCULATION_EXECUTION_STATE_VALUES` in [`crate::ops::types`].
pub const CALCULATION_STATE_CREATED: &str = "CREATED";

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
    /// The notebook the session serves, which is the notebook this calculation is a cell
    /// of (see the module docs).
    pub notebook_id: String,
    /// "A description of the calculation" (`StartCalculationExecutionRequest$Description`),
    /// as the submission carried it.
    pub description: Option<String>,
    /// "A string that contains the code of the calculation"
    /// (`StartCalculationExecutionRequest$CodeBlock`), which is what
    /// `GetCalculationExecutionCode` answers with.
    pub code_block: String,
    /// When the calculation was submitted.
    pub submitted_at: SystemTime,
}

impl CalculationRecord {
    /// The `CalculationExecutionState` this calculation reads as.
    ///
    /// The state the submission stored, and the only one it can be in: a calculation is
    /// created ready to run, and nothing in this service runs it (see the module docs), so
    /// no read derives a later state from the clock.
    pub fn state(&self) -> &'static str {
        CALCULATION_STATE_CREATED
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
}

/// A fresh `CalculationExecutionId`: "The calculation execution UUID"
/// (`GetCalculationExecutionRequest$CalculationExecutionId`), a version-4 UUID in its
/// hyphenated form, which is inside the modelled `CalculationExecutionId` length (1..36).
/// Minted from the maintained crate rather than laid out here.
pub fn new_calculation_execution_id() -> String {
    ::uuid::Uuid::new_v4().as_hyphenated().to_string()
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

/// The refusal a request addressing ONE calculation this service will not act on is
/// answered with: the SAME refusal a session-addressed request takes
/// ([`session_not_authorized`]), named here for the resource the operation resolves — a
/// calculation is addressed inside the session it was submitted into, so Athena states the
/// same class and the same sentence for both.
pub fn calculation_not_authorized<E>() -> crate::scaffolded::Scaffolded<E> {
    session_not_authorized()
}
