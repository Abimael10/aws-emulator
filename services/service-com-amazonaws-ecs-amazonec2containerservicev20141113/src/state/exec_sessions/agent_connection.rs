// SPDX-License-Identifier: CC0-1.0

//! Whether a task's execute-command agent can REACH the Systems Manager channel at all: the
//! IAM question the agent's own connection turns on.
//!
//! # The rule this module implements
//!
//! The agent Amazon ECS runs inside the containers of an exec-enabled task opens its control
//! and data channels to Systems Manager AS THE TASK ROLE — "the Amazon ECS Exec feature
//! requires a task IAM role to grant containers the permissions needed for communication
//! between the managed SSM agent and the SSM service", which is the
//! `ssmmessages:CreateControlChannel`, `ssmmessages:CreateDataChannel`,
//! `ssmmessages:OpenControlChannel` and `ssmmessages:OpenDataChannel` actions
//! ([`SSM_MESSAGES_CHANNEL_ACTIONS`]). A task whose role does not allow them therefore has an
//! agent that can never connect: its `ManagedAgent$lastStatus` does not reach
//! [`STATUS_RUNNING`], and `ExecuteCommand` on it earns the one sentence Amazon ECS states
//! both agent conditions in ([`super::EXECUTE_COMMAND_NOT_AVAILABLE_MESSAGE`]).
//!
//! # How the question is answered, and when
//!
//! It is put to IAM itself — the simulation of the role's own policies, over the in-process
//! hop this workspace keeps ONE route to
//! ([`::service_common::iam_role::role_withholds_action_through`]) — so the documents in force
//! are collected and decided by the service that HOLDS them. The answer is recorded on the
//! task once ([`ExecPlane::agent_connections`]) and the clock reads it back, because the
//! agent's own status is a transition the clock materialises
//! ([`super::super::execute_command_agent`]) and a store pass can make no call.
//!
//! WITHHOLDING needs an ANSWER about the role, not the absence of one: a value that names no
//! role of this account, a role this emulator's IAM holds nothing under (a task role is
//! routinely provisioned out of band, so its absence is evidence about this store and not
//! about the role), and any failure of the hop are each "nothing was learned" and leave the
//! agent to connect — the same deliberate non-refusal every other reading of a
//! caller-supplied role in this workspace takes. What DOES withhold is a simulation that
//! answers and does not allow the action: the policies in force on a role this account holds
//! are the authority on what its agent may do, and an agent they do not authorize cannot
//! connect.

use ::std::time::SystemTime;

use crate::state::*;

/// The actions the execute-command agent opens its Systems Manager channels with, which the
/// TASK ROLE has to allow for the agent to connect: the two channels it creates and the two it
/// opens.
pub const SSM_MESSAGES_CHANNEL_ACTIONS: [&str; 4] = [
    "ssmmessages:CreateControlChannel",
    "ssmmessages:CreateDataChannel",
    "ssmmessages:OpenControlChannel",
    "ssmmessages:OpenDataChannel",
];

/// The resource those actions are evaluated against: the channel actions carry no resource of
/// their own, so a grant for them is a grant on every resource.
const SSM_MESSAGES_CHANNEL_RESOURCE: &str = "*";

/// One task whose agent's connection has not been decided yet.
#[derive(Debug, Clone)]
pub struct ExecChannelDecision {
    /// The task the decision is about.
    pub task_arn: String,
    /// The task role its agent would connect as, and `None` for a task that resolves none.
    pub role_arn: Option<String>,
    /// The Region the task was placed in, which the question is asked in.
    pub region: String,
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// Every exec-enabled task whose agent's connection this store has not decided yet.
    pub(in crate::state) fn tasks_awaiting_exec_channel_decision(
        &self,
    ) -> Vec<ExecChannelDecision> {
        self.tasks
            .values()
            .filter(|task| task.enable_execute_command)
            .filter(|task| !self.exec.agent_connections.contains_key(&task.task_arn))
            .map(|task| ExecChannelDecision {
                task_arn: task.task_arn.clone(),
                role_arn: self.launched_task_role_arn(task),
                region: super::super::request_arn::RequestArn::parse(&task.task_arn)
                    .map(|arn| arn.region.to_string())
                    .unwrap_or_default(),
            })
            .collect()
    }

    /// Record what IAM answered about one task's agent: whether it can reach the Systems
    /// Manager channel at all.
    ///
    /// Written once per task — the policies in force on a role are read when the agent
    /// connects, and an agent that connected stays connected for the life of the container.
    pub(in crate::state) fn record_exec_channel_decision(&mut self, task_arn: &str, reaches: bool) {
        if !self.tasks.contains_key(task_arn) {
            return;
        }
        self.exec
            .agent_connections
            .entry(task_arn.to_string())
            .or_insert(reaches);
    }

    /// Whether the execute-command agent of the task `task_arn` names can reach the Systems
    /// Manager channel: `false` until the decision has been taken, which is why the agent
    /// comes up a moment after its container does rather than with it.
    pub fn execute_command_channel_reaches(&self, task_arn: &str) -> bool {
        self.exec
            .agent_connections
            .get(task_arn)
            .copied()
            .unwrap_or(false)
    }
}

/// Decide the agent connection of every exec-enabled task that has not been decided yet.
///
/// Call it with NO guard of this store held, from a door that has just placed or read a task
/// or from the clock pump: it takes the lock itself to collect the tasks, and each decision is
/// an in-process task that outlives the call — the question is a call to the service that
/// holds the role's policies, which no store pass can make. With no async runtime to start
/// them on nothing is decided, and the agent of such a task stays where it is rather than
/// being reported connected on no answer at all.
pub fn settle_execute_command_agent_connections<Eng: Send + Sync + 'static>(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    engine: &::std::sync::Arc<Eng>,
) {
    let Some(state) = service_state(&**engine) else {
        return;
    };
    let Ok(runtime) = ::tokio::runtime::Handle::try_current() else {
        return;
    };
    let pending = state.lock().tasks_awaiting_exec_channel_decision();
    for decision in pending {
        let recorded = ::engine::ServiceState::clone(&state);
        let callback = callback.clone();
        runtime.spawn(async move {
            let reaches = channel_is_authorized(&callback, &decision).await;
            let mut store = recorded.lock();
            store.record_exec_channel_decision(&decision.task_arn, reaches);
            // The agent's own status follows from the answer, so the pass that materialises
            // it runs here rather than waiting for the next caller: a task whose role
            // authorizes the channel has a connected agent as soon as that is known.
            store.advance_clock(SystemTime::now());
        });
    }
}

/// Whether the role one task's agent would connect as authorizes the channel actions — see
/// the module docs for what counts as an answer.
async fn channel_is_authorized(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    decision: &ExecChannelDecision,
) -> bool {
    let Some(role_arn) = decision.role_arn.as_deref() else {
        // A task that resolves no task role is one nothing can be shown about here; whether
        // such a launch is accepted at all is the launch's own rule
        // ([`super::super::launch_role_trust`]).
        return true;
    };
    for action in SSM_MESSAGES_CHANNEL_ACTIONS {
        // The simulation ANSWERS only for a role this account's IAM holds: a role it holds
        // nothing under — a task role provisioned out of band — and any failure of the hop
        // leave nothing learned, which is what the shared reading answers `false` for.
        let withheld = ::service_common::iam_role::role_withholds_action_through(
            callback,
            &decision.region,
            role_arn,
            action,
            SSM_MESSAGES_CHANNEL_RESOURCE,
        )
        .await;
        if withheld {
            ::tracing::info!(
                task_arn = %decision.task_arn,
                %action,
                "the task role does not authorize the execute-command agent's Systems \
                 Manager channel, so its agent does not connect"
            );
            return false;
        }
    }
    true
}
