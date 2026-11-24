Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ecs__ssm.e2e-diagnose-cloudwatch-alarm-firing-lifecycle
Issue-Key: cloudwatch__ecs__ssm.e2e-diagnose-cloudwatch-alarm-firing-lifecycle|_alarm#12|Timeout|found = cw.describe_alarms(AlarmNames=[name], AlarmTypes=["MetricAlarm"])["MetricAlarms"]
Issue-Commit: 7a64cae84a9cfda7163325030ecd9d33dd5ead34

# cloudwatch__ecs__ssm: the CloudWatch alarm-firing diagnose lifecycle never reaches a verdict — the deployed alarm/metric estate never converges, so the replay exhausts its budget (1 cause)

> _Note: this issue was identified against the code base at commit `7a64cae84a9cfda7163325030ecd9d33dd5ead34`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAlarms

## Description
One net-new root cause, grounded in the one failing check the suite lists under `### cloudwatch__ecs__ssm` that is marked neither ALREADY FILED nor OWNED (the other two of the three failures under this token ARE marked OWNED and are skipped). Related OPEN issue that no longer carries this check's replay key (the suite attributes it only to the sibling latency-breach check, so it does not cover this one): 20261121T160051Z-cloudwatch-ecs-ssm-both-cloudwatch-diagnose-lifecycles-stall.md; also related and not covering it: 20261104T062603Z-cloudwatch-ecs-ssm-a-deployed-service-latency-alarm.md, 20261109T061533Z-cloudwatch-ecs-ssm-a-fargate-task-s-container.md and 20261110T232635Z-cloudwatch-ecs-ssm-a-cloudformation-created-aws-cloudwatch-alarm-is-invisible.md. A fix for THIS issue is confirmed only by the check named below passing.

Cause 1 of 1: the six-phase alarm-firing diagnose lifecycle (deploy, setup, pre_invoke, solve, cleanup, destroy) never progresses to a graded verdict — a phase blocks forever on state the emulator never produces, and the replay is killed by its round budget instead of failing an assertion, so the whole scenario is unobservable.
AWS: recorded behavior only for the end-to-end convergence; the per-step contract the lifecycle waits on is modelled. Smithy: cloudwatch DescribeAlarms returns MetricAlarms for an alarm that exists, each MetricAlarm carrying StateValue in [OK, ALARM, INSUFFICIENT_DATA] plus the declared Namespace/MetricName/ExtendedStatistic/Threshold/ComparisonOperator/EvaluationPeriods/Period; cloudwatch GetMetricStatistics returns Datapoints for data inside the queried window; ssm GetParametersByPath/GetParameter return the stored Parameter values. On AWS each read answers as soon as the stack's alarm exists and the workload has published data, and a p90 3-of-3 alarm over a 30 s high-resolution period observes the degraded flip within a few evaluation periods — well inside the scenario's own declared round deadline. Every call in the frozen read trajectory (DescribeAlarms, GetMetricStatistics, cloudformation DescribeStacks, ecs DescribeServices, ssm GetParametersByPath, ssm GetParameter) is a 200; there is no error code or refusal in the recorded behavior.
Emulator: the check is killed with `Error: Check timed out after 600.0s`. No assertion is reached and no error code is returned, so the emulator never produces the state a phase blocks on: the alarm the CloudFormation estate declares is not describable with the declared metric/threshold configuration, and/or the deployed Fargate workload publishes no datapoints for the metric the alarm is defined over, so StateValue never leaves its initial value and never reaches 'ALARM' after the SSM latency flag is flipped to degraded. The 600 s figure is this run's harness budget and is not itself a requirement.
Why: a client driving exactly this diagnosis — describe the alarm the stack created, read the metric behind it, read the SSM flag that drives the workload, then watch the alarm transition — never gets an answer. DescribeAlarms either omits the alarm or reports a state unrelated to the published data, GetMetricStatistics returns no datapoints, and any automation keyed on the alarm transition (notification, runbook, scaling) never fires. The breach can never be pinned on the dependency and the quiesce leg can never be shown to clear the alarm.
Fix: the CloudFormation AWS::CloudWatch::Alarm resource handler must register the alarm in the CloudWatch state DescribeAlarms reads, with the template's declared configuration echoed back; the ECS task data-plane must actually run the deployed service's container so its metric and SSM side effects exist; and the CloudWatch alarm evaluator must drive MetricAlarm.StateValue from the published datapoints over the declared period/evaluation-period/extended-statistic. All three legs must land for this lifecycle to grade.
Traces: cloudwatch__ecs__ssm.e2e-diagnose-cloudwatch-alarm-firing-lifecycle.

## Evidence
Suite: service token `cloudwatch__ecs__ssm` 0/3 (0%), 3 failed; two of the three are marked OWNED, the one below is not.

Expected (AWS): the lifecycle completes its six phases — after CREATE_COMPLETE the stack's alarm is immediately describable with the declared Namespace/MetricName/ExtendedStatistic(p90)/Threshold(15000)/ComparisonOperator/EvaluationPeriods(3)/Period(30); the seeded and live datapoints are readable through GetMetricStatistics; ssm GetParametersByPath/GetParameter read back the latency-mode flag that was written; the alarm reaches StateValue 'ALARM' while the workload is degraded and returns to OK/INSUFFICIENT_DATA when quiesced — all within the scenario's declared round deadline.

Actual (mismatch verdict, no assertion reached, no error code):
  cloudwatch__ecs__ssm.e2e-diagnose-cloudwatch-alarm-firing-lifecycle — "Error: Check timed out after 600.0s"
The budget number is a harness/per-run value; the requirement is that each read the lifecycle makes answers with the state AWS would have, so the replay reaches a verdict rather than exhausting its budget.

## Steps to Reproduce
1. CreateStack the lifecycle's estate (deployments/flint-latency/template.json: an ECS cluster with a Fargate service, the SSM parameters carrying the latency/publish flags, and an AWS::CloudWatch::Alarm over the service's latency metric at ExtendedStatistic p90, Period 30, EvaluationPeriods 3, Threshold 15000) and wait for CREATE_COMPLETE.
2. Call cloudwatch DescribeAlarms for the stack's alarm name and compare the returned MetricAlarm's Namespace/MetricName/ExtendedStatistic/Threshold/ComparisonOperator/EvaluationPeriods/Period against the template's declaration.
3. Poll ecs DescribeServices until runningCount == desiredCount, then poll cloudwatch GetMetricStatistics (Period=60, Statistics=Average) for the workload's namespace and the five component metrics (FrameQuartzLag, ProcessFrame, MotionDetection, HLSStartLag, InternalPipelineTime) — no datapoints appear.
4. ssm PutParameter the latency mode to 'degraded', then poll DescribeAlarms for StateValue == 'ALARM' — the transition never happens and the check is killed by its round budget with no assertion.
5. Re-replay check cloudwatch__ecs__ssm.e2e-diagnose-cloudwatch-alarm-firing-lifecycle.
