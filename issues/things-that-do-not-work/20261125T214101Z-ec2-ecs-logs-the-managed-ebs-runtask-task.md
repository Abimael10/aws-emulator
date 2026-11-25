Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__ecs__logs.deep-ecs-1-guestdepth-ebs-volume-configured-at-launch
Issue-Key: ec2__ecs__logs.deep-ecs-1-guestdepth-ebs-volume-configured-at-launch|run#19|AssertionError|assert running.get("lastStatus") == "RUNNING", (
Issue-Commit: 76a27d9bdd16e996b43ee8be0e1161c3fa717030

# ec2__ecs__logs: the managed-EBS RunTask task no longer reaches RUNNING — it is stopped immediately as "Essential container in task exited", so the volume/mount behavior behind it is unreachable

> _Note: this issue was identified against the code base at commit `76a27d9bdd16e996b43ee8be0e1161c3fa717030`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunTask

## Description
Cause 1 of 1: a FARGATE task launched with a managed EBS volumeConfigurations entry is settled as STOPPED with stoppedReason "Essential container in task exited" instead of reaching RUNNING, although its essential container's command ends in `sleep 900`.

AWS: Smithy ecs#RunTask accepts volumeConfigurations: TaskVolumeConfigurations for a task definition volume with configuredAtLaunch=true, and ecs#Task.lastStatus is "The last known status for the task". A task whose single essential container prints its mount/partition/df probe output and then runs `sleep 900` reaches lastStatus RUNNING and STAYS RUNNING for the life of that command; "Essential container in task exited" (stopCode EssentialContainerExited) is AWS's fixed text for a task whose essential container has genuinely terminated, and it is not emitted while the command is still running. Recorded behavior: DescribeTasks settles at RUNNING and only then are the EBS attachment and the in-guest mount observable.

Emulator: DescribeTasks reports lastStatus='STOPPED' with stoppedReason='Essential container in task exited' before the task can be observed RUNNING, so the check now fails at its RUNNING assertion. This is EARLIER than the failure the open issue 20261118T053334Z-ec2-ecs-logs-2-net-new-causes-a-7de377.md records for this same check (that issue's cause 2 observes a RUNNING task whose guest simply has no /data mount, and its replay key is the mount assertion); the task not reaching RUNNING at all is a distinct, net-new behavior that now hides it, and it is not the configuredAtLaunch refusal wording that issue's cause 1 covers.

Why: a client that supplies a managed EBS volume at RunTask is told its task exited immediately, so the volume attachment, the requested filesystemType/size inside the container and the volume's deletion on task stop are all unobservable, and a correct request is indistinguishable from one whose container really failed.

Fix: the ECS task-lifecycle component behind RunTask — advance a placed task to RUNNING and actually run the container definition's command, setting stoppedReason/stopCode only on real termination; the managed-EBS mount realisation tracked by 20261118T053334Z sits behind it.

Traces: ec2__ecs__logs.deep-ecs-1-guestdepth-ebs-volume-configured-at-launch.

## Evidence
Expected (AWS): after RunTask(launchType=FARGATE, count=1, awsvpcConfiguration, volumeConfigurations=[{name:'ebs', managedEBSVolume:{roleArn:<infra role>, sizeInGiB:4, volumeType:'gp3', filesystemType:'ext4', ...}}]) on a task definition with volumes=[{name:'ebs', configuredAtLaunch:true}] and a container mountPoint at /data whose command ends in `sleep 900`, DescribeTasks reports lastStatus 'RUNNING'.

Actual (emulator): AssertionError: task never reached RUNNING: lastStatus='STOPPED' stoppedReason='Essential container in task exited' (check line 115, before any volume or in-guest assertion). The volume name 'ebs' and containerPath '/data' come from the request; cluster/task/volume ids are per-run values.

## Steps to Reproduce
1. Create a subnet + security group, a CloudWatch Logs log group, an execution role (logs:CreateLogStream, logs:PutLogEvents), an ECS infrastructure role trusting ecs.amazonaws.com with AmazonECSInfrastructureRolePolicyForVolumes, and an ECS cluster.
2. RegisterTaskDefinition with volumes=[{name:'ebs', configuredAtLaunch:true}], one FARGATE LINUX/X86_64 essential container with mountPoints=[{sourceVolume:'ebs', containerPath:'/data', readOnly:false}], an awslogs logConfiguration, and a command that prints its mount/partition/df probe sections, writes and re-reads /data/probe.txt, prints READY and then runs `sleep 900`.
3. RunTask with volumeConfigurations=[{name:'ebs', managedEBSVolume:{roleArn:<infra role>, sizeInGiB:4, volumeType:'gp3', filesystemType:'ext4'}}].
4. Poll DescribeTasks. Expected lastStatus 'RUNNING'; observed 'STOPPED' with stoppedReason 'Essential container in task exited'.
