Issue-Source: trace-sweep-divergence
Issue-Trace: autoscaling-asghc-traj-ip-target-group-attach-refused-instance-group-accepted
Issue-Step: task#9
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: autoscaling-asghc-traj-ip-target-group-attach-refused-instance-group-accepted autoscaling-batch-delete-scheduled-action-partial-failure-envelope autoscaling-batch-put-scheduled-action-partial-failure-envelope autoscaling-batch-put-scheduled-actions-creates-multiple autoscaling-scheduled-action-put-delete-visibility
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling|AttachTrafficSources|4xx->2xx|$.ErrorResponse
Issue-Key: autoscaling|AttachTrafficSources|4xx->2xx|status
Issue-Key: autoscaling|DescribeAutoScalingGroups|2xx->2xx|$.DescribeAutoScalingGroupsResponse.DescribeAutoScalingGroupsResult.AutoScalingGroups.member.TrafficSources.member.Identifier
Issue-Key: autoscaling|DescribeScheduledActions|2xx->2xx|$.DescribeScheduledActionsResponse.DescribeScheduledActionsResult.ScheduledUpdateGroupActions
Issue-Related: 20261113T064920Z-autoscaling-attachloadbalancertargetgroups-skips-target-type-validation-group-target.md
Issue-Commit: 57917fbf623074cc27ff2073937536de1df2e679

# autoscaling traffic-source and scheduled-action state diverges: an invalid attach is accepted and a delete is not applied (3 causes)

> _Note: this issue was identified against the code base at commit `57917fbf623074cc27ff2073937536de1df2e679`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AttachTrafficSources

## Description
Three net-new causes where an autoscaling write that AWS rejects (or applies) is handled differently and the group's subsequent reads carry the wrong state. Each must be closed separately.

Cause 1 of 3 — AttachTrafficSources accepts a target group whose target type is not 'instance'.
AWS: Smithy: AttachTrafficSources input {AutoScalingGroupName required, TrafficSources required (list of TrafficSourceIdentifier), SkipZonalShiftValidation}; modeled errors InstanceRefreshInProgress / ResourceContention / ServiceLinkedRoleFailure, with ValidationError as the service-common argument refusal. An Auto Scaling group registers EC2 instances, so AWS admits only target groups whose target type is 'instance' and refuses an 'ip'-type one. Recorded behavior: HTTP 400, Error.Type 'Sender', Error.Code 'ValidationError', message "Provided Target Group '<target-group-arn>' has invalid target type. Please ensure all provided Target Groups have target type of instance." — fixed text around one variable part, the offending target group ARN taken from the request.
Emulator: HTTP 200 with no ErrorResponse at all; the attach succeeds.
Why: a caller's misconfiguration (an ip/lambda target group wired to an ASG) is silently accepted, so the error surfaces later as instances that never register, and negative tests that assert the refusal pass against AWS and fail only in production.
Fix: AttachTrafficSources' validation — resolve each TrafficSourceIdentifier against the ELBv2 target group's target type before mutating group state. The open issue 20261113T064920Z-autoscaling-attachloadbalancertargetgroups-skips-target-type-validation-group-target.md covers the SAME omission on AttachLoadBalancerTargetGroups and does not carry this operation's replay key; 20261117T012829Z-autoscaling-ec2-elbv2-3-net-new-causes-the.md covers the separate-stores defect, not this check.
Traces: autoscaling-asghc-traj-ip-target-group-attach-refused-instance-group-accepted#task#9.

Cause 2 of 3 — the group read then reports the traffic source AWS never attached.
AWS: Smithy: DescribeAutoScalingGroups -> AutoScalingGroup.TargetGroupARNs (list) and .TrafficSources (list of TrafficSourceIdentifier). Because the invalid-target-type attach was refused (cause 1), AWS's read lists exactly the one instance-type target group that was attached successfully — one TargetGroupARNs entry and one TrafficSources entry, whose Identifier is that same ARN.
Emulator: both lists carry two entries — the refused ip-type target group ARN as well as the accepted instance-type one — and TrafficSources[].Identifier reports the refused ARN.
Why: the group's advertised traffic-source set is wrong, so a client reconciling attachments (or deciding what to detach) acts on an attachment AWS does not have; it is also the durable half of cause 1 and will not clear itself once the attach is refused.
Fix: the group's traffic-source/target-group state — the attach must not commit on the rejected path, and the read must project only committed attachments.
Traces: autoscaling-asghc-traj-ip-target-group-attach-refused-instance-group-accepted#task#12.

Cause 3 of 3 — a scheduled action removed by BatchDeleteScheduledAction is still returned by DescribeScheduledActions.
AWS: Smithy: BatchDeleteScheduledAction {AutoScalingGroupName required, ScheduledActionNames required} -> BatchDeleteScheduledActionAnswer.FailedScheduledActions; a name in the batch that does not exist comes back in FailedScheduledActions while the names that do exist are deleted (partial-failure envelope). DescribeScheduledActions (paginated, items ScheduledUpdateGroupActions) then lists nothing for a group whose only action was deleted, serializing an EMPTY ScheduledUpdateGroupActions element rather than omitting it.
Emulator: HTTP 200 whose ScheduledUpdateGroupActions still contains the deleted action (its ScheduledActionName, ScheduledActionARN, Time/StartTime, MinSize/MaxSize/DesiredCapacity), where the recording has the element present and empty.
Why: the batch delete's partial-failure envelope cannot be trusted — a client that deletes a mix of valid and bogus action names, then re-reads to confirm, sees the action it deleted and either retries forever or concludes the delete failed; the action also stays scheduled in emulator state.
Fix: BatchDeleteScheduledAction's handler/state mutation (apply the deletions for the names that resolved, independently of the ones that failed) and the DescribeScheduledActions serializer's empty-list shape.
Traces: autoscaling-batch-delete-scheduled-action-partial-failure-envelope#task#7; the same divergence key also fires at autoscaling-batch-put-scheduled-action-partial-failure-envelope#task#6, autoscaling-batch-put-scheduled-actions-creates-multiple#task#6 and autoscaling-scheduled-action-put-delete-visibility#task#10. Hypothesis: the scheduled-action store is never mutated on the delete path, which would explain all four members.

## Evidence
Cause 1 — fixture autoscaling-asghc-traj-ip-target-group-attach-refused-instance-group-accepted step task#9, AttachTrafficSources: status expected 400, got 200; $.ErrorResponse missing — expected {'Error': {'Type': 'Sender', 'Code': 'ValidationError', 'Message': "Provided Target Group 'arn:aws:elasticloadbalancing:us-west-2:000000000000:targetgroup/<name>-ip/<id>' has invalid target type. Please ensure all provided Target Groups have target type of instance."}, 'RequestId': <shaped>}, got None.
Cause 2 — same fixture, step task#12, DescribeAutoScalingGroups, 200 vs 200: AutoScalingGroups.member.TargetGroupARNs.member expected the single instance-type ARN ('.../<name>-in/<id>'), got a two-element list ['.../<name>-ip/<id>', '.../<name>-in/<id>']; AutoScalingGroups.member.TrafficSources.member.Identifier expected the instance-type ARN, got the ip-type ARN.
Cause 3 — fixture autoscaling-batch-delete-scheduled-action-partial-failure-envelope step task#7, DescribeScheduledActions, 200 vs 200: $.DescribeScheduledActionsResponse.DescribeScheduledActionsResult.ScheduledUpdateGroupActions expected an empty element, got {'member': {'AutoScalingGroupName': <group>, 'ScheduledActionName': <action>, 'ScheduledActionARN': <arn>, 'Time': '2035-06-01T00:00:00Z', 'StartTime': '2035-06-01T00:00:00Z', 'MinSize': '0', 'MaxSize': '1', 'DesiredCapacity': '1'}} (cluster of 4 steps).
The ARNs, group/action names and ids above are one recording's generated values; the requirement is the list membership and the empty-list shape, not those literals.

## Steps to Reproduce
1. Replay the wire corpus (trace set simparity-wire-aws-v1) against the emulator.
2. Causes 1 and 2: fixture autoscaling-asghc-traj-ip-target-group-attach-refused-instance-group-accepted — at step task#9, AttachTrafficSources on the group naming an ELBv2 target group created with TargetType=ip; expect 400 ValidationError, observe 200. Then at step task#12, DescribeAutoScalingGroups for the same group and inspect TargetGroupARNs and TrafficSources: only the instance-type target group should be listed.
3. Cause 3: fixture autoscaling-batch-delete-scheduled-action-partial-failure-envelope — PutScheduledUpdateGroupAction creates an action, BatchDeleteScheduledAction names that action plus a nonexistent one, then at step task#7 DescribeScheduledActions for the group: ScheduledUpdateGroupActions must be present and empty.
