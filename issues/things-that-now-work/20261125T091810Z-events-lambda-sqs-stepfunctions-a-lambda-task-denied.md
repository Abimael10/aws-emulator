Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events__lambda__sqs__stepfunctions.sfn-task-lambda-fails-without-invoke-permission
Issue-Key: events__lambda__sqs__stepfunctions.sfn-task-lambda-fails-without-invoke-permission|run#11|AssertionError|assert "AccessDenied" in blob, f"expected an access-denied cause, got: {blob[:600]}"
Issue-Commit: 87ad42ce826020c3c25d7c413e65e0908692b03d

# events__lambda__sqs__stepfunctions: a Lambda Task denied lambda:InvokeFunction produces a cause with no AccessDeniedException error code — the SDK suffix is the comma form, not AWS's '(Service: AWSLambda; ...; Error Code: AccessDeniedException; ...; Proxy: null)'

> _Note: this issue was identified against the code base at commit `87ad42ce826020c3c25d7c413e65e0908692b03d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
Cause 1 of 1: a state machine whose role lacks lambda:InvokeFunction does fail its `arn:aws:states:::lambda:invoke` Task, but the failure cause it records carries no AccessDeniedException error code, because the parenthesised SDK suffix is rendered in the wrong format. Failing check: events__lambda__sqs__stepfunctions.sfn-task-lambda-fails-without-invoke-permission.

AWS: recorded behavior only — Lambda's Smithy model declares no AccessDeniedException on Invoke (Smithy: Invoke's modeled errors cover InvalidParameterValueException, ResourceNotFoundException, TooManyRequestsException, the EFS*/KMS*/EC2* families and others; an IAM authorization denial is not modeled), so the recorded wire text is the requirement. The execution fails with error `Lambda.AWSLambdaException` and an ExecutionFailed cause whose fixed text is `User: <sts assumed-role ARN> is not authorized to perform: lambda:InvokeFunction on resource: <function ARN> because no identity-based policy allows the lambda:InvokeFunction action (Service: AWSLambda; Status Code: 403; Error Code: AccessDeniedException; Request ID: <request id>; Proxy: null)`. Fixed text: that IAM sentence template plus the semicolon-separated suffix with the literal tokens `Service: AWSLambda`, `Status Code: 403`, `Error Code: AccessDeniedException` and `Proxy: null`; variable parts from request/state are the assumed-role ARN (account, role name, per-call session name), the function ARN and the request id. So the string `AccessDenied` is always present in the execution history for this denial.

Emulator: the cause ends `(Service: Lambda, Status Code: 403, Request ID: <uuid>) (SDK Attempt Count: 1)` — comma-separated, service name `Lambda` instead of `AWSLambda`, no `Error Code: AccessDeniedException` clause, no `Proxy: null`, plus a trailing `(SDK Attempt Count: 1)` AWS does not emit. The check's assertion that `AccessDenied` appears somewhere in the history/description blob therefore fails, even though the status, the error name and the IAM sentence are right.

Note: open issue 20261109T210043Z-cloudformation-lambda-stepfunctions-a-step-functions-lambda-task-denied-lambda-invokefunction.md claims the comma form IS AWS's and the semicolon form is the defect; the recordings replayed here contradict it, so it does not cover this failure.

Why: a client or operator classifying the failure looks for the denial's error code (`AccessDeniedException`) in the execution history to distinguish a permissions problem from a Lambda service fault; with the code absent, the only signal left is `Lambda.AWSLambdaException`, which reads as a generic Lambda service exception, so least-privilege regressions are not reported as authorization failures.

Fix: the Step Functions optimized lambda:invoke integration's error serializer — the component that maps a Lambda refusal into the task/execution error and cause written into the ExecutionFailed/TaskFailed history event. Render AWS's semicolon-separated suffix with `Service: AWSLambda`, the refusal's status, `Error Code: <refusal's error code>`, the request id and `Proxy: null`, and drop the attempt-count clause.

Traces: events__lambda__sqs__stepfunctions.sfn-task-lambda-fails-without-invoke-permission.

## Evidence
Check events__lambda__sqs__stepfunctions.sfn-task-lambda-fails-without-invoke-permission (assert at line 106: `assert "AccessDenied" in blob`):
AssertionError: expected an access-denied cause, got: [{"timestamp": ..., "type": "ExecutionFailed", "id": 6, "previousEventId": 5, "executionFailedEventDetails": {"error": "Lambda.AWSLambdaException", "cause": "User: arn:aws:sts::000000000000:assumed-role/sfn-role-noperm83104b0d29b8/e9c6a338a6d7468099acc4610f69d648 is not authorized to perform: lambda:InvokeFunction on resource: arn:aws:lambda:us-west-2:000000000000:function:sim2real-xsvc-fn-7d29372756e2 because no identity-based policy allows the lambda:InvokeFunction action (Service: Lambda, Status Code: 403, Request ID: 8042ee8f-f7f7-4b22-b3be-e4dcd3d73776) (SDK ...

Expected the same IAM sentence closed with AWS's suffix `(Service: AWSLambda; Status Code: 403; Error Code: AccessDeniedException; Request ID: <id>; Proxy: null)`, which is where the `AccessDenied` token the check looks for comes from. Role, function, session and request-id values above are per-run; the divergence is the suffix format and the missing error-code token.

## Steps to Reproduce
1. Create a Lambda function.
2. Create a role trusting states.amazonaws.com with an EMPTY policy set (no lambda:InvokeFunction).
3. Create a state machine with one Task: Resource arn:aws:states:::lambda:invoke, Parameters {FunctionName: <fn arn>, 'Payload.$': '$'}.
4. StartExecution with {"n": 21} and wait for FAILED.
5. GetExecutionHistory and inspect executionFailedEventDetails.cause: AWS's text contains 'Error Code: AccessDeniedException' inside a semicolon-separated '(Service: AWSLambda; ...; Proxy: null)' suffix; the emulator's contains neither.
