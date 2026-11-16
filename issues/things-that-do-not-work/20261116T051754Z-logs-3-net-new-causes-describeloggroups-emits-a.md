Issue-Source: trace-sweep-divergence
Issue-Trace: logs-describe-log-groups-pagination
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: logs-describe-log-groups-pagination logs-insights-start-query-and-describe-workflow logs-log-group-query-discovery logs-osdocstat-traj-insights-query-audit-group logs-osdocstat-traj-log-group-arn-forms logs-logs-insights-query-workflow
Issue-Shape-Id: com.amazonaws.cloudwatchlogs#Logs_20140328
Issue-Key: logs|DescribeLogGroups|2xx->2xx|$.nextToken
Issue-Key: logs|StartQuery|4xx->4xx|$.message
Issue-Key: logs|DeleteQueryDefinition|4xx->4xx|$.message
Issue-Commit: d82f0db59041398d88dcfc9540f4f1030809eb47

# logs: 3 net-new causes — DescribeLogGroups emits a nextToken on its last page, and StartQuery and DeleteQueryDefinition refusal messages are not AWS's wording

> _Note: this issue was identified against the code base at commit `d82f0db59041398d88dcfc9540f4f1030809eb47`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeLogGroups, StartQuery, DeleteQueryDefinition

## Description
Three net-new root causes in the CloudWatch Logs read/query surface, found by replaying the wire fixture corpus (simparity-wire-aws-v1) against the emulator. They share one shape of fault: the operation reaches the right HTTP status and the right error code, but the response body it serves is not the body AWS serves — an extra pagination token on one, re-worded refusal messages on the other two. None of the three failing steps is carried as a replay key by any open issue, and none is blocked by an unimplemented operation. ALL THREE causes must be closed before this issue is submitted; each cause below names its own fixture and step.

--- Cause 1 of 3: DescribeLogGroups mints a nextToken for the page that already returned the last matching log group.

AWS: Smithy (logs, com.amazonaws.cloudwatchlogs#Logs_20140328) DescribeLogGroups is @paginated {inputToken: nextToken, outputToken: nextToken, items: logGroups, pageSize: limit}; the output member `nextToken` targets NextToken (string, @length min 1) documented as "The token for the next set of items to return. The token expires after 24 hours." A paginated AWS read emits `nextToken` only when a further set of items exists; when the page just served contains the last item that matches the request's selection (here a `logGroupNamePrefix`), the response carries `logGroups` and OMITS `nextToken` entirely — that absence is the documented termination condition every SDK paginator and every `while nextToken` loop keys on. Recorded behavior confirms it: the final page of the prefix walk comes back with no `nextToken` member at all.

Emulator: the final page is served with HTTP 200 and the correct `logGroups`, but it also carries a `nextToken`. The token is a hex-encoded plaintext JSON cursor whose `after` field is the name of the LAST log group the page just returned (decoded: {"kind":"DescribeLogGroups","logGroupNamePrefix":"<prefix>","logGroupNamePattern":null,"logGroupClass":null,"logGroupIdentifiers":null,"after":"<last log group name on this page>"}), i.e. the handler mints a continuation token unconditionally from the tail of whatever page it emitted instead of minting one only when unreturned matches remain.

Why: a client that paginates the documented way — call again while `nextToken` is present — is told there is another page when there is not. It makes an extra DescribeLogGroups round trip that returns nothing, and any code that treats "token present" as "more results exist" (progress reporting, prefetching, assembling a complete inventory, or a loop that only breaks on an absent token) draws the wrong conclusion or fails to terminate on the first pass. It also leaks the reader's internal cursor state onto the wire, where AWS's token is opaque.

Fix: DescribeLogGroups' handler/serializer pagination stage — decide the token from whether unreturned matches remain after the page is cut (look ahead past the page boundary, or compare the page size to the remaining match count) and omit `nextToken` when they do not, rather than always echoing the page's last key.

Traces: logs-describe-log-groups-pagination#task#5 (DescribeLogGroups, expected 200 / actual 200, `$.nextToken: unexpected_present`). Code: services/service-com-amazonaws-cloudwatchlogs-logs-20140328/src/ops/describe_log_groups.rs.

--- Cause 2 of 3: StartQuery's MalformedQueryException body is a bare parser diagnostic, not AWS's refusal for an invalid query string.

AWS: Smithy (logs) StartQuery takes `queryString` (QueryString, @length 0..10000, required) and models MalformedQueryException (client, HTTP 400) with members `queryCompileError` (QueryCompileError) and `message`; the error is documented as "The query string is not valid. Details about this error are displayed in a QueryCompileError object." So for a query string that does not compile, the requirement is HTTP 400 with error code `MalformedQueryException` and a body whose message identifies the fault class and whose compile detail accompanies it. Recorded behavior only: the corpus records this refusal's `message` as a shaped (non-literal) value, so the fixed text is asserted by shape rather than quoted; the status (400) and the code (MalformedQueryException) are recorded literally and the emulator matches both. The variable parts of such a message are taken from the request — the offending token from the submitted `queryString` and its line/position — and the per-call request id; the fixed text is the service's own sentence for the fault class, and it is that fixed text the emulator does not supply.

Emulator: HTTP 400 with `__type` MalformedQueryException (both correct) and `$.message` = "unexpected symbol found <token> at line <n> and position <n> (Service: AWSLogs; Status Code: 400; Error Code: MalformedQueryException; Request ID: <per-call request id>; Proxy: null)" — in the recorded run, "unexpected symbol found abc at line 1 and position 24 (…)". That is the parser's own lower-case diagnostic emitted as the whole message, with no service sentence in front of it and no `queryCompileError` object beside it, and it does not satisfy the recorded message assertion. Hypothesis: the Insights compiler's internal parse-error string is passed straight through as the modelled error's message.

Why: StartQuery is the entry point for every Logs Insights client, and a malformed query is the single most common refusal it serves. Clients, the CLI and SDK error-mapping layers key on this message to tell an uncompilable query apart from the other 400s StartQuery models (InvalidParameterException, LimitExceededException, ResourceNotFoundException), and consoles surface the compile detail to point at the offending token. A message that is only the raw parser diagnostic — and a body with no QueryCompileError — gives them nothing stable to match and nothing to locate the error with, even though the status and code look right.

Fix: StartQuery's query-compile refusal construction (the path that turns a compile failure into the modelled MalformedQueryException) — it must word the message as AWS words this fault class, interpolating the offending token and its line/position from the submitted `queryString`, and populate the modelled `queryCompileError` member rather than emitting the parser string alone.

Traces: logs-insights-start-query-and-describe-workflow#task#4 (representative), and the same divergence on logs-log-group-query-discovery#task#6, logs-osdocstat-traj-insights-query-audit-group#task#7, logs-osdocstat-traj-log-group-arn-forms#task#7. Code: services/service-com-amazonaws-cloudwatchlogs-logs-20140328/src/ops/start_query.rs.

--- Cause 3 of 3: DeleteQueryDefinition's not-found message carries the PutQueryDefinition guidance sentence.

AWS: Smithy (logs) DeleteQueryDefinition takes `queryDefinitionId` (QueryId, @length 1..256, required) and models ResourceNotFoundException (client, HTTP 400). Recorded behavior for a `queryDefinitionId` that names no definition (including one that was just deleted): HTTP 400, error code `ResourceNotFoundException`, message `Query definition id is invalid. (Service: AWSLogs; Status Code: 400; Error Code: ResourceNotFoundException; Request ID: <per-call request id>; Proxy: null)`. Fixed text: the sentence `Query definition id is invalid.` plus the ` (Service: AWSLogs; Status Code: 400; Error Code: ResourceNotFoundException; Request ID: …; Proxy: null)` suffix CloudWatch Logs appends; the only variable part is the per-call request id. Nothing about the update/create selector belongs on the delete path, which has no such option.

Emulator: the status (400) and the code (ResourceNotFoundException) are right, but the message is `Query definition id is invalid. If you are updating query definition, please provide correct query definition id; If you are creating new query definition, do not use --query-definition-id as a parameter (Service: AWSLogs; …; Proxy: null)` — the guidance sentence AWS adds only on the PutQueryDefinition path (where `queryDefinitionId` is the optional update selector) is spliced into the delete-path refusal. Hypothesis: both refusals are built from one shared not-found helper written for the Put path.

Why: clients and SDK/CLI error-message matching key on this message to distinguish an unknown query-definition id from the other ResourceNotFoundException causes, and the delete refusal advises the caller about a `--query-definition-id` option that DeleteQueryDefinition does not have. Any assertion or log-scrape on AWS's wording fails.

Fix: DeleteQueryDefinition's not-found refusal construction — give the delete path its own message `Query definition id is invalid.` and leave the longer guidance sentence to PutQueryDefinition.

Traces: logs-logs-insights-query-workflow#task#20. Code: services/service-com-amazonaws-cloudwatchlogs-logs-20140328/src/ops/delete_query_definition.rs calls crate::state::query_definition_not_found_message() in services/service-com-amazonaws-cloudwatchlogs-logs-20140328/src/state.rs. Related open issue: 20261115T160223Z-logs-2-net-new-causes-insights-parse-output.md describes this same wording defect as its Cause 2, but it is a BEHAVIORAL issue whose replay keys are the check ids logs.delete-query-definition-removes-resource / logs.op-contract-delete-query-definition — it does not carry this wire fixture step, so this cluster is not covered by it and is filed here with its replay key.

## Evidence
Cause 1 — logs-describe-log-groups-pagination#task#5, DescribeLogGroups, expected HTTP 200 / actual HTTP 200.
  Divergence: `$.nextToken: unexpected_present (expected None, got '7b226b696e64223a2244657363726962654c6f6747726f757073…')`.
  Expected: the final page of the prefix walk carries `logGroups` and no `nextToken` member.
  Actual: the same page carries a `nextToken`; hex-decoded it is the plaintext internal cursor {"kind":"DescribeLogGroups","logGroupNamePrefix":"<prefix>","logGroupNamePattern":null,"logGroupClass":null,"logGroupIdentifiers":null,"after":"<the last log group name on the page just returned>"} — a continuation key pointing past the last item, with no further matches behind it.

Cause 2 — logs-insights-start-query-and-describe-workflow#task#4, StartQuery, expected HTTP 400 / actual HTTP 400.
  Divergence: `$.message: value_mismatch (expected '<shaped-not-asserted>', got 'unexpected symbol found abc at line 1 and position 24 (Service: AWSLogs; Status Code: 400; Error Code: MalformedQueryException; Request ID: <per-call request id>; Proxy: null)')`.
  Status 400 and error code MalformedQueryException both match the recording; only the message body diverges — it is the raw parser diagnostic with no service sentence for the fault class and no modelled `queryCompileError` object beside it. The offending token ('abc') and the line/position ('line 1 and position 24') are values taken from the request's `queryString`, not part of the required text.
  Same cluster, same divergence: logs-log-group-query-discovery#task#6, logs-osdocstat-traj-insights-query-audit-group#task#7, logs-osdocstat-traj-log-group-arn-forms#task#7 (4 affected steps).

Cause 3 — logs-logs-insights-query-workflow#task#20, DeleteQueryDefinition, expected HTTP 400 / actual HTTP 400.
  Divergence: `$.message: value_mismatch`.
  Expected: 'Query definition id is invalid. (Service: AWSLogs; Status Code: 400; Error Code: ResourceNotFoundException; Request ID: <per-call request id>; Proxy: null)'.
  Actual: 'Query definition id is invalid. If you are updating query definition, please provide correct query definition id; If you are creating new query definition, do not use --query-definition-id as a parameter (Service: AWSLogs; Status Code: 400; Error Code: ResourceNotFoundException; Request ID: <per-call request id>; Proxy: null)'.
  Status 400 and code ResourceNotFoundException match; the fixed text diverges by the spliced-in PutQueryDefinition guidance sentence. The request id in both strings is a per-run value and is not part of the requirement.

## Steps to Reproduce
Cause 1 (fixture logs-describe-log-groups-pagination, step task#5):
1. CreateLogGroup for a small set of log groups sharing one name prefix, e.g. <prefix>-a, <prefix>-b, <prefix>-c.
2. DescribeLogGroups(logGroupNamePrefix=<prefix>, limit=<n less than the number of groups>) and keep following the returned `nextToken` until the page that contains the LAST matching group (<prefix>-c).
3. Inspect that final response. Expected: `logGroups` present, `nextToken` absent. Observed: `nextToken` present, hex-decoding to the internal DescribeLogGroups cursor with "after" set to the last group name on the page just returned; the next call with it returns no further groups.

Cause 2 (fixture logs-insights-start-query-and-describe-workflow, step task#4):
1. CreateLogGroup <group>.
2. StartQuery(logGroupName=<group>, startTime=<t0>, endTime=<t1>, queryString=<a query string that does not compile — e.g. one whose 24th character begins an unexpected token such as `abc`>).
3. Read the refusal. Expected: HTTP 400, `__type` MalformedQueryException, message worded as AWS words an uncompilable-query refusal (service sentence plus the offending token and its line/position from the submitted queryString), with the modelled `queryCompileError` object in the body. Observed: HTTP 400 and MalformedQueryException (correct) but the message is only the parser diagnostic 'unexpected symbol found abc at line 1 and position 24 (Service: AWSLogs; …; Proxy: null)' and no queryCompileError accompanies it.

Cause 3 (fixture logs-logs-insights-query-workflow, step task#20):
1. PutQueryDefinition(name=<unique>, queryString='fields @timestamp, @message | limit 5') and take the returned `queryDefinitionId`.
2. DeleteQueryDefinition(queryDefinitionId=<that id>) — succeeds; DescribeQueryDefinitions no longer lists it.
3. DeleteQueryDefinition(queryDefinitionId=<the same, now dead, id>), or any id that was never allocated. Expected: HTTP 400 ResourceNotFoundException with message 'Query definition id is invalid. (Service: AWSLogs; Status Code: 400; Error Code: ResourceNotFoundException; Request ID: <per-call id>; Proxy: null)'. Observed: the same status and code with the PutQueryDefinition guidance sentence ('If you are updating query definition, … do not use --query-definition-id as a parameter') spliced in before the suffix.
