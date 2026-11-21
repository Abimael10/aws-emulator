Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sts.assume-role-session-policy-action-wildcard-granularity sts.assume-role-session-policy-arns-intersect-role-permissions sts.assume-role-session-policy-intersection-allows-and-denies sts.assume-root-duration-exceeds-max-rejected sts.assume-root-malformed-target-principal-rejected sts.assume-root-task-policy-arn-not-a-root-task-policy-rejected sts.get-session-token-duration-exceeds-max-rejected sts.get-federation-token-tags-exceeding-max-count-rejected sts.get-session-token-minimum-session-token-size-exceeds-max-rejected
Issue-Shape-Id: com.amazonaws.sts#AWSSecurityTokenServiceV20110615
Issue-Key: sts.assume-role-session-policy-action-wildcard-granularity|run#34|AssertionError|assert message_exact == _expected_denial(message_exact, (
Issue-Key: sts.assume-role-session-policy-arns-intersect-role-permissions|run#46|AssertionError|assert denied_message == _expected_denial(denied_message, (
Issue-Key: sts.assume-role-session-policy-intersection-allows-and-denies|run#42|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Key: sts.assume-root-duration-exceeds-max-rejected|run#6|AssertionError|assert control_code == "AccessDenied", (
Issue-Key: sts.assume-root-malformed-target-principal-rejected|run#5|AssertionError|assert control_code == "AccessDenied", (
Issue-Key: sts.assume-root-task-policy-arn-not-a-root-task-policy-rejected|run#5|AssertionError|assert control_code == "AccessDenied", (
Issue-Key: sts.get-session-token-duration-exceeds-max-rejected|run#3|AssertionError|assert at_max["Credentials"]["AccessKeyId"].startswith("ASIA"), (
Issue-Key: sts.get-federation-token-tags-exceeding-max-count-rejected|run#3|AssertionError|assert at_max["Credentials"]["AccessKeyId"].startswith("ASIA"), (
Issue-Key: sts|GetSessionToken|ValidationException
Issue-Commit: 78812e82c5d05e63ee0f30b5ef7177ddca283e5f

# sts: 4 net-new causes — session-policy AccessDenied messages drop AWS's authorization-details pointer, AssumeRoot answers NotImplemented, GetSessionToken/GetFederationToken mint EMULATOR-class key ids, and GetSessionToken refuses MinimumSessionTokenSize=4096

> _Note: this issue was identified against the code base at commit `78812e82c5d05e63ee0f30b5ef7177ddca283e5f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssumeRole

## Description
Four distinct root causes in the STS surface, all grounded in failing behavioral checks listed under `### sts`.

Cause 1 of 4: an AccessDenied produced by a session-policy scope-down omits the trailing authorization-details pointer AWS appends to the message.
AWS: recorded behavior only (no Smithy trait governs message text). The recording shows AWS's IAM/STS denial message for a request a session policy fails to allow is the head `User: <assumed-role ARN of the session> is not authorized to perform: <service:Action> on resource: <resource ARN> because no session policy allows the <service:Action> action` FOLLOWED BY the fixed sentence `. Go to https://<region>.console.aws.amazon.com/iam/home?region=<region>#/authorization-details/<authorization-id> for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: <authorization-id>`. Fixed text: everything except the variable parts — the principal ARN, the action, the resource ARN, the region, and the per-request authorization id (which appears twice and must be the same value in both places). The status/code are unchanged: HTTP 403, Error.Code `AccessDenied`.
Emulator: the message ends at `... because no session policy allows the iam:ListUsers action` — the entire `. Go to https://...` pointer sentence is absent. Replay (sts.assume-role-session-policy-intersection-allows-and-denies) shows expected `...because no session policy allows the iam:ListRoles action. Go to <authorization-details pointer>` vs got `...because no session policy allows the iam:ListRoles action`.
Why: a client or operator that parses the denial for the authorization id (to call GetRequestAuthorizationDetails or open the console link) finds nothing, and any assertion on AWS's full denial text fails even though the authorization decision itself was right.
Fix: the denial-message formatter shared by the request-authorization path (the component that renders the AccessDenied message for session-policy denials), not the policy evaluator — the verdict and error code are already correct.
Traces: sts.assume-role-session-policy-action-wildcard-granularity, sts.assume-role-session-policy-arns-intersect-role-permissions, sts.assume-role-session-policy-intersection-allows-and-denies.

Cause 2 of 4: AssumeRoot is not implemented, so none of its input validation or authorization exists.
AWS: Smithy: AssumeRoot input TargetPrincipal (required, @length min 12 max 2048), TaskPolicyArn (required, PolicyDescriptorType), DurationSeconds @range min 0 max 900, MinimumSessionTokenSize @range min 0 max 4096; modeled errors ExpiredTokenException (400) and RegionDisabledException (403). Recorded behavior: for a caller with no centralized-root-access grant, a well-formed AssumeRoot request (TargetPrincipal = the caller's own 12-digit account id, TaskPolicyArn = a real `arn:aws:iam::aws:policy/root-task/<name>` ARN, DurationSeconds at the 900 maximum) is refused HTTP 403 with Error.Code `AccessDenied`; range/shape validation runs AHEAD of that authorization, so DurationSeconds=901 is refused `ValidationError` with the message `1 validation error detected: Value '901' at 'durationSeconds' failed to satisfy constraint: Member must have value less than or equal to 900` (fixed text apart from the submitted value and the limit), a TargetPrincipal that is not a 12-digit account id is refused `ValidationError` with the fixed message `Request must contain a valid target principal`, and a TaskPolicyArn outside the root-task job-function set is refused `ValidationError` with the fixed message `Request must contain valid AWS managed task policy ARN`.
Emulator: every AssumeRoot call answers Error.Code `NotImplemented`, so neither the authorization refusal nor any of the three validation refusals can be reached.
Why: a client exercising centralized root access gets an unmodeled error code instead of AccessDenied/ValidationError, cannot distinguish a malformed request from an unauthorized one, and cannot test its own error handling for root-task sessions at all.
Fix: implement the AssumeRoot operation handler plus its input validation (durationSeconds range, target-principal shape, root-task policy-ARN membership) ordered before the root-assumption authorization check.
Traces: sts.assume-root-duration-exceeds-max-rejected, sts.assume-root-malformed-target-principal-rejected, sts.assume-root-task-policy-arn-not-a-root-task-policy-rejected.

Cause 3 of 4: GetSessionToken and GetFederationToken mint an AccessKeyId outside the STS temporary (ASIA) class.
AWS: recorded behavior only (Credentials.AccessKeyId carries no model constraint). Every STS-minted temporary credential's AccessKeyId begins with the `ASIA` key-class prefix; the remainder is a per-call random value and is not part of the requirement.
Emulator: GetSessionToken(DurationSeconds=129600) returned AccessKeyId `EMULATORAS7TEBCUJNUA` and GetFederationToken with 50 Tags returned `EMULATORASYTNBV3LINR` — an `EMULATOR`-prefixed id. The call itself succeeded (HTTP 200), so this is a wrong value, not a refusal.
Why: any client, policy condition or tool that classifies a credential by its key-id prefix (ASIA = temporary/session, AKIA = long-term) misclassifies every emulator session credential; it also blocks these two boundary checks from ever reaching their validation assertions.
Fix: the credential-minting component shared by the STS token operations (the access-key-id generator used when issuing temporary credentials). NOTE: open issues 20261115T051545Z-sts-getsessiontoken-mints-an-emul-class-accesskeyid-instead.md and 20261102T105139Z-sts-assumerole-mints-temporary-credentials-with-an-emulator.md describe the same class defect for GetSessionToken and AssumeRole; neither is marked as owning these two checks, and neither covers GetFederationToken, so this cause is filed with them named.
Traces: sts.get-session-token-duration-exceeds-max-rejected, sts.get-federation-token-tags-exceeding-max-count-rejected.

Cause 4 of 4: GetSessionToken refuses MinimumSessionTokenSize at its modeled maximum with an emulator-authored error.
AWS: Smithy: GetSessionToken.MinimumSessionTokenSize targets minimumSessionTokenSizeType with @range min 0 max 4096, so 4096 is a legal value and must be ACCEPTED (HTTP 200, Credentials minted, SessionTokenSize/SessionTokenUtilization reported); only a value past the maximum is refused, with Error.Code `ValidationError` and the message `1 validation error detected: Value '<submitted value>' at 'minimumSessionTokenSize' failed to satisfy constraint: Member must have value less than or equal to 4096` (fixed text apart from the submitted value and the limit).
Emulator: GetSessionToken(MinimumSessionTokenSize=4096) is refused with Error.Code `ValidationException` and the emulator-authored message `MinimumSessionTokenSize 4096 is larger than the 712-byte session token this emulator mints, and a minted session token cannot be padded (the limit is 4096 bytes)` — a code AWS does not use for STS awsQuery validation, a message that describes the emulator's own token encoding, and a refusal of a value the model declares legal.
Why: a client that asks for a minimum session-token size inside the modeled range is refused outright, and the error code it must handle (`ValidationException`) is not the one STS sends (`ValidationError`), so retry/validation logic written against AWS breaks.
Fix: GetSessionToken's input validation and its credential/session-token serializer — accept any MinimumSessionTokenSize within 0..=4096 (padding the minted session token to the requested size) and refuse only values past 4096, with STS's ValidationError code and message template.
Traces: sts.get-session-token-minimum-session-token-size-exceeds-max-rejected.

## Evidence
Cause 1 — sts.assume-role-session-policy-intersection-allows-and-denies:
  expected: "User: arn:aws:sts::<account>:assumed-role/<role>/<session> is not authorized to perform: iam:ListRoles on resource: arn:aws:iam::<account>:role/ because no session policy allows the iam:ListRoles action. Go to <authorization-details pointer>"
  got:      "User: arn:aws:sts::<account>:assumed-role/<role>/<session> is not authorized to perform: iam:ListRoles on resource: arn:aws:iam::<account>:role/ because no session policy allows the iam:ListRoles action"
  (the same tail is missing in sts.assume-role-session-policy-action-wildcard-granularity and sts.assume-role-session-policy-arns-intersect-role-permissions, for iam:ListUsers)

Cause 2 — all three assume-root checks:
  expected Error.Code for the well-formed neighbour control (TargetPrincipal=<own account>, TaskPolicyArn=arn:aws:iam::aws:policy/root-task/S3UnlockBucketPolicy, DurationSeconds=900): "AccessDenied"
  got: "NotImplemented"
  so the intended ValidationError refusals were never reached:
    DurationSeconds=901 -> ValidationError "1 validation error detected: Value '901' at 'durationSeconds' failed to satisfy constraint: Member must have value less than or equal to 900"
    TargetPrincipal not an account id -> ValidationError "Request must contain a valid target principal"
    TaskPolicyArn=arn:aws:iam::aws:policy/AdministratorAccess -> ValidationError "Request must contain valid AWS managed task policy ARN"

Cause 3:
  GetSessionToken(DurationSeconds=129600) -> Credentials.AccessKeyId expected to start with "ASIA", got 'EMULATORAS7TEBCUJNUA'
  GetFederationToken(Name=<name>, Tags=[50 tags]) -> Credentials.AccessKeyId expected to start with "ASIA", got 'EMULATORASYTNBV3LINR'

Cause 4:
  GetSessionToken(MinimumSessionTokenSize=4096) expected: HTTP 200 with minted Credentials
  got: ClientError (ValidationException) "MinimumSessionTokenSize 4096 is larger than the 712-byte session token this emulator mints, and a minted session token cannot be padded (the limit is 4096 bytes)"
  expected refusal only at 4097: ValidationError "1 validation error detected: Value '4097' at 'minimumSessionTokenSize' failed to satisfy constraint: Member must have value less than or equal to 4096"

## Steps to Reproduce
Cause 1: create a role whose inline policy allows iam:List*, AssumeRole it with an inline session Policy whose only Allow is a narrower action set (e.g. Action=["iam:ListRoles"]), then call iam:ListUsers with the minted credentials. Read Error.Message: it must end with the ". Go to https://<region>.console.aws.amazon.com/iam/home?region=<region>#/authorization-details/<id> for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: <id>" pointer.

Cause 2: call sts:AssumeRoot with TargetPrincipal=<the caller's own 12-digit account id>, TaskPolicyArn={"arn":"arn:aws:iam::aws:policy/root-task/S3UnlockBucketPolicy"}, DurationSeconds=900 (expect AccessDenied, not NotImplemented); then repeat with DurationSeconds=901, with TargetPrincipal="not-an-account-id-but-long-enough", and with TaskPolicyArn={"arn":"arn:aws:iam::aws:policy/AdministratorAccess"} (each expect ValidationError with the message given above). Use a client with parameter_validation=False so STS's own validation answers.

Cause 3: call sts:GetSessionToken(DurationSeconds=129600) and sts:GetFederationToken(Name=<2-32 chars>, Tags=[50 entries]) and inspect Credentials.AccessKeyId — it must be ASIA-class.

Cause 4: call sts:GetSessionToken(MinimumSessionTokenSize=4096) — it must succeed and mint credentials; then sts:GetSessionToken(MinimumSessionTokenSize=4097) — it must be refused ValidationError with the "1 validation error detected ... minimumSessionTokenSize ... less than or equal to 4096" message.
