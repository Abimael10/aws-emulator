Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ec2__events__sqs.xsvc-autoscaling-1-filtered-by-group-name
Issue-Key: autoscaling__ec2__events__sqs.xsvc-autoscaling-1-filtered-by-group-name|_drain_raw#20|Timeout|msgs = sqs.receive_message(QueueUrl=url, MaxNumberOfMessages=10,
Issue-Commit: e7612e758c56c4625d86f552a9150b3dc94d2a44

# Auto Scaling launch events never settle through EventBridge rules filtered on detail.AutoScalingGroupName — the group-name filtering flow never completes

> _Note: this issue was identified against the code base at commit `e7612e758c56c4625d86f552a9150b3dc94d2a44`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAutoScalingGroup

## Description
One net-new root cause.

Cause 1 of 1: the end-to-end Auto Scaling -> EventBridge -> SQS flow for `EC2 Instance Launch Successful` never settles, so a rule filtered on `detail.AutoScalingGroupName` can never be shown to deliver its own group's launch event and to withhold a sibling group's. The check makes no progress and is aborted by the suite's per-check budget. Failing check: autoscaling__ec2__events__sqs.xsvc-autoscaling-1-filtered-by-group-name.

AWS: recorded behavior only — neither the Auto Scaling nor the EventBridge Smithy model declares the event envelope, so the requirement is the recording's. Creating an Auto Scaling group with `MinSize=0, MaxSize=1, DesiredCapacity=1` and a launch template provokes exactly one scaling activity per group, and Auto Scaling publishes one event per successful launch onto the default event bus with `source` = `aws.autoscaling`, `detail-type` = `EC2 Instance Launch Successful`, `detail.AutoScalingGroupName` = the group's name, `detail.EC2InstanceId` = the instance the group lists under `Instances`, and `resources` = exactly the group's `AutoScalingGroupARN` plus `arn:aws:ec2:<region>:<account>:instance/<instance-id>`. The envelope carries `version` = `"0"`, the caller's `account`, the request's `region`, an `id` that is a UUID, and a `time` that is an ISO-8601 UTC instant to whole seconds (`YYYY-MM-DDTHH:MM:SSZ`). A rule whose pattern pins `detail.AutoScalingGroupName` to one literal group receives that group's launch event and never the sibling group's; an unfiltered/prefix-filtered control rule receives both. The single launch event reaching two matching rules keeps ONE `id` across both deliveries, and two different events never share an `id`.

Emulator: the check never reaches any of those assertions. It is aborted with `Error: Check timed out after 60.0s`, i.e. the flow produced no observable progress inside the budget, while every wait inside the check is itself far longer-lived (the SQS drain loop allows 600 s, the group-membership poll 300 s, the rule-liveness barrier 300 s). Each empty `ReceiveMessage` in the drain loop burns its 10 s `WaitTimeSeconds`, so a budget exhausted at 60 s is what an endlessly empty queue looks like: the loop spins on long-polls that return nothing. Hypothesis (not stated by the model or by the recording): the launch events are never published onto the default bus at all, so `_collect` on the control queue never satisfies its predicate; a nearby, narrower possibility is that the PutEvents liveness barrier (`_await_rules_live`) never round-trips its nonce through the rules, which would stall the same way before any Auto Scaling event is provoked. Either way the emulator serves no event where AWS serves one per launch.

Why: a client that reacts to capacity changes through EventBridge — a rule per Auto Scaling group feeding a queue, a Lambda, or an alarm pipeline — sees nothing happen. Scale-out completes silently, downstream consumers are never woken, and group-name filtering cannot be validated at all: a user cannot distinguish "my pattern is wrong" from "no event was ever published", because both look like an empty queue. Any automation whose correctness depends on one event per launch, with stable `id` across matching rules and the group ARN plus instance ARN in `resources`, cannot be exercised against the emulator.

Fix: the Auto Scaling scaling-activity path that must emit a lifecycle event when a launch succeeds, and the EventBridge default-bus matching/delivery path that must route it to every matching rule target — the emitter's event construction (source, detail-type, detail.AutoScalingGroupName, detail.EC2InstanceId, resources, and the shared envelope id/time/account/region) plus the bus's pattern evaluation for a `detail.AutoScalingGroupName` literal and for a `{"prefix": ...}` match. Confirm the PutEvents-to-SQS liveness path first, since the check gates on it before provoking any Auto Scaling activity.

Traces: autoscaling__ec2__events__sqs.xsvc-autoscaling-1-filtered-by-group-name — builds a VPC/subnet and one launch template per group, creates two groups (`<base>-a`, `<base>-b`) each at `DesiredCapacity=1`, puts one rule filtered on `detail.AutoScalingGroupName == <base>-a` and one prefix-filtered control rule, then drains both queues. It never completes.

## Evidence
Failing check autoscaling__ec2__events__sqs.xsvc-autoscaling-1-filtered-by-group-name:

  Error: Check timed out after 60.0s

No assertion in the check body was reached, so the emulator never produced an event to grade.

Expected (AWS), after two Auto Scaling groups are created with DesiredCapacity=1:
  - the prefix-filtered control queue receives launch events for BOTH groups:
      {detail.AutoScalingGroupName for e in ctl} == {"<base>-a", "<base>-b"}
  - the queue behind the rule filtered on detail.AutoScalingGroupName == "<base>-a" receives
      exactly {"<base>-a"} and never the sibling's event
  - per group: {detail.EC2InstanceId} equals the InstanceIds the group lists under Instances
  - per event: resources == { <AutoScalingGroupARN>, "arn:aws:ec2:<region>:<account>:instance/<instance-id>" }
  - envelope: version == "0", source == "aws.autoscaling",
      detail-type == "EC2 Instance Launch Successful", account == caller's account,
      region == request region, id matches ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$,
      time matches ^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$ and falls inside the run window
  - the one launch event of "<base>-a" reaches both rules under the SAME id:
      {e["id"] for e in filt} == {e["id"] for e in ctl if detail.AutoScalingGroupName == "<base>-a"}
  - no two different events share an id

Actual (emulator): nothing observable. The check is killed at 60 s. Its own internal budgets are
  _DRAIN_TIMEOUT_S = 600.0 (SQS drain until both groups' events arrive),
  _poll(..., timeout_s=300.0) for group membership,
  _await_rules_live(..., timeout_s=300.0) for the PutEvents nonce barrier,
all of which exceed the 60 s cap, and each empty ReceiveMessage costs the queue's
ReceiveMessageWaitTimeSeconds=10 — consistent with queues that stay empty rather than with any
single slow call. The assertion that would have fired on a delivered-but-wrong event
("the control queue receiving launch events for both ...: not delivered within 600.0s ... A target
that never emits its own events fails exactly here") was never reached either, which is itself the
signal that no event was produced within the budget.

Not attributable to the runner: no tool was missing, no name resolution or TLS failure was reported,
and nothing asserted here needs a real account or a live run — the account id and region are read
back from the emulator's own STS/endpoint, and the id/time values are only shape- and window-checked.

## Steps to Reproduce
1. Point AWS SDK clients for autoscaling, ec2, events, sqs and sts at the emulator.
2. Create a VPC (10.93.0.0/16) and one subnet (10.93.1.0/24) in an AZ that offers t3.micro; resolve an AMI from SSM parameter /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64.
3. Create two launch templates (ImageId = that AMI, InstanceType = t3.micro), one per group name `<base>-a` and `<base>-b`.
4. Create two SQS queues with Attributes={"ReceiveMessageWaitTimeSeconds": "10"} and read each QueueArn.
5. PutRule on the default bus + PutTargets at queue 1 with EventPattern
   {"$or":[{"source":["aws.autoscaling"],"detail-type":["EC2 Instance Launch Successful"],"detail":{"AutoScalingGroupName":["<base>-a"]}},{"source":["sim2real.control"],"detail":{"nonce":["<n1>"]}}]}
   and a second rule + target at queue 2 with the same shape but "AutoScalingGroupName":[{"prefix":"<base>-"}] and nonce <n2>. SetQueueAttributes on each queue with a Policy allowing events.amazonaws.com sqs:SendMessage with ArnEquals aws:SourceArn = that rule's ARN. Assert PutTargets FailedEntryCount == 0.
6. Liveness barrier: PutEvents(Source="sim2real.control", Detail={"nonce": <n>}) for each queue and ReceiveMessage until both nonces come back through their own rules. If this never completes, the EventBridge->SQS delivery path is the blocker.
7. CreateAutoScalingGroup for `<base>-a` and `<base>-b`, each MinSize=0, MaxSize=1, DesiredCapacity=1, LaunchTemplate=$Latest, VPCZoneIdentifier = the subnet.
8. ReceiveMessage/DeleteMessageBatch both queues in a loop.
   - AWS: the control queue yields aws.autoscaling / "EC2 Instance Launch Successful" events for BOTH group names; the filtered queue yields only `<base>-a`'s, with the same event id as the control queue's copy of it.
   - Emulator: both queues stay empty; the loop makes no progress and the 60 s per-check budget expires.
9. Replay the behavioral check directly: autoscaling__ec2__events__sqs.xsvc-autoscaling-1-filtered-by-group-name.
