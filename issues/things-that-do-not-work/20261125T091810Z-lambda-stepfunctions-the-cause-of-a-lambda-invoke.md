Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__stepfunctions.lambda-task-credentials-rolearn-overrides-the-execution-role lambda__stepfunctions.task-credentials-role-without-invoke-permission-denied
Issue-Key: lambda__stepfunctions.lambda-task-credentials-rolearn-overrides-the-execution-role|run#19|AssertionError|assert re.fullmatch(cause_pattern, cause), f"cause did not match the expected shape: {cause!r}"
Issue-Key: lambda__stepfunctions.task-credentials-role-without-invoke-permission-denied|run#24|AssertionError|assert masked == (
Issue-Commit: 87ad42ce826020c3c25d7c413e65e0908692b03d

# lambda__stepfunctions: the cause of a lambda:invoke Task denied lambda:InvokeFunction carries an SDK-v2 comma trailer instead of AWS's '(Service: AWSLambda; Status Code: 403; Error Code: AccessDeniedException; Request ID: ...; Proxy: null)'

> _Note: this issue was identified against the code base at commit `87ad42ce826020c3c25d7c413e65e0908692b03d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
Cause 1 of 1: when a Step Functions Task using the optimized `arn:aws:states:::lambda:invoke` integration is denied lambda:InvokeFunction (whether the denied identity is the execution role or the role named by the Task's Credentials.RoleArn), the execution's failure `cause` is rendered with the wrong SDK suffix: the service name, the separator style, the `Error Code` clause and the `Proxy` clause all diverge, and an extra attempt-count clause is appended. Failing checks: lambda__stepfunctions.lambda-task-credentials-rolearn-overrides-the-execution-role, lambda__stepfunctions.task-credentials-role-without-invoke-permission-denied.

AWS: recorded behavior only — the Lambda Smithy model declares no AccessDeniedException on Invoke (Smithy: Invoke's modeled errors are InvalidParameterValueException, ResourceNotFoundException, TooManyRequestsException, EFS*/KMS*/EC2* etc.; an IAM authorization denial is not modeled), so the requirement is the recorded wire text. The execution fails with error `Lambda.AWSLambdaException` (both checks assert exactly this and the emulator matches) and a cause whose fixed text is:
  `User: <sts assumed-role ARN of the identity that performed the invoke> is not authorized to perform: lambda:InvokeFunction on resource: <function ARN> because no identity-based policy allows the lambda:InvokeFunction action (Service: AWSLambda; Status Code: 403; Error Code: AccessDeniedException; Request ID: <request id>; Proxy: null)`
Fixed text: the IAM sentence template (`User: ... is not authorized to perform: <action> on resource: <resource> because no identity-based policy allows the <action> action`) and the semicolon-separated parenthesised suffix with literal tokens `Service: AWSLambda`, `Status Code: 403`, `Error Code: AccessDeniedException` and `Proxy: null`. Variable parts taken from the request/state: the assumed-role ARN (account, role name, and the per-call session name), the function ARN, and the per-call request id. No `(SDK Attempt Count: n)` clause follows the suffix.

Emulator: the IAM sentence is right but the suffix is `(Service: Lambda, Status Code: 403, Request ID: <uuid>) (SDK Attempt Count: 1)` — comma-separated instead of semicolon-separated, service name `Lambda` instead of `AWSLambda`, no `Error Code: AccessDeniedException` token, no `Proxy: null` token, and a trailing ` (SDK Attempt Count: 1)` clause AWS does not emit. Replay output for lambda-task-credentials-rolearn-overrides-the-execution-role: cause did not match the expected shape: 'User: arn:aws:sts::<account>:assumed-role/<exec-role>/<session> is not authorized to perform: lambda:InvokeFunction on resource: <fn arn> because no identity-based policy allows the lambda:InvokeFunction action (Service: Lambda, Status Code: 403, Request ID: <uuid>) (SDK Attempt Count: 1)'. Same suffix in task-credentials-role-without-invoke-permission-denied, where the denied identity is correctly the assumed task-role session.

Note: open issue 20261109T210043Z-cloudformation-lambda-stepfunctions-a-step-functions-lambda-task-denied-lambda-invokefunction.md asserts the OPPOSITE — it names the comma-separated `(Service: Lambda, Status Code: 403, Request ID: ...) (SDK Attempt Count: n)` form as AWS's and the semicolon form as the defect. The recordings replayed here show the semicolon/`AWSLambda`/`Error Code`/`Proxy: null` form is what AWS serves for this denial, so that issue does not cover this one and the two must be reconciled in favour of the recorded text.

Why: a client that branches on the denial — matching `AccessDeniedException` or the 403 authorization class in the cause before retrying, alarming or reporting the missing grant — finds no error-code token at all in the cause and a service name (`Lambda`) that never appears in AWS causes, so the denial is misclassified as an unspecified Lambda service fault. Log pipelines and regex-based runbooks keyed on AWS's suffix never match.

Fix: the Step Functions optimized lambda:invoke integration's error serializer — the component that wraps a Lambda refusal into the execution's error/cause (and thus into the ExecutionFailed / TaskFailed history event details). It must render AWS's semicolon-separated suffix with `Service: AWSLambda`, the refusal's status code, `Error Code: <the refusal's error code>`, the request id and `Proxy: null`, and must not append an attempt-count clause.

Traces: lambda__stepfunctions.lambda-task-credentials-rolearn-overrides-the-execution-role (Credentials.RoleArn positive plus the no-Credentials negative control), lambda__stepfunctions.task-credentials-role-without-invoke-permission-denied (denied task role).

## Evidence
Check lambda__stepfunctions.lambda-task-credentials-rolearn-overrides-the-execution-role (assert at line 109):
Expected (regex, per-call session name and request id masked):
  ^User: arn:aws:sts::<account>:assumed-role/<exec-role>/[A-Za-z0-9]+ is not authorized to perform: lambda:InvokeFunction on resource: <fn arn> because no identity-based policy allows the lambda:InvokeFunction action \(Service: AWSLambda; Status Code: 403; Error Code: AccessDeniedException; Request ID: <uuid>; Proxy: null\)$
Actual:
  'User: arn:aws:sts::000000000000:assumed-role/sim2real-role-dd724e2c6d8f/50747a544d6f46cfae4f83923c702573 is not authorized to perform: lambda:InvokeFunction on resource: arn:aws:lambda:us-west-2:000000000000:function:sim2real-fn-ff9c83cdf16c because no identity-based policy allows the lambda:InvokeFunction action (Service: Lambda, Status Code: 403, Request ID: cb49f7f4-d0bc-4fb2-928d-30eab8d599e9) (SDK Attempt Count: 1)'

Check lambda__stepfunctions.task-credentials-role-without-invoke-permission-denied (assert at line 233, session name and request id masked before comparison):
Expected:
  'User: arn:aws:sts::<account>:assumed-role/<denied-task-role>/<session> is not authorized to perform: lambda:InvokeFunction on resource: <fn arn> because no identity-based policy allows the lambda:InvokeFunction action (Service: AWSLambda; Status Code: 403; Error Code: AccessDeniedException; Request ID: <request-id>; Proxy: null)'
Actual:
  '... because no identity-based policy allows the lambda:InvokeFunction action (Service: Lambda, Status Code: 403, Request ID: c3e65943-ce1c-4cf9-a6d6-10e099607a68) (SDK Attempt Count: 1)'

The account id, role/function names, session names and request ids above are per-run values; the divergence is the suffix format: separator (`,` vs `;`), service name (`Lambda` vs `AWSLambda`), the missing `Error Code: AccessDeniedException` and `Proxy: null` tokens, and the extra `(SDK Attempt Count: 1)` clause. Status (403) and error name (Lambda.AWSLambdaException) already match.

## Steps to Reproduce
1. Create a Lambda function that returns {'ok': True}.
2. Create an execution role trusting states.amazonaws.com with NO lambda:InvokeFunction grant (optionally a second task role trusted by it, also without the grant, and grant the execution role sts:AssumeRole on it).
3. Create a state machine with one Task, Resource arn:aws:states:::lambda:invoke, Parameters {FunctionName: <fn arn>, Payload: {}} — once without Credentials, once with Credentials.RoleArn set to the ungranted task role.
4. StartExecution and wait for the execution to reach FAILED.
5. Read error/cause from DescribeExecution (or the ExecutionFailed/TaskFailed event in GetExecutionHistory) and compare the parenthesised SDK suffix with AWS's '(Service: AWSLambda; Status Code: 403; Error Code: AccessDeniedException; Request ID: <id>; Proxy: null)'.
