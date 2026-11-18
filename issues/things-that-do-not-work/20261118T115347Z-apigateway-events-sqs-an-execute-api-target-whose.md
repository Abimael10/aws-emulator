Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__events__sqs.xsvc-apigateway-1-target-role-without-execute-api-invoke-is-denied
Issue-Key: apigateway__events__sqs.xsvc-apigateway-1-target-role-without-execute-api-invoke-is-denied|run#28|Timeout|found = _receive(sqs, dlq_url, lambda m: _marker_of(m) in attempted, want=1,
Issue-Commit: f2831bfd9793e6f7d4548f15bac1de57747f4451

# apigateway__events__sqs: an execute-api target whose role lacks execute-api:Invoke is still never dead-lettered, so the denial check reaches no verdict (times out)

> _Note: this issue was identified against the code base at commit `f2831bfd9793e6f7d4548f15bac1de57747f4451`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutEvents (EventBridge delivery to an arn:aws:execute-api target under a RoleArn that does not allow execute-api:Invoke)

## Description
Cause 1 of 1: an EventBridge rule target naming an arn:aws:execute-api resource with a RoleArn whose policies do NOT grant execute-api:Invoke on that ARN produces no observable outcome: no dead-letter message ever lands on the target's DeadLetterConfig queue, and the check that grades the denial exhausts the whole 60 s run budget without reaching an assertion.

AWS: recorded behavior only for the message text (no Smithy member models it). EventBridge invokes an API Gateway target as the role named in the target's RoleArn (PutTargets.Target.RoleArn: EventBridge relies on the IAM role specified for EC2 instances, Kinesis data streams, Step Functions state machines and API Gateway APIs). A role that may not execute-api:Invoke the target's own ARN gets 403 Forbidden from the AWS_IAM-protected stage, the delivery fails, and with RetryPolicy MaximumRetryAttempts=0 the event is written immediately to the target's dead-letter queue carrying message attributes RULE_ARN (the rule's ARN), TARGET_ARN (the target's ARN) and ERROR_CODE = "NO_PERMISSIONS". The recorded ERROR_MESSAGE is, as fixed text with the bracketed parts taken from the request and the delivery: 'Got status code "403" while trying to invoke "POST <stage invoke URL>/<substituted path>". (Service: api_gateway; Status Code: 403; Error Code: Forbidden; Request ID: <per-delivery request id>; Proxy: null)' — the request id is minted per delivery attempt and is the only variable part besides the substituted request line; every other byte is fixed. Nothing may be delivered to the stage's sink for the denied target.

Emulator: nothing is observable. The check publishes matching events in a bounded loop and polls the dead-letter queue; the run budget expires first ("Check timed out after 60.0s") with no message on the DLQ, so a denied delivery is indistinguishable from an event that matched nothing. The sibling checks of this scenario pass, so bus matching, PutTargets with an execute-api ARN plus HttpParameters, the signed request at the stage and the dead-letter write itself all work on that code path — the handover is not reached for a target of this kind/role combination.

Why: the dead-letter record with ERROR_CODE=NO_PERMISSIONS and the quoted 403 is the only signal a caller has that a mis-scoped target role stopped the delivery. With no record at all, an operator cannot distinguish a permission defect from a pattern that never matched, and cannot confirm that the denial actually happened rather than the event leaking to the protected stage.

Fix: the EventBridge target-delivery path for execute-api targets — ensure the authorization outcome for the target's RoleArn reaches the failure/dead-letter handover (and that the 403 from the AWS_IAM stage is classified NO_PERMISSIONS with the message above) rather than ending the delivery attempt silently.

REOPENS/relates: the open 20261118T120000Z-apigateway-events-sqs-a-denied-execute-api-target-dead.md carries this same check id and records the investigation that established the handover is not reached for this target; the suite did not mark this failure as owned by it in this replay (its replay key differs — it was keyed at _poll_signed_post#8 while this run times out without a frame), so the failure is reported here with its current signature. This is not a second defect: treat the two as the same root cause.

Traces: apigateway__events__sqs.xsvc-apigateway-1-target-role-without-execute-api-invoke-is-denied.

## Evidence
Check: apigateway__events__sqs.xsvc-apigateway-1-target-role-without-execute-api-invoke-is-denied [mismatch]
Error: Check timed out after 60.0s

The check's own budgets are far longer than the harness budget (a 240 s publish/dead-letter loop and a 180 s post-repair loop), so the timeout means no progress was made toward the first dead letter, not that a bound was merely tight.

Expected (AWS), per the assertions the check never reached:
  - one message on the target's DeadLetterConfig queue carrying the published marker
  - MessageAttributes RULE_ARN == the rule's ARN, TARGET_ARN == arn:aws:execute-api:<region>:<account>:<api-id>/<stage>/POST/orders/*
  - MessageAttributes ERROR_CODE == "NO_PERMISSIONS"
  - MessageAttributes ERROR_MESSAGE == 'Got status code "403" while trying to invoke "POST <stage invoke url>/orders/<detail.id>". (Service: api_gateway; Status Code: 403; Error Code: Forbidden; Request ID: <request id>; Proxy: null)'  — fixed text as quoted; the method, the substituted request line and the request id come from the delivery, the request id being minted per attempt
  - no denied-path message (x-order-src: rule-denied) at the stage's sink queue
  - after granting execute-api:Invoke to the same role, the same target delivers to the sink

Actual: nothing lands on the dead-letter queue; the check is killed at the 60 s budget before any of the above can be graded.

## Steps to Reproduce
1. Create an SQS sink queue and a REST API whose POST /<route>/{id} method is AWS_IAM-authorized with an AWS-type SQS SendMessage integration; deploy it to a stage. Confirm a signed POST answers 200 and the message reaches the sink.
2. Create a custom event bus and a rule matching {"source": ["sim2real.orders"], "detail-type": ["OrderPlaced"]}.
3. Create an IAM role trusted by events.amazonaws.com whose inline policy allows only execute-api:InvalidateCache (NOT execute-api:Invoke) on arn:aws:execute-api:<region>:<account>:<api-id>/<stage>/POST/orders/*.
4. Create a dead-letter queue whose policy allows the rule to send to it. PutTargets a target with that ARN, that RoleArn, HttpParameters {PathParameterValues: ["$.detail.id"], HeaderParameters: {x-order-src: rule-denied}}, DeadLetterConfig pointing at the DLQ, and RetryPolicy {MaximumRetryAttempts: 0, MaximumEventAgeInSeconds: 60}.
5. PutEvents a matching entry, then poll the dead-letter queue.
   -> EXPECT one message with ERROR_CODE=NO_PERMISSIONS and the quoted 403 ERROR_MESSAGE.
   -> OBSERVE no message at all; the check never reaches a verdict and is cut off at the run budget.
6. Or replay the check id in trace_ids.
