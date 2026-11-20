Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync__sqs.http-datasource-signs-the-sqs-api-and-delivers-the-message appsync__sqs.http-datasource-role-without-sendmessage-denied-then-granted-delivers
Issue-Key: appsync__sqs.http-datasource-signs-the-sqs-api-and-delivers-the-message|_graphql#19|AssertionError|assert not body.get("errors"), f"sendToQueue mutation returned GraphQL errors: {body['errors']!r}"
Issue-Key: appsync__sqs.http-datasource-role-without-sendmessage-denied-then-granted-delivers|_graphql#19|AssertionError|assert not body.get("errors"), f"sendToQueue mutation returned GraphQL errors: {body['errors']!r}"
Issue-Commit: 316bc8d4d27618b04e0a3739e3ce26f595ac855b

# appsync__sqs: an HTTP data source pointed at the SQS API is never called — the mutation returns "This endpoint does not call the HTTP data source <name>." and no message is ever sent

> _Note: this issue was identified against the code base at commit `316bc8d4d27618b04e0a3739e3ce26f595ac855b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDataSource

## Description
One net-new root cause, grounded in two failing behavioral checks that are neither ALREADY FILED nor OWNED by an open issue.

Cause 1 of 1: a resolver bound to a type=HTTP data source whose endpoint is the SQS API is never dispatched, so SigV4 signing of the outbound AWS call and the role-permission transition cannot happen at all.
Failing check ids: appsync__sqs.http-datasource-signs-the-sqs-api-and-delivers-the-message, appsync__sqs.http-datasource-role-without-sendmessage-denied-then-granted-delivers
AWS: Smithy: CreateDataSource.type=HTTP with httpConfig = HttpDataSourceConfig{endpoint, authorizationConfig{authorizationType: AWS_IAM, awsIamConfig{signingRegion, signingServiceName: "sqs"}}} and serviceRoleArn; the data-plane envelope is not a Smithy shape, so the rest is AppSync's documented HTTP-data-source contract plus recorded behavior. AppSync renders the resolver's request mapping template into an HTTP request, signs it SigV4 with the data source's service role for the configured signingServiceName/signingRegion, and sends it to the endpoint — so a mutation over an HTTP data source aimed at the SQS API actually calls SendMessage and the message becomes receivable on the queue, with the GraphQL response HTTP 200, no `errors` member, and SQS's response bound into $ctx.result for the response template. Authorization follows the role: while the service role lacks sqs:SendMessage the signed call is denied (AppSync surfaces the AccessDenied outcome of the call) and nothing lands on the queue; once the policy grants it, the same mutation delivers.
Emulator: the mutation never reaches SQS. The 200 body is {"data":{"sendToQueue":null},"errors":[{"path":["sendToQueue"],"message":"This endpoint does not call the HTTP data source sim2real_sqs_http."}]} — an emulator-authored message for a capability that is not implemented; the outbound request is never built, never signed and never sent, so neither the delivery nor the denied→granted transition can be observed.
Why: calling an AWS service API directly from AppSync through an AWS_IAM-signed HTTP data source is the documented Lambda-less integration pattern. On the emulator every such field returns null with a message no AWS SDK or application has ever seen, so cross-service flows (AppSync → SQS and, by the same path, any other AWS API) simply do not work, and IAM-scoped behaviour on those calls cannot be exercised.
Fix: the GraphQL data plane's resolver dispatch for HTTP data sources in the appsync service crate — render the request template into an HTTP request, sign it SigV4 with the data source's serviceRoleArn for httpConfig.authorizationConfig.awsIamConfig, send it to httpConfig.endpoint, and bind the response into ctx.result (including surfacing the call's AccessDenied outcome when the role lacks the action).
Related-but-not-covering: 20261114T023619Z-apigateway-appsync-an-appsync-http-data-source-signing.md and 20261111T065707Z-apigateway-appsync-the-graphql-api-s-published-uris.md record the signing scenario under the apigateway__appsync service token and carry neither of these checks' replay keys.
Traces: both check ids above.

## Evidence
Expected (AWS): with an SQS queue, an appsync-trusted role allowed sqs:SendMessage, and a type=HTTP data source {endpoint: the SQS endpoint, authorizationConfig: {authorizationType: AWS_IAM, awsIamConfig: {signingRegion: <region>, signingServiceName: "sqs"}}}, the mutation `sendToQueue(q: <queueUrl>, b: <payload>)` returns HTTP 200 with no `errors`, and ReceiveMessage on the queue returns that payload. With the role's sqs:SendMessage removed the same mutation is denied and nothing is enqueued; after the grant, it delivers.
Actual (emulator), both checks: HTTP 200 with GraphQL errors and nothing enqueued — AssertionError: sendToQueue mutation returned GraphQL errors: [{'path': ['sendToQueue'], 'message': 'This endpoint does not call the HTTP data source sim2real_sqs_http.'}].

## Steps to Reproduce
1. sqs.CreateQueue and keep its QueueUrl; iam.CreateRole trusting appsync.amazonaws.com (first with no sqs:SendMessage, for the denial leg).
2. appsync.CreateGraphqlApi(API_KEY) + CreateApiKey; StartSchemaCreation with a schema exposing `sendToQueue(q: String!, b: String!): String` and poll to SUCCESS.
3. CreateDataSource(apiId, name='sim2real_sqs_http', type='HTTP', httpConfig={endpoint: <the SQS endpoint>, authorizationConfig: {authorizationType: 'AWS_IAM', awsIamConfig: {signingRegion: <region>, signingServiceName: 'sqs'}}}, serviceRoleArn=<role>).
4. CreateResolver(Mutation.sendToQueue, kind=UNIT, dataSourceName='sim2real_sqs_http') whose request template issues the SQS SendMessage call and whose response template returns ctx.result.
5. POST the mutation to uris.GRAPHQL with x-api-key — AWS returns 200 with no errors and the message is receivable from the queue; the emulator returns data null plus "This endpoint does not call the HTTP data source sim2real_sqs_http."
6. For the second check, attach sqs:SendMessage to the role and repeat: AWS flips from denied (nothing enqueued) to delivered; the emulator answers the same "does not call the HTTP data source" error both times.
