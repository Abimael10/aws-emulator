Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: logs.logs-insights-semantics-stats-aggregates-by-field-match-hand-computation logs.logsquery-stats-group-key-absent-for-events-without-the-field
Issue-Shape-Id: com.amazonaws.cloudwatchlogs#Logs_20140328
Issue-Key: logs.logs-insights-semantics-stats-aggregates-by-field-match-hand-computation|run#21|AssertionError|assert len(keyless) == 1 and keyless[0]["n"] == "2", (
Issue-Key: logs.logsquery-stats-group-key-absent-for-events-without-the-field|_run_query#15|Timeout|time.sleep(1.5)
Issue-Commit: 9279d0e488cae7bddd5fab1b173f5621c68407ec

# logs: a Logs Insights `stats ... by <field>` drops the events that lack the grouping field instead of returning one keyless aggregate row

> _Note: this issue was identified against the code base at commit `9279d0e488cae7bddd5fab1b173f5621c68407ec`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetQueryResults

## Description
Cause 1 of 1: CloudWatch Logs Insights aggregation in this emulator discards every log event that does not carry the `by` field, so a `stats ... by <field>` answer contains only the keyed groups. AWS returns one additional aggregate row for those events, and that row omits the grouping field entirely.

AWS: In CloudWatch Logs Insights, `stats <aggregates> by <field>` partitions EVERY matched event. Events in which the grouping field is absent (never present in the event, or produced by a `parse` stage that did not match the line) are aggregated into a single additional group, and the row GetQueryResults returns for that group contains only the aggregate columns — there is no `ResultField` whose `field` is the grouping field, and no empty string, null or placeholder value standing in for it. Smithy only fixes the envelope, not the semantics: `GetQueryResults` returns `results: QueryResults`, a list of rows, each row a list of `ResultField {field, value}` (`ResultField` = "Contains one field from one log event returned by a CloudWatch Logs Insights query, along with the value of that field"), so a row is a free-form field set and a row that simply omits the grouping field is representable on the wire. The counting rule is recorded behavior only: each matched event is counted in exactly one group, so the aggregate counts across all rows (keyed plus keyless) sum to the number of matched events, and `statistics.recordsMatched` (QueryStatistics.recordsMatched = "The number of log events that matched the query string") covers all matched events including the ones that fall in the keyless group. Status is 200 in both the recording and the emulator; this is a response-content divergence, not a refusal.

Emulator: the keyless group is simply missing from `results`. Only the keyed groups are returned, and the events lacking the grouping field are dropped from the aggregation (they are neither counted in a keyed group nor in a group of their own).
  - With `stats count(*) as n, sum(ms) as s, avg(ms) as a, min(ms) as mn, max(ms) as mx, count_distinct(user) as u, pct(ms, 50) as p50 by svc | sort svc asc` over JSON events carrying `svc` plus two plain-text events carrying no fields, the keyed rows are correct, but the set of rows with no `svc` field is EMPTY: `the 2 events lacking svc must form exactly one keyless bucket counting 2, got []`. AWS returns that one extra row with `n` equal to the number of such events (2 here) and no `svc` field.
  - With `filter @message like /<marker>/ | parse @message "svc=* lat=*" as svc, lat | stats count(*) as n by svc | sort n desc` over five parseable `REQ ... svc=<s> lat=<n>` lines plus one `HEARTBEAT ...` line that the filter selects but the `parse` cannot match, the emulator returns only the two keyed rows; the third row AWS returns — the aggregate-only row for the unparsed line, `{"n": "1"}` — never appears, so the check's poll for the full row set never completes and the check is killed at its 60 s budget (`Check timed out after 60.0s`). The timeout is a consequence of the missing row, not of runner slowness: the emulator answers `Complete` with a row set that is permanently one row short.

Why: a client reading an Insights aggregation gets a silently incomplete answer. The per-group numbers no longer sum to the matched-event count, so dashboards, cost/latency rollups and "count by status/service" panels under-report exactly the traffic whose field is missing or unparseable — the population an operator most needs to see. Code that cross-checks the group totals against `statistics.recordsMatched`, or that polls until the expected number of groups appears, never agrees with the emulator and either reports bad numbers or hangs.

Fix: the Logs Insights query engine's aggregation stage in the CloudWatch Logs service crate — the `stats ... by` grouping step must route events whose grouping field is absent into one distinct group rather than filtering them out, and the result serializer must emit that group's row with the aggregate columns only, emitting no `ResultField` for the grouping field (not an empty-string or null value). The matched-event accounting (`statistics.recordsMatched`) must include those events.

Note: the open issue 20261116T174514Z-logs-a-field-defining-insights-stage-seeds-the.md concerns the COLUMN SET of Insights rows (a field-defining stage seeding rows with the default projection) and does not cover this: here whole rows are absent, and the required fix is the opposite of adding columns — the keyless row must carry FEWER fields than its siblings.

Traces: logs.logs-insights-semantics-stats-aggregates-by-field-match-hand-computation, logs.logsquery-stats-group-key-absent-for-events-without-the-field.

## Evidence
Check logs.logs-insights-semantics-stats-aggregates-by-field-match-hand-computation (mismatch):
  Seeded in one log group: 6 JSON events `{"svc": ..., "ms": ..., "user": ...}` (svc in api/db/web) and 2 plain-text events with no fields.
  Query: `stats count(*) as n, sum(ms) as s, avg(ms) as a, min(ms) as mn, max(ms) as mx, count_distinct(user) as u, pct(ms, 50) as p50 by svc | sort svc asc`
  Expected (AWS): 4 rows — api/db/web keyed rows (which the emulator got right) PLUS exactly one row with no `svc` field whose `n` equals the number of events lacking `svc` (2 in this seeding).
  Actual (emulator): rows with no `svc` field = [] ->
    AssertionError: the 2 events lacking svc must form exactly one keyless bucket counting 2, got []

Check logs.logsquery-stats-group-key-absent-for-events-without-the-field (mismatch):
  Seeded: 5 events `REQ <marker> svc=<alpha|beta> lat=<n>` (alpha x3, beta x2) and 1 event `HEARTBEAT <marker> no fields here`.
  Query: `filter @message like /<marker>/ | parse @message "svc=* lat=*" as svc, lat | stats count(*) as n by svc | sort n desc`
  Expected (AWS): 3 rows, in `sort n desc` order — {svc: alpha, n: 3}, {svc: beta, n: 2}, then {n: 1} with NO `svc` field at all; the group counts sum to the 6 matched events and statistics.recordsMatched == 6.
  Actual (emulator): the query reaches status Complete but only ever returns the 2 keyed rows; the aggregate-only row for the unparsed HEARTBEAT line is never present, so the check's retry loop for the full row set is still polling when the harness kills it ->
    Error: Check timed out after 60.0s
  HTTP status in both checks is 200 — the divergence is in the body (a missing row), not in a refusal.

## Steps to Reproduce
1. CreateLogGroup + CreateLogStream, then PutLogEvents with a mix of events that carry a field and events that do not:
   - e.g. several JSON messages `{"svc":"api","ms":120,"user":"alice"}` ... and two messages with no structure at all (`no fields here`).
2. StartQuery over that log group with `stats count(*) as n by svc` (optionally with more aggregates and `| sort svc asc`), covering a time range that includes all events.
3. Poll GetQueryResults until status == "Complete" and read `results`.
4. Observe: the emulator returns only the rows for the svc values present (api, db, web); there is no row consisting of the aggregate columns alone. AWS returns one extra row, `{"n": "<count of events without svc>"}`, carrying no `svc` ResultField, and the group counts sum to `statistics.recordsMatched`.
5. Same with a parse-derived key: PutLogEvents with five `REQ <marker> svc=alpha lat=100`-style lines and one `HEARTBEAT <marker> no fields here`, then StartQuery `filter @message like /<marker>/ | parse @message "svc=* lat=*" as svc, lat | stats count(*) as n by svc | sort n desc`. The emulator returns 2 rows; AWS returns 3, the last being `{"n": "1"}` with no `svc` field, and recordsMatched == 6.
