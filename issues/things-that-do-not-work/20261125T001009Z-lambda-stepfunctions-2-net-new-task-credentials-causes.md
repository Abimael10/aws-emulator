Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__stepfunctions.task-credentials-dynamic-rolearn-on-legacy-lambda-arn-task-assumes-task-role lambda__stepfunctions.task-credentials-role-trust-excluding-execution-role-fails-task
Issue-Key: lambda__stepfunctions.task-credentials-dynamic-rolearn-on-legacy-lambda-arn-task-assumes-task-role|run#24|AssertionError|assert masked == (
Issue-Key: lambda__stepfunctions.task-credentials-role-trust-excluding-execution-role-fails-task|_run_denied#15|AssertionError|raise AssertionError(
Issue-Commit: a28aa3cbb761848919217b218a8a95a512704746

# lambda__stepfunctions: 2 net-new Task-Credentials causes — the Lambda 403 denial cause uses an SDK-v1 suffix, and a task role whose trust excludes the execution role is assumed anyway

> _Note: this issue was identified against the code base at commit `a28aa3cbb761848919217b218a8a95a512704746`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartExecution

## Description
Two net-new root causes in the Step Functions Task `Credentials` path, both reached by running a state machine whose Task carries `Credentials.RoleArn` over a Lambda function (StartSyncExecution for the EXPRESS arms, StartExecution/DescribeExecution for the STANDARD one). Both failing checks are named in the Issue-Trace-Ids of the OPEN issue 20261109T210043Z-lambda-stepfunctions-8-net-new-causes-step-functions-ignores-task.md (its Cause 1 of 8, "a Task state's Credentials member is not interpreted"), but that cause no longer reproduces: Credentials is now resolved and assumed, both positive controls pass, and these two checks fail at later assertions, so that issue does not cover what is described here. Also related and NOT covering these: 20261109T210043Z-cloudformation-lambda-stepfunctions-a-step-functions-lambda-task-denied-lambda-invokefunction.md reports the same SDK-suffix renderer, but keyed to a single cloudformation__lambda__stepfunctions check (and the status code it reported, 400, is now 403 — only the suffix format is still wrong).

Cause 1 of 2: the cause of a Lambda task denied lambda:InvokeFunction closes with an SDK-v1-style suffix instead of AWS's SDK suffix. Failing check id: lambda__stepfunctions.task-credentials-dynamic-rolearn-on-legacy-lambda-arn-task-assumes-task-role.
AWS:      recorded behavior only for the text — Smithy com.amazonaws.sfn#StartSyncExecutionOutput / #DescribeExecutionOutput model only the generic `error` (SensitiveError, @length 0-256) and `cause` (SensitiveCause, @length 0-32768) members that carry it. The recording shows a legacy Lambda-ARN Task (Resource IS the function ARN) run under an execution role that holds no lambda:InvokeFunction fail with status FAILED, error `Lambda.AWSLambdaException` and a cause that is Lambda's own AccessDeniedException sentence followed by AWS's SDK suffix: `User: arn:<partition>:sts::<account>:assumed-role/<execution-role-name>/<session> is not authorized to perform: lambda:InvokeFunction on resource: <function ARN> because no identity-based policy allows the lambda:InvokeFunction action (Service: Lambda, Status Code: 403, Request ID: <request-id>) (SDK Attempt Count: <n>)`. Fixed text: that IAM sentence and the two parenthesised groups, comma-separated, with the service named `Lambda` and the second group `(SDK Attempt Count: <n>)`. Variable parts, taken from the request and the emulator's state: the partition, account, execution-role name and its assumed-role session name, the function ARN from the Task's Resource, and the per-call request id and attempt count.
Emulator: emits the identical IAM sentence but closes it with `(Service: AWSLambda; Status Code: 403; Error Code: AccessDeniedException; Request ID: <request-id>; Proxy: null)` — semicolon-separated, the service named `AWSLambda` instead of `Lambda`, an `Error Code`/`Proxy: null` pair AWS does not emit on this path, and no `(SDK Attempt Count: <n>)` group at all. Status FAILED and error `Lambda.AWSLambdaException` match (those assertions passed). Hypothesis: the cause is rendered by a legacy SDK-v1 style formatter rather than from the Lambda refusal as the v2 SDK renders it.
Why:      a client that parses the cause to report the denied principal/action, or that branches on the service name and status in the suffix, reads a service name that never appears in AWS causes and loses the attempt count; alarm, retry and log-scraping logic keyed on AWS's cause text does not match.
Fix:      the Step Functions Lambda-integration error serializer that wraps a Lambda refusal into the execution's cause — render AWS's comma-separated `(Service: Lambda, Status Code: ..., Request ID: ...) (SDK Attempt Count: ...)` suffix and drop the `Error Code`/`Proxy` pair.
Traces:   lambda__stepfunctions.task-credentials-dynamic-rolearn-on-legacy-lambda-arn-task-assumes-task-role (listed under the suite report's ### lambda__stepfunctions heading).

Cause 2 of 2: the target role's trust policy is never evaluated when a Task's Credentials.RoleArn is assumed — a role that does NOT trust the execution role is assumed and the task runs. Failing check id: lambda__stepfunctions.task-credentials-role-trust-excluding-execution-role-fails-task.
AWS:      recorded behavior only. Two task roles carry the identical lambda:InvokeFunction grant and differ only in who may assume them; the execution role holds an identity-based sts:AssumeRole grant on BOTH. The one whose AssumeRolePolicyDocument names the execution role as principal succeeds; the one whose trust names an unrelated principal fails the task with States.TaskFailed and the not-authorized-to-assume cause naming both roles — the sibling recording stepfunctions.task-credentials-role-arn-from-variable-resolved-before-assume shows that cause's template as `The role <execution-role ARN> is not authorized to assume the task state's role, <target-role ARN>.` (fixed text, with the two ARNs taken from the state machine's roleArn and the Task's resolved Credentials.RoleArn).
Emulator: every one of the 10 executions the check ran against the excluded-trust machine ended `SUCCEEDED` with an empty cause — the assume is granted on the strength of the execution role's identity-based sts:AssumeRole permission alone, and the target role's trust policy (its principal, and by extension its Conditions) is never consulted. The positive twin also succeeds, so nothing distinguishes a correctly trusted role from one that excludes the caller.
Why:      a client relying on AWS's behavior gets no authorization boundary at all on per-task roles: a state machine whose Credentials.RoleArn names a role it may not assume passes in the emulator and fails in AWS, so least-privilege and cross-account trust configurations cannot be validated, and a negative test of that boundary silently reports success.
Fix:      the Task-credentials assume seam — evaluate the target role's trust policy (principal match, and the sts:ExternalId/source Conditions it carries) in the emulator's sts:AssumeRole path before vending per-task credentials, and on refusal fail the task with States.TaskFailed carrying AWS's not-authorized-to-assume cause rather than running the integration call.
Traces:   lambda__stepfunctions.task-credentials-role-trust-excluding-execution-role-fails-task (listed under the suite report's ### lambda__stepfunctions heading).

## Evidence
Cause 1 — check lambda__stepfunctions.task-credentials-dynamic-rolearn-on-legacy-lambda-arn-task-assumes-task-role [mismatch]
Definition under test (negative twin): {"StartAt":"InvokeLambda","States":{"InvokeLambda":{"Type":"Task","Resource":"<function ARN>","End":true}}} on an EXPRESS machine whose execution role holds NO lambda:InvokeFunction (the positive twin, identical but with "Credentials":{"RoleArn.$":"$.roleArn"} naming an assumable task role, SUCCEEDED and echoed the raw event, matching AWS).
StartSyncExecution answered FAILED with error 'Lambda.AWSLambdaException' (both as AWS does) and cause (session name and request id masked by the check):
  Expected: "User: arn:aws:sts::<account>:assumed-role/<exec-role>/<session> is not authorized to perform: lambda:InvokeFunction on resource: <function ARN> because no identity-based policy allows the lambda:InvokeFunction action (Service: Lambda, Status Code: 403, Request ID: <request-id>) (SDK Attempt Count: <n>)"
  Actual:   "User: arn:aws:sts::<account>:assumed-role/<exec-role>/<session> is not authorized to perform: lambda:InvokeFunction on resource: <function ARN> because no identity-based policy allows the lambda:InvokeFunction action (Service: AWSLambda; Status Code: 403; Error Code: AccessDeniedException; Request ID: <request-id>; Proxy: null)"
Failure text: check_lambda__stepfunctions_task_credentials_dynamic_rolearn_on_legacy_lambda_arn_task_assumes_task_role.py line 103, `assert masked == (...)` — AssertionError: the denial must name the execution role's own session and the function.
Account id, role/function names, the session name and the request id are per-run values; the divergence is the suffix format, the service name 'AWSLambda' and the missing SDK Attempt Count group.

Cause 2 — check lambda__stepfunctions.task-credentials-role-trust-excluding-execution-role-fails-task [mismatch]
Two task roles with the same inline lambda:InvokeFunction policy on the same function: role A's trust principal is the execution role, role B's trust principal is an unrelated role ARN. The execution role is granted sts:AssumeRole on both. Two STANDARD machines, each a Task with "Credentials":{"RoleArn":"<A|B>"}.
  Expected (recorded AWS): the machine naming role B fails — status FAILED, error States.TaskFailed, cause "The role <execution-role ARN> is not authorized to assume the task state's role, <role B ARN>."; the machine naming role A SUCCEEDS.
  Actual (emulator): the role-A machine SUCCEEDS (as AWS does) and the role-B machine also SUCCEEDS on all 10 attempts.
Failure text: check_lambda__stepfunctions_task_credentials_role_trust_excluding_execution_role_fails_task.py line 332, in _run_denied — AssertionError: every one of 10 executions on '<state machine ARN>' never reached the task-role denial under test; last status 'SUCCEEDED' cause ''.
Role and machine names are per-run values; the requirement is that an assume whose target trust excludes the execution role is refused and surfaces as States.TaskFailed.

## Steps to Reproduce
Cause 1 (Lambda denial cause suffix):
1. CreateFunction a trivial python3.12 function; CreateRole an execution role trusting states.amazonaws.com with NO lambda:InvokeFunction grant.
2. CreateStateMachine (type EXPRESS) with definition {"StartAt":"InvokeLambda","States":{"InvokeLambda":{"Type":"Task","Resource":"<function ARN>","End":true}}} using that role.
3. StartSyncExecution with any input.
   Expected: status FAILED, error Lambda.AWSLambdaException, cause ending "... because no identity-based policy allows the lambda:InvokeFunction action (Service: Lambda, Status Code: 403, Request ID: <id>) (SDK Attempt Count: <n>)".
   Actual: the same sentence ending "(Service: AWSLambda; Status Code: 403; Error Code: AccessDeniedException; Request ID: <id>; Proxy: null)".

Cause 2 (target-role trust not evaluated):
1. CreateFunction as above. CreateRole an execution role trusting states.amazonaws.com.
2. CreateRole two task roles, each with an inline policy allowing lambda:InvokeFunction on the function; role A's AssumeRolePolicyDocument principal is {"AWS": "<execution role ARN>"}, role B's is an unrelated role ARN.
3. PutRolePolicy on the execution role allowing sts:AssumeRole on both task role ARNs.
4. CreateStateMachine twice, each Task carrying "Credentials": {"RoleArn": "<role A|B ARN>"}; StartExecution on each and poll DescribeExecution to terminal.
   Expected: the role-A execution SUCCEEDS; the role-B execution FAILS with error States.TaskFailed and cause "The role <execution-role ARN> is not authorized to assume the task state's role, <role B ARN>.".
   Actual: both SUCCEED (the role-B execution repeats as SUCCEEDED on every retry).
