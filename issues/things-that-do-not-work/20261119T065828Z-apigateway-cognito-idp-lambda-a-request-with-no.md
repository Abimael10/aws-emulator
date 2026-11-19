Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cognito-idp__lambda.apigw-cognito-authorizer-missing-token-not-delivered
Issue-Key: apigateway__cognito-idp__lambda.apigw-cognito-authorizer-missing-token-not-delivered|_stays_absent#20|Timeout|time.sleep(interval_s)
Issue-Commit: e99960bc24a06e08ddf96250dc369db8427b79f1

# apigateway__cognito-idp__lambda: a request with no Authorization header to a COGNITO_USER_POOLS method on a deployed REST stage never answers — the check is killed at the 60 s harness cap instead of getting 401 Unauthorized

> _Note: this issue was identified against the code base at commit `e99960bc24a06e08ddf96250dc369db8427b79f1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
One root cause, grounded in one failing behavioral check.

Cause 1 of 1: traffic driven at a deployed REST API stage whose GET method has authorizationType=COGNITO_USER_POOLS (authorizer type COGNITO_USER_POOLS, providerARNs naming a live user pool, identitySource=method.request.header.Authorization, AWS_PROXY integration to a Lambda) never produces any HTTP response at all. The check polls the stage invoke URL for a status in {200,401,403} with a 150 s budget and is terminated by the harness at 60 s with no status ever observed.

AWS: Smithy (apigateway) models Authorizer.type as the enum AuthorizerType with value COGNITO_USER_POOLS, and Authorizer.identitySource is documented as required for a COGNITO_USER_POOLS authorizer, holding the request-header mapping expression for the token; the same documentation states that when the identity source is absent/null/empty the authorizer "returns a 401 Unauthorized response without calling the Lambda function". The data-plane behaviour itself is recorded behavior only: against real AWS, a GET to the deployed stage path with NO Authorization header answers HTTP 401 with API Gateway's Unauthorized envelope — body {"message":"Unauthorized"} (the text "Unauthorized" is fixed; the envelope carries no request-derived values) and header x-amzn-ErrorType: UnauthorizedException — and the integration is NOT invoked; a token-shaped but unverifiable bearer value likewise answers 401 without invoking the integration; and the identical request carrying a real pool IdToken answers the integration's 200.

Emulator: no HTTP status and no error code are produced for any of these requests. Every poll of the invoke URL fails to reach 200, 401 or 403 and the check is killed by the harness ('Error: Check timed out after 60.0s'). The emulator therefore neither refuses nor serves the request: the authorizer decision is never rendered on the wire.

Why: a client calling a deployed, Cognito-protected API blocks until its own socket timeout instead of receiving 401 fast, and the entire deny contract is unobservable — nothing proves an unauthenticated request was refused rather than quietly admitted to the backend. Because the response never arrives, the negative-delivery evidence (the backend Lambda must not run for a rejected request) cannot be established either, so an over-permissive authorizer would be indistinguishable from a correct one.

Fix: the execute-api deployed-stage request path in the apigateway service — the data-plane router that resolves stage/resource/method must evaluate the method's COGNITO_USER_POOLS authorizer and terminate the request with 401 (Unauthorized envelope) when the identity source is missing or the token does not verify against the pool named in providerARNs, and dispatch to the AWS_PROXY integration only on success. Today the request path produces no response at all.

Note: the open issue 20261023T170254Z-apigateway-cognito-idp-lambda-five-causes-cognito-authorizer.md describes the same family under its Cause 1 and even lists this check id, but the replay report did not mark this check as owned by it (its recorded replay key for this check no longer matches the current failure site), so it is filed here and that issue is named rather than duplicated silently.

Traces: apigateway__cognito-idp__lambda.apigw-cognito-authorizer-missing-token-not-delivered.

## Evidence
Failing check: [mismatch] apigateway__cognito-idp__lambda.apigw-cognito-authorizer-missing-token-not-delivered
Reported detail: 'Error: Check timed out after 60.0s' — no status line, no error code, no body.

Expected (AWS): first leg — GET <stage invoke URL>/secure with no Authorization header => HTTP 401, body containing API Gateway's fixed 'Unauthorized' envelope ({"message":"Unauthorized"}), backend not invoked. Second leg — the same GET with Authorization: 'Bearer <unverifiable string>' => HTTP 401, backend not invoked. Control leg — the same GET with a valid pool IdToken => HTTP 200 whose JSON body is produced by the AWS_PROXY integration, and the backend's log line appears in /aws/lambda/<fn>.

Actual (emulator): the first leg's poll (accept = status in {200,401,403}, 150 s budget, 3 s interval) never sees an accepted status; the check consumes the whole 60 s harness budget and is killed before any assertion can be evaluated. No 401, no 403, no 200, and no error response of any kind is observed on the deployed stage URL.

## Steps to Reproduce
1. cognito-idp: CreateUserPool, CreateUserPoolClient (ALLOW_ADMIN_USER_PASSWORD_AUTH), AdminCreateUser + AdminSetUserPassword(Permanent=true), then AdminInitiateAuth(ADMIN_USER_PASSWORD_AUTH) to obtain a real IdToken.
2. lambda: create a python3.12 AWS_PROXY backend function that returns 200 with a marker body, and AddPermission for apigateway.amazonaws.com on arn:aws:execute-api:<region>:<account>:<api-id>/*/*.
3. apigateway: CreateRestApi (REGIONAL), CreateAuthorizer(type=COGNITO_USER_POOLS, providerARNs=[<pool ARN>], identitySource=method.request.header.Authorization), CreateResource(pathPart=secure), PutMethod(GET, authorizationType=COGNITO_USER_POOLS, authorizerId=<id>), PutIntegration(type=AWS_PROXY -> the function), CreateDeployment + CreateStage(test).
4. GET <invoke-url>/test/secure with NO Authorization header. Expect HTTP 401 with the body {"message":"Unauthorized"}; observe that the request never resolves to any status (the poll runs out and the check is killed at the 60 s cap).
5. Repeat with Authorization: 'Bearer not-a-real-jwt.at.all' (expect 401) and with the real IdToken (expect 200 carrying the backend marker), and check /aws/lambda/<fn> for the backend log line — none of these legs is reachable today.
Re-replay: apigateway__cognito-idp__lambda.apigw-cognito-authorizer-missing-token-not-delivered
