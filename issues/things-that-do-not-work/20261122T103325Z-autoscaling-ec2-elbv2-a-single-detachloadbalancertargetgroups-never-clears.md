Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ec2__elbv2.asgdx-reattaching-an-attached-target-group-through-either-api-keeps-one-attachment
Issue-Key: autoscaling__ec2__elbv2.asgdx-reattaching-an-attached-target-group-through-either-api-keeps-one-attachment|_poll#15|Timeout|time.sleep(interval_s)
Issue-Commit: 68c71f6585a425304635c9d8c1cc1d9408beb2ea

# autoscaling__ec2__elbv2: a single DetachLoadBalancerTargetGroups never clears the target-group attachment from every view, so the re-attach/detach set-semantics check never converges

> _Note: this issue was identified against the code base at commit `68c71f6585a425304635c9d8c1cc1d9408beb2ea`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DetachLoadBalancerTargetGroups

## Description
ONE net-new root cause (Cause 1 of 1). It must be closed before this issue is submitted.

=== Cause 1 of 1: after a target group is attached at CreateAutoScalingGroup time and then re-attached through both AttachLoadBalancerTargetGroups and AttachTrafficSources, a single DetachLoadBalancerTargetGroups does not make all three attachment views empty, so the group never converges to 'no attachment'. ===
Failing check: autoscaling__ec2__elbv2.asgdx-reattaching-an-attached-target-group-through-either-api-keeps-one-attachment

AWS: Smithy (services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/smithy-model/com-amazonaws-autoscaling-autoscaling-2011-01-01.json, awsQuery, 2011-01-01): DetachLoadBalancerTargetGroups input {AutoScalingGroupName required XmlStringMaxLen255 length 1-255, TargetGroupARNs required list}, output DetachLoadBalancerTargetGroupsResultType (empty), only error ResourceContentionFault (500); DetachTrafficSources input {AutoScalingGroupName required, TrafficSources required list}, empty output, same single error; DescribeTrafficSources output {TrafficSources: TrafficSourceStates, NextToken} with errors InvalidNextToken (400) / ResourceContentionFault (500); DescribeLoadBalancerTargetGroups output {LoadBalancerTargetGroups, NextToken}; DescribeAutoScalingGroups projects AutoScalingGroups[].TargetGroupARNs and AutoScalingGroups[].TrafficSources. The set semantics and the cross-view equivalence are recorded behavior only (the model declares the members, not the invariant): on AWS the ELBv2 attachment surface is ONE set per group, attachment is idempotent (attaching an already-attached target group through either API is accepted and still leaves exactly one attachment), and ONE detach through either API removes the attachment from ALL views — DescribeAutoScalingGroups.TargetGroupARNs, DescribeAutoScalingGroups.TrafficSources, DescribeLoadBalancerTargetGroups and DescribeTrafficSources(TrafficSourceType='elbv2') all stop reporting it (entries pass through a transient 'Removing' state and then disappear or read State 'Removed') within the normal convergence budget.
Emulator: the group never reaches the empty state. The check attaches the target group at create, re-attaches it with AttachLoadBalancerTargetGroups and again with AttachTrafficSources({Identifier: <tg>, Type: 'elbv2'}), then calls DetachLoadBalancerTargetGroups(TargetGroupARNs=[<tg>]) once and polls all three views for {arns: [], lbtg: [], dts: []} with a 180 s deadline; it is killed by the harness with 'Check timed out after 60.0s' with no assertion ever reported. Everything before that poll is bounded by at most ~6 s of sleeps and a fixed number of create/describe calls, and the group is created with DesiredCapacity 0 so no instance launch is waited on — the only unbounded wait in the check is the post-detach convergence poll. Hypothesis: the single legacy detach leaves the attachment reported by at least one of the three views (the traffic-source store written by AttachTrafficSources, or an entry stuck in a non-terminal 'Removing'/non-'Removed' state), so the predicate never holds. The emulator serves the describes successfully — it just never stops reporting the attachment.
Why: detach is how a client decommissions load-balanced capacity. If one detach does not clear every view, a client that detaches through the legacy API and then reads the modern traffic-source view (as CloudFormation/CDK drift detection does) still sees the group wired to a target group that is being deleted, blocks the target group's own delete on a phantom reference, and retries or double-detaches forever. Worse, no automation can ever observe the group reaching 'no attachment', so every teardown and every test of attachment set semantics hangs instead of failing fast.
Fix: the autoscaling group attachment state and its detach handlers — DetachLoadBalancerTargetGroups and DetachTrafficSources must remove the entry from the ONE per-group attachment set keyed by {Identifier, Type} (written alike by CreateAutoScalingGroup's TargetGroupARNs, AttachLoadBalancerTargetGroups and AttachTrafficSources, with set semantics so a redundant attach never duplicates), and all four readers — DescribeTrafficSources, DescribeLoadBalancerTargetGroups and the DescribeAutoScalingGroups serializer's TargetGroupARNs and TrafficSources members — must stop reporting it (advancing any transient 'Removing' state to removal) within the normal response budget.
Related open issue that does NOT currently cover this failure: 20261117T012829Z-autoscaling-ec2-elbv2-3-net-new-causes-the.md names this same check in its Issue-Trace-Ids, but its replay key for it is the create-time assertion `assert views() == once, f"the attachment made at create reads {views()!r}"` (its Cause 1: DescribeTrafficSources returning an empty list for a group created with TargetGroupARNs). The replay no longer marks this check OWNED: it now gets PAST that assertion and dies later, in the post-detach convergence poll, so a fixer closing that issue's Cause 1 will not reach or fix the detach defect described here.
Traces: autoscaling__ec2__elbv2.asgdx-reattaching-an-attached-target-group-through-either-api-keeps-one-attachment.

## Evidence
Replay of behavioral suite simparity-behavioral-aws-v1 against the booted emulator: 363/465 checks passed (78.1%), 89 mismatched, 13 errored, bootstrap_signal false. Service heading '### autoscaling__ec2__elbv2': 48/60 (80%), 12 failed; 11 of those are marked OWNED by an open issue and are skipped. This one carries no owner and no stub.

[mismatch] autoscaling__ec2__elbv2.asgdx-reattaching-an-attached-target-group-through-either-api-keeps-one-attachment
  Error: Check timed out after 60.0s

Check body (as replayed), with the call sequence and its budgets:
  _network / _cluster_sg / _launch_template / _target_group  — fixed number of ec2+elbv2 creates (NLB target group, Protocol TCP, Port 44300, TargetType 'instance', HTTP health check on :4000)
  _asg(..., tgs=[tg], desired=0)  — CreateAutoScalingGroup with TargetGroupARNs=[tg], DesiredCapacity 0 (no instance launch is awaited)
  line 81: assert views() == {'arns': [tg], 'lbtg': [tg], 'dts': [tg]}  — the create-time assertion that the previously filed issue 20261117T012829Z keys on; no AssertionError was reported this run
  AttachLoadBalancerTargetGroups(TargetGroupARNs=[tg]) then AttachTrafficSources([{Identifier: tg, Type: 'elbv2'}])
  3 x (views() == once, sleep 2)  — ~6 s, bounded
  DetachLoadBalancerTargetGroups(TargetGroupARNs=[tg])
  _poll(views, lambda v: v == {'arns': [], 'lbtg': [], 'dts': []}, timeout_s=180.0, interval_s=5.0, description='one detach clears the attachment from every view')

views() is built from three reads: DescribeAutoScalingGroups -> AutoScalingGroups[0].TargetGroupARNs; DescribeLoadBalancerTargetGroups(AutoScalingGroupName=<name>) -> LoadBalancerTargetGroupARN of entries whose State != 'Removed'; DescribeTrafficSources(AutoScalingGroupName=<name>, TrafficSourceType='elbv2') -> Identifier of entries whose State != 'Removed'.

The 180 s post-detach poll is the only wait in the check that exceeds the harness's 60 s cap, which is why the run is killed there rather than returning a verdict: the emulator answers every describe but never stops reporting the detached attachment in all three views. Generated names/ARNs (sim2real-asgdx-<id>, the target-group ARN suffix) are per-run values, not part of the requirement; the requirement is that one detach empties all three views.

Smithy consulted: DetachLoadBalancerTargetGroups (input {AutoScalingGroupName required length 1-255, TargetGroupARNs required list}, output DetachLoadBalancerTargetGroupsResultType, errors: ResourceContentionFault only), DetachTrafficSources (input {AutoScalingGroupName required, TrafficSources required list}, errors: ResourceContentionFault only), DescribeTrafficSources (output {TrafficSources: TrafficSourceStates, NextToken}, paginated, errors InvalidNextToken/ResourceContentionFault).

## Steps to Reproduce
1. git_clone the repo and run the behavioral suite (simparity-behavioral-aws-v1); the check in trace_ids reproduces under the '### autoscaling__ec2__elbv2' heading.
2. Create a VPC with one subnet, a security group, a launch template (AL2023, t3.micro), and an ELBv2 target group <tg> (Protocol TCP, Port 44300, TargetType 'instance') in that VPC.
3. CreateAutoScalingGroup(<name>, LaunchTemplate=..., MinSize=0, MaxSize=2, DesiredCapacity=0, VPCZoneIdentifier=<subnet>, TargetGroupARNs=[<tg>], HealthCheckType='ELB').
4. Read the three views and expect each to name exactly <tg>: DescribeAutoScalingGroups -> TargetGroupARNs; DescribeLoadBalancerTargetGroups(<name>) -> entries with State != 'Removed'; DescribeTrafficSources(<name>, TrafficSourceType='elbv2') -> entries with State != 'Removed'.
5. Re-attach the SAME target group through both APIs: AttachLoadBalancerTargetGroups(<name>, TargetGroupARNs=[<tg>]) and AttachTrafficSources(<name>, TrafficSources=[{Identifier: <tg>, Type: 'elbv2'}]). Both must be accepted (200) and the three views must still show exactly ONE attachment on three consecutive reads (set semantics, no duplicate).
6. Call DetachLoadBalancerTargetGroups(<name>, TargetGroupARNs=[<tg>]) ONCE.
7. Poll the three views. Expect all three to become empty (any transient 'Removing' entry resolving to removed/'Removed') well within the normal convergence budget.
   Observed: the views never all go empty — the check is killed with 'Check timed out after 60.0s' inside this 180 s poll. Record which of the three views still reports <tg> and in what State to confirm the fix.
8. Repeat with the detach issued through the OTHER API (DetachTrafficSources(<name>, TrafficSources=[{Identifier: <tg>, Type: 'elbv2'}])) and expect the same empty result, then re-replay the check in trace_ids.
