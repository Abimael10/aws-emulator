Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync__cognito-idp.deep-appsync-2-appsync-authz-oidc-iat-ttl-rejects-aged-token
Issue-Key: appsync__cognito-idp.deep-appsync-2-appsync-authz-oidc-iat-ttl-rejects-aged-token|run#23|AssertionError|assert status == 401, (
Issue-Commit: 4de176942ad9f309e2f2fa330d84b5df9adf2b94

# appsync__cognito-idp: an OPENID_CONNECT API's openIDConnectConfig.iatTTL is never enforced on the GraphQL data plane — a token older than iatTTL is served 200 instead of refused 401 UnauthorizedException (1 cause)

> _Note: this issue was identified against the code base at commit `4de176942ad9f309e2f2fa330d84b5df9adf2b94`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PostGraphQL (AppSync GraphQL data plane) / CreateGraphqlApi

## Description
Cause 1 of 1: on a GraphQL API whose authenticationType is OPENID_CONNECT and whose openIDConnectConfig carries iatTTL, the data plane admits a request whose bearer token's `iat` claim is older than iatTTL. The age of the token is never checked, so iatTTL is configuration-only. Failing check: appsync__cognito-idp.deep-appsync-2-appsync-authz-oidc-iat-ttl-rejects-aged-token.

AWS: Smithy — OpenIDConnectConfig$iatTTL is "The number of milliseconds that a token is valid after it's issued to a user" (GraphqlApi$openIDConnectConfig, set at CreateGraphqlApi/UpdateGraphqlApi; the control-plane model does not describe the data plane, so the refusal shape below is recorded behavior only). Recorded behavior: for an OPENID_CONNECT API with openIDConnectConfig={issuer, clientId, iatTTL: N}, AppSync validates the presented JWT and additionally requires now - iat <= N/1000. While the token's age is inside iatTTL the field is served normally — HTTP 200, body exactly {"data": {"<field>": <value>}} with no "errors" member. Once the same still-unexpired token is older than iatTTL, the request is refused with HTTP 401 and a body whose "errors" list is exactly one entry [{"errorType": "UnauthorizedException", "message": "Unauthorized"}] and no data ("data" absent or null). The fixed text here is the errorType `UnauthorizedException` and the message `Unauthorized` — neither has any variable part; the only request/config-derived values in the rule are iatTTL (milliseconds, from openIDConnectConfig) and the token's own `iat`. A twin API configured with the same issuer/clientId but NO iatTTL accepts that same aged, unexpired token (iatTTL is the only bound being applied; `exp` still governs expiry).

Emulator: the aged token is accepted. The emulator answered HTTP 200 with {"data": {"ping": "pong"}} for a token whose iat was 18.0 s old on an API that had retained iatTTL=8000 ms (GetGraphqlApi confirmed openIDConnectConfig.iatTTL == 8000, so the control plane stored it). No 401, no UnauthorizedException, no errors member. The fresh-token leg against the same API passed, so the token itself is validated — only the iat-age bound is missing.

Why: a client relying on AWS's behavior loses the short-lived-token guarantee iatTTL exists to provide. iatTTL is the mechanism that caps replay of a captured OIDC token for an AppSync API independently of the (typically one-hour) `exp`; with it ignored, a stolen or logged token keeps working for its whole lifetime, and an application that tightened iatTTL as a security control gets no enforcement while GetGraphqlApi reports the control in place. Negative tests for this control pass silently against the emulator.

Fix: the GraphQL data plane's OPENID_CONNECT authorization path (the per-request authorizer that validates the OIDC bearer token before resolver execution): after signature/issuer/clientId validation, read the API's stored openIDConnectConfig.iatTTL and, when present and non-zero, reject a request whose token `iat` is more than iatTTL milliseconds in the past with HTTP 401 and the single UnauthorizedException/"Unauthorized" error entry and no data; leave the request path unchanged when iatTTL is absent.

Traces: appsync__cognito-idp.deep-appsync-2-appsync-authz-oidc-iat-ttl-rejects-aged-token. Open issue 20261114T070045Z-appsync-cognito-idp-an-appsync-js-resolver-over.md was filed from the earlier state of this same check (the APPSYNC_JS/NONE resolver answering {"ping": null}, which blocked the warm-up leg); the resolver now returns "pong" and the check reaches the aged-token leg, so this iatTTL gap is the next, distinct cause and is not covered by that issue. Open issue 20261110T155328Z-appsync-cognito-idp-an-openid-connect-graphql-api.md covers a still earlier state of the same check (unserved uris.GRAPHQL host).

## Evidence
Verbatim from the replay of appsync__cognito-idp.deep-appsync-2-appsync-authz-oidc-iat-ttl-rejects-aged-token:
  AssertionError: a token 18.0s past iat must be rejected under iatTTL=8000ms with 401, got 200 {'data': {'ping': 'pong'}}

The check's assertions at that point are:
  assert status == 401
  assert "data" not in body or body.get("data") is None
  assert errors == [{"errorType": "UnauthorizedException", "message": "Unauthorized"}]

Expected (AWS): HTTP 401, body with no data and errors == [{"errorType": "UnauthorizedException", "message": "Unauthorized"}], for a token presented more than openIDConnectConfig.iatTTL milliseconds after its iat and still inside exp.
Actual (emulator): HTTP 200, body {"data": {"ping": "pong"}}, no errors member.

Preconditions the check proved before this call:
- GetGraphqlApi(apiId=<ttl api>)["graphqlApi"]["openIDConnectConfig"]["iatTTL"] == 8000, so the API retained the configured bound.
- The same API served a freshly minted token (age < 8 s) with HTTP 200 {"data": {"ping": "pong"}} — token validation itself works.
- The aged token was still unexpired: age >= 18 s, time.time() < exp (exp is 1 h out), so only the iat-age bound could refuse it.

## Steps to Reproduce
1. CreateUserPool + an admin-auth app client + one confirmed user; use https://cognito-idp.<region>.amazonaws.com/<poolId> as the OIDC issuer and the app client id as clientId. Mint ID tokens with AdminInitiateAuth.
2. CreateGraphqlApi(authenticationType="OPENID_CONNECT", openIDConnectConfig={"issuer": <issuer>, "clientId": <clientId>, "iatTTL": 8000}); StartSchemaCreation with `schema { query: Query } type Query { ping: String }` and wait for SUCCESS; CreateDataSource(type="NONE"); CreateResolver(typeName="Query", fieldName="ping", kind="UNIT", dataSourceName=<that source>, runtime=APPSYNC_JS, code=<request function doing runtime.earlyReturn('pong'), response function returning ctx.result>).
3. Create a twin API the same way but with openIDConnectConfig={"issuer", "clientId"} only (no iatTTL).
4. Mint a token; immediately POST {"query": "query P { ping }"} to the iatTTL API's uris.GRAPHQL with header Authorization: <token>. Expect HTTP 200 {"data": {"ping": "pong"}} (token age < 8 s).
5. Sleep until the token is 18 s old (still far inside its 1 h exp) and POST the identical request with the SAME token.
   Expected: HTTP 401, no data, errors == [{"errorType": "UnauthorizedException", "message": "Unauthorized"}].
   Observed: HTTP 200, {"data": {"ping": "pong"}}.
6. POST the same aged token to the twin API without iatTTL: it must be accepted (HTTP 200 {"data": {"ping": "pong"}}) — only the iatTTL API may refuse it.
