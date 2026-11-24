Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ec2.xsvc-autoscaling-2-ec2-terminated-member-is-replaced-at-unchanged-desired-unless-replace-unhealthy-is-suspended autoscaling__ec2.set-desired-capacity-bound-follows-the-updated-max-size
Issue-Key: autoscaling__ec2.xsvc-autoscaling-2-ec2-terminated-member-is-replaced-at-unchanged-desired-unless-replace-unhealthy-is-suspended|_poll#22|TimeoutError|raise TimeoutError(f"{what}: the run budget ended first; not graded")
Issue-Key: autoscaling__ec2.set-desired-capacity-bound-follows-the-updated-max-size|run#13|AssertionError|assert got == ("ValidationError",
Issue-Commit: a6c2e348e539d87c7f8cd286303d9f9234b5e1b8

# autoscaling__ec2: 2 net-new causes — an EC2-terminated group member is never marked Unhealthy or replaced, and SetDesiredCapacity's above-max refusal appends ": <group-name>"

> _Note: this issue was identified against the code base at commit `a6c2e348e539d87c7f8cd286303d9f9234b5e1b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetDesiredCapacity

## Description
Two net-new causes in the autoscaling__ec2 pairing, neither of whose replay keys is carried by an open issue.

Cause 1 of 2 — a group member terminated out-of-band by ec2.TerminateInstances is never marked Unhealthy and never replaced, so DesiredCapacity is never re-satisfied. Failing check: autoscaling__ec2.xsvc-autoscaling-2-ec2-terminated-member-is-replaced-at-unchanged-desired-unless-replace-unhealthy-is-suspended.
AWS: recorded behavior plus AWS's documented EC2 health check (https://docs.aws.amazon.com/autoscaling/ec2/userguide/ec2-auto-scaling-health-checks.html, https://docs.aws.amazon.com/autoscaling/ec2/userguide/as-suspend-resume-processes.html). Smithy models the member view: DescribeAutoScalingGroups returns Instances with HealthStatus and LifecycleState, and SuspendProcesses takes ScalingProcesses including ReplaceUnhealthy. AWS behavior: when EC2 reports a member terminated or stopped, Auto Scaling marks it Unhealthy, terminates it out of the group and launches a replacement at UNCHANGED DesiredCapacity, recording a scaling activity whose Cause contains "in response to an EC2 health check indicating it has been terminated or stopped" and then "in response to an unhealthy instance needing to be replaced"; a twin group with ReplaceUnhealthy suspended launches nothing until ResumeProcesses.
Emulator: after one ec2.TerminateInstances kills both twins' members and EC2 reports both instances 'terminated', the control group (nothing suspended) never produces a replacement member — the poll for a single InService member with a new instance id exhausts the 900 s budget and the check is cut off without a verdict, so the ReplaceUnhealthy differential it is built to measure is never reached either.
Why: a group silently runs below its DesiredCapacity forever. Self-healing is the core promise of an Auto Scaling group; a client that kills or loses an instance sees capacity never restored, no Unhealthy transition to observe, and — because the control side never replaces — no way to tell a correctly suspended ReplaceUnhealthy process from a broken replacement path.
Fix: the Auto Scaling health-check/reconciliation component in the autoscaling crate — the periodic reconciliation that reads each member's EC2 state must set HealthStatus Unhealthy for a terminated/stopped member, remove it from the group, launch a replacement to restore DesiredCapacity, write the two activities with AWS's Cause sentences, and skip the replacement when ReplaceUnhealthy is suspended.

Cause 2 of 2 — SetDesiredCapacity's above-MaxSize refusal re-words AWS's message by appending the group name. Failing check: autoscaling__ec2.set-desired-capacity-bound-follows-the-updated-max-size.
AWS: HTTP 400, awsQuery ErrorResponse, Error.Code ValidationError, Error.Message 'New SetDesiredCapacity value <requested> is above max value <MaxSize> for the AutoScalingGroup.' The fixed text is 'New SetDesiredCapacity value ' + ' is above max value ' + ' for the AutoScalingGroup.'; the only variable parts are the capacity from the request and the group's CURRENT stored MaxSize, and the sentence ENDS at that period — the group name is never part of it. Smithy models SetDesiredCapacity with only ScalingActivityInProgressFault and ResourceContentionFault, so a capacity-bound refusal is the service-common ValidationError and the sentence is recorded behavior only.
Emulator: the same HTTP 400 and Error.Code ValidationError, but Error.Message '... for the AutoScalingGroup: <group-name>' — AWS's terminating period is replaced by ': ' plus the group name. The bound itself tracks the lowered MaxSize correctly and the refused call changes nothing, so only the wording diverges.
Why: ValidationError is the generic code for every Auto Scaling argument refusal, so the message is the only machine-visible statement of which rule failed. A caller or audit pipeline matching AWS's sentence never recognises the rejection, and since the appended name is a per-run value the emulator's text differs on every run, so it cannot be matched by a fixed string of its own either.
Fix: the message builder in the autoscaling crate's SetDesiredCapacity handler, in the above-MaxSize branch of its bound check — emit AWS's sentence and stop at 'for the AutoScalingGroup.' The identical wording defect is described for OTHER check ids by open issues 20261113T163227Z-autoscaling-cloudformation-ec2-setdesiredcapacity-s-above-max-refusal-appends-group-name.md and 20261107T081710Z-autoscaling-auto-scaling-capacity-bound-refusals-6-net-new.md; neither carries this check's replay key (which exercises the bound AFTER UpdateAutoScalingGroup lowers and re-raises MaxSize), so it is filed here.

Traces: autoscaling__ec2.xsvc-autoscaling-2-ec2-terminated-member-is-replaced-at-unchanged-desired-unless-replace-unhealthy-is-suspended (Cause 1); autoscaling__ec2.set-desired-capacity-bound-follows-the-updated-max-size (Cause 2).

## Evidence
Behavioral replay of simparity-behavioral-aws-v1 against the emulator built from this checkout (160/441 checks passed; the emulator is up and answering).

Cause 1 — autoscaling__ec2.xsvc-autoscaling-2-ec2-terminated-member-is-replaced-at-unchanged-desired-unless-replace-unhealthy-is-suspended [error]:
  control_after = _poll(lambda: _members(asg, control), ...)
  TimeoutError: the control group replacing its EC2-terminated instance <instance-id>: the run budget ended first; not graded
  Expected: the control group (nothing suspended) shows exactly one member whose instance id differs from the killed one and whose LifecycleState is InService, with (MinSize, DesiredCapacity, MaxSize) unchanged at (0, 1, 2); the killed member first reads HealthStatus Unhealthy.
  Actual: the group kept the terminated member / no replacement ever appeared; the poll ran out the 900 s budget, so the check reached no verdict at all. (The instance id is a per-run value.)

Cause 2 — autoscaling__ec2.set-desired-capacity-bound-follows-the-updated-max-size [mismatch]:
  assert got == ('ValidationError', 'New SetDesiredCapacity value 3 is above max value 2 for the AutoScalingGroup.')
  AssertionError: SetDesiredCapacity 3 against the lowered max 2 was answered ('ValidationError', 'New SetDesiredCapacity value 3 is above max value 2 for the AutoScalingGroup: sim2real-asgbound-<id>')
  Fixed text: 'New SetDesiredCapacity value … is above max value … for the AutoScalingGroup.'; variables: 3 = requested capacity, 2 = the group's current MaxSize. 'sim2real-asgbound-<id>' is a per-run generated group name and is not part of AWS's message. Error.Code is correct; only the message diverges, by the ': <group-name>' suffix replacing the terminating period.

## Steps to Reproduce
Cause 1:
1. Create a launch template and two identical Auto Scaling groups (MinSize 0, DesiredCapacity 1, MaxSize 2) in one subnet; wait until each has one InService member.
2. autoscaling SuspendProcesses(subject, ScalingProcesses=['ReplaceUnhealthy']); leave the control group untouched.
3. ec2 TerminateInstances(InstanceIds=[control_member, subject_member]) and wait until EC2 reports both 'terminated'.
4. Poll DescribeAutoScalingGroups on the control group: AWS marks the member Unhealthy, drops it and launches a replacement at DesiredCapacity 1 (activities quoting 'in response to an EC2 health check indicating it has been terminated or stopped' and 'in response to an unhealthy instance needing to be replaced'); the emulator never produces a replacement.
5. The subject group must launch nothing until ResumeProcesses(['ReplaceUnhealthy']) — unreachable today because step 4 never completes.

Cause 2:
1. Create an Auto Scaling group with MinSize=0, MaxSize=3, DesiredCapacity=0 and confirm SetDesiredCapacity 3 is accepted.
2. SetDesiredCapacity 0, then UpdateAutoScalingGroup(MaxSize=2) and confirm the group reads back 0/2/0.
3. POST Action=SetDesiredCapacity with DesiredCapacity=3. Expect HTTP 400, Error.Code ValidationError, Error.Message exactly 'New SetDesiredCapacity value 3 is above max value 2 for the AutoScalingGroup.'
4. The emulator answers the same status and code with '… for the AutoScalingGroup: <group-name>'.
5. Raise MaxSize back to 3 and confirm the identical SetDesiredCapacity 3 is accepted again (it is), so only the message is at fault.
