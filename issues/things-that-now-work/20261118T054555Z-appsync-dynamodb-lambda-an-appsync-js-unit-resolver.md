Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync__dynamodb__lambda.deep-appsync-2-appsync-authz-ttl-override-zero-reinvokes-authorizer-every-request
Issue-Key: appsync__dynamodb__lambda.deep-appsync-2-appsync-authz-ttl-override-zero-reinvokes-authorizer-every-request|_leg#24|AssertionError|assert status == 200 and body == {"data": {"whoami": token}}, (
Issue-Commit: 37301ce23688d3043715086de5be7f765b8fadfc

# appsync__dynamodb__lambda: an APPSYNC_JS UNIT resolver over a NONE data source resolves to null (200, no errors), so a Lambda authorizer's ttlOverride:0 re-invocation contract is unobservable

> _Note: this issue was identified against the code base at commit `37301ce23688d3043715086de5be7f765b8fadfc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GraphQL (AppSync data-plane POST on the API's GRAPHQL uri)

## Description
One net-new root cause in the AppSync GraphQL data plane, reached through this service's authorization-cache scenario.

Cause 1 of 1: a UNIT resolver with runtime=APPSYNC_JS over a NONE data source answers HTTP 200 with its field null and no `errors` member, instead of serving the value the resolver returned, so the request never carries the authorizer's resolverContext.
AWS: Smithy: appsync#LambdaAuthorizerConfig.authorizerResultTtlInSeconds (@range min 0 max 3600, default 0) is documented as 'The number of seconds a response should be cached for... The Lambda function can override this by returning a ttlOverride key in its response', and appsync#Resolver carries kind/dataSourceName/runtime/code for the resolver itself. The model covers only the control plane, so execution is recorded behavior only (AppSync developer guide, 'Lambda authorization' and 'JavaScript resolvers'/runtime.earlyReturn): the authorizer returns {isAuthorized, resolverContext, deniedFields, ttlOverride}; AppSync binds `resolverContext` onto `ctx.identity.resolverContext` for the resolver; a request function that calls `runtime.earlyReturn(v)` short-circuits the data-source call and makes `v` the field value; and AppSync answers HTTP 200 with that value and no `errors` member. With ttlOverride 0 the decision is not cached, so the authorizer runs on every request — a contract that can only be observed if the authorized request actually resolves its field.
Emulator: the authorizer is reached and authorizes (the request is served HTTP 200 with no `errors`), but the field comes back null: `{'data': {'whoami': None}}` for a resolver whose request function early-returns `ctx.identity.resolverContext.token`. The identical symptom appears in this suite for a resolver that early-returns a plain literal constant (see the companion issue filed for appsync__lambda in this same sweep), so the result is discarded whatever it is. Hypothesis: the APPSYNC_JS evaluator does not wire `runtime.earlyReturn`'s value or the response function's return value into the field value, so the field falls back to null; whether resolverContext is bound onto ctx.identity cannot be told apart while every resolver result is dropped. No HTTP refusal is involved. Open issue 20261110T155738Z-appsync-dynamodb-lambda-the-published-graphql-endpoint-is.md attributes this scenario's failure to the published uris.GRAPHQL naming an unserved real-AWS host; that is no longer what happens — the request reaches the emulator's endpoint and is answered 200 — and that issue does not carry this check.
Why: a client gets a successful-looking 200 whose data is null and whose error channel is empty, so a resolver that ran cannot be distinguished from a legitimately null field; and every per-request authorization behaviour layered on it (resolverContext propagation, ttlOverride 0 forcing a fresh authorizer invocation per request, a positive TTL serving a cached grant that still carries its context) becomes unverifiable because no leg ever returns a value.
Fix: the AppSync data-plane resolver executor — the APPSYNC_JS evaluation path for a UNIT resolver (the runtime.earlyReturn short-circuit and the response function's return value feeding the field value), and the AWS_LAMBDA authorization hook's binding of the authorizer's resolverContext onto ctx.identity. Not CreateResolver's validation or the control-plane projections, which already round-trip runtime/code and authorizerResultTtlInSeconds.
Traces: appsync__dynamodb__lambda.deep-appsync-2-appsync-authz-ttl-override-zero-reinvokes-authorizer-every-request.

## Evidence
Check appsync__dynamodb__lambda.deep-appsync-2-appsync-authz-ttl-override-zero-reinvokes-authorizer-every-request, [mismatch]:
Setup: an API with authenticationType AWS_LAMBDA and lambdaAuthorizerConfig.authorizerResultTtlInSeconds set; the authorizer increments a DynamoDB counter per invocation and returns {"isAuthorized": true, "resolverContext": {"token": <the request's token>}} plus "ttlOverride": 0 for cache-disabling tokens. Query.whoami is a UNIT resolver over a NONE data source, runtime APPSYNC_JS, whose request function calls runtime.earlyReturn(ctx.identity.resolverContext.token) and whose response function returns ctx.result.
Expected (AWS): HTTP 200 with body {"data": {"whoami": "<the token sent in Authorization>"}} and no `errors` member, on every request of the leg.
Actual: AssertionError: request 1/4 for '<token>' must be authorized and carry its resolverContext, got 200 {'data': {'whoami': None}} — HTTP 200, field null, `errors` absent. (The token string is a per-run generated value, not a requirement; the requirement is that the field carries the value the resolver returned.)

## Steps to Reproduce
1. Build and boot the emulator from this checkout and replay the behavioral suite; the check id in trace_ids fails.
2. Create a DynamoDB table for the invocation counter and a Lambda authorizer that ADDs 1 to the counter keyed by the request's authorizationToken and returns {"isAuthorized": true, "resolverContext": {"token": <that token>}}, plus "ttlOverride": 0 for tokens that must not be cached.
3. CreateGraphqlApi with authenticationType AWS_LAMBDA and lambdaAuthorizerConfig {authorizerUri: <authorizer arn>, authorizerResultTtlInSeconds: <non-zero>}; StartSchemaCreation with `type Query { whoami: String }`; lambda AddPermission for appsync.amazonaws.com with SourceArn the api ARN.
4. CreateDataSource type NONE and CreateResolver for Query.whoami with kind UNIT, runtime {"name":"APPSYNC_JS","runtimeVersion":"1.0.0"} and code whose request function calls runtime.earlyReturn(ctx.identity.resolverContext.token) and whose response function returns ctx.result.
5. POST `query { whoami }` to the API's GRAPHQL uri with an Authorization header: AWS answers 200 with data.whoami equal to that token; the emulator answers 200 with data.whoami null and no `errors`, so the ttlOverride:0 re-invocation count can never be compared.
6. Re-replay exactly the check id in trace_ids to confirm a fix.
