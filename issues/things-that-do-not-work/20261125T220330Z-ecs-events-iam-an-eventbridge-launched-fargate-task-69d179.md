Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__events__iam.ecs-target-assign-public-ip-governs-the-eventbridge-launched-tasks-real-public-ip
Issue-Key: ecs__events__iam.ecs-target-assign-public-ip-governs-the-eventbridge-launched-tasks-real-public-ip|_await_attached#19|AssertionError|assert att["status"] == "ATTACHED", (
Issue-Commit: 2c036246c52324f8f473f1b5e9b5314460be2843

# ecs__events__iam: an EventBridge-launched Fargate task's ElasticNetworkInterface attachment settles DELETED instead of ATTACHED — now for the AssignPublicIp=ENABLED target too, so the public-IP contract is unobservable

> _Note: this issue was identified against the code base at commit `2c036246c52324f8f473f1b5e9b5314460be2843`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTasks

## Description
Cause 1 of 1: the ElasticNetworkInterface attachment of an awsvpc/FARGATE task launched by an EventBridge ECS target reaches the terminal status DELETED straight away, and this now happens for the AssignPublicIp=ENABLED target as well — the half that previously reached ATTACHED. The check fails on its FIRST task (the 'public' marker, AssignPublicIp=ENABLED, run line 67) rather than on the DISABLED twin, so no EventBridge-launched task's ENI attachment reaches an attached state at all.

AWS:      Smithy (ecs) Attachment$status documents the valid values PRECREATED, CREATED, ATTACHING, ATTACHED, DETACHING, DETACHED, DELETED and FAILED; Attachment$details for an elastic network interface carries the network interface ID, the MAC address, the subnet ID and the private IPv4 address. Smithy AwsVpcConfiguration$assignPublicIp governs only whether the task's elastic network interface receives a public IP address — not whether the ENI is created and attached. The attachment's state sequence is therefore recorded behavior only: a task placed with awsvpc networking has its single ElasticNetworkInterface attachment driven through ATTACHING to ATTACHED as the task moves PENDING -> RUNNING, for AssignPublicIp=ENABLED and DISABLED alike, and only moves to DETACHED/DELETED after the task has stopped and its interface has been torn down. Once the attachment is ATTACHED, EC2's own record of that interface reports an Association.PublicIp that is a globally routable address when the target set AssignPublicIp=ENABLED and no Association.PublicIp when it set DISABLED. The requirement is the terminal value ATTACHED for a task that was placed; the attachment id, eni-id, MAC, subnet id and private IPv4 address of any one run are values from the emulator's own state and are not part of it.
Emulator: DescribeTasks reports the ENABLED target's task with an ElasticNetworkInterface attachment already in terminal status 'DELETED', with its details fully populated (subnetId, networkInterfaceId eni-..., macAddress, privateIPv4Address in 172.31.0.0/16). The check's wait, which accepts ATTACHED/DELETED/FAILED as terminal, ends on DELETED, so the AssignPublicIp differential the check exists to measure is never evaluated.
Why:      a client that reads Task$attachments to resolve a freshly launched task's ENI — to look up its address through EC2 DescribeNetworkInterfaces, to register the task's IP as an ELBv2 ip target, or simply to confirm AssignPublicIp took effect — is handed a torn-down interface for a task it has just launched, and cannot distinguish 'task running' from 'networking failed'. Any automation gated on attachments[].status == 'ATTACHED' never proceeds.
Fix:      the ECS task-lifecycle/attachment state that creates the awsvpc ENI attachment and transitions its status (as surfaced by DescribeTasks attachments[]), on the EventBridge-ECS-target placement path: the attachment must settle ATTACHED while the task runs and move to DELETED only after the task stops.
Note:     the open issue 20261122T102325Z-ecs-events-iam-an-eventbridge-launched-fargate-task.md describes this class for the AssignPublicIp=DISABLED twin only, and states explicitly that the ENABLED target's attachment did reach ATTACHED; it does not cover the ENABLED-side failure reported here and does not carry this replay key. The resolved issue 20261117T115655Z-ecs-events-iam-a-fargate-task-launched-by.md claimed the same class and is marked fixed, but the behavior reproduces at this commit on the other target, so a resolved issue does not cover it.
Traces:   ecs__events__iam.ecs-target-assign-public-ip-governs-the-eventbridge-launched-tasks-real-public-ip.

## Evidence
Check ecs__events__iam.ecs-target-assign-public-ip-governs-the-eventbridge-launched-tasks-real-public-ip [mismatch].

Expected (AWS): DescribeTasks on each of the two EventBridge-launched tasks reports an attachment of type 'ElasticNetworkInterface' with status 'ATTACHED' and details containing networkInterfaceId 'eni-...'; EC2 DescribeNetworkInterfaces then shows Association.PublicIp set to a globally routable address for the AssignPublicIp=ENABLED target's ENI and no Association.PublicIp for the DISABLED target's ENI.

Actual (emulator), raised at run line 67 on the ENABLED ('public') target — i.e. the first of the two tasks, not the DISABLED twin:

  AssertionError: ENI attachment for task arn:aws:ecs:<region>:<account>:task/<cluster>/<task-id> ended 'DELETED' instead of ATTACHED: {'id': '<attachment-id>', 'type': 'ElasticNetworkInterface', 'status': 'DELETED', 'details': [{'name': 'subnetId', 'value': 'subnet-...'}, {'name': 'networkInterfaceId', 'value': 'eni-...'}, {'name': 'macAddress', 'value': '02:00:...'}, {'name': 'privateIPv4Address', 'value': '172.31.0.x'}]}

(check_ecs__events__iam_ecs_target_assign_public_ip_governs_the_eventbridge_launched_tasks_real_public_ip.py line 263, reached from line 67.) The task ARN, attachment id, eni id, MAC and private IPv4 shown are this run's values; the divergence is the terminal status 'DELETED' where AWS reports 'ATTACHED'.

## Steps to Reproduce
1. Create an ECS cluster, an ecs-tasks.amazonaws.com task role, and an events.amazonaws.com role with ecs:RunTask on * and iam:PassRole on the task role.
2. RegisterTaskDefinition: networkMode awsvpc, requiresCompatibilities [FARGATE], cpu 256 / memory 512, one essential long-running container.
3. PutRule on the default bus with EventPattern {"source":[<tok>]}; PutTargets two ECS targets on that cluster and task definition, both LaunchType FARGATE in the same subnet and security group, differing only in EcsParameters.NetworkConfiguration.awsvpcConfiguration.AssignPublicIp (ENABLED with Input marker 'public', DISABLED with marker 'private').
4. PutEvents an event matching the rule; poll ListTasks/DescribeTasks until one task per marker exists.
5. Poll DescribeTasks for each task's attachments[] entry of type ElasticNetworkInterface: the ENABLED target's attachment is reported with status 'DELETED' instead of ever reaching 'ATTACHED', so step 6 is never reached.
6. (Unreached) Read each attachment's networkInterfaceId through EC2 DescribeNetworkInterfaces and compare Association.PublicIp against the target's AssignPublicIp.
