Issue-Source: trace-sweep-divergence
Issue-Trace: logs-describe-queries-pagination
Issue-Step: task#8
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: logs-describe-queries-pagination logs-insights-query-single-log-group logs-insights-query-workflow logs-import-task-lifecycle logs-insights-start-query-and-describe-workflow logs-log-group-query-discovery logs-osdocstat-traj-insights-query-audit-group logs-osdocstat-traj-log-group-arn-forms logs-log-group-lifecycle-and-tags
Issue-Shape-Id: com.amazonaws.cloudwatchlogs#Logs_20140328
Issue-Key: logs|DescribeQueries|2xx->2xx|$.queries[*].queryString
Issue-Key: logs|DescribeQueries|2xx->2xx|$.queries[*]
Issue-Key: logs|DescribeImportTaskBatches|4xx->4xx|$.__type
Issue-Key: logs|DescribeImportTaskBatches|4xx->4xx|$.message
Issue-Key: logs|StartQuery|4xx->4xx|$.message
Issue-Key: logs|ListLogGroups|2xx->2xx|$.nextToken
Issue-Commit: 9c5c108e968319a9f7cfd2267f751f496cc6a487

# logs: 4 causes — Insights queryString echo, ImportId format validation, MalformedQueryException message and ListLogGroups pagination token all diverge

> _Note: this issue was identified against the code base at commit `9c5c108e968319a9f7cfd2267f751f496cc6a487`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeQueries, StartQuery, DescribeImportTaskBatches, ListLogGroups

## Description
Four independent net-new root causes in the CloudWatch Logs (service token `logs`) emulator, found by replaying the wire corpus simparity-wire-aws-v1. Cause count: 4. Each cause below names its own fixture and step; ALL FOUR must be closed before submitting — the four replay bindings are carried by Issue-Trace/Issue-Step plus Issue-Trace-Ids.

=== Cause 1 of 4: the Insights queryString is not echoed back verbatim — the millisecond epoch in a SOURCE command's END= is truncated to whole seconds ===
AWS: Smithy (services/service-com-amazonaws-cloudwatchlogs-logs-20140328) models StartQuery.queryString as a required QueryString member (@length 0..10000) and DescribeQueries.queries as a QueryInfoList whose QueryInfo carries queryString. The model states no normalization of that text, and the recording shows AWS storing and returning the query string BYTE-FOR-BYTE as submitted: a query submitted as `SOURCE "<logGroupName>" START=<startEpochMillis> END=<endEpochMillis> | fields @message | filter @message like /level/ | limit 12` comes back from DescribeQueries with exactly that text, including the full millisecond END= value. (Status 200 in both cases; this is a response-content divergence, not an error.)
Emulator: DescribeQueries returns the same query with the END= epoch shortened by its last three digits — the millisecond value is re-rendered as a whole-second value (recorded instance: submitted `END=4102444800999`, returned `END=4102444800`; the START=, the pipeline stages and the limit are unchanged). So the emulator is not storing the submitted text but re-serializing a parsed form of it, and the ms→s conversion of the SOURCE time bounds is lossy.
Why: queryString is the API's record of WHAT was run. A client that reads a query back to re-submit it, to key a cache on it, to diff a scheduled query against its definition, or to display it to an operator gets a query whose END bound is 1000x too small — re-submitting the echoed text queries a window that ended in 1970 and returns no events, with no error anywhere to explain it.
Fix: the StartQuery handler's query-record state (store the submitted queryString verbatim rather than a parsed/re-rendered form) and/or the DescribeQueries serializer that emits QueryInfo.queryString.
Traces: logs-describe-queries-pagination#task#8 (also task#9, task#10) and logs-insights-query-single-log-group#task#6 (also logs-insights-query-workflow#task#3) — the same truncation in a DescribeQueries whose other QueryInfo members (createTime, queryDuration, bytesScanned, userIdentity) are shape-only assertions and do match.

=== Cause 2 of 4: DescribeImportTaskBatches does not validate ImportId's format, so a malformed import id is refused as not-found instead of as an invalid parameter ===
AWS: Smithy models DescribeImportTaskBatches.importId as required, @length 1..256, @pattern ^[\-a-zA-Z0-9]+$, and binds both InvalidParameterException and ResourceNotFoundException (both client, HTTP 400). Beyond the pattern, the recorded behavior shows AWS additionally requiring the id to be a UUID and checking that BEFORE any lookup: an importId that satisfies the pattern but is not a UUID is refused HTTP 400 with __type `InvalidParameterException` and the fixed message `ImportId must be a valid UUID format` (no variable parts — the rejected value is not echoed). ResourceNotFoundException with `No matching import found.` is reserved for a WELL-FORMED uuid that names no import.
Emulator: the same request is refused HTTP 400 with __type `ResourceNotFoundException` and message `No matching import found.` — the format precondition is absent and the handler goes straight to the import lookup, which misses.
Why: the two classes mean opposite things to a caller. InvalidParameterException says the request is wrong and must be rebuilt; ResourceNotFoundException says the request is fine but the import is gone, which is exactly the condition a caller polls or retries through. A client that typo'd or truncated an import id is told the import does not exist, so a `ResourceNotFoundException` branch (give up / treat as deleted) runs where the `InvalidParameterException` branch (fix the argument) should, and the validation error is never surfaced.
Fix: the DescribeImportTaskBatches handler's input validation — add the UUID-format precondition on importId, raising the modeled InvalidParameterException with AWS's fixed sentence, ahead of the state lookup that produces the not-found refusal.
Traces: logs-import-task-lifecycle#task#4. Related but distinct and NOT covering this: open issue 20261119T142436Z-logs-2-causes-createimporttask-misclassifies-a-nonexistent-import.md is about CreateImportTask's importSourceArn classification (and only mentions DescribeImportTaskBatches' not-found message for a well-formed id, which the emulator already gets right); it does not carry this replay key.

=== Cause 3 of 4: StartQuery's MalformedQueryException message carries a client-side SDK envelope inside the wire `message` field ===
AWS: Smithy binds MalformedQueryException to StartQuery (client, HTTP 400). On the wire the error shape's `message` is the bare query diagnostic only — the parser's text naming the offending symbol and its line/position, e.g. `unexpected symbol found <symbol> at line <n> and position <m>`, where the fixed text is `unexpected symbol found ... at line ... and position ...` and the symbol/line/position are derived from the submitted queryString. The service name, HTTP status, error code, request id and proxy are response METADATA: AWS carries them in the HTTP status line, the __type field and the x-amzn-RequestId header, and it is the AWS Java SDK v1 client that composes them into a `(Service: ...; Status Code: ...; Error Code: ...; Request ID: ...; Proxy: null)` suffix when it renders an exception. AWS never puts that suffix in the serialized `message`.
Emulator: the 400 refusal's `message` is the diagnostic with that whole client-side envelope appended: `unexpected symbol found abc at line 1 and position 24 (Service: AWSLogs; Status Code: 400; Error Code: MalformedQueryException; Request ID: <uuid>; Proxy: null)`. The fixed, always-wrong part is the parenthetical ` (Service: AWSLogs; Status Code: 400; Error Code: MalformedQueryException; Request ID: ...; Proxy: null)`; the request id inside it is per-run and is NOT what is being reported — the defect is that the envelope is on the wire at all, duplicating the status and the code that already travel in the status line and __type.
Why: the message is what a client logs and shows an operator, and tooling parses it for the symbol/position to point at the bad token. A message that re-states response metadata makes that parse fail, and any SDK that renders its own envelope produces doubled, contradictory text (two status codes, two request ids — one of them not the id of the call the client made).
Fix: the StartQuery handler's query-validation error construction / the logs error serializer — emit only the parser diagnostic as the exception `message` and let status, __type and the request-id header carry the metadata.
Traces: logs-insights-start-query-and-describe-workflow#task#4 (also logs-log-group-query-discovery#task#6, logs-osdocstat-traj-insights-query-audit-group#task#7, logs-osdocstat-traj-log-group-arn-forms#task#7). Distinct from open issue 20261117T115022Z-logs-5-causes-generic-not-found-invalid-token.md, whose cause 5 is about StartQuery picking InvalidParameterException instead of MalformedQueryException for a query with NO log-group selector; here the class is already right and the MESSAGE diverges. That issue does not carry this replay key.

=== Cause 4 of 4: ListLogGroups omits nextToken on a truncated page, so the result set cannot be paged ===
AWS: Smithy models ListLogGroups with input `limit` (ListLimit, @range 1..1000) and `nextToken` (@length min 1), and output `logGroups` plus `nextToken` (@length min 1). When more log groups match than the page returns, AWS populates the output nextToken with an opaque continuation token, which the caller replays as the input nextToken to fetch the next page; it is omitted only on the last page.
Emulator: the truncated page comes back HTTP 200 with the logGroups array but NO nextToken member at all, so the response looks like a complete, final page.
Why: the caller has no way to continue, and no signal that it should. Every paginating client — including SDK paginators, which stop as soon as the token is absent — silently observes only the first page and treats a partial list as the account's complete set of log groups, so downstream inventory, cleanup and tagging passes skip every group past the first page with no error.
Fix: the ListLogGroups handler's pagination state/serializer — mint and emit the continuation token whenever matching log groups remain beyond the returned page (the token value itself is environment-specific and is not asserted; only its presence is).
Traces: logs-log-group-lifecycle-and-tags#task#3.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up, /__admin/health 200; 1266 steps, 1167 passed, score 92.2%). All four causes are `verdict: mismatch` clusters with empty `owned_by` and empty `stub_path`, i.e. not covered by any already-filed issue.

Cause 1 — fixture logs-describe-queries-pagination, step task#8, DescribeQueries, status 200 -> 200:
  key $.queries[0].queryString
  expected: 'SOURCE "<log-group>" START=0 END=4102444800999 | fields @message | filter @message like /level/ | limit 12'
  actual:   'SOURCE "<log-group>" START=0 END=4102444800 | fields @message | filter @message like /level/ | limit 12'
  cluster members: logs-describe-queries-pagination#task#8, #task#9, #task#10.
Same truncation via fixture logs-insights-query-single-log-group, step task#6 (cluster key $.queries[0]): expected queryString '... START=0 END=4102444800999 | fields @timestamp, @message | sort @timestamp desc | limit 20 | limit 20', actual '... START=0 END=4102444800 | ...'; the other QueryInfo members in that object (createTime, queryDuration, bytesScanned, userIdentity) are shape-only expectations and are satisfied. Cluster members: logs-insights-query-single-log-group#task#6, logs-insights-query-workflow#task#3.

Cause 2 — fixture logs-import-task-lifecycle, step task#4, DescribeImportTaskBatches, status 400 -> 400:
  $.__type:   expected 'InvalidParameterException', got 'ResourceNotFoundException'
  $.message:  expected 'ImportId must be a valid UUID format', got 'No matching import found.'
  Fixed text of the expected message: the whole sentence 'ImportId must be a valid UUID format' (no request- or state-derived parts).

Cause 3 — fixture logs-insights-start-query-and-describe-workflow, step task#4, StartQuery, status 400 -> 400:
  $.message: expected <recorded value masked by the fixture, because the symbol/line/position vary>, got 'unexpected symbol found abc at line 1 and position 24 (Service: AWSLogs; Status Code: 400; Error Code: MalformedQueryException; Request ID: 42c5f30e-d1be-44dc-a381-d0ea75253285; Proxy: null)'.
  The diverging part is the appended fixed envelope ' (Service: AWSLogs; Status Code: 400; Error Code: MalformedQueryException; Request ID: <per-run uuid>; Proxy: null)'; the diagnostic prefix ('unexpected symbol found <symbol> at line <n> and position <m>', whose symbol/line/position come from the submitted queryString) is the only part AWS serializes. The request id inside the envelope is a per-run value and is not itself the finding.
  cluster members: logs-insights-start-query-and-describe-workflow#task#4, logs-log-group-query-discovery#task#6, logs-osdocstat-traj-insights-query-audit-group#task#7, logs-osdocstat-traj-log-group-arn-forms#task#7.

Cause 4 — fixture logs-log-group-lifecycle-and-tags, step task#3, ListLogGroups, status 200 -> 200:
  $.nextToken: missing — expected present (value shape-only, not asserted, since the token is opaque and environment-specific), got None.

## Steps to Reproduce
Cause 1:
1. logs.CreateLogGroup(<name>) and put a few events into a stream in it.
2. logs.StartQuery(queryString='SOURCE "<name>" START=0 END=4102444800999 | fields @message | filter @message like /level/ | limit 12', startTime=0, endTime=4102444800999).
3. logs.DescribeQueries(logGroupName=<name>) and read queries[0].queryString.
4. Observe 'END=4102444800' where the submitted text said 'END=4102444800999'; AWS echoes the submitted string verbatim.

Cause 2:
1. logs.DescribeImportTaskBatches(importId=<a value matching ^[\-a-zA-Z0-9]+$ that is NOT a UUID, e.g. 'not-a-uuid'>).
2. Observe HTTP 400 __type 'ResourceNotFoundException' / message 'No matching import found.'; AWS answers HTTP 400 __type 'InvalidParameterException' / message 'ImportId must be a valid UUID format'.
3. For contrast, repeat with a well-formed UUID that names no import: both AWS and the emulator answer ResourceNotFoundException 'No matching import found.'

Cause 3:
1. logs.CreateLogGroup(<name>).
2. logs.StartQuery(logGroupName=<name>, startTime=0, endTime=<epoch>, queryString=<a query with a syntax error, e.g. one with a stray token 'abc' partway through>).
3. Observe HTTP 400 MalformedQueryException whose message ends with ' (Service: AWSLogs; Status Code: 400; Error Code: MalformedQueryException; Request ID: <uuid>; Proxy: null)'; AWS's wire message is the bare parser diagnostic with no such suffix.

Cause 4:
1. logs.CreateLogGroup several times so more log groups exist than one page will hold.
2. logs.ListLogGroups(limit=<less than the number of matching groups>).
3. Observe HTTP 200 with a short logGroups array and no nextToken member; AWS returns a nextToken that continues the listing.
