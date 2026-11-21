Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ecs__ssm.e2e-diagnose-cloudwatch-alarm-firing-lifecycle cloudwatch__ecs__ssm.e2e-diagnose-cloudwatch-latency-breach-from-alarm-config-lifecycle
Issue-Key: cloudwatch__ecs__ssm.e2e-diagnose-cloudwatch-alarm-firing-lifecycle|_poll#12|Timeout|time.sleep(interval_s)
Issue-Key: cloudwatch__ecs__ssm.e2e-diagnose-cloudwatch-latency-breach-from-alarm-config-lifecycle|_poll#12|Timeout|time.sleep(interval_s)
Issue-Commit: 2c5dcf2b9a89c9dd56a9400ba366d007bab5eb92

# cloudwatch__ecs__ssm: both CloudWatch diagnose lifecycles stall and never reach a verdict — the deployed alarm/latency estate never converges inside the replay budget

> _Note: this issue was identified against the code base at commit `2c5dcf2b9a89c9dd56a9400ba366d007bab5eb92`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAlarms

## Description
One net-new root cause, grounded in the two failing checks the suite lists under `### cloudwatch__ecs__ssm` that are marked neither ALREADY FILED nor OWNED. Related OPEN issues that do not carry these two checks' replay keys and therefore do not cover them: 20261104T062603Z-cloudwatch-ecs-ssm-a-deployed-service-latency-alarm.md (a deployed service-latency alarm is not describable), 20261109T061533Z-cloudwatch-ecs-ssm-a-fargate-task-s-container.md (a Fargate task's container never runs) and 20261110T232635Z-cloudwatch-ecs-ssm-a-cloudformation-created-aws-cloudwatch-alarm-is-invisible.md (which owns only the third check of this service token, fargate-task-role-without-ssm-get-parameter-is-denied-the-latency-flag). A fix for this issue is confirmed only by the two checks named below passing.

Cause 1 of 1: the CloudWatch alarm-firing / latency-breach diagnose lifecycles never progress to a graded verdict — the replay is killed by its budget while waiting on the deployed estate, so the whole six-phase scenario (deploy, setup, pre_invoke, solve, cleanup, destroy) is unobservable.
AWS: recorded behavior only for the end-to-end timing; the per-step contract each lifecycle waits on is modelled — Smithy: cloudwatch DescribeAlarms returns MetricAlarms for an alarm that exists, with MetricAlarm.StateValue in [OK, ALARM, INSUFFICIENT_DATA]; GetMetricStatistics returns Datapoints for data inside the queried window; ssm GetParameter returns the stored Parameter.Value. On AWS each of these reads answers as soon as the alarm exists and the publisher has emitted data, so a lifecycle that deploys the estate, seeds metric data and flips an SSM flag observes the alarm transition and finishes well inside the scenario's own 600 s round deadline.
Emulator: neither lifecycle reaches a verdict. `e2e-diagnose-cloudwatch-alarm-firing-lifecycle` is killed with `Check timed out after 600.0s`; `e2e-diagnose-cloudwatch-latency-breach-from-alarm-config-lifecycle` is killed with `Check timed out after 60.0s`. No assertion is reported, so the emulator never produces the state one of the phases blocks on. Hypothesis: the same two defects the related open issues describe — a CloudFormation-created AWS::CloudWatch::Alarm that DescribeAlarms does not return, and a Fargate task whose container never runs and therefore publishes no metric data — leave the setup phase's bounded waits unsatisfiable, so the replay burns its whole budget rather than failing an assertion.
Why: a client driving exactly this diagnose workflow (describe the alarm the stack created, read the metric behind it, read the SSM flag that drives the workload, watch the alarm go to ALARM) never gets an answer: the calls either return an empty alarm list or never show the datapoints the alarm is defined over, so no latency diagnosis can be concluded and no automation keyed on the alarm transition ever fires.
Fix: the CloudFormation AWS::CloudWatch::Alarm resource handler must register the alarm in the CloudWatch state DescribeAlarms reads, and the ECS task data-plane must actually run the deployed service's container so its metric/SSM side effects exist; the alarm evaluator must then drive StateValue from the published datapoints. Both legs must land for these lifecycles to grade.
Traces: cloudwatch__ecs__ssm.e2e-diagnose-cloudwatch-alarm-firing-lifecycle, cloudwatch__ecs__ssm.e2e-diagnose-cloudwatch-latency-breach-from-alarm-config-lifecycle.

## Evidence
Suite: service token `cloudwatch__ecs__ssm` 0/3 (0%), 3 failed; one of the three is OWNED, the two below are not.

Expected (AWS): each lifecycle completes its six phases — the deployed alarm is describable immediately after CREATE_COMPLETE, the seeded/live metric datapoints are readable, the SSM flags read back the values written, and the alarm reaches StateValue 'ALARM' on the degraded workload — within the scenario's declared 600 s round deadline.
Actual (mismatch verdicts, no assertion reached):
  cloudwatch__ecs__ssm.e2e-diagnose-cloudwatch-alarm-firing-lifecycle — "Error: Check timed out after 600.0s"
  cloudwatch__ecs__ssm.e2e-diagnose-cloudwatch-latency-breach-from-alarm-config-lifecycle — "Error: Check timed out after 60.0s"
The two budget numbers are harness/per-run values, not requirements; the requirement is that each read the lifecycle makes answers with the state AWS would have, so the replay reaches a verdict instead of exhausting its budget.

## Steps to Reproduce
1. Deploy the lifecycle's CloudFormation estate (ECS cluster + Fargate service, SSM parameters for the latency/publish flags, an AWS::CloudWatch::Alarm over the service's latency metric) and wait for CREATE_COMPLETE.
2. Call cloudwatch DescribeAlarms for the stack's alarm name and compare the returned MetricAlarm's Namespace/MetricName/Statistic/Period/EvaluationPeriods/Threshold/ComparisonOperator/TreatMissingData with the template's declaration.
3. Poll ecs DescribeServices until runningCount == desiredCount == 1, then poll cloudwatch GetMetricStatistics for the publisher's namespace — datapoints never appear.
4. ssm PutParameter the latency mode to 'degraded' and poll DescribeAlarms for StateValue == 'ALARM' — the transition never happens and the replay exhausts its budget.
5. Re-replay checks cloudwatch__ecs__ssm.e2e-diagnose-cloudwatch-alarm-firing-lifecycle and cloudwatch__ecs__ssm.e2e-diagnose-cloudwatch-latency-breach-from-alarm-config-lifecycle.
