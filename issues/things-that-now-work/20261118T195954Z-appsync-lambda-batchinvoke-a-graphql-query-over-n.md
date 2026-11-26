Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync__lambda.appsync-batch-invoke-merges-sibling-fields-into-one-invocation
Issue-Key: appsync__lambda.appsync-batch-invoke-merges-sibling-fields-into-one-invocation|_post_json#21|Timeout|return None, {"transport_error": str(exc)}
Issue-Commit: 4d1b2267f80e5aa417961aede209ea6b978242d4

# AppSync Lambda BatchInvoke: a GraphQL query over N sibling fields never settles — the batched-resolver check never reaches a verdict and the whole check times out

> _Note: this issue was identified against the code base at commit `4d1b2267f80e5aa417961aede209ea6b978242d4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
BatchInvoke

## Description
Cause 1 of 1: a GraphQL request whose N sibling fields are served by a Lambda data source in BatchInvoke mode never reaches the state AWS reaches — the check's first barrier (one HTTP 200 GraphQL response, no `errors`, N parent objects each with a non-null batched child field) is never observed, so the replay is cut off by the check budget with `Check timed out after 60.0s` and no assertion is ever evaluated. Failing check: appsync__lambda.appsync-batch-invoke-merges-sibling-fields-into-one-invocation.

AWS: AppSync serves a GraphQL POST on the API's GRAPHQL URI and returns HTTP 200 with a body of {"data": ...} (and an "errors" array only when a field errored). Smithy (services/service-com-amazonaws-appsync-awsdeepdishcontrolplaneservice/smithy-model/com-amazonaws-appsync-awsdeepdishcontrolplaneservice.json): CreateResolver POST /v1/apis/{apiId}/types/{typeName}/resolvers models `maxBatchSize` (MaxBatchSize, default 0, range 0..2000) and `runtime`/`code` for the APPSYNC_JS runtime, and returns the created `Resolver` echoing `maxBatchSize`; errors BadRequestException(400), NotFoundException(404), UnauthorizedException(401), ConcurrentModificationException(409), InternalFailureException(500). The resolver-level behavior the check exercises is documented, not modeled, so for that part this is recorded/documented behavior only: with `operation: 'BatchInvoke'` and maxBatchSize >= N, AppSync merges the N sibling resolutions of one field into exactly ONE Lambda invocation whose event is a BARE JSON list of the N merged payloads; the function answers with a list of N elements in request order using the per-element envelope {"data": ..., "errorMessage": ..., "errorType": ...}; AppSync maps element i onto sibling i. A parent list field served by a NONE data source with an APPSYNC_JS resolver calling `runtime.earlyReturn(<value>)` returns that value for the field without any backend call. All of this completes in one request/response: AWS answers the GraphQL POST with 200 and populated data, it does not leave the field unresolved.

Emulator: the replay never gets a GraphQL response satisfying `status == 200 and not body['errors'] and len(data.posts) == N and every post.detail is non-null`. The check polls that condition and is still polling when the run budget ends, so the harness reports verdict `mismatch` with detail `Check timed out after 60.0s`; no status/error-code/message is produced on this path to quote, because the condition is never reached rather than answered wrongly. The same suite shows the sibling check appsync__lambda.appsync-batch-invoke-partial-item-error-isolates-the-failing-sibling getting deep into its body (it fails on an assertion, not a timeout), so the shared setup — queue, roles, python3.12 function reaching State=Active, CreateGraphqlApi + StartSchemaCreation reaching SUCCESS/ACTIVE, two CreateDataSource calls, two CreateResolver calls, CreateResolver echoing maxBatchSize — does complete; what does not complete is this scenario's own GraphQL resolve. Hypothesis: the APPSYNC_JS parent resolver on the NONE data source does not honour `runtime.earlyReturn`, so `Query.posts` never produces the N parents and the batched child field is never reached; alternatively the BatchInvoke merge of the N sibling resolutions never dispatches the single Lambda invocation. Either way the field stays unresolved indefinitely instead of being answered.

Why: a client relying on AWS's behavior gets a request that never completes instead of a 200 with data. Any GraphQL query over a list whose child field is batched (the standard N+1 avoidance pattern AppSync documents for Lambda data sources) hangs rather than returning either data or a GraphQL error, so callers cannot distinguish a slow backend from a resolver that will never answer, and no amount of client-side retry or error handling recovers. It also makes the batching contract itself (exactly one invocation, a bare list event, result i onto sibling i) unverifiable.

Fix: the AppSync GraphQL data-plane executor — the APPSYNC_JS resolver runtime (`runtime.earlyReturn` on a NONE data source, which must terminate the request step and return the value as the field's result) and the Lambda data source's BatchInvoke dispatch path that must collect the N sibling resolutions of one field into a single invocation and complete the GraphQL response. The same component is named by open issue 20261118T165433Z-appsync-lambda-batchinvoke-one-element-s-per-item-....md, which covers per-element error isolation in the BatchInvoke RESPONSE mapping; that issue is bound only to the partial-item-error check's replay key and does not cover this check, so this cause carries its own.

Traces: appsync__lambda.appsync-batch-invoke-merges-sibling-fields-into-one-invocation.

## Evidence
Behavioral suite replay against the emulator built from this checkout; service heading `appsync__lambda` 8/10 (80%), 2 failed.

[mismatch] appsync__lambda.appsync-batch-invoke-merges-sibling-fields-into-one-invocation
  Error: Check timed out after 60.0s

Expected (AWS): the GraphQL POST `query Batched($ids: [ID!]!) { posts(ids: $ids) { id detail { forId batchLen } } }` with N ids returns HTTP 200, no `errors` key, `data.posts` of length N in the order of the `ids` argument, and each `posts[i].detail` non-null with `forId == ids[i]` and `batchLen == N`; the Lambda is invoked exactly ONCE for that request with an event that is a bare JSON list of N payloads. Fixed text of the requirement: status 200, no GraphQL `errors`, N results in request order, one invocation, a list-shaped event. Per-run values that merely fill the template: the generated api id, the GRAPHQL URI host and port, the API key id, the role/function/queue names, the generated post ids, the Lambda aws_request_id.

Actual (emulator): the poll on exactly that condition never succeeds; the run budget ends first and the harness records `Check timed out after 60.0s`. No HTTP status, error code or error message is emitted on this path for the resolve — the request is never answered in the asserted state, so there is no diverging status/code/message to quote. The preceding control-plane calls in the same check succeed (CreateResolver returns a `resolver` whose `maxBatchSize` echoes the requested value, which the check asserts before reaching the barrier), confirming the divergence is in the GraphQL data-plane executor rather than in resolver creation.

## Steps to Reproduce
1. Clone the repo and build/boot the emulator from this branch, then replay the behavioral suite and re-run exactly the check id in trace_ids: appsync__lambda.appsync-batch-invoke-merges-sibling-fields-into-one-invocation.

Manually:
1. Create an SQS queue as a side channel and an execution role trusting lambda.amazonaws.com with AWSLambdaBasicExecutionRole plus sqs:SendMessage on that queue.
2. Create a python3.12 function whose handler inspects the event shape (bare list vs mapping with a `payload` key), reports {kind, n, forIds, requestId} to the queue, and returns one {"data": {"forId": <id>, "batchLen": <n>}, "errorMessage": null, "errorType": null} element per item; wait for State=Active.
3. CreateGraphqlApi with authenticationType=API_KEY, StartSchemaCreation with a schema exposing `Query.posts(ids: [ID!]!): [Post]` and `Post.detail: Detail`, poll GetSchemaCreationStatus to SUCCESS/ACTIVE, and CreateApiKey.
4. CreateDataSource type=NONE, and CreateDataSource type=AWS_LAMBDA pointing at the function with a service role granting lambda:InvokeFunction.
5. CreateResolver for Query.posts on the NONE data source, runtime APPSYNC_JS 1.0.0, with a request function calling `runtime.earlyReturn(ctx.args.ids.map((id) => ({ id })))`.
6. CreateResolver for Post.detail on the Lambda data source, runtime APPSYNC_JS 1.0.0, request returning `{ operation: 'BatchInvoke', payload: { forId: ctx.source.id } }`, with maxBatchSize=10.
7. POST `query Batched($ids: [ID!]!) { posts(ids: $ids) { id detail { forId batchLen } } }` with 5 ids and the `x-api-key` header to the API's GRAPHQL URI.
8. Observe that no response satisfying "200, no errors, 5 posts each with a non-null detail" is ever produced, so the request never completes as AWS completes it; expected is one 200 response with all 5 details populated and exactly one queue report of {kind: "list", n: 5}.
