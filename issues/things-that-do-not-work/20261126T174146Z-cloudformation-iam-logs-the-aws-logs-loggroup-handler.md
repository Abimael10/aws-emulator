Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__iam__logs.logs-loggroup-handler-permission-role-deploys-then-denies-without-create-permission cloudformation__iam__logs.logs-loggroup-handler-permission-update-denied-then-delete-denied-recovers
Issue-Key: cloudformation__iam__logs.logs-loggroup-handler-permission-role-deploys-then-denies-without-create-permission|run#25|AssertionError|assert re.fullmatch(pattern, failed[0]), f"the log group failed with {failed[0]!r}"
Issue-Key: cloudformation__iam__logs.logs-loggroup-handler-permission-update-denied-then-delete-denied-recovers|run#45|AssertionError|assert _mask(reason) == expected_reason, f"the log group update failed with {reason!r}"
Issue-Commit: 5183cad56763ee54d3f0a63f14aa900b48a924f9

# cloudformation__iam__logs: the AWS::Logs::LogGroup handler's AccessDenied ResourceStatusReason is malformed — it drops the SDK exception suffix and appends ':*' to the log-group ARN

> _Note: this issue was identified against the code base at commit `5183cad56763ee54d3f0a63f14aa900b48a924f9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackEvents

## Description
Two net-new causes, both in the text CloudFormation records as the StackEvent ResourceStatusReason when the AWS::Logs::LogGroup resource handler is denied one of its declared handler permissions by the stack's service role (RoleARN). The stack status, the resource status and the HandlerErrorCode are already right (CREATE_FAILED / UPDATE_FAILED, HandlerErrorCode: AccessDenied); only the reason string diverges, and it diverges the same way on the CREATE path (logs:CreateLogGroup denied) and the UPDATE path (logs:PutRetentionPolicy denied).

Cause 1 of 2: the quoted handler message omits the trailing SDK-exception detail. Failing checks: cloudformation__iam__logs.logs-loggroup-handler-permission-role-deploys-then-denies-without-create-permission, cloudformation__iam__logs.logs-loggroup-handler-permission-update-denied-then-delete-denied-recovers.
AWS: recorded behavior only — Smithy (com.amazonaws.cloudformation) models StackEvent.ResourceStatusReason only as "Success/failure message associated with the resource" (shape ResourceStatusReason, no pattern), so the wording is not in the model. The recording shows CloudFormation wrapping the AccessDeniedException its handler got from CloudWatch Logs, with the AWS SDK's own exception detail still attached inside the quoted message, in this fixed form: Resource handler returned message: "<AccessDenied message> (Service: CloudWatchLogs, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: <n>)" (RequestToken: <uuid>, HandlerErrorCode: AccessDenied). Fixed text: `Resource handler returned message: "`, `(Service: CloudWatchLogs, Status Code: 400, Request ID: `, `) (SDK Attempt Count: `, `)" (RequestToken: `, `, HandlerErrorCode: AccessDenied)`. Variable parts taken from the call and from state: the request id and the request token (per-run uuids), the attempt count (an integer), and the inner AccessDenied message's own variables (see cause 2).
Emulator: the inner message ends right after the denied action and the SDK detail is absent — e.g. `... because no identity-based policy allows the logs:CreateLogGroup action" (RequestToken: <uuid>, HandlerErrorCode: AccessDenied)` — with no `(Service: CloudWatchLogs, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: <n>)` segment inside the quotes. Same on the UPDATE path for logs:PutRetentionPolicy.
Why: operators and deploy tooling parse this reason to learn which service refused, with what HTTP status, and under which request id, so the failure can be correlated with the CloudTrail/service-side record. A client that scrapes `Status Code: 400` or the `Request ID` out of a CREATE_FAILED/UPDATE_FAILED event gets nothing and cannot attribute the denial, and a regression test pinned to AWS's reason text fails against a stack that otherwise behaved correctly.
Fix: the resource-handler error-to-ResourceStatusReason formatter on the CloudFormation side (the component that renders a handler's downstream service exception into the StackEvent reason), not the logs service's own error serializer.

Cause 2 of 2: the resource ARN named in the AccessDenied message carries a spurious `:*` suffix. Failing checks: cloudformation__iam__logs.logs-loggroup-handler-permission-role-deploys-then-denies-without-create-permission, cloudformation__iam__logs.logs-loggroup-handler-permission-update-denied-then-delete-denied-recovers.
AWS: recorded behavior only — the IAM-style denial message names the log group's own ARN with no wildcard child suffix: `User: arn:aws:sts::<account>:assumed-role/<role-name>/<session-name> is not authorized to perform: <action> on resource: arn:aws:logs:<region>:<account>:log-group:<logGroupName> because no identity-based policy allows the <action> action`. Fixed text: `User: `, ` is not authorized to perform: `, ` on resource: `, ` because no identity-based policy allows the `, ` action`. Variable parts from the request and state: the assumed-role ARN of the stack's RoleARN plus the session name CloudFormation assumed with, the IAM action the handler called, the account, the region and the LogGroupName from the template.
Emulator: it appends `:*` to the log-group ARN — `on resource: arn:aws:logs:<region>:<account>:log-group:<logGroupName>:*` — so the resource the message names is the log-group's log-stream wildcard rather than the log group the handler acted on.
Why: the resource ARN in a denial message is what an operator copies into the policy that has to be widened. With `:*` appended it names a different resource than the one the handler was refused on, so a policy written from the message does not grant the denied call, and any client matching the message against the resource it asked for mismatches.
Fix: the authorization-denial message builder for the CloudWatch Logs operations the AWS::Logs::LogGroup handler calls (CreateLogGroup, PutRetentionPolicy, DeleteLogGroup), where the resource ARN of the denied call is rendered.

Traces: both failing checks above; each asserts the full reason string of the one CREATE_FAILED (resp. UPDATE_FAILED) event for the `Group` logical resource.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the booted emulator, service heading `cloudformation__iam__logs`.

[mismatch] cloudformation__iam__logs.logs-loggroup-handler-permission-role-deploys-then-denies-without-create-permission
  AssertionError: the log group failed with 'Resource handler returned message: "User: arn:aws:sts::<account>:assumed-role/<role>/AWSCloudFormation is not authorized to perform: logs:CreateLogGroup on resource: arn:aws:logs:<region>:<account>:log-group:/<group>:* because no identity-based policy allows the logs:CreateLogGroup action" (RequestToken: <uuid>, HandlerErrorCode: AccessDenied)'
  Expected (AWS): 'Resource handler returned message: "User: arn:aws:sts::<account>:assumed-role/<role>/<session> is not authorized to perform: logs:CreateLogGroup on resource: arn:aws:logs:<region>:<account>:log-group:/<group> because no identity-based policy allows the logs:CreateLogGroup action (Service: CloudWatchLogs, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: <n>)" (RequestToken: <uuid>, HandlerErrorCode: AccessDenied)'

[mismatch] cloudformation__iam__logs.logs-loggroup-handler-permission-update-denied-then-delete-denied-recovers
  AssertionError: the log group update failed with 'Resource handler returned message: "User: arn:aws:sts::<account>:assumed-role/<role>/AWSCloudFormation is not authorized to perform: logs:PutRetentionPolicy on resource: arn:aws:logs:<region>:<account>:log-group:/<group>:* because no identity-based policy allows the logs:PutRetentionPolicy action" (RequestToken: <uuid>, HandlerErrorCode: AccessDenied)'
  Expected (AWS): the same string with the log-group ARN unsuffixed and with '(Service: CloudWatchLogs, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)' inside the quotes.

The account id, region, role name, generated log-group name, session name, request ids and request tokens shown above are per-run values; the requirement is the fixed text and the placement of the request's own values. Both checks' positive controls passed (a role holding exactly AWS::Logs::LogGroup's declared handler permissions creates, updates in place and deletes the group), so the divergence is confined to the denial message.

Smithy consulted: com.amazonaws.cloudformation StackEvent members ResourceStatusReason / ResourceStatus / LogicalResourceId (no wording constraint on the reason), which is why the message requirement is stated as recorded behavior.

## Steps to Reproduce
1. Clone the repo and replay the behavioral suite; both ids in trace_ids reproduce.
2. Create an IAM role trusted by cloudformation.amazonaws.com with an inline policy granting exactly AWS::Logs::LogGroup's declared CREATE+UPDATE+DELETE handler permissions, and a second identical role minus logs:CreateLogGroup (for the UPDATE case, minus logs:PutRetentionPolicy).
3. CreateStack with DisableRollback=true, RoleARN set to the deficient role, and a template whose only resource is {"Type": "AWS::Logs::LogGroup", "Properties": {"LogGroupName": "/<name>"}}.
4. Wait for CREATE_FAILED and read DescribeStackEvents for the `Group` logical resource's CREATE_FAILED event.
5. Expected ResourceStatusReason: Resource handler returned message: "User: arn:aws:sts::<account>:assumed-role/<role>/<session> is not authorized to perform: logs:CreateLogGroup on resource: arn:aws:logs:<region>:<account>:log-group:/<name> because no identity-based policy allows the logs:CreateLogGroup action (Service: CloudWatchLogs, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: <n>)" (RequestToken: <uuid>, HandlerErrorCode: AccessDenied). Actual: the log-group ARN has `:*` appended and the `(Service: ...) (SDK Attempt Count: ...)` segment is missing.
6. Repeat with the role missing logs:PutRetentionPolicy: create the group with retention unset, then UpdateStack adding RetentionInDays=1, and read the `Group` UPDATE_FAILED event's reason — same two divergences for logs:PutRetentionPolicy.
7. Re-replay both ids in trace_ids to confirm the fix.
