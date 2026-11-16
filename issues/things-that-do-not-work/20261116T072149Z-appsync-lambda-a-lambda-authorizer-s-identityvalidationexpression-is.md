Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync__lambda.appsync-lambda-authorizer-identity-validation-expression-rejects-before-invoke
Issue-Key: appsync__lambda.appsync-lambda-authorizer-identity-validation-expression-rejects-before-invoke|run#24|AssertionError|assert rejected_status == 401, (
Issue-Commit: 4b5ad12ea538b6995117aae35ef3d6dcdbc48855

# appsync__lambda: a Lambda authorizer's identityValidationExpression is never applied — a non-matching token is served 200 instead of refused 401 before the function runs

> _Note: this issue was identified against the code base at commit `4b5ad12ea538b6995117aae35ef3d6dcdbc48855`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GraphQL (data-plane POST /graphql, AWS_LAMBDA authorizer)

## Description
Cause 1 of 1: LambdaAuthorizerConfig$identityValidationExpression is stored but never evaluated on the data plane, so the regex gate in front of the authorizer function does not exist.
AWS: Smithy LambdaAuthorizerConfig$identityValidationExpression — "A regular expression for validation of tokens before the Lambda function is called." The expression is tested against the Authorization token BEFORE the authorizer Lambda is invoked, so a token that does not match is refused even when the function would authorize every token it receives. Recorded behavior for that refusal: HTTP 401 with exactly one error, errorType 'UnauthorizedException' and message 'You are not authorized to make this call.' (entirely fixed text — nothing from the request is interpolated), and NO 'data' key in the body at all. A token that does match still reaches the function and resolves normally.
Emulator: with identityValidationExpression '^sim2real-ok-.*$' on the API and an authorizer function that authorizes unconditionally, a request carrying Authorization 'sim2real-nope-<marker>' was answered HTTP 200 with a full GraphQL data envelope ({'data': {'echo': 'rejected-…'}}) — the regex was not applied and the request was authorized by the function instead. The matching-token leg of the same check reached the resolver and returned 200, so the authorizer path itself is live; only the regex gate is missing.
Why: the gate is overly permissive in the direction that matters — tokens the configured expression excludes are admitted, and the authorizer function is invoked for traffic AWS never forwards to it. A client that relies on identityValidationExpression to reject malformed/foreign tokens (and to bound authorizer invocations) gets no refusal at all, and the 401/UnauthorizedException class never appears.
Fix: the AWS_LAMBDA authorization path in the GraphQL data plane must evaluate the API's lambdaAuthorizerConfig.identityValidationExpression against the Authorization header before dispatching the authorizer invocation, and refuse a non-match with the 401 UnauthorizedException envelope (no data key).
Traces: appsync__lambda.appsync-lambda-authorizer-identity-validation-expression-rejects-before-invoke.

## Evidence
Expected: HTTP 401, body with exactly one error {errorType: 'UnauthorizedException', message: 'You are not authorized to make this call.'} and no 'data' key, for a token that fails identityValidationExpression ('^sim2real-ok-.*$').
Actual: AssertionError "a token failing identityValidationExpression must be refused at the transport before the always-authorizing function could run; got 200 {'data': {'echo': 'rejected-5ed9431c00c3'}}" (the marker is a per-run value; the requirement is the 401 refusal and the absent data key).
Control in the same check: a matching token ('sim2real-ok-<marker>') was answered 200 with data.echo == the marker, so the authorizer invocation path works and only the pre-invoke regex gate diverges.

## Steps to Reproduce
1. Create an IAM role for lambda.amazonaws.com and deploy a Lambda authorizer function that returns {"isAuthorized": true} for every event; grant lambda:InvokeFunction to appsync.amazonaws.com.
2. CreateGraphqlApi with authenticationType=AWS_LAMBDA and lambdaAuthorizerConfig={authorizerUri: <function arn>, identityValidationExpression: '^sim2real-ok-.*$'}.
3. StartSchemaCreation with 'type Query { echo(marker: String!): String }', poll GetSchemaCreationStatus to SUCCESS, CreateDataSource type=NONE and a UNIT resolver for Query.echo that echoes $ctx.args.marker.
4. POST {"query": "query Echo($marker: String!) { echo(marker: $marker) }", "variables": {"marker": "<m>"}} to GraphqlApi$uris.GRAPHQL with header Authorization: 'sim2real-ok-<m>' -> 200 and data.echo == <m> (barrier: the authorizer path is live).
5. Repeat with Authorization: 'sim2real-nope-<m>' -> AWS answers 401 with one UnauthorizedException error and no data key; the emulator answers 200 with data.echo == <m>.
