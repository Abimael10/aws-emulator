Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync__logs.appsync-field-log-level-all-delivers-tracing-for-the-response-request-id
Issue-Key: appsync__logs.appsync-field-log-level-all-delivers-tracing-for-the-response-request-id|_poll_until#10|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Commit: b3a98b26c8bf2cb2bdcd9c0a5c1e0e4aad254ca8

# appsync__logs: with fieldLogLevel ALL the API's GraphQL endpoint never answers the resolver, so no RequestSummary/Tracing event is ever delivered to /aws/appsync/apis/<apiId>

> _Note: this issue was identified against the code base at commit `b3a98b26c8bf2cb2bdcd9c0a5c1e0e4aad254ca8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateGraphqlApi

## Description
Cause 1 of 1: a GraphQL API created with logConfig.fieldLogLevel=ALL never serves its resolver's answer on the published endpoint, so the request-logging leg (RequestSummary + Tracing events in CloudWatch Logs) can never happen.
Failing check id: appsync__logs.appsync-field-log-level-all-delivers-tracing-for-the-response-request-id
AWS: Smithy: CreateGraphqlApi accepts logConfig {fieldLogLevel (enum NONE|ERROR|ALL), cloudWatchLogsRoleArn, excludeVerboseContent} and echoes it back on GraphqlApi$logConfig; CreateResolver returns Resolver$resolverArn. The log payloads themselves are recorded/documented behavior only (AppSync monitoring guide): with fieldLogLevel ALL, each GraphQL request served on uris.GRAPHQL (a) answers HTTP 200 whose body carries data.<field> with the resolver's value, (b) carries the response header x-amzn-RequestId — a per-request value, and the documented correlator — and (c) causes AppSync to publish, into log group /aws/appsync/apis/<apiId> (the variable part is the API's id), exactly one RequestSummary event per request keyed to that requestId (with graphQLAPIId == the api's id, statusCode == the status the client observed, latency a positive nanosecond integer) and one Tracing event per resolved field (parentType/fieldName/returnType taken from the SDL, path == the response path of the field, duration and startOffset nanosecond integers, resolverArn byte-identical to the resolverArn CreateResolver reported). A twin API with fieldLogLevel NONE publishes no Tracing events at all.
Emulator: the check never gets past step (a): the poll for the ALL API's resolver to answer on the live endpoint exhausts the run budget and the check dies with `TimeoutError: the ALL api's resolver to answer on the live endpoint: the run budget ended first; not graded`. The API, the ACTIVE schema, the NONE data source, the APPSYNC_JS resolver (runtime.earlyReturn(ctx.args.msg)) and the api key are all created successfully, and CreateGraphqlApi does store fieldLogLevel ALL, but no 200 response carrying data.probe == the probe marker is ever served, so no x-amzn-RequestId correlator and no log events exist to grade.
Why: a client that enables request/field logging to debug its resolvers sees an endpoint that never answers and a log group that never appears — both the data-plane answer and every observability signal built on it (RequestSummary status/latency, per-field Tracing with resolverArn, and the NONE-vs-ALL distinction) are absent, so monitoring and alarm wiring written against AWS silently observes nothing.
Fix: the AppSync GraphQL data plane — the handler that serves the published uris.GRAPHQL endpoint and executes an APPSYNC_JS UNIT resolver over a NONE data source must answer 200 with the resolved field and a per-request x-amzn-RequestId; the logging path must then emit RequestSummary/Tracing records to /aws/appsync/apis/<apiId> under the API's logConfig (and emit no Tracing at fieldLogLevel NONE).
Note on grading: the harness labels this timeout "not graded" because the run budget, not the poll's own deadline, ended the wait; it is filed anyway because the unanswered endpoint is the emulator's, not the runner's.
Related-but-not-covering: 20261110T153046Z-appsync-logs-appsync-the-graphql-api-s-published-uris-endpoint.md records this service token's published-uris host problem; it does not carry this check's replay key, and the symptom here is an endpoint that accepts the request but never serves the resolver's answer.
Traces: appsync__logs.appsync-field-log-level-all-delivers-tracing-for-the-response-request-id (error: TimeoutError waiting for the ALL api's resolver to answer on the live endpoint).

## Evidence
Expected (AWS): POST uris.GRAPHQL with x-api-key and `query P($m: String!) { probe(msg: $m) }` -> HTTP 200, body data.probe == the marker passed in variables, response header x-amzn-RequestId present and distinct per request; within minutes, filter_log_events on /aws/appsync/apis/<apiId> for that requestId returns exactly one RequestSummary (graphQLAPIId == apiId, statusCode == 200, latency > 0) and exactly one Tracing event for fieldName "probe" (parentType "Query", returnType "String", path ["probe"], duration > 0, integer startOffset, resolverArn == CreateResolver's resolverArn); the twin fieldLogLevel=NONE API logs no Tracing event.
Actual (emulator): the endpoint never serves a 200 whose data.probe equals the marker. The check fails before any logging assertion:
  File "check_appsync__logs_appsync_field_log_level_all_delivers_tracing_for_the_response_request_id.py", line 311, in run
    _poll_until(...)
  TimeoutError: the ALL api's resolver to answer on the live endpoint: the run budget ended first; not graded
Setup calls that did succeed: CreateGraphqlApi (logConfig.fieldLogLevel stored as ALL), StartSchemaCreation -> terminal SUCCESS/ACTIVE, CreateDataSource(type=NONE), CreateResolver(APPSYNC_JS 1.0.0), CreateApiKey.

## Steps to Reproduce
1. iam.CreateRole trusted by appsync.amazonaws.com with logs:CreateLogGroup/CreateLogStream/PutLogEvents.
2. appsync.CreateGraphqlApi(authenticationType=API_KEY, logConfig={fieldLogLevel: "ALL", cloudWatchLogsRoleArn: <that role>, excludeVerboseContent: false}); confirm logConfig.fieldLogLevel == "ALL" in the response.
3. StartSchemaCreation with `schema { query: Query } type Query { probe(msg: String!): String }`; poll GetSchemaCreationStatus to SUCCESS/ACTIVE.
4. CreateDataSource(type="NONE"); CreateResolver(typeName="Query", fieldName="probe", kind="UNIT", runtime={APPSYNC_JS,1.0.0}, code using runtime.earlyReturn(ctx.args.msg)); CreateApiKey.
5. POST to uris.GRAPHQL with x-api-key and `query P($m: String!) { probe(msg: $m) }`, variables {"m": "<marker>"} — AWS returns 200 with data.probe == "<marker>" and an x-amzn-RequestId header; the emulator never returns such a response, so the poll runs until the run budget ends.
6. (unreachable) filter_log_events on /aws/appsync/apis/<apiId> for that requestId and assert one RequestSummary and one Tracing event as described; repeat with a twin fieldLogLevel=NONE API and assert no Tracing event.
