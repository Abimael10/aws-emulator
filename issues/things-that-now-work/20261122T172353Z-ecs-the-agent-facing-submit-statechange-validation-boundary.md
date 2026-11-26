Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.task-state-change-server-enforced
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs.task-state-change-server-enforced|_poll_until#23|Timeout|time.sleep(interval_s)
Issue-Commit: a54ade7ee60c9fa02ca1ca5cbef236b2fbd7f50e

# ecs: the agent-facing Submit*StateChange validation boundary never reaches a verdict — the EC2-backed task the positive control needs never becomes observable

> _Note: this issue was identified against the code base at commit `a54ade7ee60c9fa02ca1ca5cbef236b2fbd7f50e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SubmitTaskStateChange

## Description
Cause 1 of 1: the server-side validation boundary of the agent-facing state-change APIs is unreachable — the scenario makes no observable progress and is cut off before any of its four refusal/no-op assertions runs (failing check: ecs.task-state-change-server-enforced).

AWS: SubmitTaskStateChange, SubmitContainerStateChange and SubmitAttachmentStateChanges are agent-facing and validated server-side. Smithy (com-amazonaws-ecs-amazonec2containerservicev20141113): all three model AccessDeniedException (HTTP 403), ClientException, ClusterNotFoundException, InvalidParameterException (HTTP 400) and ServerException, and SubmitTaskStateChange's input carries cluster/task/status/containers/attachments/managedAgents. The specific refusals are recorded behavior: (1) a task-state report against a FARGATE task — which has no verifiable container-instance identity — is refused 403 AccessDeniedException with the fixed text 'Fargate task state changes from the agent must have the instance verified'; (2) an out-of-enum attachment status on a real attachment ARN is refused 400 InvalidParameterException with the fixed text 'attachmentStatus should be one of [ATTACHED]'; (3) a containers[] entry naming a container that is not part of the task is refused 400 InvalidParameterException with the message 'Unable to find container <containerName> for task <taskId>' — fixed text 'Unable to find container ' / ' for task ', with <containerName> taken verbatim from the request and <taskId> the task-id segment of the task ARN in the request — and the task's container list is left untouched (no fabricated container record); (4) an idempotent RUNNING report for a task on a genuinely registered EC2 container instance is accepted as a no-op (lastStatus stays RUNNING), as is an idempotent SubmitContainerStateChange RUNNING for one of its containers. Reaching (3) and (4) also requires that RunTask on a Fargate task returns an ElasticNetworkInterface attachment inline and that an ECS-optimized EC2 instance with ECS_CLUSTER user data registers as a container instance whose StartTask'd task reaches lastStatus RUNNING.

Emulator: the check is terminated by the harness ('Check timed out after 60.0s') and reported as a mismatch with no assertion reached — the prerequisites the refusal boundary is asserted against (the inline ENI attachment, the registered container instance, a task that reaches RUNNING) never become observable inside the budget, so neither the three refusals nor the two idempotent no-ops are ever exercised.

Why: a client or agent-shaped tool driving the state-change APIs gets no signal at all: the Fargate boundary is unverified (an unauthorized report may be silently accepted), an out-of-enum attachment status may be accepted, and a payload naming a bogus container may mutate task state instead of being refused — all failure modes that only appear in production. Tests that assert these codes/messages hang instead of failing.

Fix: the task lifecycle/placement state that must drive a StartTask'd task on a registered container instance to RUNNING and return the Fargate task's ENI attachment inline, plus the request validation in the handlers for SubmitTaskStateChange / SubmitContainerStateChange / SubmitAttachmentStateChanges (identity check for Fargate tasks, attachmentStatus enum, container-name lookup against the stored task) and their error classification/messages. Related but distinct open issues: 20261120T023605Z-ecs-an-ec2-instance-launched-with-an-ecs.md and 20261113T124512Z-ecs-8-net-new-causes-container-dependson-health.md; neither carries this check's replay key.

Traces: ecs.task-state-change-server-enforced.

## Evidence
Expected (AWS):
- ecs submit-task-state-change --cluster <c> --task <fargate-task-arn> --status RUNNING → 403 AccessDeniedException, message 'Fargate task state changes from the agent must have the instance verified'.
- ecs submit-attachment-state-changes --cluster <c> --attachments attachmentArn=<real attachment arn>,status=NOT_A_REAL_STATUS → 400 InvalidParameterException, message 'attachmentStatus should be one of [ATTACHED]'.
- ecs submit-task-state-change --cluster <c> --task <ec2-task-arn> --containers containerName=not-a-real-container-on-this-task,status=RUNNING → 400 InvalidParameterException, message 'Unable to find container not-a-real-container-on-this-task for task <task-id>' (the container name echoed from the request, the task id from the request's task ARN), and DescribeTasks still reports the original container list with no added record.
- ecs submit-task-state-change --task <ec2-task-arn> --status RUNNING and ecs submit-container-state-change --containerName app --status RUNNING on an already-RUNNING task → accepted, lastStatus remains RUNNING.

Actual (emulator): 'Error: Check timed out after 60.0s'. The check is reported as a mismatch having reached none of those calls' assertions; the Fargate RunTask ENI attachment, the registered container instance and the RUNNING EC2-backed task never become observable within the budget.

## Steps to Reproduce
1. ecs create-cluster.
2. ecs register-task-definition family=<fg> networkMode=awsvpc requiresCompatibilities=[FARGATE] cpu=256 memory=512 with one container 'app'.
3. ecs run-task --launch-type FARGATE --network-configuration awsvpcConfiguration={subnets=[<subnet>],securityGroups=[<sg>],assignPublicIp=ENABLED}; confirm the returned task carries an attachments[] entry of type ElasticNetworkInterface.
4. ecs submit-task-state-change --cluster <c> --task <that task arn> --status RUNNING → expect 403 AccessDeniedException with the Fargate-verification message.
5. ecs submit-attachment-state-changes with the real attachment ARN and status NOT_A_REAL_STATUS → expect 400 InvalidParameterException 'attachmentStatus should be one of [ATTACHED]'.
6. Launch an ECS-optimized EC2 instance with an instance profile and UserData 'ECS_CLUSTER=<c>'; poll list/describe-container-instances until it registers.
7. ecs register-task-definition (bridge/default network mode) and ecs start-task onto that container instance; poll describe-tasks until lastStatus=RUNNING.
8. Repeat the idempotent RUNNING submit-task-state-change / submit-container-state-change (expect no-ops) and the unknown-container payload (expect the InvalidParameterException above and an unchanged container list). Observe instead that the scenario never converges and is cut off with no verdict.
