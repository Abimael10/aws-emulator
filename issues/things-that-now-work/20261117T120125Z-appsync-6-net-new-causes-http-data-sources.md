Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync.http-data-source-resolver-forwards-backend-status-and-body appsync.http-data-source-sigv4-signing-toggles-backend-authorization appsync.unit-resolver-util-helpers-single-response appsync.resolver-invalid-filter-expression-operator-rejected appsync.create-data-source-eventbridge-missing-config-rejected appsync.data-source-name-conflict-scoped-to-api appsync.list-graphql-apis-max-results-above-limit-rejected
Issue-Shape-Id: com.amazonaws.appsync#AWSDeepdishControlPlaneService
Issue-Key: appsync.http-data-source-resolver-forwards-backend-status-and-body|run#9|AssertionError|assert identity_resp is not None and not identity_resp.get("errors"), (
Issue-Key: appsync.http-data-source-sigv4-signing-toggles-backend-authorization|_poll_until#11|Timeout|time.sleep(interval_s)
Issue-Key: appsync.unit-resolver-util-helpers-single-response|run#19|AssertionError|assert not body.get("errors"), f"unexpected GraphQL errors: {body.get('errors')}"
Issue-Key: appsync.resolver-invalid-filter-expression-operator-rejected|run#22|KeyError|assert error["errorType"] == "DynamoDBFilterError", f"unexpected errorType: {error['errorType']!r}"
Issue-Key: appsync.create-data-source-eventbridge-missing-config-rejected|run#13|AssertionError|assert message == "Missing EventBridge data source config", (
Issue-Key: appsync|CreateDataSource|BadRequestException
Issue-Key: appsync.list-graphql-apis-max-results-above-limit-rejected|expect_error#2|AssertionError|assert got_message == message, f"expected message {message!r}, got {got_message!r}"
Issue-Commit: 316bc8d4d27618b04e0a3739e3ce26f595ac855b

# appsync: 6 net-new causes — HTTP data sources are never called, the VTL $util helper library is incomplete, a filter-expression error carries no errorType, and three refusal messages are emulator-authored

> _Note: this issue was identified against the code base at commit `316bc8d4d27618b04e0a3739e3ce26f595ac855b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDataSource

## Description
Six distinct, net-new root causes in the appsync emulator — three in the GraphQL data plane, three in control-plane refusal wording. Every one is grounded in a failing behavioral check that is neither ALREADY FILED nor OWNED by an open issue. A fixer must close all six before submitting.

Cause 1 of 6: an HTTP data source is never called — a resolver bound to a type=HTTP data source answers a GraphQL error announcing the endpoint does not call it.
Failing check ids: appsync.http-data-source-resolver-forwards-backend-status-and-body, appsync.http-data-source-sigv4-signing-toggles-backend-authorization
AWS: Smithy: CreateDataSource.type=HTTP with httpConfig (HttpDataSourceConfig{endpoint, authorizationConfig{authorizationType: AWS_IAM, awsIamConfig{signingRegion, signingServiceName}}}); the data-plane envelope is not a Smithy shape, so the rest is AppSync's documented HTTP-resolver contract plus recorded behavior. A UNIT resolver whose request mapping template is the documented HTTP request object {"version":"2018-05-29","method":...,"resourcePath":...,"params":{"query":{...},"headers":{...},"body":...}} causes AppSync to issue that HTTP request to the data source's endpoint and to bind the outcome to $ctx.result as {statusCode, headers, body} — the backend's status code and body are forwarded verbatim to the response template, including non-2xx statuses (the field resolves with the backend's status rather than failing). When authorizationConfig is AWS_IAM, AppSync signs the outbound request with SigV4 using the data source's service role for the configured signingServiceName/signingRegion, so a backend that requires authentication answers 200 for the signed data source and 403 for an otherwise identical unsigned one. HTTP status of the GraphQL call stays 200 and `errors` is absent on the happy path.
Emulator: the field never resolves. The 200 body is {"data":{"<field>":null},"errors":[{"path":["<field>"],"message":"This endpoint does not call the HTTP data source <dataSourceName>."}]} — an emulator-authored message for a capability that simply is not implemented; the signing variant does not even return (the check is cut off at the 60 s cap while polling for a successful invocation).
Why: every integration that reaches a REST/AWS-API backend through AppSync (the documented way to call any AWS service without Lambda) is dead: status-and-body forwarding cannot be observed, the SigV4-vs-unsigned authorization distinction cannot be observed, and clients get a null field plus a message no AWS SDK or application has ever seen instead of their backend's response.
Fix: the GraphQL data plane's resolver dispatch for HTTP data sources in the appsync service crate — build the outbound request from the rendered request template, sign it with the data source's service role when authorizationConfig is AWS_IAM, and bind {statusCode, headers, body} into ctx.result for the response template.
Related-but-not-covering: 20261114T023619Z-apigateway-appsync-an-appsync-http-data-source-signing.md and 20261111T065707Z-apigateway-appsync-the-graphql-api-s-published-uris.md record the signing scenario under the apigateway__appsync service token; neither carries these checks' replay keys.
Traces: appsync.http-data-source-resolver-forwards-backend-status-and-body (errors[0].message "This endpoint does not call the HTTP data source sim2real_ds_<id>."), appsync.http-data-source-sigv4-signing-toggles-backend-authorization (timed out after 60 s with no successful invocation).

Cause 2 of 6: the VTL engine's $util helper library is incomplete — documented string/math/list/map helpers abort the resolver with "No such helper".
Failing check id: appsync.unit-resolver-util-helpers-single-response
AWS: recorded behavior plus AppSync's resolver-utility reference (the VTL envelope is not a Smithy shape). A NONE-data-source UNIT resolver's mapping templates may call the whole documented $util surface in one pass, and each helper has a defined result: $util.str.toReplace("hello world","hello","mellow") -> "mellow world"; $util.math.maxVal(3.5, 7.25) -> 7.25; $util.list.copyAndRetainAll(["red","green","blue"],["green","blue"]) -> the retained list; $util.map.copyAndRetainAllKeys({...},["b","c"]) -> the retained map; $util.dynamodb.toDynamoDB(<string>) -> {"S":"<string>"}; $util.transform.toDynamoDBFilterExpression({"status":{"eq":<v>}}) -> the {"expression","expressionNames","expressionValues"} document; $util.time.nowEpochSeconds() -> the current epoch second as an integer. The GraphQL response is 200 with no `errors` member and every field populated.
Emulator: the first helper encountered aborts the field: 200 with {"data":{"probe":null},"errors":[{"path":["probe"],"message":"No such helper: $util.str.toReplace"}]}. The helper registry carries only a subset of the documented utilities, and an unknown name becomes a field error instead of a value.
Why: mapping templates written against the documented $util reference — the normal way AppSync VTL resolvers are authored — fail at query time on the emulator with a message AWS never emits, so any application or test suite using string/math/list/map utilities cannot run at all.
Fix: the VTL evaluation engine's $util helper registry in the appsync service crate (the mapping-template interpreter used by the GraphQL data plane) — implement the documented $util.str / $util.math / $util.list / $util.map / $util.dynamodb / $util.transform / $util.time helpers.
Related-but-not-covering: 20261116T090637Z-appsync-cloudwatch-cognito-idp-dynamodb-logs-the-vtl.md records the same registry gap for $util.dynamodb.toDynamoDBJson under a different service token and does not carry this check's replay key.
Traces: appsync.unit-resolver-util-helpers-single-response (mismatch: errors[0].message "No such helper: $util.str.toReplace").

Cause 3 of 6: a $util.transform.toDynamoDBFilterExpression failure produces a GraphQL error with no errorType and the wrong message.
Failing check id: appsync.resolver-invalid-filter-expression-operator-rejected
AWS: recorded behavior plus AppSync's transformation-helper reference. $util.transform.toDynamoDBFilterExpression called with an operator the helper does not define fails only that field at query time: HTTP 200, data.<field> is null, and exactly one entry is appended to the top-level `errors` array for that path carrying errorType == "DynamoDBFilterError" and message == "Error generating DynamoDB filter expression from input - {<the rejected filter object as rendered>}" — the fixed text is "Error generating DynamoDB filter expression from input - ", the variable part is the filter object built from the request's arguments (in this check's shape {status={bogusOperator=<the token argument>}}). A sibling field on the same API whose template uses a documented operator still resolves and returns the rendered filter document.
Emulator: one error entry is produced for the field, but it has no `errorType` member at all (indexing it raises KeyError: 'errorType'), so the failure is untyped and the documented message/typing contract for filter-transform failures cannot be observed.
Why: clients branch on errorType to tell a filter/transform failure apart from an authorization denial or a resolver-authored error; with the member absent, every such failure collapses into one untyped bucket and code reading error.errorType throws on the emulator where it works against AWS.
Fix: the GraphQL data plane's error-envelope serializer for VTL mapping-template failures in the appsync service crate — classify a transform-helper failure as DynamoDBFilterError and emit errorType alongside message/path.
Related-but-not-covering: 20261116T135132Z-appsync-2-net-new-causes-a-graphql-error.md covers the missing errorType for an APPSYNC_JS util.error abort; this is the VTL transform-helper path and that issue does not carry this check's replay key.
Traces: appsync.resolver-invalid-filter-expression-operator-rejected (error: KeyError: 'errorType' at `assert error["errorType"] == "DynamoDBFilterError"`).

Cause 4 of 6: the CreateDataSource EventBridge missing-config refusal message is emulator-authored.
Failing check id: appsync.create-data-source-eventbridge-missing-config-rejected
AWS: Smithy: CreateDataSource.type=AMAZON_EVENTBRIDGE requires eventBridgeConfig (EventBridgeDataSourceConfig) and the operation models BadRequestException(400). Omitting the config is refused with HTTP 400 BadRequestException and the fixed message "Missing EventBridge data source config" — fixed text, no request-derived parts and no trailing period.
Emulator: the status and code are right but the message reads "Missing Amazon EventBridge data source config" — an extra "Amazon " the service does not emit.
Why: refusal messages are part of the contract clients log, match and assert on; a reworded message breaks message-matching error handling and makes emulator transcripts non-comparable with AWS ones.
Fix: the CreateDataSource handler's type/config consistency validation in the appsync service crate — emit AWS's exact wording.
Traces: appsync.create-data-source-eventbridge-missing-config-rejected (mismatch: got 'Missing Amazon EventBridge data source config').

Cause 5 of 6: the duplicate-data-source refusal message carries a trailing period AWS does not emit.
Failing check id: appsync.data-source-name-conflict-scoped-to-api
AWS: Smithy: CreateDataSource models BadRequestException(400); a second data source with a name already present on the SAME api is refused with HTTP 400 BadRequestException and the message "Data source with name <name> already exists" — the fixed text is "Data source with name " + " already exists" with NO trailing period, and the variable part is the requested data-source name. The conflict is scoped per api: the same name on another api still succeeds.
Emulator: same status and code, but the message is "Data source with name <name> already exists." — one extra '.'.
Why: same as Cause 4 — exact refusal text is a requirement; a message that differs by punctuation defeats message assertions and log comparison.
Fix: the CreateDataSource handler's name-conflict refusal in the appsync service crate.
Traces: appsync.data-source-name-conflict-scoped-to-api (mismatch: expected '… already exists', got '… already exists.').

Cause 6 of 6: the ListGraphqlApis maxResults refusal message is emulator-authored.
Failing check id: appsync.list-graphql-apis-max-results-above-limit-rejected
AWS: Smithy: ListGraphqlApis.maxResults targets MaxResults with @range min 0 max 25 (httpQuery maxResults) and the operation models BadRequestException(400). A maxResults above the ceiling is refused with HTTP 400 BadRequestException and the fixed message "maxResults must be greater than 0 and less than or equal to 25." (fixed text including the trailing period; the ceiling 25 is the only value in it), while maxResults=25 itself succeeds.
Emulator: same status and code, but the message is "Invalid maxResults given. Must fall within range 0 to 25" — different sentence, different bound phrasing, no trailing period.
Why: same as Causes 4 and 5 — the message is part of the refusal contract clients match on.
Fix: the ListGraphqlApis handler's maxResults validation in the appsync service crate (and the same ceiling message used by the sibling paginated list operations).
Traces: appsync.list-graphql-apis-max-results-above-limit-rejected (mismatch: got 'Invalid maxResults given. Must fall within range 0 to 25').

## Evidence
Cause 1 — appsync.http-data-source-resolver-forwards-backend-status-and-body
Expected (AWS): a UNIT resolver over a type=HTTP data source whose request template is the documented HTTP request object returns 200 with no `errors` and data.identityCall == {statusCode: <the backend's status>, body: <the backend's body>}, the non-2xx variant forwarding the backend's error status/body just the same.
Actual (emulator): 200 with {'data': {'identityCall': None}, 'errors': [{'path': ['identityCall'], 'message': 'This endpoint does not call the HTTP data source sim2real_ds_2c1a990490f3.'}]}.
appsync.http-data-source-sigv4-signing-toggles-backend-authorization: no verdict at all — "Check timed out after 60.0s" while polling for the signed call to succeed (AWS: signedCall -> backend 200, unsignedCall -> backend 403).

Cause 2 — appsync.unit-resolver-util-helpers-single-response
Expected (AWS): 200, no `errors`, and data.probe == {str: 'mellow world', math: 7.25, list: '["green","blue"]', map: '{"b":2,"c":3}', dynamodb: '{"S":"<token>"}', filter: <the JSON-encoded filter document>, epoch: <current epoch seconds>}.
Actual (emulator): 200 with {'data': {'probe': None}, 'errors': [{'path': ['probe'], 'message': 'No such helper: $util.str.toReplace'}]}.

Cause 3 — appsync.resolver-invalid-filter-expression-operator-rejected
Expected (AWS): 200; data.badFilter is null; exactly one errors entry for path ['badFilter'] with errorType == 'DynamoDBFilterError' and message == 'Error generating DynamoDB filter expression from input - {status={bogusOperator=<token>}}'; the sibling goodFilter field still returns the rendered '(contains(#status,:status_contains))' filter document.
Actual (emulator): an errors entry for ['badFilter'] exists but has no 'errorType' member — the check fails with KeyError: 'errorType' at `assert error["errorType"] == "DynamoDBFilterError"`.

Cause 4 — appsync.create-data-source-eventbridge-missing-config-rejected
Expected (AWS): 400 BadRequestException, Message exactly "Missing EventBridge data source config".
Actual (emulator): 400 BadRequestException, Message 'Missing Amazon EventBridge data source config'.

Cause 5 — appsync.data-source-name-conflict-scoped-to-api
Expected (AWS): ('BadRequestException', 'Data source with name <name> already exists').
Actual (emulator): ('BadRequestException', 'Data source with name <name> already exists.').

Cause 6 — appsync.list-graphql-apis-max-results-above-limit-rejected
Expected (AWS): ListGraphqlApis(maxResults=26) -> 400 BadRequestException, Message "maxResults must be greater than 0 and less than or equal to 25."; ListGraphqlApis(maxResults=25) -> 200.
Actual (emulator): 400 BadRequestException, Message 'Invalid maxResults given. Must fall within range 0 to 25'.

## Steps to Reproduce
Cause 1:
1. CreateGraphqlApi(API_KEY) + CreateApiKey; StartSchemaCreation with a schema exposing `identityCall: BackendResponse` (type BackendResponse { statusCode: Int body: String }) and poll to SUCCESS.
2. CreateDataSource(apiId, name=<ds>, type='HTTP', httpConfig={endpoint: <a reachable backend>}) — and for the signing variant also authorizationConfig={authorizationType: 'AWS_IAM', awsIamConfig:{signingRegion, signingServiceName}} with a service role.
3. CreateResolver(typeName=Query, fieldName=identityCall, dataSourceName=<ds>, kind=UNIT, requestMappingTemplate='{"version":"2018-05-29","method":"GET","resourcePath":"/","params":{"query":{...}}}', responseMappingTemplate='{"statusCode": $ctx.result.statusCode, "body": $util.toJson($ctx.result.body)}').
4. POST { identityCall { statusCode body } } to uris.GRAPHQL with x-api-key — AWS returns the backend's status/body; the emulator returns data null plus "This endpoint does not call the HTTP data source <ds>."

Cause 2:
1. CreateGraphqlApi(API_KEY) + CreateApiKey; StartSchemaCreation with `type Query { probe(token: String!): Probe }` / `type Probe { str: String math: Float list: AWSJSON map: AWSJSON dynamodb: AWSJSON filter: AWSJSON epoch: Int }`.
2. CreateDataSource(type=NONE); CreateResolver(Query.probe, kind=UNIT) with requestMappingTemplate '{"version":"2018-05-29","payload":{}}' and a response template calling $util.str.toReplace, $util.math.maxVal, $util.list.copyAndRetainAll, $util.map.copyAndRetainAllKeys, $util.dynamodb.toDynamoDB, $util.transform.toDynamoDBFilterExpression and $util.time.nowEpochSeconds.
3. POST `query Probe($token: String!) { probe(token: $token) { str math list map dynamodb filter epoch } }` — the emulator answers "No such helper: $util.str.toReplace".

Cause 3:
1. Same API/schema setup with `badFilter(token: String!): AWSJSON` and `goodFilter(token: String!): AWSJSON`.
2. CreateResolver for badFilter with responseMappingTemplate '$util.toJson($util.transform.toDynamoDBFilterExpression({"status":{"bogusOperator":$context.arguments.token}}))' and for goodFilter with the same using "contains".
3. POST a query selecting both fields and inspect errors[path=['badFilter']] — AWS carries errorType 'DynamoDBFilterError' and the "Error generating DynamoDB filter expression from input - …" message; the emulator's entry has no errorType.

Cause 4:
1. CreateGraphqlApi(API_KEY). 2. CreateDataSource(apiId, name=<ds>, type='AMAZON_EVENTBRIDGE') with NO eventBridgeConfig -> expect 400 "Missing EventBridge data source config"; the emulator says "Missing Amazon EventBridge data source config".

Cause 5:
1. CreateGraphqlApi(API_KEY) twice. 2. CreateDataSource(api A, name=<ds>, type=NONE) -> 200. 3. CreateDataSource(api A, name=<ds>, type=NONE) again -> expect 400 "Data source with name <ds> already exists" (no period); the emulator appends '.'. 4. CreateDataSource(api B, name=<ds>) still succeeds.

Cause 6:
1. ListGraphqlApis(maxResults=26) -> expect 400 BadRequestException "maxResults must be greater than 0 and less than or equal to 25."; the emulator returns 'Invalid maxResults given. Must fall within range 0 to 25'. 2. ListGraphqlApis(maxResults=25) -> 200.
