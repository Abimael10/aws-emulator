Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2__cognito-idp__lambda.http-api-jwt-authorization-scope-mismatch-not-delivered apigatewayv2__cognito-idp__lambda.http-api-jwt-authorizer-rejects-foreign-issuer-and-audience apigatewayv2__cognito-idp__lambda.jwt-authorizer-claims-reach-the-lambda-event apigatewayv2__cognito-idp__lambda.jwt-authorizer-expired-token-rejected-never-reaches-lambda
Issue-Key: apigatewayv2__cognito-idp__lambda.http-api-jwt-authorization-scope-mismatch-not-delivered|_v2_invoke_url#18|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{path.lstrip('/')}"
Issue-Key: apigatewayv2__cognito-idp__lambda.http-api-jwt-authorizer-rejects-foreign-issuer-and-audience|_v2_invoke_url#18|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{path.lstrip('/')}"
Issue-Key: apigatewayv2__cognito-idp__lambda.jwt-authorizer-claims-reach-the-lambda-event|_v2_invoke_url#17|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{path.lstrip('/')}"
Issue-Key: apigatewayv2__cognito-idp__lambda.jwt-authorizer-expired-token-rejected-never-reaches-lambda|_v2_invoke_url#16|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{path.lstrip('/')}"
Issue-Commit: c7bc1d610827f184f11257fe9f128e3fbb77240c

# REOPENS: an HTTP API's invoke address is still an AWS execute-api host the emulator does not serve, so all four JWT-authorizer behaviours (scope mismatch, foreign issuer/audience, claims in the Lambda event, expired token) remain unreachable

> _Note: this issue was identified against the code base at commit `c7bc1d610827f184f11257fe9f128e3fbb77240c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateApi

## Description
REOPENS 20261109T124858Z-apigatewayv2-cognito-idp-lambda-an-http-api-s.md: its fix left the call refused.

Cause 1 of 1: the value apigatewayv2 publishes as an HTTP API's invoke address is an AWS production host rather than an address the emulator is reached on, so every request a client builds from it is refused before it is sent — and with it the whole JWT-authorizer surface of the HTTP API.

AWS: Smithy apigatewayv2 CreateApiResponse.ApiEndpoint is documented as "The URI of the API, of the form {api-id}.execute-api.{region}.amazonaws.com. The stage name is typically appended to this URI to form a complete path to a deployed API stage." — ApiEndpoint is the address a client invokes, and requests to it are served by the API's routes. For an emulator the requirement is reachability, not the literal AWS name: recorded behavior only — a value the emulator publishes as an invoke address must be a host the emulator itself serves (http(s)://<own host>:<port>/<path>). Behind that address AWS enforces the JWT authorizer per route: a Cognito access token holding a route's AuthorizationScopes is served (200 from the integration) while one lacking it is refused 403 and the integration is never invoked; a token from a foreign issuer or with a wrong audience, and an expired token, are refused 401; and an accepted token's claims are surfaced in requestContext.authorizer.jwt.claims of the Lambda event.

Emulator: CreateApi returns ApiEndpoint as "https://<apiId>.execute-api.<region>.amazonaws.com" (one run showed https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com — the api-id, region and port are per-run values; the HOST FORM is the defect). The emulator announced an address other than the one it is reached on, so the data-plane call is refused at egress with "endpoint_url was given https://<apiId>.execute-api.<region>.amazonaws.com (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener". No HTTP status, error code or body is produced, because nothing is sent.

Why: a client that follows the documented path (read ApiEndpoint, append the route) cannot invoke the API at all. No authorization decision of a JWT authorizer is observable — a scope-mismatched request is neither delivered nor refused 403, a foreign-issuer/wrong-audience or expired token is never refused 401, and the claims never reach the Lambda event — so an application cannot be exercised against the emulator and the emulator cannot be distinguished from one with no authorizer enforcement at all. The open issues 20261025T084658Z-apigatewayv2-cognito-idp-lambda-all-three-jwt-authorizer--r2.md, 20261029T070719Z-apigatewayv2-cognito-idp-lambda-an-http-api-route.md, 20261029T182116Z-apigatewayv2-cognito-idp-lambda-an-http-api-route-2f3eea.md and 20261114T123844Z-apigatewayv2-cognito-idp-lambda-a-route-behind-a.md describe neighbouring symptoms (a route that never answers / a held-scope token refused 401); none of them carries this refusal, and none can be confirmed while the published address is unreachable.

Fix: the apigatewayv2 CreateApi/GetApi/GetApis serializers (the ApiEndpoint member) must publish a host the emulator serves, and the execute-api HTTP API data plane — including JWT-authorizer evaluation on that path — must answer on it.

Traces: apigatewayv2__cognito-idp__lambda.http-api-jwt-authorization-scope-mismatch-not-delivered, .http-api-jwt-authorizer-rejects-foreign-issuer-and-audience, .jwt-authorizer-claims-reach-the-lambda-event, .jwt-authorizer-expired-token-rejected-never-reaches-lambda.

## Evidence
Replay — all four checks fail identically at the same frame (context.py _published_target), e.g. apigatewayv2__cognito-idp__lambda.http-api-jwt-authorization-scope-mismatch-not-delivered:
```
File "context.py", line 603, in endpoint_route
  return self._published_target(url, "endpoint_url"), {}
File "context.py", line 617, in _published_target
  raise PublishedUrlRejected(...)
simparity.published.PublishedUrlRejected: endpoint_url was given https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:43317/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
```
The same refusal, differing only in the per-run loopback port (43317 / 41117 / 41049 / 33903), is reported for .http-api-jwt-authorizer-rejects-foreign-issuer-and-audience, .jwt-authorizer-claims-reach-the-lambda-event and .jwt-authorizer-expired-token-rejected-never-reaches-lambda.
Expected: ApiEndpoint names an emulator-served host, so a GET to <ApiEndpoint>/<route> with a Cognito access token reaches the HTTP API: 200 from the integration when the token holds the route's AuthorizationScopes, 403 (integration not invoked) on a scope mismatch, 401 for a foreign issuer / wrong audience / expired token, and requestContext.authorizer.jwt.claims present in the Lambda event.
Actual: ApiEndpoint is an *.execute-api.<region>.amazonaws.com host, so no request is ever sent and no response exists. (api-id, region and port are per-run values, not part of the requirement.)

This exact refusal is what 20261109T124858Z-apigatewayv2-cognito-idp-lambda-an-http-api-s.md (status: fixed) claimed to fix; the same four checks are refused the same way again.

## Steps to Reproduce
1. cognito-idp: create a user pool, a resource server with a scope, and an app client; mint an access token for a user.
2. apigatewayv2 CreateApi(Name=..., ProtocolType="HTTP"); CreateAuthorizer(AuthorizerType="JWT", IdentitySource=["$request.header.Authorization"], JwtConfiguration={Issuer: <pool issuer>, Audience: [<client id>]}).
3. CreateIntegration(AWS_PROXY to a Lambda, PayloadFormatVersion="2.0"); CreateRoute for a route whose AuthorizationScopes the token holds and another requiring a scope it lacks; CreateStage(StageName="$default", AutoDeploy=true).
4. Read ApiEndpoint from CreateApi/GetApi and GET <ApiEndpoint>/<route> with the Authorization bearer token.
Expected: the request reaches the emulator's HTTP API data plane and is answered 200 / 403 / 401 per the authorizer's decision.
Actual: ApiEndpoint is https://<apiId>.execute-api.<region>.amazonaws.com, a host the emulator does not serve, so the call is refused at egress and never made.
