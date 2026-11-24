Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__logs__sns__sqs.deep-cloudwatch-2-cwlogalarm-m-of-n-breach-count-drives-alarm cloudwatch__logs__sns__sqs.deep-cloudwatch-2-cwlogalarm-zero-match-count-is-missing-not-zero
Issue-Key: cloudwatch__logs__sns__sqs.deep-cloudwatch-2-cwlogalarm-m-of-n-breach-count-drives-alarm|run#68|AssertionError|assert alarm_note["QueryExecutionId"] in by_id, (
Issue-Key: cloudwatch__logs__sns__sqs.deep-cloudwatch-2-cwlogalarm-zero-match-count-is-missing-not-zero|_poll_until#20|AssertionError|raise AssertionError(f"{description} not met within {timeout_s}s. Last result: {last!r}")
Issue-Commit: f4f47df5aa8b9a19bc159acc32ad01036b03b7fe

# cloudwatch__logs__sns__sqs: a log alarm's AWS-managed scheduled query records no executions — GetScheduledQueryHistory answers an empty triggerHistory even after the alarm has evaluated and notified

> _Note: this issue was identified against the code base at commit `f4f47df5aa8b9a19bc159acc32ad01036b03b7fe`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetScheduledQueryHistory

## Description
Cause 1 of 1: the CloudWatch Logs scheduled query that `cloudwatch PutLogAlarm` provisions for a log alarm never accumulates trigger history. `logs GetScheduledQueryHistory` answers HTTP 200 with `triggerHistory: []` for that query's ARN, for every window asked, even though the alarm the query backs has demonstrably been evaluated by the emulator in the same run (the breach alarm reached ALARM with COMPLETE results of value 7 in its StateReasonData, the control alarm reached OK on two COMPLETE results of value 2, and both transitions were delivered SNS -> SQS naming a QueryExecutionId). Failing check ids: cloudwatch__logs__sns__sqs.deep-cloudwatch-2-cwlogalarm-m-of-n-breach-count-drives-alarm, cloudwatch__logs__sns__sqs.deep-cloudwatch-2-cwlogalarm-zero-match-count-is-missing-not-zero.

AWS:      Smithy (com.amazonaws.cloudwatchlogs#Logs_20140328): `GetScheduledQueryHistory` takes required `identifier` (@length 1..300 — a scheduled-query name or ARN), required `startTime` and `endTime` (long epoch, @range min 0), optional `executionStatuses`, `maxResults` (@range 1..1000) and `nextToken`; it is @paginated with `items: triggerHistory`. Its output carries `name`, `scheduledQueryArn` and `triggerHistory`, a list of `TriggerHistoryRecord` = { `queryId` (@length 1..256, documented as "The unique identifier for this query execution"), `executionStatus` (enum Running | InvalidQuery | Complete | Failed | Timeout, documented as "The execution status of the scheduled query run"), `triggeredTimestamp`, `errorMessage`, `destinations` }. `GetScheduledQuery` on the same identifier models `lastTriggeredTime` and `lastExecutionStatus`, i.e. the service tracks per-execution outcomes for a scheduled query. Recorded behavior for the join: every scheduled evaluation of an alarm-owned (scheduleType AWS_MANAGED) query appends one record inside the requested [startTime, endTime] window, reaching `executionStatus` "Complete" when the query finished, and the `queryId` of the execution that decided a transition is the same id CloudWatch reports as `QueryExecutionId` in that transition's alarm notification. The ids, ARNs and timestamps are per-run values; the requirement is that one such record per evaluation exists and that the notified execution id is among them.

Emulator: `triggerHistory` is always empty. Check m-of-n fails its final assertion with `AssertionError: QueryExecutionId '00000000-0000-4000-8000-000000000001' absent from the scheduled query's trigger history []` — the alarm evaluator ran and published, yet the query it ran has no record of any execution, and the id the notification names exists in no record. Check zero-match uses the same history as its barrier (poll until >= 3 records with executionStatus "Complete" before reading the alarm) and fails with `AssertionError: three Complete executions of the quiet alarm's scheduled query arn:aws:logs:<region>:<account>:scheduled-query:<name> not met within 240s. Last result: []` — 240 s of polling, never a single record. `GetScheduledQuery` on the same ARN does answer (the m-of-n check passed its queryString / logGroupIdentifiers / scheduleExpression / executionRoleArn / scheduleType=AWS_MANAGED assertions), so the query resource exists and only its execution history is missing.

Why:      a client cannot see whether a log alarm's query is actually running, when it last ran, or whether an execution failed — the only API that exposes per-execution status reports "never ran" for a query that is running. Two concrete consequences: (1) the execution a notification blames cannot be correlated back to the query that produced it, so alert triage ("which run crossed the threshold, and did it complete or time out?") is impossible; (2) any client that uses the history as a liveness barrier before reading alarm state — the standard way to distinguish "no transition" from "has not evaluated yet" — blocks forever, which is exactly what makes the zero-match semantics (count(*) over zero matching lines must stay INSUFFICIENT_DATA rather than becoming 0 -> OK) unobservable here.

Fix:      the CloudWatch Logs scheduled-query state and the `GetScheduledQueryHistory` handler, plus the alarm evaluator that must register each evaluation it performs as a trigger-history record on the AWS-managed query it evaluates (queryId = the execution id the alarm action/notification reports as QueryExecutionId, executionStatus Complete on success), and the window/`executionStatuses` filtering and pagination of that handler. No open issue covers this: a search of the open issues for "scheduled-query", "log alarm" and the cloudwatch__logs heading returns only unrelated causes (PutTransformer parser prose, AWS::Logs::MetricFilter failure reason, sub-minute GetMetricStatistics resolution).

Traces:   cloudwatch__logs__sns__sqs.deep-cloudwatch-2-cwlogalarm-m-of-n-breach-count-drives-alarm (final assertion), cloudwatch__logs__sns__sqs.deep-cloudwatch-2-cwlogalarm-zero-match-count-is-missing-not-zero (barrier poll, 240 s).

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator built from this checkout.

[mismatch] cloudwatch__logs__sns__sqs.deep-cloudwatch-2-cwlogalarm-m-of-n-breach-count-drives-alarm
  check...m_of_n...py line 220: assert alarm_note["QueryExecutionId"] in by_id
  AssertionError: QueryExecutionId '00000000-0000-4000-8000-000000000001' absent from the scheduled query's trigger history []

[mismatch] cloudwatch__logs__sns__sqs.deep-cloudwatch-2-cwlogalarm-zero-match-count-is-missing-not-zero
  check...zero_match...py line 106 -> _poll_until line 182
  AssertionError: three Complete executions of the quiet alarm's scheduled query arn:aws:logs:us-west-2:000000000000:scheduled-query:sim2real-xsvc-la-quiet-<id> not met within 240s. Last result: []

Expected (AWS): logs GetScheduledQueryHistory(identifier=<the alarm's QueryARN>, startTime=now-900, endTime=now+60) returns HTTP 200 with triggerHistory holding one TriggerHistoryRecord per scheduled evaluation performed in that window, each with a queryId, a triggeredTimestamp inside the window and executionStatus "Complete" once the run finished; the record whose queryId equals the QueryExecutionId carried by the alarm's SNS notification is present and Complete. (Window bounds, ARNs, ids and timestamps are per-run values filling that shape.)
Actual: HTTP 200 with triggerHistory == [] in every call — immediately after a transition that was evaluated and notified, and still [] after 240 s of 10 s-interval polling. GetScheduledQuery on the same ARN answers normally (queryString, logGroupIdentifiers, scheduleExpression, executionRoleArn, scheduleType=AWS_MANAGED all round-trip), so only the execution history is absent.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. iam CreateRole + PutRolePolicy for a role trusting logs.amazonaws.com with logs:StartQuery/StopQuery/GetQueryResults/DescribeLogGroups.
3. logs CreateLogGroup + CreateLogStream, then PutLogEvents with 7 lines containing "BREACH" and 3 that do not.
4. cloudwatch PutLogAlarm(AlarmName=<a>, ScheduledQueryConfiguration={QueryString="fields @timestamp, @message | filter @message like /BREACH/", LogGroupIdentifiers=[<group>], ScheduledQueryRoleARN=<role>, ScheduleConfiguration={ScheduleExpression="rate(1 minute)", StartTimeOffset=300, EndTimeOffset=0}, AggregationExpression="count(*)"}, QueryResultsToEvaluate=2, QueryResultsToAlarm=1, Threshold=3.0, ComparisonOperator=GreaterThanOrEqualToThreshold, AlarmActions=[<topic>], OKActions=[<topic>]).
5. cloudwatch DescribeAlarms(AlarmNames=[<a>], AlarmTypes=["LogAlarm"]) and take ScheduledQueryConfiguration.QueryARN.
6. Wait until the alarm leaves INSUFFICIENT_DATA (it reaches ALARM, and its StateReasonData lists COMPLETE results with value 7).
7. logs GetScheduledQueryHistory(identifier=<that QueryARN>, startTime=<now-900>, endTime=<now+60>).
Observed: triggerHistory is [] — no record for any of the evaluations that just happened, and none whose queryId matches the QueryExecutionId in the alarm's SNS notification.
Expected: one TriggerHistoryRecord per evaluation, executionStatus "Complete", including the execution the notification names.
8. Or replay the two check ids in trace_ids.
