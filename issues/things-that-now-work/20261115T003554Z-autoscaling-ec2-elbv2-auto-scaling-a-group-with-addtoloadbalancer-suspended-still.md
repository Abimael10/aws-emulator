Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ec2__elbv2.deep-autoscaling-3-asg-suspend-add-to-load-balancer-withholds-registration
Issue-Key: autoscaling__ec2__elbv2.deep-autoscaling-3-asg-suspend-add-to-load-balancer-withholds-registration|run#3|AssertionError|assert subject_id not in registered, (
Issue-Commit: a49cdab94fc41177458b6e53730040285f53d8d6

# Auto Scaling: a group with AddToLoadBalancer suspended still registers its launched instance with the attached target group

> _Note: this issue was identified against the code base at commit `a49cdab94fc41177458b6e53730040285f53d8d6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SuspendProcesses

## Description
One net-new root cause.

Cause 1 of 1: SuspendProcesses("AddToLoadBalancer") is recorded but not enforced, so instances launched while the process is suspended are still registered with the group's attached target group.

AWS: Smithy (autoscaling) models SuspendProcesses with member ScalingProcesses of ProcessNames, and DescribeAutoScalingGroups returns SuspendedProcesses. AWS documents AddToLoadBalancer as the process that "adds instances to the attached load balancer or target group when they launch"; while it is suspended Auto Scaling launches the instance and brings it InService but does NOT register it, and ELBv2 DescribeTargetHealth for that instance id answers TargetHealth.State 'unused' with Reason exactly 'Target.NotRegistered'. Resuming the process does not retro-register instances launched during the suspension — only instances launched after the resume are registered. The model does not state the registration side effect, so for the registration/Reason behaviour this is recorded behavior only.

Emulator: after SuspendProcesses(ScalingProcesses=['AddToLoadBalancer']) on the subject group (and DescribeAutoScalingGroups correctly echoing exactly that suspended process), a scale-out registers the new instance anyway. One second after the scale-out the shared target group already held both the control group's instance and the subject's instance, and DescribeTargetHealth for the subject's instance did not report Reason 'Target.NotRegistered'. The failing assertion reads: "the subject's instance <instance-id> was registered with the target group although AddToLoadBalancer is suspended (1s after scale-out); registered=[<control instance id>, <subject instance id>]". The instance ids and the 1s elapsed figure are per-run values; the requirement is that the suspended group's instance NEVER appears in the target group's registered set and is reported as Target.NotRegistered.

Why: a client that suspends AddToLoadBalancer to stage capacity before exposing it to traffic (blue/green warm-up, pre-bake, instance-level smoke tests) gets its un-validated instance put into the load balancer rotation immediately. Conversely a client that relies on the resume NOT retro-registering earlier instances is given a different fleet than AWS would. Both are silent traffic-correctness faults, not error-path cosmetics.

Fix: the Auto Scaling group's launch/registration path — the state that records SuspendedProcesses must gate the target-group registration side effect, and the ELBv2 target-health read must report Reason 'Target.NotRegistered' for an instance that was never registered.

Traces: autoscaling__ec2__elbv2.deep-autoscaling-3-asg-suspend-add-to-load-balancer-withholds-registration.

## Evidence
Expected (AWS): with AddToLoadBalancer suspended, the target group's registered set never contains the subject group's instance; DescribeTargetHealth by that instance id returns Reason 'Target.NotRegistered' while the ASG still reports it InService; after ResumeProcesses the instance launched during the suspension stays Target.NotRegistered and only post-resume launches are registered.

Actual (emulator): AssertionError: the subject's instance i-00000000000000002 was registered with the target group although AddToLoadBalancer is suspended (1s after scale-out); registered=['i-00000000000000001', 'i-00000000000000002']. DescribeAutoScalingGroups did report SuspendedProcesses == {'AddToLoadBalancer'} for the subject and nothing suspended for the control group, so the suspension was stored and then ignored by the registration path.

## Steps to Reproduce
1. Create a target group and two Auto Scaling groups attached to it (a subject and a control), each at desired capacity 1, from a launch template.
2. Wait for the fresh target group to have no registered targets.
3. Call SuspendProcesses(AutoScalingGroupName=<subject>, ScalingProcesses=['AddToLoadBalancer']) and confirm DescribeAutoScalingGroups reports exactly that process suspended on the subject and nothing on the control.
4. Raise the subject's desired capacity to scale out and wait for the new instance to be running in EC2.
5. Poll ELBv2 DescribeTargetHealth for the target group: the subject's instance id must never appear; AWS keeps the registered set equal to the control's instance only. The emulator registers the subject's instance.
6. Also call DescribeTargetHealth with Targets=[{'Id': <subject instance id>}]: AWS answers Reason 'Target.NotRegistered'.
