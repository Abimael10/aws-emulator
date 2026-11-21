Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudwatch__dynamodb__lambda__logs__sqs.xflavor-order-service-cfn-alarm-echoes-declared-period apigateway__cloudwatch__dynamodb__lambda__logs__sqs.xflavor-order-service-sam
Issue-Key: apigateway__cloudwatch__dynamodb__lambda__logs__sqs.xflavor-order-service-cfn-alarm-echoes-declared-period|<lambda>#12|Timeout|("table", lambda: ddb.describe_table(TableName=state["TableName"])),
Issue-Key: apigateway__cloudwatch__dynamodb__lambda__logs__sqs.xflavor-order-service-sam|_poll_until#11|Timeout|time.sleep(interval_s)
Issue-Commit: cf3f6fdf9cb70669f15b109005870eccb99ca479

# apigateway__cloudwatch__dynamodb__lambda__logs__sqs: both order-service cross-flavor deployments now HANG instead of finishing — no terminal StackStatus and no stage response before the check's cap (2 causes)

> _Note: this issue was identified against the code base at commit `cf3f6fdf9cb70669f15b109005870eccb99ca479`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Two net-new root causes, both in the order-intake ("order-service") cross-flavor scenario of this service group. Both checks are killed by the harness cap with their ONLY output being a timeout line: no leg is ever graded, so the emulator never produced either a usable deployment or an error a caller could branch on. Related open issues exist for these two checks but neither covers the behavior seen here, because in both cases the emulator now fails EARLIER and differently than the key those issues carry (named per cause below).

Cause 1 of 2: the plain-CloudFormation order-service stack never reaches a terminal stack status, so the check hangs to its cap instead of deploying or failing.
AWS: Smithy (cloudformation) DescribeStacks -> Stack.StackStatus targets com.amazonaws.cloudformation#StackStatus, an enum whose terminal members are CREATE_COMPLETE, CREATE_FAILED, ROLLBACK_COMPLETE, ROLLBACK_FAILED, UPDATE_COMPLETE, UPDATE_FAILED, UPDATE_ROLLBACK_COMPLETE, UPDATE_ROLLBACK_FAILED, DELETE_COMPLETE, DELETE_FAILED. A CreateStack/UpdateStack of this template (REST API + 2 Lambda functions + DynamoDB table + SQS queue + event-source mapping + log group + CloudWatch alarm) always walks from *_IN_PROGRESS to one of those terminal statuses, and every call in that walk (DescribeStacks, DescribeStackEvents, and the stage request that follows) returns a response. Recorded behavior only for the individual leg assertions.
Emulator: the check's entire output is 'Check timed out after 60.0s'. Neither a terminal StackStatus nor any HTTP status line from the deployed stage was observed, i.e. the stack stays in an *_IN_PROGRESS status (or one call in the deploy path never returns) for longer than the whole check budget. This is NOT the behavior open issue 20261110T214424Z-apigateway-cloudwatch-dynamodb-lambda-lo-...-two-more.md describes for this same check: its replay key for it is '...xflavor-order-service-cfn-alarm-echoes-declared-period|_wait_stack#4|AssertionError|raise AssertionError(', i.e. a stack that DID reach ROLLBACK_COMPLETE and was asserted on. The check no longer reaches that assertion at all, so that issue does not cover this divergence.
Why: a client deploying a plain CloudFormation application gets neither a converged stack nor a failure: every waiter it writes against DescribeStacks blocks forever, and no rollback reason is ever available to diagnose.
Fix: the CloudFormation CreateStack/UpdateStack progression in the cloudformation crate — a stack must always leave its *_IN_PROGRESS status for a modeled terminal StackStatus within bounded time (including when a nested resource handler errors, which must land as *_FAILED/ROLLBACK_COMPLETE with a resource-status reason), and no call on that path may block.
Traces: apigateway__cloudwatch__dynamodb__lambda__logs__sqs.xflavor-order-service-cfn-alarm-echoes-declared-period

Cause 2 of 2: the SAM-rendered order-service application still never completes, now hanging past the far larger 600 s cap.
AWS: recorded behavior only — a stack deployed with CAPABILITY_AUTO_EXPAND from an AWS::Serverless-2016-10-31 template converges to CREATE_COMPLETE (Smithy cloudformation StackStatus as above), the method/AWS_PROXY integration and lambda:InvokeFunction permission synthesized from the function's Events block serve the stage invoke URL over HTTPS, and every polled stack/resource status and every stage request returns.
Emulator: the check's entire output is 'Check timed out after 600.0s' — no terminal stack status, no HTTP status line, no body, so none of the nine legs is graded. Open issue 20261024T043741Z-apigateway-cloudwatch-dynamodb-lambda-lo-...-the-sam.md describes a hang of this check, but its replay key is '...xflavor-order-service-sam|_post_order#5|Timeout|time.sleep(5.0)' at a 60 s cap; the hang now occurs with a ten-times-larger budget and not at that frame, so that key does not carry this failure and it is filed here. Hypothesis: the AWS::Serverless::* resources are expanded late or not at all, and the execute-api data plane for the resulting stage never answers.
Why: the whole SAM path is unusable — a caller gets a stalled deploy rather than a working endpoint or an error, and every downstream behavior (queue delivery, structured logs, metric datapoint, alarm period, deny propagation, teardown) is unobservable.
Fix: the AWS::Serverless-2016-10-31 transform expansion in the cloudformation crate (it must materialize the API, integration, permission and event-source-mapping resources the template only declares under Events) and the execute-api stage request path for a deployed stage, which must always produce a response rather than hang.
Traces: apigateway__cloudwatch__dynamodb__lambda__logs__sqs.xflavor-order-service-sam

## Evidence
Cause 1 — expected: CreateStack of order_service_cfn.yaml reaches a terminal StackStatus (CREATE_COMPLETE on real AWS) and the POST /orders stage request answers 2xx, so the wiring/worker/logs/metrics/alarm/denial/drift/teardown legs are graded. Actual: the check produces one line only — 'Error: Check timed out after 60.0s' — with no terminal stack status and no HTTP status line. (The check's own per-leg ceilings are far larger: deploy 600 s, route 180 s, worker 180 s, logs 240 s, metrics 300 s, so the timeout is not a leg assertion firing but the absence of any response.)

Cause 2 — expected: the SAM order-service stack reaches CREATE_COMPLETE and its stage serves POST /orders. Actual: 'Error: Check timed out after 600.0s' — no terminal stack status, no HTTP status, no body.

## Steps to Reproduce
1. Cause 1: CreateStack the hand-written order-intake CloudFormation template (REST API POST /orders -> intake Lambda -> SQS -> worker Lambda -> DynamoDB table, plus log group and CloudWatch alarm) with CAPABILITY_IAM/CAPABILITY_NAMED_IAM, then poll DescribeStacks: the stack never reports a terminal StackStatus and the stage never answers; the check is killed at its cap with no graded leg.
2. Cause 2: CreateStack the equivalent AWS::Serverless-2016-10-31 template with CAPABILITY_AUTO_EXPAND and POST an order at the stage invoke URL: nothing returns, and the check is killed at the 600 s cap.
3. Re-replay apigateway__cloudwatch__dynamodb__lambda__logs__sqs.xflavor-order-service-cfn-alarm-echoes-declared-period and apigateway__cloudwatch__dynamodb__lambda__logs__sqs.xflavor-order-service-sam; each must reach a terminal StackStatus and then run its wiring/worker/logs/metrics/alarm/denial/drift/teardown legs.
