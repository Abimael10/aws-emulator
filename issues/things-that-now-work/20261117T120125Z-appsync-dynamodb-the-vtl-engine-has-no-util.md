Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync__dynamodb.appsync-ddb-putitem-resolver-full-replacement-drops-omitted-attribute appsync__dynamodb.appsync-ddb-query-resolver-limit-paginates-in-sort-key-order appsync__dynamodb.appsync-ddb-query-resolver-malformed-next-token-rejected
Issue-Key: appsync__dynamodb.appsync-ddb-putitem-resolver-full-replacement-drops-omitted-attribute|run#12|AssertionError|assert first is not None and not first.get("errors"), f"first putItem failed: {first}"
Issue-Key: appsync__dynamodb.appsync-ddb-query-resolver-limit-paginates-in-sort-key-order|run#14|AssertionError|assert page1 is not None and not page1.get("errors"), f"first page query failed: {page1}"
Issue-Key: appsync__dynamodb.appsync-ddb-query-resolver-malformed-next-token-rejected|run#13|AssertionError|assert warmup is not None and not warmup.get("errors"), f"warm-up query never succeeded: {warmup}"
Issue-Commit: 316bc8d4d27618b04e0a3739e3ce26f595ac855b

# appsync__dynamodb: the VTL engine has no $util.dynamodb.toDynamoDB / toDynamoDBJson helpers, so every DynamoDB mapping-template resolver fails the field with "No such helper"

> _Note: this issue was identified against the code base at commit `316bc8d4d27618b04e0a3739e3ce26f595ac855b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateResolver

## Description
One net-new root cause, grounded in three failing behavioral checks that are neither ALREADY FILED nor OWNED by an open issue.

Cause 1 of 1: the GraphQL data plane's VTL evaluator does not implement the $util.dynamodb.* marshalling helpers, so a DynamoDB-data-source UNIT resolver written with the documented mapping templates aborts the field instead of issuing the PutItem/Query.
Failing check ids: appsync__dynamodb.appsync-ddb-putitem-resolver-full-replacement-drops-omitted-attribute, appsync__dynamodb.appsync-ddb-query-resolver-limit-paginates-in-sort-key-order, appsync__dynamodb.appsync-ddb-query-resolver-malformed-next-token-rejected
AWS: recorded behavior plus AppSync's resolver-utility reference (the VTL/GraphQL envelope is not a Smithy shape; Smithy: CreateDataSource.type=AMAZON_DYNAMODB with dynamodbConfig and CreateResolver's requestMappingTemplate/responseMappingTemplate MappingTemplate @length 1..65536 are the modeled surface). The documented DynamoDB mapping templates are authored with the marshalling helpers: $util.dynamodb.toDynamoDB(<value>) renders a single attribute value ({"S":"…"}, {"N":…}, …) and $util.dynamodb.toDynamoDBJson(<value>) renders the same already JSON-serialized for inlining into the template body. With them, a PutItem template fully replaces the item (attributes omitted from the template are gone from the stored item), and a Query template with "limit" and "nextToken" returns items in sort-key order with an opaque nextToken that pages the rest, while a malformed nextToken is refused at query time. The GraphQL response for all of these is HTTP 200 with no `errors` member and the field populated.
Emulator: the first marshalling helper in the template aborts the field. The 200 body is {"data":{"<field>":null},"errors":[{"path":["<field>"],"message":"No such helper: $util.dynamodb.toDynamoDB"}]} for the PutItem resolver and "No such helper: $util.dynamodb.toDynamoDBJson" for both Query resolvers — the helper registry is missing these names and an unknown helper becomes a field error rather than a rendered value, so no DynamoDB request is ever issued and none of the overwrite / pagination / malformed-token behaviour can be observed.
Why: this is how AppSync DynamoDB resolvers are written in the documentation, in CDK's generated templates and in Amplify's; on the emulator every such field returns null plus a message AWS never emits, so the whole DynamoDB-backed application surface (writes, paginated reads, and the refusal of a bad pagination token) is unusable.
Fix: the VTL evaluation engine's $util helper registry in the appsync service crate (the mapping-template interpreter the GraphQL data plane uses for DynamoDB data sources) — implement $util.dynamodb.toDynamoDB / toDynamoDBJson (and the sibling toString/toNumber/toList/toMap/toDynamoDBFilterExpression forms) so the rendered request reaches the DynamoDB data source.
Related-but-not-covering: 20261116T090637Z-appsync-cloudwatch-cognito-idp-dynamodb-logs-the-vtl.md records the same missing toDynamoDBJson helper under the appsync__cloudwatch__cognito-idp__dynamodb__logs service token and does not carry these checks' replay keys.
Traces: all three check ids above.

## Evidence
Expected (AWS): with an AMAZON_DYNAMODB data source and the documented templates,
- PutItem resolver: `mutation { putItem(...) }` returns 200 with no `errors` and the written item; a second putItem omitting an attribute fully replaces the item (the omitted attribute is absent on a subsequent read).
- Query resolver with limit/nextToken: `query { listItems(...) }` returns 200 with no `errors`, items in sort-key order and an opaque nextToken that pages the remainder.
- Query resolver given a malformed nextToken: the field is refused at query time (the warm-up query first succeeding).
Actual (emulator), all three: HTTP 200 with the field null and a single error entry naming a missing helper —
- {'data': {'putItem': None}, 'errors': [{'path': ['putItem'], 'message': 'No such helper: $util.dynamodb.toDynamoDB'}]}
- {'data': {'listItems': None}, 'errors': [{'path': ['listItems'], 'message': 'No such helper: $util.dynamodb.toDynamoDBJson'}]} (both query checks).

## Steps to Reproduce
1. dynamodb.CreateTable with a partition+sort key; iam.CreateRole trusting appsync.amazonaws.com with access to it.
2. appsync.CreateGraphqlApi(API_KEY) + CreateApiKey; StartSchemaCreation with the Item/Query/Mutation schema and poll to SUCCESS.
3. CreateDataSource(apiId, name=<ds>, type='AMAZON_DYNAMODB', dynamodbConfig={tableName, awsRegion}, serviceRoleArn=<role>).
4. CreateResolver(Mutation.putItem, kind=UNIT, dataSourceName=<ds>) with a PutItem requestMappingTemplate using $util.dynamodb.toDynamoDB(...) for the key/attributes, and CreateResolver(Query.listItems, kind=UNIT) with a Query requestMappingTemplate using $util.dynamodb.toDynamoDBJson(...) plus "limit" and "nextToken".
5. POST the mutation (and then the paginated query) to uris.GRAPHQL with x-api-key — AWS returns the item / the ordered page; the emulator returns data null and errors[0].message "No such helper: $util.dynamodb.toDynamoDB" (resp. toDynamoDBJson).
