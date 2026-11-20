Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: redshift__sqs.sqs-dead-letter-queue-captures-failed-eventbridge-delivery-positive redshift__sqs.cfn-deploy-redshift-sqs-queue-policy-negative
Issue-Key: redshift__sqs.sqs-dead-letter-queue-captures-failed-eventbridge-delivery-positive|run#17|AssertionError|assert error_code == "NO_PERMISSIONS", (
Issue-Key: redshift__sqs.cfn-deploy-redshift-sqs-queue-policy-negative|run#13|AssertionError|assert error_code == "NO_PERMISSIONS", f"unexpected DLQ error code: {error_code!r}"
Issue-Commit: 8cc3611ff22f59380400560d7db928143fbfc872

# redshift__sqs: an EventBridge SQS target whose queue policy does not authorize events.amazonaws.com dead-letters with ERROR_CODE='NO_RESOURCE' where AWS stamps 'NO_PERMISSIONS'

> _Note: this issue was identified against the code base at commit `8cc3611ff22f59380400560d7db928143fbfc872`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutTargets

## Description
One net-new root cause, grounded in two failing checks (both listed by the suite under `redshift__sqs`).

Cause 1 of 1: when an EventBridge rule target is an SQS queue that EXISTS but whose resource policy does not authorize events.amazonaws.com to sqs:SendMessage for that rule, the dead-letter copy EventBridge writes to the target's DeadLetterConfig queue is stamped with the message attribute ERROR_CODE='NO_RESOURCE' instead of AWS's 'NO_PERMISSIONS'. The delivery failure is an authorization failure, and the emulator classifies it as a missing-resource failure. Failing checks: redshift__sqs.sqs-dead-letter-queue-captures-failed-eventbridge-delivery-positive (target queue created with no policy at all) and redshift__sqs.cfn-deploy-redshift-sqs-queue-policy-negative (CloudFormation-deployed target queue whose policy names only the OTHER rule's aws:SourceArn, so it exists and refuses this rule).

AWS: recorded behavior only — EventBridge's dead-letter message attributes are not modelled. Smithy (eventbridge) documents Target$DeadLetterConfig only as "The DeadLetterConfig that defines the target queue to send dead-letter queue events to" and no shape enumerates ERROR_CODE's values, so the tokens come from the recording. The recordings behind both checks show that a dead letter for an SQS target whose queue exists but authorizes events.amazonaws.com for nothing carries ERROR_CODE='NO_PERMISSIONS' — a fixed enumerated token with no part taken from the request — alongside RULE_ARN and TARGET_ARN naming the rule and the failed target (values from state) and a body that is the original event verbatim (same source, detail-type and detail). AWS keeps ERROR_CODE and ERROR_MESSAGE in the same class: 'NO_PERMISSIONS' goes with SQS's authorization refusal (HTTP 403, error code AccessDenied), while 'NO_RESOURCE' is the class for SQS's own not-found refusal, 'The specified queue does not exist or you do not have access to it. (Service: AmazonSQS; Status Code: 400; Error Code: AWS.SimpleQueueService.NonExistentQueue; Request ID: <id>; Proxy: null)' — all fixed text with the Request ID the only per-call part.

Emulator: the dead letter does arrive on the DeadLetterConfig queue, with the correct body (the token in `detail` matched) and the correct RULE_ARN (asserted and matched before the failure), but ERROR_CODE is 'NO_RESOURCE' in both checks even though the target queue was created by the check itself and exists throughout — in the second check it even has a policy, just one scoped to a different rule's aws:SourceArn. So the emulator reports 'the queue is not there' for a queue that is there and refused the principal. Because both checks stop on ERROR_CODE, the no-leak assertion on the unpermitted target queue and the post-grant redelivery leg were not reached in this run.

Why: a client that triages, routes, alarms on or re-drives from the EventBridge dead-letter taxonomy is told the wrong class. A consumer branching on ERROR_CODE='NO_PERMISSIONS' to point an operator at the missing AWS::SQS::QueuePolicy — the commonest EventBridge→SQS misconfiguration — never fires; a consumer branching on 'NO_RESOURCE' to delete a stale target or stop re-driving acts on a queue that in fact exists and is one SetQueueAttributes away from working. The CloudFormation check makes this concrete: the only broken thing in the stack is a queue policy scoped to the wrong SourceArn, and the taxonomy hides that.

Fix: EventBridge's target-delivery failure classifier that fills the dead-letter ERROR_CODE/ERROR_MESSAGE message attributes, together with the SQS SendMessage refusal it consumes — a 403/AccessDenied refusal from SQS must map to ERROR_CODE='NO_PERMISSIONS' and a 400/AWS.SimpleQueueService.NonExistentQueue refusal to 'NO_RESOURCE', and both attributes must be filled from that one refusal so they can never name different classes.

NOTE for the fixer — this classifier has been reported in BOTH directions from other check groups, so do not flip it globally. Open issues that do NOT carry these two checks' replay keys and therefore do not cover this cause: 20261115T153528Z-elbv2-events-sqs-an-eventbridge-sqs-target-with.md (same direction as this issue: NO_RESOURCE emitted where NO_PERMISSIONS is required, and paired with a 403 AccessDenied ERROR_MESSAGE), 20261115T200918Z-cloudwatch-events-sqs-an-alarm-configuration-change-delivery.md, 20261115T165311Z-events-the-eventbridge-sqs-target-dead-letter-error.md (ERROR_MESSAGE wording), and 20261111T033241Z-events-eventbridge-sqs-target-dead-letters-misclassify-an-unauthorized.md (the OPPOSITE direction, asking for NO_RESOURCE with SQS's 400 NonExistentQueue message). Making ERROR_CODE and ERROR_MESSAGE derive from one refusal closes this without choosing a side arbitrarily.

Traces: redshift__sqs.sqs-dead-letter-queue-captures-failed-eventbridge-delivery-positive; redshift__sqs.cfn-deploy-redshift-sqs-queue-policy-negative.

## Evidence
From the behavioral replay:
  [mismatch] redshift__sqs.sqs-dead-letter-queue-captures-failed-eventbridge-delivery-positive
    check line 61: `assert error_code == "NO_PERMISSIONS", ...`
    AssertionError: expected ERROR_CODE=NO_PERMISSIONS (missing SQS resource policy), got 'NO_RESOURCE'
  [mismatch] redshift__sqs.cfn-deploy-redshift-sqs-queue-policy-negative
    check line 55: `assert error_code == "NO_PERMISSIONS", f"unexpected DLQ error code: {error_code!r}"`
    AssertionError: unexpected DLQ error code: 'NO_RESOURCE'

Expected vs actual, per dead-letter message attribute (ReceiveMessage on the DeadLetterConfig queue with MessageAttributeNames=['All']):
  expected ERROR_CODE = "NO_PERMISSIONS"  — a fixed enumerated token; no part of it comes from the request
  actual   ERROR_CODE = "NO_RESOURCE"
  ERROR_MESSAGE must be in the same class as the code: the NO_PERMISSIONS-class message is SQS's authorization refusal (HTTP 403, Error Code AccessDenied) naming the events.amazonaws.com principal, the sqs:SendMessage action and the target queue's ARN — fixed text with the principal, action, queue ARN and Request ID the variable parts. The NO_RESOURCE-class message AWS pairs with that code is instead the fixed sentence 'The specified queue does not exist or you do not have access to it. (Service: AmazonSQS; Status Code: 400; Error Code: AWS.SimpleQueueService.NonExistentQueue; Request ID: <id>; Proxy: null)'.
  RULE_ARN: matched in the positive check (asserted immediately before ERROR_CODE) — the requirement is only that it names the rule whose delivery failed.

Everything before the ERROR_CODE assertion matched in both checks, which bounds the defect to the failure classifier:
  - positive check: the DLQ (policy-granted for the rule's ARN) received exactly one dead letter whose body parsed and whose detail.token equalled the token published by PutEvents, and whose RULE_ARN equalled the rule's ARN; the target queue in that check is created with NO resource policy and exists for the whole run.
  - CloudFormation check: the stack deployed a correctly scoped rule/queue pair and a mis-scoped pair (the bad queue's policy names only the other rule's aws:SourceArn), the event was published through PutEvents under a non-reserved source carrying the Redshift/CloudTrail envelope shape, and the dead letter reached the bad rule's DLQ.
In both cases the target queue EXISTS and the only defect in the scenario is the absent/mis-scoped resource policy, so 'NO_RESOURCE' cannot be AWS's class for it.

## Steps to Reproduce
Minimal reproduction (the positive check):
1. sqs CreateQueue twice: a DLQ and a target queue. Read both ARNs with GetQueueAttributes(AttributeNames=['QueueArn']).
2. events PutRule on the default bus, State ENABLED, EventPattern {"source":["sim2real.dlqtest"],"detail-type":["DLQ Capture Test"]}.
3. sqs SetQueueAttributes on the DLQ ONLY, with a Policy allowing Principal {"Service":"events.amazonaws.com"} sqs:SendMessage on the DLQ, Condition ArnEquals aws:SourceArn = the rule ARN. Leave the TARGET queue with no policy at all.
4. events PutTargets with one target: {Id:'1', Arn: the target queue ARN, DeadLetterConfig:{Arn: the DLQ ARN}, RetryPolicy:{MaximumRetryAttempts:0, MaximumEventAgeInSeconds:60}}. FailedEntryCount must be 0.
5. events PutEvents with one entry {Source:'sim2real.dlqtest', DetailType:'DLQ Capture Test', Detail: '{"token":"<token>"}'}.
6. sqs ReceiveMessage on the DLQ with MessageAttributeNames=['All'].
   Expected: one dead letter whose Body is the original event (detail.token == <token>), with RULE_ARN = the rule ARN and ERROR_CODE = 'NO_PERMISSIONS', and an ERROR_MESSAGE in that same class (SQS's 403/AccessDenied authorization refusal).
   Emulator: the dead letter and RULE_ARN are correct, but ERROR_CODE = 'NO_RESOURCE'.
7. sqs ReceiveMessage on the target queue — it must hold nothing.
8. SetQueueAttributes to grant the TARGET queue events.amazonaws.com sqs:SendMessage for this rule, re-issue the same PutEvents with a new token, and confirm the target queue now receives it — proving the queue existed and was addressed correctly all along and that only the missing permission suppressed delivery (so the class is a permissions class, not a resource class).

Second reproduction (the CloudFormation check): deploy one stack containing two rules and two queues — a correctly scoped rule/queue pair whose AWS::SQS::QueuePolicy names its own rule's aws:SourceArn, and a mis-scoped pair whose queue policy names only the OTHER rule's aws:SourceArn — each target carrying a DeadLetterConfig. PutEvents a 'AWS API Call via CloudTrail'-shaped envelope under a non-reserved source matching the mis-scoped rule, then read the bad rule's DLQ: ERROR_CODE must be 'NO_PERMISSIONS', while the correctly scoped pair in the same stack delivers normally.

Replay: redshift__sqs.sqs-dead-letter-queue-captures-failed-eventbridge-delivery-positive and redshift__sqs.cfn-deploy-redshift-sqs-queue-policy-negative.
