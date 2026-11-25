Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2__cognito-idp__lambda.jwt-authorizer-expired-token-rejected-never-reaches-lambda apigatewayv2__cognito-idp__lambda.http-api-jwt-authorizer-rejects-foreign-issuer-and-audience apigatewayv2__cognito-idp__lambda.http-api-jwt-authorization-scope-mismatch-not-delivered apigatewayv2__cognito-idp__lambda.jwt-authorizer-claims-reach-the-lambda-event
Issue-Key: apigatewayv2__cognito-idp__lambda.jwt-authorizer-expired-token-rejected-never-reaches-lambda|_poll_http#18|AssertionError|raise AssertionError(
Issue-Key: apigatewayv2__cognito-idp__lambda.http-api-jwt-authorizer-rejects-foreign-issuer-and-audience|_poll_http#19|TimeoutError|raise TimeoutError(f"{description} ({url}): the run budget ended first; not graded")
Issue-Key: apigatewayv2__cognito-idp__lambda.http-api-jwt-authorization-scope-mismatch-not-delivered|_poll_http#18|Timeout|time.sleep(interval_s)
Issue-Key: apigatewayv2__cognito-idp__lambda.jwt-authorizer-claims-reach-the-lambda-event|_poll_http#19|Timeout|time.sleep(interval_s)
Issue-Commit: 7a03c6c633814b42753ce8c8f03d4e7bdff82ea0

# REOPENS: an HTTP API route behind a Cognito JWT authorizer is now reachable but refuses a freshly minted, valid token with 401 Unauthorized, so no JWT-authorizer behaviour is observable

> _Note: this issue was identified against the code base at commit `7a03c6c633814b42753ce8c8f03d4e7bdff82ea0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAuthorizer

## Description
REOPENS 20261114T171242Z-apigatewayv2-cognito-idp-lambda-reopens-an-http-api-s-invoke-address-is.md: its fix left the call refused. The published invoke address is now an emulator-served one (https://127.0.0.1:<port>/_aws/execute-api/<apiId>/<route>), so the request reaches the emulator — and the emulator answers 401 for a token that must be accepted, or never answers at all.

Cause 1 of 1: the JWT authorizer of an HTTP API refuses every presented token, including a freshly minted access/ID token from exactly the user pool configured as the authorizer's Issuer and the app client configured as its Audience, so the integration is never invoked and no authorization decision of the authorizer is distinguishable.

AWS: Smithy apigatewayv2 — CreateAuthorizer{AuthorizerType=JWT, IdentitySource, JwtConfiguration{Issuer, Audience}} (BadRequestException 400 / NotFoundException 404 / ConflictException 409 are its only modelled refusals), and CreateApi -> ApiEndpoint, the address a client invokes. Recorded behavior of the data plane: a request to a route protected by a JWT authorizer whose Issuer is the pool's issuer and whose Audience lists the app client is ACCEPTED when the bearer token verifies against that issuer's JWKS, is unexpired and carries the route's AuthorizationScopes — the integration runs and its response is returned 200, with the token's claims surfaced in requestContext.authorizer.jwt.claims of the Lambda event (payload format 2.0). It is refused 403 with body {"message":"Forbidden"} when the token is valid but lacks the route's AuthorizationScopes, and 401 with body {"message":"Unauthorized"} when the token is missing, expired, from a foreign issuer or carries an audience the authorizer does not list; in every refusal the integration is never invoked. The 401/403 bodies are fixed text with no variable part.

Emulator: a GET to the deployed route carrying a bearer token minted seconds earlier for the configured pool and client is answered HTTP 401 with body {"message":"Unauthorized"} for the whole 180 s the check polls, and on two of the four checks the route produces no answer at all within the check's budget. The status/code/body the emulator serves is AWS's wording for a refusal — the defect is that the refusal is served for a token AWS accepts, so the authorizer's ACCEPT path is unreachable and its three refusal classes (scope mismatch 403, foreign issuer/wrong audience 401, expired token 401) are indistinguishable from the blanket 401.

Why: a client cannot use an HTTP API behind a Cognito JWT authorizer at all — every authenticated request is refused — and, worse for a test, the emulator cannot be distinguished from a correct one on the negative cases, because it answers 401 whether the token is good or bad. The claims an application reads from requestContext.authorizer.jwt.claims never arrive, and a scope-gated route's 403 is never produced. Hypothesis: the authorizer cannot verify the token because the pool's issuer is published on a host the emulator does not serve (filed separately for cognito-idp: the `iss` claim is https://cognito-idp.<region>.localhost:<port>/<poolId>), so JWKS retrieval inside the authorizer fails and every token falls through to Unauthorized. Related OPEN issues 20261025T084658Z-apigatewayv2-cognito-idp-lambda-all-three-jwt-authorizer--r2.md, 20261029T070719Z-apigatewayv2-cognito-idp-lambda-an-http-api-route.md and 20261029T182116Z-apigatewayv2-cognito-idp-lambda-an-http-api-route-2f3eea.md describe the earlier symptom (the route never answers); the fixed 20261114T123844Z-apigatewayv2-cognito-idp-lambda-a-route-behind-a.md describes this same 401-for-a-valid-token and did not resolve it.

Fix: JWT-authorizer evaluation on the apigatewayv2 execute-api data plane (issuer/JWKS resolution, audience and expiry validation, scope matching) and the route-to-integration dispatch behind it — a token that verifies against the configured Issuer with a listed Audience must reach the integration (200, claims in requestContext.authorizer.jwt.claims), a scope mismatch must be 403 {"message":"Forbidden"} without invoking it, and only a missing/expired/foreign-issuer/wrong-audience token may be 401 {"message":"Unauthorized"}.

Traces (check ids): apigatewayv2__cognito-idp__lambda.jwt-authorizer-expired-token-rejected-never-reaches-lambda, .http-api-jwt-authorizer-rejects-foreign-issuer-and-audience, .http-api-jwt-authorization-scope-mismatch-not-delivered, .jwt-authorizer-claims-reach-the-lambda-event.

## Evidence
apigatewayv2__cognito-idp__lambda.jwt-authorizer-expired-token-rejected-never-reaches-lambda:
```
File "check_..._jwt_authorizer_expired_token_rejected_never_reaches_lambda.py", line 107, in run
  status, _, text = _poll_http(
AssertionError: GET /expiry with a freshly minted, still-valid token (https://127.0.0.1:38115/_aws/execute-api/mp67t4dbd4/expiry) never reached an accepted status within 180.0s. Last: 401 '{"message":"Unauthorized"}'
```
apigatewayv2__cognito-idp__lambda.http-api-jwt-authorizer-rejects-foreign-issuer-and-audience:
```
File "check_..._http_api_jwt_authorizer_rejects_foreign_issuer_and_audience.py", line 107, in run
  status, _, text = _poll_http(
TimeoutError: GET /guarded with a token from the configured pool and client (https://127.0.0.1:42511/_aws/execute-api/mp67t4dbd4/guarded): the run budget ended first; not graded
```
.http-api-jwt-authorization-scope-mismatch-not-delivered and .jwt-authorizer-claims-reach-the-lambda-event: Check timed out after 60.0s on the same positive-control GET.
Expected (AWS): the control GET with the freshly minted token for the configured Issuer/Audience -> HTTP 200 and the integration's body, with the token's claims in requestContext.authorizer.jwt.claims of the Lambda event; a valid token lacking the route's AuthorizationScopes -> 403 {"message":"Forbidden"} and the integration not invoked; an expired token, a foreign issuer or an unlisted audience -> 401 {"message":"Unauthorized"} and the integration not invoked.
Actual (emulator): 401 {"message":"Unauthorized"} for the valid token, sustained for 180 s, or no answer within the budget. (API id, loopback port and route paths are per-run values; the refusal of a valid token is the requirement violated.)
This is what 20261114T171242Z-...-reopens-an-http-api-s-invoke-address-is.md (status: fixed) claimed to fix: it carries exactly these four check keys. The invoke address is fixed; the calls are still refused.

## Steps to Reproduce
1. cognito-idp: CreateUserPool, a resource server with a scope, and an app client with that scope; mint an access/ID token for a confirmed user (InitiateAuth).
2. apigatewayv2: CreateApi(ProtocolType='HTTP'); CreateAuthorizer(AuthorizerType='JWT', IdentitySource=['$request.header.Authorization'], JwtConfiguration={'Issuer': <pool issuer>, 'Audience': [<client id>]}).
3. CreateIntegration(AWS_PROXY to a Lambda, PayloadFormatVersion='2.0'); CreateRoute for a route whose AuthorizationScopes the token holds and another requiring a scope it lacks; CreateStage(StageName='$default', AutoDeploy=true).
4. Read ApiEndpoint and GET <ApiEndpoint>/<route> with 'Authorization: Bearer <token>'.
   Expected: 200 with the integration's body (claims visible in requestContext.authorizer.jwt.claims). Actual: 401 {"message":"Unauthorized"}, repeatedly, for the whole poll window.
5. Repeat with a token lacking the route's scope (expect 403 {"message":"Forbidden"}), with an expired token, and with a token from another pool / for an unlisted audience (expect 401) — none of these is distinguishable while a valid token is also refused 401.
