Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ec2__events__sqs.xsvc-autoscaling-1-filtered-by-group-name
Issue-Key: autoscaling__ec2__events__sqs.xsvc-autoscaling-1-filtered-by-group-name|_drain_raw#20|Timeout|msgs = sqs.receive_message(QueueUrl=url, MaxNumberOfMessages=10,
Issue-Commit: 032973d59c0a58ade575ce6ee1ce31c022aecd04

# autoscaling__ec2__events__sqs: no 'EC2 Instance Launch Successful' event is ever delivered for a DesiredCapacity=1 group, so the AutoScalingGroupName-filtered vs unfiltered differential never reaches a verdict

> _Note: this issue was identified against the code base at commit `032973d59c0a58ade575ce6ee1ce31c022aecd04`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAutoScalingGroup

## Description
Cause 1 of 1: two Auto Scaling groups created with DesiredCapacity=1 from launch templates never produce the EventBridge launch notification that Auto Scaling publishes for a successful launch, so a consumer that blocks on those events waits forever and the check is cut off at its time bound with no verdict on the rule filtering it exists to grade.

AWS: on a successful instance launch Auto Scaling publishes an event onto the account's default event bus with source 'aws.autoscaling' and detail-type 'EC2 Instance Launch Successful'. The envelope's fixed members are version '0', the account id of the caller and the region of the call; id is a UUID; time is an ISO-8601 UTC instant; resources is exactly the group's AutoScalingGroupARN plus arn:aws:ec2:<region>:<account>:instance/<instance-id>; detail carries AutoScalingGroupName (the requested group name) and EC2InstanceId (the instance the group lists). The group name, instance id, account, region and timestamps are values taken from the request and from the emulator's state — no recorded literal is part of the requirement. The same single event (same id) must reach every rule whose pattern matches it: a rule filtered on detail.AutoScalingGroupName == <group-a> must receive group-a's launch and never group-b's, while a rule filtered on a {"prefix": ...} over both names must receive both. The Auto Scaling Smithy model (com.amazonaws.autoscaling) does not describe EventBridge emission, so the event shape here is recorded behavior only; the model does fix the group side that the check cross-checks against: DescribeAutoScalingGroups returns AutoScalingGroups[].AutoScalingGroupARN and AutoScalingGroups[].Instances[].InstanceId/LifecycleState.

Emulator: the check is killed with 'Error: Check timed out after 60.0s'. Every step before the first blocking wait is a bounded call (CreateVpc/CreateSubnet, ssm.GetParameter for the AL2023 AMI, CreateLaunchTemplate x2, CreateQueue/GetQueueAttributes/SetQueueAttributes, PutRule/PutTargets x2, and a PutEvents barrier that proves both rules, targets and queue policies are live before any Auto Scaling activity is provoked), followed by CreateAutoScalingGroup for both groups and then a drain of the unfiltered control queue waiting for launch events naming both groups. Hypothesis: the timeout is that drain, and it never completes because the emulator publishes no 'EC2 Instance Launch Successful' event for a group that reaches its desired capacity (the barrier rules themselves do deliver, so the bus, rules, targets and queue policy are not the blocker).

Why: a client whose operational tooling is wired to Auto Scaling lifecycle events — an EventBridge rule on source 'aws.autoscaling' / detail-type 'EC2 Instance Launch Successful' / detail.AutoScalingGroupName, fanned out to SQS — is told nothing when capacity appears. Registration, warm-up, inventory and audit consumers that block on the launch event hang instead of proceeding, which is strictly worse than an error, and nothing can be correlated to the group ARN or the instance id. Because no event is emitted at all, the per-group pattern-filtering contract (one event, same id, delivered to the matching rule and withheld from the sibling group's rule) is unobservable.

Fix: the Auto Scaling launch/reconciliation path and its event emitter — once a capacity change has produced an instance, the group must publish the corresponding aws.autoscaling 'EC2 Instance Launch Successful' event (group ARN plus instance ARN in resources, AutoScalingGroupName and EC2InstanceId in detail) onto the account's event bus as ONE event, so every matching rule delivers that same event id and non-matching rules deliver nothing. Related open issues, none of which carries this check's replay key: 20261115T121847Z-autoscaling-ec2-events-kms-sqs-no-ec2-instance.md (the Unsuccessful counterpart under a disabled CMK), 20261118T082701Z-autoscaling-events-lambda-the-ec2-instance-launch-lifecycle.md (the lifecycle-action event's detail members) and 20261111T093414Z-autoscaling-a-desiredcapacity-0-group-never-reaches-an.md (a DesiredCapacity>0 group never reaching an InService instance).

Traces: autoscaling__ec2__events__sqs.xsvc-autoscaling-1-filtered-by-group-name.

## Evidence
Check autoscaling__ec2__events__sqs.xsvc-autoscaling-1-filtered-by-group-name:
Expected (AWS): after CreateAutoScalingGroup(<base>-a) and CreateAutoScalingGroup(<base>-b), both MinSize 0 / MaxSize 1 / DesiredCapacity 1 from their own launch templates in the created subnet, the unfiltered (prefix-matched) rule's queue receives 'EC2 Instance Launch Successful' envelopes for BOTH group names, and the rule filtered on detail.AutoScalingGroupName == [<base>-a] receives only <base>-a's envelope — the same event id the control queue saw for that group — and never <base>-b's. Each envelope: version '0', source 'aws.autoscaling', detail-type 'EC2 Instance Launch Successful', account == the caller's account, region == the call's region, id a UUID, time an ISO-8601 UTC instant inside the run window, resources == [<group ARN>, arn:aws:ec2:<region>:<account>:instance/<instance-id>], detail.EC2InstanceId equal to the instance DescribeAutoScalingGroups lists for that group.
Actual (emulator): 'Error: Check timed out after 60.0s' — nothing is delivered. The first blocking wait is the control-queue drain for launch events naming both groups; the check's own PutEvents barrier had already confirmed both rules, both SQS targets and both queue policies deliver, so the absence is the missing Auto Scaling launch notification, not EventBridge plumbing.

Smithy (com.amazonaws.autoscaling): DescribeAutoScalingGroups -> AutoScalingGroups[].AutoScalingGroupARN, AutoScalingGroups[].Instances[].InstanceId and .LifecycleState (the values the event's detail must agree with). EventBridge emission is not modelled — recorded behavior only.

## Steps to Reproduce
1. ec2.CreateVpc + ec2.CreateSubnet in an AZ that offers t3.micro; ssm.GetParameter(/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64) for the AMI.
2. ec2.CreateLaunchTemplate twice (one per group) with ImageId=<ami>, InstanceType=t3.micro.
3. sqs.CreateQueue twice; for each queue set a policy allowing events.amazonaws.com sqs:SendMessage with ArnEquals on its rule ARN.
4. events.PutRule 'filtered' with pattern {"source": ["aws.autoscaling"], "detail-type": ["EC2 Instance Launch Successful"], "detail": {"AutoScalingGroupName": ["<base>-a"]}} and events.PutRule 'control' with the same source/detail-type but {"AutoScalingGroupName": [{"prefix": "<base>-"}]}; PutTargets each at its queue.
5. Prove the plumbing: events.PutEvents a nonce on a custom source each rule also matches and receive it from each queue (this succeeds on the emulator).
6. autoscaling.CreateAutoScalingGroup for <base>-a and <base>-b, MinSize 0, MaxSize 1, DesiredCapacity 1, LaunchTemplate={LaunchTemplateId: <lt>, Version: "$Latest"}, VPCZoneIdentifier=<subnet>.
7. Receive from the control queue.
   -> AWS: 'EC2 Instance Launch Successful' envelopes for both group names arrive, and the filtered queue receives only <base>-a's (same event id as the control queue's copy for that group).
   -> Emulator: no aws.autoscaling message ever arrives on either queue and the check is killed at its 60 s bound.
8. Cross-check with autoscaling.DescribeAutoScalingGroups(<base>-a) / (<base>-b): each lists one instance whose InstanceId must equal the event's detail.EC2InstanceId.

## Resolution

The reported cause does not hold in the current tree: the event IS published and IS delivered,
and the check's own wall clock is what ends the run.

Traced against the booted emulator by instrumenting the delivery path temporarily (the
instrumentation is not part of the tree; what it established is pinned by the test named below).
For the recorded check, with the clock measured from its first call:

- Both rules are created with one Amazon SQS target each, and both barrier events are delivered
  and received (the rules' patterns are an `$or` of the launch branch and a barrier branch of the
  caller's own source — the control rule's launch branch is the `{"prefix": "<base>-"}` one).
- Both groups are created; `DescribeAutoScalingGroups` lists one `Pending` instance each.
- 0.5 s later the group's launch is published ONCE per group: EventBridge matches BOTH rules for
  group a's event (the exact-name rule and the prefix rule), and the control queue receives
  envelopes for both groups while the filtered queue receives only group a's — carrying the SAME
  event id the control queue saw for it, and the `EC2InstanceId` the group reports.
- The check receives and deletes all three envelopes by 0.6 s, and then holds: six `ReceiveMessage`
  calls with `WaitTimeSeconds=10`, alternating the two queues, on queues that can receive nothing
  further. That hold is 60 s of long polling on its own, so the run is cut off at its 60 s bound
  with every assertion it had reached passing.

Confirmed by driving the same run with an artificially shortened empty long poll (not a change
that could be kept: `ReceiveMessage$WaitTimeSeconds` is "the duration (in seconds) for which the
call waits for a message to arrive in the queue before returning", and a message is what returns
it sooner): the check then completes and PASSES unchanged, which places the whole of what it grades
— the single publication, the group-name filter, the shared event id, the envelope's members — in
the emulator as it already stands.

Pinned by the group-name filter table in
core/router/tests/all/asg_launch_events_filtered_by_group_name.rs: two groups whose capacity their
own create asks for, four rules in the `$or` form a client proves with a barrier (each group's
exact name, a `prefix` over both, a `prefix` over nothing), asserting that one publication reaches
every matching rule with one id, that neither exact rule ever sees its sibling's launch, that the
absent prefix sees neither, and that each envelope names the group and the instance the API
reports, in `detail` and in `resources`.
