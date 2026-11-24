Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: codedeploy.deployment-group-alarm-in-alarm-state-blocks-the-deployment
Issue-Shape-Id: com.amazonaws.codedeploy#CodeDeploy_20141006
Issue-Key: codedeploy.deployment-group-alarm-in-alarm-state-blocks-the-deployment|_wait_agent_active#17|TimeoutError|raise TimeoutError(f"the CodeDeploy agent on {instance_id!r} active: the run budget ended first; not graded")
Issue-Commit: 7bfda40add59a57de4c47a21257187f7eb42eb8e

# codedeploy: the alarm-gating check on the Server platform cannot start — SSM Run Command never reports the codedeploy-agent unit active on a launched managed node

> _Note: this issue was identified against the code base at commit `7bfda40add59a57de4c47a21257187f7eb42eb8e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDeployment

## Description
One net-new root cause in the codedeploy behavioral group, carrying a replay key no open issue holds.

Relation to what is already filed: open issues 20261114T164802Z-codedeploy-the-server-platform-ec2-agent-readiness-gate.md, 20261114T130417Z-codedeploy-the-server-platform-ec2-agent-surface-is.md, 20261107T094623Z-codedeploy-no-server-platform-ec2-agent-deployment-check.md and 20261110T134745Z-codedeploy-the-minimumhealthyhosts-concurrency-check-cannot-start-the.md describe the same symptom family (the Server-platform host-readiness gate never passing), and 20261108T090921Z-codedeploy-never-refuses-an-alarm-enabled-deployment-whose.md covers alarm polling permissions. None of them carries THIS check's replay key, and the suite did not mark it owned or already filed, so it is filed here with its own key.

=== Cause 1 of 1: the Server-platform agent-readiness gate never passes, so the alarm-gating (ALARM_ACTIVE) behavior of an EC2/on-premises deployment is never created or graded ===
Failing check ids for this cause: codedeploy.deployment-group-alarm-in-alarm-state-blocks-the-deployment.

AWS: a Server-platform (computePlatform 'Server') check first proves the launched host is usable by running a shell command on it through Systems Manager Run Command — ssm:SendCommand(InstanceIds=[<id>], DocumentName='AWS-RunShellScript', Parameters={commands:['systemctl is-active codedeploy-agent']}) followed by ssm:GetCommandInvocation(CommandId, InstanceId). On AWS a reachable managed node settles out of the transient statuses within seconds (Smithy ssm: CommandInvocationStatus = Pending|InProgress|Delayed|Success|Cancelled|TimedOut|Failed|Cancelling) and the invocation carries StandardOutputContent with the command's stdout — 'active' for a running codedeploy-agent unit. Only then does the check create the deployment and read back the alarm gate that is the actual subject: Smithy (codedeploy, com.amazonaws.codedeploy#CodeDeploy_20141006) models AlarmConfiguration{enabled, ignorePollAlarmFailure, alarms} on a deployment group, DeploymentStatus = Created|Queued|InProgress|Baking|Succeeded|Failed|Stopped|Ready, ErrorCode including ALARM_ACTIVE, and LifecycleEventStatus = Pending|InProgress|Succeeded|Failed|Skipped|Unknown. AWS stops a deployment created while an associated alarm is already in ALARM: deploymentInfo.status 'Stopped' with errorInformation.code 'ALARM_ACTIVE' naming the alarm, and no lifecycle hook of the revision runs on the host.
Emulator: the instance launches, SSM reports it, but the readiness poll in the check's `_wait_agent_active` helper never observes 'active' from `systemctl is-active codedeploy-agent` over SendCommand/GetCommandInvocation; the loop runs until the run budget is exhausted and the check dies before any codedeploy:CreateDeployment, with: "TimeoutError: the CodeDeploy agent on 'i-00000000000000001' active: the run budget ended first; not graded". Nothing about the alarm gate (Stopped / ALARM_ACTIVE, or the absence of the revision's AfterInstall marker on the host) is therefore produced to compare. Hypothesis: the emulated Run Command execution plane records a sent invocation against a running instance but never completes it with stdout, and no emulated host runs a codedeploy-agent unit, so the readiness condition can never become true.
Why: a client cannot use this emulator for any EC2/on-premises CodeDeploy workflow, and specifically cannot observe alarm-gated deployments: an alarm already in ALARM must stop the deployment (Stopped/ALARM_ACTIVE) before any hook runs. Because the blocking symptom is a status/output that never arrives rather than a modeled error, client wait loops hang instead of failing fast, which is worse than a refusal.
Fix: two components, in order — (1) the SSM Run Command execution plane for an emulated managed node (the state behind SendCommand/GetCommandInvocation/ListCommandInvocations must advance an invocation sent to a running instance to a terminal CommandInvocationStatus and populate StandardOutputContent/StandardErrorContent/ResponseCode for AWS-RunShellScript, including the codedeploy-agent unit being reported active on a host whose user data installs it); then (2) the CodeDeploy Server-platform deployment driver — the instance-target lifecycle and alarm-gating path behind CreateDeployment/GetDeployment/GetDeploymentTarget, which must set status 'Stopped' with errorInformation.code 'ALARM_ACTIVE' when an associated CloudWatch alarm is in ALARM at creation and run no lifecycle hook in that case.
Traces: codedeploy.deployment-group-alarm-in-alarm-state-blocks-the-deployment — cut off in the host-readiness setup (`_wait_agent_active`) before any CreateDeployment.

## Evidence
Expected (AWS): after the launched host is reported by SSM, ssm:SendCommand(DocumentName='AWS-RunShellScript', Parameters={commands:['systemctl is-active codedeploy-agent']}) + ssm:GetCommandInvocation settles on a terminal CommandInvocationStatus ('Success' for a reachable node) with StandardOutputContent == 'active'. The check then creates a Server deployment with an alarm already forced to ALARM and AWS returns deploymentInfo.status == 'Stopped' with errorInformation.code == 'ALARM_ACTIVE' (ErrorCode enum), while the revision's AfterInstall marker file is never written on the host; with the same alarm at OK the same revision deploys to 'Succeeded' with the AfterInstall lifecycle event 'Succeeded' (LifecycleEventStatus).

Actual (emulator): the readiness gate never passes. Verbatim from the replay:
- codedeploy.deployment-group-alarm-in-alarm-state-blocks-the-deployment [error]:
  File "check_codedeploy_deployment_group_alarm_in_alarm_state_blocks_the_deployment.py", line 64, in run -> active = _wait_agent_active(ctx, ssm, instance_id)
  File ... line 733, in _wait_agent_active -> raise TimeoutError(...)
  TimeoutError: the CodeDeploy agent on 'i-00000000000000001' active: the run budget ended first; not graded
No codedeploy:CreateDeployment with computePlatform 'Server' is issued, so no deployment status, errorInformation.code, lifecycle event status or on-host effect is produced.

Per-run values that are NOT part of the requirement: the instance id (i-...), the generated application / deployment-group / alarm / bucket names, the deployment id, the command id, and the run budget itself. The requirement is that a Run Command sent to a running managed node reaches a terminal CommandInvocationStatus with its stdout, and that a Server-platform deployment created while an associated alarm is in ALARM is Stopped with error code ALARM_ACTIVE before any hook runs.

## Steps to Reproduce
1. Create a VPC with a public subnet and an instance profile granting AmazonSSMManagedInstanceCore plus S3 read; read the AL2023 AMI id from the public SSM parameter /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64.
2. ec2:RunInstances one t3.micro with that profile, tagged with the key the deployment group will filter on, and user data installing the SSM Agent and the CodeDeploy agent; wait for instance state 'running' and for ssm:DescribeInstanceInformation to report it.
3. ssm:SendCommand(InstanceIds=[<id>], DocumentName='AWS-RunShellScript', Parameters={commands:['systemctl is-active codedeploy-agent']}), then poll ssm:GetCommandInvocation(CommandId=<returned id>, InstanceId=<id>) for StandardOutputContent == 'active'.
4. Observe the invocation never yields 'active' (the agent-readiness gate never passes) and the check dies with TimeoutError before creating any deployment.
5. Because of step 4 the subject behavior is never reached: cloudwatch:PutMetricAlarm on a namespace with no datapoints (TreatMissingData='notBreaching'), codedeploy:CreateApplication(computePlatform='Server') + CreateDeploymentGroup(alarmConfiguration={enabled:true, ignorePollAlarmFailure:false, alarms:[{name:<alarm>}]}, ec2TagFilters=[...], deploymentStyle={deploymentType:'IN_PLACE',deploymentOption:'WITHOUT_TRAFFIC_CONTROL'}), cloudwatch:SetAlarmState(<alarm>,'ALARM'), then codedeploy:CreateDeployment with the S3 zip revision — expected deploymentInfo.status 'Stopped' with errorInformation.code 'ALARM_ACTIVE' and no AfterInstall hook marker on the host.
