Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync__dynamodb.appsync-iam-signed-mutation-reaches-realtime-subscriber-and-query-reads-it-back
Issue-Key: appsync__dynamodb.appsync-iam-signed-mutation-reaches-realtime-subscriber-and-query-reads-it-back|_poll_until#15|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Commit: 5b88798ccff0ec7f765809a75ef2c7dfb1d7aad0

# appsync__dynamodb: a SigV4-signed createNote mutation on an AWS_IAM GraphQL API never resolves, so the realtime-subscriber + read-back flow exhausts the run budget

> _Note: this issue was identified against the code base at commit `5b88798ccff0ec7f765809a75ef2c7dfb1d7aad0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateResolver

## Description
Cause 1 of 1: an APPSYNC_JS unit resolver over an AMAZON_DYNAMODB data source, invoked by a SigV4-signed mutation on an AWS_IAM-authenticated GraphQL API, never produces the awaited response — the harness polls the signed createNote mutation until the run budget ends and raises TimeoutError, so neither the realtime delivery of the mutation to the subscriber nor the query read-back is ever graded. Failing check: appsync__dynamodb.appsync-iam-signed-mutation-reaches-realtime-subscriber-and-query-reads-it-back.

AWS: CreateDataSource type=AMAZON_DYNAMODB (dynamodbConfig + serviceRoleArn) plus CreateResolver (Smithy: appsync CreateResolver, POST /v1/apis/{apiId}/types/{typeName}/resolvers; code -> Code @length 1..32768 with runtime AppSyncRuntime name APPSYNC_JS version 1.0.0; errors BadRequestException 400, NotFoundException 404, UnauthorizedException 401, ConcurrentModificationException 409, InternalFailureException 500) makes AppSync execute the resolver on every operation. The GraphQL POST and the realtime protocol are not modeled — recorded behavior only: a SigV4-signed mutation on an authenticationType=AWS_IAM api returns HTTP 200 with a data member and no "errors", the resolver's ddb.put lands the item (here with author taken from ctx.identity.accountId, which only an IAM-authorized request carries), the mutation is pushed to every matching @aws_subscribe subscriber over the realtime endpoint, and a following signed query reads the same item back. Every one of those answers is bounded.

Emulator: the signed mutation never resolves. The harness's poll for "the signed createNote mutation to resolve" runs out: TimeoutError: the signed createNote mutation to resolve: the run budget ended first; not graded (check_...py line 116, from line 386). No status, error code or GraphQL envelope is captured, so the divergence is "the awaited response is never served".

Why: AppSync-over-DynamoDB under AWS_IAM is a mainstream topology. A signed client cannot write or read an item, cannot observe that ctx.identity was populated from the signature, and no subscriber ever receives the mutation — the call simply never finishes, which no AWS behaviour does.

Fix: the GraphQL data-plane executor and its AMAZON_DYNAMODB data-source adapter (resolver dispatch, the APPSYNC_JS runtime's ctx.identity binding for SigV4 callers, the realtime push to @aws_subscribe subscribers) must answer each request, bounded, with the AWS-shaped {data, errors} envelope.

Traces: appsync__dynamodb.appsync-iam-signed-mutation-reaches-realtime-subscriber-and-query-reads-it-back. Same family as the open issue 20261114T140817Z-appsync-dynamodb-every-dynamodb-data-source-resolver-check.md, which lists nine other appsync__dynamodb checks but does NOT carry this check's replay key; 20261112T152034Z (realtime upgrade) and 20261122T231454Z (published uris) own other halves of this service token and likewise do not cover it.

## Evidence
Check: appsync__dynamodb.appsync-iam-signed-mutation-reaches-realtime-subscriber-and-query-reads-it-back
Verdict: error —
  File "check_appsync__dynamodb_appsync_iam_signed_mutation_reaches_realtime_subscriber_and_query_reads_it_back.py", line 386, in run: status, created = _poll_until(...)
  File ... line 116, in _poll_until: raise TimeoutError(f"{description}: the run budget ended first; not graded")
  TimeoutError: the signed createNote mutation to resolve: the run budget ended first; not graded

Expected (AWS): the SigV4-signed POST of
  mutation CreateNote($id: ID!, $board: String!, $body: String!) { createNote(id:$id, board:$board, body:$body) { id board body author } }
to the api's GRAPHQL uri returns HTTP 200 with data.createNote populated and no "errors" member; the APPSYNC_JS request handler ddb.put({key:{id}, item:{board, body, author: ctx.identity.accountId}}) writes the item (author non-null, proving the signature was evaluated); the matching onCreateNote subscription receives the same object over the realtime endpoint; the signed GetNote query reads it back with consistentRead.
Actual (emulator): the mutation never resolves within the run budget — no HTTP status, no error code, no envelope.

## Steps to Reproduce
1. dynamodb.create_table with hash key id (S); iam.create_role trusting appsync.amazonaws.com with dynamodb access to that table.
2. appsync.create_graphql_api(name=..., authenticationType="AWS_IAM"); start_schema_creation with a schema exposing Query.getNote, Mutation.createNote and Subscription.onCreateNote(board) @aws_subscribe(mutations:["createNote"]); wait for SUCCESS.
3. appsync.create_data_source(apiId, name=..., type="AMAZON_DYNAMODB", serviceRoleArn=<role>, dynamodbConfig={awsRegion, tableName}).
4. appsync.create_resolver for Mutation.createNote and Query.getNote with runtime {name:"APPSYNC_JS", runtimeVersion:"1.0.0"} and code using ddb.put / ddb.get, the request handler storing author: ctx.identity.accountId.
5. Open a SigV4-signed realtime connection and start the onCreateNote subscription, then POST the SigV4-signed createNote mutation to api["uris"]["GRAPHQL"].
   Observed: the mutation never resolves; the poll exhausts the run budget (TimeoutError, not graded).
   Expected: 200 with data.createNote (author = the signer's account), the same object pushed to the subscriber, and the signed getNote query reading it back.
