Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__ecs__logs.deep-ecs-1-guestdepth-ebs-runtask-without-volume-configuration-rejected ec2__ecs__logs.deep-ecs-1-guestdepth-ebs-volume-configured-at-launch
Issue-Key: ec2__ecs__logs.deep-ecs-1-guestdepth-ebs-runtask-without-volume-configuration-rejected|run#15|AssertionError|assert outcome["message"] == _EXPECTED_MESSAGE, (
Issue-Key: ec2__ecs__logs.deep-ecs-1-guestdepth-ebs-volume-configured-at-launch|run#35|AssertionError|assert len(mount) == 1, f"/data should be exactly one mount in the guest, got {mount!r}"
Issue-Commit: c15cdd15031c1a75a7d82b29238996f9aa6fdeea

# ec2__ecs__logs: 2 net-new causes — a RunTask configuredAtLaunch refusal carries the wrong InvalidParameterException message, and a managed EBS volume is never mounted inside the container

> _Note: this issue was identified against the code base at commit `c15cdd15031c1a75a7d82b29238996f9aa6fdeea`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunTask

## Description
Two net-new root causes in the ECS RunTask path for task definitions that declare a `configuredAtLaunch` volume. Both were clustered from the two failing checks of the `ec2__ecs__logs` group that no open issue owns; they are distinct from 20261116T034914Z-ec2-ecs-logs-a-stopped-fargate-task-s.md (containers never run / no exitCode) because here the container DOES run and its stdout reaches CloudWatch Logs — the only thing missing is the volume in its mount table.

Cause 1 of 2: RunTask's refusal for a configuredAtLaunch volume with no volumeConfigurations uses the emulator's own sentence instead of AWS's.
AWS: a `configuredAtLaunch` volume must be supplied a configuration at run time — Smithy: ecs#Volume.configuredAtLaunch documents "To configure a volume at launch time, use this task definition revision and specify a volumeConfigurations object when calling the CreateService, UpdateService, RunTask or StartTask APIs"; ecs#RunTask takes `volumeConfigurations: TaskVolumeConfigurations` and models `InvalidParameterException` (errorKind client, HTTP 400). Recorded behavior for the message: AWS refuses with HTTP 400, error code `InvalidParameterException`, and the message `Task definition has configuredAtLaunch volume but no volume configuration provided at runtime.` — that whole sentence is fixed text for this error class; it interpolates nothing from the request, no volume name and no task-definition ARN.
Emulator: the status (400) and the code (`InvalidParameterException`) are right, and no task is started, but the message is a different sentence that additionally interpolates the volume name: `Volume Configuration must be provided when the volume 'ebs' is configuredAtLaunch.` (`ebs` is the volume name from the request's task definition — the only variable part of the emulator's wording; the rest of its text is fixed and simply not AWS's text).
Why: a client that classifies or surfaces this refusal by message — SDK error-handling branches, IaC providers that pattern-match the reason they bubble up, and operators reading the failure — sees a sentence AWS never emits, so the rejection cannot be recognised as the missing-volume-configuration case.
Fix: the message produced by RunTask's volumeConfigurations validation in the ECS RunTask handler (the InvalidParameterException raised when a task definition volume has configuredAtLaunch=true and the request supplies no matching volumeConfigurations entry).
Traces: ec2__ecs__logs.deep-ecs-1-guestdepth-ebs-runtask-without-volume-configuration-rejected.

Cause 2 of 2: a managed EBS volume supplied at RunTask is attached in EC2 state but never mounted into the container's filesystem.
AWS: when RunTask supplies `volumeConfigurations[].managedEBSVolume` (roleArn, sizeInGiB, volumeType, filesystemType) for the task definition's configuredAtLaunch volume, and a container declares `mountPoints[] {sourceVolume, containerPath}` for it, ECS creates and attaches the EBS volume AND the container sees it as a filesystem at the requested containerPath: in the guest, /proc/mounts contains exactly one row for that path whose fstype is the requested filesystemType, /proc/partitions lists the backing device at the requested size, df reports just under the requested size, and the container can write a file there and read it back. (Smithy: ecs#TaskManagedEBSVolumeConfiguration.sizeInGiB/volumeType/filesystemType, ecs#MountPoint.containerPath; the in-guest mount state itself is recorded behavior only.)
Emulator: everything on the API side is right — the task reaches lastStatus RUNNING, exactly one volume carries the run's tag, it is 4 GiB gp3, its State is `in-use` and it has exactly one `attached` attachment — and the container genuinely runs, because its probe output (BEGIN-MOUNT/END-MOUNT, BEGIN-PART, BEGIN-DF, READY) is delivered to the awslogs stream. But the mount section is empty: `grep ' <containerPath> ' /proc/mounts` yields nothing, so the guest has no filesystem at the containerPath at all and the volume is never mounted into the container.
Why: a client that puts task data on a managed EBS volume silently writes into the container's ephemeral layer (or fails on a missing directory) while the control plane reports the volume as attached, so durable-storage behaviour, the requested filesystem type and the requested size are all unverifiable and wrong at run time.
Fix: the RunTask task-execution path that realises `volumeConfigurations[].managedEBSVolume` — it must bind the attached volume into the container's filesystem at each matching `mountPoints[].containerPath` (with the requested filesystemType and size) rather than only recording the EC2-side attachment.
Traces: ec2__ecs__logs.deep-ecs-1-guestdepth-ebs-volume-configured-at-launch.

## Evidence
Cause 1 — expected (AWS): RunTask refused with HTTP 400, Code `InvalidParameterException`, Message `Task definition has configuredAtLaunch volume but no volume configuration provided at runtime.` (whole sentence fixed text). Actual (emulator): HTTP 400 and Code `InvalidParameterException` as expected, but Message `Volume Configuration must be provided when the volume 'ebs' is configuredAtLaunch.` — reported by the check as `AssertionError: the InvalidParameterException did not name the unconfigured configuredAtLaunch volume as its cause, so it may be some other parameter's rejection: "Volume Configuration must be provided when the volume 'ebs' is configuredAtLaunch."`. `ebs` is the volume name taken from the request; the surrounding text is the emulator's own fixed wording, not AWS's.

Cause 2 — expected (AWS): with volumeConfigurations[].managedEBSVolume {sizeInGiB:4, volumeType:gp3, filesystemType:ext4} and a container mountPoint on that volume, the guest's /proc/mounts has exactly one row for the containerPath with fstype ext4, /proc/partitions shows the backing device at 4194304 KiB, df reports just under 4 GiB for it, and the probe write/read-back succeeds. Actual (emulator): `AssertionError: /data should be exactly one mount in the guest, got []` — the mount list extracted between the guest's BEGIN-MOUNT and END-MOUNT markers is empty. The preceding assertions all passed, so the divergence is isolated to the in-guest mount: the task is RUNNING, exactly one tagged volume exists with Size 4 / VolumeType gp3 / State in-use and one attachment in state `attached`, the awslogs stream `ecs/app/<taskId>` exists and carries the guest's markers including READY. `/data` is the containerPath from the request and `ebs` the volume name; the generated cluster/task/volume ids and the stream name are per-run values.

## Steps to Reproduce
Shared setup: create a VPC subnet + security group, a CloudWatch Logs log group, an ECS task-execution role (logs:CreateLogStream, logs:PutLogEvents), an ECS infrastructure role trusting ecs.amazonaws.com with AmazonECSInfrastructureRolePolicyForVolumes, and an ECS cluster.

Cause 1:
1. RegisterTaskDefinition with volumes=[{name:'ebs', configuredAtLaunch:true}], one FARGATE LINUX/X86_64 container that declares mountPoints=[{sourceVolume:'ebs', containerPath:'/data'}] and an awslogs logConfiguration.
2. Call RunTask (launchType FARGATE, platformVersion LATEST, count 1, awsvpcConfiguration) WITHOUT `volumeConfigurations`.
3. Observe the error: status 400 and Code `InvalidParameterException` are correct, but the Message is `Volume Configuration must be provided when the volume 'ebs' is configuredAtLaunch.` instead of AWS's `Task definition has configuredAtLaunch volume but no volume configuration provided at runtime.`

Cause 2:
1. RegisterTaskDefinition as above, with the container command printing its mount table and partitions, writing and re-reading a probe file under /data, then printing READY and sleeping (BEGIN-MOUNT/END-MOUNT, BEGIN-PART/END-PART, BEGIN-DF/END-DF markers around each section).
2. Call RunTask with volumeConfigurations=[{name:'ebs', managedEBSVolume:{roleArn:<infra role>, sizeInGiB:4, volumeType:'gp3', filesystemType:'ext4', tagSpecifications:[{resourceType:'volume', tags:[{key:'sim2real:ebs-run', value:<run tag>}]}]}}].
3. Poll DescribeTasks until lastStatus=RUNNING; DescribeVolumes filtered on the run tag shows one 4 GiB gp3 volume, in-use, with one `attached` attachment — all as AWS.
4. Read the log stream `ecs/app/<taskId>` until READY appears and extract the lines between BEGIN-MOUNT and END-MOUNT → empty, i.e. the guest has no /data mount; the requested ext4 filesystem and 4 GiB device are absent from /proc/partitions and df as well.
