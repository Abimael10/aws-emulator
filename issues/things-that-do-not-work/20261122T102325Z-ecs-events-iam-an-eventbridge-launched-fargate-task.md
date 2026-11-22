Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__events__iam.ecs-target-assign-public-ip-governs-the-eventbridge-launched-tasks-real-public-ip
Issue-Key: ecs__events__iam.ecs-target-assign-public-ip-governs-the-eventbridge-launched-tasks-real-public-ip|_await_attached#20|AssertionError|assert att["status"] == "ATTACHED", (
Issue-Commit: aa409114eda17ea1bc8a6cd65487c38f901b417a

# ecs__events__iam: an EventBridge-launched Fargate task with AssignPublicIp=DISABLED never gets an ATTACHED ENI — DescribeTasks reports its ElasticNetworkInterface attachment as DELETED

> _Note: this issue was identified against the code base at commit `aa409114eda17ea1bc8a6cd65487c38f901b417a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTasks

## Description
Cause 1 of 1: the ElasticNetworkInterface attachment of an awsvpc/FARGATE task launched by an EventBridge ECS target whose NetworkConfiguration.awsvpcConfiguration.AssignPublicIp is DISABLED is reported by DescribeTasks with status 'DELETED', never 'ATTACHED', while the identical twin target in the same rule with AssignPublicIp=ENABLED does reach ATTACHED.

AWS: Smithy (ecs) Attachment$status documents the valid values PRECREATED, CREATED, ATTACHING, ATTACHED, DETACHING, DETACHED, DELETED and FAILED, and Attachment$details for an elastic network interface carries networkInterfaceId, macAddress, subnetId and privateIPv4Address. Smithy AwsVpcConfiguration$assignPublicIp is documented as 'Whether the task's elastic network interface receives a public IP address' — it governs only the public-IP association on the ENI, not whether the ENI is created and attached. A task placed with awsvpc networking therefore gets its ENI attachment driven through ATTACHING to ATTACHED as the task moves PENDING -> RUNNING, for AssignPublicIp=ENABLED and DISABLED alike; the attachment only becomes DETACHED/DELETED after the task has stopped and its ENI has been torn down. The requirement is the state sequence and the terminal value ATTACHED for a task that was placed; the concrete attachment id, eni-id, MAC, subnet id and private IPv4 address in any one run are values from the emulator's own state, not part of the requirement.

Emulator: for the twin targets of one rule (Id 'public' with AssignPublicIp=ENABLED, Id 'private' with AssignPublicIp=DISABLED, same task definition, same subnet, same security group), the 'public' task's attachment reaches ATTACHED, but the 'private' task's ElasticNetworkInterface attachment is reported in a terminal 'DELETED' state by DescribeTasks, with its details already fully populated (networkInterfaceId eni-..., privateIPv4Address 172.31.0.x). The check's wait, which accepts ATTACHED/DELETED/FAILED as terminal, ends on DELETED, so the task's ENI never passes through an attached state at all. Hypothesis: the emulator tears the awsvpc attachment down (or stops the task and deletes its ENI) when the task has no public IP, instead of attaching the ENI first and letting the task's own outcome follow.

Why: a client that reads Task$attachments to resolve a task's ENI — to look up its address via ec2 DescribeNetworkInterfaces, to register the task's IP as an ELBv2 ip target, or simply to verify that AssignPublicIp took effect — sees a DELETED attachment for a task it has just launched. It cannot resolve the ENI it was handed, and it cannot distinguish 'task running with a private-only ENI' (what AWS gives it) from 'ENI torn down / task gone'. Any automation gated on attachments[].status == 'ATTACHED' never proceeds for private-networking tasks.

Fix: the ECS task-lifecycle/attachment state that creates the awsvpc ENI attachment and transitions its status (surfaced by DescribeTasks attachments[]), plus the RunTask/EventBridge-target placement path's handling of awsvpcConfiguration.assignPublicIp=DISABLED — DISABLED must suppress only the ENI's public-IP association, never the attachment reaching ATTACHED.

Related open issues record neighbouring ENI/attachment and Fargate-lifecycle defects but do not carry this check's replay key and do not cover it: 20261122T042537Z-ecs-5-net-new-causes-capacity-provider-base.md (Cause 4, SubmitAttachmentStateChanges never moves an attachment ATTACHING -> ATTACHED) and 20261118T145001Z-ecs-2-net-new-causes-a-long-running.md.

Traces: ecs__events__iam.ecs-target-assign-public-ip-governs-the-eventbridge-launched-tasks-real-public-ip

## Evidence
Check: ecs__events__iam.ecs-target-assign-public-ip-governs-the-eventbridge-launched-tasks-real-public-ip [mismatch]

Expected (AWS): DescribeTasks on each of the two EventBridge-launched tasks reports an attachment of type 'ElasticNetworkInterface' with status 'ATTACHED' and details containing networkInterfaceId/eni-*; then ec2 DescribeNetworkInterfaces shows Association.PublicIp set to a globally routable address for the AssignPublicIp=ENABLED target's ENI and no Association.PublicIp for the AssignPublicIp=DISABLED target's ENI.

Actual (emulator): the ENABLED target's task reached ATTACHED, and the DISABLED target's task ended in a terminal DELETED attachment:

  AssertionError: ENI attachment for task arn:aws:ecs:<region>:<account>:task/<cluster>/<task-id> ended 'DELETED' instead of ATTACHED: {'id': '...', 'type': 'ElasticNetworkInterface', 'status': 'DELETED', 'details': [{'name': 'subnetId', 'value': 'subnet-...'}, {'name': 'networkInterfaceId', 'value': 'eni-...'}, {'name': 'macAddress', 'value': '02:00:...'}, {'name': 'privateIPv4Address', 'value': '172.31.0.5'}]}

(raised from check_ecs__events__iam_ecs_target_assign_public_ip_governs_the_eventbridge_launched_tasks_real_public_ip.py line 263, reached from line 68.) Because the attachment is terminal-DELETED, the public-IP differential that the check exists to measure is never evaluated.

## Steps to Reproduce
1. Create a cluster, an ecs-tasks.amazonaws.com task role and an events.amazonaws.com role with ecs:RunTask on * and iam:PassRole on the task role.
2. RegisterTaskDefinition: family <tok>, cpu 256, memory 512, networkMode awsvpc, requiresCompatibilities [FARGATE], one essential container running ['sleep','600'].
3. PutRule on the default bus with EventPattern {"source":[<tok>]}; PutTargets with two ECS targets on the same cluster ARN and task definition, both LaunchType FARGATE in the same subnet and security group, one with EcsParameters.NetworkConfiguration.awsvpcConfiguration.AssignPublicIp=ENABLED (Input marker 'public') and one with AssignPublicIp=DISABLED (Input marker 'private').
4. PutEvents an event matching the rule; poll ListTasks/DescribeTasks until one task per marker exists.
5. Poll DescribeTasks for each task's attachments[] entry of type ElasticNetworkInterface: the DISABLED target's attachment reports status 'DELETED' instead of ever reaching 'ATTACHED'.
