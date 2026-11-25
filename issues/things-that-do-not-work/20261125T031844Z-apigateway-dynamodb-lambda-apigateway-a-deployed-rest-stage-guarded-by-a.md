Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__dynamodb__lambda.apigw-authorizer-deny-integration-not-invoked
Issue-Key: apigateway__dynamodb__lambda.apigw-authorizer-deny-integration-not-invoked|_collect_marker#27|Timeout|time.sleep(3.0)
Issue-Commit: 738f456db4ce6c2a753baa11d2746fa368552910

# apigateway: a deployed REST stage guarded by a TOKEN Lambda authorizer never answers — neither the Allow (200) nor the Deny (403) leg returns, the check dies at the 60 s cap

> _Note: this issue was identified against the code base at commit `738f456db4ce6c2a753baa11d2746fa368552910`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutMethod

## Description
Cause 1 of 1: the data-plane request to a deployed REST API stage whose method is authorizationType=CUSTOM with a TOKEN Lambda authorizer never receives a response at all. Failing check: apigateway__dynamodb__lambda.apigw-authorizer-deny-integration-not-invoked (verdict: mismatch, "Check timed out after 60.0s"). Both legs of the scenario — the Allow leg and the Deny leg — are unobservable because the request is left unanswered.

AWS: the control plane that sets this up is modeled — Smithy (apigateway, backplanecontrolservice): CreateAuthorizer input requires restApiId/name/type (type is the AuthorizerType enum; TOKEN here) and accepts identitySource, authorizerUri and authorizerResultTtlInSeconds (NullableInteger; 0 disables result caching); PutMethod input requires restApiId/resourceId/httpMethod/authorizationType and accepts authorizerId; PutIntegration binds type=AWS_PROXY; CreateDeployment publishes the stage. The invoke-time request/response on the execute-api stage is NOT modeled — recorded behavior only: every request to the deployed stage invokes the TOKEN authorizer with authorizationToken (taken from the identitySource header) and methodArn, then evaluates the returned policyDocument. When the single statement is {Action: execute-api:Invoke, Effect: Allow, Resource: <methodArn>} the stage dispatches the AWS_PROXY integration and answers with the integration's response — HTTP 200 and the integration's side effect becomes visible. When Effect is Deny the stage answers HTTP 403 with an AccessDeniedException-class body and does NOT dispatch the integration, so no side effect appears. The fixed text of that 403 body is AWS's standard access-denied wording for a denied authorizer policy ("User is not authorized to access this resource with an explicit deny"); the variable parts are taken from the request and the evaluated policy (the method ARN / resource being denied), never from any single run. In every case the stage ANSWERS, bounded — no AWS behaviour leaves an execute-api request hanging.

Emulator: the GET against the deployed stage URL (/<stage>/<path>) returns nothing at all. No HTTP status line, no error code and no body are ever captured, for either the Allow token or the Deny token, and the harness kills the check at its 60 s budget: "Error: Check timed out after 60.0s". Consequently the check's own bounded polls — table ACTIVE, the marker-item appearance poll, and the 45 s leak-observation window used to prove the integration did NOT run — can never conclude.

Why: a client calling an authorizer-gated REST stage hangs instead of receiving 200 or 403. Nothing times out at the client's own deadline with a usable error either; the connection simply produces no response. Worse, the security-relevant guarantee is silently unverifiable: a caller cannot tell whether API Gateway refused the denied request or quietly dispatched the backend, so an over-permissive emulator (one that invokes the integration despite an explicit Deny) is indistinguishable from a correct one. Any client, SDK or test suite that depends on a denied request being refused with 403 and leaving no side effect gets no answer on which to base that decision.

Fix: the API Gateway REST data plane for a deployed stage — the invoke pipeline that resolves stage/resource/method from the request path, invokes the configured TOKEN authorizer's Lambda with authorizationToken and methodArn, evaluates the returned policyDocument against the method ARN, and then either dispatches the AWS_PROXY integration (Allow) or serves the 403 access-denied response WITHOUT dispatching (Deny). Each stage of that pipeline must terminate and produce a response rather than leaving the request open; the Deny branch in particular must short-circuit before the integration dispatch.

Traces: apigateway__dynamodb__lambda.apigw-authorizer-deny-integration-not-invoked.

Related open issues that the replay report did NOT mark as owning this check: 20261123T001943Z-apigateway-dynamodb-lambda-the-token-authorizer-deny-scenario.md (describes the same unanswered TOKEN-authorizer flow on this service token but was not reported as carrying this run's replay key), 20261124T223818Z-apigateway-lambda-logs-the-token-authorizer-deny-scenario.md and 20261107T191316Z-apigateway-lambda-logs-the-lambda-authorizer-deny-check.md (the same unanswered deny path observed through CloudWatch Logs, a different service token). This filing is grounded in the failing check above, not in those issues.

## Evidence
Check: apigateway__dynamodb__lambda.apigw-authorizer-deny-integration-not-invoked
Verdict: mismatch — "Error: Check timed out after 60.0s" (no response recorded for either leg).

Setup performed through the control plane (all succeeded): CreateRestApi; CreateResource pathPart="guarded"; CreateAuthorizer(type="TOKEN", identitySource="method.request.header.Authorization", authorizerUri=arn:aws:apigateway:<region>:lambda:path/2015-03-31/functions/<authorizer-fn-arn>/invocations, authorizerResultTtlInSeconds=0); PutMethod(httpMethod="GET", authorizationType="CUSTOM", authorizerId=<id>); PutIntegration(type="AWS_PROXY", integrationHttpMethod="POST", uri=<backend-fn invocations arn>); CreateDeployment(stageName=<stage>). lambda:AddPermission grants lambda:InvokeFunction to apigateway.amazonaws.com for BOTH the authorizer (SourceArn .../authorizers/<id>) and the backend (SourceArn .../*/GET/guarded), so a missing invoke grant cannot explain a non-response.

The authorizer returns principalId "probe-user" and a policyDocument with one statement {Action: "execute-api:Invoke", Effect: "Allow" when authorizationToken matches the allowed token else "Deny", Resource: event["methodArn"]}. The backend writes a marker item into DynamoDB and returns {statusCode: 200}.

Expected (AWS):
  Allow leg — GET <invoke-url>/<stage>/guarded with the allowed Authorization header -> HTTP 200 from the AWS_PROXY integration, and the backend's marker item becomes readable with GetItem(ConsistentRead=True).
  Deny leg  — GET the same URL with any other Authorization value -> HTTP 403 with an AccessDeniedException-class body carrying AWS's explicit-deny wording, and the marker item does NOT reappear for the whole observation window (the integration must never be invoked).

Actual (emulator):
  Allow leg — no status, no headers, no body; the request never completes.
  Deny leg  — never reached, because the check is cut off at the 60 s harness cap during/after the first leg.
No HTTP status, error code or error message is produced for comparison, so the divergence is "the response never arrives" rather than a wrong status.

## Steps to Reproduce
1. dynamodb.create_table(KeySchema=[{AttributeName: pk, KeyType: HASH}], AttributeDefinitions=[{pk, S}], BillingMode="PAY_PER_REQUEST"); poll describe_table until TableStatus == "ACTIVE".
2. Create two Lambda functions:
   - backend: PutItem {pk: "invoked", path: <event path>} into that table, return {statusCode: 200, body: ...}.
   - authorizer: return {principalId: "probe-user", policyDocument: {Version: "2012-10-17", Statement: [{Action: "execute-api:Invoke", Effect: "Allow" if event["authorizationToken"] == <allowed token> else "Deny", Resource: event["methodArn"]}]}}.
3. apigateway.create_rest_api(name=...); get_resources to find the "/" root id; create_resource(parentId=<root>, pathPart="guarded").
4. apigateway.create_authorizer(type="TOKEN", identitySource="method.request.header.Authorization", authorizerUri=<authorizer fn invocations arn>, authorizerResultTtlInSeconds=0).
5. apigateway.put_method(httpMethod="GET", authorizationType="CUSTOM", authorizerId=<authorizer id>); apigateway.put_integration(httpMethod="GET", type="AWS_PROXY", integrationHttpMethod="POST", uri=<backend fn invocations arn>); apigateway.create_deployment(stageName=<stage>).
6. lambda.add_permission(Action="lambda:InvokeFunction", Principal="apigateway.amazonaws.com") for the authorizer (SourceArn .../authorizers/<id>) and for the backend (SourceArn .../*/GET/guarded).
7. GET <rest-api invoke url>/<stage>/guarded with header Authorization: <allowed token>.
   Expected: HTTP 200, then the marker item {pk: "invoked"} becomes readable via GetItem(ConsistentRead=True).
   Observed: the request never returns; no status/body is produced and the check is killed at the 60 s harness cap.
8. Delete the marker item, then GET the same URL with header Authorization: <any other value>.
   Expected: HTTP 403 with AWS's explicit-deny access-denied body, and the marker item never reappears over the observation window (the integration is not invoked).
   Observed: unreachable — step 7 never completes.
