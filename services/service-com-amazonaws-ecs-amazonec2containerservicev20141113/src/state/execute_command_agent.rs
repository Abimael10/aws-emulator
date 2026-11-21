// SPDX-License-Identifier: CC0-1.0

//! The execute-command plane of a task: the managed agent Amazon ECS runs inside the
//! containers of a task whose launch turned the feature on, and the one fact
//! `ExecuteCommand` is decided by.
//!
//! # Who provisions the agent
//!
//! `ManagedAgent$name`: "When the execute command feature is turned on, the managed agent
//! name is `ExecuteCommandAgent`" — the agent is Amazon ECS's own, placed in the container
//! because the LAUNCH asked for the feature (`RunTask$enableExecuteCommand`, "whether to
//! use the execute command functionality for the containers in this task"). So a task
//! launched with the flag carries one agent per container and a task launched without it
//! carries none, and the agent is where its container is in the container's own lifecycle:
//! it starts with the container and ends with it.
//!
//! That makes the agent a transition the CLOCK owes a launched task, materialised by
//! [`super::ComAmazonawsEcsAmazonec2containerservicev20141113State::advance_clock`] rather
//! than by a caller's call — the agent came up when its container did, whether or not
//! anybody called to look. The ECS agent's own report of a managed agent
//! (`SubmitTaskStateChange$managedAgents`) is the newer word on it wherever it is further
//! along, which is the same reading of a stale report every other status of this service
//! takes ([`super::container_stage_rank`]).
//!
//! # What it decides
//!
//! Whether a container has such an agent at all is what `ExecuteCommand` reads: a session is
//! opened ON that agent, so a container without one is a container no session can be opened
//! on, which is the condition Amazon ECS states in one sentence
//! ([`super::EXECUTE_COMMAND_NOT_AVAILABLE_MESSAGE`]). What a session would be CARRIED over
//! — the Systems Manager data channel the `Session$streamUrl` names — is not something this
//! emulator serves, so a container that does have a running agent is a target that cannot be
//! connected to rather than a session (see `crate::ops::execute_command`).

use super::*;

/// `ManagedAgentName`'s one value, which is the name of the agent the execute command
/// feature runs in a container (`ManagedAgent$name`). Held here rather than read off the
/// modelled value set, so the one agent this plane is about is named rather than positioned
/// — the unit test below holds it to the model.
pub const EXECUTE_COMMAND_AGENT: &str = "ExecuteCommandAgent";

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// Bring the execute-command agent of every container of every exec-enabled task up to
    /// where its container has got to, as of `now`.
    ///
    /// A container of such a task carries exactly one agent record, created the first time
    /// this pass sees the container; from then on the agent is moved ON to the container's
    /// own stage and never back, so a stage the ECS agent itself has reported further along
    /// stands ([`super::container_stage_rank`]). `lastStartedAt` is "the time when the
    /// managed agent was last started", so it is stamped at the instant the agent first
    /// reaches [`STATUS_RUNNING`].
    pub(super) fn start_execute_command_agents(&mut self, now: SystemTime) {
        for task in self.tasks.values_mut() {
            if !task.enable_execute_command {
                continue;
            }
            let mut changed = false;
            for container in &mut task.containers {
                if !container
                    .managed_agents
                    .iter()
                    .any(|agent| agent.name == EXECUTE_COMMAND_AGENT)
                {
                    container.managed_agents.push(ManagedAgentRecord {
                        name: EXECUTE_COMMAND_AGENT.to_string(),
                        last_status: container.last_status.clone(),
                        reason: None,
                        last_started_at: (container.last_status == STATUS_RUNNING)
                            .then(|| reported_instant(now)),
                    });
                    changed = true;
                    continue;
                }
                let stage = container.last_status.clone();
                let Some(agent) = container
                    .managed_agents
                    .iter_mut()
                    .find(|agent| agent.name == EXECUTE_COMMAND_AGENT)
                else {
                    continue;
                };
                let onward = match (
                    container_stage_rank(&stage),
                    container_stage_rank(&agent.last_status),
                ) {
                    (Some(next), Some(reported)) => next > reported,
                    // A stage this service cannot rank is left exactly as it was reported:
                    // nothing here can tell whether it is ahead of the container's.
                    _ => false,
                };
                if !onward {
                    continue;
                }
                if stage == STATUS_RUNNING {
                    agent.last_started_at = Some(reported_instant(now));
                }
                agent.last_status = stage;
                changed = true;
            }
            if changed {
                // "Every time a task experiences a change ... the version counter is
                // incremented."
                task.version += 1;
            }
        }
    }

    /// Whether the container stored under `container_arn` of the task stored under
    /// `task_arn` is running an execute-command agent: the task was launched with execute
    /// command enabled and that container's agent reports [`STATUS_RUNNING`].
    ///
    /// Both halves are read from the store rather than from the request — the flag the
    /// launch recorded (`Task$enableExecuteCommand`) and the agent's own last known status
    /// (`ManagedAgent$lastStatus`) — so what a caller is told about a session is decided by
    /// the task's own state.
    pub fn execute_command_agent_is_running(&self, task_arn: &str, container_arn: &str) -> bool {
        self.tasks
            .get(task_arn)
            .filter(|task| task.enable_execute_command)
            .and_then(|task| {
                task.containers
                    .iter()
                    .find(|container| container.container_arn == container_arn)
            })
            .is_some_and(|container| {
                container.managed_agents.iter().any(|agent| {
                    agent.name == EXECUTE_COMMAND_AGENT && agent.last_status == STATUS_RUNNING
                })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The agent this plane is about is the one the model declares, so a model revision
    /// cannot leave the name behind.
    #[test]
    fn the_execute_command_agent_is_the_modelled_managed_agent() {
        assert!(
            crate::ops::types::MANAGED_AGENT_NAME_VALUES.contains(&EXECUTE_COMMAND_AGENT),
            "expected {EXECUTE_COMMAND_AGENT} to be a modelled ManagedAgentName value, got {:?}",
            crate::ops::types::MANAGED_AGENT_NAME_VALUES
        );
    }

    /// A store holding no task reports no running agent for any reference, so the gate
    /// answers off the STORE rather than off the names it was given.
    #[test]
    fn no_agent_is_running_for_a_task_this_store_does_not_hold() {
        let store = ComAmazonawsEcsAmazonec2containerservicev20141113State::default();
        for (task, container) in [("task", "container"), ("", ""), ("arn:aws:ecs:::task/c/1", "")] {
            assert!(
                !store.execute_command_agent_is_running(task, container),
                "case {task:?}/{container:?}: expected no agent for a task nothing stored"
            );
        }
    }
}
