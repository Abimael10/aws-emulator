Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events.deploy-cfn-target-dead-letter-config-captures-undeliverable-events-until-a-stack-update-adds-the-queue-policy
Issue-Shape-Id: com.amazonaws.eventbridge#AWSEvents
Issue-Key: events.deploy-cfn-target-dead-letter-config-captures-undeliverable-events-until-a-stack-update-adds-the-queue-policy|_receive#32|Timeout|for m in sqs.receive_message(QueueUrl=url, MaxNumberOfMessages=10, WaitTimeSeconds=5,
Issue-Commit: e9606f7cc5c12500a2de4a220302f880c5ee8e5a

# events: the CloudFormation-deployed EventBridge target DeadLetterConfig scenario now reaches no verdict at all — the stack/dead-letter sequence makes no observable progress and is cut off at the replay ceiling

> _Note: this issue was identified against the code base at commit `e9606f7cc5c12500a2de4a220302f880c5ee8e5a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutEvents

## Description
One net-new root cause, grounded in the one failing check the suite lists under `events`.

NOTE for the fixer: two open issues already describe value-level divergences reached BY this same check — 20261114T222509Z-events-a-cloudformation-deployed-eventbridge-sqs-target-with.md (dead-letter ERROR_CODE 'NO_PERMISSIONS' instead of NO_RESOURCE) and 20261115T165311Z-events-the-eventbridge-sqs-target-dead-letter-error.md (dead-letter ERROR_MESSAGE is an emulator-authored 403 AccessDenied sentence instead of SQS's 400 NonExistentQueue sentence). The suite does NOT report this check as owned by either, because it no longer fails on either of those assertions: it now produces no verdict whatsoever. That is a distinct, earlier-stage cause and is filed here. A fixer must make the scenario progress again before either of those issues can be confirmed fixed.

Cause 1 of 1: the whole CloudFormation-deployed dead-letter scenario makes no observable progress and the check is killed at the 60 s replay ceiling, so not one of its stages is reached. AWS: CreateStack over a template of AWS::Events::EventBus + two AWS::SQS::Queue + AWS::Events::Rule (with Targets[0].RetryPolicy and Targets[0].DeadLetterConfig) + an AWS::SQS::QueuePolicy on the DLQ only settles to CREATE_COMPLETE and exposes its Outputs; ListTargetsByRule then reads the target back with RetryPolicy {MaximumRetryAttempts:3, MaximumEventAgeInSeconds:3600} and DeadLetterConfig {Arn:<DLQ ARN>}; a matching PutEvents (Smithy events: PutEvents returns FailedEntryCount and Entries[].EventId — FailedEntryCount must be 0 for an accepted entry) is dead-lettered once on the DLQ with message attributes ERROR_CODE='NO_RESOURCE', ERROR_MESSAGE='The specified queue does not exist or you do not have access to it. (Service: AmazonSQS; Status Code: 400; Error Code: AWS.SimpleQueueService.NonExistentQueue; Request ID: <id>; Proxy: null)' (all fixed text except the per-call request id SQS minted), RULE_ARN and TARGET_ARN naming the failed delivery's rule and target (values from state), while the policy-less target queue stays empty; an UpdateStack that only adds the target queue's AWS::SQS::QueuePolicy settles to UPDATE_COMPLETE without replacing the rule or the queue, after which the same PutEvents is delivered to the target queue. The dead-letter message attributes themselves are not modeled, so their values are recorded behavior only. Emulator: the run produces no verdict — the replay reports the check only as 'Check timed out after 60.0s', with no assertion reached, so the stack never settled to CREATE_COMPLETE with its Outputs, or the dead letter never arrived on the DLQ, inside the window where this scenario previously got as far as asserting ERROR_CODE/ERROR_MESSAGE. The two issues named above record that it used to reach the DLQ with the right body, right RetryPolicy/DeadLetterConfig and a wrong ERROR_CODE/ERROR_MESSAGE; it no longer gets there at all. Why: every client-visible contract in this scenario is now unobservable rather than merely wrong — a client cannot deploy an EventBridge rule with a DeadLetterConfig via CloudFormation and see either the dead letter or, after adding the queue policy, restored delivery. A dead-letter consumer or redrive tool built on it has nothing to read, and the known classification defects cannot even be confirmed fixed because the check can no longer reach their assertions. Fix: whichever stage of this path stalls — CloudFormation's convergence of the AWS::Events::EventBus / AWS::Events::Rule (with target RetryPolicy + DeadLetterConfig) / AWS::SQS::Queue / AWS::SQS::QueuePolicy stack to CREATE_COMPLETE with its Outputs, and EventBridge's target-delivery failure path that must write the dead letter to the configured DeadLetterConfig queue promptly — must make progress; the ERROR_CODE/ERROR_MESSAGE classifier behind it is already filed in the two issues named above and should be closed together with this one.

Traces: events.deploy-cfn-target-dead-letter-config-captures-undeliverable-events-until-a-stack-update-adds-the-queue-policy.

## Evidence
events.deploy-cfn-target-dead-letter-config-captures-undeliverable-events-until-a-stack-update-adds-the-queue-policy:
  replay verdict: [mismatch] — `Error: Check timed out after 60.0s`. No assertion message of any kind is reported, so no stage of the scenario returned a comparable value.

Expected sequence, in order, none of which was observed to complete:
  1. CreateStack -> DescribeStacks StackStatus 'CREATE_COMPLETE' with Outputs QueueUrl/QueueArn/DlqUrl/DlqArn/RuleArn.
  2. ListTargetsByRule(Rule=<rule>, EventBusName=<bus>) -> exactly 1 target; Arn == Outputs.QueueArn; RetryPolicy == {MaximumRetryAttempts: 3, MaximumEventAgeInSeconds: 3600}; DeadLetterConfig == {Arn: Outputs.DlqArn}.
  3. PutEvents(one matching entry on <bus>) -> FailedEntryCount 0 with an EventId.
  4. ReceiveMessage(DlqUrl, MessageAttributeNames=['All']) -> the event body with id == that EventId, ERROR_CODE 'NO_RESOURCE', ERROR_MESSAGE 'The specified queue does not exist or you do not have access to it. (Service: AmazonSQS; Status Code: 400; Error Code: AWS.SimpleQueueService.NonExistentQueue; Request ID: <id>; Proxy: null)' (fixed text; only Request ID varies per call), RULE_ARN == Outputs.RuleArn, TARGET_ARN == Outputs.QueueArn; and ReceiveMessage(QueueUrl) empty.
  5. UpdateStack adding the target AWS::SQS::QueuePolicy -> 'UPDATE_COMPLETE' with RuleArn and QueueArn unchanged, then PutEvents delivered to QueueUrl.

Actual: the check is terminated at the harness's 60 s cap with no verdict. Contrast with the previously recorded emulator behavior on this same check (open issues 20261114T222509Z-... and 20261115T165311Z-...), where stages 1-3 passed and stage 4 failed on ERROR_CODE='NO_PERMISSIONS' / the 403 AccessDenied ERROR_MESSAGE — the scenario no longer reaches that point.

## Steps to Reproduce
1. CreateStack with a template containing: AWS::Events::EventBus (Name=<bus>); two AWS::SQS::Queue (Queue, Dlq); AWS::Events::Rule (Name=<rule>, EventBusName={Ref Bus}, EventPattern {"source":[<source>]}, State ENABLED, Targets=[{Id:'queue', Arn:{Fn::GetAtt:[Queue,Arn]}, RetryPolicy{MaximumRetryAttempts:3,MaximumEventAgeInSeconds:3600}, DeadLetterConfig{Arn:{Fn::GetAtt:[Dlq,Arn]}}}]); AWS::SQS::QueuePolicy allowing events.amazonaws.com sqs:SendMessage on the Dlq ONLY; Outputs QueueUrl/QueueArn/DlqUrl/DlqArn/RuleArn.
2. Poll DescribeStacks until StackStatus == CREATE_COMPLETE (expected well inside the replay budget) and read the Outputs.
3. ListTargetsByRule(Rule=<rule>, EventBusName=<bus>) and check the single target's Arn, RetryPolicy and DeadLetterConfig.
4. PutEvents one entry matching the rule's source on <bus>; expect FailedEntryCount 0.
5. ReceiveMessage on DlqUrl with MessageAttributeNames=['All'] and compare ERROR_CODE / ERROR_MESSAGE / RULE_ARN / TARGET_ARN as listed in the evidence; confirm QueueUrl holds no message.
6. UpdateStack with the same template plus an AWS::SQS::QueuePolicy on Queue; wait UPDATE_COMPLETE (RuleArn and QueueArn unchanged) and PutEvents again — the event must now reach QueueUrl.
   Emulator today: the sequence does not get through these stages within the replay window and the check is killed with no verdict.
