Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ecs__elbv2.cfn-security-group-blocks-alb-to-ecs-traffic-negative
Issue-Key: cloudformation__ecs__elbv2.cfn-security-group-blocks-alb-to-ecs-traffic-negative|_http_get_retry#12|Timeout|time.sleep(interval_s)
Issue-Commit: ece48e4c529129499372a72f3508539e00fd9f72

# cloudformation__ecs__elbv2: the security-group-blocked ALB→ECS negative control never reaches a verdict — the CFN ECS+ALB stack does not converge inside the check budget

> _Note: this issue was identified against the code base at commit `ece48e4c529129499372a72f3508539e00fd9f72`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: a CloudFormation stack that combines an internet-facing ALB (listener + two listener rules + two target groups) with an ECS cluster/task definition/service never settles far enough for the security-group differential to be observable, so the check is cut off with no assertion performed.

AWS: recorded behavior only — nothing here is modelled as an error. CreateStack settles to a terminal StackStatus (CREATE_COMPLETE for this template) and its Outputs are readable; the sibling ECS service whose task security group DOES admit ingress from the ALB's security group has its task registered in its target group and elbv2 DescribeTargetHealth reports TargetHealth.State 'healthy', and an HTTP GET through the ALB's DNS name returns HTTP 200 with the nginx default page. The service whose task security group has NO ingress rule from the ALB's security group has its health checks dropped, so its target reaches TargetHealth.State 'unhealthy' and never 'healthy'; with every target in that group unhealthy the ALB fails open and still forwards, the task's security group drops the connection, and the ALB answers HTTP 504 (gateway timeout) after its target connect timeout — not 503. Smithy: com.amazonaws.elasticloadbalancingv2#TargetHealth.State -> TargetHealthStateEnum (initial|healthy|unhealthy|unused|draining|unavailable); DescribeTargetHealth returns TargetHealthDescriptions. The 200/504 data-plane statuses and the health-state transition are recorded behaviour.

Emulator: the check produces only `Error: Check timed out after 60.0s`. It is cut off while waiting for the stack/ECS service to converge (CreateStack is given 1080 s by the check, the two _wait_ecs_service_steady waits and the 180 s target-health waits follow), so no terminal stack status, no target-health state and no HTTP status is ever obtained — the emulator emits no status code, error code or body for this scenario at all.

Why: a client cannot tell whether the emulator honours security-group boundaries between an ALB and its ECS targets. Both the positive control (open ingress ⇒ healthy ⇒ 200) and the negative control (no ingress ⇒ unhealthy ⇒ 504) are unreachable, so an emulator that routes traffic regardless of security groups — or that never routes at all — is indistinguishable from AWS. Infrastructure tests that assert a network boundary is enforced hang instead of passing or failing with a diagnosis.

Fix: the CloudFormation provisioner's convergence for the ECS+ELBv2 resource set (AWS::ECS::Cluster / AWS::ECS::TaskDefinition / AWS::ECS::Service and AWS::ElasticLoadBalancingV2::LoadBalancer / Listener / ListenerRule / TargetGroup) so the stack and the services reach a terminal/steady state promptly, plus the ELBv2 target-registration + health-probe state and the ALB data-plane forwarding path that must observe the task security group.

Related open issue that looks similar but does NOT carry this check's replay key (the replay report lists this check as unowned, while its four siblings are owned by it): 20261114T080626Z-cloudformation-ecs-elbv2-five-cloudformation-ecs-alb-scenarios.md. Neighbouring open issues 20261111T095650Z-cloudformation-ecs-elbv2-2-net-new-causes-an.md and 20261114T200121Z-cloudformation-ecs-elbv2-2-net-new-causes-cfn.md describe the same neighbourhood but do not cover this check.

Traces: cloudformation__ecs__elbv2.cfn-security-group-blocks-alb-to-ecs-traffic-negative.

## Evidence
Failing check (unowned in the replay report):
  #### [mismatch] cloudformation__ecs__elbv2.cfn-security-group-blocks-alb-to-ecs-traffic-negative
  Error: Check timed out after 60.0s

Expected (AWS / recorded): CreateStack reaches a terminal StackStatus and publishes its Outputs; the open-SG service's task becomes a 'healthy' target and `GET http://<alb-dns-name>/` returns HTTP 200 whose body contains the nginx default page; the blocked-SG service's task becomes an 'unhealthy' target and never 'healthy', and `GET http://<alb-dns-name>/blocked/` returns HTTP 504.

Actual (emulator): no verdict at all — the check is killed at the 60 s harness cap with `Error: Check timed out after 60.0s`. No terminal stack status, no TargetHealth state and no HTTP status code are produced.

## Steps to Reproduce
1. CreateStack with a VPC, two subnets in distinct AZs, an internet-facing ALB with one listener and two path rules, two target groups, an ECS cluster and Fargate task definitions, and one ECS service whose task security group admits ingress from the ALB's security group.
2. Create the sibling service whose task security group has NO ingress from the ALB's security group, wired to the second target group.
3. Poll DescribeStacks for a terminal status and ecs DescribeServices for steady state, then elbv2 DescribeTargetHealth on both target groups.
4. Observe the run never settles: the check is cut off at 60 s before the positive control's HTTP 200 and before the blocked path's expected HTTP 504 are ever requested.
