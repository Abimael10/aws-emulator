Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 6c051e05fffd5374b9cc043b9f5d424dad86d0dc

# The approvers' VOTED activity is recorded on the live approver set only, so GetApprovalTeam reports one approver with two different LastActivity values

_Filed: 2026-11-24T17:19:17Z_
_Operation: UpdateApprovalTeam | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `6c051e05fffd5374b9cc043b9f5d424dad86d0dc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

A team keeps TWO approver records for the same approver while a requested change waits: the live set (`ApprovalTeam::approvers`) and the set of the version its approvers were asked to approve (`PendingUpdate::approvers`). `UpdateApprovalTeam` builds the second one from the first, retaining the stored record for every identity the team already holds, so both sets carry the SAME `ApproverId` for the same identity.

When the clock materialises the approvers' answer, `ApprovalTeam::record_update_approval` (and its sibling `record_deletion_approval`) stamps `LastActivity = VOTED` / `LastActivityTime` on `self.approvers` only — it never touches the records the requested version holds. The other clock-owed per-approver change in the same pass, the invitation response, IS applied through `ApprovalTeam::approver_records_mut()`, the helper whose own doc says it is "what a change the clock owes an approver record is applied through".

The observable result: between the approval and the activation (the `UPDATE_PENDING_ACTIVATION` stage), `GetApprovalTeam` renders both sets through the one `reported_approver` renderer and reports the same `ApproverId` as `LastActivity: VOTED` with the approval instant under `Approvers`, and as `LastActivity: RESPONDED_TO_INVITATION` with the invitation instant under `PendingUpdate.Approvers` — two different last activities for one approver in one response. That contradicts the renderer's stated purpose ("so the two cannot disagree about what one approver looks like") and the sibling rule the crate applies to the other per-identity property, MFA sync state, which is deliberately written to BOTH sets for exactly this reason.

## Evidence

The helper the crate says a clock-owed approver change goes through — services/service-com-amazonaws-mpa-awsfluffycoreservice/src/state.rs:1605-1613:
```
    /// The same two sets, for update — what a change the clock owes an approver record
    /// is applied through ([`ComAmazonawsMpaAwsfluffycoreserviceState::materialize_due`]).
    pub fn approver_records_mut(&mut self) -> impl Iterator<Item = &mut Approver> {
        let requested = self
            .pending_update
            .as_mut()
            .map(|update| update.approvers.as_mut_slice())
            .unwrap_or_default();
        self.approvers.iter_mut().chain(requested)
    }
```
and the clock-owed invitation response, which uses it — state.rs:3142-3157:
```
            for approver in team.approver_records_mut() {
...
                approver.last_activity = Some(ApproverLastActivity::RespondedToInvitation);
                approver.last_activity_time = Some(due);
```

The VOTED write, which does NOT — state.rs:1636-1649 (`record_update_approval`, called from `materialize_due` at state.rs:3289 `team.record_update_approval(due, &answered);`):
```
    pub fn record_update_approval(&mut self, at: SystemTime, answered: &[String]) {
...
        for approver in &mut self.approvers {
            if answered.iter().any(|id| id == &approver.approver_id) {
                approver.last_activity = Some(ApproverLastActivity::Voted);
                approver.last_activity_time = Some(at);
            }
        }
```
the same one-set loop in `record_deletion_approval` — state.rs:1761-1766.

The sibling rule for the other per-identity property — state.rs:1716-1727:
```
    /// Synchronize the MFA devices of EVERY approver record on the team — the effect the
    /// `SYNCHRONIZE_MFA_DEVICES` update action asks for.
    ///
    /// A device belongs to the approver's IDENTITY, not to one version of the team, so the
    /// records of both sets the team holds ([`ApprovalTeam::approver_records_mut`]) are
    /// synchronized: an approver the team carries live and in the version it has been
    /// asked to approve cannot be reported in sync under one and out of sync under the
    /// other.
```

Both sets hold the SAME approver ids — services/service-com-amazonaws-mpa-awsfluffycoreservice/src/ops/update_approval_team.rs:448-454:
```
            let mut requested_approvers = requested_approver_set(
                &mut state,
                input.approvers.as_ref(),
                &existing_approvers,
                now,
            )?
            .unwrap_or(existing_approvers);
```
and state.rs:3829-3834 (a retained identity keeps its stored record, hence its id):
```
                let retained = existing.iter().find(|stored| {
                    stored.primary_identity_id == identity_id
                        && stored.primary_identity_source_arn == source_arn
                });
                match retained {
                    Some(stored) => Some(stored.clone()),
```

The read that reports both, through one renderer — src/ops/get_approval_team.rs:300-304:
```
/// One approver as this read reports it, off the stored record.
///
/// The same rendering serves the team's own approvers and the approvers of the change it
/// has been asked to approve (`PendingUpdate$Approvers`), so the two cannot disagree about
/// what one approver looks like.
```
used for the live set at get_approval_team.rs:244 and for the requested set at get_approval_team.rs:288.

Test coverage: grepping `Voted|VOTED` across `tests/` matches only the enum-token row in tests/all/approval_team_lifecycle.rs:536, and `tests/all/pending_team_update.rs` (the suite for the approval/activation stages) asserts only `NumberOfApprovers` and the configuration members of `PendingUpdate` — nothing pins (or justifies) the one-set VOTED write.

## Steps to Reproduce

1. CreateApprovalTeam with two approvers and ApprovalStrategy MofN MinApprovalsRequired 1; wait out INVITATION_RESPONSE_DELAY so the team is ACTIVE (every approver reports LastActivity RESPONDED_TO_INVITATION).
2. UpdateApprovalTeam with only a Description change: the change is recorded as a pending version whose approver set holds clones of the live records (same ApproverIds), and an update session is started.
3. Push the pending update's activation_due into the future (as tests/all/update_while_a_version_is_outstanding.rs::postpone_activation does) so the version stays at the post-approval stage, then let UPDATE_APPROVAL_DELAY elapse.
4. GetApprovalTeam: StatusCode is UPDATE_PENDING_ACTIVATION; Approvers[0].LastActivity is VOTED with the approval instant, while PendingUpdate.Approvers[0].LastActivity (same ApproverId, same PrimaryIdentityId) is still RESPONDED_TO_INVITATION with the invitation instant — one approver reported with two different last activities in one response.
5. The same divergence follows StartActiveApprovalTeamDeletion's approval on a team that still reports a (failed) PendingUpdate, through record_deletion_approval.
