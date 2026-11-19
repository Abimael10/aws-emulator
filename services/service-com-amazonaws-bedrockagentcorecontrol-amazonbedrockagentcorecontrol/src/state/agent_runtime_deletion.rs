// SPDX-License-Identifier: CC0-1.0

//! The window an ACCEPTED agent-runtime deletion is under way in: the runtime stays
//! readable in `DELETING` until the removal the clock owes it comes due, and only then is
//! the record gone.
//!
//! `DeleteAgentRuntime` answers 202 carrying `status` (`AgentRuntimeStatus`), so the
//! deletion is asynchronous: the call ACCEPTS it, it does not carry it out. What a client
//! observes in that window is the point — a runtime still present and reporting `DELETING`,
//! a change asked of it refused as the conflict it is
//! ([`agent_runtime_delete_in_flight`], the modelled `ConflictException`), and a read
//! answering the modelled 404 only once the removal is due — because that is how a client
//! tells "busy, retry later" from "gone, stop".
//!
//! The removal is owed by the CLOCK: it was due at that instant whether or not anybody
//! called, so every operation that reads the runtime store materialises it first
//! ([`super::ComAmazonawsBedrockagentcorecontrolAmazonbedrockagentcorecontrolState::expire_deleted_agent_runtimes`])
//! and nothing ever sleeps for it. This is the same shape the gateway, the memory and the
//! browser profile families already record a deletion in; it lives here rather than in any
//! one `ops/` file so the delete, the guards and every read cannot drift apart.

use super::*;

/// How long an accepted agent-runtime deletion stays under way — the span the runtime
/// reports `DELETING` for before the clock owes its removal.
///
/// AWS documents no duration for it, so the span is chosen: long enough that a caller
/// acting straight after the delete (the next call on the same connection) still observes
/// the `DELETING` state and the conflict it answers, short enough to keep a client script
/// quick. Stored as an instant on the record and never slept on.
pub const AGENT_RUNTIME_REMOVAL_DELAY: Duration = Duration::from_secs(3);

/// How this service words the conflict a change asked of a runtime whose deletion is
/// already under way is: the runtime's ARN and the status the request found.
///
/// `UpdateAgentRuntime` models `ConflictException` (HTTP 409) beside
/// `ResourceNotFoundException` (404), and the two say different things to a client: the
/// runtime is busy going away, so the remedy is to wait rather than to stop. The sentence is
/// fixed; its one variable part is the ARN of the runtime the request addressed, which the
/// store holds.
pub fn agent_runtime_delete_in_flight(agent_runtime_arn: &str) -> ConflictException {
    ConflictException {
        message: Some(format!(
            "Cannot update agent {agent_runtime_arn} while it's {STATUS_DELETING}. Please try \
             again when the agent is ready."
        )),
    }
}

impl AgentRuntimeRecord {
    /// Whether this runtime's own deletion has been accepted and is still under way.
    pub fn is_deleting(&self) -> bool {
        self.removal_due_at.is_some()
    }

    /// Accept this runtime's deletion: it reports `DELETING` from now on, and the clock owes
    /// its removal [`AGENT_RUNTIME_REMOVAL_DELAY`] later.
    ///
    /// A deletion already under way is NOT re-armed — a repeat reports the same runtime
    /// going away rather than pushing its removal further out.
    pub fn begin_deleting(&mut self, now: SystemTime) {
        if !self.is_deleting() {
            self.removal_due_at = Some(now + AGENT_RUNTIME_REMOVAL_DELAY);
        }
    }

    /// The status this runtime reports for `version` at `now`.
    ///
    /// A runtime whose deletion is under way reports `DELETING` whatever its versions were
    /// doing: the resource is going away, which is the one thing a client polling it needs
    /// to see. Otherwise the version's own clock-driven status is what is reported.
    pub fn status_of<'v>(
        &self,
        version: &'v AgentRuntimeVersionRecord,
        now: SystemTime,
    ) -> &'v str {
        if self.is_deleting() {
            STATUS_DELETING
        } else {
            version.status_at(now)
        }
    }
}

impl ComAmazonawsBedrockagentcorecontrolAmazonbedrockagentcorecontrolState {
    /// Remove every agent runtime whose deletion the clock has already made due, with the
    /// name it reserved, the endpoints on it and the idempotency records naming it.
    ///
    /// Called by every operation that reads the runtime store, before it reads: the removal
    /// was owed at this instant whether or not anybody called, so a caller that polls the
    /// runtime sees `DELETING` and then the modelled 404 without anything sleeping for it.
    pub fn expire_deleted_agent_runtimes(&mut self, now: SystemTime) {
        let due: Vec<String> = self
            .agent_runtimes
            .iter()
            .filter(|(_, record)| record.removal_due_at.is_some_and(|at| now >= at))
            .map(|(id, _)| id.clone())
            .collect();
        for id in due {
            if let Some(record) = self.agent_runtimes.remove(&id) {
                self.runtime_names.remove(&record.agent_runtime_name);
                self.forget_client_tokens_for(&record.agent_runtime_id);
            }
        }
    }
}
