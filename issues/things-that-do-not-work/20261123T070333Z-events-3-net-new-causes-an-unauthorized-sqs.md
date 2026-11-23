Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events.queue-policy-deny-and-missing-allow-both-dead-letter-as-no-resource-without-retries events.deploy-cfn-event-bus-policy-admits-the-named-role-and-a-stack-update-removing-it-denies-again events.deploy-cfn-schedule-rule-fires-into-its-target-and-a-stack-update-disabling-it-stops-firing
Issue-Shape-Id: com.amazonaws.eventbridge#AWSEvents
Issue-Key: events.queue-policy-deny-and-missing-allow-both-dead-letter-as-no-resource-without-retries|run#29|AssertionError|assert attrs["ERROR_CODE"] == "NO_RESOURCE", f"an explicit queue-policy Deny dead-lettered as {attrs['ERROR_CODE']!r}"
Issue-Key: events.deploy-cfn-event-bus-policy-admits-the-named-role-and-a-stack-update-removing-it-denies-again|run#30|AssertionError|assert after["BusArn"] == bus_arn and after["QueueUrl"] == out["QueueUrl"], (
Issue-Key: sqs|ReceiveMessage|AWS.SimpleQueueService.NonExistentQueue
Issue-Commit: 97a2ab1025745e797dcfe05b920fe05efd92486d

# events: 3 net-new causes — an unauthorized SQS target dead-letters as NO_PERMISSIONS/403 AccessDenied instead of NO_RESOURCE/NonExistentQueue, and UpdateStack replaces resources whose properties did not change

> _Note: this issue was identified against the code base at commit `97a2ab1025745e797dcfe05b920fe05efd92486d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutEvents

## Description
Three net-new root causes, grounded in three failing checks the suite lists under `events`. Causes 2 and 3 are plausibly the same stack-update defect but are counted separately because each is proved by a different observation and must each be re-replayed; all three must be closed before this issue is submitted.

Cause 1 of 3: the dead letter EventBridge writes for an SQS target the destination queue's resource policy does not authorize is classified NO_PERMISSIONS with an emulator-authored 403 AccessDenied sentence, where AWS classifies it NO_RESOURCE with SQS's NonExistentQueue refusal. Failing check: events.queue-policy-deny-and-missing-allow-both-dead-letter-as-no-resource-without-retries.
AWS: EventBridge assumes no role for an SQS target — the send is authorized only by the destination queue's resource policy, and the dead letter records the refusal SQS itself returned. Smithy (sqs): SendMessage's modeled errors are InvalidAddress, InvalidMessageContents, InvalidSecurity, the KMS.* family, QueueDoesNotExist, RequestThrottled and UnsupportedOperation — there is NO AccessDenied on SendMessage, so a 403 AccessDenied in SQS's name is not a refusal SQS can produce; QueueDoesNotExist is httpError 400 with awsQueryError code `AWS.SimpleQueueService.NonExistentQueue` (SQS hides a queue the caller may not write to behind the not-found class). The dead-letter message attributes are not modeled, so their values are recorded behavior only: ERROR_CODE is the fixed token `NO_RESOURCE`; ERROR_MESSAGE is the fixed sentence `The specified queue does not exist or you do not have access to it. (Service: AmazonSQS; Status Code: 400; Error Code: AWS.SimpleQueueService.NonExistentQueue; Request ID: <id>; Proxy: null)` — every word fixed except `<id>`, the per-call request id (a per-run value, not part of the requirement); RULE_ARN and TARGET_ARN carry the failed delivery's rule and target ARNs from state. This holds identically for a queue with no policy at all and for a policy whose Statement explicitly Denies events.amazonaws.com.
Emulator: the event is dead-lettered, but ERROR_CODE='NO_PERMISSIONS' and ERROR_MESSAGE='User: events.amazonaws.com is not authorized to perform: sqs:SendMessage on resource: <queue ARN> because no resource-based policy allows the sqs:SendMessage action (Service: AmazonSQS; Status Code: 403; Error Code: AccessDenied; Request ID: <id>; Proxy: null)'. The queue ARN interpolated into that sentence is a per-run value that has no place in this message class at all.
Why: ERROR_CODE and ERROR_MESSAGE are the only fields that tell an operator or a redrive tool why a delivery failed. A consumer that branches on NO_RESOURCE (the documented symptom of the commonest EventBridge→SQS misconfiguration) never fires, and one that parses the status/code pair out of the message reads 403/AccessDenied where AWS reports 400/AWS.SimpleQueueService.NonExistentQueue.
Fix: the EventBridge target-delivery failure path that classifies a failed send and fills the dead-letter ERROR_CODE/ERROR_MESSAGE attributes, together with the SQS SendMessage authorization refusal it records. CAUTION: do not flip the classifier globally — a target whose service genuinely answers AccessDenied (a role-assumed target, or KMS on an encrypted queue) keeps AccessDenied/NO_PERMISSIONS; the discriminator is which refusal the target service actually returns. Context: fixed issue 20261111T033241Z-events-eventbridge-sqs-target-dead-letters-misclassify-an-unauthorized.md claimed this exact classification, and open issue 20261115T165311Z-events-the-eventbridge-sqs-target-dead-letter-error.md records that its ERROR_CODE half had started passing while only ERROR_MESSAGE diverged; this run shows NO_PERMISSIONS again, so the code half has regressed and the check now stops on the earlier assertion (line 67) rather than the message assertion that issue keys on.

Cause 2 of 3: a stack update that removes only an AWS::Events::EventBusPolicy resource replaces the stack's event bus and/or its SQS queue instead of leaving them untouched. Failing check: events.deploy-cfn-event-bus-policy-admits-the-named-role-and-a-stack-update-removing-it-denies-again.
AWS: recorded behavior only (CloudFormation's update semantics are not expressed in the Smithy model). An UpdateStack whose only template difference is the deletion of one resource performs no action on the remaining resources: their physical ids are stable, so the stack Outputs built from `{"Ref": "Bus"}`/`Fn::GetAtt Bus.Arn` and `{"Ref": "Queue"}` read back byte-identical before and after UPDATE_COMPLETE, and the pre-update bus ARN and queue URL stay live.
Emulator: after UPDATE_COMPLETE on the policy-removing template, the stack's BusArn and/or QueueUrl output no longer equals the value read after CREATE_COMPLETE — the update replaced a resource whose properties did not change.
Why: every client that captures a physical id from a stack output (a queue URL it polls, a bus ARN it publishes to) is silently pointed at a resource that no longer exists after an unrelated update, and the data in the replaced resource is lost. It also makes the deployed-policy negative arm unreachable: the check cannot tell a denied PutEvents from one aimed at a bus that was swapped out.
Fix: the CloudFormation change-set/update engine's per-resource diff — a resource whose template properties are unchanged must be left in place (no replacement, same physical id) when another resource is added or removed from the template.

Cause 3 of 3: the SQS queue a stack created stops existing across the stack's lifetime — ReceiveMessage on the queue URL the stack reports in its Outputs is refused AWS.SimpleQueueService.NonExistentQueue. Failing check: events.deploy-cfn-schedule-rule-fires-into-its-target-and-a-stack-update-disabling-it-stops-firing.
AWS: recorded behavior only. `{"Ref": "<AWS::SQS::Queue>"}` resolves to the queue's URL, and that URL serves ReceiveMessage for as long as the stack holds the queue; an update that changes only AWS::Events::Rule State=ENABLED→DISABLED touches neither the queue nor its URL, so the same URL keeps serving before and after UPDATE_COMPLETE.
Emulator: ReceiveMessage on the stack's QueueUrl output raises QueueDoesNotExist (AWS.SimpleQueueService.NonExistentQueue, 'The specified queue does not exist.'), so the check errors out instead of reaching a verdict. Hypothesis: the same update-replacement defect as Cause 2 destroys the queue behind an unrelated rule-state update (the identical symptom appears in cloudformation__events__sqs, filed separately under that service); the alternative is that the Ref/PhysicalResourceId the stack publishes for an AWS::SQS::Queue declared with empty Properties is not a usable queue URL.
Why: a stack-deployed queue is unusable from the outside — nothing can poll the target of a deployed rule, so no deploy-then-verify flow works against the emulator.
Fix: the CloudFormation AWS::SQS::Queue resource provider — its create/update handling and the physical resource id (queue URL) it publishes through Ref and stack Outputs, plus the update diff named in Cause 2.

Traces: events.queue-policy-deny-and-missing-allow-both-dead-letter-as-no-resource-without-retries (assertion at line 67); events.deploy-cfn-event-bus-policy-admits-the-named-role-and-a-stack-update-removing-it-denies-again (assertion at line 62); events.deploy-cfn-schedule-rule-fires-into-its-target-and-a-stack-update-disabling-it-stops-firing (ReceiveMessage inside the receive helper).

## Evidence
Cause 1 — events.queue-policy-deny-and-missing-allow-both-dead-letter-as-no-resource-without-retries, line 67:
  assert attrs["ERROR_CODE"] == "NO_RESOURCE"
  AssertionError: an explicit queue-policy Deny dead-lettered as 'NO_PERMISSIONS'
Expected dead-letter attributes for a target queue that denies / does not allow events.amazonaws.com:
  ERROR_CODE  = 'NO_RESOURCE' (fixed token)
  ERROR_MESSAGE = 'The specified queue does not exist or you do not have access to it. (Service: AmazonSQS; Status Code: 400; Error Code: AWS.SimpleQueueService.NonExistentQueue; Request ID: <id>; Proxy: null)' — all fixed text except the per-run Request ID
Actual:
  ERROR_CODE  = 'NO_PERMISSIONS'
  ERROR_MESSAGE = 'User: events.amazonaws.com is not authorized to perform: sqs:SendMessage on resource: <target queue ARN> because no resource-based policy allows the sqs:SendMessage action (Service: AmazonSQS; Status Code: 403; Error Code: AccessDenied; Request ID: <id>; Proxy: null)'
Smithy (sqs) SendMessage errors: InvalidAddress(404), InvalidMessageContents(400), InvalidSecurity(403), KMS.*(400/403), QueueDoesNotExist(400, awsQueryError AWS.SimpleQueueService.NonExistentQueue), RequestThrottled(403), UnsupportedOperation(400). No AccessDenied is modeled.

Cause 2 — events.deploy-cfn-event-bus-policy-admits-the-named-role-and-a-stack-update-removing-it-denies-again, line 62:
  assert after["BusArn"] == bus_arn and after["QueueUrl"] == out["QueueUrl"]
  AssertionError: the update replaced the bus or the queue instead of removing only the policy
Template difference between the two deploys is exactly one resource (the AWS::Events::EventBusPolicy) being absent; expected: both outputs byte-identical across CREATE_COMPLETE → UPDATE_COMPLETE.

Cause 3 — events.deploy-cfn-schedule-rule-fires-into-its-target-and-a-stack-update-disabling-it-stops-firing:
  botocore.errorfactory.QueueDoesNotExist: An error occurred (AWS.SimpleQueueService.NonExistentQueue) when calling the ReceiveMessage operation: The specified queue does not exist.
The QueueUrl passed is the stack's own Output, `{"Ref": "Queue"}` of an AWS::SQS::Queue in the same stack (template also contains the rate(1 minute) rule with an InputTransformer target on that queue and an AWS::SQS::QueuePolicy allowing events.amazonaws.com). Expected: ReceiveMessage succeeds (returning the scheduled, input-transformed delivery) both before and after the State=DISABLED update.

## Steps to Reproduce
Cause 1:
1. CreateQueue three times: a DLQ (policy allowing events.amazonaws.com sqs:SendMessage), a target queue whose policy explicitly Denies events.amazonaws.com, and a second target queue left with NO policy.
2. CreateEventBus; PutRule with a pattern on a unique source; PutTargets for both target queues with RetryPolicy {MaximumRetryAttempts: 185, MaximumEventAgeInSeconds: 86400} and DeadLetterConfig.Arn = the DLQ, plus a healthy third target.
3. PutEvents one matching entry; ReceiveMessage the DLQ with MessageAttributeNames=['All'].
   Expected: one dead letter per failing target, ERROR_CODE='NO_RESOURCE', ERROR_MESSAGE = SQS's NonExistentQueue sentence (400 / AWS.SimpleQueueService.NonExistentQueue), RULE_ARN/TARGET_ARN naming the rule and target, and no retries.
   Emulator: ERROR_CODE='NO_PERMISSIONS' with the 403 AccessDenied sentence.
4. Replace the Deny with an Allow and PutEvents again — delivery must succeed, proving the queue existed all along.

Cause 2:
1. CreateStack with a template containing AWS::Events::EventBus, AWS::Events::Rule, AWS::SQS::Queue, AWS::SQS::QueuePolicy and an AWS::Events::EventBusPolicy admitting a bare IAM role, with Outputs BusArn = Fn::GetAtt Bus.Arn and QueueUrl = Ref Queue. Wait for CREATE_COMPLETE and record both outputs.
2. UpdateStack with the same template minus the EventBusPolicy resource only; wait for UPDATE_COMPLETE.
3. DescribeStacks and compare Outputs.
   Expected: BusArn and QueueUrl unchanged (only the policy was removed).
   Emulator: one or both changed — the bus and/or queue was replaced.

Cause 3:
1. CreateStack with a template containing AWS::SQS::Queue (empty Properties), an AWS::Events::Rule with ScheduleExpression rate(1 minute), State ENABLED and an InputTransformer target on that queue, an AWS::SQS::QueuePolicy allowing events.amazonaws.com, and Outputs QueueUrl = Ref Queue / RuleArn = Fn::GetAtt Rule.Arn. Wait for CREATE_COMPLETE.
2. ReceiveMessage on the QueueUrl output, then UpdateStack with State=DISABLED, wait for UPDATE_COMPLETE and ReceiveMessage again.
   Expected: both ReceiveMessage calls succeed on the same URL (deliveries before the update, none scheduled more than 60 s after it).
   Emulator: ReceiveMessage is refused AWS.SimpleQueueService.NonExistentQueue.
