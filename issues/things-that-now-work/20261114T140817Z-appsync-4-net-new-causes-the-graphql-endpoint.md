Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync.graphql-query-missing-api-key-rejected appsync.graphql-endpoint-cors-preflight-and-cross-origin-error-readability appsync.graphql-field-level-auth-directive-denies-other-provider appsync.resolver-util-unauthorized-denies-field-but-serves-sibling
Issue-Shape-Id: com.amazonaws.appsync#AWSDeepdishControlPlaneService
Issue-Key: appsync.graphql-query-missing-api-key-rejected|_poll_graphql#9|AssertionError|raise AssertionError(f"graphql endpoint never met expectation; last={last_body}")
Issue-Key: appsync.graphql-endpoint-cors-preflight-and-cross-origin-error-readability|run#21|AssertionError|assert code == 200, f"the CORS preflight must succeed, got {code!r} {raw!r}"
Issue-Key: appsync.graphql-field-level-auth-directive-denies-other-provider|run#13|AssertionError|assert data.get("keyOnly") == "key-only-value", (
Issue-Key: appsync.resolver-util-unauthorized-denies-field-but-serves-sibling|run#28|AssertionError|assert err.get("errorType") == "Unauthorized", (
Issue-Commit: 8a8830456e168aad64737c1eda6ee6081a6c4868

# appsync: 4 net-new causes — the GraphQL endpoint serves a query with NO API key, refuses the CORS preflight with 403, ignores field-level @aws_* auth directives, and omits errorType from the GraphQL errors envelope

> _Note: this issue was identified against the code base at commit `8a8830456e168aad64737c1eda6ee6081a6c4868`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateGraphqlApi

## Description
Four net-new root causes in the authorization and error-envelope layer of the emulator's GraphQL data plane (the listener serving GraphqlApi$uris.GRAPHQL). None is covered by an open issue: 20261114T022837Z-appsync-cloudwatch-appsync-graphql-errors-omit-errortype-and.md carries only the appsync__cloudwatch checks, and the appsync-token checks below are unowned.

Cause 1 of 4: a query carrying NO x-api-key header is served instead of refused.
AWS: CreateGraphqlApi (Smithy: appsync CreateGraphqlApi, POST /v1/apis, authenticationType AuthenticationType required — API_KEY here) makes the published endpoint reject any request that does not present a valid API key. Recorded behavior only for the POST itself: a request to GraphqlApi$uris.GRAPHQL with no x-api-key header is answered HTTP 401 with body {"errors":[{"errorType":"UnauthorizedException","message":"You are not authorized to make this call."}]} — the whole sentence is fixed text, no request values appear in it — and the operation is NOT executed.
Emulator: HTTP 200 with a normal data envelope for the unauthenticated request: {'data': {'echo': None}}. No 401, no errorType, no UnauthorizedException; the check's poll for the refusal never sees one.
Why: the API-key authentication mode is not enforced at all, so any anonymous caller reaches the API. Code that relies on AppSync to reject unauthenticated traffic is validated against an emulator that admits it — the most dangerous possible false pass.
Fix: the data plane's authorization gate must resolve the api's authenticationType and refuse a missing/unknown API key with 401 UnauthorizedException before the document is parsed or any resolver dispatched.
Traces: appsync.graphql-query-missing-api-key-rejected.

Cause 2 of 4: the CORS preflight on the GraphQL endpoint is refused 403.
AWS: recorded behavior only. An OPTIONS preflight to GraphqlApi$uris.GRAPHQL carrying Origin + Access-Control-Request-Method/Headers succeeds (2xx) and returns the Access-Control-Allow-* headers; AppSync does not authenticate the preflight and never applies a cross-origin policy to it, so a browser can then issue the real POST and read the error body.
Emulator: HTTP 403 with body {"errors":[{"errorType":"UnauthorizedException","message":"cross-site origin"}]}. Both the status and the message are emulator-authored: AWS has no "cross-site origin" error message, and a preflight is never a 403.
Why: no browser-based client (Amplify, Apollo in a page, the AppSync console) can call the emulator: the preflight fails, so the POST is never sent and the GraphQL error envelope is unreadable from script.
Fix: the GraphQL endpoint handler must answer OPTIONS preflights unauthenticated with 2xx plus the Access-Control-Allow-Origin/Methods/Headers response headers, and must not run the auth gate on them.
Traces: appsync.graphql-endpoint-cors-preflight-and-cross-origin-error-readability.

Cause 3 of 4: field-level @aws_api_key / @aws_iam auth directives are not honoured — the field a provider IS granted resolves to null.
AWS: recorded behavior only for the POST; the modeling context is CreateGraphqlApi.authenticationType plus additionalAuthenticationProviders (AdditionalAuthenticationProviders). On an API with more than one provider, a field annotated @aws_api_key resolves normally for an API-key caller and a field annotated @aws_iam is refused for that same caller with a per-field GraphQL error (errorType "Unauthorized", message "Not Authorized to access <fieldName> on type <typeName>" — fixed text with the field and type names taken from the query) and data.<field> null.
Emulator: BOTH fields come back null for the API-key caller — {'keyOnly': None, 'iamOnly': None} — so the granted field is denied too and the two directives are indistinguishable.
Why: multi-auth APIs cannot be modelled: a caller that AWS authorizes for a field gets nothing, and a client cannot tell an authorization denial from an empty result.
Fix: the data plane's per-field authorization must read the schema's @aws_* directives and the request's resolved auth provider, resolve the fields the provider is granted, and emit the per-field Unauthorized error only for the others.
Traces: appsync.graphql-field-level-auth-directive-denies-other-provider.

Cause 4 of 4: GraphQL errors raised by util.unauthorized() carry no errorType.
AWS: recorded behavior only. util.unauthorized() in a resolver produces an entry in the errors array whose errorType is the fixed string "Unauthorized" (with message "Not Authorized to access <fieldName> on type <typeName>"), the sibling fields in the same document still resolving normally.
Emulator: the errors entry is emitted without an errorType member at all (the check reads errorType and gets None), so a client cannot classify the failure. The sibling open issue 20261114T022837Z-appsync-cloudwatch-appsync-graphql-errors-omit-errortype-and.md reports the same omission for the appsync__cloudwatch checks only.
Why: AppSync clients branch on errorType (Unauthorized, QueryDepthLimitReached, DynamoDB:ConditionalCheckFailedException, Lambda:Unhandled, ...). With the member missing, every error is an untyped string and retry/auth-refresh logic cannot fire.
Fix: the GraphQL error serializer must populate errorType on every errors entry it emits (starting with the util.unauthorized/util.error paths and the service's own typed errors).
Traces: appsync.resolver-util-unauthorized-denies-field-but-serves-sibling.

## Evidence
Cause 1 — appsync.graphql-query-missing-api-key-rejected [mismatch]:
  File "check_appsync_graphql_query_missing_api_key_rejected.py", line 113, in _poll_graphql
  AssertionError: graphql endpoint never met expectation; last={'data': {'echo': None}}
  Expected (AWS): HTTP 401 with {"errors":[{"errorType":"UnauthorizedException","message":"You are not authorized to make this call."}]} for the key-less POST.
  Actual (emulator): HTTP 200 with a data envelope — the request is served.

Cause 2 — appsync.graphql-endpoint-cors-preflight-and-cross-origin-error-readability [mismatch]:
  AssertionError: the CORS preflight must succeed, got 403 '{"errors":[{"errorType":"UnauthorizedException","message":"cross-site origin"}]}'
  Expected (AWS): 2xx on the OPTIONS preflight with Access-Control-Allow-* headers.

Cause 3 — appsync.graphql-field-level-auth-directive-denies-other-provider [mismatch]:
  AssertionError: expected the @aws_api_key field to resolve for an API-key caller, got {'keyOnly': None, 'iamOnly': None}

Cause 4 — appsync.resolver-util-unauthorized-denies-field-but-serves-sibling [mismatch]:
  AssertionError: util.unauthorized() must surface errorType 'Unauthorized', got None

Smithy cited: appsync CreateGraphqlApi (POST /v1/apis; authenticationType -> AuthenticationType @required; additionalAuthenticationProviders -> AdditionalAuthenticationProviders; errors UnauthorizedException 401, BadRequestException 400, ApiLimitExceededException 400, ConcurrentModificationException 409, LimitExceededException 429, InternalFailureException 500); GraphqlApi$uris. The GraphQL data-plane POST, its {data,errors[{errorType,message,path}]} envelope and the @aws_* schema directives are not modeled — recorded behavior only.

## Steps to Reproduce
1. api = appsync.create_graphql_api(name=..., authenticationType="API_KEY")["graphqlApi"]; key = appsync.create_api_key(apiId=api["apiId"])["apiKey"]["id"]; start_schema_creation with 'schema { query: Query } type Query { echo: String }' and wait for SUCCESS; attach any resolver for Query.echo.
2. Cause 1: POST {"query":"query { echo }"} to api["uris"]["GRAPHQL"] with NO x-api-key header.
   Observed: HTTP 200, body {"data":{"echo":null}}. Expected: HTTP 401, {"errors":[{"errorType":"UnauthorizedException","message":"You are not authorized to make this call."}]}.
3. Cause 2: send an OPTIONS request to the same URL with Origin: https://example.com, Access-Control-Request-Method: POST, Access-Control-Request-Headers: content-type,x-api-key.
   Observed: HTTP 403, {"errors":[{"errorType":"UnauthorizedException","message":"cross-site origin"}]}. Expected: 2xx with Access-Control-Allow-Origin/Methods/Headers.
4. Cause 3: create an api with authenticationType="API_KEY" and additionalAuthenticationProviders=[{authenticationType:"AWS_IAM"}]; schema 'type Query { keyOnly: String @aws_api_key iamOnly: String @aws_iam }' with resolvers on both; POST a query selecting both fields with only x-api-key.
   Observed: {"data":{"keyOnly":null,"iamOnly":null}}. Expected: keyOnly resolved, iamOnly null with a per-field error errorType "Unauthorized".
5. Cause 4: give a field an APPSYNC_JS resolver whose response function calls util.unauthorized(), and select it next to a sibling field; POST the query.
   Observed: the errors entry has no errorType member. Expected: errorType == "Unauthorized" with the sibling field still resolved.
