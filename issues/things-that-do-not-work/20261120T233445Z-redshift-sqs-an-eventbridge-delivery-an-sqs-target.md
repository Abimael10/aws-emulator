Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: redshift__sqs.cfn-deploy-redshift-sqs-queue-policy-negative redshift__sqs.sqs-dead-letter-queue-captures-failed-eventbridge-delivery-positive
Issue-Key: redshift__sqs.cfn-deploy-redshift-sqs-queue-policy-negative|run#13|AssertionError|assert error_code == "NO_PERMISSIONS", f"unexpected DLQ error code: {error_code!r}"
Issue-Key: redshift__sqs.sqs-dead-letter-queue-captures-failed-eventbridge-delivery-positive|run#17|AssertionError|assert error_code == "NO_PERMISSIONS", (
Issue-Commit: 1b14ccf8bc557e9f3ed20a9ce1fb318436d9b18d

# redshift__sqs: an EventBridge delivery an SQS target's resource policy refuses is dead-lettered with ERROR_CODE='NO_RESOURCE' instead of 'NO_PERMISSIONS'

> _Note: this issue was identified against the code base at commit `1b14ccf8bc557e9f3ed20a9ce1fb318436d9b18d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutTargets

## Description
One net-new root cause, grounded in two failing checks the suite lists under `redshift__sqs`.

Cause 1 of 1: when an EventBridge rule's SQS target exists but its resource policy does not authorize the delivery — either because the queue has no policy at all, or because its policy grants events.amazonaws.com only for a DIFFERENT rule's aws:SourceArn — the dead-letter copy written to the target's DeadLetterConfig queue is stamped with the message attribute ERROR_CODE='NO_RESOURCE'. That is the missing-target class; an authorization refusal belongs to the NO_PERMISSIONS class. Failing checks: redshift__sqs.cfn-deploy-redshift-sqs-queue-policy-negative (policy scoped to the other rule's SourceArn), redshift__sqs.sqs-dead-letter-queue-captures-failed-eventbridge-delivery-positive (target left with no policy at all).

AWS: recorded behavior only — EventBridge's dead-letter message attributes are not modelled. Smithy (eventbridge): Target.DeadLetterConfig is documented only as the queue dead-letter events are sent to, and no shape enumerates ERROR_CODE's values, so the tokens come from the recordings. In both recordings the dead letter carries the original event verbatim in its body plus the message attributes RULE_ARN (the rule of the failed delivery, a value from state), TARGET_ARN (the refusing target, from state) and ERROR_CODE — a fixed enumerated token with nothing in it taken from the request — set to 'NO_PERMISSIONS' whenever the target is present and the refusal is an authorization refusal (HTTP 403 / AccessDenied from SQS SendMessage), and to 'NO_RESOURCE' only when the target itself is not there (SQS's HTTP 400 AWS.SimpleQueueService.NonExistentQueue refusal). ERROR_CODE and the sibling ERROR_MESSAGE always name the same class. That both checks exercise the authorization class is confirmed by their own controls: the correctly scoped rule/queue pair in the same stack receives the event, and in the second check granting the missing policy makes the very same queue receive a later event — so the queue existed and was addressed correctly throughout.

Emulator: the dead letter does arrive on the DeadLetterConfig queue, once, with the event body and RULE_ARN matching (asserted and passed in the second check), but ERROR_CODE is 'NO_RESOURCE' in both scenarios: `AssertionError: unexpected DLQ error code: 'NO_RESOURCE'` and `AssertionError: expected ERROR_CODE=NO_PERMISSIONS (missing SQS resource policy), got 'NO_RESOURCE'`. The emulator is therefore classifying an SQS authorization refusal on a live queue as a missing resource.

Why: a client that triages, routes or alarms on the EventBridge dead-letter taxonomy is told the target queue does not exist when the queue is one SetQueueAttributes away from working. A consumer branching on ERROR_CODE='NO_PERMISSIONS' to point an operator at the missing AWS::SQS::QueuePolicy — the commonest EventBridge->SQS misconfiguration — never fires, while one branching on 'NO_RESOURCE' deletes or stops re-driving a target that is healthy.

Fix: EventBridge's target-delivery failure classifier that fills the dead-letter ERROR_CODE/ERROR_MESSAGE message attributes, together with the SQS SendMessage refusal it consumes — both attributes must be filled from the one refusal so they can never name different classes. Note for the fixer: this classifier is already contested and must not be flipped globally without reading the neighbours. 20261115T153528Z-elbv2-events-sqs-an-eventbridge-sqs-target-with.md reports the same wrong code (paired with a 403 AccessDenied ERROR_MESSAGE) for a policy-less SQS target but does NOT carry these two checks' replay keys, while 20261111T033241Z-events-eventbridge-sqs-target-dead-letters-misclassify-an-unauthorized.md and 20261114T222509Z-events-a-cloudformation-deployed-eventbridge-sqs-target-with.md ask for 'NO_RESOURCE' with SQS's 400 NonExistentQueue message for a policy-less target. The behaviour these two checks require is the authorization class: a 403 AccessDenied refusal of SendMessage must yield ERROR_CODE='NO_PERMISSIONS'.

Traces: redshift__sqs.cfn-deploy-redshift-sqs-queue-policy-negative (check line 55), redshift__sqs.sqs-dead-letter-queue-captures-failed-eventbridge-delivery-positive (check line 61).

## Evidence
redshift__sqs.cfn-deploy-redshift-sqs-queue-policy-negative (check line 55):
  assert error_code == "NO_PERMISSIONS", f"unexpected DLQ error code: {error_code!r}"
  AssertionError: unexpected DLQ error code: 'NO_RESOURCE'

redshift__sqs.sqs-dead-letter-queue-captures-failed-eventbridge-delivery-positive (check line 61):
  assert error_code == "NO_PERMISSIONS", ("expected ERROR_CODE=NO_PERMISSIONS (missing SQS resource policy), got ...")
  AssertionError: expected ERROR_CODE=NO_PERMISSIONS (missing SQS resource policy), got 'NO_RESOURCE'

Expected vs actual, per dead-letter message attribute (ReceiveMessage on the DeadLetterConfig queue with MessageAttributeNames=['All']):
  expected ERROR_CODE = "NO_PERMISSIONS" (fixed enumerated token; no part of it comes from the request), with an ERROR_MESSAGE in the same class (SQS's 403 AccessDenied refusal of sqs:SendMessage on the target queue ARN, the ARN from state and the request id per call).
  actual   ERROR_CODE = "NO_RESOURCE" in both scenarios.
  RULE_ARN / body: matched — exactly one dead letter arrived, its body was the original event (detail carrying the check's token / cluster identifier) and RULE_ARN named the rule of the failed delivery.

What matched before the failure: in the CFN scenario the stack deployed and PutEvents reported FailedEntryCount 0; in the second scenario the rule, the DLQ grant scoped to the rule ARN and PutTargets with DeadLetterConfig + RetryPolicy{MaximumRetryAttempts:0, MaximumEventAgeInSeconds:60} were all accepted, the dead letter was captured within the budget, and its RULE_ARN assertion passed. Not reached because the runs stop on ERROR_CODE: the no-leak assertion on the unauthorized target queue, the positive control delivery to the correctly scoped queue, and the post-grant redelivery to the same queue.

## Steps to Reproduce
1. CreateQueue twice: a dead-letter queue and a target queue. SetQueueAttributes a Policy on the DLQ allowing Principal {Service: events.amazonaws.com} sqs:SendMessage with Condition ArnEquals aws:SourceArn = the rule ARN. Leave the TARGET queue either with no policy at all, or with a policy whose aws:SourceArn condition names a DIFFERENT rule.
2. PutRule on a bus with an EventPattern matching a custom source/detail-type, State ENABLED.
3. PutTargets with one target: {Id:'1', Arn: the unauthorized target queue, DeadLetterConfig:{Arn: the DLQ}, RetryPolicy:{MaximumRetryAttempts:0, MaximumEventAgeInSeconds:60}}; FailedEntryCount must be 0.
4. PutEvents one entry matching the pattern.
5. ReceiveMessage on the DLQ with MessageAttributeNames=['All'].
   Expected: exactly one dead letter whose body is the original event, with RULE_ARN = the rule, TARGET_ARN = the unauthorized queue, ERROR_CODE = 'NO_PERMISSIONS' and an ERROR_MESSAGE in that same class (403 AccessDenied on sqs:SendMessage).
   Emulator: ERROR_CODE = 'NO_RESOURCE'.
6. ReceiveMessage on the target queue — it must hold nothing. Then SetQueueAttributes to grant events.amazonaws.com sqs:SendMessage for this rule and PutEvents again: the same queue must now receive the event, proving the queue existed all along and only the missing permission suppressed delivery.
