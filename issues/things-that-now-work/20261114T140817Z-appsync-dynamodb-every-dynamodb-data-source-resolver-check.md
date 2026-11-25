Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync__dynamodb.appsync-batch-get-item-resolver-preserves-arity-and-appends-per-key-errors appsync__dynamodb.appsync-concurrent-conditional-mutations-single-winner appsync__dynamodb.appsync-dynamodb-unit-resolver-round-trips-item appsync__dynamodb.appsync-optimistic-concurrency-conflict-returns-server-version-and-data appsync__dynamodb.appsync-unsigned-request-to-iam-auth-api-rejected appsync__dynamodb.deep-appsync-1-appsync-conflict-automerge-concurrent-stale-writers-all-merge-with-distinct-versions appsync__dynamodb.deep-appsync-1-appsync-conflict-automerge-stale-delete-rejected-and-no-tombstone-written appsync__dynamodb.deep-appsync-1-appsync-conflict-automerge-stale-disjoint-writes-both-land-and-same-scalar-keeps-server-value appsync__dynamodb.deep-appsync-1-appsync-conflict-sync-delta-returns-only-changes-after-lastsync-with-tombstone
Issue-Key: appsync__dynamodb.appsync-batch-get-item-resolver-preserves-arity-and-appends-per-key-errors|_poll_until#18|Timeout|time.sleep(interval_s)
Issue-Key: appsync__dynamodb.appsync-concurrent-conditional-mutations-single-winner|_poll_until#17|Timeout|time.sleep(interval_s)
Issue-Key: appsync__dynamodb.appsync-dynamodb-unit-resolver-round-trips-item|_poll_until#19|Timeout|time.sleep(interval_s)
Issue-Key: appsync__dynamodb.appsync-optimistic-concurrency-conflict-returns-server-version-and-data|_poll_until#23|Timeout|time.sleep(interval_s)
Issue-Key: appsync__dynamodb.appsync-unsigned-request-to-iam-auth-api-rejected|_poll_until#25|Timeout|time.sleep(interval_s)
Issue-Key: appsync__dynamodb.deep-appsync-1-appsync-conflict-automerge-concurrent-stale-writers-all-merge-with-distinct-versions|_poll_until#8|Timeout|time.sleep(interval_s)
Issue-Key: appsync__dynamodb.deep-appsync-1-appsync-conflict-automerge-stale-delete-rejected-and-no-tombstone-written|_poll_until#11|Timeout|time.sleep(interval_s)
Issue-Key: appsync__dynamodb.deep-appsync-1-appsync-conflict-automerge-stale-disjoint-writes-both-land-and-same-scalar-keeps-server-value|_poll_until#16|Timeout|time.sleep(interval_s)
Issue-Key: appsync__dynamodb.deep-appsync-1-appsync-conflict-sync-delta-returns-only-changes-after-lastsync-with-tombstone|_poll_until#15|Timeout|time.sleep(interval_s)
Issue-Commit: 8a8830456e168aad64737c1eda6ee6081a6c4868

# appsync__dynamodb: every DynamoDB-data-source resolver check now hangs — the GraphQL data plane never serves an awaited response, so all 9 unit-resolver / conflict-detection / IAM-auth flows exhaust the 60 s cap

> _Note: this issue was identified against the code base at commit `8a8830456e168aad64737c1eda6ee6081a6c4868`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateResolver

## Description
Cause 1 of 1: with an AMAZON_DYNAMODB data source bound to a resolver, no request to the API's published GraphQL endpoint ever reaches the state AWS would serve; all nine appsync__dynamodb checks in this suite end with "Check timed out after 60.0s" and no response is captured.
AWS: CreateDataSource type=AMAZON_DYNAMODB with dynamodbConfig + serviceRoleArn, plus CreateResolver (Smithy: appsync CreateResolver, POST /v1/apis/{apiId}/types/{typeName}/resolvers; requestMappingTemplate/responseMappingTemplate -> MappingTemplate @length 1..65536 @pattern "^.*$"; code -> Code @length 1..32768 with runtime AppSyncRuntime APPSYNC_JS/1.0.0; syncConfig -> SyncConfig for conflict detection; errors BadRequestException 400, NotFoundException 404, UnauthorizedException 401, ConcurrentModificationException 409, InternalFailureException 500), makes AppSync execute the resolver against DynamoDB on every query/mutation. The data-plane POST is not modeled — recorded behavior only: a mutation returns HTTP 200 with no "errors" member and the written item echoed back, a subsequent query reads it back from the table, a stale OPTIMISTIC_CONCURRENCY/AUTOMERGE write returns the typed conflict error plus the server version and data, BatchGetItem resolvers preserve request arity and append one error per missing key, and an unsigned request to an AWS_IAM api is refused 401 UnauthorizedException.
Emulator: none of these ever completes. Every one of the nine checks is cut off by the harness at its 60 s budget with no observable status, error code or body, so the divergence is "the awaited response is never served" rather than a wrong value. This is the same family as the already-filed appsync-token data-plane failures (resolver dispatch and the VTL/APPSYNC_JS runtime), and the sibling issues that own the endpoint-publication and realtime-upgrade halves of this service token are 20261110T155738Z-appsync-dynamodb-creategraphqlapi-publishes-a-real-appsync-api.md, 20261111T065707Z-appsync-dynamodb-2-net-new-causes-the-graphql.md and 20261112T152034Z-appsync-dynamodb-the-realtime-endpoint-of-an-aws.md — none of them carries these nine replay keys.
Why: AppSync-over-DynamoDB is the single most common AppSync topology. A client cannot write or read an item, cannot observe conflict detection/sync deltas, and cannot verify that an unsigned call to an AWS_IAM api is refused — the request simply never finishes, which no AWS behaviour ever does.
Fix: the GraphQL data-plane executor and its AMAZON_DYNAMODB data-source adapter (resolver dispatch, the VTL/APPSYNC_JS runtime, the conflict-detection/sync path and the endpoint's auth gate) must answer each request, bounded, with the AWS-shaped envelope; re-replay all nine checks listed in trace_ids after the appsync-token resolver-execution causes are fixed, since they share that root.
Traces: the nine appsync__dynamodb checks listed under Evidence.

## Evidence
All nine failing checks report exactly the same detail, with no response recorded:
  Error: Check timed out after 60.0s

[mismatch] appsync__dynamodb.appsync-dynamodb-unit-resolver-round-trips-item — expected (AWS): 200, no "errors", the item written by the mutation read back by the query.
[mismatch] appsync__dynamodb.appsync-unit-resolver-reads-dynamodb-via-api-key is OWNED elsewhere and excluded; the nine below are unowned.
[mismatch] appsync__dynamodb.appsync-batch-get-item-resolver-preserves-arity-and-appends-per-key-errors — expected: result list of the same length as the requested keys, one appended error per absent key.
[mismatch] appsync__dynamodb.appsync-concurrent-conditional-mutations-single-winner — expected: exactly one conditional mutation succeeds, the losers carry DynamoDB:ConditionalCheckFailedException.
[mismatch] appsync__dynamodb.appsync-optimistic-concurrency-conflict-returns-server-version-and-data — expected: the conflict error with the server's _version and the server item in data.
[mismatch] appsync__dynamodb.appsync-unsigned-request-to-iam-auth-api-rejected — expected: 401 UnauthorizedException for an unsigned POST to an AWS_IAM api.
[mismatch] appsync__dynamodb.deep-appsync-1-appsync-conflict-automerge-concurrent-stale-writers-all-merge-with-distinct-versions
[mismatch] appsync__dynamodb.deep-appsync-1-appsync-conflict-automerge-stale-delete-rejected-and-no-tombstone-written
[mismatch] appsync__dynamodb.deep-appsync-1-appsync-conflict-automerge-stale-disjoint-writes-both-land-and-same-scalar-keeps-server-value
[mismatch] appsync__dynamodb.deep-appsync-1-appsync-conflict-sync-delta-returns-only-changes-after-lastsync-with-tombstone

Smithy cited: appsync CreateResolver (POST /v1/apis/{apiId}/types/{typeName}/resolvers; requestMappingTemplate/responseMappingTemplate, code/runtime, syncConfig, maxBatchSize @range 0..2000; errors BadRequestException 400 / NotFoundException 404 / UnauthorizedException 401 / ConcurrentModificationException 409 / InternalFailureException 500); CreateDataSource type AMAZON_DYNAMODB with dynamodbConfig + serviceRoleArn; CreateGraphqlApi.authenticationType AWS_IAM. The GraphQL POST, its {data,errors} envelope and the conflict-detection error payloads are not modeled — recorded behavior only.

## Steps to Reproduce
1. dynamodb.create_table with a simple hash key (plus a GSI for the query variants); iam.create_role trusting appsync.amazonaws.com with dynamodb:* on the table.
2. api = appsync.create_graphql_api(name=..., authenticationType="API_KEY" — or "AWS_IAM" for the unsigned-request check); create_api_key; start_schema_creation with a type exposing putItem/getItem (and _version for the conflict variants); wait for SUCCESS.
3. appsync.create_data_source(apiId, name="ddb", type="AMAZON_DYNAMODB", serviceRoleArn=<role>, dynamodbConfig={awsRegion, tableName}); appsync.create_resolver for the mutation and the query (VTL templates or APPSYNC_JS code; add syncConfig={conflictHandler:"AUTOMERGE"|"OPTIMISTIC_CONCURRENCY", conflictDetection:"VERSION"} for the conflict checks).
4. POST the mutation, then the query, to api["uris"]["GRAPHQL"] with x-api-key (or SigV4-signed / deliberately unsigned for the AWS_IAM api).
   Observed: the request never produces the awaited response; each check is cut off at its 60 s budget.
   Expected: 200 with the item written/read back, the typed conflict error with server version+data for a stale write, per-key errors with preserved arity for BatchGetItem, and 401 UnauthorizedException for the unsigned call.
