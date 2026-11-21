Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 1f5d5a2e3955ce51e23f36cd9ec02202b9df7d93

# The shared export admission refuses an accountIds naming a member account of the very organization it honours includeMemberAccounts for

_Filed: 2026-11-14T18:35:01Z_
_Operation: ExportAutoScalingGroupRecommendations | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `1f5d5a2e3955ce51e23f36cd9ec02202b9df7d93`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state::export::create_export_job` — the one admission rule every `Export*Recommendations` operation of this service applies — contradicts itself about whether this emulator has an organization.

One branch refuses any `accountIds` entry other than the caller's own on the stated premise that the emulator "records no organization", answering the cross-account refusal. A later branch in the SAME function conditions `includeMemberAccounts` on `manages_an_organization()`, which is true exactly when the store holds member accounts — i.e. it accepts an organization-wide export precisely in the state the first branch assumes cannot exist.

So once the store holds organization member accounts (the state `GetEnrollmentStatusesForOrganization` reports rows for, that `UpdateEnrollmentStatus{includeMemberAccounts:true}` requires and writes enrollment into, and that `GetEnrollmentStatus` counts with `numberOfMemberAccountsOptedIn`), one and the same management-account caller may export "recommendations for resources in all member accounts of the organization" via `includeMemberAccounts`, but is refused 400 with "Invalid permission to access another account. If this is an organization account, request the management account to enable service access." when it names one of those same recorded member accounts in `accountIds`. The same `is_foreign_account`/`requested_account_id` premise is shared by every `Get*Recommendations` reader, so all of them refuse a member account the service otherwise reports an enrollment row for.

## Evidence

Both sides inside one function. src/state/export.rs:436-450:
```
    // `accountIds` is the MANAGEMENT-account mechanism for naming a member account of an
    // organization, and this emulator presents exactly one standalone account
    // ([`super::is_foreign_account`]) and records no organization. So an id naming any OTHER
    // account asks for an export of an account whose analysis this service does not hold, and
    // is refused with the refusal the whole crate shares ([`super::CROSS_ACCOUNT_MESSAGE`])
    if account_ids
        .iter()
        .any(|value| super::is_foreign_account(value))
    {
        return Err(ExportRefusal::InvalidParameter(
            super::CROSS_ACCOUNT_MESSAGE.to_string(),
        ));
    }
```
src/state/export.rs:493-496 (same function, ~45 lines later):
```
    if let Some(message) = guard.include_member_accounts_rejection(request.include_member_accounts)
    {
        return Err(ExportRefusal::InvalidParameter(message.to_string()));
    }
```
which resolves to src/state.rs:4020-4026:
```
    pub fn include_member_accounts_rejection(
        &self,
        include_member_accounts: bool,
    ) -> Option<&'static str> {
        (include_member_accounts && !self.manages_an_organization())
            .then_some(INCLUDE_MEMBER_ACCOUNTS_MESSAGE)
    }
```
and src/state.rs:4005-4007:
```
    pub fn manages_an_organization(&self) -> bool {
        !self.organization_members.is_empty()
    }
```

The premise the refusal rests on, src/state.rs:951-961 (`is_foreign_account`): "This emulator presents exactly one standalone account ([`engine::emulator_account_id`]) and records no organization, so a resource owned by any other account is one no request can be answered about" — contradicted by the module's own header, src/state.rs:14-18: "The ORGANIZATION's member accounts live here too ([`OrganizationMemberStore`]) … while an `UpdateEnrollmentStatus` call that includes them writes their enrollment — which `GetEnrollmentStatusesForOrganization` reports and `GetEnrollmentStatus` counts over."

Test coverage: tests/all/include_member_accounts.rs exercises `includeMemberAccounts` against a recorded organization (record_organization) and tests/all/include_member_accounts.rs:381-408 pins only the mutual-exclusion refusal for `accountIds:[ACCOUNT]`; no test covers `accountIds` naming a recorded MEMBER account, so this behaviour is unpinned.

## Steps to Reproduce

1. Boot an emulator whose store holds organization member accounts (the state tests reach with record_organization, i.e. OrganizationMemberStore non-empty — no API of this service creates one).
2. Call UpdateEnrollmentStatus {"status":"Active","includeMemberAccounts":true} — accepted, and GetEnrollmentStatusesForOrganization now reports a row per member account (so the service does present an organization).
3. Create a destination bucket, then call ExportAutoScalingGroupRecommendations {"s3DestinationConfig":{"bucket":"b"},"includeMemberAccounts":true} — accepted: the export covers the organization's member accounts.
4. Call ExportAutoScalingGroupRecommendations {"s3DestinationConfig":{"bucket":"b"},"accountIds":["<one member account id reported in step 2>"]} — refused 400 InvalidParameterValueException with the cross-account message, on the premise that the service records no organization.
5. Same divergence through the readers: GetAutoScalingGroupRecommendations {"accountIds":["<that member account>"]} is refused with the same message.
