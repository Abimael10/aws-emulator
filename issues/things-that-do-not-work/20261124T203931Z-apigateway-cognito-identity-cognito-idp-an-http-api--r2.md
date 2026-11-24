Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cognito-identity__cognito-idp.apigw-cognito-user-pool-client-app-client-id-validation-positive apigateway__cognito-identity__cognito-idp.apigw-cognito-authorizer-multiple-user-pools-audience-negative
Issue-Key: apigateway__cognito-identity__cognito-idp.apigw-cognito-user-pool-client-app-client-id-validation-positive|_poll_invoke#17|Timeout|time.sleep(interval_s)
Issue-Key: apigateway__cognito-identity__cognito-idp.apigw-cognito-authorizer-multiple-user-pools-audience-negative|_poll_invoke#15|Timeout|time.sleep(interval_s)
Issue-Commit: b396de6621b98a4950294fcbb23a4743977dd096

# apigateway__cognito-identity__cognito-idp: an HTTP API route behind a Cognito JWT authorizer still never answers 2xx for a token issued to the configured audience — both audience checks exhaust the harness budget

> _Note: this issue was identified against the code base at commit `b396de6621b98a4950294fcbb23a4743977dd096`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAuthorizer

## Description
REOPENS 20261114T110649Z-apigateway-cognito-identity-cognito-idp-an-http-api.md: its fix left the call refused. That resolved issue carries exactly these two checks' replay keys and describes this same cause; on replay both still fail identically, so the fix did not take and the behaviour is re-filed with their keys.

Cause 1 of 1: an HTTP API whose route is AuthorizationType=JWT, bound to a JWT authorizer configured with the pool's issuer and Audience=[<app client id>], and whose $default stage is created with AutoDeploy=true, never serves a successful response for an ID token issued to that client. The control request is polled every 3 s against <stage url>/test and never observes 200, so the run is killed at the harness cap ('Check timed out after 60.0s') and the audience-negative leg is never reached.

AWS: Smithy (apigatewayv2) CreateAuthorizer takes AuthorizerType (enum, JWT), IdentitySource (e.g. $request.header.Authorization) and JwtConfiguration (JWTConfiguration — 'Represents the configuration of a JWT authorizer. Required for the JWT authorizer type. Supported only for HTTP APIs.'); JWTConfiguration$Audience is 'A list of the intended recipients of the JWT. A valid JWT must provide an aud that matches at least one entry in this list', and JWTConfiguration$Issuer is the provider's base domain. Recorded behavior: a GET through the published ApiEndpoint's $default stage carrying 'Authorization: Bearer <ID token whose aud is the configured client and whose iss is the configured issuer>' is authorized and reaches the AWS_PROXY integration, which answers with its own status and body (here 200, body 'ok'); the same request with an ID token issued to a client that is NOT listed in Audience is refused HTTP 401 and the integration is never invoked.

Emulator: the request reaches no decidable outcome. Neither an authorized 200 from the integration nor a 401 refusal is produced within the budget — the control poll returns a non-2xx (or no answer) for its whole window — so the JWT authorizer's issuer/audience decision is unobservable on the HTTP API data plane. All control-plane calls (CreateApi, CreateIntegration, AddPermission, CreateAuthorizer, CreateRoute, CreateStage $default AutoDeploy, also via `aws apigatewayv2 create-stage`) succeed first.

Why: the JWT authorizer is the standard authorization mode for an HTTP API fronted by a Cognito user pool. A client of the emulator can provision the whole API successfully and then find that no request through the published endpoint is ever served: the allow path (aud matches) is unusable and the refusal path (aud is an unlisted client of the same pool) silently has no enforcement to observe, so audience scoping cannot be relied on or tested.

Fix: the apigatewayv2 HTTP API execute-api data plane — the $default-stage request path that matches the route key, runs the JWT authorizer (issuer + aud validation against JwtConfiguration, identity source $request.header.Authorization) and then invokes the AWS_PROXY integration — together with the stage/route readiness that makes the published ApiEndpoint answer at all. Open issues describe the same symptom for other check groups but do not carry these keys: 20261025T084658Z-apigatewayv2-cognito-idp-lambda-all-three-jwt-authorizer--r2.md, 20261029T070719Z-apigatewayv2-cognito-idp-lambda-an-http-api-route.md, 20261029T182116Z-apigatewayv2-cognito-idp-lambda-an-http-api-route-2f3eea.md.

Traces: apigateway__cognito-identity__cognito-idp.apigw-cognito-user-pool-client-app-client-id-validation-positive (hangs in _poll_invoke on the first graded assertion), apigateway__cognito-identity__cognito-idp.apigw-cognito-authorizer-multiple-user-pools-audience-negative (hangs in the same control poll, so its 401 leg never runs).

## Evidence
Expected (AWS): GET <ApiEndpoint>/test with 'Authorization: Bearer <ID token of the client listed in JwtConfiguration.Audience>' returns HTTP 200 with the integration's body 'ok'; the same request with an ID token issued to a client NOT in Audience returns HTTP 401 and the integration is never invoked.

Actual (emulator): both checks report 'Error: Check timed out after 60.0s'. The control poll (urllib GET, retried every 3 s, 60 s window, returning only on status 200) never returns, so `assert status == 200, f"expected 200 for a token issued to the configured client, got {status}: {body}"` is never reached in apigw-cognito-user-pool-client-app-client-id-validation-positive, and the negative assertion `assert bad_status == 401` in apigw-cognito-authorizer-multiple-user-pools-audience-negative is never exercised. The fixed issue 20261114T110649Z-...-an-http-api.md recorded the same two timeouts at the same two poll sites.

## Steps to Reproduce
1. CreateUserPool; two CreateUserPoolClient calls (client_a, client_b, GenerateSecret=false, ExplicitAuthFlows ALLOW_ADMIN_USER_PASSWORD_AUTH/ALLOW_REFRESH_TOKEN_AUTH); AdminCreateUser + AdminSetUserPassword(Permanent=true); AdminInitiateAuth per client for token_a and token_b.
2. CreateFunction (python3.12) returning {"statusCode":200,"body":"ok"}.
3. apigatewayv2 CreateApi (ProtocolType=HTTP), CreateIntegration (AWS_PROXY, PayloadFormatVersion 2.0, IntegrationMethod POST), lambda AddPermission for apigateway.amazonaws.com on arn:aws:execute-api:<region>:<acct>:<apiId>/*/*.
4. CreateAuthorizer (AuthorizerType=JWT, IdentitySource=[$request.header.Authorization], JwtConfiguration={Audience:[client_a], Issuer:<pool issuer url>}); CreateRoute (RouteKey 'GET /test', AuthorizationType=JWT, AuthorizerId, Target=integrations/<id>); CreateStage StageName='$default' AutoDeploy=true (SDK or `aws apigatewayv2 create-stage --auto-deploy`).
5. GET <ApiEndpoint>/test with 'Authorization: Bearer <token_a>' — expected 200 'ok'; actual: never answers 2xx and the check is killed at the runner cap. Then GET with 'Bearer <token_b>' — expected 401 with no integration invocation; never reached.
