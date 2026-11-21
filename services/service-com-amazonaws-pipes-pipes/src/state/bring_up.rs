// SPDX-License-Identifier: CC0-1.0

//! Why a pipe cannot be BROUGHT UP — the question a lifecycle write asks before it reports
//! a pipe on its way to a healthy state.
//!
//! A pipe is accepted synchronously and reaches the state its `DesiredState` asked for a
//! moment later ([`super::STATE_SETTLE_DELAY`]), so what a pipe could not be brought up for
//! is not an answer to the call: it is where the pipe SETTLES. A move that cannot be
//! completed lands in the terminal failure state the model declares for it —
//! `CREATE_FAILED` for a creation, `START_FAILED` for a start, `UPDATE_FAILED` for a
//! reconfiguration — carrying the `StateReason` that says why, and the pipe runs nothing.
//!
//! Two things a pipe cannot poll its source without are decided here, in the order the
//! pipe would need them:
//!
//! 1. its EXECUTION ROLE, which every stage acts as ([`super::execution_role`]): a role
//!    whose trust document withholds the assume is one no stage could act as, and a role
//!    whose policies withhold EVERY call the source stage makes on the source is one under
//!    which that stage could neither read the source nor even look at it;
//! 2. the SOURCE itself: a `Source` naming a resource the service that owns it does not
//!    hold is a source with nothing to poll.
//!
//! What a role withholds only in PART is no part of that: a role that may make some of the
//! source stage's calls and not others leaves the pipe to come up, and each withheld call
//! then fails the executions that make it — a pipe that may look at its source and not take
//! its records comes up `RUNNING` and moves nothing, its records left where they are. Only
//! a role under which NONE of those calls can be made is a source the pipe could not reach
//! at all, which is the same as a source that is not there.
//!
//! Both of those are read off the ACCOUNT's records and the owner of the source, which is
//! why they are asked with no store locked ([`refusal`]). The third thing decided here is
//! read off the CONFIGURATION alone ([`configuration_refusal`]): the dead-letter destination
//! a stream source's parameters name, which the request boundary cannot refuse because the
//! call itself is accepted, and which a pipe whose batches have retries to spend could not
//! give up on a batch without.
//!
//! The TARGET side is deliberately NOT among them. A delivery the target refuses — a role
//! that may not write to it, a target that is not there — is one EXECUTION of a RUNNING
//! pipe failing, counted as such ([`super::metrics`]) with the records left on the source
//! for the next attempt; that is what lets a pipe deliver the record it withheld as soon as
//! the grant it was missing is in force. Only what the source stage could not reach its
//! source at all without decides whether the pipe comes up.
//!
//! Nothing is refused on evidence this emulator does not have: a source of a kind no owner
//! here is asked, a source of another account, an owner that did not answer, and (through
//! [`super::execution_role`]) a role the account does not hold, an unreadable trust
//! document or a policy attachment whose document this account does not carry — each shows
//! nothing, and a pipe carrying it comes up.

/// Why the pipe reading `source` under the source parameters `source_parameters` cannot be
/// brought up, decided from the CONFIGURATION ALONE, or `None` when nothing of the kind can be
/// shown.
///
/// The half of the bring-up decision that asks no other service and reads no store, so a
/// lifecycle write can take it with the pipes store locked and against the configuration the
/// write would LEAVE the pipe in: what it answers is not what the account holds but what the
/// configuration says. Its one question is the dead-letter destination
/// ([`super::data_plane::dead_letter_refusal`]), which the request boundary cannot refuse,
/// because the member is an ARN of no fixed service and AWS accepts the call. The answer is
/// the sentence that states the rule the destination broke, reported as the pipe's
/// `StateReason` as it stands.
///
/// Every lifecycle write that reports a pipe up asks BOTH this and [`refusal`], so none of
/// them can bring up a pipe another would not.
pub fn configuration_refusal(
    source: &str,
    source_parameters: Option<&crate::ops::types::PipeSourceParameters>,
) -> Option<String> {
    let reason = super::data_plane::dead_letter_refusal(source, source_parameters)?;
    Some(super::execution_role::within_the_reason_bound(reason))
}

/// Why the pipe that would run as `role_arn` over the source `source` cannot be brought up,
/// or `None` when nothing of the kind can be shown.
///
/// The half of the decision that asks the ACCOUNT's own records and the owner of the source,
/// so it is taken with no store locked; [`configuration_refusal`] is the other half.
///
/// The one decision behind every lifecycle write that reports a pipe up (`CreatePipe`,
/// `StartPipe`, `UpdatePipe`), so none of them can bring up a pipe another would not.
///
/// The answer is a sentence for the pipe's own `StateReason`, bounded by that member's
/// `@length` ([`super::execution_role::within_the_reason_bound`]). No store is locked while
/// the owner of the source is asked, so no request waits on this decision behind a lock.
pub async fn refusal<Eng: Send + Sync + 'static>(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    engine: &Eng,
    source: &str,
    role_arn: &str,
) -> Option<String> {
    // The trust document first: a role this service cannot assume at all never reaches the
    // question of what it may do.
    if let Some(reason) = super::execution_role::engine_assume_refusal(engine, role_arn) {
        return Some(reason);
    }
    if let Some(reason) = source_read_refusal(engine, source, role_arn) {
        return Some(reason);
    }
    absent_source_refusal(ctx, source).await
}

/// Why the role `role_arn` could not reach `source` AT ALL, or `None` when the policies in
/// force on it leave it at least one of the calls its source stage makes on that source.
///
/// The calls asked about are the one that TAKES the records for that kind of source and the
/// other calls the same stage makes on it ([`super::data_plane::source_read_call`]) — a
/// source whose records this emulator polls through no call of its own is a question that is
/// not asked. The answer is a refusal only when EVERY one of them is withheld: a role that
/// may make none of them names a source the pipe can neither read nor even look at, which is
/// the same as a source that is not there. A role that withholds only SOME of them leaves
/// the pipe to come up, and each withheld call then fails the executions that make it, with
/// the records left on the source — the way a withheld TARGET call already behaves (see the
/// module docs).
///
/// What the pipe REPORTS for it is the record-taking call and nothing else
/// ([`super::execution_role::source_read_failure_reason`]): the decision's own sentence says
/// which document withheld the grant, which is for the poller's log rather than for the state
/// a pipe rests in.
fn source_read_refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    source: &str,
    role_arn: &str,
) -> Option<String> {
    let read = super::data_plane::source_read_call(source)?;
    let iam = super::execution_role::iam_state(engine)?;
    // Held for the length of this one decision, and no call leaves the process while it is.
    let held = iam.lock();
    super::execution_role::call_refusal(role_arn, read.action, source, &held)?;
    // One call it may still make is one source-stage call that is not withheld, so the pipe
    // comes up: what it may not do is the running pipe's to fail.
    if read.others.iter().any(|action| {
        super::execution_role::call_refusal(role_arn, action, source, &held).is_none()
    }) {
        return None;
    }
    Some(super::execution_role::source_read_failure_reason(
        read.api, read.service,
    ))
}

/// Why the source `source` names cannot be polled because it is not there, or `None` when
/// the service that owns it does not say so.
async fn absent_source_refusal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    source: &str,
) -> Option<String> {
    super::data_plane::source_is_absent(ctx.callback(), source)
        .await
        .then(|| {
            super::execution_role::within_the_reason_bound(format!(
                "Pipes cannot poll the source {source}: it does not exist."
            ))
        })
}
