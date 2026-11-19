Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__iam__lambda__s3.cfn-iam-role-permissions-boundary-in-template-positive cloudformation__iam__lambda__s3.lambda-sdk-cfn-create-stack-missing-passrole-negative cloudformation__iam__lambda__s3.s3-notification-lambda-role-missing-put-tagging-negative
Issue-Key: cloudformation__iam__lambda__s3.cfn-iam-role-permissions-boundary-in-template-positive|run#14|AssertionError|assert "AccessDenied" in denied["error"], (
Issue-Key: cloudformation__iam__lambda__s3.lambda-sdk-cfn-create-stack-missing-passrole-negative|run#12|AssertionError|assert result.get("ok") is False, f"create_stack unexpectedly succeeded: {result}"
Issue-Key: cloudformation__iam__lambda__s3.s3-notification-lambda-role-missing-put-tagging-negative|run#17|AssertionError|assert status_body == want_status, (
Issue-Commit: 93aea946e1be9d371a5c94c0146ea55c9b1afa4f

# cloudformation__iam__lambda__s3: 3 net-new causes — the Lambda runtime SDK has no `iam` service, CreateStack never authorizes iam:PassRole on RoleARN, and S3 PutObjectTagging is served without identity-policy authorization

> _Note: this issue was identified against the code base at commit `93aea946e1be9d371a5c94c0146ea55c9b1afa4f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Three net-new root causes, all in the authorization/execution surface these checks exercise from inside a Lambda function. None is marked owned by an open issue.

Cause 1 of 3: the emulated Lambda execution environment's SDK does not expose the `iam` service, so a function cannot call IAM at all.
AWS: recorded behavior only. A Lambda function's bundled AWS SDK can construct a client for every AWS service; a function whose role allows iam:CreateUser but carries a PermissionsBoundary of arn:aws:iam::aws:policy/PowerUserAccess reaches IAM and is refused AccessDenied (the boundary excludes IAM write actions), and no user is created; after the boundary is detached the same call succeeds.
Emulator: the function's own SDK raises botocore's UnknownServiceError before any call leaves the function — "Unknown service: 'iam'. Valid service names are: sqs, sns, sts, events, eventbridge, pipes, scheduler, dynamodb, logs, ... transfer" — i.e. the runtime's client registry omits iam (and anything else not in that list). The boundary refusal AWS produces is therefore unreachable, and the function's error is a client-side exception rather than an AWS error response.
Why: any function that administers IAM (or any other service missing from that registry) fails for the wrong reason, with a message no AWS client would ever see, so in-function authorization behaviour cannot be exercised at all.
Fix: the Lambda execution environment the emulator serves — its SDK/service registry and client factory must expose the full modeled service surface (certainly `iam`) and route those calls to the emulator's endpoint for that service.
Traces: cloudformation__iam__lambda__s3.cfn-iam-role-permissions-boundary-in-template-positive.

Cause 2 of 3: CreateStack does not authorize iam:PassRole for the RoleARN it is handed.
AWS: CreateStack with a RoleARN, called by a principal that has no iam:PassRole permission for that role, is refused with HTTP 403 AccessDenied and the message `User: <caller's assumed-role ARN> is not authorized to perform: iam:PassRole on resource: <RoleARN> because no identity-based policy allows the iam:PassRole action` — fixed text `User: ... is not authorized to perform: iam:PassRole on resource: ... because no identity-based policy allows the iam:PassRole action`, with the caller ARN taken from the signing credentials and the resource from the request's RoleARN. No stack is created. Granting iam:PassRole on the same role lets an equivalent CreateStack reach CREATE_COMPLETE (the check's positive control).
Emulator: CreateStack from the Lambda handler whose role lacks iam:PassRole succeeds (the handler reports {'ok': True}) and the stack is created.
Why: the PassRole guard is the control that stops a caller from escalating privilege through a stack's execution role; without it, any principal that may call CreateStack can act as any role in the account, and a negative test of that boundary cannot be written.
Fix: CreateStack's authorization in the CloudFormation handler — evaluate iam:PassRole for the request's RoleARN against the calling principal's policies and refuse with AWS's AccessDenied code and message.
Traces: cloudformation__iam__lambda__s3.lambda-sdk-cfn-create-stack-missing-passrole-negative.

Cause 3 of 3: S3 serves PutObjectTagging for a principal no identity policy authorizes.
AWS: PutObjectTagging by a role whose policies do not allow s3:PutObjectTagging is refused with HTTP 403 AccessDenied and the message `User: arn:aws:sts::<account>:assumed-role/<role-name>/<session-name> is not authorized to perform: s3:PutObjectTagging on resource: "arn:aws:s3:::<bucket>/<key>" because no identity-based policy allows the s3:PutObjectTagging action` — fixed text as quoted, with the assumed-role ARN from the caller's credentials and the quoted resource ARN from the bucket and key in the request; the object's tag set is unchanged. Once the permission is added the same call succeeds.
Emulator: the call is served 200 and the tag is applied — the function reports 'OK' where the refusal string was expected.
Why: S3 data-plane calls are not authorized, so least-privilege roles are not enforced: a client's deny-by-omission is silently an allow, and no negative-path test over S3 tagging is possible.
Fix: the S3 PutObjectTagging handler's authorization path — identity-policy evaluation for S3 data-plane operations, returning AWS's AccessDenied code and message.
Traces: cloudformation__iam__lambda__s3.s3-notification-lambda-role-missing-put-tagging-negative.

## Evidence
Cause 1 — cfn-iam-role-permissions-boundary-in-template-positive:
  expected: the function's iam:CreateUser call is refused and its reported error contains 'AccessDenied' (produced by the role's PermissionsBoundary arn:aws:iam::aws:policy/PowerUserAccess); no user created
  actual:   AssertionError: the refusal must be the specific AccessDenied the boundary produces; got "Unknown service: 'iam'. Valid service names are: sqs, sns, sts, events, eventbridge, pipes, scheduler, dynamodb, logs, cloudwatchlogs, cloudwatch, apigateway, ... transfer"

Cause 2 — lambda-sdk-cfn-create-stack-missing-passrole-negative:
  expected: create_stack(RoleARN=<role the caller cannot pass>) -> AccessDenied with the iam:PassRole message, and the stack does not exist
  actual:   AssertionError: create_stack unexpectedly succeeded: {'ok': True}

Cause 3 — s3-notification-lambda-role-missing-put-tagging-negative:
  expected: 'ERROR:AccessDenied|User: arn:aws:sts::<account>:assumed-role/<role>/<session> is not authorized to perform: s3:PutObjectTagging on resource: "arn:aws:s3:::<bucket>/trigger/negative.txt" because no identity-based policy allows the s3:PutObjectTagging action'
  actual:   'OK' (the tagging call was served and the tag applied)

## Steps to Reproduce
Cause 1: deploy a stack with an AWS::IAM::Role carrying PermissionsBoundary arn:aws:iam::aws:policy/PowerUserAccess plus an identity policy allowing s3 and iam:CreateUser, and an AWS::Lambda::Function using it. Invoke the function so it builds an `iam` client and calls CreateUser: the emulator's runtime raises "Unknown service: 'iam'"; AWS returns AccessDenied from the boundary.

Cause 2: create role R (assumable by the stack) and a Lambda whose role may call cloudformation:CreateStack but has NO iam:PassRole on R. From the function call CreateStack(RoleARN=R's ARN): the emulator succeeds; AWS refuses 403 AccessDenied with the iam:PassRole message and creates no stack. Then grant iam:PassRole on R and repeat for the positive control.

Cause 3: create a bucket with an ObjectCreated notification to a Lambda whose role may read the object but lacks s3:PutObjectTagging. Put an object under trigger/: the function's PutObjectTagging is served 200 by the emulator; AWS refuses 403 AccessDenied with the quoted message and leaves the object untagged.
