// SPDX-License-Identifier: CC0-1.0

//! The CHECKPOINT TOKEN one durable execution's next checkpoint must carry, and what a
//! checkpoint of it does to the execution.
//!
//! # The rule this module implements
//!
//! `CheckpointDurableExecution` "saves the progress of a durable function execution during
//! runtime", and the ORDER of those saves is carried by a token rather than by the caller:
//! `CheckpointToken` is "a unique token that identifies the current checkpoint state […] Each
//! checkpoint operation consumes this token and returns a new one", which "ensures that
//! checkpoints are applied in the correct order and prevents duplicate or out-of-order state
//! updates". So exactly one token is current for an execution at any instant: the one minted
//! when it started, or the one the last applied checkpoint rotated to
//! ([`DurableExecutionRecord::take_current_checkpoint_token`]). A checkpoint carrying any
//! other token — one already consumed, one of another execution, one a caller made up — is
//! not the current state's and is refused with [`INVALID_CHECKPOINT_TOKEN_MESSAGE`], leaving
//! the history exactly as it was. An execution whose history is CLOSED (one that succeeded,
//! failed, timed out or was stopped) has no current token at all, because nothing can be
//! appended to it any more.
//!
//! A `ClientToken` makes a repeat of the SAME request a repeat rather than a second
//! checkpoint: "an optional idempotency token to ensure that duplicate checkpoint requests are
//! handled correctly. If provided, Lambda uses this token to detect and handle duplicate
//! requests within a 15-minute window" — so the token one checkpoint answered is remembered
//! with the instant it was answered at ([`AnsweredCheckpoint`]) and a repeat inside
//! [`CHECKPOINT_IDEMPOTENCY_WINDOW`] is answered with it again, applying nothing a second
//! time. Past that window nothing is remembered, and the repeat is a checkpoint carrying a
//! consumed token like any other.
//!
//! The UPDATES a checkpoint carries are applied as the history events they are, by the one
//! appender the run's own checkpoints go through ([`super::durable_steps::append_checkpoint`]):
//! a checkpoint that reaches the history through the API and one the in-process runtime
//! reports become the same event, by the same rule.

use super::*;

/// How a checkpoint carrying a token that is not the execution's current one is refused.
///
/// The token names "the current checkpoint state", so a token that is not the current one
/// names no state to apply the checkpoint to: the value itself is what is wrong with the
/// request, which is the modelled `InvalidParameterValueException` ("one of the parameters in
/// the request is not valid"). The sentence names the member it refuses and carries nothing of
/// the request back.
pub const INVALID_CHECKPOINT_TOKEN_MESSAGE: &str = "Invalid checkpoint token";

/// How long a `ClientToken` identifies a duplicate checkpoint request for: "Lambda uses this
/// token to detect and handle duplicate requests within a 15-minute window"
/// (`CheckpointDurableExecution$ClientToken`).
pub const CHECKPOINT_IDEMPOTENCY_WINDOW: Duration = Duration::from_secs(15 * 60);

/// What one checkpoint was ANSWERED, kept so a duplicate request carrying the same
/// `ClientToken` is answered the same thing instead of being applied twice.
#[derive(Debug, Clone)]
pub struct AnsweredCheckpoint {
    /// The `ClientToken` the request carried, which is what identifies the duplicate.
    pub client_token: String,
    /// When that request was answered, from which the 15-minute window runs.
    pub at: SystemTime,
    /// The `CheckpointToken` it was answered with, and none for a checkpoint that closed the
    /// execution's history and so left no token for a next one.
    pub checkpoint_token: Option<String>,
}

/// A fresh checkpoint token.
///
/// The model's `CheckpointToken` is `@length(1, 2048)` and `^[A-Za-z0-9+/]+={0,2}$`, i.e.
/// base64 text, so the token is the base64 of a fresh UUID's bytes: unique per checkpoint and
/// inside that pattern. It is opaque to the caller, which only ever sends back the one it was
/// handed.
#[must_use]
pub fn mint_checkpoint_token() -> String {
    ::base64::Engine::encode(
        &::base64::engine::general_purpose::STANDARD,
        ::uuid::Uuid::new_v4().as_bytes(),
    )
}

impl DurableExecutionRecord {
    /// The token a checkpoint of this execution must carry right now, and `None` for an
    /// execution whose history is closed.
    ///
    /// A terminal execution takes no further checkpoint — "the execution transitions to
    /// STOPPED status and cannot be resumed", and a succeeded, failed or timed-out execution
    /// is just as final — so it has no current checkpoint state for one to be applied to.
    #[must_use]
    pub fn current_checkpoint_token(&self) -> Option<&str> {
        (self.status == EXECUTION_STATUS_RUNNING).then_some(self.checkpoint_token.as_str())
    }

    /// Whether `token` is the token a checkpoint of this execution must carry; see
    /// [`Self::current_checkpoint_token`].
    #[must_use]
    pub fn checkpoint_token_is_current(&self, token: &str) -> bool {
        self.current_checkpoint_token() == Some(token)
    }

    /// CONSUME this execution's current checkpoint token and report the one the next
    /// checkpoint must carry.
    ///
    /// "Each checkpoint operation consumes this token and returns a new one", so the stored
    /// token is replaced: the token the consumed checkpoint carried is no longer current, and
    /// a repeat of it is refused. An execution this checkpoint CLOSED — one whose updates
    /// settled it — has no next checkpoint, so none is minted and none is answered.
    pub fn take_current_checkpoint_token(&mut self) -> Option<String> {
        if self.status != EXECUTION_STATUS_RUNNING {
            // Nothing can be applied to a closed history, so the token this execution still
            // holds is not current (see [`Self::current_checkpoint_token`]) and no new one is
            // minted for a checkpoint that cannot come.
            return None;
        }
        self.checkpoint_token = mint_checkpoint_token();
        Some(self.checkpoint_token.clone())
    }

    /// What the checkpoint `client_token` identified was answered, when that request was
    /// answered inside the idempotency window ending at `now`.
    ///
    /// The answer is the `CheckpointToken` the first request of that client token was
    /// answered with, which the duplicate is answered again: the inner `Option` is that
    /// token, absent for a checkpoint that left none.
    #[must_use]
    pub fn answered_checkpoint(
        &self,
        client_token: &str,
        now: SystemTime,
    ) -> Option<Option<String>> {
        let answered = self.answered_checkpoint.as_ref()?;
        if answered.client_token != client_token {
            return None;
        }
        let elapsed = now.duration_since(answered.at).unwrap_or_default();
        (elapsed <= CHECKPOINT_IDEMPOTENCY_WINDOW).then(|| answered.checkpoint_token.clone())
    }

    /// Remember that the checkpoint `client_token` identified was answered `checkpoint_token`
    /// at `now`, so a duplicate request inside the window is answered the same thing.
    ///
    /// Only ONE checkpoint is remembered per execution: the window is bounded, and a client
    /// retries the checkpoint it has not yet been answered rather than an older one, so the
    /// record is the last answer rather than a log of every client token ever sent.
    pub fn remember_checkpoint(
        &mut self,
        client_token: &str,
        checkpoint_token: Option<String>,
        now: SystemTime,
    ) {
        self.answered_checkpoint = Some(AnsweredCheckpoint {
            client_token: client_token.to_string(),
            at: now,
            checkpoint_token,
        });
    }
}
