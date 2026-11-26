Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam.assumed-role-session-is-authorized-by-the-role-policy
Issue-Shape-Id: com.amazonaws.iam#AWSIdentityManagementV20100508
Issue-Key: iam.assumed-role-session-is-authorized-by-the-role-policy|_denial_message#29|AssertionError|raise AssertionError(f"real AWS refuses this call, but it SUCCEEDED and returned {result!r}")
Issue-Commit: 5af22e124f1b4edcbe644b4b43fbf6a6331dae9c

# iam: an assumed-role session whose only permission policy is the AWS-managed AWSDenyAll is served 200 instead of AccessDenied

> _Note: this issue was identified against the code base at commit `5af22e124f1b4edcbe644b4b43fbf6a6331dae9c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListAttachedRolePolicies

## Description
One root cause, grounded in one failing behavioral check that the replay reports owned by no open issue and blocked by no unimplemented stub.

Cause 1 of 1: a request signed by a role session whose role carries ONLY an attached AWS-managed policy is authorized as if the role granted the action, so a session of a role carrying nothing but arn:aws:iam::aws:policy/AWSDenyAll completes an IAM call that AWS refuses.

AWS: a role session is authorized by the role's identity-based policies alone — AssumeRole documents that "the temporary security credentials ... have the permissions of the role" — and AWS's evaluation gives an explicit Deny precedence over every Allow. arn:aws:iam::aws:policy/AWSDenyAll is the AWS-managed policy whose single statement is Effect Deny / Action "*" / Resource "*", so every call made by a session of a role carrying it is refused HTTP 403 with error code AccessDenied. Smithy (com.amazonaws.iam#ListAttachedRolePolicies) models only InvalidInputException (400), NoSuchEntityException (404) and ServiceFailureException (500); AccessDenied is the service-common 403 refusal, so the model itself says the operation has no success path for an unauthorized caller — the rest is recorded behavior. The message AWS returns for this class is: `User: arn:aws:sts::<account-id>:assumed-role/<role-name>/<session-name> is not authorized to perform: iam:ListAttachedRolePolicies on resource: role <role-name> with an explicit deny in an identity-based policy: arn:aws:iam::aws:policy/AWSDenyAll` closed by the authorization-details pointer `. Go to https://<region>.console.aws.amazon.com/iam/home?region=<region>#/authorization-details/<authorization-id> for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: <authorization-id>` (the same authorization id twice). Fixed text is every word of those two sentences; the variable parts are the account id, the session's assumed-role ARN and session name, the role named as the resource, the denying policy's ARN, the region and the per-request authorization id — all taken from the request and the emulator's state, never from one recording. Note that even if the emulator held no text for AWSDenyAll, the role carries no Allow at all, so AWS's implicit deny gives the same 403 AccessDenied class (without the `with an explicit deny in an identity-based policy: ...` clause).

Emulator: the call SUCCEEDS. The deny session's iam:ListAttachedRolePolicies on the sibling role is answered HTTP 200 with `{'AttachedPolicies': [], 'IsTruncated': False}` — no error code, no refusal. The same run proves the surrounding state is right: ListAttachedRolePolicies reports arn:aws:iam::aws:policy/AWSDenyAll attached to the role, GetCallerIdentity for the session returns an arn:aws:sts::<account>:assumed-role/<role>/<session> principal, and the sibling session holding an inline Allow succeeds — only the refusal is missing. The emulator's IAM authorization gate states this as its rule: an AWS-managed policy's document is not held by the account, and "a principal with such an attachment is allowed where the documents that ARE held neither grant nor deny" — which turns both the explicit Deny of AWSDenyAll and the implicit deny of a role with no grant into an allow.

Why: a client cannot test least-privilege or deny-by-default against the emulator. Any session scoped down by an AWS-managed deny policy (AWSDenyAll is the canonical break-glass/CI guard) is silently fully privileged, so a caller that expects AccessDenied sees a successful mutation or read, and a suite that asserts a boundary is enforced passes against an emulator that enforces nothing for AWS-managed attachments.

Fix: IAM's authorization gate and the account's AWS-managed policy catalog — the gate must not resolve "attached policy whose document is unknown" to allow, and the catalog must hold the documents of the AWS-managed policies the emulator lets callers attach (at minimum AWSDenyAll's Deny *) so an explicit deny refuses and a role with no Allow falls to the implicit deny. The refusal itself is already produced by the gate's AccessDenied path, so the message and 403 come from there once the decision is correct.

Traces: iam.assumed-role-session-is-authorized-by-the-role-policy. The rule that produces the allow is documented in services/service-com-amazonaws-iam-awsidentitymanagementv20100508/src/state/authorization.rs (module docs, "The callers that are not IAM users" — "A principal holding a grant this emulator cannot read", lines 114-122); the per-operation gate call is crate::state::authorization::authorize_named. No open issue carries this check: 20261113T053027Z-cloudformation-iam-lambda-s3-s3-createbucket-is-served-200-for-an-assumed.md is the same shape of defect in S3 and 20261115T003554Z-...-iam-simulateprincipalpolicy-answers-501-notimplemented-for-an-aws.md is about SimulatePrincipalPolicy over an unknown AWS-managed policy, so neither covers IAM's own authorization of a session carrying one.

## Evidence
Replay of simparity-behavioral-aws-v1 against the emulator built from this checkout (937/1144 passed; bootstrap_signal false; service token `iam` is bootstrapped).

Failing check `iam.assumed-role-session-is-authorized-by-the-role-policy`, verdict mismatch, detail verbatim:
  File "check_iam_assumed_role_session_is_authorized_by_the_role_policy.py", line 222, in run
    denied_msg = _denial_message(...)
  File "check_iam_assumed_role_session_is_authorized_by_the_role_policy.py", line 113, in _denial_message
    raise AssertionError(f"real AWS refuses this call, but it SUCCEEDED and returned {result!r}")
  AssertionError: real AWS refuses this call, but it SUCCEEDED and returned {'AttachedPolicies': [], 'IsTruncated': False, 'ResponseMetadata': {'RequestId': '<per-run>', 'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'text/xml', 'content-length': '349', ...}, 'RetryAttempts': 0}}

Expected (AWS): HTTP 403, Error.Code `AccessDenied`, Error.Message `User: arn:aws:sts::<account-id>:assumed-role/<deny-role>/<session-name> is not authorized to perform: iam:ListAttachedRolePolicies on resource: role <allow-role> with an explicit deny in an identity-based policy: arn:aws:iam::aws:policy/AWSDenyAll. Go to https://<region>.console.aws.amazon.com/iam/home?region=<region>#/authorization-details/<authorization-id> for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: <authorization-id>`.
Actual (emulator): HTTP 200 with an empty AttachedPolicies list and no Error member at all.

Controls inside the same check that PASSED, and so are not at issue: ListAttachedRolePolicies reports arn:aws:iam::aws:policy/AWSDenyAll attached to the deny role; both sessions' GetCallerIdentity ARNs are arn:aws:sts::<account>:assumed-role/<role>/<session>; the sibling role's session, which holds an inline Allow for iam:ListAttachedRolePolicies on itself, succeeds at the identical call. The request id, account id, generated role/session names and content-length are per-run values.

## Steps to Reproduce
1. Create role A with a trust policy allowing arn:aws:iam::<account>:root to sts:AssumeRole, and attach arn:aws:iam::aws:policy/AWSDenyAll to it (confirm with ListAttachedRolePolicies that the attachment is recorded).
2. Create role B with the same trust policy and PutRolePolicy an inline Allow of iam:ListAttachedRolePolicies on role B's own ARN.
3. sts:AssumeRole both roles; confirm each session's GetCallerIdentity ARN is arn:aws:sts::<account>:assumed-role/<role>/<session>.
4. As role B's session, call iam:ListAttachedRolePolicies(RoleName=B) — expect 200 with an empty AttachedPolicies list (this already works).
5. As role A's session, call the identical iam:ListAttachedRolePolicies(RoleName=B) — AWS answers 403 AccessDenied with the explicit-deny message naming arn:aws:iam::aws:policy/AWSDenyAll and the authorization-details pointer; the emulator answers 200 with an empty AttachedPolicies list.
6. Repeat step 5 against a role with NO policies at all: AWS refuses with the same AccessDenied class (implicit deny, no explicit-deny clause).
7. Re-replay iam.assumed-role-session-is-authorized-by-the-role-policy and confirm it passes.
