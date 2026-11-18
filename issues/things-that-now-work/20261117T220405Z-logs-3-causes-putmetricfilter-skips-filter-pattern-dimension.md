Issue-Source: trace-sweep-divergence
Issue-Trace: logs-metric-filter-pattern-dimension-support-and-count-progression
Issue-Step: task#8
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: logs-metric-filter-pattern-dimension-support-and-count-progression logs-insights-start-query-and-describe-workflow logs-log-group-query-discovery logs-osdocstat-traj-insights-query-audit-group logs-osdocstat-traj-log-group-arn-forms
Issue-Shape-Id: com.amazonaws.cloudwatchlogs#Logs_20140328
Issue-Key: logs|PutMetricFilter|4xx->2xx|$
Issue-Key: logs|PutMetricFilter|4xx->2xx|status
Issue-Key: logs|DescribeLogGroups|2xx->2xx|$.logGroups[*].metricFilterCount
Issue-Key: logs|StartQuery|4xx->4xx|$.message
Issue-Commit: de9b84ff7ff323aecc47ca6e86b97248953b7849

# logs: 3 causes — PutMetricFilter skips filter-pattern dimension validation, metricFilterCount counts rejected filters, StartQuery error message carries SDK decoration

> _Note: this issue was identified against the code base at commit `de9b84ff7ff323aecc47ca6e86b97248953b7849`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutMetricFilter

## Description
Three net-new CloudWatch Logs wire divergences found replaying the simparity-wire-aws-v1 corpus. All three are in the `logs` service and none is owned by an open issue or blocked by a stub. A fixer must close ALL THREE before submitting.

Cause 1 of 3 — PutMetricFilter accepts a metric transformation with `dimensions` for a filter pattern that does not support dimensions.
AWS: a metric filter may only carry `metricTransformations[].dimensions` when the filter pattern extracts named fields (JSON / space-delimited patterns with named field tokens); for a pattern that extracts no named fields the request is rejected before the filter is created. The recorded response is HTTP 400 with `__type` = `InvalidParameterException` and `message` = `The specified filter pattern does not support dimensions`. That message is entirely fixed text for this error class — it has no variable parts taken from the request or from state. Smithy: PutMetricFilter errors include InvalidParameterException (errorKind client, httpError 400); MetricTransformation member `dimensions` is documented as "The fields to use as dimensions for the metric. One metric filter can include as many as three dimensions." The rule that an unsupported pattern rejects dimensions is recorded behavior only (the model does not encode the pattern/dimension compatibility rule).
Emulator: answers HTTP 200 with an empty body (`{}`) — the invalid filter is accepted and stored.
Why: a client that relies on the service to validate the pattern/dimension combination gets a success for a filter that AWS would never create; the filter then appears in Describe/List results and in the group's filter count, so the client's view of its metric filters silently diverges from AWS and the dimension misuse (a documented high-charge hazard) is never surfaced.
Fix: PutMetricFilter's request validation in the logs service crate — it must reject `metricTransformations[].dimensions` when the supplied `filterPattern` extracts no named fields, and raise InvalidParameterException with the message above instead of mutating state.
Traces: logs-metric-filter-pattern-dimension-support-and-count-progression#task#8 (expected 400 InvalidParameterException, got 200 with an empty body).

Cause 2 of 3 — DescribeLogGroups reports a metricFilterCount that includes filters the service should have rejected.
AWS: Smithy LogGroup member `metricFilterCount` is "The number of metric filters." — the number of metric filters that actually exist on the log group. After the recorded sequence (one valid PutMetricFilter plus one PutMetricFilter that AWS refuses, see Cause 1) the group has exactly 1 metric filter, and DescribeLogGroups returns `logGroups[0].metricFilterCount` = 1. Status is 200 in both the recording and the emulator; only the counter diverges.
Emulator: returns `logGroups[0].metricFilterCount` = 2 — it counts the filter the refused request created.
Why: a client that reads metricFilterCount to decide whether it is at the per-group filter limit, or to reconcile its desired filter set, sees a filter that does not exist on AWS; capacity checks and reconciliation loops take the wrong branch.
Fix: the log-group state that backs metricFilterCount in the logs service crate must only count successfully created filters — i.e. the counter must be incremented after PutMetricFilter validation succeeds, not before it. Hypothesis: this is the downstream effect of Cause 1, but it is listed separately because the counter is maintained independently of the handler's validation and must be verified on its own trace after the fix.
Traces: logs-metric-filter-pattern-dimension-support-and-count-progression#task#9 (expected metricFilterCount 1, got 2).

Cause 3 of 3 — StartQuery's MalformedQueryException message embeds AWS-SDK client-side decoration in the wire body.
AWS: StartQuery rejects an unparsable `queryString` with MalformedQueryException (Smithy: StartQuery errors include MalformedQueryException, errorKind client, httpError 400). The wire message is the query-parser diagnostic alone: fixed text of the form `unexpected symbol found <symbol> at line <n> and position <m>`, where the symbol and the line/position numbers are variable parts derived from the submitted queryString. Nothing else belongs in `message`: the `(Service: ...; Status Code: ...; Error Code: ...; Request ID: ...; Proxy: ...)` suffix is assembled client-side by the AWS Java SDK from the response envelope and the request id header — AWS never ships it inside the response body. The fixture asserts the shape of `message` for this step (expected value is recorded as the shape placeholder `<shaped-not-asserted>`), and the emulator's value does not conform to it.
Emulator: HTTP 400 is correct, but `message` is `unexpected symbol found abc at line 1 and position 24 (Service: AWSLogs; Status Code: 400; Error Code: MalformedQueryException; Request ID: <uuid>; Proxy: null)` — the parser diagnostic followed by a full SDK-style decoration block. The defect is that fixed decoration text being present at all, not the particular request id inside it.
Why: a client that displays or matches on the error message gets the decoration twice (once from the body, once appended by its own SDK), and a client that parses the message to recover the offending symbol/position has to strip text AWS does not send; log scrapers keyed on the AWS message form stop matching.
Fix: StartQuery's handler / the logs error serializer must populate `message` with only the service-side parser diagnostic and let the status code, error code (`__type`) and request id travel in their own envelope positions.
Traces: logs-insights-start-query-and-describe-workflow#task#4; the same cluster also covers logs-log-group-query-discovery#task#6, logs-osdocstat-traj-insights-query-audit-group#task#7 and logs-osdocstat-traj-log-group-arn-forms#task#7.

## Evidence
Replay of simparity-wire-aws-v1 (189 fixtures, 1266 steps, 1146 passed, score 90.5%), 46 divergence clusters; 43 are already owned by open issues, these 3 are net-new.

Cause 1 — fixture logs-metric-filter-pattern-dimension-support-and-count-progression, step task#8, operation PutMetricFilter:
  expected: status 400, body {"__type": "InvalidParameterException", "message": "The specified filter pattern does not support dimensions"}
  actual:   status 200, body {}
  detail:   "status: expected 400, got 200 / $: empty_body"

Cause 2 — fixture logs-metric-filter-pattern-dimension-support-and-count-progression, step task#9, operation DescribeLogGroups:
  expected: $.logGroups[0].metricFilterCount == 1
  actual:   $.logGroups[0].metricFilterCount == 2
  detail:   "$.logGroups[0].metricFilterCount: value_mismatch (expected 1, got 2)"

Cause 3 — fixture logs-insights-start-query-and-describe-workflow, step task#4, operation StartQuery (status 400 expected and returned):
  expected: $.message conforming to the recorded shape (<shaped-not-asserted>) — the parser diagnostic only
  actual:   $.message == "unexpected symbol found abc at line 1 and position 24 (Service: AWSLogs; Status Code: 400; Error Code: MalformedQueryException; Request ID: cfd5e73e-315c-4819-94fb-f1f6bfcd5bec; Proxy: null)"
  cluster also affects: logs-log-group-query-discovery#task#6, logs-osdocstat-traj-insights-query-audit-group#task#7, logs-osdocstat-traj-log-group-arn-forms#task#7

Model references (services/service-com-amazonaws-cloudwatchlogs-logs-20140328/smithy-model/com-amazonaws-cloudwatchlogs-logs-20140328.json): PutMetricFilter -> InvalidParameterException (client, 400); StartQuery -> MalformedQueryException (client, 400); LogGroup.metricFilterCount "The number of metric filters."; MetricTransformation.dimensions "The fields to use as dimensions for the metric...". Handlers live under services/service-com-amazonaws-cloudwatchlogs-logs-20140328/src/ops/ with group state in src/state.rs (see src/lib.rs:10-13).

Note: no open issue covers these three. The nearest open logs issues (20261111T030649Z-logs-5-net-new-causes-ingest-transformers-are.md, 20261105T111951Z-cloudwatch-lambda-logs-5-net-new-root-causes.md) concern ingest transformers / GetLogObject / Lambda-vended log delivery and metric-filter alarm wording, not PutMetricFilter dimension validation, metricFilterCount accounting or the StartQuery error body.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Cause 1 and 2: replay fixture logs-metric-filter-pattern-dimension-support-and-count-progression.
   - step task#8 issues PutMetricFilter against the log group created earlier in the fixture with a filterPattern that extracts no named fields and a metricTransformations[0].dimensions map. Expect HTTP 400 InvalidParameterException "The specified filter pattern does not support dimensions"; the emulator answers HTTP 200 with {}.
   - step task#9 then issues DescribeLogGroups for that group. Expect logGroups[0].metricFilterCount == 1; the emulator returns 2.
3. Cause 3: replay fixture logs-insights-start-query-and-describe-workflow, step task#4 — StartQuery with a syntactically invalid queryString. Status 400 is correct; inspect the JSON body's `message`: the emulator appends "(Service: AWSLogs; Status Code: 400; Error Code: MalformedQueryException; Request ID: ...; Proxy: null)" after the parser diagnostic. Same divergence reproduces from logs-log-group-query-discovery#task#6, logs-osdocstat-traj-insights-query-audit-group#task#7 and logs-osdocstat-traj-log-group-arn-forms#task#7.
4. Re-run replay_wire_corpus() after the fix: all four StartQuery steps, plus task#8 and task#9 of the metric-filter fixture, must pass.
