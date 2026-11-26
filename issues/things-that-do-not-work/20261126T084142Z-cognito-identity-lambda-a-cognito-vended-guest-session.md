Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-identity__lambda.xsvc-cognito-identity-3-lambda-context-carries-the-vended-identity-and-none-for-an-iam-caller
Issue-Key: cognito-identity__lambda.xsvc-cognito-identity-3-lambda-context-carries-the-vended-identity-and-none-for-an-iam-caller|_error#37|AssertionError|raise AssertionError(f"expected a rejection, but the call SUCCEEDED and returned {result!r}")
Issue-Commit: 47635055653444c8189ab0ed1a8047834cbe5b1c

# cognito-identity__lambda: a Cognito-vended guest session whose role allows only lambda:InvokeFunction is still served GetFunction 200 instead of AccessDeniedException

> _Note: this issue was identified against the code base at commit `47635055653444c8189ab0ed1a8047834cbe5b1c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetFunction

## Description
Cause 1 of 1: Lambda control-plane reads are not authorized against the identity-based policy of the role a Cognito identity pool vended, so a session scoped to one action is allowed every action.

Failing check ids: cognito-identity__lambda.xsvc-cognito-identity-3-lambda-context-carries-the-vended-identity-and-none-for-an-iam-caller

AWS: Smithy (services/service-com-amazonaws-lambda-awsgirapiservice/smithy-model, GetFunction) models only InvalidParameterValueException/ResourceNotFoundException/ServiceException/TooManyRequestsException — the authorization refusal is the service-common IAM one and is not in the operation's error list, so for the refusal itself this is recorded behavior only. A request signed with temporary credentials from cognito-identity:GetCredentialsForIdentity is evaluated against the identity-based policy attached to the identity pool's role. When that policy grants only `lambda:InvokeFunction` on one function ARN, `lambda:GetFunction` is not allowed and AWS refuses with HTTP 403, error code `AccessDeniedException`, message of the form:
  `User: <session-arn> is not authorized to perform: lambda:GetFunction on resource: <function-arn> because no identity-based policy allows the lambda:GetFunction action`
In that message the fixed text is `User: `, ` is not authorized to perform: `, ` on resource: `, ` because no identity-based policy allows the `, ` action`. The variable parts come from the request and from state: <session-arn> is `arn:aws:sts::<account-id>:assumed-role/<role-name>/CognitoIdentityCredentials` — the account of the signing credentials, the role name configured as the pool's `unauthenticated` role, and the role-session name `CognitoIdentityCredentials` that Cognito fixes for pool-vended sessions; the action name is the operation being refused; <function-arn> is the ARN the FunctionName in the request resolves to. (The literal account id, role name and function name in any one recording are values filling that template, not part of it.)

Emulator: the call SUCCEEDS. Replay shows GetFunction answered HTTP 200 with a full Configuration body (FunctionName/FunctionArn/Role/State: Active ...) for a client signed with the just-vended guest credentials, and the check's `_error()` helper raised `AssertionError: expected a rejection, but the call SUCCEEDED and returned {...'HTTPStatusCode': 200...}`. No AccessDeniedException is produced at all, so neither the status, the error code nor the message exists to compare.

Why: a client that relies on AWS bounding a Cognito-vended session by its role gets the opposite of the real answer. Least-privilege checks pass against the emulator and fail in AWS; worse, a test that asserts a guest session CANNOT read function configuration (role ARNs, environment, code location) silently records a pass while the emulator hands that data to an unprivileged principal. Authorization regressions in application code therefore go undetected.

Fix: the authorization layer the Lambda control-plane handlers consult before serving a read — the request's signing credentials must be resolved to the assumed-role session the Cognito pool vended, the identity-based policy of that role evaluated for the requested action/resource, and a non-matching request refused with the 403 AccessDeniedException above rather than falling through to GetFunction's happy path. The same gate is what the other Lambda control-plane operations need; GetFunction is where the check observes it.

Note: the earlier parts of this check (context.identity propagation, filed as 20261105T111951Z-cognito-identity-lambda-the-vended-cognito-identity-never.md, now marked fixed) pass; the check now fails further down at the authorization assertion, which that issue did not cover and which is not a refusal of the operation it fixed.

Traces: cognito-identity__lambda.xsvc-cognito-identity-3-lambda-context-carries-the-vended-identity-and-none-for-an-iam-caller (assertion at the check's `_error(lambda: vended_lam.get_function(...))` step).

## Evidence
Check cognito-identity__lambda.xsvc-cognito-identity-3-lambda-context-carries-the-vended-identity-and-none-for-an-iam-caller, [mismatch].

Expected (AWS): GetFunction signed with the Cognito-vended guest credentials is refused — HTTP 403, Error.Code `AccessDeniedException`, Error.Message `User: arn:aws:sts::<account-id>:assumed-role/<guest-role-name>/CognitoIdentityCredentials is not authorized to perform: lambda:GetFunction on resource: <function-arn> because no identity-based policy allows the lambda:GetFunction action`, because the pool's unauthenticated role carries only `{"Effect":"Allow","Action":"lambda:InvokeFunction","Resource":"<function-arn>"}`.

Actual (emulator): HTTP 200 with the function's Configuration. Replay detail:
  File "check_cognito_identity__lambda_xsvc_cognito_identity_3_lambda_context_carries_the_vended_identity_and_none_for_an_iam_caller.py", line 254, in run
    code, message = _error(lambda: vended_lam.get_function(FunctionName=fn_name))
  AssertionError: expected a rejection, but the call SUCCEEDED and returned {'ResponseMetadata': {... 'HTTPStatusCode': 200 ...}, 'Configuration': {'FunctionName': ..., 'FunctionArn': 'arn:aws:lambda:<region>:<account>:function:<name>', 'Role': 'arn:aws:iam::<account>:role/<exec-role>', 'State': 'Active', ...}}

The positive legs preceding it pass: the vended session can Invoke, and context.identity carries the identity id and pool id, so the session itself is intact — only the policy evaluation is missing.

## Steps to Reproduce
1. iam:CreateRole an execution role granting nothing, and lambda:CreateFunction a python3.12 function with it; wait for State=Active.
2. cognito-identity:CreateIdentityPool with AllowUnauthenticatedIdentities=true.
3. iam:CreateRole a guest role trusted by cognito-identity.amazonaws.com for sts:AssumeRoleWithWebIdentity (aud = the pool id, amr = unauthenticated), and iam:PutRolePolicy on it allowing ONLY `lambda:InvokeFunction` on the function's ARN.
4. cognito-identity:SetIdentityPoolRoles with Roles={"unauthenticated": <guest-role-arn>}.
5. cognito-identity:GetId, then GetCredentialsForIdentity for that identity id.
6. Build a lambda client signed with the returned AccessKeyId/SecretKey/SessionToken and call lambda:Invoke — succeeds (correct).
7. With the SAME vended credentials call lambda:GetFunction(FunctionName=<name>).
   Expected: 403 AccessDeniedException with the message above. Observed: 200 and the full function configuration.
