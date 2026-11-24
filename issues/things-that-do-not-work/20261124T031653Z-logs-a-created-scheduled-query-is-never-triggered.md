Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: logs.e2e-scheduled-query-history-contains-real-results logs.get-scheduled-query-history-orders-executions-by-actual-run-time logs.scheduled-query-history-after-create logs.scheduled-query-history-recorded logs.scheduled-query-history-returned-in-chronological-order
Issue-Shape-Id: com.amazonaws.cloudwatchlogs#Logs_20140328
Issue-Key: logs.e2e-scheduled-query-history-contains-real-results|run#21|AssertionError|assert record is not None, "scheduled query never recorded a Complete execution"
Issue-Key: logs.get-scheduled-query-history-orders-executions-by-actual-run-time|_poll_for_trigger_history#15|AssertionError|raise AssertionError(f"scheduled query {identifier} never produced trigger history within {timeout_s}s")
Issue-Key: logs.scheduled-query-history-after-create|run#13|AssertionError|raise AssertionError(
Issue-Key: logs.scheduled-query-history-recorded|run#28|AssertionError|raise AssertionError(
Issue-Key: logs.scheduled-query-history-returned-in-chronological-order|run#11|AssertionError|raise AssertionError(
Issue-Commit: 02e30cec75724d494cfd983590fb8cff440247b8

# logs: a created scheduled query is never triggered, so GetScheduledQueryHistory never returns a triggerHistory record and no execution ever reaches a terminal executionStatus (1 cause)

> _Note: this issue was identified against the code base at commit `02e30cec75724d494cfd983590fb8cff440247b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetScheduledQueryHistory

## Description
One net-new root cause, grounded in five failing behavioral checks the suite lists under `### logs` and marks neither ALREADY FILED nor OWNED. No open issue carries these checks' replay keys; a keyword search for 'scheduled-query' over the open issues returns nothing, so this behaviour is unowned.

=== Cause 1 of 1: CreateScheduledQuery accepts and returns a scheduledQueryArn, but the query is never executed on its schedule, so GetScheduledQueryHistory.triggerHistory stays permanently empty ===

AWS: Smithy: `CreateScheduledQuery` takes a required `scheduleExpression` (ScheduleExpression, @length 0..256), a required `queryLanguage`, a required `queryString`, a required `executionRoleArn`, optional `logGroupIdentifiers` (@length 1..50), optional `scheduleStartTime`/`scheduleEndTime`, optional `startTimeOffset`/`endTimeOffset`, and optional `state` (ScheduledQueryState); it returns `scheduledQueryArn` and `state`. `GetScheduledQueryHistory` takes a required `identifier` (ScheduledQueryIdentifier, @length 1..300 — name or ARN) plus required `startTime`/`endTime` and returns `name`, `scheduledQueryArn` and `triggerHistory` (TriggerHistoryRecordList, paginated on `nextToken`/`maxResults` 1..1000 with items `triggerHistory`). Each `TriggerHistoryRecord` carries `queryId` (@length 1..256), `executionStatus` of the `ExecutionStatus` enum [Running, InvalidQuery, Complete, Failed, Timeout], `triggeredTimestamp` ("The timestamp when the scheduled query execution was triggered", a long, @range min 0), optional `errorMessage`, and `destinations`. In AWS, an ENABLED scheduled query whose scheduleExpression fires (the checks use a 60-second schedule) is actually run by CloudWatch Logs: for each firing a TriggerHistoryRecord appears in GetScheduledQueryHistory whose `executionStatus` advances from `Running` to a terminal value (`Complete` for a valid query over a populated log group), whose `triggeredTimestamp` is the real wall-clock time of that firing, and whose `queryId` is a usable Insights query id that `GetQueryResults` resolves to the rows the query matched. Repeated firings accumulate as multiple records ordered by `triggeredTimestamp`.

Emulator: `CreateScheduledQuery` is accepted and mints an ARN (e.g. of the shape `arn:aws:logs:<region>:<account>:scheduled-query:<name>`), but nothing ever executes it. `GetScheduledQueryHistory` answers with an empty `triggerHistory` for the whole poll window of every check: no record with `executionStatus=Running` ever appears, no record ever reaches a terminal `ExecutionStatus`, no `triggeredTimestamp` is ever recorded, and no `queryId` is ever produced for `GetQueryResults`. The five checks fail with 'scheduled query never recorded a Complete execution', 'never produced trigger history within <budget>', 'did not reach a terminal executionStatus in GetScheduledQueryHistory within the poll window', 'no scheduled query execution landed in GetScheduledQueryHistory within the bounded test window', and 'only accumulated 0 execution(s) within the poll window'. The emulator stores the scheduled-query definition but has no trigger/execution path behind it.

Why: a client that uses a scheduled query to run a saved Insights query on a cadence gets a resource that reports itself created and ENABLED but never produces a single result. Every downstream contract collapses: there is no `queryId` to pass to `GetQueryResults`, so results are unreachable; there is no `executionStatus` to poll, so success and failure are indistinguishable from 'not yet run'; there is no `triggeredTimestamp`, so ordering of executions by run time cannot be observed at all. Dashboards, alarms and ETL built on scheduled Insights queries silently produce nothing rather than erroring, which is the worst failure mode for a scheduler.

Fix: the scheduled-query execution path behind the CloudWatch Logs scheduled-query state — a scheduler that evaluates each ENABLED scheduled query's `scheduleExpression` (honouring `scheduleStartTime`/`scheduleEndTime` and `timezone`), runs its `queryString` against its `logGroupIdentifiers` over the window derived from `startTimeOffset`/`endTimeOffset`, registers the run as an Insights query so its `queryId` resolves through GetQueryResults, and appends a TriggerHistoryRecord (queryId, executionStatus, triggeredTimestamp) that the `GetScheduledQueryHistory` handler/serializer returns, filtered by `startTime`/`endTime`/`executionStatuses` and ordered by `triggeredTimestamp`.

Traces: logs.e2e-scheduled-query-history-contains-real-results, logs.get-scheduled-query-history-orders-executions-by-actual-run-time, logs.scheduled-query-history-after-create, logs.scheduled-query-history-recorded, logs.scheduled-query-history-returned-in-chronological-order.

## Evidence
Suite: 919/1114 passed (82.5%); service `logs` 448/481 (93%), 33 failed. Five of those failures were marked neither ALREADY FILED nor OWNED, and all five fail on the same missing behaviour.

[mismatch] logs.e2e-scheduled-query-history-contains-real-results
  Expected (AWS): GetScheduledQueryHistory records an execution with executionStatus=Complete, and GetQueryResults for that record's queryId returns the matched events.
  Actual: `AssertionError: scheduled query never recorded a Complete execution` — triggerHistory stayed empty.

[mismatch] logs.get-scheduled-query-history-orders-executions-by-actual-run-time
  Expected (AWS): a real `triggeredTimestamp` and a `Complete` executionStatus, with GetQueryResults resolving the recorded queryId.
  Actual: `AssertionError: scheduled query arn:aws:logs:<region>:<account>:scheduled-query:<name> never produced trigger history within <budget>s`. The ARN and the budget in that message are this run's values; the requirement is that any trigger history is produced at all.

[mismatch] logs.scheduled-query-history-after-create
  Expected (AWS): a frequently-firing scheduled query reaches a terminal ExecutionStatus in GetScheduledQueryHistory.
  Actual: `AssertionError: scheduled query did not reach a terminal executionStatus in GetScheduledQueryHistory within the poll window`.

[mismatch] logs.scheduled-query-history-recorded
  Expected (AWS): executions of a scheduled query produce entries retrievable via GetScheduledQueryHistory.
  Actual: `AssertionError: no scheduled query execution landed in GetScheduledQueryHistory within the bounded test window; scheduling latency exceeded the budget`.

[mismatch] logs.scheduled-query-history-returned-in-chronological-order
  Expected (AWS): GetScheduledQueryHistory returns execution records ordered by monotonic triggeredTimestamp.
  Actual: `AssertionError: scheduled query only accumulated 0 execution(s) within the poll window; not enough history to assert ordering` — zero records, so ordering is unobservable.

The poll budgets (60s/270s schedules) and the generated query names/ARNs are per-run values, not requirements; the requirement is that an ENABLED scheduled query fires and records a TriggerHistoryRecord with a queryId, an ExecutionStatus and a triggeredTimestamp.

## Steps to Reproduce
1. CreateLogGroup and PutLogEvents a handful of events into it so a query has rows to match.
2. Create an IAM execution role the scheduled query can assume (CreateRole + a policy granting logs:StartQuery/GetQueryResults on the log group).
3. CreateScheduledQuery with name=<unique>, queryLanguage=CWLI, queryString='fields @timestamp, @message | limit 5', logGroupIdentifiers=[<the log group ARN>], scheduleExpression set to fire every minute, executionRoleArn=<the role>, state=ENABLED. Note the returned scheduledQueryArn and state.
4. Poll GetScheduledQueryHistory(identifier=<that ARN or name>, startTime=<creation time - 60>, endTime=<now + 300>) every 10s for several minutes.
5. Observe: `triggerHistory` is always `[]` — no record with executionStatus=Running ever appears, none reaches Complete/Failed/Timeout/InvalidQuery, and no triggeredTimestamp or queryId is ever recorded. In AWS at least one record appears within a couple of schedule periods, and repeated firings accumulate in triggeredTimestamp order.
6. Because no queryId is ever produced, GetQueryResults for a recorded execution cannot be attempted at all.
