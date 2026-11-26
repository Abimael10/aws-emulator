Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: stepfunctions.sqs-send-message-task-without-permission-is-denied
Issue-Shape-Id: com.amazonaws.sfn#AWSStepFunctions
Issue-Key: stepfunctions.sqs-send-message-task-without-permission-is-denied|run#8|AssertionError|assert desc["status"] == "FAILED", (
Issue-Commit: 7f8f759d74cb6fc509a9ff7e87e7c97fb9625f4c

# stepfunctions: an sqs:sendMessage Task state runs under an execution role that lacks sqs:SendMessage and the execution SUCCEEDS instead of failing SQS.AmazonSQSException

> _Note: this issue was identified against the code base at commit `7f8f759d74cb6fc509a9ff7e87e7c97fb9625f4c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartExecution

## Description
One net-new root cause: the optimized SQS integration of a Task state does not authorize the call against the execution role, so a denied role behaves exactly like a permitted one.

AWS: a Task state whose Resource is arn:aws:states:::sqs:sendMessage performs SendMessage under the state machine's execution role. When that role holds no sqs:SendMessage grant for the queue, SQS answers 403 AccessDenied and Step Functions surfaces it as a task failure: the execution reaches status FAILED with error 'SQS.AmazonSQSException' and a cause that is SQS's own AccessDenied message wrapped in Step Functions' service suffix — the fixed template "User: <assumed-role principal ARN> is not authorized to perform: sqs:sendmessage on resource: <queue ARN> because no identity-based policy allows the sqs:sendmessage action (Service: AmazonSQS; Status Code: 403; Error Code: AccessDenied; Request ID: <id>; Proxy: null)". Its fixed text is the whole sentence skeleton including the parenthesised 'Service: AmazonSQS; Status Code: 403; Error Code: AccessDenied' tail; its variable parts are the assumed-role principal ARN (arn:aws:sts::<account>:assumed-role/<role name>/<session>), the queue ARN, and the per-request id — all taken from the request and the emulator's own state, never from a recording. Nothing is delivered to the queue. A sibling state machine whose role DOES hold the grant succeeds and delivers.
Emulator: the execution of the DENIED state machine reaches status SUCCEEDED, with no error and no cause — the authorization check is simply absent, so the task is carried out regardless of the role's policy.
Why: this inverts a security-relevant outcome. A caller using the emulator to verify least-privilege (that a workflow cannot write to a queue it was not granted) gets a green result for a role AWS refuses, so the misconfiguration ships; and a client whose Catch branch handles SQS.AmazonSQSException is never exercised because the error never occurs.
Fix: the sqs:sendMessage optimized-integration path of the Task state in the states crate — evaluate the execution role's identity-based policy for sqs:SendMessage on the target queue before dispatching, and on denial fail the task with error 'SQS.AmazonSQSException' and the AccessDenied cause above instead of performing the send.
Traces: stepfunctions.sqs-send-message-task-without-permission-is-denied.

Adjacent OPEN issues that do NOT cover this check (none carries its replay key; they are filed against other service headings/scenarios): 20261117T142140Z-sqs-stepfunctions-a-cross-region-sqs-sendmessage-task.md (cross-region queue) and 20261117T134237Z-lambda-sqs-stepfunctions-2-causes-a-step-functions-task-under-a.md. A similar open issue does not cover a failing check, so this is filed here and those are named.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout: 761/955 passed; `stepfunctions` 441/546. The check is listed under '### stepfunctions' and is marked neither ALREADY FILED nor OWNED.

stepfunctions.sqs-send-message-task-without-permission-is-denied:
  AssertionError: execution ended 'SUCCEEDED', expected FAILED: None

Expected (AWS): DescribeExecution status FAILED, error 'SQS.AmazonSQSException', cause (request id masked by the check) =
  "User: arn:aws:sts::<account>:assumed-role/<denied role name>/<session> is not authorized to perform: sqs:sendmessage on resource: <queue ARN> because no identity-based policy allows the sqs:sendmessage action (Service: AmazonSQS; Status Code: 403; Error Code: AccessDenied; Request ID: <id>; Proxy: null)"
  and the queue receives no message, while the sibling state machine whose role carries {"Effect":"Allow","Action":"sqs:SendMessage","Resource":"<queue ARN>"} SUCCEEDS and does deliver.
Actual (emulator): status SUCCEEDED, error/cause absent.

Fixed vs. variable parts of the message: fixed — "User: ", " is not authorized to perform: sqs:sendmessage on resource: ", " because no identity-based policy allows the sqs:sendmessage action (Service: AmazonSQS; Status Code: 403; Error Code: AccessDenied; Request ID: ", "; Proxy: null)". Variable — the assumed-role principal ARN (account and role name from the request/state, session name per assume), the queue ARN from the Task's Parameters, and the per-request Request ID.

## Steps to Reproduce
Boot the emulator from this checkout and replay stepfunctions.sqs-send-message-task-without-permission-is-denied. Manually: create an SQS queue; create two roles trusted by states.amazonaws.com, one with NO inline policy statements and one allowing sqs:SendMessage on that queue's ARN; create one STANDARD state machine per role whose single Task state has Resource "arn:aws:states:::sqs:sendMessage" and Parameters naming the queue URL and a message body. StartExecution on the denied one and poll DescribeExecution to a terminal status: expect FAILED with error 'SQS.AmazonSQSException' and the AccessDenied cause, and expect ReceiveMessage on the queue to return nothing. StartExecution on the permitted one: expect SUCCEEDED and the message delivered.
