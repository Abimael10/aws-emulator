Issue-Severity: CRITICAL
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: mpa.create-approval-team-missing-permission-rejected mpa.create-approval-team-duplicate-name-conflict-rejected mpa.concurrent-create-same-name-exactly-one-wins mpa.create-approval-team-invalid-name-pattern-rejected mpa.min-approvals-exceeds-approver-count-rejected mpa.create-identity-source-invalid-instance-arn-pattern-rejected
Issue-Shape-Id: com.amazonaws.mpa#AWSFluffyCoreService
Issue-Key: mpa.create-approval-team-missing-permission-rejected|run#10|AssertionError|raise AssertionError(
Issue-Key: mpa.create-approval-team-duplicate-name-conflict-rejected|run#12|AssertionError|raise AssertionError(
Issue-Key: mpa.concurrent-create-same-name-exactly-one-wins|run#16|AssertionError|assert len(successes) == 0, (
Issue-Key: mpa.create-approval-team-invalid-name-pattern-rejected|run#12|AssertionError|assert invalid_code == ("AccessDeniedException", MANAGEMENT_ACCOUNT_ONLY), invalid_code
Issue-Key: mpa.min-approvals-exceeds-approver-count-rejected|run#15|AssertionError|assert over_code == "AccessDeniedException", (over_code, over_message)
Issue-Key: mpa.create-identity-source-invalid-instance-arn-pattern-rejected|run#7|AssertionError|assert code1 == ("AccessDeniedException", MANAGEMENT_ACCOUNT_ONLY), code1
Issue-Commit: 2a513259435d6dcec5aa5a741411e3a8a3522aaf

# mpa create plane does not enforce the organization management-account gate: CreateApprovalTeam creates teams and CreateIdentitySource/CreateApprovalTeam report ValidationException where AWS answers 403 AccessDeniedException (2 causes)

> _Note: this issue was identified against the code base at commit `2a513259435d6dcec5aa5a741411e3a8a3522aaf`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateApprovalTeam

## Description
2 net-new causes in the mpa create plane, one shared fault: the organization management-account authorization decision AWS makes BEFORE any request-body shape/semantic validation and before any state mutation is never made by the emulator. A caller that is not the organization's management account is either served a successful create (200) or an emulator-authored 400 ValidationException about the request body, instead of AWS's 403 AccessDeniedException.

NOTE ON RELATED FILED WORK: the open wire issue 20261115T144214Z-mpa-create-operations-skip-the-organization-management-account.md describes the same shared fault from the wire corpus and carries wire replay keys (mpa|CreateApprovalTeam|4xx->4xx|..., mpa|CreateIdentitySource|4xx->2xx|...). It does not carry any of the behavioral check ids below, so these checks are not covered by it; this issue records them so a fixer can re-replay the behavioral surface. The open issue 20261104T085053Z-mpa-identity-source-reads-and-deletes-refuse-a.md covers only identity-source READ/DELETE ARN-acceptance ordering and does not cover either create path.

Cause 1 of 4 (operation CreateApprovalTeam, the gate is not applied at all — the create succeeds):
AWS: Smithy: CreateApprovalTeam POST /approval-teams, output 200 CreateApprovalTeamResponse; modeled errors AccessDeniedException (httpError 403, doc "You do not have sufficient access to perform this action. Check your permissions, and try again.", Message @required String @length max 1000), ConflictException (409), ServiceQuotaExceededException (402), ThrottlingException (429), InternalServerException (500), ValidationException (400). Recorded behavior: a caller outside the organization's management account gets HTTP 403, error code AccessDeniedException, and the Message "Action can only be performed by an organization\u2019s management account." That whole sentence is the FIXED text of this error class (note the typographic apostrophe U+2019 in "organization\u2019s"); it has NO variable parts — nothing from the request or from state is interpolated. No approval team is created, so a subsequent ListApprovalTeams shows no team under the attempted Name. The gate does not depend on IAM grants: a fully privileged non-management-account principal is refused identically.
Emulator: HTTP 200 with a full CreateApprovalTeamResponse (CreationTime, Arn, Name, VersionId) and the team then visible to ListApprovalTeams. Repeating the identical call also succeeds, and 8 concurrent calls with the identical Name produce at least one success, so neither the gate nor any Name-uniqueness rule refuses the caller.
Why: a client asserting that a non-management-account principal cannot create an approval team sees the privileged create succeed, so every negative-authorization branch keyed on AccessDeniedException is dead code and every downstream step then runs against an approval team AWS would never have created. Negative-authorization tests pass vacuously, and callers can mutate the MPA control plane from accounts AWS refuses.
Fix: the CreateApprovalTeam handler in the mpa service crate's per-operation handler directory — make the organization management-account authorization decision first, before ClientToken/idempotency handling, body validation and any state mutation, and raise AccessDeniedException (403) with the fixed message.
Traces: mpa.create-approval-team-missing-permission-rejected, mpa.create-approval-team-duplicate-name-conflict-rejected, mpa.concurrent-create-same-name-exactly-one-wins.

Cause 2 of 4 (operation CreateApprovalTeam, ordering — Name pattern validation is evaluated ahead of the gate):
AWS: Smithy: CreateApprovalTeamRequest.Name @required, target ApprovalTeamName, @length max 64, @pattern ^[a-zA-Z0-9._-]+$. Botocore does not enforce `pattern` client-side, so a Name containing a space reaches the service. Recorded behavior: the malformed-Name request is refused with exactly the same refusal a well-formed Name gets — 403 AccessDeniedException, Message "Action can only be performed by an organization\u2019s management account." — because the authorization decision precedes shape validation and the shape complaint is never reached.
Emulator: HTTP 400, error code ValidationException, Message "1 validation error detected: Value '<the request's Name>' at 'name' failed to satisfy constraint: Member must satisfy regular expression pattern: ^[a-zA-Z0-9._-]+$" — fixed text aside, the quoted value is the request's own Name. The emulator evaluates the Name pattern before deciding authorization.
Why: the caller is told its Name is malformed rather than that it lacks the management-account privilege, so the status (400 vs 403) and error-code assertions both fail and the caller "fixes" a Name that was never the problem. It also leaks which input values the service dislikes to a principal AWS refuses up front.
Fix: the same CreateApprovalTeam handler's validation ordering — the authorization gate must run before Name/ApprovalStrategy/Approvers/Policies validation.
Traces: mpa.create-approval-team-invalid-name-pattern-rejected.

Cause 3 of 4 (operation CreateApprovalTeam, ordering — the MinApprovalsRequired-vs-Approvers cross-field check is evaluated ahead of the gate):
AWS: Smithy: CreateApprovalTeamRequest.ApprovalStrategy @required (MofN.MinApprovalsRequired) and Approvers @required list @length 1..20. Recorded behavior: a request with MinApprovalsRequired greater than the number of Approvers gets the identical 403 AccessDeniedException / "Action can only be performed by an organization\u2019s management account." that a request with MinApprovalsRequired equal to the approver count gets — proving the cross-field count check is never evaluated for an unauthorized caller.
Emulator: HTTP 400, error code ValidationException, Message "ApprovalStrategy.MofN.MinApprovalsRequired (<requested min>) must not exceed the number of Approvers (<approver count>)." — the two parenthesised numbers are taken from the request; this wording is the emulator's own and is not AWS's text for any modeled error. Note that even if this check belonged here, it fires in place of the authorization refusal.
Why: the refusal the caller receives depends on input the service should not have examined, so a client cannot distinguish "not the management account" from "bad strategy" and retries with a corrected count, which (per Cause 1) then succeeds — a create AWS refuses.
Fix: same CreateApprovalTeam handler — move the cross-field ApprovalStrategy/Approvers consistency check behind the organization management-account gate.
Traces: mpa.min-approvals-exceeds-approver-count-rejected.

Cause 4 of 4 (operation CreateIdentitySource, ordering — InstanceArn validation is evaluated ahead of the gate):
AWS: Smithy: CreateIdentitySource POST /identity-sources, input IdentitySourceParameters @required (IamIdentityCenter.InstanceArn, Region) plus ClientToken/Tags; modeled errors AccessDeniedException (403), ServiceQuotaExceededException (402), ThrottlingException (429), InternalServerException (500), ValidationException (400). Recorded behavior: a non-management-account caller is refused with 403 AccessDeniedException and the same fixed Message "Action can only be performed by an organization\u2019s management account." for BOTH a syntactically malformed InstanceArn and a well-formed but nonexistent one, and no identity source is created in either case — the gate precedes InstanceArn shape validation. ListIdentitySources from the same caller succeeds, so the control plane is otherwise reachable.
Emulator: HTTP 400, error code ValidationException, Message "Invalid Resource Arn" (fixed emulator text, no request values) when the InstanceArn is malformed. The emulator validates the ARN before deciding authorization and never reaches the 403.
Why: a caller probing the authorization boundary of CreateIdentitySource is told its ARN is malformed, so the 403/AccessDeniedException branch never fires, and (combined with the wire-reported 200 on a well-formed ARN) the emulator performs or mislabels a privileged create across the whole identity-source create path.
Fix: the CreateIdentitySource handler in the same per-operation handler directory — gate on the organization management-account decision before IdentitySourceParameters/InstanceArn validation, ClientToken handling and any state mutation.
Traces: mpa.create-identity-source-invalid-instance-arn-pattern-rejected.

## Evidence
Cause 1 — mpa.create-approval-team-missing-permission-rejected (CreateApprovalTeam, POST /approval-teams, non-management-account caller):
  expected: HTTP 403, Error.Code "AccessDeniedException", Error.Message "Action can only be performed by an organization\u2019s management account." (entirely fixed text), and no team visible to ListApprovalTeams
  actual:   HTTP 200 with CreateApprovalTeamResponse {CreationTime, Arn arn:aws:mpa:us-east-1:<account>:approval-team/<requested name>, Name <requested name>, VersionId "1"}; check assertion: "expected AccessDeniedException from a non-management-account caller, but CreateApprovalTeam unexpectedly succeeded"

Cause 1 — mpa.create-approval-team-duplicate-name-conflict-rejected: the FIRST create already returns 200 ("expected AccessDeniedException on first CreateApprovalTeam call, but it unexpectedly succeeded"), so the duplicate-Name probe never reaches its point.

Cause 1 — mpa.concurrent-create-same-name-exactly-one-wins: 8 concurrent identical-Name creates, expected 8 AccessDeniedException refusals and zero teams; actual 1 success (HTTP 200, Arn .../approval-team/<name>, VersionId "1").

Cause 2 — mpa.create-approval-team-invalid-name-pattern-rejected (Name containing spaces):
  expected: ("AccessDeniedException", "Action can only be performed by an organization\u2019s management account.")
  actual:   ("ValidationException", "1 validation error detected: Value '<request Name>' at 'name' failed to satisfy constraint: Member must satisfy regular expression pattern: ^[a-zA-Z0-9._-]+$") at HTTP 400

Cause 3 — mpa.min-approvals-exceeds-approver-count-rejected (MinApprovalsRequired 3 over 2 Approvers):
  expected: ("AccessDeniedException", "Action can only be performed by an organization\u2019s management account."), identical to the MinApprovalsRequired==2 control call
  actual:   ("ValidationException", "ApprovalStrategy.MofN.MinApprovalsRequired (3) must not exceed the number of Approvers (2).") at HTTP 400 — the numbers are the request's values; the wording is the emulator's

Cause 4 — mpa.create-identity-source-invalid-instance-arn-pattern-rejected (malformed InstanceArn):
  expected: ("AccessDeniedException", "Action can only be performed by an organization\u2019s management account.") at HTTP 403, and the same for a well-formed nonexistent InstanceArn, with no identity source created
  actual:   ("ValidationException", "Invalid Resource Arn") at HTTP 400
  positive control in the same check: ListIdentitySources succeeds, so the mpa control plane is reachable from this caller.

Model evidence (services/service-com-amazonaws-mpa-awsfluffycoreservice/smithy-model/com-amazonaws-mpa-awsfluffycoreservice.json): AccessDeniedException is bound to both CreateApprovalTeam and CreateIdentitySource with errorKind client / httpError 403 and a @required Message (String, @length max 1000); ValidationException (400) is modeled for input-shape failures only. CreateApprovalTeamRequest.Name is @length max 64 @pattern ^[a-zA-Z0-9._-]+$.

## Steps to Reproduce
1. Boot the emulator and replay the behavioral suite (simparity-behavioral-aws-v1).
2. Cause 1: run mpa.create-approval-team-missing-permission-rejected — call mpa CreateApprovalTeam in us-east-1 as a caller that is not the organization's management account with a well-formed Name, an MofN strategy of 1, one approver and one managed policy-version ARN from ListPolicies/GetPolicyVersion. Observe HTTP 200 and a created team (also visible to ListApprovalTeams) where AWS answers 403 AccessDeniedException "Action can only be performed by an organization\u2019s management account." and creates nothing. Repeat the identical call (mpa.create-approval-team-duplicate-name-conflict-rejected) and issue 8 concurrent identical-Name calls (mpa.concurrent-create-same-name-exactly-one-wins): all must be refused 403, none may succeed.
3. Cause 2: run mpa.create-approval-team-invalid-name-pattern-rejected — same call with a Name containing spaces (violating ^[a-zA-Z0-9._-]+$). Observe 400 ValidationException about the 'name' pattern instead of the identical 403 AccessDeniedException a well-formed Name must get.
4. Cause 3: run mpa.min-approvals-exceeds-approver-count-rejected — same call with ApprovalStrategy.MofN.MinApprovalsRequired=3 and 2 Approvers. Observe 400 ValidationException "ApprovalStrategy.MofN.MinApprovalsRequired (3) must not exceed the number of Approvers (2)." instead of the 403 AccessDeniedException, which must be byte-identical to the MinApprovalsRequired=2 control call's refusal.
5. Cause 4: run mpa.create-identity-source-invalid-instance-arn-pattern-rejected — call mpa CreateIdentitySource with IdentitySourceParameters.IamIdentityCenter.InstanceArn set to a malformed ARN, then to a well-formed nonexistent arn:aws:sso:::instance/ssoins-<hex>. Observe 400 ValidationException "Invalid Resource Arn" on the malformed one instead of 403 AccessDeniedException with the fixed management-account message; neither attempt may create an identity source.
6. After the fix, re-replay all six checks: every CreateApprovalTeam and CreateIdentitySource call from a non-management-account caller must answer HTTP 403 with error code AccessDeniedException and Message "Action can only be performed by an organization\u2019s management account." regardless of how malformed the body is, and ListApprovalTeams / ListIdentitySources must show no resource created.
