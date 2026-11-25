Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__events__sqs.instance-state-change-notifications-are-delivered-per-state-to-their-own-rule
Issue-Key: ec2__events__sqs.instance-state-change-notifications-are-delivered-per-state-to-their-own-rule|_build#5|Timeout|time.sleep(_WIRING_SETTLE_S)
Issue-Commit: 9034a7f4323bec4093511276a5153556005621ed

# ec2__events__sqs: no "EC2 Instance State-change Notification" reaches an EventBridge rule's SQS target, so the per-state rule differential never reaches a verdict (check times out)

> _Note: this issue was identified against the code base at commit `9034a7f4323bec4093511276a5153556005621ed`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StopInstances

## Description
Cause 1 of 1: EC2 publishes no "EC2 Instance State-change Notification" envelope onto the EventBridge default bus, so an ENABLED rule whose target is an SQS queue receives nothing for any instance transition. The check that grades this — a broad rule matching every state of the subject instance against a narrow rule pinned to detail.state "shutting-down" — blocks on its first delivery barrier and is killed by the harness, so neither the positive deliveries nor the pinned-rule negative is ever evaluated. Failing check: ec2__events__sqs.instance-state-change-notifications-are-delivered-per-state-to-their-own-rule.

AWS: the EC2 Smithy model declares no event schema and no errors for the operations that drive the transitions — smithy_query on services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json reports `errors: []` for StopInstances, StartInstances and TerminateInstances, and their outputs carry only InstanceStateChangeList — so the eventing contract is recorded behavior only. What the recording shows: every instance state transition emits exactly one EventBridge envelope with `source` = "aws.ec2" and `detail-type` = "EC2 Instance State-change Notification". The envelope carries `account` = the calling account, `region` = the request's region, `resources` = exactly one element, the instance ARN (arn:aws:ec2:<region>:<account>:instance/<instance-id>), `detail.instance-id` = that instance id, `detail.state` = one of the six modelled values pending | running | stopping | stopped | shutting-down | terminated, and `time` = an ISO-8601 UTC instant (trailing Z) at which the transition happened. The fixed text here is the source and detail-type strings and the six state tokens; the account, region, instance id, ARN and time are values taken from the request and the emulator's state, not from any one run. A StopInstances therefore delivers the pair {stopping, stopped}, a StartInstances {pending, running}, and a TerminateInstances {shutting-down, terminated}, to a rule whose pattern matches only {source, detail-type, detail.instance-id}. A second rule on the identical wiring whose pattern additionally pins `detail: {"state": ["shutting-down"]}` receives NOTHING across the stop and the start (no messages on consecutive long polls, and ApproximateNumberOfMessages and ApproximateNumberOfMessagesNotVisible both "0"), and receives exactly one message — state shutting-down and nothing else — when the instance is terminated.

Emulator: zero envelopes are delivered to either queue. The replay reports only `Error: Check timed out after 60.0s`: the check stops the instance, observes it reach "stopped" through DescribeInstances, then blocks draining the broad rule's queue for the {stopping, stopped} pair (in-check budget 420s) and never clears that barrier, so the harness kills it. Because the positive delivery never happens, the emulator's silence on the narrow rule is indistinguishable from total absence of instance-state eventing, and none of the envelope requirements (source, detail-type, account, region, single-element resources naming the instance ARN, detail.instance-id, detail.state within the six modelled values, a live `time`) nor the narrow-rule negative nor the one-message-on-terminate repair are ever graded.

Why: a client that keys automation off instance lifecycle events — draining a load balancer on stopping, releasing a licence on terminated, pinning a rule to a single state — waits forever against this emulator, because the rule it wired up (PutRule/PutTargets succeed and read back ENABLED with the exact pattern and the single queue target) is never fed. Worse, an "this state emits nothing" assertion passes vacuously for the wrong reason: nothing is emitted for ANY state, so the per-state filtering contract cannot be verified at all.

Fix: EC2's instance-lifecycle event emission into the EventBridge default bus — the StopInstances, StartInstances and TerminateInstances handlers (and the state machine that advances an instance through pending/running/stopping/stopped/shutting-down/terminated) must publish one "EC2 Instance State-change Notification" envelope per transition, with the instance ARN as the sole member of `resources` and the current state in `detail.state`, and EventBridge's rule matcher must apply a `detail.state` pin so a narrowly patterned rule receives only its own state.

Related open issues, which do NOT cover this check: 20261107T075601Z-ec2-events-sqs-ec2-eventbridge-volume-instance-lifecycle-notifications-never-arrive.md and 20261107T081525Z-ec2-events-sqs-6-net-new-causes-ec2.md both concern missing EC2 lifecycle eventing, and 20261116T003513Z-ec2-events-sqs-no-ebs-volume-notification-reaches.md is the EBS-volume analogue of this defect, but the replay did not mark this failure as owned by any of them, so it is live and unowned and is filed here; a fixer closing those should re-replay this check id too.

Traces: ec2__events__sqs.instance-state-change-notifications-are-delivered-per-state-to-their-own-rule.

## Evidence
From the behavioral replay (### ec2__events__sqs):

[mismatch] ec2__events__sqs.instance-state-change-notifications-are-delivered-per-state-to-their-own-rule
Error: Check timed out after 60.0s

Expected (recorded AWS behavior, as asserted by the check):
- Broad rule {"source":["aws.ec2"],"detail-type":["EC2 Instance State-change Notification"],"detail":{"instance-id":[<iid>]}} on the default bus with one SQS target receives, across the check's stop / start / terminate, the full set of detail.state values {"stopping","stopped","pending","running","shutting-down","terminated"}.
- Every delivered envelope has source == "aws.ec2", detail-type == "EC2 Instance State-change Notification", account == the caller's account, region == the request region, resources == [arn:aws:ec2:<region>:<account>:instance/<iid>] (exactly one element), detail.instance-id == <iid>, detail.state in the six modelled values, and a `time` inside the run's window.
- Narrow rule, identical wiring plus "detail":{"instance-id":[<iid>],"state":["shutting-down"]}: zero messages on three consecutive 10s long polls and ApproximateNumberOfMessages == "0" and ApproximateNumberOfMessagesNotVisible == "0" after the stop and again after the start; then exactly the state "shutting-down" (and no other state) after TerminateInstances.

Actual: zero envelopes delivered to either queue. PutRule / set_queue_attributes / PutTargets all succeed (FailedEntryCount 0) and DescribeRule + ListTargetsByRule read the wiring back as ENABLED with the exact pattern and the single queue ARN, but after StopInstances the instance reaches "stopped" through DescribeInstances while the broad rule's queue stays empty. The check never clears that first drain barrier and is killed by the harness: `Error: Check timed out after 60.0s`. The narrow-rule absence legs and the terminate leg are therefore never graded.

## Steps to Reproduce
1. Create a VPC (10.82.0.0/16) and a subnet (10.82.1.0/24) in an AZ that offers t3.micro, plus a security group; resolve the AL2023 AMI via SSM GetParameter /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64 and RunInstances one t3.micro into the subnet. Wait for "running" BEFORE wiring the rules, so the launch's own pending/running events are out of scope.
2. Create two SQS queues (ReceiveMessageWaitTimeSeconds 10, VisibilityTimeout 30) and read each QueueArn via GetQueueAttributes.
3. PutRule two ENABLED default-bus rules: BROAD with EventPattern {"source":["aws.ec2"],"detail-type":["EC2 Instance State-change Notification"],"detail":{"instance-id":[<iid>]}} and NARROW with the same plus "state":["shutting-down"]. SetQueueAttributes on each queue with a Policy allowing events.amazonaws.com sqs:SendMessage on the queue ARN conditioned on aws:SourceArn = the rule ARN, then PutTargets the queue onto its rule. Confirm with DescribeRule and ListTargetsByRule.
4. StopInstances the instance and poll DescribeInstances until State.Name == "stopped".
5. Long-poll the BROAD queue for envelopes naming <iid> until both detail.state "stopping" and "stopped" have arrived. Observed: no messages ever arrive; the check is killed at the 60s harness budget.
6. Expected continuation: the NARROW queue is empty on three consecutive long polls with both depth attributes "0"; StartInstances then delivers {"pending","running"} to BROAD and still nothing to NARROW; TerminateInstances delivers exactly {"shutting-down"} to NARROW and {"shutting-down","terminated"} to BROAD, every envelope carrying source aws.ec2, detail-type "EC2 Instance State-change Notification", the caller's account, the request region, resources == [the instance ARN] and a live ISO-8601 time.
Replay: ec2__events__sqs.instance-state-change-notifications-are-delivered-per-state-to-their-own-rule.
