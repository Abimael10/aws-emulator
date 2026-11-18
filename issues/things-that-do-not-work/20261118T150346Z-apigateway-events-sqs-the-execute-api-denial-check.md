Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__events__sqs.xsvc-apigateway-1-target-role-without-execute-api-invoke-is-denied
Issue-Key: apigateway__events__sqs.xsvc-apigateway-1-target-role-without-execute-api-invoke-is-denied|_receive#28|Timeout|return found
Issue-Commit: 8e5675a008353ca996c02d5e79f804eabbe67887

# apigateway__events__sqs: the execute-api denial check still reaches no verdict — nothing it waits for (stage response, permitted-twin delivery, denied-twin dead letter) arrives inside the budget

> _Note: this issue was identified against the code base at commit `8e5675a008353ca996c02d5e79f804eabbe67887`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutEvents

## Description
1 net-new root cause (a NEW replay key for a check two open issues already name at earlier stall points; the suite reported this key unowned).

Cause 1 of 1: the check that grades an EventBridge execute-api target whose RoleArn lacks execute-api:Invoke produces no observable effect at all inside the harness budget — the replay detail is exactly `Error: Check timed out after 60.0s`, with no stack frame, so the stall is no longer at either of the two points already filed. Failing check: apigateway__events__sqs.xsvc-apigateway-1-target-role-without-execute-api-invoke-is-denied.

AWS: EventBridge invokes an API Gateway (execute-api) target AS the role named in PutTargets' RoleArn, so a role that may not execute-api:Invoke the target's own ARN produces a 403 at the stage and no delivery. With RetryPolicy {MaximumRetryAttempts: 0, MaximumEventAgeInSeconds: 60} the event is written once to the target's DeadLetterConfig queue, carrying message attributes RULE_ARN (the rule's ARN), TARGET_ARN (the target ARN from PutTargets), ERROR_CODE and ERROR_MESSAGE. ERROR_CODE is the fixed text `NO_PERMISSIONS`. ERROR_MESSAGE is the fixed template `Got status code "403" while trying to invoke "POST <stage invoke URL>/orders/<path id>". (Service: api_gateway; Status Code: 403; Error Code: Forbidden; Request ID: <id>; Proxy: null)` — the fixed text is everything outside the angle brackets, the stage invoke URL and the substituted path id come from the request (the target's HttpParameters PathParameterValues `$.detail.id`), and the Request ID is minted per delivery attempt and is NOT part of the requirement. Structurally AWS also guarantees two things the check leans on before it ever gets there: a signed POST to a deployed AWS_IAM-authorized stage always terminates in an HTTP response, and the twin target whose role DOES hold execute-api:Invoke delivers the same event through the stage to the sink queue. Recorded behavior only for the per-attribute values; the model says nothing about dead-letter attribute names.

Emulator: nothing arrives. The whole reported detail is `Error: Check timed out after 60.0s`: no status, no dead letter, no stack frame naming which poll was in flight. The check's waits, in order, are (1) its warm-up — a signed POST to the freshly deployed stage expecting 200, then a drain of the sink queue for the warm-up marker (60 s), (2) the permitted twin's delivery to the sink (60 s per published event, inside a 240 s republish loop), and (3) the denied twin's dead letter (45 s per iteration, accepted only when ERROR_MESSAGE starts `Got status code "403"`). None of them completed. Two open issues carry EARLIER keys for this same check — 20261118T115347Z-apigateway-events-sqs-an-execute-api-target-whose.md and 20261118T120000Z-apigateway-events-sqs-a-denied-execute-api-target-dead.md (key `…|_poll_signed_post#8|Timeout|time.sleep(5)`) — and the suite did not mark this key owned by either, so the stall has moved again and is now unlocated. The second of those issues records that an instrumented run which dead-lettered EVERY outcome class of the delivery pass still produced no message this check could find, i.e. the delivery hand-over is not reached for this target at all; this report adds that the check now does not even get as far as reporting a frame.

Why: a client that scopes an EventBridge target's role gets no answer either way — the denial is never surfaced as a dead letter (so a mis-scoped role is indistinguishable from an event nothing matched), and the stage leg the same configuration depends on never returns a status, so no caller timeout/retry policy observes an outcome. A hang is strictly worse than a wrong status.

Fix: the EventBridge target-delivery path for execute-api targets — role-scoped invocation of the deployed stage (the delivery must be attempted AS the target's RoleArn and classified NO_PERMISSIONS when that role cannot invoke the target ARN) and the dead-letter hand-over that writes RULE_ARN/TARGET_ARN/ERROR_CODE/ERROR_MESSAGE for a failed delivery — together with the execute-api data-plane path for an AWS_IAM-authorized method, which must always terminate in a method response so the check's warm-up and its permitted twin can complete.

Traces: apigateway__events__sqs.xsvc-apigateway-1-target-role-without-execute-api-invoke-is-denied (replay detail `Error: Check timed out after 60.0s`, no frame).

## Evidence
Expected (AWS): one PutEvents entry on a custom bus fans out to two rules targeting `arn:aws:execute-api:<region>:<account>:<api-id>/<stage>/POST/orders/*`. The target whose RoleArn allows execute-api:Invoke delivers through the stage to the sink queue (message carries the orderSrc attribute `rule-good` and the published marker). The twin whose role allows only execute-api:InvalidateCache delivers nothing and, with MaximumRetryAttempts=0, writes exactly one dead letter to its DeadLetterConfig queue with MessageAttributes RULE_ARN = the bad rule's ARN, TARGET_ARN = the target ARN, ERROR_CODE = "NO_PERMISSIONS" and ERROR_MESSAGE = `Got status code "403" while trying to invoke "POST <stage invoke URL>/orders/<detail.id>". (Service: api_gateway; Status Code: 403; Error Code: Forbidden; Request ID: <per-attempt id>; Proxy: null)`. Nothing with orderSrc `rule-denied` ever reaches the stage's sink. After iam.PutRolePolicy grants execute-api:Invoke to that role, the same target delivers.

Actual (emulator): no status, no sink delivery and no dead letter were observed. The complete replay detail is:
  Error: Check timed out after 60.0s
There is no frame in the detail, so the stalling poll is not identifiable from the recording — unlike the two already-filed keys for this check, which named `_poll_signed_post` and `_receive`. API id, stage host/port, account, queue URLs, role ARNs, the generated markers and the per-attempt Request ID are per-run values and are not requirements; the requirements are the 403 classification, the fixed ERROR_CODE text and the ERROR_MESSAGE template above, and that the permitted twin delivers while the denied twin does not.

## Steps to Reproduce
1. CreateQueue (sink) and deploy a REGIONAL REST API whose POST /orders/{id} method is AWS_IAM-authorized and integrated to SQS SendMessage against the sink queue; create_deployment + stage.
2. Signed POST to the stage invoke URL for /orders/w-1 and drain the sink for the warm-up marker -> EXPECT 200 and one message.
3. events.CreateEventBus + two PutRule with the identical pattern {"source": ["sim2real.orders"], "detail-type": ["OrderPlaced"]}.
4. iam.CreateRole twice, trusted by events.amazonaws.com: one allowing execute-api:Invoke on `arn:aws:execute-api:<region>:<account>:<api-id>/<stage>/POST/orders/*`, one allowing only execute-api:InvalidateCache on the same ARN.
5. PutTargets: the permitted role on rule A; on rule B the same target ARN with the under-privileged role, HttpParameters {PathParameterValues: ["$.detail.id"], HeaderParameters: {"x-order-src": "rule-denied"}}, DeadLetterConfig pointing at a DLQ whose policy admits events.amazonaws.com for that rule ARN, and RetryPolicy {MaximumRetryAttempts: 0, MaximumEventAgeInSeconds: 60}.
6. PutEvents one matching entry (FailedEntryCount 0).
   -> EXPECT the permitted twin's message in the sink and exactly one dead letter with ERROR_CODE "NO_PERMISSIONS" and the quoted ERROR_MESSAGE; nothing with orderSrc `rule-denied` at the stage.
   -> OBSERVE neither: the check is cut off at the 60 s harness cap with no frame.
7. Or replay the check id in trace_ids.
