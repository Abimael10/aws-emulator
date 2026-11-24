Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__lambda__logs.apigw-lambda-authorizer-deny-blocks-backend-invocation
Issue-Key: apigateway__lambda__logs.apigw-lambda-authorizer-deny-blocks-backend-invocation|_stays_absent#22|Timeout|time.sleep(interval_s)
Issue-Commit: 9bfcdac351b9a65b625c204ecd025e93d3476b59

# apigateway__lambda__logs: the TOKEN-authorizer Deny scenario on a deployed REST stage reaches no verdict — the check is killed at the 60 s cap with no 403, no allowed 200 and no access-log record

> _Note: this issue was identified against the code base at commit `9bfcdac351b9a65b625c204ecd025e93d3476b59`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke (execute-api GET on a deployed REST stage with a CUSTOM/TOKEN authorizer)

## Description
Cause 1 of 1: a REST method with authorizationType=CUSTOM bound to a TOKEN Lambda authorizer produces none of the observables AWS produces when the request is driven through the deployed stage — the check reaches no verdict at all and is killed at the harness cap.

AWS: Smithy (apigateway): CreateAuthorizer POST /restapis/{restApiId}/authorizers -> 201, input members type (AuthorizerType, required), authorizerUri, identitySource, authorizerResultTtlInSeconds (NullableInteger); TestInvokeAuthorizer POST /restapis/{restApiId}/authorizers/{authorizerId} returns policy/principalId, so the authorizer's policy document is a modelled, first-class artefact of the service. The data plane itself is not modelled, so for it this is recorded behavior only: a request carrying a token for which the authorizer Lambda returns a policyDocument whose Statement has Effect "Deny" on the request's methodArn is refused by the execute-api front door with HTTP 403 and a JSON body whose fixed text is AWS's explicit-deny message — `{"Message":"User is not authorized to access this resource with an explicit deny"}` (fixed text; it contains no request- or state-derived values) — and the backend integration is NEVER invoked. The same route with a token the authorizer allows answers 200 with the integration's body. With accessLogSettings set to a JSON format naming $context.requestId, $context.status and $context.path, the stage writes one access-log record per served request into the destination log group: status is the string "403" for the denied request, path is the STAGE-QUALIFIED resource path (fixed shape `/{stageName}/{resourcePath}`, both values taken from the request — e.g. the stage name and resource created by the caller), and requestId is a non-empty id the gateway bound to the request.

Emulator: the check produces no graded observation of any kind; the suite reports only `Error: Check timed out after 60.0s`. The first bounded wait in the check polls the deployed stage's invoke URL until it answers one of 200/401/403 (its own deadline is 150 s, longer than the harness cap), so on the evidence the stage never answers an accepted status for the denied request; downstream of it nothing is graded either — not the 403 explicit-deny body, not the absence of the backend's BACKEND-INVOKED line, not the 403 access-log record and its stage-qualified $context.path, and not the allowing-token 200 control.

Why: a client cannot distinguish "the authorizer denied the request" from "the authorizer was ignored and the backend ran". Any consumer that relies on a custom authorizer as its authorization boundary gets no refusal to test against, and a gateway that answers nothing (or answers 200) silently exposes the backend integration the Deny was supposed to protect.

Fix: the execute-api data-plane front door for REST stages — the CUSTOM/TOKEN authorizer gate that invokes the authorizer function, evaluates the returned policy document against the method ARN and short-circuits with 403 before the integration is dispatched — together with the stage's access-log record emission ($context.status / $context.path / $context.requestId resolution) in the apigateway stage handler.

Note: the open issues 20261123T174035Z-apigateway-dynamodb-lambda-a-rest-api-request-denied.md and 20261123T001943Z-apigateway-dynamodb-lambda-the-token-authorizer-deny-scenario.md describe a similar TOKEN-authorizer hang under the apigateway__dynamodb__lambda heading; they do not carry this check's replay key, so this check is filed separately and should be re-replayed on its own.

Traces: apigateway__lambda__logs.apigw-lambda-authorizer-deny-blocks-backend-invocation.

## Evidence
Expected (AWS): denied token -> HTTP 403, body {"Message":"User is not authorized to access this resource with an explicit deny"}; backend Lambda log group contains no BACKEND-INVOKED line; access log in the destination group contains the configured JSON record with "status":"403" and "path":"/<stage>/<resource>" (stage-qualified) and a non-empty requestId; allowing token -> HTTP 200 with the backend's marker body and a BACKEND-INVOKED line in its log group.

Actual (emulator): no verdict. Suite output for apigateway__lambda__logs.apigw-lambda-authorizer-deny-blocks-backend-invocation is exactly `[mismatch] ... Error: Check timed out after 60.0s`. The stage never reached any of 200/401/403 for the denied request inside the harness budget (the check's own poll deadline for that first beat is 150 s), so no status, body, access-log record or absence window was ever graded.

## Steps to Reproduce
1. Create two Python Lambdas: an authorizer returning {principalId, policyDocument:{Statement:[{Action:"execute-api:Invoke", Effect: "Allow" if authorizationToken=="allow-me" else "Deny", Resource: event["methodArn"]}]}}, and a backend proxy function that prints "BACKEND-INVOKED <marker>" and returns 200.
2. apigateway CreateRestApi; CreateAuthorizer(type=TOKEN, authorizerUri=the authorizer function's invocation ARN, identitySource=method.request.header.Authorization, authorizerResultTtlInSeconds=0); lambda AddPermission for apigateway.amazonaws.com on both functions.
3. CreateResource pathPart=secure; PutMethod(GET, authorizationType=CUSTOM, authorizerId); PutIntegration(type=AWS_PROXY -> backend).
4. UpdateAccount to set cloudwatchRoleArn, CreateDeployment stage "test" and UpdateStage with accessLogSettings {destinationArn: a log group, format: {"requestId":"$context.requestId","status":"$context.status","path":"$context.path"}}.
5. GET https://<api-id>.execute-api.<region>.../test/secure with Authorization: deny-me -> expect 403 and the explicit-deny message; then with Authorization: allow-me -> expect 200 and the marker.
6. FilterLogEvents on the backend's /aws/lambda/<name> group (no BACKEND-INVOKED for the denied request) and on the access-log group (a JSON record with status 403 and path /test/secure).
Observed: step 5 never returns an accepted status and the check is killed at the 60 s harness cap with no verdict.
