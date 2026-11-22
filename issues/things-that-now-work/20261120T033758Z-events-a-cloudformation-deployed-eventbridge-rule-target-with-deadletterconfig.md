Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events.deploy-cfn-target-dead-letter-config-captures-undeliverable-events-until-a-stack-update-adds-the-queue-policy
Issue-Shape-Id: com.amazonaws.eventbridge#AWSEvents
Issue-Key: events.deploy-cfn-target-dead-letter-config-captures-undeliverable-events-until-a-stack-update-adds-the-queue-policy|_receive#32|Timeout|for m in sqs.receive_message(QueueUrl=url, MaxNumberOfMessages=10, WaitTimeSeconds=5,
Issue-Commit: 36c52b4dca18359d29d13b5f35377729e4798d85

# A CloudFormation-deployed EventBridge rule target with DeadLetterConfig never dead-letters when the target queue has no queue policy, so the deploy-path dead-letter contract is unobservable

> _Note: this issue was identified against the code base at commit `36c52b4dca18359d29d13b5f35377729e4798d85`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutTargets

## Description
One net-new root cause in the EventBridge target-delivery/dead-letter path, observed through the CloudFormation deploy surface. A closely related issue is already open for the direct (PutTargets) surface — 20261029T182228Z-events-eventbridge-does-not-authorize-sqs-targets-against-the.md — but this check was not bound to it, so it is filed here and should be resolved together with it.

Cause 1 of 1: an EventBridge rule target deployed with RetryPolicy + DeadLetterConfig, whose target SQS queue carries NO resource policy granting events.amazonaws.com sqs:SendMessage, produces neither a refusal-driven dead-letter nor (after a stack update adds the queue policy) a restored delivery; the check never observes a DLQ message and ends in a hard timeout.

AWS: EventBridge invokes an SQS target by calling sqs:SendMessage as the service principal events.amazonaws.com. With no matching Allow in the target queue's resource policy the call is refused, so the envelope is NOT enqueued on the target queue; because the target carries a DeadLetterConfig (Smithy: PutTargets.Targets[].DeadLetterConfig, member Arn -> ResourceArn @length 1..1600, "The ARN of the SQS queue specified as the target for the dead-letter queue"), the envelope is instead delivered to that DLQ with the message attributes RULE_ARN, TARGET_ARN, ERROR_CODE and ERROR_MESSAGE. For an authorization refusal ERROR_CODE is NO_RESOURCE and ERROR_MESSAGE is SQS's own error as EventBridge records it — fixed text "The specified queue does not exist or you do not have access to it. (Service: AmazonSQS; Status Code: 400; Error Code: AWS.SimpleQueueService.NonExistentQueue; Request ID: <id>; Proxy: null)", where only <id> is a per-call value (SQS answers an unauthorized principal as if the queue did not exist). The refusal is not retryable, so the RetryPolicy (Smithy: RetryPolicy.MaximumRetryAttempts @range 0..185, MaximumEventAgeInSeconds @range 60..86400) adds no attempts. RULE_ARN must equal the deployed rule's ARN and TARGET_ARN the deployed target queue's ARN, and the dead-lettered body is the original event envelope (its source is the request's Source). After a stack UPDATE that adds only an AWS::SQS::QueuePolicy allowing events.amazonaws.com sqs:SendMessage on that queue — leaving the rule and queue in place, not replaced — the next matching event is delivered to the target queue normally.

Emulator: the check ends "Error: Check timed out after 60.0s". No message carrying the PutEvents EventId ever reaches the DLQ, so the NO_RESOURCE dead-letter with its four attributes is never produced, and the check cannot even reach the post-update leg that proves delivery is restored. This matches the already-filed direct-surface symptom (no resource-policy authorization on SQS targets, and no dead-letter path at all), now reproduced for a stack-deployed rule/target. Hypothesis: the same missing authorization + dead-letter path is the cause here; a secondary possibility consistent with the same timeout is that the deployed stack (AWS::Events::EventBus, AWS::Events::Rule with inline Targets carrying RetryPolicy/DeadLetterConfig, AWS::SQS::Queue, AWS::SQS::QueuePolicy) does not settle to CREATE_COMPLETE, which the fixer should rule out first by replaying the check.

Why: a client that deploys least-privilege infrastructure and alarms on its DLQ sees silence where AWS gives it a NO_RESOURCE dead-letter naming the rule, the target and the SQS refusal. It cannot distinguish "nothing matched" from "delivery refused", cannot verify that adding the queue policy is what fixes delivery, and an IaC rollout that forgot the queue policy looks healthy.

Fix: resource-policy authorization in the EventBridge SQS-target invoker and its dead-letter path (the four documented message attributes, no retries on an authorization refusal), reachable for targets created through the CloudFormation AWS::Events::Rule provider as well as through PutTargets; and the AWS::SQS::QueuePolicy provider's in-place update so adding the policy restores delivery without replacing the rule or the queue.

Traces: events.deploy-cfn-target-dead-letter-config-captures-undeliverable-events-until-a-stack-update-adds-the-queue-policy

## Evidence
Check events.deploy-cfn-target-dead-letter-config-captures-undeliverable-events-until-a-stack-update-adds-the-queue-policy -> "Error: Check timed out after 60.0s".

Expected (AWS): after PutEvents of a matching entry, one message on the DeadLetterConfig queue whose message attributes are ERROR_CODE="NO_RESOURCE", ERROR_MESSAGE="The specified queue does not exist or you do not have access to it. (Service: AmazonSQS; Status Code: 400; Error Code: AWS.SimpleQueueService.NonExistentQueue; Request ID: <id>; Proxy: null)" (fixed text; <id> is the per-call request id, the only variable part), RULE_ARN = the deployed rule's ARN and TARGET_ARN = the deployed target queue's ARN; the body is the original envelope with source = the PutEvents Source; and ZERO messages on the policy-less target queue. Then, after an update adding the AWS::SQS::QueuePolicy (rule ARN and queue ARN unchanged), a subsequent matching event IS received on the target queue.

Actual (emulator): no message with the PutEvents EventId ever arrives on the DLQ; the receive loop runs until the 60s check deadline and the check is killed, so neither the dead-letter attributes nor the post-update restored delivery is ever observed.

## Steps to Reproduce
1) CreateStack with: AWS::Events::EventBus; two AWS::SQS::Queue (target + DLQ); AWS::Events::Rule on that bus with EventPattern {"source":[<source>]}, State ENABLED, and one inline target pointing at the target queue's ARN with RetryPolicy {MaximumRetryAttempts:3, MaximumEventAgeInSeconds:3600} and DeadLetterConfig {Arn: <dlq arn>}; an AWS::SQS::QueuePolicy allowing events.amazonaws.com sqs:SendMessage on the DLQ ONLY (no policy on the target queue). Wait for CREATE_COMPLETE.
2) ListTargetsByRule — the target reads back with the RetryPolicy and DeadLetterConfig above.
3) PutEvents one entry on that bus with the matching Source.
4) ReceiveMessage on the DLQ with MessageAttributeNames=["All"]: expected one message with ERROR_CODE=NO_RESOURCE, the SQS NonExistentQueue ERROR_MESSAGE, RULE_ARN and TARGET_ARN; observed nothing until the check deadline. ReceiveMessage on the target queue: expected empty.
5) UpdateStack with the same template plus an AWS::SQS::QueuePolicy granting events.amazonaws.com sqs:SendMessage on the TARGET queue, wait for UPDATE_COMPLETE (rule ARN and queue ARN unchanged), PutEvents again: expected the event on the target queue.
