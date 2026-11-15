Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events.queue-policy-deny-and-missing-allow-both-dead-letter-as-no-resource-without-retries events.deploy-cfn-target-dead-letter-config-captures-undeliverable-events-until-a-stack-update-adds-the-queue-policy
Issue-Shape-Id: com.amazonaws.eventbridge#AWSEvents
Issue-Key: events.queue-policy-deny-and-missing-allow-both-dead-letter-as-no-resource-without-retries|run#30|AssertionError|assert _shaped(attrs["ERROR_MESSAGE"]) == _NON_EXISTENT_QUEUE, (
Issue-Key: events.deploy-cfn-target-dead-letter-config-captures-undeliverable-events-until-a-stack-update-adds-the-queue-policy|run#21|AssertionError|assert shaped == _NON_EXISTENT_QUEUE, f"dead-letter ERROR_MESSAGE was {shaped!r}"
Issue-Commit: 78517b75c0e3f9eb9fe3d5d92b3abb1be57beaa5

# events: the EventBridge SQS-target dead-letter ERROR_MESSAGE is still an emulator-authored AccessDenied/403 sentence instead of SQS's NonExistentQueue refusal

> _Note: this issue was identified against the code base at commit `78517b75c0e3f9eb9fe3d5d92b3abb1be57beaa5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutEvents

## Description
One net-new root cause, grounded in two failing checks the suite lists under `events`. Open issue 20261111T033241Z-events-eventbridge-sqs-target-dead-letters-misclassify-an-unauthorized.md describes the same area and names one of these check ids, but the suite does not report either check as owned by it: its ERROR_CODE half now passes (the dead letter is classified NO_RESOURCE) while the ERROR_MESSAGE half still diverges, so the two checks now fail on a later assertion and are filed here as the surviving divergence. A fixer should close both together.

Cause 1 of 1: when a rule's SQS target queue exists but its resource policy does not authorize events.amazonaws.com (no policy at all, or an explicit Deny), the dead letter's ERROR_MESSAGE attribute is an emulator-authored IAM-style AccessDenied sentence with Status Code 403 and Error Code AccessDenied, where AWS records the refusal SQS itself returned.
Failing checks: events.queue-policy-deny-and-missing-allow-both-dead-letter-as-no-resource-without-retries; events.deploy-cfn-target-dead-letter-config-captures-undeliverable-events-until-a-stack-update-adds-the-queue-policy.

AWS: EventBridge assumes no role for an SQS target — the send is authorized solely by the destination queue's resource policy, and what EventBridge writes into the dead letter is the error the queue returned. Smithy (sqs): SendMessage's modeled errors are InvalidAddress, InvalidMessageContents, InvalidSecurity, the KMS.* family, QueueDoesNotExist, RequestThrottled and UnsupportedOperation — there is NO AccessDenied/AccessDeniedException on SendMessage at all, so a 403 AccessDenied in SQS's name is not a refusal SQS can produce. QueueDoesNotExist is httpError 400 with awsQueryError code `AWS.SimpleQueueService.NonExistentQueue`: SQS hides a queue it will not let the caller write to behind the not-found class. The dead-letter message attributes themselves are not modelled, so their values are recorded behavior only: ERROR_MESSAGE = `The specified queue does not exist or you do not have access to it. (Service: AmazonSQS; Status Code: 400; Error Code: AWS.SimpleQueueService.NonExistentQueue; Request ID: <id>; Proxy: null)` — every word is fixed text except `<id>`, the per-call request id SQS minted (a per-run value, not part of the requirement). ERROR_CODE is the fixed token `NO_RESOURCE`, RULE_ARN and TARGET_ARN name the failed delivery's rule and target (values from state), and the event is dead-lettered once with no retries.

Emulator: the event IS dead-lettered with the right ERROR_CODE, body, RULE_ARN and TARGET_ARN, but ERROR_MESSAGE = `User: events.amazonaws.com is not authorized to perform: sqs:sendmessage on resource: <queue ARN> because no resource-based policy allows the sqs:sendmessage action (Service: AmazonSQS; Status Code: 403; Error Code: AccessDenied; Request ID: <id>; Proxy: null)`. That sentence attributes to AmazonSQS a 403 AccessDenied the service does not model on SendMessage, and lower-cases the action token to `sqs:sendmessage` where an IAM-style sentence would name `sqs:SendMessage`.

Why: ERROR_MESSAGE is the only field that tells an operator or a redrive tool WHY a delivery failed. A consumer that greps SQS's NonExistentQueue sentence (the documented symptom of the commonest EventBridge→SQS misconfiguration) matches nothing here, and a consumer that parses the status/code pair out of the message reads 403/AccessDenied where AWS reports 400/AWS.SimpleQueueService.NonExistentQueue — so triage, alarming and automated redrive all branch wrongly. It also contradicts the emulator's own ERROR_CODE=NO_RESOURCE on the same message, leaving the dead letter internally inconsistent.

Fix: the EventBridge target-delivery failure path that fills the dead-letter ERROR_MESSAGE attribute, together with the SQS SendMessage authorization refusal it records — an unauthorized send must surface as SQS's QueueDoesNotExist (400 / AWS.SimpleQueueService.NonExistentQueue / that fixed sentence) and be recorded verbatim, with only the request id varying. Note the related, separately filed divergence in SQS's absent-queue sentence (filed under `sqs` in this sweep): the not-authorized sentence here and the plainly-absent sentence `The specified queue does not exist.` are two distinct fixed texts under the same error code and must not be collapsed. CAUTION: do not flip the classifier globally — a target whose service genuinely answers AccessDenied (a role-assumed target, or KMS on an encrypted queue) keeps its AccessDenied message and NO_PERMISSIONS code; the discriminator is which refusal the target service actually returns.

Traces: events.queue-policy-deny-and-missing-allow-both-dead-letter-as-no-resource-without-retries (assertion at line 68); events.deploy-cfn-target-dead-letter-config-captures-undeliverable-events-until-a-stack-update-adds-the-queue-policy (assertion at line 49).

## Evidence
Expected vs actual on the dead letter's ERROR_MESSAGE message attribute (identical in both checks; the request id is shaped to <id> by the check before comparison):
  expected = "The specified queue does not exist or you do not have access to it. (Service: AmazonSQS; Status Code: 400; Error Code: AWS.SimpleQueueService.NonExistentQueue; Request ID: <id>; Proxy: null)"
  actual   = "User: events.amazonaws.com is not authorized to perform: sqs:sendmessage on resource: <queue ARN> because no resource-based policy allows the sqs:sendmessage action (Service: AmazonSQS; Status Code: 403; Error Code: AccessDenied; Request ID: <id>; Proxy: null)"
Fixed text in the requirement: the whole sentence plus the parenthesised Service/Status Code/Error Code/Proxy fields. Variable: only Request ID (per-run). The queue ARN that appears in the emulator's sentence is a per-run value and has no place in this message class at all.

events.queue-policy-deny-and-missing-allow-both-dead-letter-as-no-resource-without-retries (line 68):
  assert _shaped(attrs["ERROR_MESSAGE"]) == _NON_EXISTENT_QUEUE
  AssertionError: the denied target's ERROR_MESSAGE was 'User: events.amazonaws.com is not authorized to perform: sqs:sendmessage on resource: arn:aws:sqs:<region>:<acct>:<queue> because no resource-based policy allows the sqs:sendmessage action (Service: AmazonSQS; Status Code: 403; Error Code: AccessDenied; Request ID: <id>; Proxy: null)'
  (the check's earlier ERROR_CODE == "NO_RESOURCE" assertion now passes; this later message assertion is where it stops, before the no-retry, healthy-sibling and Deny→Allow restoration arms)

events.deploy-cfn-target-dead-letter-config-captures-undeliverable-events-until-a-stack-update-adds-the-queue-policy (line 49), where rule, target queue, DLQ, RetryPolicy {MaximumRetryAttempts: 3, MaximumEventAgeInSeconds: 3600} and DeadLetterConfig all come from a CloudFormation stack deployed WITHOUT the target queue policy:
  assert shaped == _NON_EXISTENT_QUEUE
  AssertionError: dead-letter ERROR_MESSAGE was 'User: events.amazonaws.com is not authorized to perform: sqs:sendmessage on resource: arn:aws:sqs:<region>:<acct>:<stack queue> because no resource-based policy allows the sqs:sendmessage action (Service: AmazonSQS; Status Code: 403; Error Code: AccessDenied; Request ID: <id>; Proxy: null)'
  (it stops here, before the stack-update arm that adds the queue policy and expects delivery to resume)

## Steps to Reproduce
1. CreateQueue twice: a dead-letter queue (grant it a policy allowing events.amazonaws.com sqs:SendMessage) and a rule-target queue left with NO resource policy. Repeat the whole scenario with a target-queue policy whose Statement explicitly Denies events.amazonaws.com — both must behave identically.
2. CreateEventBus (or use the default bus); PutRule with an event pattern on a unique source; PutTargets with the target queue's ARN plus DeadLetterConfig.Arn = the DLQ ARN and a RetryPolicy.
3. PutEvents with one matching entry.
4. ReceiveMessage on the DLQ with MessageAttributeNames=['All'].
   Expected: exactly one dead letter, ERROR_CODE='NO_RESOURCE' and ERROR_MESSAGE='The specified queue does not exist or you do not have access to it. (Service: AmazonSQS; Status Code: 400; Error Code: AWS.SimpleQueueService.NonExistentQueue; Request ID: <id>; Proxy: null)', with RULE_ARN/TARGET_ARN naming the rule and target, and no further attempts.
   Emulator: ERROR_CODE='NO_RESOURCE' (correct) but ERROR_MESSAGE is the 403 AccessDenied sentence naming 'sqs:sendmessage' and the queue ARN.
5. SetQueueAttributes to grant the target queue events.amazonaws.com sqs:SendMessage and PutEvents again — delivery succeeds, proving the queue existed all along and only authorization had failed.
6. Same via CloudFormation: deploy a stack containing the bus, rule (with RetryPolicy and DeadLetterConfig), target queue and DLQ but no QueuePolicy on the target, PutEvents, read the DLQ, then update the stack to add the QueuePolicy and confirm delivery resumes.
