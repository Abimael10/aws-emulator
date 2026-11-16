Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync__logs.appsync-field-log-level-all-delivers-tracing-for-the-response-request-id
Issue-Key: appsync__logs.appsync-field-log-level-all-delivers-tracing-for-the-response-request-id|_poll_until#10|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Commit: 8a8830456e168aad64737c1eda6ee6081a6c4868

# appsync__logs: a fieldLogLevel=ALL api's resolver never answers on the live endpoint, so no request-id-correlated field tracing is ever written to CloudWatch Logs

> _Note: this issue was identified against the code base at commit `8a8830456e168aad64737c1eda6ee6081a6c4868`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateResolver

## Description
Cause 1 of 1: on a GraphQL API configured with logConfig fieldLogLevel=ALL, the resolver bound to the probed field never produces an answer on the published endpoint, so the check's bounded poll for a live response exhausts the run budget and the downstream CloudWatch Logs assertions (the per-field tracing records correlated by the response's x-amzn-RequestId) are never reached.
AWS: CreateGraphqlApi (Smithy: appsync CreateGraphqlApi, POST /v1/apis; logConfig -> LogConfig{fieldLogLevel: FieldLogLevel NONE|ERROR|ALL, cloudWatchLogsRoleArn, excludeVerboseContent}) plus CreateResolver (POST /v1/apis/{apiId}/types/{typeName}/resolvers; requestMappingTemplate/responseMappingTemplate -> MappingTemplate @length 1..65536, or code/runtime APPSYNC_JS; errors BadRequestException 400, NotFoundException 404, UnauthorizedException 401, ConcurrentModificationException 409, InternalFailureException 500). Recorded behavior only for the data plane: the POST returns HTTP 200 with the resolved field and an x-amzn-RequestId header, and with fieldLogLevel=ALL AppSync writes request/response-summary and per-field tracing entries into the api's /aws/appsync/apis/<apiId> log group, each carrying that same requestId.
Emulator: the resolver never answers on the live endpoint — the check's _poll_until for "the ALL api's resolver to answer on the live endpoint" raises TimeoutError("...: the run budget ended first; not graded"). No status, body or log stream is observable; the logging behaviour cannot even be reached. This is the appsync__logs instance of the data-plane resolver-execution family filed for the appsync token in this same sweep; the open issue 20261110T153046Z-appsync-logs-appsync-the-graphql-api-s-published-uris-endpoint.md covers only the endpoint-publication half of this token and does not carry this replay key.
Why: a client that turns on fieldLogLevel=ALL to debug a resolver gets neither its answer nor any log record, and observability wired on AppSync logs (log-metric filters, alarms, log-based tracing) cannot be exercised at all.
Fix: the GraphQL data-plane executor (resolver dispatch and its VTL/APPSYNC_JS runtime) must answer the field, and the data plane's logging path must then emit the fieldLogLevel=ALL request/response/field tracing entries into the api's CloudWatch Logs group, stamped with the same request id the response header carries.
Traces: appsync__logs.appsync-field-log-level-all-delivers-tracing-for-the-response-request-id.

## Evidence
Check appsync__logs.appsync-field-log-level-all-delivers-tracing-for-the-response-request-id [error]:

  File "check_appsync__logs_appsync_field_log_level_all_delivers_tracing_for_the_response_request_id.py", line 311, in run
    _poll_until(
  File "...", line 49, in _poll_until
    raise TimeoutError(f"{description}: the run budget ended first; not graded")
  TimeoutError: the ALL api's resolver to answer on the live endpoint: the run budget ended first; not graded

Expected (AWS): the POST of `query { probe(msg: ...) }` to the api's GRAPHQL uri answers HTTP 200 with data.probe resolved and an x-amzn-RequestId header; CloudWatch Logs group /aws/appsync/apis/<apiId> then contains tracing entries for that request id (the request id and api id are per-run values; the requirement is that the entries exist and share the response's request id).
Actual (emulator): no response from the resolver within the poll window, so nothing downstream is observable.

Smithy cited: appsync CreateGraphqlApi (POST /v1/apis; logConfig -> LogConfig{fieldLogLevel FieldLogLevel, cloudWatchLogsRoleArn, excludeVerboseContent}); CreateResolver (POST /v1/apis/{apiId}/types/{typeName}/resolvers; requestMappingTemplate/responseMappingTemplate -> MappingTemplate @length 1..65536 @pattern ^.*$, code -> Code @length 1..32768, runtime AppSyncRuntime; errors BadRequestException 400 / NotFoundException 404 / UnauthorizedException 401 / ConcurrentModificationException 409 / InternalFailureException 500). The data-plane POST, its x-amzn-RequestId header and the log-record layout are not modeled — recorded behavior only.

## Steps to Reproduce
1. iam.create_role trusting appsync.amazonaws.com with the AWSAppSyncPushToCloudWatchLogs permissions.
2. api = appsync.create_graphql_api(name=..., authenticationType="API_KEY", logConfig={"fieldLogLevel": "ALL", "cloudWatchLogsRoleArn": <role>, "excludeVerboseContent": False})["graphqlApi"]; appsync.create_api_key(apiId=api["apiId"]).
3. appsync.start_schema_creation(apiId, definition=b'schema { query: Query } type Query { probe(msg: String!): String }'); poll to SUCCESS. create_data_source(type="NONE") and create_resolver for Query.probe.
4. POST {"query":"query { probe(msg: \"hello\") }"} to api["uris"]["GRAPHQL"] with x-api-key, polling for a live answer.
   Observed: the resolver never answers; the poll runs out of budget (TimeoutError).
   Expected: 200 with data.probe resolved plus an x-amzn-RequestId header, followed by tracing events for that request id in the api's /aws/appsync/apis/<apiId> log group.
