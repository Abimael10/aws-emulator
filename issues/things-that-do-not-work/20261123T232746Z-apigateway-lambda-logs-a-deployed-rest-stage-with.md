Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__lambda__logs.apigw-lambda-authorizer-deny-blocks-backend-invocation
Issue-Key: apigateway__lambda__logs.apigw-lambda-authorizer-deny-blocks-backend-invocation|_stays_absent#22|Timeout|time.sleep(interval_s)
Issue-Commit: f74c2e237ecba449a4252c2c022ae2f28d9c08d0

# apigateway__lambda__logs: a deployed REST stage with a TOKEN Lambda authorizer and accessLogSettings never answers — the Deny/Allow legs and the access-log record are cut off at the 60 s harness cap

> _Note: this issue was identified against the code base at commit `f74c2e237ecba449a4252c2c022ae2f28d9c08d0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
Cause 1 of 1: an HTTPS GET to a deployed REST API stage whose GET method is authorizationType=CUSTOM behind a TOKEN Lambda authorizer, and whose stage carries accessLogSettings pointing at a CloudWatch Logs group, never receives a response. Neither the Deny leg (403 without invoking the AWS_PROXY integration), nor the Allow leg (200 from the backend), nor the access-log record for the refused request is ever observable. Failing check: apigateway__lambda__logs.apigw-lambda-authorizer-deny-blocks-backend-invocation.

AWS: Smithy (apigateway, com-amazonaws-apigateway-backplanecontrolservice) models the control plane used here — CreateAuthorizer (POST /restapis/{restApiId}/authorizers, 201; required restApiId/name/type, type=TOKEN from the AuthorizerType enum, plus authorizerUri, identitySource, authorizerResultTtlInSeconds), PutMethod (authorizationType/authorizerId), PutIntegration (type=AWS_PROXY, integrationHttpMethod=POST), CreateDeployment (stageName), and UpdateStage (PATCH /restapis/{restApiId}/stages/{stageName}, 200, output member accessLogSettings of shape AccessLogSettings carrying destinationArn + format). The execute-api data plane itself is not in the Smithy model — recorded behavior only. Recorded behavior: every request to the deployed stage is evaluated against the TOKEN authorizer, whose Lambda is invoked with authorizationToken (from method.request.header.Authorization) and methodArn, and with authorizerResultTtlInSeconds=0 no decision is cached. When the returned policyDocument statement is {Action: execute-api:Invoke, Effect: Deny, Resource: <methodArn>}, API Gateway answers HTTP 403 with an explicit-deny body whose message contains the fixed text "not authorized" (AWS's wording for an authorizer explicit deny: "User is not authorized to access this resource with an explicit deny"; the variable part is the principal/resource taken from the request and the authorizer's policy) and does NOT dispatch the AWS_PROXY integration, so the backend Lambda never runs and emits no log line. When the statement's Effect is Allow, the stage answers HTTP 200 with the integration's body and the backend Lambda's stdout reaches /aws/lambda/<backend-name>. Independently, with accessLogSettings configured, the gateway writes one access-log record per request into the configured log group, rendered through the configured $context format: $context.status is the status actually served ("403" for the denied request), $context.path is the stage-qualified path /{stage}/{resource} (the variable parts are the deployed stage name and the resource path from the request), and $context.requestId is a bound, non-empty request id. Every one of these responses is bounded; AWS never leaves an execute-api request unanswered.

Emulator: nothing is ever served for the GET on the deployed stage. The replay output carries no status, no error code and no body — the only observation is "Error: Check timed out after 60.0s", i.e. the harness cut the check off while it was still awaiting the first response. Because the request never completes, every downstream observation is unreachable too: the 403 status and its "not authorized" message, the absence of the BACKEND-INVOKED marker in /aws/lambda/<backend-name>, the access-log record containing "403" and naming /secure, the stage-qualified $context.path value "/<stage>/<resource>", the non-empty $context.requestId, and the Allow-leg 200 plus its backend log line.

Why: a client calling an authorizer-gated REST stage hangs instead of receiving 403 or 200, so SDKs and browsers see a socket timeout where AWS returns a definite answer. Worse, the security guarantee this path exists to provide is unverifiable: because no response and no backend log line are produced, a caller cannot tell "the authorizer's Deny was honoured and the integration was not dispatched" from "the authorizer was ignored" — and an emulator that later starts answering 200 for a denied token would be silently over-permissive. Operators relying on access logs also get no record that the request was ever seen or refused.

Fix: the API Gateway execute-api data plane for a deployed REST stage — the request pipeline that resolves stage/resource/method, invokes the TOKEN authorizer's Lambda with authorizationToken and methodArn, evaluates the returned policyDocument and either serves 403 without dispatching the integration (Deny) or dispatches the AWS_PROXY integration and returns its response (Allow); and the access-logging emitter that renders the stage's accessLogSettings format ($context.status, $context.path, $context.requestId) into the configured CloudWatch Logs group. Each step must terminate and answer.

Related open issue that does NOT carry this check's replay key and therefore does not cover it: 20261123T001943Z-apigateway-dynamodb-lambda-the-token-authorizer-deny-scenario.md (same hang on a different scenario token, without the accessLogSettings/$context leg and without the backend CloudWatch Logs observation).

Traces: apigateway__lambda__logs.apigw-lambda-authorizer-deny-blocks-backend-invocation.

## Evidence
Check: apigateway__lambda__logs.apigw-lambda-authorizer-deny-blocks-backend-invocation
Verdict: mismatch — "Error: Check timed out after 60.0s" (no status, no error code, no body ever recorded).

Expected (AWS), per the check's own assertions:
  GET <invoke-url>/<stage>/secure with header `Authorization: deny-me`
    -> HTTP 403, body message containing the fixed text "not authorized" (the authorizer's explicit-deny message; principal/resource are variable parts taken from the request and the returned policy).
    -> the backend Lambda does NOT run: no "BACKEND-INVOKED <marker>" line appears in /aws/lambda/<backend-name> for the whole 120 s observation window.
    -> one access-log record appears in the configured log group, parseable as the configured JSON format, containing "403", with "path" == "/<stage>/secure" (stage-qualified) and a non-empty "requestId".
  GET the same URL with header `Authorization: allow-me`
    -> HTTP 200 whose JSON body carries the backend's marker, and "BACKEND-INVOKED <marker>" appears in /aws/lambda/<backend-name>.

Actual (emulator): the awaited response never arrives; the check is cut off at the 60 s cap before any leg reaches a verdict. No status code, error code or body is produced for either the Deny or the Allow leg, and no access-log record is observed.

Setup facts that rule out a missing grant as the reason: lambda:AddPermission grants lambda:InvokeFunction to apigateway.amazonaws.com on BOTH the authorizer (arn:aws:execute-api:<region>:<account>:<api-id>/authorizers/<authorizer-id>) and the backend (arn:aws:execute-api:<region>:<account>:<api-id>/*/*); authorizerResultTtlInSeconds=0 disables decision caching; the account-level cloudwatchRoleArn is set before UpdateStage so the accessLogSettings patch is accepted; and the configured format names $context.requestId, which UpdateStage requires.

## Steps to Reproduce
1. Create two Lambda functions: an `authorizer` that returns {principalId, policyDocument:{Version:"2012-10-17", Statement:[{Action:"execute-api:Invoke", Effect: "Allow" if event["authorizationToken"]=="allow-me" else "Deny", Resource: event["methodArn"]}]}}; and a `backend` that prints "BACKEND-INVOKED <MARKER>" and returns {statusCode:200, body: json({"marker": <MARKER>})}, with MARKER set as an environment variable.
2. logs.create_log_group for the access log, and set the API Gateway account-level cloudwatchRoleArn (UpdateAccount) so accessLogSettings can be patched.
3. apigateway.create_rest_api; create_resource(pathPart="secure"); create_authorizer(type="TOKEN", identitySource="method.request.header.Authorization", authorizerUri="arn:aws:apigateway:<region>:lambda:path/2015-03-31/functions/<authorizer-arn>/invocations", authorizerResultTtlInSeconds=0).
4. put_method(httpMethod="GET", authorizationType="CUSTOM", authorizerId=<id>); put_integration(type="AWS_PROXY", integrationHttpMethod="POST", uri="arn:aws:apigateway:<region>:lambda:path/2015-03-31/functions/<backend-arn>/invocations").
5. lambda.add_permission for apigateway.amazonaws.com on the authorizer (.../authorizers/<id>) and on the backend (.../*/*).
6. create_deployment(stageName="test"), then update_stage with patchOperations setting accessLogSettings/destinationArn to the log group ARN and accessLogSettings/format to {"requestId":"$context.requestId","status":"$context.status","path":"$context.path"}.
7. GET https://<api-id>.execute-api.<region>.<host>/test/secure with header `Authorization: deny-me`.
   Observed: the request never completes; the check is cut off at the 60 s harness cap with no response.
   Expected: HTTP 403 whose body message contains "not authorized", and no "BACKEND-INVOKED" line in /aws/lambda/<backend-name>.
8. filter_log_events on the access log group: expect one record containing "403" that parses as the configured JSON with path == "/test/secure" and a non-empty requestId.
9. GET the same URL with header `Authorization: allow-me`.
   Expected: HTTP 200 carrying the backend marker, and "BACKEND-INVOKED <marker>" in /aws/lambda/<backend-name>.
