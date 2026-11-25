Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr__ecs__elbv2.deploy-cfn-negative-missing-service-target-group-binding
Issue-Key: ecr__ecs__elbv2.deploy-cfn-negative-missing-service-target-group-binding|_poll_until#13|Timeout|last = probe()
Issue-Commit: ece48e4c529129499372a72f3508539e00fd9f72

# ecr__ecs__elbv2: the missing-LoadBalancers negative control never reaches a verdict — the ECR-image-backed CFN ECS+ALB stack does not converge inside the check budget

> _Note: this issue was identified against the code base at commit `ece48e4c529129499372a72f3508539e00fd9f72`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: a CloudFormation stack holding two ECR-image-backed ECS services on one target group — one WITH a LoadBalancers binding, one deliberately WITHOUT — never settles far enough for the binding differential to be observable, so the check is cut off with no assertion performed.

AWS: recorded behavior only; nothing in this scenario is an error. CreateStack settles to a terminal StackStatus and publishes its Outputs (including the target group ARN); both services place a RUNNING task with a private IP that ecs ListTasks/DescribeTasks report. The service that declares LoadBalancers has its task IP registered in the target group and elbv2 DescribeTargetHealth reports TargetHealth.State 'healthy' for it. The sibling that omits LoadBalancers is never registered at all: its task IP does not appear in DescribeTargetHealth's TargetHealthDescriptions in ANY state (not merely 'unhealthy'), and ecs DescribeServices reports that service's `loadBalancers` as an empty list. Smithy: com.amazonaws.ecs#Service.loadBalancers -> LoadBalancers (list, absent/empty when not declared); com.amazonaws.elasticloadbalancingv2#DescribeTargetHealth output TargetHealthDescriptions with TargetHealth.State -> TargetHealthStateEnum.

Emulator: the check produces only `Error: Check timed out after 60.0s`. It is cut off inside the stack-deploy wait (900 s allowed by the check) / the task-IP and target-health polls, so neither the terminal stack status, nor the healthy registration of the wired service, nor the expected-empty registration of the unwired service is ever obtained — no status code, error code or body is produced.

Why: a client cannot tell whether the LoadBalancers binding has any effect. Because the positive control (declared binding ⇒ registered and healthy) and the negative control (no binding ⇒ never a target, empty `loadBalancers`) are both unreachable, an emulator that registers every running task regardless of the binding, or that registers nothing at all, is indistinguishable from AWS. Deployment tests of the standard ECR→ECS→ALB shape hang instead of yielding a verdict.

Fix: the CloudFormation provisioner's convergence for the ECR/ECS/ELBv2 resource set (AWS::ECR pull-through-cache-backed task definitions, AWS::ECS::Cluster/Service, AWS::ElasticLoadBalancingV2::LoadBalancer/Listener/TargetGroup) so the stack reaches a terminal status promptly, plus the ECS scheduler/task state DescribeTasks reads and the ELBv2 target-registration state DescribeTargetHealth reads — registration must follow the service's loadBalancers binding and nothing else.

Related open issues that look similar but do NOT carry this check's current replay key (the replay report lists this check as unowned): 20261114T081024Z-ecr-ecs-elbv2-the-cfn-negative-control-for.md (same check, older key `_poll_until#13|Timeout`), 20261111T095650Z-ecr-ecs-elbv2-ecr-image-backed-ecs-services.md, 20261112T060648Z-ecr-ecs-elbv2-an-ecr-image-backed-fargate.md and 20261110T143849Z-ecr-ecs-elbv2-an-ecs-service-created-with.md.

Traces: ecr__ecs__elbv2.deploy-cfn-negative-missing-service-target-group-binding.

## Evidence
Failing check (unowned in the replay report):
  #### [mismatch] ecr__ecs__elbv2.deploy-cfn-negative-missing-service-target-group-binding
  Error: Check timed out after 60.0s

Expected (AWS / recorded): CreateStack reaches a terminal StackStatus and publishes TargetGroupArn; both services have a RUNNING task with a private IP; elbv2 DescribeTargetHealth contains the LoadBalancers-wired service's task IP with TargetHealth.State 'healthy'; the LoadBalancers-less service's task IP appears in NO TargetHealthDescriptions entry, and ecs DescribeServices reports its `loadBalancers` as [].

Actual (emulator): no verdict at all — the check is killed at the 60 s harness cap with `Error: Check timed out after 60.0s`. No terminal stack status, no task IPs, no target-health descriptions and no `loadBalancers` comparison are produced.

## Steps to Reproduce
1. Create an ECR pull-through-cache rule for the public registry and CreateStack with a VPC/two subnets, an internet-facing ALB, a listener, one target group, an ECS cluster, a task definition referencing the cached image, and two ECS services on that cluster — one declaring LoadBalancers against the target group and one omitting it.
2. Poll DescribeStacks for a terminal status, then ecs ListTasks/DescribeTasks for each service's task private IP.
3. Call elbv2 DescribeTargetHealth on the stack's target group and ecs DescribeServices on the LoadBalancers-less service.
4. Observe the run never settles: the check is cut off at 60 s before the wired service's healthy registration or the unwired service's absence from the target group can be asserted.
