Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 2d62b7c5101c1773b5baea44a16f2931bd175612

# The requested-version conflict offers CancelSession as the remedy at a stage where CancelSession answers 409, so the version is abandonable by no path

_Filed: 2026-11-24T12:52:06Z_
_Operation: DeleteInactiveApprovalTeamVersion | Class: error-response_

> _Note: this issue was identified against the code base at commit `2d62b7c5101c1773b5baea44a16f2931bd175612`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`DeleteInactiveApprovalTeamVersion` refuses a request naming the version a team was asked to approve whenever that version's status is not `INACTIVE`, and the refusal it states tells the caller the version "is abandoned by cancelling its update session with CancelSession".

That remedy is reachable only at the FIRST of the two stages the refusal covers. A requested change carries `TeamStatus::Pending` for both `UPDATE_PENDING_APPROVAL` and `UPDATE_PENDING_ACTIVATION` — `crate::state::ApprovalTeam::record_update_approval` moves only `status_code`, never `status` — so the same refusal, with the same remedy clause, is raised once the approvers have already answered. At that point the update session is `APPROVED` (`crate::state::Session::approve` set it), and this crate's own `CancelSession` handler answers an `APPROVED` session with `ConflictException` (409).

So at `UPDATE_PENDING_ACTIVATION` the crate refuses the delete, points at `CancelSession`, and `CancelSession` refuses too: no operation of this crate can abandon that version, while the refusal asserts one can. The sibling `RefusedVersion::Requested` doc states the narrower rule the message is written for ("while the team's answer is outstanding it is `PENDING`") — but by this stage the answer has been given, so neither the doc's premise nor the remedy it words holds.

The crate's own test table reaches this stage (`Arrangement` "carrying a change the approvers approved and the clock has not activated yet") and pins only the two STATUS claims the sentence makes; it follows the remedy through from the awaiting-approval arrangement alone, so the unreachable remedy is pinned by nothing.

## Evidence

The refusal and its remedy — services/service-com-amazonaws-mpa-awsfluffycoreservice/src/ops/delete_inactive_approval_team_version.rs:285-295:
```
            if !RefusedVersion::Requested
                .deletable()
                .contains(&requested.status)
            {
                return Err(conflict(
                    RefusedVersion::Requested,
                    requested.status.as_wire(),
                    "a version awaiting the team's approval is abandoned by cancelling its \
                     update session with CancelSession",
                ));
            }
```
The only status it accepts — same file:167-175:
```
            Self::Requested => &[crate::state::TeamStatus::Inactive],
```
and the rule the remedy is worded for — same file:158-161:
```
    /// The version the team was ASKED TO APPROVE. Deleted once it is a version the team
    /// will never move to (`INACTIVE`); while the team's answer is outstanding it is
    /// `PENDING`, and abandoning it is the update session's business, not this one's.
    Requested,
```

That `PENDING` also covers the post-approval stage — services/service-com-amazonaws-mpa-awsfluffycoreservice/src/state.rs:1636-1643 (`record_update_approval` leaves `status` alone):
```
    pub fn record_update_approval(&mut self, at: SystemTime, answered: &[String]) {
        let Some(update) = self.pending_update.as_mut() else {
            return;
        };
        update.approval_due = None;
        update.status_code = Some(TeamStatusCode::UpdatePendingActivation);
        update.activation_due = at.checked_add(UPDATE_ACTIVATION_DELAY);
        self.status_code = Some(TeamStatusCode::UpdatePendingActivation);
```
and state.rs:1452-1457 says so outright:
```
    /// Whether this change is still on its way to becoming the team's live
    /// configuration — waiting on the approvers, or on the activation they approved.
    #[must_use]
    pub fn is_outstanding(&self) -> bool {
        self.status == TeamStatus::Pending
    }
```

The session that the remedy names is `APPROVED` by then — state.rs:2315-2321:
```
            for approver in self.approvers.iter_mut().take(required) {
                approver.response = SessionResponse::Approved;
                approver.response_time = Some(at);
            }
            self.status = SessionStatus::Approved;
```
and the remedy is refused — services/service-com-amazonaws-mpa-awsfluffycoreservice/src/ops/cancel_session.rs:213-225:
```
            other @ (crate::state::SessionStatus::Approved
            | crate::state::SessionStatus::Failed) => {
                return Err(CancelSessionErrorModeled::ConflictException(ConflictException {
                    message: crate::state::bounded_message(format!(
                        "Approval session '{}' is {} and no longer outstanding, so it cannot \
                         be cancelled.",
```

Test coverage: services/service-com-amazonaws-mpa-awsfluffycoreservice/tests/all/version_delete_refusal_statuses.rs:17-26 states the tables — the first spans "carrying a change the approvers approved and the clock has not activated yet", the second "takes the refusals at their word" for "the requested version (`INACTIVE`, reached by the very remedy the refusal offers — cancelling the update session)" — and its follow-through row (same file:343-361, `Accepted::RequestedAfterCancellation`) arranges `Arrangement::AwaitingApproval` only, so nothing pins the remedy at the post-approval stage. `status_claims` (same file:48-65) parses only the two status claims out of the sentence, not the remedy clause.

## Steps to Reproduce

1. CreateApprovalTeam, then let the clock materialise the approvers' invitation responses so the team is ACTIVE.
2. UpdateApprovalTeam with a configuration member (e.g. Description) — the change is recorded as a pending version and an update session is started; note the VersionId returned and the UpdateSessionArn GetApprovalTeam reports.
3. Let `crate::state::UPDATE_APPROVAL_DELAY` elapse but not `UPDATE_ACTIVATION_DELAY`, then GetApprovalTeam: StatusCode is UPDATE_PENDING_ACTIVATION and PendingUpdate.Status is PENDING.
4. DELETE /approval-teams/{Arn}/{VersionId} for the PENDING version id: 409 ConflictException whose message ends "...a version awaiting the team's approval is abandoned by cancelling its update session with CancelSession".
5. PUT /sessions/{UpdateSessionArn} (CancelSession) on the session the refusal named: 409 ConflictException "...is APPROVED and no longer outstanding, so it cannot be cancelled." — the remedy the previous refusal offered is itself refused, and no other operation abandons that version (UpdateApprovalTeam naming a configuration member is also 409 while the version is outstanding).
