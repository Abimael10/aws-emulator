Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 2db67cdf583f83462fa29a2772c7057ac4b27efa

# The identity-source IN-USE rule ignores stored session approver records, so GetSession reports an approver authenticated by a source the store has removed

_Filed: 2026-11-23T09:19:15Z_
_Operation: DeleteIdentitySource | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `2db67cdf583f83462fa29a2772c7057ac4b27efa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`DeleteIdentitySource`'s in-use gate — and the clock pass that actually drops the record — decide "is this identity source in use?" by walking the stored TEAMS only (`ComAmazonawsMpaAwsfluffycoreserviceState::team_arn_using_identity_source`, which iterates `self.approval_teams` and each team's `approver_records()`). The store's SESSIONS also hold identity-source references: every `SessionApprover` carries its own `identity_source_arn`, copied off the team's approvers when the session was started, and nothing ever rewrites a stored session's approver list.

So as soon as a team's approver set stops naming a source (an `UpdateApprovalTeam` that replaces the approvers, whose activation installs the new set), the source counts as unused even though sessions this store still holds — the update session that change was put in, or an earlier baseline session — still name it. The delete is accepted, the clock's re-check at the removal instant also finds nothing, the record is dropped, and `GetSession` then answers 200 with `ApproverResponses[].IdentitySourceArn` naming a source that resolves to nothing: exactly the state the crate's own comments say cannot arise.

The crate already extended the rule once for this reason — `approver_records()` reads BOTH approver sets a team holds (live + `PendingUpdate`), with a dedicated suite for it — but the session records were not included, so the stated invariant does not hold.

## Evidence

The invariant the crate states, in the clock pass that performs the removal:

- `services/service-com-amazonaws-mpa-awsfluffycoreservice/src/state.rs:3438-3440`: "rather than trusting the request that scheduled the removal: a reference / recorded after that request was accepted keeps the source, so no read ever / reports an approver authenticated by a source this store no longer holds."
- `services/service-com-amazonaws-mpa-awsfluffycoreservice/src/state.rs:4055-4058` (`malformed_approver_identity_source`): "the store would otherwise keep — and later report, from `GetApprovalTeam` and `GetSession` — an / identity source reference that resolves to nothing and that / [`...team_arn_using_identity_source`] can never / match."

The resolver that decides it, which reads TEAMS only:

- `services/service-com-amazonaws-mpa-awsfluffycoreservice/src/state.rs:3597-3605`:
```
pub fn team_arn_using_identity_source(&self, arn: &str) -> Option<&str> {
    self.approval_teams
        .values()
        .find(|team| {
            team.approver_records()
                .any(|approver| approver.primary_identity_source_arn == arn)
        })
        .map(|team| team.arn.as_str())
}
```
`self.sessions` is never consulted — neither by the request gate (`src/ops/delete_identity_source.rs:274`: `if let Some(team_arn) = state.team_arn_using_identity_source(&input.identity_source_arn) {`) nor by the removal (`src/state.rs:3456`: `&& self.team_arn_using_identity_source(&source.arn).is_none()`).

The reference the store keeps, and the read that reports it:

- `services/service-com-amazonaws-mpa-awsfluffycoreservice/src/state.rs:2148` (`SessionApprover`): `pub identity_source_arn: String,`
- `services/service-com-amazonaws-mpa-awsfluffycoreservice/src/ops/update_approval_team.rs:413-419`: the update session's approvers are built from `team_record.approvers` with `identity_source_arn: approver.primary_identity_source_arn.clone()`, and no later path rewrites them.
- `services/service-com-amazonaws-mpa-awsfluffycoreservice/src/ops/get_session.rs:285`: `identity_source_arn: Some(approver.identity_source_arn.clone()),`

Test coverage: `tests/all/identity_source_removal_in_use.rs` states the rule it pins as "the IN-USE check `DeleteIdentitySource` makes, over BOTH approver sets a stored approval team holds" (module doc, lines 3-13) and `tests/all/identity_source_in_use_by_pending_update.rs` covers the pending-update set; grepping `session|Session` in `tests/all/identity_source_removal_in_use.rs` returns no matches, so no test pins (or justifies) sessions being excluded.

## Steps to Reproduce

1. CreateIdentitySource twice and wait out IDENTITY_SOURCE_ACTIVATION_DELAY, giving two ACTIVE sources S1 and S2.
2. CreateApprovalTeam T with `Approvers: [{PrimaryIdentityId: identity-1, PrimaryIdentitySourceArn: S1}]`; wait out INVITATION_RESPONSE_DELAY so the team is ACTIVE.
3. UpdateApprovalTeam T with `Approvers: [{PrimaryIdentityId: identity-2, PrimaryIdentitySourceArn: S2}]`. Note the returned VersionId and, from GetApprovalTeam, the `UpdateSessionArn` U — U's stored approver carries identity_source_arn = S1.
4. Wait out UPDATE_APPROVAL_DELAY + UPDATE_ACTIVATION_DELAY and GetApprovalTeam T: the live approvers are now [identity-2 / S2] and no PendingUpdate remains, so no team references S1.
5. DeleteIdentitySource(S1): answered 200 (the in-use gate finds no team naming S1) and the source goes DELETING.
6. Wait out IDENTITY_SOURCE_DELETION_DELAY and GetIdentitySource(S1): 404 — the record is gone.
7. GetSession(U): 200 with `ApproverResponses[0].IdentitySourceArn == S1`, i.e. a read reporting an approver authenticated by a source this store no longer holds — the state the comment at src/state.rs:3439-3440 says cannot happen.
