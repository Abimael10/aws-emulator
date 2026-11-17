Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__dynamodb__events__lambda.xsvc-cloudformation-3-update-rollback-complete-rejected-by-exact-rollback-complete-rule
Issue-Key: cloudformation__dynamodb__events__lambda.xsvc-cloudformation-3-update-rollback-complete-rejected-by-exact-rollback-complete-rule|_poll_item#35|Timeout|time.sleep(3.0)
Issue-Commit: 9d0149c39ab5c73582ec7b48950d23789498e457

# cloudformation__dynamodb__events__lambda: the UPDATE_ROLLBACK_COMPLETE vs ROLLBACK_COMPLETE near-miss check never converges, so stack status-change events are never shown reaching their Lambda target

> _Note: this issue was identified against the code base at commit `9d0149c39ab5c73582ec7b48950d23789498e457`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
Cause 1 of 1 — the stack status-change → EventBridge rule → Lambda → DynamoDB chain never completes.
AWS: CloudFormation publishes `CloudFormation Stack Status Change` events (source aws.cloudformation) to the default event bus with detail.stack-id and detail.status-details.status/status-reason. A rule whose pattern pins detail.status-details.status to exactly `ROLLBACK_COMPLETE` must NOT be delivered an event whose status is `UPDATE_ROLLBACK_COMPLETE` — EventBridge string operands match the whole value, not a prefix or suffix — while a matching status is delivered and its Lambda target writes the stack-id, status, status-reason, event id, detail-type and source it received. Recorded behavior only (the detail shape is not modeled).
Emulator: the check never reaches any of those assertions — the replay reports `Check timed out after 60.0s`. Hypothesis: the chain's first convergence wait does not complete — either CloudFormation publishes no stack status-change event, or the rule never invokes its Lambda target, so nothing is ever written to the DynamoDB table the check reads.
Why: a client cannot establish either half of the contract: that a stack status-change event is delivered at all, or that an exact-status rule withholds the near-miss status. A quiet queue that is actually dead reads as a correct near-miss.
Fix: CloudFormation's stack status-change event publication to the default bus and the rule → Lambda target invocation path.
Traces: cloudformation__dynamodb__events__lambda.xsvc-cloudformation-3-update-rollback-complete-rejected-by-exact-rollback-complete-rule.

## Evidence
`Check timed out after 60.0s` — no assertion reached. The check's own budgets are STACK_TIMEOUT_S 240 and DELIVERY_TIMEOUT_S 300, and its Lambda handler writes each received event's stack-id, status, status-reason, id, detail-type and source into DynamoDB; no such item was ever observable because the check never got past its first wait.

## Steps to Reproduce
1. Create a DynamoDB table and a Lambda function that writes each received EventBridge event's detail.stack-id, detail.status-details.status/status-reason, event id, detail-type and source into it.
2. Create a default-bus rule on detail-type `CloudFormation Stack Status Change` with detail.status-details.status pinned to exactly [`ROLLBACK_COMPLETE`], targeting that function (with the lambda:InvokeFunction permission for events.amazonaws.com).
3. CreateStack successfully, then UpdateStack with a template that fails server-side (an AWS::SSM::Parameter Value violating its AllowedPattern) so the stack reaches UPDATE_ROLLBACK_COMPLETE.
4. Confirm the table receives items for matching statuses and none for UPDATE_ROLLBACK_COMPLETE.
