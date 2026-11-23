Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cognito-identity__cognito-idp.apigw-cognito-user-pool-client-app-client-id-validation-positive apigateway__cognito-identity__cognito-idp.apigw-cognito-authorizer-multiple-user-pools-audience-negative
Issue-Key: apigateway__cognito-identity__cognito-idp.apigw-cognito-user-pool-client-app-client-id-validation-positive|_poll_invoke#17|Timeout|time.sleep(interval_s)
Issue-Key: apigateway__cognito-identity__cognito-idp.apigw-cognito-authorizer-multiple-user-pools-audience-negative|_poll_invoke#15|Timeout|time.sleep(interval_s)
Issue-Commit: 47219b124a45386662a63382125b929260efea78

# apigateway__cognito-identity__cognito-idp: an HTTP API route behind a Cognito JWT authorizer never answers 200 for a token issued to the configured audience — both audience checks exhaust their budget

> _Note: this issue was identified against the code base at commit `47219b124a45386662a63382125b929260efea78`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAuthorizer

## Description
Cause 1 of 1: an HTTP API ($default auto-deploy stage) whose route has AuthorizationType=JWT and a JWT authorizer configured with Issuer=<the pool's issuer> and Audience=[<app client id>] never serves a successful response for an ID token issued to that client; the control request is polled until the whole-check budget is exhausted and the run is killed with 'Check timed out after 60.0s'. Failing checks: apigateway__cognito-identity__cognito-idp.apigw-cognito-user-pool-client-app-client-id-validation-positive (SDK + AWS CLI create-stage path) and apigateway__cognito-identity__cognito-idp.apigw-cognito-authorizer-multiple-user-pools-audience-negative (which can never reach its negative assertion because the control token never succeeds first).

AWS: Smithy (apigatewayv2): CreateAuthorizer takes AuthorizerType (enum, JWT), IdentitySource (required list, e.g. $request.header.Authorization) and JwtConfiguration (JWTConfiguration — 'Represents the configuration of a JWT authorizer. Required for the JWT authorizer type. Supported only for HTTP APIs.'); JWTConfiguration$Audience is documented as 'A list of the intended recipients of the JWT. A valid JWT must provide an aud that matches at least one entry in this list.' Recorded behavior: with a route AuthorizationType=JWT bound to that authorizer and an AWS_PROXY integration, a request bearing an ID token whose aud equals the configured app client id and whose iss equals the configured issuer is authorized and reaches the integration, which returns its own status and body (here 200 with body 'ok'); a token whose aud is an unlisted client of the same pool is refused 401 and the integration is never invoked.

Emulator: the request never reaches a decidable outcome. The check repeatedly GETs <stage url>/test with a valid Bearer ID token and never observes 200, so the harness terminates it at its cap: 'Error: Check timed out after 60.0s'. Neither an authorized 200 from the integration nor a 401 refusal is produced within the budget, so the JWT-authorizer audience/issuer decision cannot be observed at all on the HTTP API data plane.

Why: the JWT authorizer is the standard authorization mode for HTTP APIs fronted by a Cognito user pool. A client of the emulator can create the api, integration, authorizer, route and $default auto-deploy stage successfully, yet no request through the published ApiEndpoint is ever served, so both the allow path (aud matches the configured client) and the refusal path (aud is an unlisted client) are untestable; code that depends on audience scoping silently has no enforcement to observe.

Fix: the apigatewayv2 HTTP API data plane — the $default stage request path that must match the route key, run the JWT authorizer (issuer + aud validation against JwtConfiguration) and then invoke the AWS_PROXY integration — together with the stage/route readiness that makes the published ApiEndpoint serve at all. Related open issues cover the same symptom for other check groups but do not carry these checks' replay keys: 20261025T084658Z-apigatewayv2-cognito-idp-lambda-all-three-jwt-authorizer--r2.md, 20261029T070719Z-apigatewayv2-cognito-idp-lambda-an-http-api-route.md and 20261029T182116Z-apigatewayv2-cognito-idp-lambda-an-http-api-route-2f3eea.md.

Traces: the two failing check ids above.

## Evidence
Expected (AWS): GET <ApiEndpoint>/$default/test with Authorization: Bearer <ID token of the client listed in JwtConfiguration.Audience> returns 200 with the integration's body ('ok'); the same request with an ID token issued to a client that is NOT in Audience returns 401 and the integration is never invoked.

Actual (emulator): 'Error: Check timed out after 60.0s' for both checks — the polled control request (urllib GET, retried every 3 s until the budget runs out) never returns 200, so the first graded assertion (`assert status == 200, f"expected 200 for a token issued to the configured client, got {status}: {body}"`) is never reached and the audience-negative case is never exercised.

## Steps to Reproduce
1. CreateUserPool + two CreateUserPoolClient calls (client_a, client_b, GenerateSecret=false, ALLOW_ADMIN_USER_PASSWORD_AUTH); AdminCreateUser + AdminSetUserPassword; AdminInitiateAuth for each client to get token_a and token_b.
2. CreateFunction (python3.12) returning {"statusCode":200,"body":"ok"}; apigatewayv2 CreateApi (ProtocolType=HTTP), CreateIntegration (AWS_PROXY, PayloadFormatVersion 2.0), Lambda AddPermission for apigateway.amazonaws.com on arn:aws:execute-api:<region>:<acct>:<apiId>/*/*.
3. CreateAuthorizer (AuthorizerType=JWT, IdentitySource=[$request.header.Authorization], JwtConfiguration={Audience:[client_a], Issuer:<pool issuer url>}) and CreateRoute (RouteKey 'GET /test', AuthorizationType=JWT, AuthorizerId, Target=integrations/<id>).
4. CreateStage StageName='$default' AutoDeploy=true (via SDK, or via `aws apigatewayv2 create-stage --auto-deploy`).
5. GET <ApiEndpoint>/test with Authorization: Bearer <token_a>. Expected 200 'ok'; actual: the endpoint never answers 200 and the scenario is killed at the runner cap. Then GET with Bearer <token_b>: expected 401 with no integration invocation.
