Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr__ecs__elbv2.elbv2-no-target-registration-without-service-lb-config
Issue-Key: ecr__ecs__elbv2.elbv2-no-target-registration-without-service-lb-config|run#14|Timeout|while time.monotonic() < deadline:
Issue-Commit: ef5208a2316f6e603f33dcb5e1ac1bc1a8b9d2da

# ecr__ecs__elbv2: the no-loadBalancers target-registration negative control reaches no verdict — a Fargate service's running task is never registered as an ELBv2 target, so the positive control never converges

> _Note: this issue was identified against the code base at commit `ef5208a2316f6e603f33dcb5e1ac1bc1a8b9d2da`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTargetHealth

## Description
Cause 1 of 1: an ECS service created WITH a loadBalancers block never registers its running task with the named target group, so the differential against a service created without that block cannot be observed at all and the check is cut off at the harness budget with no verdict.
AWS: recorded behavior only (no Smithy constraint states it). CreateService with loadBalancers=[{targetGroupArn, containerName, containerPort}] makes ECS register each running task's awsvpc ENI address with that target group: DescribeTargetHealth(TargetGroupArn=<wired tg>) reports one TargetHealthDescription within the check's 180 s window, while an otherwise identical service created with NO loadBalancers block leaves an unrelated target group in the same VPC with TargetHealthDescriptions == [] for a sustained 60 s window. The negative result is only meaningful because the positive control converges first.
Emulator: neither leg completes. The check creates the VPC/subnets/SG, the cluster, the task definition, two target groups and an ALB, then two Fargate services (desiredCount 1 each) and waits for one RUNNING task per service and for one registered target in the wired group; it makes no observable progress and is killed at the 60 s replay ceiling with no assertion ever evaluated — no task-to-target registration is performed, so DescribeTargetHealth on the wired group stays empty and the sustained emptiness of the unwired group proves nothing.
Why: the whole ECS-service-to-ELBv2 wiring is unverifiable. A client cannot confirm that declaring loadBalancers actually puts traffic on its tasks, nor that omitting it keeps a target group empty; an absent registration is indistinguishable from a correctly withheld one, so a misconfigured service looks the same as a working one and any rollout gated on target registration or target health never proceeds.
Fix: the ECS service scheduler's ELBv2 integration — a service with a loadBalancers block must register/deregister its RUNNING tasks' ENI addresses as ip targets in the named target group (and only in that one), and the target state must be visible to DescribeTargetHealth.
Traces: ecr__ecs__elbv2.elbv2-no-target-registration-without-service-lb-config.
Related open issues that do NOT carry this check's replay key: 20261118T134743Z-ecr-ecs-elbv2-the-no-loadbalancers-negative-control.md, 20261114T193643Z-ecr-ecs-elbv2-an-ecs-service-s-running.md, 20261120T151638Z-ecr-ecs-elbv2-a-fargate-task-whose-ecr.md, 20261110T220525Z-ecr-ecs-elbv2-the-ecs-container-health-alb.md.

## Evidence
Check ecr__ecs__elbv2.elbv2-no-target-registration-without-service-lb-config:

  Error: Check timed out after 60.0s

No assertion in the check body was ever reached: the flow is _create_network -> _create_cluster -> _register_task_def -> _create_target_group_only -> _create_alb_and_tg -> CreateService without loadBalancers -> CreateService with loadBalancers=[{targetGroupArn, containerName:'app', containerPort:80}] -> _wait_running_count(...,1,timeout_s=360) for each service -> _wait_target_count(<wired tg>,1,timeout_s=180) (the positive control) -> a 60 s loop asserting DescribeTargetHealth(<unwired tg>)['TargetHealthDescriptions'] == []. Expected against AWS: one TargetHealthDescription in the wired group and a sustained empty list in the unwired group. Observed: no verdict — the emulator never reaches a registered target, so the check consumes its entire budget. VPC/subnet/SG/cluster/task-definition names and ARNs are per-run values.

## Steps to Reproduce
1. ec2: create a VPC with two subnets (MapPublicIpOnLaunch), an IGW + default route, and a security group allowing intra-group TCP.
2. ecs.create_cluster; ecs.register_task_definition(networkMode='awsvpc', requiresCompatibilities=['FARGATE'], cpu='256', memory='512', one container named 'app' listening on 80).
3. elbv2.create_target_group twice in that VPC (target_type ip): one left unused, one attached to an internet-facing ALB listener.
4. ecs.create_service twice with desiredCount=1 and awsvpc networkConfiguration on those subnets — once with no loadBalancers block, once with loadBalancers=[{'targetGroupArn': <wired tg>, 'containerName': 'app', 'containerPort': 80}].
5. Poll ecs.describe_services for runningCount==1 on both, then elbv2.describe_target_health(TargetGroupArn=<wired tg>) for one target: AWS registers it; the emulator never does, and describe_target_health on the wired group stays empty while the check's budget expires.
