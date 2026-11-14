Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ecs__elbv2.cfn-alb-listener-rule-routes-to-ecs-service-positive cloudformation__ecs__elbv2.elbv2-listener-rule-path-based-routing-to-multiple-ecs-services-positive cloudformation__ecs__elbv2.cfn-ecs-service-health-check-grace-period-positive
Issue-Key: cloudformation__ecs__elbv2.cfn-alb-listener-rule-routes-to-ecs-service-positive|_poll_until#10|Timeout|time.sleep(interval_s)
Issue-Key: cloudformation__ecs__elbv2.elbv2-listener-rule-path-based-routing-to-multiple-ecs-services-positive|_wait_service_stable#16|Timeout|waiter.wait(
Issue-Key: cloudformation__ecs__elbv2.cfn-ecs-service-health-check-grace-period-positive|run#20|AssertionError|assert t["lastStatus"] != "STOPPED", (
Issue-Commit: 6b20299dda36b610e0eb6266197358af63fda05b

# cloudformation__ecs__elbv2: 2 net-new causes — CFN ECS+ALB listener-rule routing stacks never converge, and healthCheckGracePeriodSeconds does not protect a slow-starting task (its container is reported exited)

> _Note: this issue was identified against the code base at commit `6b20299dda36b610e0eb6266197358af63fda05b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateService

## Description
Two net-new root causes in the CloudFormation-provisioned ECS + ELBv2 surface. Neither is carried by an open issue: the nearest ones are 20261114T080626Z-cloudformation-ecs-elbv2-five-cloudformation-ecs-alb-scenarios.md (it lists cfn-alb-listener-rule-routes-to-ecs-service-positive but under a different replay key, and the replay report leaves that check unowned in this run), 20261109T144605Z-cloudformation-ecs-elbv2-4-net-new-causes-a.md and 20261108T175930Z-cloudformation-ecs-elbv2-2-net-new-causes-elbv2.md — none of them covers the two causes below.

Cause 1 of 2: a CloudFormation stack that fronts an ECS service with an ALB listener rule never converges, so path-based routing is never observable.
AWS:      recorded behavior only. The stack declares AWS::ElasticLoadBalancingV2::LoadBalancer/Listener/ListenerRule/TargetGroup plus AWS::ECS::Cluster/TaskDefinition/Service. In AWS the stack reaches CREATE_COMPLETE, DescribeServices reports runningCount == desiredCount with the PRIMARY deployment rolloutState COMPLETED, DescribeTargetHealth reports the task's awsvpc private IP with TargetHealth.State 'healthy' (no Reason when healthy — Smithy: elasticloadbalancingv2 TargetHealth.Reason "If the target state is healthy, a reason code is not provided"), a request whose path the rule matches is answered 200 by the task, and a path the rule does not match is answered by the listener's default fixed-response action (404 with the configured body).
Emulator: both checks end with `Error: Check timed out after 60.0s` — no assertion is ever reached, so the emulator drives neither the stack nor the ECS/ELBv2 reads behind it to a settled state inside the budget.
Why:      a client cannot deploy the most common containerised web shape (ALB + listener rule in front of an ECS service) and observe it working; the deploy never settles and never errors, so every downstream verification — rule matching, the default action for a non-matching path, multi-service path fan-out — hangs instead of answering or failing with a diagnosis.
Fix:      the CloudFormation provisioner's convergence for the ECS+ELBv2 resource set and the ECS service/ELBv2 target-registration state it depends on; the stack's per-resource events identify the handler that does not settle.
Traces:   cloudformation__ecs__elbv2.cfn-alb-listener-rule-routes-to-ecs-service-positive, cloudformation__ecs__elbv2.elbv2-listener-rule-path-based-routing-to-multiple-ecs-services-positive.

Cause 2 of 2: a new task revision whose container sleeps before it listens is STOPPED with stoppedReason "Essential container in task exited", so healthCheckGracePeriodSeconds never protects it.
AWS:      Smithy, ecs CreateService.healthCheckGracePeriodSeconds — "The period of time, in seconds, that the Amazon ECS service scheduler ignores unhealthy Elastic Load Balancing, VPC Lattice, and container health checks after a task has first started." With grace = 90 s and a container whose entryPoint is ["sh","-c"] and command is "sleep <n> && nginx -g 'daemon off;'", the container process stays alive for the whole command, so the task reaches and stays at lastStatus RUNNING; its ALB health checks fail for the first <n> seconds and the scheduler ignores them for the grace period, after which the target reaches TargetHealth.State 'healthy' and the SAME task is still RUNNING.
Emulator: this check gets past the deploy and the first target-health wait, rolls the slow-start revision with UpdateService(forceNewDeployment=True), and then DescribeTasks reports the new task at lastStatus 'STOPPED' with stoppedReason 'Essential container in task exited' inside the 35 s observation window. The container command is treated as having exited immediately instead of running, and the grace period is therefore never exercised.
Why:      every rolling deployment of an application that is not instantly ready (JVM/interpreter warm-up, migrations, a pre-start sleep) is killed by the emulator, and healthCheckGracePeriodSeconds — the one knob AWS gives a client to express that — has no effect. A client sees a permanent crash-loop for a task definition that is healthy on AWS, and the diagnosis it is handed ("Essential container in task exited") is wrong.
Fix:      the ECS task lifecycle/container runtime that decides when an essential container has exited (it must honour the container's entryPoint/command for its lifetime) and the service scheduler's grace-period gate that must not stop a task for failing ELB or container health checks while healthCheckGracePeriodSeconds has not elapsed.
Traces:   cloudformation__ecs__elbv2.cfn-ecs-service-health-check-grace-period-positive.

## Evidence
Cause 1 — both checks fail with no assertion reached:
  #### [mismatch] cloudformation__ecs__elbv2.cfn-alb-listener-rule-routes-to-ecs-service-positive
  Error: Check timed out after 60.0s
  #### [mismatch] cloudformation__ecs__elbv2.elbv2-listener-rule-path-based-routing-to-multiple-ecs-services-positive
  Error: Check timed out after 60.0s
Expected: stack CREATE_COMPLETE, service steady (runningCount == desired, PRIMARY rolloutState COMPLETED), target 'healthy', matching path -> 200 from the task, non-matching path -> the listener's default fixed response (404). Actual: the run is cut off at 60 s with no verdict.

Cause 2 — cloudformation__ecs__elbv2.cfn-ecs-service-health-check-grace-period-positive [mismatch]:
  File "check_cloudformation__ecs__elbv2_cfn_ecs_service_health_check_grace_period_positive.py", line 60, in run
    assert t["lastStatus"] != "STOPPED", (...)
  AssertionError: grace period 90s should have protected the new task from being stopped due to failing health checks, but it was stopped: Essential container in task exited
Expected: DescribeTasks reports lastStatus RUNNING throughout the sleep window and the same task is later the healthy target. Actual: lastStatus 'STOPPED', stoppedReason 'Essential container in task exited'.

## Steps to Reproduce
Cause 1:
1. CreateStack with an internet-facing ALB in two subnets, an HTTP listener whose default action is a fixed response (404, "no-rule-matched"), a listener rule forwarding a path pattern to a target group, and an ECS cluster + Fargate task definition + service wired to that target group (desiredCount 1).
2. Poll DescribeStacks for CREATE_COMPLETE, ecs DescribeServices for runningCount == 1 with PRIMARY rolloutState COMPLETED, and elbv2 DescribeTargetHealth for a 'healthy' target.
3. GET the matched path and an unmatched path through the ALB DNS name.
4. Observe that step 2 never settles: the check is cut off at its 60 s budget with no assertion reached.

Cause 2:
1. Deploy the same ALB+ECS stack with the service's healthCheckGracePeriodSeconds = 90 and a target group with HealthCheckIntervalSeconds 5, HealthCheckTimeoutSeconds 3, healthy/unhealthy threshold 2; wait for the service steady and the target 'healthy'.
2. RegisterTaskDefinition a new family whose only essential container has entryPoint ["sh","-c"] and command ["sleep 25 && nginx -g 'daemon off;'"] with containerPort 80.
3. UpdateService(taskDefinition=<new family>, forceNewDeployment=true) and ListTasks/DescribeTasks the new task.
4. Poll DescribeTasks for ~35 s: the new task becomes lastStatus 'STOPPED' with stoppedReason 'Essential container in task exited' instead of staying RUNNING and later becoming the healthy target.
