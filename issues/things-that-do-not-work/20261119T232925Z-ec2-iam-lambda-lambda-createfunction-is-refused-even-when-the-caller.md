Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__iam__lambda.create-function-requires-matching-security-group-condition
Issue-Key: ec2__iam__lambda.create-function-requires-matching-security-group-condition|_poll#34|TimeoutError|raise TimeoutError(f"timed out after {timeout_s:.0f}s waiting until {what}; last={last!r}")
Issue-Commit: 25f334ef49f0fe78ce039b48068cddfc6b5b1616

# lambda CreateFunction is refused even when the caller's IAM policy condition on lambda:SecurityGroupIds matches the request, so the allowed VpcConfig never succeeds

> _Note: this issue was identified against the code base at commit `25f334ef49f0fe78ce039b48068cddfc6b5b1616`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateFunction

## Description
One net-new root cause, reached by 1 check.

Cause 1 of 1: a CreateFunction call made by an assumed role whose identity-based policy allows lambda:CreateFunction under the condition `"ForAllValues:StringEquals": {"lambda:SecurityGroupIds": [<sg-allowed>]}` is refused by the emulator even when the request's VpcConfig.SecurityGroupIds is exactly [<sg-allowed>] — the request the condition permits.

AWS: recorded behavior only for the authorization outcome (IAM condition-key evaluation is not in any service's Smithy model). The recording establishes the pair: the MATCHING request (VpcConfig.SecurityGroupIds == the single value the condition allows, plus iam:PassRole on the execution role and the ec2:Describe* reads the VPC-config path needs) is ALLOWED — CreateFunction answers 201 with the function's FunctionName/FunctionArn, and GetFunctionConfiguration reads back VpcConfig.SecurityGroupIds == [<sg-allowed>]; the NON-MATCHING request (a different security group) is refused `AccessDeniedException` with message `User: arn:aws:sts::<account>:assumed-role/<caller-role>/<session> is not authorized to perform: lambda:CreateFunction on resource: arn:aws:lambda:<region>:<account>:function:<name> because no identity-based policy allows the lambda:CreateFunction action` — fixed text "User: ", " is not authorized to perform: lambda:CreateFunction on resource: ", " because no identity-based policy allows the lambda:CreateFunction action", with the caller's assumed-role ARN, the target function ARN, region and account taken from the request and the caller's session. The check deliberately runs the ALLOWED request first, precisely so that the denial it grades afterwards can be attributed to the condition rather than to a policy that has not propagated.

Emulator: the matching request is never allowed. Every attempt over a 55 s window comes back as one of AccessDenied / AccessDeniedException / InvalidParameterValueException / UnrecognizedClientException, so the check's poll for "CreateFunction with the matching SecurityGroupIds is allowed" never observes a success and times out with last=None. The emulator therefore either does not evaluate the `lambda:SecurityGroupIds` condition key against the request's VpcConfig (treating the conditioned Allow as never matching), or does not honour the role's inline policy / the assumed-role session at all on this path.

Why: a client cannot use IAM conditions to scope Lambda VPC configuration — the canonical guard-rail for "functions may only attach to this security group" turns into a blanket denial of every CreateFunction, matching or not. Because the allow and the deny are then indistinguishable, no caller can tell a policy that is working from one that is mis-scoped, and least-privilege Lambda provisioning is impossible.

Fix: the authorization path for lambda CreateFunction / UpdateFunctionConfiguration (the IAM policy-evaluation component that resolves request context keys, specifically the `lambda:SecurityGroupIds` multi-valued condition key sourced from VpcConfig.SecurityGroupIds, together with the assumed-role session's inline role policy lookup). A conditioned Allow whose condition the request satisfies must authorize the call; the AccessDeniedException above must be reserved for the request that does not satisfy it.

Traces: ec2__iam__lambda.create-function-requires-matching-security-group-condition — fails at the first phase (the allowed request), so the graded denial message is never reached.

## Evidence
Expected: with an assumed role carrying the inline policy {Allow iam:PassRole on the exec role; Allow ec2:DescribeSecurityGroups/DescribeSubnets/DescribeVpcs/GetSecurityGroupsForVpc on *; Allow lambda:CreateFunction+lambda:UpdateFunctionConfiguration on * with Condition ForAllValues:StringEquals lambda:SecurityGroupIds = [<sg-allowed>]}, CreateFunction(VpcConfig={SubnetIds:[<subnet>], SecurityGroupIds:[<sg-allowed>]}) succeeds and GetFunctionConfiguration reads back SecurityGroupIds == [<sg-allowed>]; only CreateFunction(... SecurityGroupIds:[<sg-other>]) is refused AccessDeniedException with AWS's "User: ... is not authorized to perform: lambda:CreateFunction on resource: ... because no identity-based policy allows the lambda:CreateFunction action".

Actual (emulator): TimeoutError: timed out after 55s waiting until CreateFunction with the matching SecurityGroupIds is allowed; last=None — every attempt returned one of AccessDenied, AccessDeniedException, InvalidParameterValueException, UnrecognizedClientException, so the matching request was never authorized and the graded denial assertion was never reached.

## Steps to Reproduce
1. CreateVpc + one subnet; create two security groups in it, <sg-allowed> and <sg-other>.
2. Create a Lambda execution role (AWSLambdaVPCAccessExecutionRole-equivalent policies attached).
3. Create a caller role trusting the current principal, with the inline policy: Allow iam:PassRole on the execution role; Allow ec2:DescribeSecurityGroups, ec2:DescribeSubnets, ec2:DescribeVpcs, ec2:GetSecurityGroupsForVpc on *; Allow lambda:CreateFunction and lambda:UpdateFunctionConfiguration on * with Condition {"ForAllValues:StringEquals": {"lambda:SecurityGroupIds": ["<sg-allowed>"]}}.
4. sts AssumeRole into the caller role and, with those credentials, call lambda CreateFunction(Runtime=python3.12, Role=<exec role>, Code={ZipFile:...}, VpcConfig={SubnetIds:[<subnet>], SecurityGroupIds:["<sg-allowed>"]}).
5. Expected: the call succeeds (201) and GetFunctionConfiguration reports VpcConfig.SecurityGroupIds == ["<sg-allowed>"]. Observed: the call is refused on every retry for ~55 s with AccessDenied/AccessDeniedException/InvalidParameterValueException/UnrecognizedClientException and never succeeds.
6. For contrast, the same call with SecurityGroupIds=["<sg-other>"] must be the only one refused, with AccessDeniedException and AWS's "...no identity-based policy allows the lambda:CreateFunction action" message.
