Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__logs.deep-ecs-guest-identity-awsvpc-task-reachable-on-its-eni-ip-only-while-its-sg-allows-it ecs__logs.deep-ecs-guest-identity-task-role-is-the-guests-credential-source-and-identity ecs__logs.ecs-readonly-mount-point-write-rejected-while-the-writer-sibling-still-writes
Issue-Key: ecs__logs.deep-ecs-guest-identity-awsvpc-task-reachable-on-its-eni-ip-only-while-its-sg-allows-it|_await_task#3|AssertionError|assert task["lastStatus"] == status, (
Issue-Key: ecs__logs.deep-ecs-guest-identity-task-role-is-the-guests-credential-source-and-identity|_await_task#3|AssertionError|assert task["lastStatus"] == status, (
Issue-Key: ecs__logs.ecs-readonly-mount-point-write-rejected-while-the-writer-sibling-still-writes|run#11|AssertionError|assert reader.get("exitCode") == 0, (
Issue-Commit: 3b95f364a9766d54572805b5303a28b612a84a5b

# ecs__logs: Fargate containers never execute — tasks go to STOPPED/EssentialContainerExited with no exitCode, so in-guest behaviour (ENI reachability, task-role identity, read-only mounts) is unobservable

> _Note: this issue was identified against the code base at commit `3b95f364a9766d54572805b5303a28b612a84a5b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunTask

## Description
Cause 1 of 1: a Fargate task's containers are never actually run — the task is reported STOPPED with stopCode 'EssentialContainerExited' / stoppedReason 'Essential container in task exited' and the container entries carry NO exitCode at all. Related open issues 20261116T034914Z-ecs-logs-containers-never-run-and-stopped-tasks.md, 20261111T030649Z-ecs-logs-6-net-new-causes-execution-role.md and 20261105T143800Z-ecs-logs-4-net-new-causes-awslogs-delivery.md describe adjacent symptoms but the replay report does not list these three checks as owned by any of them.
AWS: RunTask(FARGATE, awsvpc) starts each container's command. A long-running container (python http.server on 8080, or the AWS Lambda python base image running a guest script) reaches Task.lastStatus 'RUNNING' and stays there; a short-lived one runs to completion and Container.exitCode is set to "the exit code returned from the container" (Smithy: DescribeTasks -> Task.containers -> Container.exitCode, BoxedInteger). Task.stopCode is the enum TaskStopCode and 'EssentialContainerExited' plus the fixed reason text 'Essential container in task exited' apply only when the essential container's own process exited.
Emulator: every such task is reported lastStatus 'STOPPED', stopCode 'EssentialContainerExited', stoppedReason 'Essential container in task exited', and the containers are listed with no exitCode member at all (e.g. containers=[('server', None, None)], and the 'readeronly' container payload has no exitCode key) — i.e. the command never ran and no exit status exists.
Why: every behaviour that is only visible from inside the container is lost: a task is not reachable on its awsvpc ENI private IP, the task role is not the guest's credential source (AWS_CONTAINER_CREDENTIALS_RELATIVE_URI / 169.254.170.2), and a readOnly:true mountPoint is never enforced with EROFS while its read-write sibling writes. A client also cannot tell a real essential-container exit apart from a container that was never started, because exitCode is absent in both.
Fix: the ECS task data-plane container-execution component behind RunTask/DescribeTasks in the ecs service crate — it must run the container command, populate Container.exitCode, and only set Task.stopCode/stoppedReason from a real process exit.
Traces: ecs__logs.deep-ecs-guest-identity-awsvpc-task-reachable-on-its-eni-ip-only-while-its-sg-allows-it, ecs__logs.deep-ecs-guest-identity-task-role-is-the-guests-credential-source-and-identity, ecs__logs.ecs-readonly-mount-point-write-rejected-while-the-writer-sibling-still-writes.

## Evidence
Expected: Task.lastStatus == 'RUNNING' for the long-running server/guest containers; Container.exitCode == 0 for the short-lived read-only reader container.
Actual (deep-ecs-guest-identity-awsvpc-task-reachable...): AssertionError: task ... is 'STOPPED', expected 'RUNNING': stopCode='EssentialContainerExited' stoppedReason='Essential container in task exited' containers=[('server', None, None)].
Actual (deep-ecs-guest-identity-task-role...): AssertionError: task ... is 'STOPPED', expected 'RUNNING': stopCode='EssentialContainerExited' stoppedReason='Essential container in task exited' containers=[('guest', None, None)].
Actual (ecs-readonly-mount-point-write-rejected...): AssertionError: the read-only container did not run to completion: {'name': 'readeronly', 'image': 'public.ecr.aws/docker/library/busybox:1.36', 'lastStatus': 'STOPPED', ... no 'exitCode' key ...} stoppedReason='Essential container in task exited'.
Fixed text in the diverging responses: stopCode 'EssentialContainerExited', stoppedReason 'Essential container in task exited'. Task/container ARNs, the private IPv4 addresses and cluster names are per-run values, not requirements.

## Steps to Reproduce
CreateCluster; create an execution role and a log group; RegisterTaskDefinition (FARGATE, awsvpc, cpu=256/memory=512) with (a) one long-running container that serves HTTP on 8080, or (b) two busybox containers sharing one volume where one mountPoint has readOnly:true; RunTask into a public subnet with assignPublicIp=ENABLED; poll DescribeTasks. Observe lastStatus 'STOPPED' with stopCode 'EssentialContainerExited' and containers without any exitCode, instead of RUNNING (a) or exitCode 0 (b).
