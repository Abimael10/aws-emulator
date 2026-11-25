Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ecs__ssm.e2e-diagnose-cloudwatch-alarm-firing-lifecycle
Issue-Key: cloudwatch__ecs__ssm.e2e-diagnose-cloudwatch-alarm-firing-lifecycle|_poll#12|Timeout|last = probe()
Issue-Commit: 5dff3459c9fa0114d27ae1b22046b841878cac27

# cloudwatch__ecs__ssm: the CloudWatch alarm-firing diagnose lifecycle still reaches no verdict — the deployed alarm/metric estate never converges inside the replay budget

> _Note: this issue was identified against the code base at commit `5dff3459c9fa0114d27ae1b22046b841878cac27`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAlarms

## Description
One root cause, grounded in the single failing check the suite lists under `### cloudwatch__ecs__ssm` that the replay marked neither ALREADY FILED nor OWNED (the service token is 0/3; the other two failures are OWNED by 20261121T160051Z-cloudwatch-ecs-ssm-both-cloudwatch-diagnose-lifecycles-stall.md and 20261110T232635Z-cloudwatch-ecs-ssm-a-cloudformation-created-aws-cloudwatch-alarm-is-invisible.md). Related OPEN issues that do NOT carry this check's replay key and therefore do not cover it: 20261121T160051Z-cloudwatch-ecs-ssm-both-cloudwatch-diagnose-lifecycles-stall.md (it names this check in its prose but its recorded replay key no longer matches this failure, so the replay did not mark this check owned by it), 20261104T062603Z-cloudwatch-ecs-ssm-a-deployed-service-latency-alarm.md (a deployed service-latency alarm is not describable) and 20261109T061533Z-cloudwatch-ecs-ssm-a-fargate-task-s-container.md (a Fargate task's container never runs). A fix is confirmed only by the check named below passing.

Cause 1 of 1: the alarm-firing diagnose lifecycle never progresses to a graded verdict — the six-phase scenario (deploy, setup, pre_invoke, solve, cleanup, destroy) is killed by its budget while waiting on the deployed estate, so no phase of the diagnosis is observable. Failing check: cloudwatch__ecs__ssm.e2e-diagnose-cloudwatch-alarm-firing-lifecycle.
AWS:      recorded behavior only for the end-to-end timing; each read the lifecycle blocks on is modelled — Smithy: cloudwatch DescribeAlarms returns MetricAlarms for an alarm that exists, with MetricAlarm.StateValue in [OK, ALARM, INSUFFICIENT_DATA]; GetMetricStatistics returns Datapoints for data inside the queried window (here a high-resolution alarm metric at Period=30 over 3 evaluation periods, backfilled every 12 s); cloudformation DescribeStacks reports the stack at CREATE_COMPLETE; ecs DescribeServices reports runningCount == desiredCount; ssm GetParametersByPath / GetParameter return the stored Parameter.Value. On AWS each of these answers as soon as the resource exists and the publisher has emitted data, so a lifecycle that deploys the estate, backfills the component latency series, reads the alarm's configuration and flips an SSM flag observes the alarm reach StateValue 'ALARM' and completes well inside its own declared 600 s round deadline.
Emulator: the check produces no assertion result at all — the replay detail is `Error: Check timed out after 600.0s`, verdict mismatch. Nothing the lifecycle waits for ever becomes true, so the run burns its whole budget instead of failing a comparison. Hypothesis: the same two defects the related open issues describe — a CloudFormation-created AWS::CloudWatch::Alarm that DescribeAlarms does not return, and an ECS/Fargate task whose container never runs and therefore publishes no metric data for the FrameQuartzLag / ProcessFrame / MotionDetection / HLSStartLag / InternalPipelineTime components under the declared Service/Operation dimensions — leave the setup and solve phases' bounded waits unsatisfiable.
Why:      a client driving exactly this diagnosis (describe the alarm the stack created, read the Period=60 bucket of the metric behind it, read the SSM parameters that configure the workload, then watch the alarm transition) never gets an answer: the alarm list comes back without the alarm, or the metric behind it never shows datapoints, so no latency root cause can be concluded and no automation keyed on the alarm transition ever fires.
Fix:      the CloudFormation AWS::CloudWatch::Alarm resource handler must register the alarm in the CloudWatch state DescribeAlarms reads (with the template's Namespace/MetricName/Statistic/Period/EvaluationPeriods/Threshold/ComparisonOperator/TreatMissingData echoed back), the ECS task data plane must actually run the deployed service's container so its metric and SSM side effects exist, and the CloudWatch alarm evaluator must drive StateValue from the published high-resolution datapoints. All three legs must land for this lifecycle to grade.
Traces:   cloudwatch__ecs__ssm.e2e-diagnose-cloudwatch-alarm-firing-lifecycle.

## Evidence
Suite replay of simparity-behavioral-aws-v1 against the emulator built from this checkout: 1144/1252 passed (91.4%), bootstrap_signal false. Under `### cloudwatch__ecs__ssm`: 0/3 (0%), 3 failed; two are OWNED, this one is not.

  [mismatch] cloudwatch__ecs__ssm.e2e-diagnose-cloudwatch-alarm-firing-lifecycle
    Error: Check timed out after 600.0s

Expected (AWS): the lifecycle completes its six phases — after the stack reaches CREATE_COMPLETE the alarm is describable immediately (DescribeAlarms returns it with the template's configuration), the backfilled component latency datapoints are readable through GetMetricStatistics at Period=60, ecs DescribeServices reports the service running, the ssm parameters read back the values written, and the alarm over the degraded component reaches StateValue 'ALARM' at its declared Threshold 15000.0 with a 30 s high-resolution period over 3 evaluation periods — all inside the scenario's declared 600 s round deadline.
Actual: no assertion is ever reached; the replay is killed at the budget with no verdict, so every graded step of the diagnosis is unverified. The 600 s figure is the harness budget for this run, not a requirement; the requirement is that each read answers with the state AWS would have, so the replay reaches a verdict.

## Steps to Reproduce
1. Clone the repo, boot the emulator built from that checkout, and replay simparity-behavioral-aws-v1.
2. Deploy the lifecycle's CloudFormation estate (ECS cluster + Fargate service, SSM parameters for the latency/publish flags, an AWS::CloudWatch::Alarm over the pipeline latency metric at Period=30 with 3 evaluation periods and Threshold 15000.0) and wait for CREATE_COMPLETE.
3. Call cloudwatch DescribeAlarms for the stack's alarm name and compare the returned MetricAlarm's Namespace/MetricName/Statistic/Period/EvaluationPeriods/Threshold/ComparisonOperator/TreatMissingData with the template's declaration.
4. Backfill / let the service publish the component series (FrameQuartzLag, ProcessFrame, MotionDetection, HLSStartLag, InternalPipelineTime) under the declared Service/Operation dimensions and poll GetMetricStatistics at Period=60 for the latest bucket — datapoints never appear.
5. Flip the SSM latency flag to the degraded value, then poll DescribeAlarms for StateValue == 'ALARM' — the transition never happens and the replay exhausts its budget with no verdict.
6. Re-replay cloudwatch__ecs__ssm.e2e-diagnose-cloudwatch-alarm-firing-lifecycle to confirm the fix.
