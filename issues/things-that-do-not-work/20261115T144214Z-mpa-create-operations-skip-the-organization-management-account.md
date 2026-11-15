Issue-Source: trace-sweep-divergence
Issue-Trace: mpa-approval-team-create-validation-error
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: mpa-approval-team-create-validation-error mpa-identity-source-lifecycle-errors
Issue-Shape-Id: com.amazonaws.mpa#AWSFluffyCoreService
Issue-Key: mpa|CreateApprovalTeam|4xx->4xx|$.Message
Issue-Key: mpa|CreateApprovalTeam|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: mpa|CreateApprovalTeam|4xx->4xx|status
Issue-Key: mpa|CreateIdentitySource|4xx->2xx|$.Message
Issue-Key: mpa|CreateIdentitySource|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: mpa|CreateIdentitySource|4xx->2xx|status
Issue-Commit: f55279b3dc5678191fb0e8465c0432e63e772ce0

# mpa create operations skip the organization management-account authorization check (2 causes)

> _Note: this issue was identified against the code base at commit `f55279b3dc5678191fb0e8465c0432e63e772ce0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateApprovalTeam

## Description
2 causes, one shared fault: the MPA create paths (CreateApprovalTeam, CreateIdentitySource) never make the organization management-account authorization decision that AWS makes BEFORE any request-body validation or state mutation. AWS refuses a caller that is not the organization's management account with 403 AccessDeniedException and the fixed message "Action can only be performed by an organization’s management account." (the full message is fixed text for this error class — it has no variable parts — and the error-type header is namespaced to this service: x-amzn-errortype "AccessDeniedException:http://internal.amazon.com/coral/com.amazon.awsfluffycoreservice/"). The emulator instead runs its own input validation (Cause 1) or performs the create outright (Cause 2), so the management-account precondition is unenforced on the whole create surface. Note two PREVIOUSLY RESOLVED issues covered this behavior (20261023T100857Z-mpa-createapprovalteam-returns-400-validationexception-instead-of-the.md and 20261024T191541Z-mpa-createapprovalteam-event-propagation.md); both are closed, so these replayed steps are net-new and read as a regression of that check. The open issue 20261104T085053Z-mpa-identity-source-reads-and-deletes-refuse-a.md covers only the READ/DELETE identity-source ARN-acceptance ordering (a different, IAM implicit-deny message) and does not cover the create paths reported here.

Cause 1 of 2: CreateApprovalTeam from a caller that is not the management account (fixture mpa-approval-team-create-validation-error, step task#2).
AWS: Smithy: CreateApprovalTeam POST /approval-teams, errors AccessDeniedException (httpError 403, doc "You do not have sufficient access to perform this action. Check your permissions, and try again.", Message @required String @length max 1000), ConflictException (409), ServiceQuotaExceededException (402), InternalServerException (500), ThrottlingException (429), ValidationException (400, doc "The input fails to satisfy the constraints specified by an Amazon Web Services service."). The recording shows AWS deciding the management-account precondition first and never reaching body validation: HTTP 403, x-amzn-errortype "AccessDeniedException:http://internal.amazon.com/coral/com.amazon.awsfluffycoreservice/", $.Message "Action can only be performed by an organization’s management account." — that whole sentence is the error class's FIXED text (note the typographic apostrophe U+2019 in "organization’s"); it interpolates nothing from the request or from state.
Emulator: HTTP 400, x-amzn-errortype "ValidationException", $.Message "Policies names a policy version that does not exist: arn:aws:mpa:::aws:policy/backup.amazonaws.com/CreateRestoreAccessVault/$DEFAULT. Attach a policy version ListPolicies and ListPolicyVersions report." — an emulator-authored policy-reference complaint reached because the authorization decision was skipped and the request body was validated instead. (The ARN quoted is the request's own Policies value; the wording is the emulator's, not AWS's.)
Why: a client probing which principal may create an approval team is told its request body is malformed rather than that it lacks the management-account privilege, so SDK/catch logic branching on AccessDeniedException never fires, the 403 status assertion fails, and a caller "fixes" a non-existent Policies problem instead of assuming the management-account role. Worse, the ordering means the emulator leaks which input values it dislikes to a caller AWS would have refused unauthenticated-of-privilege up front.
Fix: the CreateApprovalTeam handler in the mpa service crate's per-operation handler directory (services/service-com-amazonaws-mpa-awsfluffycoreservice/src/ops/): the organization management-account check must run before request-body/Policies validation and raise AccessDeniedException (403) with the fixed message above and the awsfluffycoreservice-namespaced errortype.
Traces: mpa-approval-team-create-validation-error#task#2.

Cause 2 of 2: CreateIdentitySource from a caller that is not the management account (fixture mpa-identity-source-lifecycle-errors, step task#2).
AWS: Smithy: CreateIdentitySource POST /identity-sources, input IdentitySourceParameters @required plus ClientToken/Tags; errors AccessDeniedException (403), ServiceQuotaExceededException (402), InternalServerException (500), ThrottlingException (429), ValidationException (400). The recording shows HTTP 403, x-amzn-errortype "AccessDeniedException:http://internal.amazon.com/coral/com.amazon.awsfluffycoreservice/", $.Message "Action can only be performed by an organization’s management account." — same fixed, value-free message as Cause 1; no identity source is created.
Emulator: HTTP 200 with a success CreateIdentitySourceResponse body and NO error at all: both $.Message and the x-amzn-errortype header are absent.
Why: this is the more damaging of the two — the emulator does not merely mislabel the refusal, it performs a privileged create that AWS rejects. A caller asserting that a non-management-account principal cannot create an identity source sees it succeed, and subsequent steps run against an identity source that would not exist on AWS, so every downstream read/delete assertion in the same flow is evaluated against state AWS never holds. Negative-authorization tests pass vacuously.
Fix: the CreateIdentitySource handler in the same per-operation handler directory: gate the create on the organization management-account check before any state mutation and before ClientToken/idempotency handling, returning AccessDeniedException (403) with the fixed message rather than creating the identity source.
Traces: mpa-identity-source-lifecycle-errors#task#2.

## Evidence
Cause 1 — fixture mpa-approval-team-create-validation-error step task#2, CreateApprovalTeam (POST /approval-teams):
  expected: status 403; x-amzn-errortype: "AccessDeniedException:http://internal.amazon.com/coral/com.amazon.awsfluffycoreservice/"; $.Message: "Action can only be performed by an organization’s management account." (entirely fixed text for this error class)
  actual:   status 400; x-amzn-errortype: "ValidationException"; $.Message: "Policies names a policy version that does not exist: arn:aws:mpa:::aws:policy/backup.amazonaws.com/CreateRestoreAccessVault/$DEFAULT. Attach a policy version ListPolicies and ListPolicyVersions report."
  replay divergence_key: service=mpa|CreateApprovalTeam|status=403->400|keys=$.Message,$headers.x-amzn-errortype,status

Cause 2 — fixture mpa-identity-source-lifecycle-errors step task#2, CreateIdentitySource (POST /identity-sources):
  expected: status 403; x-amzn-errortype: "AccessDeniedException:http://internal.amazon.com/coral/com.amazon.awsfluffycoreservice/"; $.Message: "Action can only be performed by an organization’s management account."
  actual:   status 200; $.Message missing (None); x-amzn-errortype header missing (None) — a success response, the identity source is created
  replay divergence_key: service=mpa|CreateIdentitySource|status=403->200|keys=$.Message,$headers.x-amzn-errortype,status

Model evidence (services/service-com-amazonaws-mpa-awsfluffycoreservice/smithy-model/com-amazonaws-mpa-awsfluffycoreservice.json): AccessDeniedException is a client error with httpError 403 and a @required Message (String, @length max 1000), and is in the service's common errors, so it is bound to both CreateApprovalTeam and CreateIdentitySource; both ops also model ValidationException (400) for input-shape failures only.

## Steps to Reproduce
1. Boot the emulator and replay the wire corpus (trace set simparity-wire-aws-v1): replay_wire_corpus().
2. Cause 1: fixture mpa-approval-team-create-validation-error, step task#2 — call mpa CreateApprovalTeam (POST /approval-teams) as a caller that is not the organization's management account. Observe 400 ValidationException with the emulator's "Policies names a policy version that does not exist: ..." message where the recording has 403 AccessDeniedException with "Action can only be performed by an organization’s management account." and x-amzn-errortype "AccessDeniedException:http://internal.amazon.com/coral/com.amazon.awsfluffycoreservice/".
3. Cause 2: fixture mpa-identity-source-lifecycle-errors, step task#2 — call mpa CreateIdentitySource (POST /identity-sources) as the same non-management-account caller. Observe HTTP 200 and a created identity source where the recording has the same 403 AccessDeniedException refusal and no resource created.
4. After the fix, re-replay both steps: each must answer 403 with x-amzn-errortype AccessDeniedException (awsfluffycoreservice coral namespace) and the exact fixed Message, and CreateIdentitySource must leave no identity source behind.
