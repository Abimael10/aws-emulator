Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__ecs.awsvpc-task-eni-matches-the-ec2-view-and-is-released-on-stop
Issue-Key: ec2__ecs.awsvpc-task-eni-matches-the-ec2-view-and-is-released-on-stop|_poll#5|Timeout|time.sleep(interval_s)
Issue-Commit: f520e482964e34c3af4b1cbe916bb0f71c86032d

# ec2__ecs: the awsvpc task-ENI cross-check against the EC2 view never reaches a verdict

> _Note: this issue was identified against the code base at commit `f520e482964e34c3af4b1cbe916bb0f71c86032d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeNetworkInterfaces

## Description
Cause 1 of 1: an awsvpc task's ENI cannot be reconciled with the EC2 view, nor shown to be released when the task stops.
AWS: Smithy (ecs): a RUNNING awsvpc task carries an Attachment of type ElasticNetworkInterface whose details include networkInterfaceId, macAddress, subnetId and privateIPv4Address, with Attachment.status ATTACHED; EC2 DescribeNetworkInterfaces on that networkInterfaceId reports the same subnet/IP/MAC while the task runs, and the interface is deleted after the task stops.
Emulator: recorded behavior only — the check exhausts its 60s budget with no verdict, so neither the ECS/EC2 agreement nor the release-on-stop is observable (the related data-plane defect is that these tasks never reach RUNNING and their attachments report DELETED from the start).
Why: a client that looks up a task's ENI in EC2 (for security-group or reachability work) cannot rely on the two views agreeing, nor on the ENI being cleaned up.
Fix: the ECS task attachment state machine and the EC2 ENI records it must create/describe/delete in step with the task lifecycle.
Traces: ec2__ecs.awsvpc-task-eni-matches-the-ec2-view-and-is-released-on-stop.

## Evidence
Error: Check timed out after 60.0s — no verdict. Expected DescribeTasks to report the ElasticNetworkInterface attachment ATTACHED with a networkInterfaceId that EC2 DescribeNetworkInterfaces resolves to the same subnet/private IP/MAC, and that interface to be gone after StopTask.

## Steps to Reproduce
1. Create a VPC/subnet/security group; CreateCluster; RegisterTaskDefinition (awsvpc, long-running command).
2. RunTask with awsvpcConfiguration; read the ElasticNetworkInterface attachment details from DescribeTasks and call EC2 DescribeNetworkInterfaces on the networkInterfaceId.
3. StopTask and re-describe the interface. The check never reaches a verdict inside its budget.
