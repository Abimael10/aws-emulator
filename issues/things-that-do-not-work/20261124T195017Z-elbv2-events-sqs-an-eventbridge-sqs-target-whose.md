Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elbv2__events__sqs.xsvc-elbv2-1-unpermitted-queue-target-routes-event-to-dead-letter-queue
Issue-Key: elbv2__events__sqs.xsvc-elbv2-1-unpermitted-queue-target-routes-event-to-dead-letter-queue|run#21|AssertionError|assert attrs.get("ERROR_CODE") == "NO_PERMISSIONS", (
Issue-Commit: d50969a6e6efc58b2521aa7f9ba160b669c146a6

# elbv2__events__sqs: an EventBridge SQS target whose queue policy grants events.amazonaws.com nothing dead-letters with ERROR_CODE 'NO_RESOURCE' instead of AWS's 'NO_PERMISSIONS'

> _Note: this issue was identified against the code base at commit `d50969a6e6efc58b2521aa7f9ba160b669c146a6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutEvents

## Description
One net-new root cause, grounded in one failing check the suite lists under `elbv2__events__sqs`. The check is not marked ALREADY FILED or OWNED in this run.

Cause 1 of 1: EventBridge's target-delivery failure classifier stamps the dead-letter message attribute ERROR_CODE='NO_RESOURCE' for a target SQS queue that exists but grants EventBridge no permission, where this recording shows AWS stamping ERROR_CODE='NO_PERMISSIONS'.
AWS: an SQS rule target is authorized solely by the destination queue's resource policy; when that policy grants events.amazonaws.com nothing, EventBridge gives up on the delivery (here with RetryPolicy MaximumRetryAttempts=0) and writes the original event to the target's DeadLetterConfig queue, stamped with message attributes TARGET_ARN (the failed target's ARN, a value from state), RULE_ARN (the rule's ARN, a value from state) and ERROR_CODE — a fixed enumerated token, nothing in it derived from the request. The EventBridge dead-letter message attributes are not modelled (Smithy events#PutEvents carries no dead-letter shape), so their values are recorded behavior only: this recording shows ERROR_CODE='NO_PERMISSIONS' for a policy-less same-account SQS target. The body is the unmodified event envelope (source 'aws.elasticloadbalancing', detail-type 'AWS API Call via CloudTrail'), a permitted sibling target on the same rule receives the event, the unpermitted queue itself receives nothing, and granting that queue sqs:SendMessage for the rule afterwards restores delivery — proving the queue existed and was addressed correctly all along and that only the missing permission suppressed delivery.
Emulator: the dead letter is produced on the right DLQ with the right body, TARGET_ARN and RULE_ARN, but it carries ERROR_CODE='NO_RESOURCE' with ERROR_MESSAGE='The specified queue does not exist or you do not have access to it. (Service: AmazonSQS; Status Code: 400; Error Code: AWS.SimpleQueueService.NonExistentQueue; Request ID: <id>; Proxy: null)' — i.e. the emulator resolves an unauthorized send into SQS's not-found class and classifies it as a missing resource rather than a missing permission. NOTE for the fixer: resolved issue 20261114T222509Z-events-a-cloudformation-deployed-eventbridge-sqs-target-with.md drove this classifier the other way (it recorded NO_RESOURCE for a CloudFormation-deployed policy-less target), and open issue 20261111T033241Z-events-eventbridge-sqs-target-dead-letters-misclassify-an-unauthorized.md covers the same classifier from other checks without carrying this check's replay key. That earlier fix appears to have been applied globally: this recording of a directly-wired (PutRule/PutTargets) policy-less target requires NO_PERMISSIONS. Do NOT simply flip the classifier back — reconcile the two recordings so each scenario keeps its own recorded ERROR_CODE, and re-replay BOTH checks.
Why: ERROR_CODE is the whole point of the EventBridge dead-letter taxonomy. A client that triages, alarms on, or re-drives from a DLQ branches on it: NO_PERMISSIONS says 'grant the queue policy', NO_RESOURCE says 'the target is gone, re-point the rule'. With the wrong token every such branch — and every consumer grepping for the permission class — is dead code for the commonest EventBridge→SQS misconfiguration, and the operator is sent to re-create a queue that exists.
Fix: EventBridge's target-delivery failure classifier that fills the dead-letter ERROR_CODE/ERROR_MESSAGE attributes, together with the SQS SendMessage authorization refusal it consumes — the classifier must distinguish an existing-but-unauthorized target (NO_PERMISSIONS) from a target that genuinely does not resolve (NO_RESOURCE), rather than collapsing both into the not-found class.
Traces: elbv2__events__sqs.xsvc-elbv2-1-unpermitted-queue-target-routes-event-to-dead-letter-queue.

## Evidence
elbv2__events__sqs.xsvc-elbv2-1-unpermitted-queue-target-routes-event-to-dead-letter-queue (check line 353):
  assert attrs.get("ERROR_CODE") == "NO_PERMISSIONS"
  AssertionError: DLQ ERROR_CODE: expected 'NO_PERMISSIONS', actual 'NO_RESOURCE'

All dead-letter message attributes the emulator served (ReceiveMessage on the DLQ with MessageAttributeNames=['All']):
  TARGET_ARN    = the unpermitted target queue's ARN            — correct (names the failed target)
  RULE_ARN      = the rule's ARN                                 — correct (names the failed delivery's rule)
  ERROR_CODE    = 'NO_RESOURCE'                                  — expected 'NO_PERMISSIONS'
  ERROR_MESSAGE = 'The specified queue does not exist or you do not have access to it. (Service: AmazonSQS; Status Code: 400; Error Code: AWS.SimpleQueueService.NonExistentQueue; Request ID: <id>; Proxy: null)' — the not-found sentence, served for a queue that exists; every word fixed except the per-call Request ID

Everything before the ERROR_CODE assertion matched: PutTargets accepted both targets (FailedEntryCount 0), the channel was proven live, the permitted sibling target received the CreateTargetGroup event, and a dead-letter copy of that event reached the DLQ.

## Steps to Reproduce
1. Create three SQS queues: <bare> (create it with NO Policy attribute and never set one — the single broken thing), <dlq> and <permitted>.
2. PutRule on the default bus with State ENABLED and an EventPattern matching the CloudTrail API-call events of a control-plane call (e.g. source ['aws.elasticloadbalancing'], detail-type ['AWS API Call via CloudTrail'], detail.eventSource ['elasticloadbalancing.amazonaws.com'], detail.eventName ['CreateTargetGroup']).
3. PutTargets on that rule with two entries: {Id 'noperm', Arn <bare arn>, DeadLetterConfig {Arn <dlq arn>}, RetryPolicy {MaximumRetryAttempts 0, MaximumEventAgeInSeconds 60}} and {Id 'permitted', Arn <permitted arn>}. Expect FailedEntryCount 0.
4. SetQueueAttributes a Policy on <dlq> and <permitted> only, allowing Principal {Service events.amazonaws.com} sqs:SendMessage on that queue with Condition ArnEquals aws:SourceArn = the rule ARN. Leave <bare> with no policy.
5. Make the matching call (CreateTargetGroup) so one event flows, and confirm <permitted> receives it.
6. ReceiveMessage on <dlq> with MessageAttributeNames=['All'].
   Expected: ERROR_CODE='NO_PERMISSIONS', TARGET_ARN=<bare arn>, RULE_ARN=the rule ARN, and the body the unmodified event envelope.
   Emulator: ERROR_CODE='NO_RESOURCE' with ERROR_MESSAGE carrying SQS's AWS.SimpleQueueService.NonExistentQueue sentence.
7. <bare> must hold no message.
8. Repair only the permission: SetQueueAttributes the same allow policy on <bare>, make the call again, and <bare> must now receive the event (proving it existed all along).
