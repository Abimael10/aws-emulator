Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.dasgrefresh-instance-refresh-replaces-every-guest-with-the-new-launch-template-version
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.dasgrefresh-instance-refresh-replaces-every-guest-with-the-new-launch-template-version|run#43|AssertionError|assert acc["attached_floor"] >= floor, (
Issue-Commit: 9f7bf23473ff024f6e39d27cef35bcce93712a4b

# ec2: a rolling instance refresh at MinHealthyPercentage=100 drops the group below its healthy floor — only 1 of 2 instances stays attached

> _Note: this issue was identified against the code base at commit `9f7bf23473ff024f6e39d27cef35bcce93712a4b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartInstanceRefresh

## Description
Cause 1 of 1: a Rolling instance refresh run with Preferences.MinHealthyPercentage=100 on a group whose DesiredCapacity is 2 lets the attached instance count fall to 1. The refresh removes capacity the healthy floor forbids it to remove.

AWS: recorded behavior only for the timeline (MinHealthyPercentage is modeled on Auto Scaling's RefreshPreferences as an integer percentage with no behavioral trait). AWS documents and the recording shows the guarantee it buys: during a Rolling refresh the group never has fewer than ceil(DesiredCapacity * MinHealthyPercentage / 100) instances ATTACHED — counting both InService instances and instances already in a Terminating/draining lifecycle state that have not yet left the group. At MinHealthyPercentage=100 with DesiredCapacity=2 that floor is 2, which leaves room for exactly one instance to be draining at a time (DesiredCapacity - floor + 1); AWS therefore launches each replacement and brings it InService BEFORE it stops serving from the original it replaces, so the Nth original only departs once N-1 replacements are already InService.

Emulator: sampling DescribeAutoScalingGroups throughout the refresh, the count of attached members (InService ∪ LifecycleState starting with `Terminating`) is observed at 1, below the required floor of 2: `AssertionError: the group must keep at least 2 instance(s) attached — InService, or draining but not yet gone — at every moment of the refresh; it was sampled at 1`. The ordering assertions that follow (max draining at once <= allowed, each original departing only after enough replacements are InService) are the same contract and are not reached. The emulator terminates an original first and launches its replacement afterwards instead of the reverse.

Why: MinHealthyPercentage is the one knob a caller has to make a refresh non-disruptive. A client that sets it to 100 to guarantee no loss of serving capacity gets exactly the gap it paid to avoid: during the refresh the group serves from half its fleet, so a load balancer in front of it has one healthy target instead of two, and a refresh that AWS makes invisible to traffic becomes a visible capacity dip.

Fix: the Auto Scaling instance-refresh engine's rolling replacement ordering and capacity accounting — a replacement must be launched and reach InService before the original it replaces is detached/terminated, and the per-sample attached count must be held at or above ceil(DesiredCapacity * MinHealthyPercentage / 100). Note the existing open issue 20261124T190717Z-autoscaling-2-net-new-causes-an-instance-refresh.md, which reports a refresh that replaces NO member at all (so the Min/MaxHealthyPercentage window was never observable); this check gets further — replacements do happen — and fails on the floor being breached, so it is a distinct cause and is filed here with that issue named.

Traces: ec2.dasgrefresh-instance-refresh-replaces-every-guest-with-the-new-launch-template-version.

## Evidence
Check: ec2.dasgrefresh-instance-refresh-replaces-every-guest-with-the-new-launch-template-version

Setup: DesiredCapacity=2, t3.micro, launch template versions 1 then 2; StartInstanceRefresh(Strategy="Rolling", DesiredConfiguration={LaunchTemplate version 2}, Preferences={MinHealthyPercentage: 100, InstanceWarmup: 0, SkipMatching: True, ScaleInProtectedInstances: "Ignore", StandbyInstances: "Ignore", AutoRollback: False}).

Expected (AWS): floor = (2 * 100) // 100 = 2 attached instances at every sample, where attached = InService ∪ {LifecycleState startswith "Terminating"}; at most (2 - 2 + 1) = 1 instance draining in any one sample; and the Nth original leaves service only after N-1 replacements are already InService.

Actual (emulator):
  File "check_ec2_dasgrefresh_instance_refresh_replaces_every_guest_with_the_new_launch_template_version.py", line 665, in run
    assert acc["attached_floor"] >= floor
  AssertionError: the group must keep at least 2 instance(s) attached — InService, or draining but not yet gone — at every moment of the refresh; it was sampled at 1

The numbers 2 and 1 here are the request's DesiredCapacity/MinHealthyPercentage-derived floor and the observed minimum; the requirement is the inequality attached >= ceil(DesiredCapacity * MinHealthyPercentage / 100), not these literals.

## Steps to Reproduce
1. Create a VPC 10.77.0.0/16 with subnet 10.77.1.0/24, an internet gateway and default route, plus an instance profile carrying AmazonSSMManagedInstanceCore.
2. CreateLaunchTemplate version 1 whose UserData writes a generation marker to /etc/sim2real-ltgen; create an Auto Scaling group on it with DesiredCapacity=2 (min 2, max 4) and wait for both instances InService.
3. CreateLaunchTemplateVersion -> version 2 (new UserData generation).
4. StartInstanceRefresh(AutoScalingGroupName=<group>, Strategy="Rolling", DesiredConfiguration={LaunchTemplate: {LaunchTemplateId, Version: "2"}}, Preferences={MinHealthyPercentage: 100, InstanceWarmup: 0, SkipMatching: true, ScaleInProtectedInstances: "Ignore", StandbyInstances: "Ignore", AutoRollback: false}).
5. While the refresh runs, poll DescribeAutoScalingGroups and record, per sample, the number of members that are InService or in a Terminating lifecycle state.
6. The minimum observed is 1; AWS never drops below 2. Also observe the departure ordering: AWS brings each replacement InService before the original it replaces leaves service.
