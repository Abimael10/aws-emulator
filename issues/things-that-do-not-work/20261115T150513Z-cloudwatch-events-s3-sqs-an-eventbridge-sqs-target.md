Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__events__s3__sqs.xsvc-s3-2-eventbridge-target-without-queue-policy-dead-letters-and-counts-failures-until-fixed
Issue-Key: cloudwatch__events__s3__sqs.xsvc-s3-2-eventbridge-target-without-queue-policy-dead-letters-and-counts-failures-until-fixed|run#33|AssertionError|assert "AWS.SimpleQueueService.NonExistentQueue" in (attrs.get("ERROR_MESSAGE") or ""), (
Issue-Commit: 58859c14105019dd1602a64ae7de76ec6c79e390

# cloudwatch__events__s3__sqs: an EventBridge SQS-target dead letter carries an AccessDenied/403 ERROR_MESSAGE instead of SQS's AWS.SimpleQueueService.NonExistentQueue refusal

> _Note: this issue was identified against the code base at commit `58859c14105019dd1602a64ae7de76ec6c79e390`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendMessage

## Description
One net-new root cause, grounded in one failing check. It is the un-fixed half of open issue 20261111T033241Z-events-eventbridge-sqs-target-dead-letters-misclassify-an-unauthorized.md: that issue asked for BOTH the dead-letter ERROR_CODE and its ERROR_MESSAGE; the ERROR_CODE half now behaves (the check's ERROR_CODE=='NO_RESOURCE' assertion passes), so the check's replay key moved to the next assertion and the suite no longer reports it as owned. The ERROR_MESSAGE half is still wrong, so it is filed here as its own work item. Related open issues that touch the same classifier but are NOT this key: 20261114T222509Z-events-a-cloudformation-deployed-eventbridge-sqs-target-with.md and 20261103T130707Z-redshift-sqs-eventbridge-dead-letters-a-delivery-its.md.

Cause 1 of 1: for an EventBridge rule whose SQS target queue exists but whose resource policy does not authorize events.amazonaws.com, the dead-letter message's ERROR_MESSAGE attribute carries an emulator-authored 403 AccessDenied sentence in SQS's name instead of SQS's own NonExistentQueue refusal. Failing check: cloudwatch__events__s3__sqs.xsvc-s3-2-eventbridge-target-without-queue-policy-dead-letters-and-counts-failures-until-fixed.

AWS: EventBridge assumes no role for an SQS target — the SendMessage is authorized solely by the destination queue's resource policy, and EventBridge dead-letters the event carrying the refusal the queue itself produced. Smithy (sqs, SendMessage): the modeled errors are InvalidAddress, InvalidMessageContents, InvalidSecurity, KmsAccessDenied, KmsDisabled, KmsInvalidKeyUsage, KmsInvalidState, KmsNotFound, KmsOptInRequired, KmsThrottled, QueueDoesNotExist, RequestThrottled and UnsupportedOperation — there is NO AccessDenied/AccessDeniedException on SendMessage at all. QueueDoesNotExist is httpError 400 with awsQueryError code `AWS.SimpleQueueService.NonExistentQueue`; i.e. SQS hides a queue the caller may not write to behind the same not-found class rather than admitting an authorization failure. The EventBridge dead-letter message attributes themselves are not modelled, so their values are recorded behavior: ERROR_CODE='NO_RESOURCE' (a fixed enumerated token) and ERROR_MESSAGE is the rendered SQS exception, 'The specified queue does not exist or you do not have access to it. (Service: AmazonSQS; Status Code: 400; Error Code: AWS.SimpleQueueService.NonExistentQueue; Request ID: <id>; Proxy: null)'. In that message every word of the sentence and every parenthetical field (Service: AmazonSQS, Status Code: 400, Error Code: AWS.SimpleQueueService.NonExistentQueue, Proxy: null) is FIXED text; the only variable part is the Request ID, the per-invocation id SQS minted. The requirement is that the dead letter relay SQS's refusal code and status, not a value from any one run.

Emulator: the event IS dead-lettered, with the right body, RULE_ARN, TARGET_ARN and the right ERROR_CODE='NO_RESOURCE', but ERROR_MESSAGE is 'User: events.amazonaws.com is not authorized to perform: sqs:sendmessage on resource: <target queue ARN> because no resource-based policy allows the sqs:sendmessage action (Service: AmazonSQS; Status Code: 403; Error Code: AccessDenied; Request ID: <id>; Proxy: null)'. That names Status Code: 403 and Error Code: AccessDenied — an error SQS does not model on SendMessage — and it lower-cases the action token to 'sqs:sendmessage' where IAM action names are 'sqs:SendMessage'. So the underlying SQS SendMessage authorization path is refusing with 403/AccessDenied where AWS refuses with 400/AWS.SimpleQueueService.NonExistentQueue, and the classifier copies that invented sentence into the dead letter.

Why: a client that triages the EventBridge DLQ reads ERROR_MESSAGE, not just ERROR_CODE. Operators and replay tooling grep it for SQS's NonExistentQueue code (and branch on 'Status Code: 400'); against this emulator that branch is dead code, and automation that distinguishes 'queue gone / not writable' from a genuine AccessDenied (a role-assumed target, or KMS on an encrypted queue) mis-routes the commonest EventBridge->SQS misconfiguration. Any client calling SQS SendMessage directly against a queue it is not authorized for likewise sees a 403 AccessDenied the SDK does not model, instead of the modeled QueueDoesNotExist it must handle.

Fix: SQS's SendMessage authorization/validation path must raise the modeled QueueDoesNotExist (HTTP 400, code AWS.SimpleQueueService.NonExistentQueue, with AWS's fixed sentence) when the send is refused only because the queue's resource policy does not allow it, and EventBridge's target-delivery failure classifier that fills the dead-letter message attributes must copy that rendered refusal verbatim into ERROR_MESSAGE alongside the already-correct ERROR_CODE='NO_RESOURCE' instead of authoring its own AccessDenied sentence. Caution for the fixer: do not flip the message globally — a target whose service really answers AccessDenied (a role-assumed target, or KMS on an encrypted queue) keeps its AccessDenied refusal and NO_PERMISSIONS class.

Traces: cloudwatch__events__s3__sqs.xsvc-s3-2-eventbridge-target-without-queue-policy-dead-letters-and-counts-failures-until-fixed. Note the check stops at this assertion, so its later steps — AWS/Events FailedInvocations for rule B reaching >= 1, Invocations for rule A, FailedInvocations==0 for rule A, and delivery to rule B's queue after the policy is attached — were never exercised and are unverified by this run.

## Evidence
Failing check cloudwatch__events__s3__sqs.xsvc-s3-2-eventbridge-target-without-queue-policy-dead-letters-and-counts-failures-until-fixed, at the ERROR_MESSAGE assertion:

  assert "AWS.SimpleQueueService.NonExistentQueue" in (attrs.get("ERROR_MESSAGE") or "")
  AssertionError: dead letter ERROR_MESSAGE does not carry SQS's NonExistentQueue refusal:
  'User: events.amazonaws.com is not authorized to perform: sqs:sendmessage on resource: arn:aws:sqs:<region>:<account>:<target-queue> because no resource-based policy allows the sqs:sendmessage action (Service: AmazonSQS; Status Code: 403; Error Code: AccessDenied; Request ID: <id>; Proxy: null)'

Expected vs actual, per dead-letter message attribute:
  ERROR_CODE     expected 'NO_RESOURCE'                                   actual 'NO_RESOURCE'  (OK — this half now behaves)
  ERROR_MESSAGE  expected SQS's fixed NonExistentQueue sentence:           actual an emulator-authored AccessDenied sentence:
                 'The specified queue does not exist or you do not have    'User: events.amazonaws.com is not authorized to perform:
                  access to it. (Service: AmazonSQS; Status Code: 400;      sqs:sendmessage on resource: <queue ARN> because no
                  Error Code: AWS.SimpleQueueService.NonExistentQueue;      resource-based policy allows the sqs:sendmessage action
                  Request ID: <id>; Proxy: null)'                          (Service: AmazonSQS; Status Code: 403; Error Code:
                                                                           AccessDenied; Request ID: <id>; Proxy: null)'
  TARGET_ARN / RULE_ARN  present and correct (per-run values; they only have to name the failed delivery's target and rule).

Fixed vs variable text in the expected ERROR_MESSAGE: the sentence 'The specified queue does not exist or you do not have access to it.' and the fields 'Service: AmazonSQS', 'Status Code: 400', 'Error Code: AWS.SimpleQueueService.NonExistentQueue' and 'Proxy: null' are FIXED; only 'Request ID' is a per-run value. In the emulator's message the diverging requirements are the status (403 vs 400), the error code (AccessDenied vs AWS.SimpleQueueService.NonExistentQueue) and the whole sentence; the queue ARN in it is a value from the request/state, and the request id is per-run.

Smithy (services/service-com-amazonaws-sqs-amazonsqs): SendMessage errors = InvalidAddress(404), InvalidMessageContents(400), InvalidSecurity(403), KmsAccessDenied(400), KmsDisabled(400), KmsInvalidKeyUsage(400), KmsInvalidState(400), KmsNotFound(400), KmsOptInRequired(403), KmsThrottled(400), QueueDoesNotExist(400, awsQueryError code AWS.SimpleQueueService.NonExistentQueue), RequestThrottled(403), UnsupportedOperation(400). AccessDenied is not among them.

## Steps to Reproduce
1. CreateBucket and PutBucketNotificationConfiguration with EventBridgeConfiguration={} so the bucket emits to EventBridge.
2. CreateQueue three times: QA (rule A's target), QB (rule B's target) and DLQ. SetQueueAttributes a Policy on QA and on DLQ allowing Principal {Service: events.amazonaws.com} Action sqs:SendMessage on that queue, conditioned on aws:SourceArn = the owning rule's ARN. Leave QB with NO resource policy.
3. PutRule twice (rule A and rule B) with the same pattern {"source":["aws.s3"],"detail-type":["Object Created"],"detail":{"bucket":{"name":[<bucket>]}}}, State=ENABLED.
4. PutTargets rule A -> QA. PutTargets rule B -> QB with RetryPolicy {MaximumRetryAttempts: 0, MaximumEventAgeInSeconds: 60} and DeadLetterConfig.Arn = the DLQ ARN.
5. PutObject into the bucket so one Object Created event matches both rules.
6. ReceiveMessage on the DLQ with MessageAttributeNames=['All'].
   Expected: one message whose attributes are ERROR_CODE='NO_RESOURCE', ERROR_MESSAGE='The specified queue does not exist or you do not have access to it. (Service: AmazonSQS; Status Code: 400; Error Code: AWS.SimpleQueueService.NonExistentQueue; Request ID: <id>; Proxy: null)', TARGET_ARN=QB's ARN, RULE_ARN=rule B's ARN.
   Emulator: ERROR_CODE='NO_RESOURCE' (correct) but ERROR_MESSAGE='User: events.amazonaws.com is not authorized to perform: sqs:sendmessage on resource: <QB ARN> because no resource-based policy allows the sqs:sendmessage action (Service: AmazonSQS; Status Code: 403; Error Code: AccessDenied; Request ID: <id>; Proxy: null)'.
7. Direct-call variant, no EventBridge needed: as a principal with no grant on QB, call SQS SendMessage(QueueUrl=QB). Expected HTTP 400 with error code AWS.SimpleQueueService.NonExistentQueue; the emulator answers 403 AccessDenied, an error SendMessage does not model.
