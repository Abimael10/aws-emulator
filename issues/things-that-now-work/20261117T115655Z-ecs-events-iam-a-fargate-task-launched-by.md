Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__events__iam.ecs-target-assign-public-ip-governs-the-eventbridge-launched-tasks-real-public-ip
Issue-Key: ecs__events__iam.ecs-target-assign-public-ip-governs-the-eventbridge-launched-tasks-real-public-ip|_await_attached#20|AssertionError|assert att["status"] == "ATTACHED", (
Issue-Commit: 9d0149c39ab5c73582ec7b48950d23789498e457

# ecs__events__iam: a Fargate task launched by an EventBridge ECS target has its ENI attachment settle DELETED instead of ATTACHED, so AssignPublicIp can never be observed on the task's real network interface

> _Note: this issue was identified against the code base at commit `9d0149c39ab5c73582ec7b48950d23789498e457`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTasks

## Description
Cause 1 of 1 — the ElasticNetworkInterface attachment of an EventBridge-launched Fargate task reaches the terminal status DELETED.
AWS: a Fargate task launched by an EventBridge rule's ECS target with an awsvpcConfiguration gets exactly one `ElasticNetworkInterface` attachment, and while the task is being placed and run that attachment settles at status ATTACHED, carrying details subnetId, networkInterfaceId (`eni-...`), macAddress and privateIPv4Address. DELETED/FAILED are the attachment's statuses after the task has stopped and its interface has been torn down, not the state a freshly launched task settles in. Only once the attachment is ATTACHED can the target's AssignPublicIp be checked against EC2's own record of the interface: with ENABLED, DescribeNetworkInterfaces reports an Association.PublicIp that is a globally routable address; with DISABLED on an identical twin target in the same rule, it reports none. Recorded behavior only (DescribeTasks models the attachment status as a free-form string).
Emulator: for the task launched by the twin target, the attachment reaches terminal status `DELETED` straight away — `{'type': 'ElasticNetworkInterface', 'status': 'DELETED', 'details': [{'name': 'subnetId', ...}, {'name': 'networkInterfaceId', 'value': 'eni-...'}, {'name': 'macAddress', ...}, {'name': 'privateIPv4Address', ...}]}` — while the sibling task's attachment did reach ATTACHED. So the attachment lifecycle of an EventBridge-launched task is not stable: one of two identically configured tasks is reported with a torn-down interface.
Why: a client that waits for a task's ENI to attach in order to read its address (a health check, a security-group or public-IP assertion, anything that joins ECS's attachment to EC2's interface record) sees a terminal DELETED and must conclude the task's networking failed, even though the task was launched. The AssignPublicIp contract of the ECS target becomes unobservable.
Fix: the ECS task state's attachment lifecycle for tasks launched through the EventBridge ECS target path — the ElasticNetworkInterface attachment must settle ATTACHED while the task is running and only move to DELETED when the task stops. The open issue 20261104T062603Z-ecs-events-iam-eventbridge-ecs-targets-are-accepted.md covers other EventBridge/ECS target checks in this service and does not carry this check's replay key.
Traces: ecs__events__iam.ecs-target-assign-public-ip-governs-the-eventbridge-launched-tasks-real-public-ip.

## Evidence
`AssertionError: ENI attachment for task arn:aws:ecs:<region>:<account>:task/<cluster>/<task-id> ended 'DELETED' instead of ATTACHED: {'id': '...', 'type': 'ElasticNetworkInterface', 'status': 'DELETED', 'details': [{'name': 'subnetId', 'value': 'subnet-...'}, {'name': 'networkInterfaceId', 'value': 'eni-...'}, {'name': 'macAddress', 'value': '...'}, {'name': 'privateIPv4Address', 'value': '...'}]}`.
Expected: attachment status ATTACHED for both the AssignPublicIp=ENABLED and the AssignPublicIp=DISABLED task, then ec2 describe-network-interfaces reporting a global Association.PublicIp for the ENABLED one and none for the DISABLED one.

## Steps to Reproduce
1. Create an ECS cluster, a task role, an EventBridge invoke role (ecs:RunTask on *, iam:PassRole on the task role) and register a Fargate task definition.
2. Create a rule on a unique source and PutTargets two ECS targets differing only in EcsParameters.NetworkConfiguration.awsvpcConfiguration.AssignPublicIp (ENABLED and DISABLED), each with a distinct SIM2REAL_TARGET container-override marker.
3. PutEvents on that source until both targets have launched a task.
4. Poll ecs describe-tasks for each task's ElasticNetworkInterface attachment — both must reach ATTACHED, not DELETED.
5. Read each attachment's networkInterfaceId through ec2 describe-network-interfaces and compare Association.PublicIp against the target's AssignPublicIp.
