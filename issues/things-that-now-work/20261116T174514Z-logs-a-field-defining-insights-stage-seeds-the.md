Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: logs.logsquery-parse-glob-extracts-fields-from-unstructured-lines
Issue-Shape-Id: com.amazonaws.cloudwatchlogs#Logs_20140328
Issue-Key: logs.logsquery-parse-glob-extracts-fields-from-unstructured-lines|run#21|AssertionError|assert set(heartbeat) == {"@ptr"}, (
Issue-Commit: 69b5bb01d5a9bd6e71aad0c8b3d3a2a49866ff19

# logs: a field-defining Insights stage seeds the result's column set with the DEFAULT projection, so rows carry @message/@timestamp AWS never returns and a record the `parse` pattern does not match is not field-free

> _Note: this issue was identified against the code base at commit `69b5bb01d5a9bd6e71aad0c8b3d3a2a49866ff19`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetQueryResults

## Description
One net-new root cause in the CloudWatch Logs Insights query surface, reached by 1 behavioral check whose replay key is carried by no open issue.

Cause 1 of 1 — a stage that DEFINES fields (`parse`, `eval`, `lookup`) adds its columns to the engine's default projection instead of REPLACING it, so every row of such a pipeline also carries the default columns, and a record the parse pattern does not match comes back carrying `@message`/`@timestamp` instead of nothing but `@ptr`.

AWS: Smithy (logs, com.amazonaws.cloudwatchlogs#Logs_20140328) GetQueryResults documents the rule directly — "Only the fields requested in the query are returned, along with a @ptr field, which is the identifier for the log record" — and types `results` as a list of rows of ResultField {field,value}, so a row's columns are the pipeline's OWN output columns. Recorded behavior for a pipeline whose only projection is a `parse`, e.g. `filter @message like /<marker>/ | parse @message "svc=* lat=* status=*" as svc, lat, st | sort @timestamp asc`: the result's columns are exactly the parsed names declared by the `as` clause plus the engine-generated `@ptr`; `@message` and `@timestamp` are NOT columns, because the pipeline never requests them. A record that the filter selects but the parse pattern cannot match is still returned as a row, and that row carries NONE of the parsed keys — not empty strings, not nulls, the keys are simply absent — so all that is left of it is the pointer the engine adds itself: the row is `{@ptr}` alone. The parsed names come from the request's `as` clause; the values come from the ingested events; `@ptr` is service-generated.

Emulator: the query reaches `Complete`, returns the right number of rows, and the five matching records now do carry `svc`/`lat`/`st` with the seeded values — but the result's column set is the DEFAULT projection plus the parsed names, so the row of the record the pattern did not match renders as the fields `['@message', '@ptr', '@timestamp']` rather than `['@ptr']`, and the check's "exactly one row may be field-free" invariant finds none. Root cause in the checkout: `declare_fields` in services/service-com-amazonaws-cloudwatchlogs-logs-20140328/src/state/insights/pipeline.rs (lines 398-419) does `rows.columns.get_or_insert_with(|| DEFAULT_COLUMNS...)` and then PUSHES the declared names onto that seed, so a pipeline that has not yet run a `fields`/`display`/aggregation stage ends up with DEFAULT_COLUMNS ∪ {parsed names}; `rendered` (same file, lines 247-256) then emits, per record, every one of those columns the record happens to carry — and a scanned record always carries `@message`/`@timestamp` because `record_of` (src/state/insights.rs lines 603-635) sets them on every record. The same seeding applies to `eval` (pipeline.rs line 598) and `lookup` (line 1028), so every field-defining stage inherits it.

Why: a client reading the rows of a `parse` pipeline gets columns AWS does not return, so code that enumerates a row's fields, renders them as a table, or diffs row shapes sees three extra columns on every row. Worse, the one signal AWS gives for "this line did not match the parse pattern" — a row with no fields but the pointer — is destroyed: every record looks like a match that happens to carry the raw message, so a client cannot tell parsed records from unparsed ones and cannot count them, with no error to diagnose because the query is reported `Complete`.

Fix: the Insights pipeline's row-projection / column-set stage in the logs service crate — a stage that DEFINES fields must establish the result's column set from the fields it created (plus `@ptr` for log-record rows), not seed it from the engine's implicit default projection; the default projection must survive only for a pipeline that projects nothing at all.

Traces: logs.logsquery-parse-glob-extracts-fields-from-unstructured-lines (its earlier legs — the parsed values on the five matching rows, their `@ptr`, `recordsMatched == 6` and the `fields @message` projection leg — all pass; only the unmatched-row field set diverges).

Related issues that do NOT carry this replay key and do not cover this cause: 20261115T160223Z-logs-2-net-new-causes-insights-parse-output.md is FIXED and its fix did land — parsed columns now reach the rows — but it left the default projection in place beside them, which is what this issue reports (the call is not refused, so this is not a reopened refusal). 20261108T145640Z-logs-9-net-new-causes-cloudwatch-logs-insights.md describes the neighbouring projection defect as its Cause 1 but carries this check only under the row-0 `(svc, lat, st)` assertion key, which now passes, and its own remaining keys are the display/sort legs. 20261108T120220Z-cloudwatch-lambda-logs-logs-insights-parse-with-named.md is a different cause (a bogus empty-key `stats` group for non-matching records).

## Evidence
Expected (AWS): for `filter @message like /<marker>/ | parse @message "svc=* lat=* status=*" as svc, lat, st | sort @timestamp asc` over five lines of the form `REQ <marker> svc=<s> lat=<n> status=<c>` plus one `HEARTBEAT <marker> no fields here` line, GetQueryResults returns 6 rows with status Complete and statistics.recordsMatched == 6.0; the five request-shaped rows carry svc/lat/st plus @ptr, and the HEARTBEAT row carries the single field @ptr — `set(row) == {"@ptr"}` — so exactly one row in the result is field-free.

Actual (emulator): status Complete, 6 rows, recordsMatched 6.0, and the five request-shaped rows do carry the seeded svc/lat/st values, but the HEARTBEAT row's fields are `['@message', '@ptr', '@timestamp']`. The check fails at check_logs_logsquery_parse_glob_extracts_fields_from_unstructured_lines.py line 114 with: AssertionError: an unmatched line must carry no parsed fields at all -- not empty strings, not nulls; got fields ['@message', '@ptr', '@timestamp']. The follow-on invariant `sum(1 for r in rows if set(r) == {"@ptr"}) == 1` is therefore also unsatisfiable, since no row is field-free.

Mechanism in the checkout: services/service-com-amazonaws-cloudwatchlogs-logs-20140328/src/state/insights/pipeline.rs:407-413 (`declare_fields` seeds `rows.columns` from `DEFAULT_COLUMNS` before appending the parsed names), consumed by `rendered` at the same file lines 247-256, over records that always carry @message/@timestamp from `record_of` in src/state/insights.rs:603-635. Called from `parse_stage` (pipeline.rs:555), `eval` (pipeline.rs:598) and the `lookup` join (pipeline.rs:1028).

## Steps to Reproduce
1. CreateLogGroup(logGroupName=<unique>) and CreateLogStream(logGroupName=<that>, logStreamName=<unique>).
2. PutLogEvents with six events: five of the form `REQ <marker> svc=<s> lat=<n> status=<c>` (e.g. svc=alpha lat=100 status=200, ... svc=beta lat=25 status=404) at increasing timestamps, plus one final event `HEARTBEAT <marker> no fields here` — a line the same filter selects but the parse pattern cannot match.
3. StartQuery(logGroupNames=[<group>], startTime=<now-600>, endTime=<now+120>, queryString='filter @message like /<marker>/ | parse @message "svc=* lat=* status=*" as svc, lat, st | sort @timestamp asc').
4. Poll GetQueryResults(queryId=...) to status Complete (retry the whole start+poll cycle until 6 rows are visible; ingestion is eventually consistent) and map each row to {field: value}.
5. Inspect the LAST row (the HEARTBEAT line, newest under `sort @timestamp asc`). Expected its field set to be exactly {'@ptr'}; observed {'@message', '@ptr', '@timestamp'}. Equivalently, count the rows whose field set is {'@ptr'}: expected exactly 1, observed 0.
6. Same shape with `| eval x = 1` or a `| lookup <table> ...` stage in place of the `parse`: the rows carry the default columns beside the defined one instead of only the pipeline's own columns.
