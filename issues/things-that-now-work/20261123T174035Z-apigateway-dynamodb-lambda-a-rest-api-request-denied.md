Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__dynamodb__lambda.apigw-authorizer-deny-integration-not-invoked
Issue-Key: apigateway__dynamodb__lambda.apigw-authorizer-deny-integration-not-invoked|_collect_marker#27|Timeout|time.sleep(3.0)
Issue-Commit: f9dc389755539ec737bd6c84507d1519aa5a1660

# apigateway__dynamodb__lambda: a REST API request denied by a TOKEN Lambda authorizer reaches no verdict — the authorizer gate on the execute-api front door never settles

> _Note: this issue was identified against the code base at commit `f9dc389755539ec737bd6c84507d1519aa5a1660`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
Cause 1 of 1: the execute-api front door's TOKEN Lambda-authorizer gate for a deployed REST API never produces an observable allow/deny outcome, so the check cannot reach a verdict. Failing check id: apigateway__dynamodb__lambda.apigw-authorizer-deny-integration-not-invoked.

AWS: a REST API method configured with a TOKEN (request-header) Lambda authorizer invokes the authorizer first. When the authorizer returns an IAM policy with Effect=Allow for the method ARN, API Gateway forwards the request to the integration and the client gets the integration's response (HTTP 200 here) and the integration's side effect is performed exactly once. When the authorizer returns Effect=Deny, API Gateway answers the request itself with HTTP 403 and a JSON body whose fixed member name is `Message` (`{"Message":"User is not authorized to access this resource with an explicit deny"}`) and the integration is NOT invoked at all — no side effect is written. The Smithy model covers only the apigateway control plane (CreateAuthorizer/PutMethod/CreateDeployment); the execute-api data plane is not modelled, so the allow/deny dispatch itself is recorded behavior only, and the recording shows 200 + exactly one integration side effect on the Allow token and 403 + no side effect on the Deny token.

Emulator: the check never completes — the replay reports `Check timed out after 60.0s`. The check's own waits are the only things that can consume that budget: a 60 s poll for the integration's DynamoDB marker on the Allow leg, and a 45 s leak window on the Deny leg. Neither resolves, so the emulator produced neither the 200-plus-marker outcome of the Allow leg nor an observable 403-without-marker outcome for the Deny leg inside the budget. Hypothesis: the deployed stage's authorizer gate does not dispatch (authorizer invoke, policy evaluation, or the 403 short-circuit before integration dispatch is inert), so the request neither reaches the integration nor is refused.

Why: a client cannot rely on the authorizer at all. An Allow never delivers the request to the backend, and — the point of the negative leg — a Deny gives the caller no 403, so code that relies on API Gateway to block unauthorized callers before the integration runs cannot distinguish a blocked request from a hung one, and a denied caller may still reach the backend.

Fix: the execute-api request path for REST APIs — the authorizer invocation and policy evaluation step, and the 403 short-circuit that must run before integration dispatch (the deployed-stage front door's handler), plus the integration dispatch that must run on Allow.

Traces: apigateway__dynamodb__lambda.apigw-authorizer-deny-integration-not-invoked.

Related but not covering this: 20261117T115022Z-apigateway-lambda-logs-2-causes-the-token-authorizer.md and 20261029T180026Z-apigateway-dynamodb-lambda-logs-4-of-the-9-causes-remain-open.md are open on neighbouring API Gateway authorizer/integration behavior; this check is not listed as owned by either.

## Evidence
Expected (AWS): Allow token -> HTTP 200 from the integration and exactly one integration side effect (the DynamoDB marker item pk="invoked"); Deny token -> HTTP 403 with body member `Message` = "User is not authorized to access this resource with an explicit deny" and NO integration side effect.
Actual (emulator): no verdict at all — replay reports `Check timed out after 60.0s`. Neither the 60 s wait for the Allow leg's integration marker nor the 45 s Deny-leg leak window resolves, i.e. the emulator never served the Allow leg's 200 + marker, and never served an observable 403 for the Deny leg.

## Steps to Reproduce
1. Create a backend Lambda that writes a marker item (pk="invoked") into a DynamoDB table, and an authorizer Lambda that returns Effect=Allow for the token "let-me-in" and Effect=Deny for "keep-me-out".
2. Create a REST API with a TOKEN authorizer bound to that authorizer Lambda, a GET /guarded method using it, an AWS_PROXY integration to the backend, and deploy to stage "test".
3. GET the deployed /guarded with header Authorization: let-me-in -> expect 200 and the marker item to appear.
4. Delete the marker item, then GET /guarded with header Authorization: keep-me-out -> expect 403 and the marker item to stay absent.
5. Replay: apigateway__dynamodb__lambda.apigw-authorizer-deny-integration-not-invoked.
