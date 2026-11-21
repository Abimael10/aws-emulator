Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync__cloudwatch__cognito-idp__dynamodb__logs.cdk-authenticated-graphql-app-workflow
Issue-Key: appsync__cloudwatch__cognito-idp__dynamodb__logs.cdk-authenticated-graphql-app-workflow|_assert_authenticated_mutation_persists#7|AssertionError|assert not doc.get("errors"), f"the mutation reported errors: {_one_line(doc.get('errors'), 300)}"
Issue-Commit: a1915270e40223bfb23a5a7de86fe74aa15a3df0

# appsync__cloudwatch__cognito-idp__dynamodb__logs: the VTL engine has no $util.dynamodb.toDynamoDBJson helper, so a CDK-deployed authenticated GraphQL mutation answers 200 with a resolver error and persists nothing

> _Note: this issue was identified against the code base at commit `a1915270e40223bfb23a5a7de86fe74aa15a3df0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateResolver

## Description
One net-new root cause.

Cause 1 of 1: the AppSync GraphQL data plane now runs VTL mapping templates, but its `$util` object does not expose the `$util.dynamodb.*` helper namespace, so every mapping template that builds a DynamoDB request with `$util.dynamodb.toDynamoDBJson(...)` fails at evaluation time. Failing check id: appsync__cloudwatch__cognito-idp__dynamodb__logs.cdk-authenticated-graphql-app-workflow.

AWS: not modeled in Smithy — the GraphQL data plane and the VTL utility surface are not API operations, so this is AWS-documented/recorded behavior only. AWS's AppSync resolver mapping-template utility reference defines `$util.dynamodb.toDynamoDBJson(Object)` (together with the rest of the `$util.dynamodb` namespace: `toDynamoDB`, `toString/toStringJson`, `toNumber/toNumberJson`, `toList/toListJson`, `toMap/toMapJson`, `toDynamoDBExtended`...) as a built-in that renders a value into its DynamoDB AttributeValue JSON form. A resolver request template using it is accepted by CreateResolver/the CFN AWS::AppSync::Resolver it is deployed from, and at request time the template renders, the DynamoDB PutItem runs, and the GraphQL response is HTTP 200 whose body carries `data.<field>` populated from the data source and NO `errors` member at all. The recording for this check shows exactly that: the authenticated `addNote` mutation returns the written note (id/text/owner/label) and the item is then readable through `getNote` and through DynamoDB directly.

Emulator: the same mutation is answered HTTP 200 with `data` unresolved and an `errors` array containing one entry whose `path` is the mutation field and whose `message` is the emulator-authored text `No such helper: $util.dynamodb.toDynamoDBJson`. That wording is entirely the emulator's: AWS emits no error for this template, and no AWS AppSync error uses the fixed text "No such helper: " + the helper reference. Nothing is written to the DynamoDB table, so the whole downstream workflow of the check (structured log, metric datapoint, alarm, drift re-apply, teardown-stops-writes) never gets a persisted item to observe.

Why: a client that deploys the canonical CDK/CloudFormation AppSync+Cognito+DynamoDB app — whose generated resolver templates always marshal arguments with `$util.dynamodb.toDynamoDBJson` — sees every mutation and query "succeed" at the HTTP layer (200) while silently returning null data and writing nothing. Because the failure is reported inside a 200 body rather than as an error status, retry and error-handling logic does not trigger; the application appears deployed and wired but stores no data, and every observability signal derived from the write (CloudWatch metric, log record, alarm) stays empty.

Fix: the AppSync data-plane resolver component — the VTL/Velocity evaluation engine's `$util` binding (the `$util.dynamodb` helper namespace in particular) used when a UNIT/PIPELINE resolver with a DynamoDB data source renders its request mapping template. The helpers must be registered and must emit DynamoDB AttributeValue JSON; a template that resolves them must not produce a GraphQL `errors` entry.

Related open issues that do NOT cover this one: 20261114T160208Z-appsync-cloudwatch-cognito-idp-dynamodb-logs-a-cloudformation.md (same service heading, but its cause is an AWS::Logs::MetricFilter NotFound refusal, not the VTL helper table), and the several "VTL mapping-template resolvers are never executed" issues filed under other service headings (e.g. 20261114T070045Z-appsync-cognito-idp-dynamodb-vtl-mapping-template-resolvers.md, 20261114T043034Z-appsync-dynamodb-events-vtl-mapping-template-resolvers-are.md) — here the template IS executed and fails only on the missing helper, which is a distinct defect in the engine's builtin table.

Traces: appsync__cloudwatch__cognito-idp__dynamodb__logs.cdk-authenticated-graphql-app-workflow, leg "wiring" (_assert_authenticated_mutation_persists).

## Evidence
Check appsync__cloudwatch__cognito-idp__dynamodb__logs.cdk-authenticated-graphql-app-workflow, leg "wiring":

Expected (AWS): the authenticated `mutation Add($id: ID!, $text: String!) { addNote(id:$id, text:$text) { id text owner label } }` returns HTTP 200 with no `errors` member and `data.addNote` populated, and the item is subsequently readable via `getNote`.

Actual (emulator): HTTP 200 with
  errors: [{'path': ['addNote'], 'message': 'No such helper: $util.dynamodb.toDynamoDBJson'}]
and no item persisted, which trips the check's assertion:
  AssertionError: the mutation reported errors: [{'path': ['addNote'], 'message': 'No such helper: $util.dynamodb.toDynamoDBJson'}]

Message anatomy: the whole string is emulator-authored. Its fixed part is "No such helper: "; its variable part is the VTL helper reference taken from the resolver's request mapping template ($util.dynamodb.toDynamoDBJson here, but any $util.dynamodb.* helper will produce the same shape). AWS produces no error message for this template at all.

## Steps to Reproduce
1. Deploy the check's CDK app (appsync__cloudwatch__cognito-idp__dynamodb__logs/deployments/graphql_app_cdk): a Cognito user pool + client, a DynamoDB table, and an AppSync GraphQL API with USER_POOL auth whose `addNote`/`getNote` resolvers are VTL mapping templates over a DynamoDB data source that marshal arguments with $util.dynamodb.toDynamoDBJson.
2. Create and confirm a user in the pool and obtain an id token.
3. POST to the API's GraphQL endpoint with that token:
   mutation Add($id: ID!, $text: String!) { addNote(id: $id, text: $text) { id text owner label } }
   variables {"id": "<any id>", "text": "<any text>"}
4. Observe the 200 response body carries errors[0].message == "No such helper: $util.dynamodb.toDynamoDBJson" and data.addNote is not populated; a DynamoDB GetItem on the table key shows no item was written. On AWS the mutation returns the note and the item exists.
