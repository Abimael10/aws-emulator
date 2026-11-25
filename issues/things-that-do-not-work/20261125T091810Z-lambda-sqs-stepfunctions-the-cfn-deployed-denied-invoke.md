Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__sqs__stepfunctions.deploy-missing-lambda-invoke-permission-sfn-template-negative
Issue-Key: lambda__sqs__stepfunctions.deploy-missing-lambda-invoke-permission-sfn-template-negative|run#15|AssertionError|assert "AccessDenied" in denied_exec.get("cause", "") or "AccessDenied" in denied_exec.get(
Issue-Commit: 87ad42ce826020c3c25d7c413e65e0908692b03d

# lambda__sqs__stepfunctions: the CFN-deployed denied-invoke Task's failure cause omits AccessDeniedException — the SDK suffix is the comma form instead of AWS's '(Service: AWSLambda; Status Code: 403; Error Code: AccessDeniedException; Request ID: ...; Proxy: null)'

> _Note: this issue was identified against the code base at commit `87ad42ce826020c3c25d7c413e65e0908692b03d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
Cause 1 of 1: for a CloudFormation-deployed state machine whose SfnRole holds no lambda:InvokeFunction, the Lambda Task fails as it should, but the recorded cause carries no AccessDeniedException error code because its parenthesised SDK suffix is rendered in the wrong format. Failing check: lambda__sqs__stepfunctions.deploy-missing-lambda-invoke-permission-sfn-template-negative.

AWS: recorded behavior only — Lambda's Smithy model declares no AccessDeniedException on Invoke (Smithy: Invoke's modeled errors are InvalidParameterValueException, ResourceNotFoundException, TooManyRequestsException, the EFS*/KMS*/EC2* families, etc.; IAM denials are not modeled), so the recorded wire text is the requirement. The execution fails with error `Lambda.AWSLambdaException` and cause `User: <sts assumed-role ARN> is not authorized to perform: lambda:InvokeFunction on resource: <function ARN> because no identity-based policy allows the lambda:InvokeFunction action (Service: AWSLambda; Status Code: 403; Error Code: AccessDeniedException; Request ID: <request id>; Proxy: null)`. Fixed text: the IAM sentence template and the semicolon-separated suffix with literal `Service: AWSLambda`, `Status Code: 403`, `Error Code: AccessDeniedException`, `Proxy: null`; variable parts from the request and state: the assumed-role ARN (account, role name, per-call session name), the function ARN, the request id. The denial is therefore always identifiable as `AccessDenied...` from the error or cause.

Emulator: the cause closes with `(Service: Lambda, Status Code: 403, Request ID: <uuid>) (SDK Attempt Count: 1)` — comma-separated, `Lambda` for `AWSLambda`, no `Error Code: AccessDeniedException`, no `Proxy: null`, and an extra attempt-count clause. The check's assertion that `AccessDenied` appears in the failed execution's error or cause fails.

Note: open issue 20261109T210043Z-cloudformation-lambda-stepfunctions-a-step-functions-lambda-task-denied-lambda-invokefunction.md states the inverse (comma form as AWS's); the recordings replayed here show the semicolon form, so that issue does not cover this failure and the two should be reconciled in favour of the recorded text.

Why: the deployment-time negative control exists so that a missing IAM grant is reported as an authorization failure; a client reading the execution's error/cause cannot find any AccessDenied signal and sees a generic Lambda service exception, so a least-privilege regression is indistinguishable from a transient Lambda fault and may be retried instead of surfaced.

Fix: the Step Functions optimized lambda:invoke integration's error serializer — the component that turns a Lambda refusal into the execution's error/cause (DescribeExecution and the TaskFailed/ExecutionFailed history event details). It must emit AWS's semicolon-separated suffix carrying `Service: AWSLambda`, the refusal's status, its error code, the request id and `Proxy: null`, with no attempt-count clause.

Traces: lambda__sqs__stepfunctions.deploy-missing-lambda-invoke-permission-sfn-template-negative.

## Evidence
Check lambda__sqs__stepfunctions.deploy-missing-lambda-invoke-permission-sfn-template-negative (assert at line 146: `assert "AccessDenied" in denied_exec.get("cause", "") or "AccessDenied" in denied_exec.get(...)`):
AssertionError: expected an AccessDenied failure, got error='Lambda.AWSLambdaException' cause='User: arn:aws:sts::000000000000:assumed-role/sim2real-xsvc-stk-32ce3961dbcb-SfnRole-C976DA3D04C8/c419c5083c8a46b08783eed63722bcc8 is not authorized to perform: lambda:InvokeFunction on resource: arn:aws:lambda:us-west-2:000000000000:function:sim2real-xsvc-stk-32ce3961dbcb-EchoFn-D10388527D54 because no identity-based policy allows the lambda:InvokeFunction action (Service: Lambda, Status Code: 403, Request ID: 6538c2a4-9f4c-4e45-976e-eaaf06bfa1ff) (SDK Attempt Count: 1)'

Expected the same sentence closed with '(Service: AWSLambda; Status Code: 403; Error Code: AccessDeniedException; Request ID: <id>; Proxy: null)'. Stack-generated role/function names, the session name and the request id are per-run values; the divergence is the suffix format, the `Lambda` vs `AWSLambda` service name, the missing `Error Code: AccessDeniedException` and `Proxy: null` tokens and the extra `(SDK Attempt Count: 1)`.

## Steps to Reproduce
1. Deploy a CloudFormation stack with: an EchoFn (AWS::Lambda::Function), an SfnRole (AWS::IAM::Role trusting states.amazonaws.com, with NO lambda:InvokeFunction policy), and an AWS::StepFunctions::StateMachine whose single Task uses Resource arn:aws:states:::lambda:invoke with FunctionName = ${EchoFn}.
2. StartExecution on the stack's StateMachineArn and wait for FAILED.
3. Read DescribeExecution's error and cause: AWS's cause contains 'Error Code: AccessDeniedException' in a semicolon-separated '(Service: AWSLambda; ...; Proxy: null)' suffix; the emulator's contains 'Service: Lambda, Status Code: 403, Request ID: ...' and no error code.
