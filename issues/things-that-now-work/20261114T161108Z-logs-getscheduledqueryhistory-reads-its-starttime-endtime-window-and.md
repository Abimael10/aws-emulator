Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: logs.e2e-scheduled-query-to-results-flow
Issue-Shape-Id: com.amazonaws.cloudwatchlogs#Logs_20140328
Issue-Key: logs.e2e-scheduled-query-to-results-flow|_poll_until#31|Timeout|time.sleep(interval_s)
Issue-Commit: c2371e1318591c291fe2d87c999629c066adc659

# logs: GetScheduledQueryHistory reads its startTime/endTime window (and reports triggeredTimestamp) in epoch SECONDS, so a millisecond window selects no run and triggerHistory is always empty

> _Note: this issue was identified against the code base at commit `c2371e1318591c291fe2d87c999629c066adc659`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetScheduledQueryHistory

## Description
One net-new root cause (Cause 1 of 1).

Cause 1 of 1: GetScheduledQueryHistory treats the request's startTime/endTime as epoch SECONDS and reports TriggerHistoryRecord$triggeredTimestamp in epoch seconds, while CloudWatch Logs states these `long` epoch instants in MILLISECONDS. A caller that asks for the window around "now" in milliseconds therefore matches no stored execution at all, so a 200 is served with an empty triggerHistory no matter how many runs the scheduled query has made. Failing check: logs.e2e-scheduled-query-to-results-flow.

AWS: Smithy (services/service-com-amazonaws-cloudwatchlogs-logs-20140328) GetScheduledQueryHistory.startTime and .endTime are `Timestamp` rendered as `long` (range min 0), required; TriggerHistoryRecord$triggeredTimestamp is the same `long` Timestamp. The model attaches no timestampFormat and the docs say only "in Unix epoch format", so the unit is the one CloudWatch Logs uses for every `long` epoch member on this surface — MILLISECONDS (InputLogEvent$timestamp, GetLogEvents/FilterLogEvents startTime and endTime, LogGroup$creationTime). Recorded behaviour: the recording asks for startTime = <create instant in ms> - 60000 and endTime = <now in ms> + 60000 and AWS answers triggerHistory containing the run of the scheduled query with executionStatus `Complete` and a queryId GetQueryResults then answers Complete rows from. So AWS reads that window as milliseconds and reports triggeredTimestamp in the same unit.

Emulator: the handler filters the recorded executions with `execution.triggered_secs() >= input.start_time && execution.triggered_secs() <= input.end_time` and renders `triggered_timestamp: Some(self.triggered_secs())`, i.e. it divides the stored millisecond instant by 1000 before comparing and before reporting. A window expressed in milliseconds is ~1000x larger than any triggered_secs value, so the filter selects nothing: GetScheduledQueryHistory answers 200 with an empty triggerHistory forever. In the failing check that makes the poll for a record with executionStatus `Complete` never succeed and the check exhausts its whole budget ("Check timed out after 600.0s"); it is the only unbounded wait in the check, and the mismatch is deterministic rather than a timing artifact — the ms-based window can never intersect a seconds-based instant. (The call is a 200, not a refusal, so this is not a reopening of the fixed issue 20261027T002339Z-logs-getscheduledqueryhistory-stub.md; the operation is implemented and answers, it answers the wrong window.)

Why: a client cannot observe that its scheduled query ever ran. Every consumer polls GetScheduledQueryHistory with a millisecond window — that is the unit the rest of CloudWatch Logs uses and the unit the AWS SDKs' own examples pass — and gets an empty history, so it can never obtain the queryId needed to call GetQueryResults, never see Complete/InvalidQuery/Failed statuses, and never see destination processing status. Any reported triggeredTimestamp is also off by a factor of 1000, which renders as 1970 when parsed as milliseconds.

Fix: the GetScheduledQueryHistory handler's window filter and the stored execution's edge conversion — the request's startTime/endTime must be compared against, and triggeredTimestamp reported in, epoch milliseconds (the unit the store already keeps). Citations: services/service-com-amazonaws-cloudwatchlogs-logs-20140328/src/ops/get_scheduled_query_history.rs lines 346-354 (the window filter) and services/service-com-amazonaws-cloudwatchlogs-logs-20140328/src/state/scheduled_query_executions.rs lines 51-58, 187-208 (triggered_secs / reported).

Traces: logs.e2e-scheduled-query-to-results-flow.

## Evidence
Expected (AWS): after creating an ENABLED scheduled query (queryLanguage CWLI, cron(*/15 * * * ? *), startTimeOffset 3600, endTimeOffset 0) over a log group holding 5 INFO and 1 WARN event, GetScheduledQueryHistory(identifier=<arn>, startTime=<createMs-60000>, endTime=<nowMs+60000>) returns triggerHistory with a record whose executionStatus == "Complete" and whose queryId GetQueryResults answers status "Complete" with eventCount == "5".

Actual (emulator): GetScheduledQueryHistory answers 200 with no triggerHistory record in that window — not just no Complete record, no record at all — because the window is interpreted in epoch seconds while the caller sent epoch milliseconds. The check's poll for the first Complete trigger therefore never terminates and the run ends "Error: Check timed out after 600.0s".

Emulator source confirming the unit: get_scheduled_query_history.rs filters on `execution.triggered_secs()` against `input.start_time`/`input.end_time`, and ScheduledQueryExecution::triggered_secs() = triggered_ms.div_euclid(1000), which is also what `reported()` puts in triggeredTimestamp.

## Steps to Reproduce
1. CreateLogGroup, CreateLogStream, PutLogEvents with a few JSON events ({"level":"INFO",...}).
2. Create an IAM role trusted by logs.amazonaws.com with logs:StartQuery/GetQueryResults/StopQuery.
3. CreateScheduledQuery(name=..., queryLanguage="CWLI", queryString="fields level | filter level = 'INFO' | stats count() as eventCount", logGroupIdentifiers=[<log group ARN without :*>], scheduleExpression="cron(*/15 * * * ? *)", startTimeOffset=3600, endTimeOffset=0, executionRoleArn=<role>, state="ENABLED").
4. Call GetScheduledQueryHistory(identifier=<scheduledQueryArn>, startTime=int(time.time()*1000)-60000, endTime=int(time.time()*1000)+60000) repeatedly.
5. Observe triggerHistory is always empty (no record in any status), so no queryId is ever available. Calling the same operation with the window divided by 1000 reveals the runs — showing the unit divergence.
6. Equivalent replay: run behavioral check logs.e2e-scheduled-query-to-results-flow.
