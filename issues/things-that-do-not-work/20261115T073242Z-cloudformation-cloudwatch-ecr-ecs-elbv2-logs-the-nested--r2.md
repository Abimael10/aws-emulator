Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__cloudwatch__ecr__ecs__elbv2__logs.cfn-nested-stack-microservice-platform-alarm-echoes-declared-period cloudformation__cloudwatch__ecr__ecs__elbv2__logs.cfn-nested-stack-microservice-platform-workflow
Issue-Key: cloudformation__cloudwatch__ecr__ecs__elbv2__logs.cfn-nested-stack-microservice-platform-alarm-echoes-declared-period|_deploy_cfn#21|AssertionError|raise AssertionError(f"Stack {name} ended in {stack['StackStatus']}")
Issue-Key: cloudformation__cloudwatch__ecr__ecs__elbv2__logs.cfn-nested-stack-microservice-platform-workflow|_deploy_cfn#21|AssertionError|raise AssertionError(f"Stack {name} ended in {stack['StackStatus']}")
Issue-Commit: d3cdd55e8b72332e5c71fd34997111c995fed5da

# cloudformation__cloudwatch__ecr__ecs__elbv2__logs: the nested-stack microservice-platform stack rolls back on create, so both platform scenarios never get a deployed estate

> _Note: this issue was identified against the code base at commit `d3cdd55e8b72332e5c71fd34997111c995fed5da`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: a nested-stack template that provisions a whole microservice platform (ECR repository, ECS cluster/task definition/service, ELBv2 load balancer + target group + listener, CloudWatch Logs group and a CloudWatch alarm) never reaches CREATE_COMPLETE — the root stack rolls back.
AWS: recorded behavior only (CloudFormation resource handlers are not in the Smithy model). CreateStack on that template reaches CREATE_COMPLETE: every AWS::CloudFormation::Stack child reaches CREATE_COMPLETE and the parent follows, after which the declared alarm Period is readable through CloudWatch DescribeAlarms and the ECS service is reachable through the load balancer.
Emulator: the root stack settles in ROLLBACK_COMPLETE, so neither scenario ever has an estate to assert against. Both checks fail at the same point in their shared deploy helper with 'Stack <name> ended in ROLLBACK_COMPLETE'.
Why: a nested multi-service platform template — the shape every real CDK/CFN microservice deployment takes — cannot be deployed at all. Nothing downstream (alarm period fidelity, the end-to-end workflow through the ALB) is reachable, and the caller gets a rolled-back stack where AWS returns a working platform.
Fix: the CloudFormation provisioner's nested-stack create path and the resource handlers for the types in this template (AWS::ECR::Repository, AWS::ECS::Cluster/TaskDefinition/Service, AWS::ElasticLoadBalancingV2::LoadBalancer/TargetGroup/Listener, AWS::Logs::LogGroup, AWS::CloudWatch::Alarm) — one of them fails its create and triggers the rollback; the failing resource's CREATE_FAILED event names it.
Failing checks: cloudformation__cloudwatch__ecr__ecs__elbv2__logs.cfn-nested-stack-microservice-platform-alarm-echoes-declared-period, cloudformation__cloudwatch__ecr__ecs__elbv2__logs.cfn-nested-stack-microservice-platform-workflow.

## Evidence
Expected: CreateStack on the nested microservice-platform template reaches CREATE_COMPLETE (every child AWS::CloudFormation::Stack CREATE_COMPLETE).
Actual (both checks, in the shared _deploy_cfn helper): AssertionError: Stack <stack name> ended in ROLLBACK_COMPLETE.
  - cfn-nested-stack-microservice-platform-alarm-echoes-declared-period: 'Stack sim2real-xsvc-stk-6ad6142b494c ended in ROLLBACK_COMPLETE'
  - cfn-nested-stack-microservice-platform-workflow: 'Stack sim2real-xsvc-stk-fdf89b79d26a ended in ROLLBACK_COMPLETE'
(The stack names are per-run values; the diverging fact is the terminal status ROLLBACK_COMPLETE instead of CREATE_COMPLETE.)

## Steps to Reproduce
1. Replay the behavioral suite and re-run both checks in trace_ids.
2. CreateStack the nested microservice-platform template (parent with AWS::CloudFormation::Stack children covering ECR, ECS cluster + task definition + service, ELBv2 LB/target group/listener, Logs log group and a CloudWatch alarm).
3. Poll DescribeStacks on the parent to a terminal status — it is ROLLBACK_COMPLETE.
4. DescribeStackEvents on the parent and on each child to identify which resource's CREATE_FAILED triggered the rollback.
