Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events.deploy-cfn-target-dead-letter-config-captures-undeliverable-events-until-a-stack-update-adds-the-queue-policy
Issue-Shape-Id: com.amazonaws.eventbridge#AWSEvents
Issue-Key: events.deploy-cfn-target-dead-letter-config-captures-undeliverable-events-until-a-stack-update-adds-the-queue-policy|run#19|AssertionError|assert attrs.get("ERROR_CODE") == "NO_RESOURCE", f"dead-letter ERROR_CODE was {attrs.get('ERROR_CODE')!r}"
Issue-Commit: d3b7765ce05e53288156df4eb42460237bf11326

# events: a CloudFormation-deployed EventBridge SQS target with no queue policy dead-letters with ERROR_CODE='NO_PERMISSIONS' instead of AWS's NO_RESOURCE

> _Note: this issue was identified against the code base at commit `d3b7765ce05e53288156df4eb42460237bf11326`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutEvents

## Description
One net-new root cause, grounded in one failing check (listed by the suite under `events`).

Cause 1 of 1: a rule target deployed by CloudFormation (AWS::Events::Rule with RetryPolicy + DeadLetterConfig) whose destination queue has NO resource policy for events.amazonaws.com is dead-lettered with the dead-letter message attribute ERROR_CODE='NO_PERMISSIONS', where AWS dead-letters it as ERROR_CODE='NO_RESOURCE' carrying SQS's own NonExistentQueue refusal. Failing check: events.deploy-cfn-target-dead-letter-config-captures-undeliverable-events-until-a-stack-update-adds-the-queue-policy.

NOTE for the fixer: open issue 20261111T033241Z-events-eventbridge-sqs-target-dead-letters-misclassify-an-unauthorized.md describes the same misclassification from four other checks; it does not carry this check's replay key, so this check is filed here and will not be re-replayed by that issue. If that issue is fixed first, re-replay this check to close this one; it is the same classifier, now reached through a CloudFormation-deployed rule/target/DeadLetterConfig.

AWS: EventBridge assumes no role for an SQS target — the send is authorized solely by the destination queue's resource policy, and the refusal the queue returns is what EventBridge classifies. Smithy (sqs): SendMessage's modeled errors are InvalidAddress (404), InvalidMessageContents (400), InvalidSecurity (403), KMS.* (400/403), QueueDoesNotExist (httpError 400, awsQueryError code 'AWS.SimpleQueueService.NonExistentQueue'), RequestThrottled (403) and UnsupportedOperation (400) — there is NO AccessDenied/AccessDeniedException modeled on SendMessage at all; SQS hides a queue it will not let the caller write to behind the not-found class. The EventBridge dead-letter message attributes are not modeled, so their values are recorded behavior: ERROR_CODE='NO_RESOURCE' (a fixed enumerated token; nothing in it comes from the request), ERROR_MESSAGE='The specified queue does not exist or you do not have access to it. (Service: AmazonSQS; Status Code: 400; Error Code: AWS.SimpleQueueService.NonExistentQueue; Request ID: <id>; Proxy: null)' — every word fixed except the Request ID, the per-call id SQS minted — plus RULE_ARN and TARGET_ARN naming the failed delivery's rule and target (values from state). Granting the queue policy afterwards (here via a stack update that only adds AWS::SQS::QueuePolicy) restores delivery, proving the queue existed all along.

Emulator: the event IS dead-lettered on the stack's DLQ with the right body, and the deployed target reads back with the right RetryPolicy and DeadLetterConfig, but the dead letter carries ERROR_CODE='NO_PERMISSIONS' instead of 'NO_RESOURCE'. The check stops on that attribute, so the emulator's ERROR_MESSAGE, RULE_ARN/TARGET_ARN and the post-update redelivery were not reached in this run; the sibling checks in the issue named above show the ERROR_MESSAGE is likewise an emulator-authored 403 AccessDenied sentence rather than SQS's 400 NonExistentQueue sentence.

Why: a client that triages, alarms on or re-drives from the EventBridge DLQ taxonomy reads the wrong class for the commonest EventBridge->SQS misconfiguration (a stack that forgot its QueuePolicy). Any consumer branch keyed on NO_RESOURCE, or grepping SQS's NonExistentQueue sentence, is dead code against this emulator.

Fix: EventBridge's target-delivery failure classifier that fills the dead-letter ERROR_CODE/ERROR_MESSAGE attributes, together with the SQS SendMessage authorization refusal it consumes — an unauthorized send must surface as SQS's QueueDoesNotExist (400 / AWS.SimpleQueueService.NonExistentQueue / that fixed sentence) and be classified NO_RESOURCE, while a target whose service genuinely answers AccessDenied (a role-assumed target, or KMS on an encrypted queue) stays NO_PERMISSIONS. Do not flip the classifier globally.

Traces: events.deploy-cfn-target-dead-letter-config-captures-undeliverable-events-until-a-stack-update-adds-the-queue-policy.

## Evidence
events.deploy-cfn-target-dead-letter-config-captures-undeliverable-events-until-a-stack-update-adds-the-queue-policy (check line 47):
  assert attrs.get("ERROR_CODE") == "NO_RESOURCE", f"dead-letter ERROR_CODE was {attrs.get('ERROR_CODE')!r}"
  AssertionError: dead-letter ERROR_CODE was 'NO_PERMISSIONS'

Expected vs actual, per dead-letter message attribute (ReceiveMessage on the stack's DLQ with MessageAttributeNames=['All']):
  expected ERROR_CODE    = "NO_RESOURCE"                     actual "NO_PERMISSIONS"
  expected ERROR_MESSAGE = "The specified queue does not exist or you do not have access to it. (Service: AmazonSQS; Status Code: 400; Error Code: AWS.SimpleQueueService.NonExistentQueue; Request ID: <id>; Proxy: null)" — all fixed text, Request ID the only variable part (not asserted in this run; the check stopped on ERROR_CODE)
  expected RULE_ARN/TARGET_ARN = the deployed rule's ARN and the deployed target queue's ARN (per-run values, requirement is only that they name the failed delivery's rule and target)

Everything before the attribute check matched: the stack reached CREATE_COMPLETE, ListTargetsByRule returned exactly one target pointing at the stack's queue with RetryPolicy {MaximumRetryAttempts: 3, MaximumEventAgeInSeconds: 3600} and DeadLetterConfig {Arn: <DLQ arn>}, and the event reached the DLQ with the right body.

## Steps to Reproduce
1. CreateStack with a template containing: AWS::Events::EventBus (Name=<bus>); two AWS::SQS::Queue (Queue, Dlq); AWS::Events::Rule (Name=<rule>, EventBusName={Ref Bus}, EventPattern {"source":[<source>]}, State ENABLED, Targets=[{Id:'queue', Arn: GetAtt Queue.Arn, RetryPolicy {MaximumRetryAttempts:3, MaximumEventAgeInSeconds:3600}, DeadLetterConfig {Arn: GetAtt Dlq.Arn}}]); an AWS::SQS::QueuePolicy allowing events.amazonaws.com sqs:SendMessage on the DLQ ONLY (no policy on the target Queue). Wait for CREATE_COMPLETE.
2. ListTargetsByRule(Rule=<rule>, EventBusName=<bus>) — one target, RetryPolicy and DeadLetterConfig as declared (matches today).
3. PutEvents one matching entry on <bus> (FailedEntryCount 0).
4. ReceiveMessage on the DLQ with MessageAttributeNames=['All'].
   Expected: ERROR_CODE='NO_RESOURCE', ERROR_MESSAGE='The specified queue does not exist or you do not have access to it. (Service: AmazonSQS; Status Code: 400; Error Code: AWS.SimpleQueueService.NonExistentQueue; Request ID: <id>; Proxy: null)', RULE_ARN=the deployed rule, TARGET_ARN=the deployed queue.
   Emulator: ERROR_CODE='NO_PERMISSIONS'.
   The policy-less target queue must hold no message.
5. UpdateStack with the same template plus an AWS::SQS::QueuePolicy allowing events.amazonaws.com sqs:SendMessage on the target Queue; wait UPDATE_COMPLETE (rule and queue ARNs unchanged) and PutEvents again — the event must now be delivered to the target queue.
