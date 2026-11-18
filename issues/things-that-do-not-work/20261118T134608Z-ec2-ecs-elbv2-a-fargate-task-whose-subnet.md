Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__ecs__elbv2.ecsnt-scaling-the-isolated-service-up-stops-its-task-before-any-target-registers
Issue-Key: ec2__ecs__elbv2.ecsnt-scaling-the-isolated-service-up-stops-its-task-before-any-target-registers|run#20|AssertionError|assert got["lastStatus"] == "RUNNING", (
Issue-Commit: 505e225be56e8a8dc67d417f965dd4a20cae2870

# ec2__ecs__elbv2: a Fargate task whose subnet HAS internet egress is stopped as 'Essential container in task exited' instead of reaching RUNNING, so no positive control can ever observe a running task

> _Note: this issue was identified against the code base at commit `505e225be56e8a8dc67d417f965dd4a20cae2870`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunTask

## Description
One net-new root cause in the ec2__ecs__elbv2 group. The open issue 20261102T085002Z-ec2-ecs-elbv2-3-net-new-ecs-root.md covers the other three checks of this heading (cluster capacityProviders, CODE_DEPLOY task sets, UpdateService gating) and does not carry this check's replay key. Neighbouring ECS issues that look similar but are filed under other service headings and do not cover this check are 20261111T145804Z-ecs-task-data-plane-container-commands-never-execute.md, 20261111T131404Z-cloudwatch-ecr-ecs-a-fargate-container-s-image.md and 20261116T130854Z-ecs-tasks-and-deployments-never-reach-a-failure.md (that last one describes the OPPOSITE symptom: a task that cannot pull its image staying RUNNING forever).

Cause 1 of 1: a Fargate task launched into an egress-capable subnet is short-circuited to STOPPED/EssentialContainerExited instead of staying RUNNING.

AWS: RunTask with launchType=FARGATE, networkConfiguration.awsvpcConfiguration naming a subnet whose route table has 0.0.0.0/0 to an attached internet gateway and assignPublicIp=ENABLED, for an awsvpc 256/512 task definition whose single container is essential=true with image public.ecr.aws/docker/library/nginx:<tag> and portMappings containerPort 80, pulls the image and advances the task PENDING -> RUNNING, and the task STAYS at lastStatus 'RUNNING' until something stops it: the nginx image's default command runs in the foreground as the container's PID 1 and does not exit. Smithy: ecs#Task$lastStatus documents the task lifecycle; ecs#Task$stopCode is ecs#TaskStopCode whose member 'EssentialContainerExited' is for a task whose essential container actually ran and exited, and ecs#Task$stoppedReason carries the reason. The fixed AWS sentence for that stop code is 'Essential container in task exited' - the wording the emulator emits is AWS's wording; what diverges is that AWS does not reach that state at all for a long-lived server image with working egress. Recorded behavior only for the timing: the check records the public twin reaching RUNNING in roughly 30 s on real AWS. The emulator gets the negative half of the same check right - the sibling task in the no-egress isolated subnets does settle STOPPED with stopCode 'TaskFailedToStart' and stoppedReason beginning 'CannotPullContainerError', and desiredCount/runningCount settle 1/0 with no target turning healthy - so the divergence is specific to the egress-capable positive control.

Emulator: the same task definition run in the public subnet with assignPublicIp=ENABLED settles lastStatus 'STOPPED' with stoppedReason 'Essential container in task exited'. The container is treated as having started and immediately exited, so the task never passes through RUNNING.

Why: a client can never observe a Fargate task that is actually serving. Every positive control that needs one RUNNING task - and therefore any ALB/NLB target that could register and pass a health check - is unreachable, so an ECS service behind a target group can only ever be observed broken. Worse, the emulator reports the SAME terminal shape ('STOPPED') for a genuinely failed launch and for a healthy long-lived container, so a client cannot distinguish an image-pull/egress fault from a working task; a diagnosis tool reading stopCode/stoppedReason concludes the customer's container exits on its own in a correctly routed subnet.

Fix: the ECS task data plane in the ecs crate - the RunTask handler and the task state machine that advances Task.lastStatus/stopCode/stoppedReason (and the container runtime it drives). A container whose image command is long-running must hold the task at RUNNING (with startedAt set and no stopCode/stoppedReason) instead of being advanced to STOPPED with stopCode EssentialContainerExited; EssentialContainerExited must be reserved for a container that really exited.

Traces: ec2__ecs__elbv2.ecsnt-scaling-the-isolated-service-up-stops-its-task-before-any-target-registers.

## Evidence
Check ec2__ecs__elbv2.ecsnt-scaling-the-isolated-service-up-stops-its-task-before-any-target-registers, verdict [mismatch]:

  File "check_ec2__ecs__elbv2_ecsnt_scaling_the_isolated_service_up_stops_its_task_before_any_target_registers.py", line 74, in run
    assert got["lastStatus"] == "RUNNING", (
  AssertionError: the public twin did not run: 'STOPPED' 'Essential container in task exited'

Expected (AWS): DescribeTasks on the task returned by RunTask reports lastStatus 'RUNNING', with no stopCode and no stoppedReason, within the check's 300 s settle budget.
Actual (emulator): lastStatus 'STOPPED', stoppedReason 'Essential container in task exited'.

The earlier assertions of the same check passed against the emulator, which scopes the defect: the isolated-subnet task did report stopCode 'TaskFailedToStart' and a stoppedReason starting 'CannotPullContainerError', DescribeServices reported (desiredCount, runningCount) == (1, 0), and no target health state ever became 'healthy'. Only the egress-capable twin diverges.

Requirement vocabulary: 'Essential container in task exited' is AWS's fixed sentence for stopCode EssentialContainerExited (ecs#TaskStopCode) and has no interpolated values; the task identifiers, cluster name, ARNs and timings in any single run are per-run values, not part of the requirement.

## Steps to Reproduce
Replay the behavioral suite and re-run the check in trace_ids. Directly: (1) create a VPC and an internet gateway, attach it, create a route table with 0.0.0.0/0 to the gateway and associate it with a /24 subnet in one availability zone; (2) create a security group in that VPC allowing inbound TCP 80 and all egress; (3) create an ECS cluster with capacityProviders FARGATE and FARGATE_SPOT, an ecs-tasks execution role carrying arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy and an ecs-tasks task role; (4) RegisterTaskDefinition with requiresCompatibilities ['FARGATE'], networkMode 'awsvpc', cpu '256', memory '512' and one containerDefinition {name:'app', image:'public.ecr.aws/docker/library/nginx:1.30', essential:true, portMappings:[{containerPort:80, protocol:'tcp'}]}; (5) RunTask on that cluster with launchType 'FARGATE', count 1 and awsvpcConfiguration {subnets:[<the public subnet>], securityGroups:[<the group>], assignPublicIp:'ENABLED'}; (6) poll DescribeTasks on the returned taskArn. Expected: lastStatus settles 'RUNNING' and stays there, with no stopCode/stoppedReason. Observed: lastStatus 'STOPPED' with stoppedReason 'Essential container in task exited'.
