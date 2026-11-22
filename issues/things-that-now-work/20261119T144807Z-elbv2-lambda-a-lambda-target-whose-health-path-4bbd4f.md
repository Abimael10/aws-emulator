Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elbv2__lambda.xsvc-elbv2-3-lambda-health-check-500-and-raise-mark-only-those-targets-unhealthy
Issue-Key: elbv2__lambda.xsvc-elbv2-3-lambda-health-check-500-and-raise-mark-only-those-targets-unhealthy|run#26|AssertionError|assert bad_h.get("Reason") == "Target.ResponseCodeMismatch", (
Issue-Commit: 365042aa5aff1c761f3edc665c19518d88f112ee

# elbv2__lambda: a Lambda target whose health path returns 500 is reported unhealthy with Reason 'Target.Timeout' / 'Request timed out' instead of 'Target.ResponseCodeMismatch' / 'Health checks failed with these codes: [500]' (1 cause)

> _Note: this issue was identified against the code base at commit `365042aa5aff1c761f3edc665c19518d88f112ee`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTargetHealth

## Description
Cause 1 of 1: health checks against a `lambda`-type target group do not classify the verdict from the status code the function returned. The target does reach State='unhealthy' now, but the reason code and description served by DescribeTargetHealth are the connection-failure pair instead of the response-code-mismatch pair, so the one signal that says WHY a Lambda target is failing is wrong.

AWS: Smithy (com.amazonaws.elasticloadbalancingv2) TargetHealth.Reason is the enum TargetHealthReasonEnum, whose members include `Target.ResponseCodeMismatch`, `Target.Timeout` and `Target.FailedHealthChecks`, and whose documentation fixes which one applies: "If the target state is unhealthy, the reason code can be one of the following values: Target.ResponseCodeMismatch - The health checks did not return an expected HTTP code. Target.Timeout - The health check requests timed out. Target.FailedHealthChecks - The load balancer received an error while establishing a connection to the target or the target response was malformed." TargetHealth.Description is "A description of the target health that provides additional details. If the state is healthy, a description is not provided." For a target group with TargetType `lambda`, HealthCheckEnabled=true, HealthCheckPath set and Matcher.HttpCode="200", the load balancer invokes the registered function with the health path and evaluates the `statusCode` the handler returns against the Matcher. A handler that answers a well-formed response with statusCode 500 therefore produces State='unhealthy', Reason='Target.ResponseCodeMismatch', Description='Health checks failed with these codes: [500]'. In that Description the text 'Health checks failed with these codes: [' and ']' is fixed; the bracketed list is the set of status codes the target actually answered with on the health path, taken from the response — it is 500 here only because the handler returned 500. `Target.Timeout` is reserved for a health request that got no answer within HealthCheckTimeoutSeconds, and `Target.FailedHealthChecks` for a connection error or a malformed/absent response (which is what a handler that RAISES produces, there being no HTTP code to mismatch). Recorded behavior only for the Lambda-target health-check wiring itself: the Smithy model describes the target-group and DescribeTargetHealth shapes, not the data-plane invocation, so the invoke-and-match rule is the ELB user guide's documented behavior plus what the recording shows.

Emulator: DescribeTargetHealth on the group whose single Lambda target answers the health path with statusCode 500 returns {'State': 'unhealthy', 'Reason': 'Target.Timeout', 'Description': 'Request timed out'}. The state is right and the target is never reported healthy, but the reason code is `Target.Timeout` rather than `Target.ResponseCodeMismatch` and the description is the fixed timeout text rather than the code-naming text, i.e. the verdict is produced without ever reading the function's returned status code. Hypothesis: the lambda-type health-check path never obtains a status code at all (no invocation, or one whose result is discarded) and falls back to a timeout verdict, which would also make the raising-handler case indistinguishable from the 500 case; the check fails on the 500 target's Reason before it can assert the raising target's `Target.FailedHealthChecks`, so only the 500 classification is evidenced here.

Why: a client that reads DescribeTargetHealth to diagnose a Lambda-backed target group is told the function did not answer in time, when in fact it answered promptly with the wrong status code. Those are different operational faults with different fixes — a timeout points at HealthCheckTimeoutSeconds, cold starts or a hung handler; a code mismatch points at the handler's return value or the Matcher. Deployment gates, runbooks and dashboards that branch on Reason/Description take the wrong branch, and because every failing Lambda target collapses onto the same timeout reason, a 500-answering function cannot be told apart from one that faults or one that is genuinely slow.

Fix: the ELBv2 health-check evaluator for `lambda`-type target groups and the target-health state DescribeTargetHealth serializes from — it must invoke the registered function on the configured HealthCheckPath, compare the returned statusCode to the group's Matcher, and on a mismatch record Reason='Target.ResponseCodeMismatch' with Description='Health checks failed with these codes: [<codes the target answered>]', reserving Reason='Target.Timeout'/'Request timed out' for a health request that produced no answer within HealthCheckTimeoutSeconds and Reason='Target.FailedHealthChecks' for an invocation error or malformed response.

Related already-open issue, which does NOT carry this check's replay key (it recorded the earlier state where the unhealthy verdict was never reached at all, timing out in the poll; the states now settle and only the reason/description diverge): 20261110T143849Z-elbv2-lambda-a-lambda-target-whose-health-path.md. Also adjacent but distinct: 20261109T153103Z-elbv2-lambda-2-net-new-causes-the-alb.md (every Lambda target reported healthy regardless) and 20261114T003845Z-elbv2-lambda-an-application-load-balancer-with-a.md (the Lambda data path never answers).

Traces: elbv2__lambda.xsvc-elbv2-3-lambda-health-check-500-and-raise-mark-only-those-targets-unhealthy.

## Evidence
Check elbv2__lambda.xsvc-elbv2-3-lambda-health-check-500-and-raise-mark-only-those-targets-unhealthy [mismatch]:
  AssertionError: the target whose health path answers 500 is unhealthy with reason 'Target.Timeout' ({'State': 'unhealthy', 'Reason': 'Target.Timeout', 'Description': 'Request timed out'}), expected 'Target.ResponseCodeMismatch'
  (raised at check_elbv2__lambda_xsvc_elbv2_3_lambda_health_check_500_and_raise_mark_only_those_targets_unhealthy.py line 381: `assert bad_h.get("Reason") == "Target.ResponseCodeMismatch"`)

Expected per AWS (TargetHealth.Reason / TargetHealth.Description for the 500-answering Lambda target):
  State       = 'unhealthy'
  Reason      = 'Target.ResponseCodeMismatch'
  Description = 'Health checks failed with these codes: [500]'
                 fixed text: 'Health checks failed with these codes: [' ... ']'
                 variable part: the status code(s) the target answered on the health path (500 here because the handler returned statusCode 500 against Matcher.HttpCode='200')

Actual from the emulator:
  State       = 'unhealthy'            (matches)
  Reason      = 'Target.Timeout'       (diverges)
  Description = 'Request timed out'    (diverges)

The same poll confirms the state machine is otherwise moving: the triple (bad=unhealthy, good=healthy, err=unhealthy) was observed, and neither failing target was ever reported healthy. Only the classification of the 500 answer diverges. The check's later assertions — the raising target's Reason='Target.FailedHealthChecks' and the healthy sibling carrying no 'Reason' member — are not reached, so they are not evidenced here.

## Steps to Reproduce
1. Create a VPC with public subnets, a permissive security group and an internet-facing Application Load Balancer.
2. Create three Lambda functions from one source whose handler raises on a path ending '/hc/raise', returns {'statusCode': 500, ...} on a path ending '/hc/fail', and returns statusCode 200 otherwise. Grant elasticloadbalancing.amazonaws.com lambda:InvokeFunction on each, scoped to the matching target group ARN.
3. CreateTargetGroup three times with TargetType='lambda', HealthCheckEnabled=true, HealthCheckIntervalSeconds=5, HealthCheckTimeoutSeconds=4, HealthyThresholdCount=2, UnhealthyThresholdCount=2, Matcher={'HttpCode': '200'} and HealthCheckPath '/hc/fail', '/hc/ok' and '/hc/raise' respectively; RegisterTargets one function into each.
4. CreateListener (HTTP:80) forwarding by default to the '/hc/ok' group, and CreateRule routing /bad/* to the '/hc/fail' group and /err/* to the '/hc/raise' group, so health checks run for all three groups.
5. Poll DescribeTargetHealth on the '/hc/fail' group until its single target leaves State='initial'.
   Expected: State='unhealthy', Reason='Target.ResponseCodeMismatch', Description='Health checks failed with these codes: [500]'.
   Observed: State='unhealthy', Reason='Target.Timeout', Description='Request timed out'.
6. For contrast, poll the '/hc/raise' group: AWS reports Reason='Target.FailedHealthChecks' there, so the two failure classes must not share one reason code.
