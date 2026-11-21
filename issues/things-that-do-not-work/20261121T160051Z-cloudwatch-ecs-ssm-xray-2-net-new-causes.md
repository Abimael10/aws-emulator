Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ecs__ssm__xray.e2e-diagnose-xray-service-latency-cause-lifecycle cloudwatch__ecs__ssm__xray.e2e-diagnose-xray-latency-cause-from-alarm-config-lifecycle
Issue-Key: cloudwatch__ecs__ssm__xray.e2e-diagnose-xray-service-latency-cause-lifecycle|phase#13|AssertionError|raise AssertionError(f"[phase {i}/{len(self.phases)} {name}] {exc}") from exc
Issue-Key: cloudwatch__ecs__ssm__xray.e2e-diagnose-xray-latency-cause-from-alarm-config-lifecycle|_poll#13|Timeout|time.sleep(interval_s)
Issue-Commit: 2c5dcf2b9a89c9dd56a9400ba366d007bab5eb92

# cloudwatch__ecs__ssm__xray: 2 net-new causes — a running Fargate publisher's X-Ray segments are never indexed (GetTraceSummaries stays empty), and the second X-Ray diagnose lifecycle never reaches a verdict

> _Note: this issue was identified against the code base at commit `2c5dcf2b9a89c9dd56a9400ba366d007bab5eb92`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetTraceSummaries

## Description
2 net-new root causes, grounded in the two failing checks the suite lists under `### cloudwatch__ecs__ssm__xray` that are marked neither ALREADY FILED nor OWNED (the third check of this service token is OWNED by 20261110T232635Z-cloudwatch-ecs-ssm-xray-a-cloudformation-created-aws-cloudwatch-alarm-is-invisible.md). Related OPEN issues that do not carry these checks' replay keys and therefore do not cover them: 20261104T034544Z-cloudwatch-ecs-ssm-xray-a-launched-fargate-task.md and 20261120T040123Z-xray-batchgettraces-re-serializes-the-stored-segment-document.md; the resolved 20261109T131428Z-cloudwatch-ecs-ssm-xray-an-aws-cloudwatch-alarm.md does not cover them either.

=== Cause 1 of 2: a Fargate service that is reported 1/1 RUNNING publishes no X-Ray segment that GetTraceSummaries will index ===
AWS: Smithy: xray PutTraceSegments (POST /TraceSegments, 200, output UnprocessedTraceSegments) accepts segment documents from a workload; xray GetTraceSummaries (POST /TraceSummaries, 200, output TraceSummaries/ApproximateTime/TracesProcessedCount, FilterExpression input member) then returns one TraceSummary per indexed trace, each carrying ServiceIds[].Name for the service that emitted it, so a `service("<name>")` filter over a recent window returns the publisher's traces. A containerized publisher that is running therefore becomes visible in the trace index within seconds.
Emulator: with the deployed ECS service reporting runningCount == desiredCount == 1, GetTraceSummaries filtered to the estate's minted service name over the last 15 minutes returns an empty list for 240 s of polling: `[phase 2/6 setup] the publisher's first frame trace not met within 240s; last []`. Nothing the container is supposed to emit becomes retrievable.
Why: a client that instruments a service with the X-Ray SDK/daemon and then diagnoses latency by querying GetTraceSummaries and BatchGetTraces gets an empty trace index for a workload the control plane reports as healthy — the entire trace-based diagnosis path (find the trace, read its segment document, attribute the latency to a downstream call) is unavailable, and so is any alarm/triage runbook built on it.
Fix: the ECS task data-plane (the deployed task's container must actually execute so it emits segments) together with the X-Ray ingest/index path behind PutTraceSegments — a submitted segment must be indexed so GetTraceSummaries returns a TraceSummary whose ServiceIds[].Name is the emitting service and whose id resolves through BatchGetTraces.
Traces: cloudwatch__ecs__ssm__xray.e2e-diagnose-xray-service-latency-cause-lifecycle (phase 2/6 setup).

=== Cause 2 of 2: the alarm-config X-Ray diagnose lifecycle never reaches a graded verdict ===
AWS: recorded behavior only for the end-to-end timing; each read the lifecycle makes is modelled and answers immediately on AWS — cloudwatch DescribeAlarms returns the MetricAlarm a CREATE_COMPLETE stack created (with StateValue in [OK, ALARM, INSUFFICIENT_DATA]), GetMetricStatistics returns Datapoints inside the queried window, ssm GetParameter returns the stored value, and xray GetTraceSummaries/BatchGetTraces resolve the publisher's trace — so the scenario finishes inside its declared 600 s round deadline.
Emulator: the check is killed with `Check timed out after 60.0s` without reaching any assertion, so the emulator never produces the state one of the phases blocks on. Hypothesis: the same two legs as Cause 1 and as the OWNED alarm-invisibility defect — a CloudFormation-created AWS::CloudWatch::Alarm that DescribeAlarms does not return, and a container that never runs so neither metrics nor segments exist — leave the setup waits unsatisfiable.
Why: the whole read-only diagnose trajectory (alarm -> metric -> trace summary -> segment document) hangs instead of answering, so a client can never attribute a latency breach, and no verdict about the estate is reachable.
Fix: the CloudFormation AWS::CloudWatch::Alarm handler (register the alarm in the CloudWatch state DescribeAlarms reads), the ECS task data-plane (run the container), and the X-Ray ingest/index path — the same components as Cause 1 plus the alarm registration leg.
Traces: cloudwatch__ecs__ssm__xray.e2e-diagnose-xray-latency-cause-from-alarm-config-lifecycle.

## Evidence
Suite: service token `cloudwatch__ecs__ssm__xray` 0/3 (0%), 3 failed; one is OWNED, the two below are not.

Cause 1 — cloudwatch__ecs__ssm__xray.e2e-diagnose-xray-service-latency-cause-lifecycle:
Expected (AWS): after the deployed service reaches 1/1 RUNNING, GetTraceSummaries with a `service("<minted-service-name>")` filter over the last 15 minutes returns at least one TraceSummary, every summary's ServiceIds[].Name including that service.
Actual (mismatch): AssertionError "[phase 2/6 setup] the publisher's first frame trace not met within 240s; last []" — TraceSummaries stays `[]` for the whole 240 s poll. The minted service name and the 240 s poll are per-run values; the requirement is that a running publisher's segments are indexed and returned.

Cause 2 — cloudwatch__ecs__ssm__xray.e2e-diagnose-xray-latency-cause-from-alarm-config-lifecycle:
Expected (AWS): the six-phase lifecycle grades (alarm describable with the template's Namespace/MetricName/ExtendedStatistic/Period/EvaluationPeriods/Threshold/ComparisonOperator/TreatMissingData, metric datapoints readable, SSM flags round-tripping, alarm reaching ALARM on degraded latency, one trace document retrievable).
Actual (mismatch): "Error: Check timed out after 60.0s" — no assertion reached. The budget number is a harness/per-run value, not a requirement.

## Steps to Reproduce
1. Deploy the X-Ray latency estate (ECS cluster + Fargate publisher service, SSM latency-mode and publish-dependency-metrics parameters, an AWS::CloudWatch::Alarm on the service's p90 latency metric) and wait for CREATE_COMPLETE.
2. Poll ecs DescribeServices until runningCount == desiredCount == 1.
3. Cause 1: call xray GetTraceSummaries for the last 15 minutes with FilterExpression `service("<stack output TraceServiceName>")` and poll — TraceSummaries stays empty; no trace id is ever available to pass to BatchGetTraces.
4. Cause 2: run the alarm-config variant of the lifecycle (DescribeAlarms on the stack's alarm, GetMetricStatistics on the publisher namespace, PutParameter the latency mode to 'degraded', poll for StateValue 'ALARM', then GetTraceSummaries/BatchGetTraces) — the replay exhausts its budget without reaching a verdict.
5. Re-replay checks cloudwatch__ecs__ssm__xray.e2e-diagnose-xray-service-latency-cause-lifecycle and cloudwatch__ecs__ssm__xray.e2e-diagnose-xray-latency-cause-from-alarm-config-lifecycle.
