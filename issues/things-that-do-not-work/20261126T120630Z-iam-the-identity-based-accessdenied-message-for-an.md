Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam.session-policy-intersects-the-role-policy
Issue-Shape-Id: com.amazonaws.iam#AWSIdentityManagementV20100508
Issue-Key: iam.session-policy-intersects-the-role-policy|run#32|AssertionError|assert grant_msg == expected_grant_msg, (
Issue-Commit: eef3054027f29376844deedfa2c9294c0516da9b

# iam: the identity-based AccessDenied message for an assumed-role session omits AWS's authorization-details trailer, so the session-policy-intersection check still fails after the session-policy branch was fixed (1 cause)

> _Note: this issue was identified against the code base at commit `eef3054027f29376844deedfa2c9294c0516da9b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateUser

## Description
One net-new root cause. Cause count: 1. A fixer must close this one cause before submitting.

This is NOT a reopen: the call is refused with exactly the code and status the recording shows (HTTP 403 / AccessDenied), which is what the check wants, and the recording does not succeed — only the message text is short. History, named so a fixer sees it and can tell that neither entry covers this replay key: FIXED 20261107T153110Z-iam-the-session-policy-accessdenied-message-omits-aws.md carries this check's replay key (iam.session-policy-intersects-the-role-policy) and asked for the trailer on the SESSION-POLICY branch; that fix landed — the check's earlier session-policy assertion (`narrow_msg`, iam:ListAttachedRolePolicies, check line 227) now PASSES — and the check now fails ~40 lines later on the IDENTITY-BASED branch, a different deny reason reached by a different step. OPEN 20261119T085411Z-iam-every-accessdenied-refusal-message-stops-at-action.md describes this same identity-based truncation across 26 checks but carries none of this check's replay key, so it does not cover it; the open issues 20261117T135333Z-iam-6-net-new-causes-denials-omit-the.md, 20261109T163411Z-iam-3-net-new-causes-denials-omit-the.md and 20261118T021609Z-iam-4-net-new-causes-denials-append-an.md are the same message family and likewise carry none of it. The failing check is marked neither "ALREADY FILED" nor "OWNED by" in the replay report, while every other iam failure in this run is marked one of the two and was skipped.

=== Cause 1 of 1: when an assumed-role session is refused because the ROLE's identity policy does not allow the action (its session policy does), the AccessDenied message stops at '... action' and omits AWS's whole authorization-details sentence ===

AWS: IAM speaks awsQuery (com.amazonaws.iam#AWSIdentityManagementV20100508, version 2010-05-08). Smithy: CreateUser takes Path, UserName (@required, userNameType, @length 1..64, @pattern ^[\w+=,.@-]+$), PermissionsBoundary and Tags, and models LimitExceededException (awsQueryError LimitExceeded, HTTP 409), EntityAlreadyExistsException (EntityAlreadyExists, 409), NoSuchEntityException (NoSuchEntity, 404), InvalidInputException (InvalidInput, 400), ConcurrentModificationException (ConcurrentModification, 409) and ServiceFailureException (ServiceFailure, 500) — the authorization refusal AccessDenied / HTTP 403 is a service-common error whose message text the model does not state, so the message requirement is recorded behavior only. An inline AssumeRole session policy confers nothing on its own (the effective permissions are the INTERSECTION of the role policy and the session policy), so an action present only in the session policy is refused for the identity-based reason, and the recorded refusal is TWO sentences:
  `User: <assumed-role-caller-arn> is not authorized to perform: <service>:<Action> on resource: <resource-description> because no identity-based policy allows the <service>:<Action> action. Go to https://us-east-1.console.aws.amazon.com/iam/home?region=us-east-1#/authorization-details/<authorization-id> for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: <authorization-id>`
Fixed text that must appear verbatim: `User: `, ` is not authorized to perform: `, ` on resource: `, ` because no identity-based policy allows the `, ` action. Go to https://us-east-1.console.aws.amazon.com/iam/home?region=us-east-1#/authorization-details/`, ` for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: `. The `us-east-1` host and the `region=us-east-1` query parameter are FIXED text, not the caller's Region, because IAM is global and signed in us-east-1. Variable parts, all taken from the signed request and the emulator's own state: `<assumed-role-caller-arn>` (the `arn:aws:sts::<account>:assumed-role/<role>/<session-name>` that signed the request), `<service>:<Action>` (the refused action, `iam:CreateUser` here, appearing twice), `<resource-description>` (how IAM names the evaluated resource for that action — the recorded form for CreateUser is the full user ARN `arn:aws:iam::<account>:user/<UserName>` built from the request's UserName), and `<authorization-id>` (the per-request authorization id, repeated inside the console URL). The check reads that id back OUT of the received message by splitting on the marker `authorization id: ` and substitutes the literal `<absent>` only when the marker does not occur at all, so no per-run value is ever compared: the requirement is that the sentence exist.

Emulator: the decision, the status and the code are right — the session policy confers nothing, HTTP 403, `Error.Code` `AccessDenied`, and the first sentence is byte-for-byte correct — and then the message ENDS. The entire second sentence is absent, and with it any authorization id to follow. The asymmetry is the tell: on the SESSION-POLICY deny reason (`... because no session policy allows the iam:ListAttachedRolePolicies action`) the same check's earlier assertion now passes with the trailer present, so the trailer is emitted on one refusal branch and not on the other.

Why: a client that reads AWS's denial text to route an operator to the IAM authorization-details console page, or that parses the message to pass an authorization id to GetRequestAuthorizationDetails, receives a truncated string with neither — the only troubleshooting handle an IAM refusal carries. Policy-troubleshooting tooling and log scrapers built against real IAM do not recognise the refusal, and any assertion on the documented full message fails even though the allow/deny decision itself was correct.

Fix: the shared authorization-refusal denial-message builder in the IAM crate — the policy-evaluation / denial-message component that renders the AccessDenied body ahead of the operation handlers (reached from services/service-com-amazonaws-iam-awsidentitymanagementv20100508/src/service.rs), not the CreateUser handler, which is never reached once the request is refused. The trailer must be appended for the identity-based deny reason on an assumed-role caller as it already is for the session-policy reason, with the console host and the region query parameter pinned to the literal us-east-1 and the authorization id taken from the request's authorization context and repeated in both positions. The deny-reason sentence itself needs no change.

Traces: iam.session-policy-intersects-the-role-policy, assertion `grant_msg == expected_grant_msg` at check line 268 (the step that assumes the role with an inline session policy granting iam:CreateUser on `*` and then calls CreateUser). The earlier session-policy assertion at check line 227 passes.

## Evidence
Verdict `mismatch`, listed under the suite heading `### iam`, marked neither "ALREADY FILED" nor "OWNED by".

  File "check_iam_session_policy_intersects_the_role_policy.py", line 268, in run
  assert grant_msg == expected_grant_msg, (
  AssertionError: expected message 'User: arn:aws:sts::<account>:assumed-role/<role>/<session> is not authorized to perform: iam:CreateUser on resource: arn:aws:iam::<account>:user/<probe-user> because no identity-based policy allows the iam:CreateUser action. Go to https://us-east-1.console.aws.amazon.com/iam/home?region=us-east-1#/authorization-details/<absent> for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: <absent>', got 'User: arn:aws:sts::<account>:assumed-role/<role>/<session> is not authorized to perform: iam:CreateUser on resource: arn:aws:iam::<account>:user/<probe-user> because no identity-based policy allows the iam:CreateUser action'

The expectation is built by the check's own helper `_with_authorization_details(base, message)`, which reads the authorization id back out of the RECEIVED message after the marker `authorization id: ` and substitutes the literal `<absent>` only when that marker is not in the message at all. So the received string is a strict prefix of the required one and the sole difference is the whole trailing sentence; no per-run authorization id, host, clock value or request id is compared.

EXPECTED (AWS): HTTP 403, Error.Code `AccessDenied`, Error.Message = the identity-based deny sentence + '. Go to https://us-east-1.console.aws.amazon.com/iam/home?region=us-east-1#/authorization-details/<authorization-id> for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: <authorization-id>'.
ACTUAL (emulator): HTTP 403, Error.Code `AccessDenied`, Error.Message = the deny sentence only.

Controls in the same check that PASS, isolating the divergence to the message trailer on this one branch:
- `grant_code == "AccessDenied"` passes: the session policy granting iam:CreateUser confers nothing, so the intersection semantics are correct.
- `narrow_get == "UNEXPECTED_SUCCESS"` passes: an action inside the intersection still succeeds.
- `narrow_list == "AccessDenied"` AND the session-policy message assertion `narrow_msg == expected_msg` (check line 227) both pass WITH the trailer present — the same builder emits it for '... because no session policy allows the iam:ListAttachedRolePolicies action'.

The account id, role/session/user names and the authorization id are per-run values filling the template; the two sentences' fixed text and the us-east-1 host/region literal are the requirement.

## Steps to Reproduce
1. iam CreateRole(RoleName=<r>) with a trust policy allowing the calling account's principal to sts:AssumeRole.
2. iam PutRolePolicy(RoleName=<r>, PolicyName=<p>) with a document that Allows iam:GetRole and iam:ListAttachedRolePolicies on <r>'s ARN and does NOT allow iam:CreateUser.
3. sts AssumeRole(RoleArn=<r's ARN>, RoleSessionName=<s>, Policy=<an inline session policy that Allows iam:CreateUser on "*">) — retry only AccessDenied while the trust policy propagates.
4. Build an iam client signing with the returned AccessKeyId/SecretAccessKey/SessionToken.
5. Call iam CreateUser(UserName=<probe>, Path="/") with that client.
6. Observe HTTP 403 and Error.Code 'AccessDenied' (both correct — a session policy grants nothing on its own) and Error.Message ending at '... because no identity-based policy allows the iam:CreateUser action'. Real IAM continues with '. Go to https://us-east-1.console.aws.amazon.com/iam/home?region=us-east-1#/authorization-details/<authorization-id> for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: <authorization-id>'. Assert the trailer by shape (match the URL and the id as opaque tokens, never pin their values).
7. Contrast with the branch that already works, which must keep its trailer: from a session whose inline Policy allows only iam:GetRole, call iam ListAttachedRolePolicies(RoleName=<r>) — the refusal reads '... because no session policy allows the iam:ListAttachedRolePolicies action' and DOES carry the trailer.
