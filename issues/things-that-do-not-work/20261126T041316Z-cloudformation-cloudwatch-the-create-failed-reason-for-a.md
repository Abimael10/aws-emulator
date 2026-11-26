Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__cloudwatch.cfn-dashboard-invalid-body-reason-then-delete-removes-dashboard
Issue-Key: cloudformation__cloudwatch.cfn-dashboard-invalid-body-reason-then-delete-removes-dashboard|run#15|AssertionError|assert reason == want_reason, f"CREATE_FAILED reason was {reason!r}, expected {want_reason!r}"
Issue-Commit: 5b5c0bdcf53ebb03a5a8067f7321f26cc20710e1

# cloudformation__cloudwatch: the CREATE_FAILED reason for a failed AWS::CloudWatch::Dashboard renders the wrapped service error in a non-AWS SDK format

> _Note: this issue was identified against the code base at commit `5b5c0bdcf53ebb03a5a8067f7321f26cc20710e1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackEvents

## Description
One net-new root cause, grounded in the failing behavioral check under the suite heading `cloudformation__cloudwatch`.

Cause 1 of 1: when a resource provisioned by CloudFormation is refused by the underlying service, the ResourceStatusReason CloudFormation records on the resource's CREATE_FAILED stack event must wrap the service error in the AWS SDK's exception rendering. The emulator uses a different rendering, so the reason text diverges in four independent ways.

AWS: Smithy (services/service-com-amazonaws-cloudformation-cloudformation) models StackEvent$ResourceStatusReason only as a string — the text itself is recorded behavior only. The recording shows AWS composes it as the fixed template `Resource handler returned message: "<service error message> (Service: <sdkId>, Status Code: <status>, Request ID: <request id>; Proxy: null) (SDK Attempt Count: <n>)" (RequestToken: <client request token>, HandlerErrorCode: <handler error code>)`. Everything outside the angle brackets is fixed text; the variable parts come from the request and from the refusing service's response: the service error message (here CloudWatch's `The field DashboardBody must be a valid JSON object` for an AWS::CloudWatch::Dashboard whose DashboardBody is not a JSON object), the HTTP status of that refusal (400), the service's request id, the stack operation's request token, and the handler error code (`InvalidRequest`). Note the service label is the SDK id `CloudWatch`, the three segments Service/Status Code/Request ID are comma-separated, there is NO `Error Code:` segment, and the quoted block is followed by ` (SDK Attempt Count: <n>)` INSIDE the quotes' closing position as shown.

Emulator: the same failure is reported as `Resource handler returned message: "The field DashboardBody must be a valid JSON object (Service: AmazonCloudWatch; Status Code: 400; Error Code: InvalidParameterInput; Request ID: <rid>; Proxy: null)" (RequestToken: <token>, HandlerErrorCode: InvalidRequest)`. Four divergences from AWS's template: (1) the service label is `AmazonCloudWatch` instead of the SDK id `CloudWatch`; (2) the segments are separated by `; ` instead of `, `; (3) an `Error Code: <code>; ` segment is inserted that AWS does not emit at all; (4) the trailing ` (SDK Attempt Count: <n>)` clause is missing. The resource DID fail and the stack DID roll back, and the request id / request token values themselves are per-run and not part of the requirement — only the template is.

Why: the reason string is the only machine-readable explanation a client gets for a rolled-back resource. CDK, deployment pipelines and operators parse it for the service, the status code and the handler error code; a reason that names a service the SDK never calls `AmazonCloudWatch`, uses `;` where the parser expects `,`, and adds an unexpected `Error Code:` segment makes that parse fail or extract the wrong field, so automated rollback diagnosis diverges from AWS.

Fix: the CloudFormation provisioning path that converts a downstream service refusal into a resource-level failure reason — the component that formats the `Resource handler returned message: ...` wrapper for the CREATE_FAILED/UPDATE_FAILED stack event (the cloudformation crate's resource-handler error rendering), not the CloudWatch PutDashboard validation itself, whose message and status are already correct.

Related already-open/already-fixed issues that do NOT carry this cause: 20261110T165732Z-cloudformation-cloudwatch-logs-a-failed-aws-logs-metricfilter.md covers the same class of reason-text divergence for AWS::Logs::MetricFilter (a different resource type and a different missing clause), and the resolved 20261025T230342Z-cloudwatch-cloudformation-stackstatusreason-for-a-failed-aws-cloudwatch-dashboard.md covered the stack-level StackStatusReason, not the resource event's ResourceStatusReason rendering.

Traces: cloudformation__cloudwatch.cfn-dashboard-invalid-body-reason-then-delete-removes-dashboard.

## Evidence
AssertionError: CREATE_FAILED reason was 'Resource handler returned message: "The field DashboardBody must be a valid JSON object (Service: AmazonCloudWatch; Status Code: 400; Error Code: InvalidParameterInput; Request ID: 22833d0f-2817-6a81-0a2b-213ac0b59edd; Proxy: null)" (RequestToken: 973efdd1-c230-3f3f-9bb2-e0cf40d0828a, HandlerErrorCode: InvalidRequest)', expected 'Resource handler returned message: "The field DashboardBody must be a valid JSON object (Service: CloudWatch, Status Code: 400, Request ID: 22833d0f-2817-6a81-0a2b-213ac0b59edd; Proxy: null) (SDK Attempt Count: 1)" (RequestToken: 973efdd1-c230-3f3f-9bb2-e0cf40d0828a, HandlerErrorCode: InvalidRequest)'

The request id and request token above are per-run values that the harness substitutes; the divergence is entirely in the fixed text: `Service: CloudWatch` vs `Service: AmazonCloudWatch`, `, ` vs `; ` segment separators, the extra `Error Code: InvalidParameterInput; ` segment, and the missing ` (SDK Attempt Count: 1)` clause. The resource status (CREATE_FAILED), the HandlerErrorCode (InvalidRequest) and the wrapped CloudWatch message are already correct, and the subsequent delete removing the dashboard also passed.

## Steps to Reproduce
1. CreateStack with a template containing a single resource `Board` of Type AWS::CloudWatch::Dashboard whose Properties are {DashboardName: <name>, DashboardBody: <a string that is not a JSON object>}.
2. Poll DescribeStacks by stack id until the status is terminal; it ends in a rollback/failed state (correct).
3. Call DescribeStackEvents for the stack and select the event with LogicalResourceId == "Board" and ResourceStatus == "CREATE_FAILED".
4. Compare its ResourceStatusReason to AWS's template: the emulator names the service `AmazonCloudWatch`, separates Service/Status Code/Request ID with `; `, inserts an `Error Code: InvalidParameterInput; ` segment, and omits the trailing ` (SDK Attempt Count: 1)` clause.
