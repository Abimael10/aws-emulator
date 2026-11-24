Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__events__sqs.xsvc-cloudwatch-2-alarm-change-event-denied-by-queue-policy-dead-letters-and-counts-failed-invocations
Issue-Key: cloudwatch__events__sqs.xsvc-cloudwatch-2-alarm-change-event-denied-by-queue-policy-dead-letters-and-counts-failed-invocations|run#29|AssertionError|assert _attr(m, "ERROR_CODE") == "NO_RESOURCE", (
Issue-Commit: 12324ebe9a30c68b02ff3d648ddb0d8f8dd552a1

# cloudwatch__events__sqs: an alarm-configuration-change delivery to an SQS target whose policy explicitly denies EventBridge dead-letters with ERROR_CODE='NO_PERMISSIONS'/AccessDenied instead of 'NO_RESOURCE' with SQS's NonExistentQueue refusal

> _Note: this issue was identified against the code base at commit `12324ebe9a30c68b02ff3d648ddb0d8f8dd552a1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutEvents

## Description
Cause 1 of 1: a rule whose SQS target exists but whose queue policy carries an explicit Deny for events.amazonaws.com is dead-lettered with ERROR_CODE='NO_PERMISSIONS' and an emulator-authored 403 AccessDenied ERROR_MESSAGE, where AWS dead-letters it with ERROR_CODE='NO_RESOURCE' carrying SQS's own NonExistentQueue refusal sentence. Failing check: cloudwatch__events__sqs.xsvc-cloudwatch-2-alarm-change-event-denied-by-queue-policy-dead-letters-and-counts-failed-invocations.

AWS: EventBridge assumes no role for an SQS target — the send is authorized solely by the destination queue's resource policy, and the refusal the queue itself produces is what EventBridge classifies into the dead-letter attributes. Smithy (sqs): SendMessage models InvalidAddress, InvalidMessageContents, InvalidSecurity, the KMS.* family, QueueDoesNotExist, RequestThrottled and UnsupportedOperation — there is NO AccessDenied/AccessDeniedException on SendMessage at all; QueueDoesNotExist is httpError 400 with awsQueryError code AWS.SimpleQueueService.NonExistentQueue, i.e. SQS hides a queue it will not let the caller write to behind the same not-found class. The EventBridge dead-letter message attributes are not modelled, so their values are recorded behavior: ERROR_CODE='NO_RESOURCE' (a fixed enumerated token — nothing in it comes from the request) and ERROR_MESSAGE='The specified queue does not exist or you do not have access to it. (Service: AmazonSQS; Status Code: 400; Error Code: AWS.SimpleQueueService.NonExistentQueue; Request ID: <id>; Proxy: null)', where every word is fixed text except the Request ID minted per invocation. RULE_ARN and TARGET_ARN name the failed delivery's rule and target (values from state), and with RetryPolicy.MaximumRetryAttempts=0 the event is dead-lettered once.

Emulator: the event IS dead-lettered to the configured DLQ with the right body, RULE_ARN and TARGET_ARN, but the attributes read ERROR_CODE='NO_PERMISSIONS' and ERROR_MESSAGE='User: events.amazonaws.com is not authorized to perform: sqs:sendmessage on resource: <target queue ARN> because no resource-based policy allows the sqs:sendmessage action (Service: AmazonSQS; Status Code: 403; Error Code: AccessDenied; Request ID: <id>; Proxy: null)' — a 403 AccessDenied in SQS's name for an error SQS does not model on SendMessage, with a lower-cased 'sqs:sendmessage' action token, which then drives the wrong ERROR_CODE class. The check stops at that first assertion, before its FailedInvocations-per-rule and post-fix-redelivery assertions.

Why: a client that triages, routes or alarms on the EventBridge dead-letter taxonomy reads the wrong class for the commonest EventBridge->SQS misconfiguration; any consumer branch keyed on NO_RESOURCE, or on SQS's NonExistentQueue sentence that operators grep for, is dead code against this emulator, and replay tooling that re-drives only NO_RESOURCE letters re-drives nothing.

Fix: the EventBridge target-delivery failure classifier that fills the dead-letter ERROR_CODE/ERROR_MESSAGE attributes, together with the SQS SendMessage authorization refusal it consumes — an unauthorized send (no policy at all, or an explicit Deny) must surface as SQS's QueueDoesNotExist (400 / AWS.SimpleQueueService.NonExistentQueue / that fixed sentence) and be classified NO_RESOURCE, while a target whose service genuinely answers AccessDenied (a role-assumed target, or KMS on an encrypted queue) stays NO_PERMISSIONS. Named for the fixer: open issue 20261111T033241Z-events-eventbridge-sqs-target-dead-letters-misclassify-an-unauthorized.md describes the same classifier defect from four OTHER checks (filed under events / events__lambda__sqs / events__s3__sqs / cloudwatch__events__s3__sqs) and does not carry this check's replay key, so this check is not re-replayed by it; fixing that classifier should close both, but this key must be replayed to confirm it — the event here is produced natively by CloudWatch's alarm-configuration-change emission rather than by PutEvents, so it exercises the classifier on a different producer path.

Traces: cloudwatch__events__sqs.xsvc-cloudwatch-2-alarm-change-event-denied-by-queue-policy-dead-letters-and-counts-failed-invocations.

## Evidence
Expected (AWS), dead-letter message attributes for the denied delivery:
  ERROR_CODE    = "NO_RESOURCE"
  ERROR_MESSAGE = "The specified queue does not exist or you do not have access to it. (Service: AmazonSQS; Status Code: 400; Error Code: AWS.SimpleQueueService.NonExistentQueue; Request ID: <id>; Proxy: null)"  (all fixed text; Request ID is the only per-invocation part)
  RULE_ARN      = the failing rule's ARN, TARGET_ARN = the denied target queue's ARN (values from state)

Actual ([mismatch] cloudwatch__events__sqs.xsvc-cloudwatch-2-alarm-change-event-denied-by-queue-policy-dead-letters-and-counts-failed-invocations, run line 234):
  assert _attr(m, "ERROR_CODE") == "NO_RESOURCE"
  AssertionError: the DLQ copy carries ERROR_CODE='NO_PERMISSIONS', expected 'NO_RESOURCE' for an SQS target whose policy denies events.amazonaws.com (ERROR_MESSAGE='User: events.amazonaws.com is not authorized to perform: sqs:sendmessage on resource: arn:aws:sqs:us-west-2:000000000000:sim2real-xsvc-cw2-tgt-15b9f6fc2b because no resource-based policy allows the sqs:sendmessage action (Service: AmazonSQS; Status Code: 403; Error Code: AccessDenied; Request ID: 7992f156-372e-4584-ad45-a2c735eb27ca; Proxy: null)')

The queue name/ARN, account id and Request ID in that actual message are per-run values and are not part of the requirement; the requirement is the fixed ERROR_CODE token and the fixed NonExistentQueue sentence with its 400 status and AWS.SimpleQueueService.NonExistentQueue code.

## Steps to Reproduce
1. sqs.CreateQueue three times: a rule-target queue, a dead-letter queue and a control queue.
2. events.PutRule twice (State=ENABLED) with EventPattern {"source":["aws.cloudwatch"],"detail-type":["CloudWatch Alarm Configuration Change"],"detail":{"alarmName":[<alarm>]}} — a 'bad' rule and a healthy control rule.
3. sqs.SetQueueAttributes: on the TARGET queue a policy whose single Statement has Effect=Deny for Principal {"Service":"events.amazonaws.com"}, Action sqs:SendMessage, Condition ArnEquals aws:SourceArn=<bad rule ARN>; on the DLQ and the control queue the same statement with Effect=Allow for their respective rules.
4. events.PutTargets on the bad rule with the target queue ARN, RetryPolicy {MaximumRetryAttempts: 0, MaximumEventAgeInSeconds: 60} and DeadLetterConfig.Arn = the DLQ ARN; events.PutTargets on the control rule with the control queue ARN.
5. cloudwatch.PutMetricAlarm on that alarm name (re-put with a fresh threshold) so one 'CloudWatch Alarm Configuration Change' event reaches both rules at once.
6. sqs.ReceiveMessage on the DLQ with MessageAttributeNames=['All'].
   Expected: one dead letter with ERROR_CODE='NO_RESOURCE' and the SQS NonExistentQueue ERROR_MESSAGE (400 / AWS.SimpleQueueService.NonExistentQueue), RULE_ARN=<bad rule>, TARGET_ARN=<target queue>.
   Emulator: ERROR_CODE='NO_PERMISSIONS' with a 403 AccessDenied ERROR_MESSAGE naming action 'sqs:sendmessage'.
7. Replay cloudwatch__events__sqs.xsvc-cloudwatch-2-alarm-change-event-denied-by-queue-policy-dead-letters-and-counts-failed-invocations.
