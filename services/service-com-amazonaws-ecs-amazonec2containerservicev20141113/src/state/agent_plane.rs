// SPDX-License-Identifier: CC0-1.0

//! The AGENT plane: what a `Submit*StateChange` report may say.
//!
//! Those operations are the container agent's — "This action is only used by the Amazon ECS
//! agent, and it is not intended for use outside of the agent" — and Amazon ECS holds a
//! report to the VALUES it may carry before it records anything.
//! `SubmitTaskStateChange$status` and `AttachmentStateChange$status` are plain strings in the
//! model, so the accepted set is recorded behavior: a task is reported at one of the stages
//! of the ECS task lifecycle ([`AGENT_REPORT_TASK_STATUSES`], and a value outside it is
//! refused naming the value itself) and an attachment is reported ATTACHED and nothing else
//! ([`AGENT_REPORT_ATTACHMENT_STATUSES`], refused with the sentence this service words every
//! out-of-set value with, [`super::value_set_message`]) — the one transition the agent makes,
//! from the `ATTACHING` Amazon ECS leaves the interface at.
//!
//! WHO may report is a question only the TASK report asks: a task's own `lastStatus` is
//! written by the agent that RUNS it, and that agent is identified by the container instance
//! it is registered as, so a task placed on infrastructure with no container instance has no
//! agent identity to verify and its state report is refused
//! ([`unverified_task_report_violation`]). The rest of the family — the containers and
//! attachments a report describes — is about what the agent observed inside the task, which
//! the plane records about a task on any capacity.

use super::*;

/// The stages a task may be REPORTED at by its agent: the ECS task lifecycle, in the order a
//  task passes through it.
///
/// `SubmitTaskStateChange$status` is "The status of the state change request" with no
/// modelled value set, so this is the lifecycle the Amazon ECS developer guide publishes
/// ("Task lifecycle"): the stages this service also stores and ranks a retry against
/// ([`TASK_LIFECYCLE`]), plus the ones only the agent observes between them.
pub const AGENT_REPORT_TASK_STATUSES: [&str; 8] = [
    STATUS_PROVISIONING,
    STATUS_PENDING,
    "ACTIVATING",
    STATUS_RUNNING,
    STATUS_DEACTIVATING,
    "STOPPING",
    "DEPROVISIONING",
    STATUS_STOPPED,
];

/// The states an ATTACHMENT may be reported in: `ATTACHED` alone.
///
/// Amazon ECS creates the interface and takes it as far as `ATTACHING` itself; the agent is
/// what reports the attach it completed, so the one state change it sends is to
/// [`ATTACHMENT_STATUS_ATTACHED`]. Every other `Attachment$status` value is a state Amazon
/// ECS itself puts the attachment in, not one a report may name.
pub const AGENT_REPORT_ATTACHMENT_STATUSES: [&str; 1] = [ATTACHMENT_STATUS_ATTACHED];

/// The member `AttachmentStateChange$status` is named by in the refusal of a value outside
/// [`AGENT_REPORT_ATTACHMENT_STATUSES`].
pub const AGENT_REPORT_ATTACHMENT_STATUS_MEMBER: &str = "attachmentStatus";

/// `InvalidParameterException$message` for a task status a report names that is not one of
/// [`AGENT_REPORT_TASK_STATUSES`].
///
/// Amazon ECS states the value it could not read rather than the set it accepts, so the
/// variable part is the status the REQUEST carried and the rest is fixed text.
pub fn invalid_task_status_message(status: &str) -> String {
    format!("The status {status} is not a valid status.")
}

/// The refusal a task status a report NAMES earns, or `None` when it names one of
/// [`AGENT_REPORT_TASK_STATUSES`] (or none at all, which reports no stage).
pub fn agent_report_task_status_violation(status: Option<&str>) -> Option<String> {
    let reported = status?;
    (!AGENT_REPORT_TASK_STATUSES.contains(&reported))
        .then(|| invalid_task_status_message(reported))
}

/// `InvalidParameterException$message` for a report that takes a task BACKWARDS: the stage
/// it names is one the task has already passed.
///
/// Worded as the agent plane words a status it cannot read ([`invalid_task_status_message`]):
/// the value is a valid status, but not one this task can be in, and the sentence says which
/// stage it has already reached. Both parts are the request's and the record's, so nothing of
/// one recording is fixed into it.
pub fn backwards_task_status_message(reported: &str, reached: &str) -> String {
    format!("The status {reported} is not a valid status for a task in the {reached} status.")
}

/// The refusal a report of `reported` earns against a task that has reached `reached`, or
/// `None` when the report is one the plane may record.
///
/// The ORDER of the task lifecycle is server-enforced: a task passes through its stages in
/// one direction ([`TASK_LIFECYCLE`]) and ENDS at the last of them, so two reports describe a
/// transition that cannot have happened and are refused rather than recorded —
///
/// * one naming an EARLIER stage than the one the task has reached, which would run the
///   lifecycle backwards;
/// * any stage at all once the task has reached the terminal [`STATUS_STOPPED`], which makes
///   no further transition, so there is nothing a report can change about it.
///
/// That is what keeps `lastStatus` a sequence a client can rely on. A report naming the stage
/// a LIVE task is already at is the agent's retry of a report that landed, so it is recorded
/// (changing nothing) and acknowledged. A stage this lifecycle does not name is not ranked at
/// all, so the agent's word on it stands.
pub fn out_of_order_task_status_violation(reported: &str, reached: &str) -> Option<String> {
    if reached == STATUS_STOPPED {
        return Some(backwards_task_status_message(reported, reached));
    }
    match (task_stage_rank(reported), task_stage_rank(reached)) {
        (Some(reported_rank), Some(reached_rank)) if reported_rank < reached_rank => {
            Some(backwards_task_status_message(reported, reached))
        }
        _ => None,
    }
}

/// `AccessDeniedException$message` for a TASK state report Amazon ECS cannot attribute to a
/// verified container agent.
///
/// Entirely fixed text: the condition is that the task runs on no container instance at all,
/// so there is no identity, no value of the request and nothing of the record to state.
pub const UNVERIFIED_TASK_REPORT_MESSAGE: &str =
    "Fargate task state changes from the agent must have the instance verified";

/// The refusal a TASK state report about `task` earns when the agent that would be reporting
/// it has no identity Amazon ECS can verify, or `None` when it has one.
///
/// The rule: a task's `lastStatus` is written by the container agent that RUNS the task
/// ("This action is only used by the Amazon ECS agent"), and that agent's identity is the
/// container instance it is registered as — so Amazon ECS takes the report only where such an
/// instance exists to be verified. A task placed on the infrastructure Amazon Web Services
/// manages runs on no container instance of this account at all
/// ([`TaskRecord::runs_on_fargate`]), so no caller can be verified as its agent: the report is
/// refused with `AccessDeniedException` and the task is left exactly where it was, which is
/// what keeps `lastStatus` the server's own sequence for such a task.
///
/// Scoped to the TASK report: a container or attachment entry says what the agent observed
/// INSIDE the task, and Amazon ECS takes those about a task on any capacity.
///
/// What is weighed here is the TASK, which is what carries the hosting identity: whether the
/// caller holds that agent's own credentials is not a question this service answers, since the
/// emulator verifies a request's signature and resolves no agent principal behind it.
pub fn unverified_task_report_violation(task: &TaskRecord) -> Option<&'static str> {
    task.runs_on_fargate()
        .then_some(UNVERIFIED_TASK_REPORT_MESSAGE)
}

/// The refusal the attachment states a report NAMES earn, or `None` when every entry names
/// one of [`AGENT_REPORT_ATTACHMENT_STATUSES`].
pub fn agent_report_attachment_status_violation<'a>(
    statuses: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    statuses
        .into_iter()
        .any(|status| !AGENT_REPORT_ATTACHMENT_STATUSES.contains(&status))
        .then(|| {
            value_set_message(
                AGENT_REPORT_ATTACHMENT_STATUS_MEMBER,
                &AGENT_REPORT_ATTACHMENT_STATUSES,
            )
        })
}
