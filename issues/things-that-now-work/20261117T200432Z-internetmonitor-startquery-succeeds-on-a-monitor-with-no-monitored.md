Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: internetmonitor.start-query-measurements-reaches-succeeded internetmonitor.get-query-results-schema-for-empty-monitor
Issue-Shape-Id: com.amazonaws.internetmonitor#InternetMonitor20210603
Issue-Key: internetmonitor.start-query-measurements-reaches-succeeded|run#9|AssertionError|raise AssertionError(f"expected StartQuery to be rejected, but it succeeded and returned {result!r}")
Issue-Key: internetmonitor.get-query-results-schema-for-empty-monitor|_error#8|AssertionError|raise AssertionError(f"expected this call to be rejected, but it succeeded and returned {result!r}")
Issue-Commit: 738532f931c20a228b1fc1d492f033ad05ac6ae5

# StartQuery succeeds on a monitor with no monitored resources instead of refusing it with BadRequestException "Invalid request body"

> _Note: this issue was identified against the code base at commit `738532f931c20a228b1fc1d492f033ad05ac6ae5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartQuery

## Description
Cause 1 of 1: StartQuery against an ACTIVE monitor that monitors no resources is accepted and mints a QueryId, where AWS refuses the request.

AWS: Internet Monitor's query interface only queries data a monitor actually collected. On a monitor with no monitored resources (created with TrafficPercentageToMonitor/MaxCityNetworksToMonitor but no Resources), StartQuery is refused for EVERY QueryType tried — the recordings exercise MEASUREMENTS and OVERALL_TRAFFIC_SUGGESTIONS — with HTTP 400, error code `BadRequestException` and message `Invalid request body`. The whole message is fixed text for this error class: it carries no value taken from the request or from state. Smithy: StartQuery POST /v20210603/Monitors/{MonitorName}/Queries, output StartQueryOutput.QueryId @required; the model lists AccessDeniedException/InternalServerException/LimitExceededException/ThrottlingException/ValidationException for the operation and BadRequestException ("A bad request was received", HTTP 400) elsewhere in the same service, so the refusal shape is modeled by the service — the resourceless-monitor rule itself is recorded behavior only. The monitor must be unaffected by the refusal: it still reads back ACTIVE through GetMonitor.

Emulator: the request returns HTTP 200 with a minted QueryId (observed body `{"QueryId": "..."}`, content-length 50) and records a query in state. The handler's doc comment states the position explicitly — "the monitor's own contents condition nothing, since what a monitor holds decides what a query RETURNS rather than whether it may be started" — and its only BadRequestException path is an EndTime earlier than StartTime; a monitor that holds no resources is never checked.

Why: a client that relies on AWS refusing an unanswerable query sees a success. It receives a QueryId AWS would never mint and then polls GetQueryStatus/GetQueryResults for a query that cannot exist, so negative-path code (catching BadRequestException to fall back, surface a user error, or skip the poll loop) is dead against the emulator, and query-lifecycle tests pass here and fail on AWS. The divergence is a success-vs-refusal flip, the hardest class for a client to absorb.

Fix: StartQuery's handler/validation in the internetmonitor service crate — refuse the start, for every QueryType, when the named monitor holds no monitored resources, with BadRequestException (HTTP 400, message `Invalid request body`), before any QueryId is minted or any query recorded, and leave the monitor's own Status untouched. The existing BadRequestException construction and the `INVALID_REQUEST_BODY_MESSAGE` constant it already uses are the right shape and wording; the missing part is the resourceless-monitor precondition.

Traces: internetmonitor.start-query-measurements-reaches-succeeded (StartQuery MEASUREMENTS; asserts BadRequestException/"Invalid request body", got 200 + QueryId) and internetmonitor.get-query-results-schema-for-empty-monitor (same refusal expected for MEASUREMENTS and OVERALL_TRAFFIC_SUGGESTIONS, got 200 + QueryId). Note: the second check also asserts GetQueryResults for an unminted QueryId answers ValidationException "Invalid query ID provided", but it aborted on this StartQuery success and never reached that call, so that assertion is unverified and is not claimed here — re-replaying that check after the fix will exercise it.

File/line citations: services/service-com-amazonaws-internetmonitor-internetmonitor20210603/src/ops/start_query.rs lines 11-13 (doc stating monitor contents condition nothing), 225-245 (the only refusal paths: unknown monitor, then EndTime < StartTime), 247-258 (unconditional mint/record).

## Evidence
Expected (recorded AWS, StartQuery on an ACTIVE monitor with no monitored resources, QueryType MEASUREMENTS and OVERALL_TRAFFIC_SUGGESTIONS):
  HTTP 400, Error.Code = "BadRequestException", Error.Message = "Invalid request body" (entirely fixed text), no QueryId returned; GetMonitor afterwards still reports Status = ACTIVE.

Actual (emulator):
  HTTP 200 with body {"QueryId": "<minted id>"}. Replay output:
  AssertionError: expected StartQuery to be rejected, but it succeeded and returned {'ResponseMetadata': {'HTTPStatusCode': 200, ...}, 'QueryId': '00000000-0000-4000-8000-000000000001'}
  (the id value itself is per-run and is not part of the requirement; the 200-instead-of-400 and the presence of a QueryId are.)

Same failure from both checks, one for each QueryType path.

## Steps to Reproduce
1. internetmonitor CreateMonitor(MonitorName=<name>, TrafficPercentageToMonitor=100, MaxCityNetworksToMonitor=500) and wait until GetMonitor reports Status=ACTIVE. Do not add any Resources.
2. Call StartQuery(MonitorName=<name>, StartTime=now-1h, EndTime=now, QueryType="MEASUREMENTS", FilterParameters=[]).
   Expected: HTTP 400 BadRequestException, message "Invalid request body".
   Actual: HTTP 200 with a minted QueryId.
3. Repeat step 2 with QueryType="OVERALL_TRAFFIC_SUGGESTIONS" — same divergence.
4. GetMonitor(MonitorName=<name>) must still report Status=ACTIVE (it does).
