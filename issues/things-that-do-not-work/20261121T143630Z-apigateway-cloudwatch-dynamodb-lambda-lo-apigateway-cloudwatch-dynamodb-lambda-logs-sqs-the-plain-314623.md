Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudwatch__dynamodb__lambda__logs__sqs.xflavor-order-service-cfn-alarm-echoes-declared-period
Issue-Key: apigateway__cloudwatch__dynamodb__lambda__logs__sqs.xflavor-order-service-cfn-alarm-echoes-declared-period|_poll_until#12|Timeout|last = probe()
Issue-Commit: 8c92c7c2cb0689aef734153d4944deecfb6a59a7

# apigateway__cloudwatch__dynamodb__lambda__logs__sqs: the plain-CloudFormation order-service deployment hangs — no terminal StackStatus and no graded leg before the check's cap (1 cause)

> _Note: this issue was identified against the code base at commit `8c92c7c2cb0689aef734153d4944deecfb6a59a7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
One net-new root cause in the order-intake ("order-service") CloudFormation-flavor scenario of this service group. The check is killed by the harness cap with its ONLY output being a timeout line: not a single leg (wiring, worker, logs, metrics, alarm, denial, drift, teardown) is ever graded, so the emulator produced neither a usable deployment nor an error a caller could branch on.

NOTE ON RELATED ISSUES: open issue 20261121T044528Z-apigateway-cloudwatch-dynamodb-lambda-lo-apigateway-cloudwatch-dynamodb-lambda-logs-sqs-both-order.md (Cause 1) and open issue 20261110T214424Z-apigateway-cloudwatch-dynamodb-lambda-lo-...-two-more.md both describe failures of this same check, but the replay keys they carry are different frames (the first carries '<lambda>#12|Timeout|("table", lambda: ddb.describe_table(...))' from the teardown probe, the second an AssertionError on a stack that DID reach ROLLBACK_COMPLETE). The replay confirms neither key matches the failure seen now — the suite marks this check neither ALREADY FILED nor OWNED — so this divergence is filed here rather than assumed covered.

Cause 1 of 1: a CreateStack/UpdateStack of the hand-written order-intake template never produces an observable terminal outcome, and the check reaches its cap with nothing graded.
AWS: Smithy (cloudformation) DescribeStacks -> Stack.StackStatus targets com.amazonaws.cloudformation#StackStatus, an enum whose non-terminal members all end in _IN_PROGRESS and whose terminal members are CREATE_COMPLETE, CREATE_FAILED, ROLLBACK_COMPLETE, ROLLBACK_FAILED, DELETE_COMPLETE, DELETE_FAILED, UPDATE_COMPLETE, UPDATE_FAILED, UPDATE_ROLLBACK_COMPLETE, IMPORT_COMPLETE, IMPORT_ROLLBACK_COMPLETE, IMPORT_ROLLBACK_FAILED. On AWS a CreateStack of this template (REST API with POST /orders, two Lambda functions, a DynamoDB table, an SQS queue, an event-source mapping, a log group and a CloudWatch alarm) always walks from a *_IN_PROGRESS status to one of those terminal statuses in bounded time, and every call on the deploy path (CreateStack, DescribeStacks, DescribeStackEvents, DescribeStackResources) returns a response — a resource handler that fails lands the stack in CREATE_FAILED/ROLLBACK_COMPLETE with a resource status reason, it does not stop answering. For the per-leg application assertions (queue delivery, structured logs, metric datapoint, the alarm echoing its declared Period/EvaluationPeriods/Threshold/TreatMissingData, the denied UpdateItem, the in-place update and the post-destroy refusals) this is recorded behavior only.
Emulator: the check's entire detail is the single line 'Error: Check timed out after 60.0s'. No terminal StackStatus, no HTTP status line from the deployed stage and no error code or message were observed at all. The check's own per-leg ceilings are far larger than the harness cap it died at (deploy 600 s, route 180 s, worker 180 s), so this is not a leg assertion firing on a converged-but-wrong stack: it is the absence of any response on the deploy path.
Why: a client deploying a plain CloudFormation application against the emulator gets neither a converged stack nor a failure it can diagnose. Every waiter written against DescribeStacks (including the AWS CLI's `cloudformation wait stack-create-complete` and the SDK waiters) blocks indefinitely, no rollback reason is ever retrievable, and no downstream behavior of the deployed estate is reachable.
Fix: the CreateStack/UpdateStack progression in the cloudformation crate's stack lifecycle/state machine — a stack must always leave its *_IN_PROGRESS status for a modeled terminal StackStatus within bounded time, including when a nested resource handler errors (which must land as *_FAILED or ROLLBACK_COMPLETE carrying a resource-status reason), and no call on that path (CreateStack, DescribeStacks, DescribeStackEvents, DescribeStackResources) may block rather than answer.
Traces: apigateway__cloudwatch__dynamodb__lambda__logs__sqs.xflavor-order-service-cfn-alarm-echoes-declared-period

## Evidence
Expected: CreateStack of the order-intake CloudFormation template with CAPABILITY_IAM/CAPABILITY_NAMED_IAM reaches a terminal StackStatus (CREATE_COMPLETE on AWS; a modeled *_FAILED/ROLLBACK_COMPLETE with a resource-status reason if a resource cannot be created), after which POST /orders on the deployed stage answers 2xx and the wiring/worker/logs/metrics/alarm/denial/drift/teardown legs can be graded.

Actual: the replayed check produces exactly one line — 'Error: Check timed out after 60.0s' — with no terminal stack status, no HTTP status line, no body, no error code and no error message. Zero of the nine legs are graded. The check's internal budgets (_DEPLOY_BUDGET_S = 600.0, _ROUTE_BUDGET_S = 180.0, _WORKER_BUDGET_S = 180.0) are all larger than the elapsed cap, so no leg assertion was reached; the deploy path simply never produced an observable outcome.

Status/code/message requirement: AWS never answers this path by not answering. A refusal must be a modeled terminal StackStatus plus the resource-status reason text for the failing resource (fixed text as AWS words it for that resource type, with the resource logical id, type and the offending property/value taken from the submitted template), not silence.

## Steps to Reproduce
1. CreateStack the hand-written order-intake CloudFormation template (REST API POST /orders -> intake Lambda -> SQS queue -> worker Lambda -> DynamoDB table, plus a log group and a CloudWatch alarm on the worker's OrdersFulfilled metric) with Capabilities=[CAPABILITY_IAM, CAPABILITY_NAMED_IAM].
2. Poll DescribeStacks for that stack name: the StackStatus never becomes one of the terminal StackStatus members, and the stage never answers POST /orders, so the check is killed at its 60 s harness cap with no leg graded and no error to branch on.
3. Re-replay apigateway__cloudwatch__dynamodb__lambda__logs__sqs.xflavor-order-service-cfn-alarm-echoes-declared-period: it must reach a terminal StackStatus and then run its wiring/worker/logs/metrics/alarm/denial/drift/teardown legs instead of timing out with no output.
