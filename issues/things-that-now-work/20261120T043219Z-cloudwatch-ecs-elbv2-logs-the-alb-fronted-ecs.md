Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ecs__elbv2__logs.alb-request-count-metric-in-cloudwatch cloudwatch__ecs__elbv2__logs.alb-target-response-time-metric-in-cloudwatch
Issue-Key: cloudwatch__ecs__elbv2__logs.alb-request-count-metric-in-cloudwatch|_poll_metric_datapoints#17|Timeout|time.sleep(30)
Issue-Key: cloudwatch__ecs__elbv2__logs.alb-target-response-time-metric-in-cloudwatch|_poll_metric_datapoints#19|Timeout|return []
Issue-Commit: c2a9aa425461796da61883782981cd948eb4a841

# cloudwatch__ecs__elbv2__logs: the ALB-fronted ECS metric scenarios make no observable progress and are cut off at the harness cap, so AWS/ApplicationELB RequestCount and TargetResponseTime are never published

> _Note: this issue was identified against the code base at commit `c2a9aa425461796da61883782981cd948eb4a841`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTargetHealth

## Description
One net-new root cause with two grounded checks (Cause 1 of 1).

Cause 1 of 1: the canonical ALB -> awsvpc Fargate ECS topology never converges far enough to serve a single successful HTTP request and never publishes the AWS/ApplicationELB request/latency series, and it now makes NO observable progress at all — both metric checks are cut off at the harness cap (600.0 s) before they even reach their own HTTP/metric assertions. Failing checks: cloudwatch__ecs__elbv2__logs.alb-request-count-metric-in-cloudwatch, cloudwatch__ecs__elbv2__logs.alb-target-response-time-metric-in-cloudwatch.

AWS: Smithy (elasticloadbalancingv2) DescribeTargetHealth -> TargetHealthDescriptions[].TargetHealth {State, Reason, Description}, errors TargetGroupNotFoundException (400 TargetGroupNotFound) / InvalidTargetException (400 InvalidTarget) / HealthUnavailableException (500 HealthUnavailable). On AWS the sequence in these checks completes inside the budget: ECS DescribeServices reports runningCount == desiredCount for a Fargate service, DescribeTargetHealth moves the registered ip target from 'initial' to 'healthy', a GET to the load balancer address is forwarded to the container and returns the container's own response, and — per the ALB CloudWatch metrics documentation these checks cite — each forwarded request increments RequestCount and publishes a TargetResponseTime sample under namespace AWS/ApplicationELB with the LoadBalancer dimension app/<name>/<id>, readable through CloudWatch GetMetricStatistics/GetMetricData over the covering 60 s periods. Every one of those steps is bounded on AWS; none of them can block for 600 s.
Emulator: both checks terminate with 'Error: Check timed out after 600.0s' and reach no assertion. The checks' own internal budgets are _wait_service_running(timeout_s=600) -> _wait_targets_healthy(timeout_s=300) -> 40 HTTP requests -> _poll_metric_datapoints(timeout_s=600), so at least one of those polls never satisfies its predicate and the harness cap fires first. Hypothesis: the hang is in service/target convergence or in the metric poll, which the open filing below reports as an endless 502/no-success path; the replay output states only the timeout, so the exact blocking poll is not attributable from this run. Net effect: no RequestCount datapoint and no TargetResponseTime sample for a load balancer that was supposed to serve real traffic.
Why: a client building the standard ALB+ECS reference topology against the emulator gets a service that never reports a routed request and metric series that stay empty, so dashboards, CloudWatch alarms on RequestCount/TargetResponseTime, autoscaling policies keyed on them, and any latency SLO are all unobservable. Worse, the call never refuses — it simply never settles — so the client has no error code to act on and sees only a hang.
Fix: the ELBv2 data-plane forwarding path (listener/rule action -> target group -> registered target proxying) together with the target-health state machine that must converge a reachable ip target to 'healthy', the ECS service scheduler that must drive runningCount to desiredCount, and the AWS/ApplicationELB metric publisher that must emit RequestCount and TargetResponseTime per forwarded request into the CloudWatch metric store these checks read.
Related open filing that does NOT carry these replay keys: 20261110T220525Z-cloudwatch-ecs-elbv2-logs-an-alb-answers-502-47cb37.md lists these same two check ids, but its replay keys are the AssertionErrors 'assert ok >= 20' (run#14) and 'assert len(latencies) >= 10' (run#15); the checks no longer reach those lines, so the suite did not mark them OWNED and they survive as live failures with a new, earlier failure signature. Adjacent open filings under this heading — 20261115T045707Z-cloudwatch-ecs-elbv2-logs-an-alb-fronted-ecs.md and 20261114T081024Z-cloudwatch-ecs-elbv2-logs-the-ecs-alb-service.md — describe neighbouring ALB/ECS convergence defects but likewise do not carry these keys.
Traces: cloudwatch__ecs__elbv2__logs.alb-request-count-metric-in-cloudwatch and cloudwatch__ecs__elbv2__logs.alb-target-response-time-metric-in-cloudwatch, both 'Check timed out after 600.0s'.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the booted emulator. Service heading cloudwatch__ecs__elbv2__logs: 16/22 (73%), 5 failed + 1 unloadable; 3 of the failures are OWNED by open filings, the two ids below survive.

[mismatch] cloudwatch__ecs__elbv2__logs.alb-request-count-metric-in-cloudwatch
  Error: Check timed out after 600.0s
  Expected: the service reaches RUNNING, the ip target reaches DescribeTargetHealth State 'healthy', >= 20 of 40 GETs through the load balancer succeed, and AWS/ApplicationELB RequestCount for dimension LoadBalancer=app/<name>/<id> sums to >= 1 over the covering periods.
  Actual: no verdict at all — the check makes no observable progress and is cut off at the 600 s cap before its first HTTP/metric assertion.

[mismatch] cloudwatch__ecs__elbv2__logs.alb-target-response-time-metric-in-cloudwatch
  Error: Check timed out after 600.0s
  Expected: >= 10 successful requests measured through the load balancer and a matching AWS/ApplicationELB TargetResponseTime series.
  Actual: no verdict at all — same 600 s cut-off.

Smithy consulted (model services/service-com-amazonaws-elasticloadbalancingv2-elasticloadbalancing-v10/smithy-model/com-amazonaws-elasticloadbalancingv2-elasticloadbalancing-v10.json): DescribeTargetHealth input {TargetGroupArn required, Targets, Include}, output TargetHealthDescriptions[], errors HealthUnavailableException/InvalidTargetException/TargetGroupNotFoundException. AWS/ApplicationELB RequestCount and TargetResponseTime semantics: recorded behavior plus the ALB CloudWatch metrics documentation the checks cite (metric period 60 s).

## Steps to Reproduce
1. Clone the repo and replay the behavioral suite; both ids in trace_ids reproduce as 'Check timed out after 600.0s'.
2. Create a VPC with two public subnets, an SG open on tcp/80 from the test CIDRs, and a task SG admitting tcp/80 from the ALB SG.
3. Register a Fargate task definition for an HTTP image on port 80, create a cluster, an ip-type target group on port 80 with health-check path '/', an internet-facing ALB across both subnets, and an HTTP:80 listener forwarding to the group.
4. Create an awsvpc ECS service with desiredCount 1 wired to that target group and poll ECS DescribeServices for runningCount == desiredCount. Expected: RUNNING within the budget. Observe whether it settles.
5. Poll ELBv2 DescribeTargetHealth for the registered target. Expected: TargetHealth.State 'healthy'. Observe whether it settles.
6. Send ~40 GETs to the load balancer address. Expected: most return the container's response.
7. Read CloudWatch AWS/ApplicationELB RequestCount (Sum) and TargetResponseTime for dimension LoadBalancer=app/<name>/<id> over the covering 60 s periods. Expected: a nonzero RequestCount sum and latency samples. Actual: the scenario never reaches this point within 600 s.
8. Re-replay both ids in trace_ids to confirm the fix.
