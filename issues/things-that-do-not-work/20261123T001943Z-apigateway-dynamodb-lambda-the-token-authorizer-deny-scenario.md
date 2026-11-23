Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__dynamodb__lambda.apigw-authorizer-deny-integration-not-invoked
Issue-Key: apigateway__dynamodb__lambda.apigw-authorizer-deny-integration-not-invoked|_collect_marker#28|Timeout|return None
Issue-Commit: 5b88798ccff0ec7f765809a75ef2c7dfb1d7aad0

# apigateway__dynamodb__lambda: the TOKEN-authorizer Deny scenario never reaches a verdict — the deployed REST stage leg hangs to the 60 s harness cap

> _Note: this issue was identified against the code base at commit `5b88798ccff0ec7f765809a75ef2c7dfb1d7aad0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TestInvokeAuthorizer

## Description
Cause 1 of 1: the whole TOKEN-authorizer Allow/Deny flow on a deployed REST API stage never completes. The check is cut off by the harness at its 60 s budget with no status, error code or body captured, so neither the Allow leg (200 + the integration's side effect) nor the Deny leg (403 + no integration invocation) is observable. Failing check: apigateway__dynamodb__lambda.apigw-authorizer-deny-integration-not-invoked.

AWS: a REST method with authorizationType=CUSTOM bound to a TOKEN Authorizer (Smithy: apigateway CreateAuthorizer, type=TOKEN, identitySource=method.request.header.Authorization, authorizerUri=arn:aws:apigateway:<region>:lambda:path/2015-03-31/functions/<fn-arn>/invocations, authorizerResultTtlInSeconds=0 to disable caching; PutMethod authorizationType/authorizerId; PutIntegration type=AWS_PROXY; CreateDeployment stageName) is evaluated on every request to the deployed execute-api stage. The data-plane request/response is not modeled — recorded behavior only: with a token the authorizer maps to an Allow policy for the method ARN, the stage returns HTTP 200 from the AWS_PROXY integration and the backend's side effect (here a DynamoDB PutItem of a marker item) becomes visible; with any other token the authorizer returns a policyDocument whose Effect is Deny and the stage answers HTTP 403 with an AccessDeniedException-class body WITHOUT invoking the integration, so no side effect appears. Either way the stage answers, bounded.

Emulator: no response is ever served for the request to the deployed stage, so the harness ends the check: "Error: Check timed out after 60.0s". Because nothing is captured, the divergence is "the awaited response never arrives" rather than a wrong status or body; the check's own polls (table ACTIVE 120 s, marker 60 s, leak window 45 s) can never be satisfied inside the run budget.

Why: a client calling an authorizer-gated stage hangs instead of getting 200 or 403. The security-relevant guarantee — API Gateway must NOT invoke the integration for a request the authorizer denied — is unverifiable, and no AWS behaviour ever leaves a request unanswered.

Fix: the API Gateway data plane for a deployed REST stage: the request pipeline that resolves the stage/resource/method, invokes the TOKEN authorizer's Lambda with authorizationToken + methodArn, evaluates the returned policyDocument, and either dispatches the AWS_PROXY integration (Allow) or serves 403 without dispatching (Deny) — each step must terminate and answer.

Traces: apigateway__dynamodb__lambda.apigw-authorizer-deny-integration-not-invoked. Related open issues that do NOT carry this replay key: 20261027T030000Z-apigateway-a-token-lambda-authorizer-allow-decision-is-never-obtained.md (TOKEN Allow answers 500 on the apigateway token), 20261113T185449Z-apigateway-dynamodb-lambda-2-net-new-causes-an.md (un-invocable authorizer error code), 20261029T235159Z-apigateway-dynamodb-lambda-2-causes-the-malformed-proxy.md (other unobservable stage behaviours on this token).

## Evidence
Check: apigateway__dynamodb__lambda.apigw-authorizer-deny-integration-not-invoked
Verdict: mismatch — "Error: Check timed out after 60.0s" (no response recorded).

Expected (AWS), per the check's own assertions:
  GET <invoke-url>/test/guarded with header Authorization: let-me-in  -> HTTP 200, and within the poll window the backend's marker item {pk: "invoked", path: "/guarded"} is readable with GetItem(ConsistentRead=True).
  GET <invoke-url>/test/guarded with header Authorization: keep-me-out -> HTTP 403, and the marker item does NOT reappear for the whole 45 s observation window (the integration must not be invoked).
Actual (emulator): the check is cut off at the 60 s cap; no status, error code or body is ever observed for either leg, so the marker poll and the leak window never conclude.

The authorizer handler returns principalId "probe-user" and a policyDocument whose single statement is {Action: execute-api:Invoke, Effect: Allow|Deny, Resource: <methodArn>}; authorizerResultTtlInSeconds=0, and lambda:InvokeFunction is granted to apigateway.amazonaws.com for BOTH the authorizer (.../authorizers/<id>) and the backend (.../*/GET/guarded), so a missing invoke grant cannot be the reason the integration does not run.

## Steps to Reproduce
1. dynamodb.create_table(KeySchema=[{pk,HASH}], BillingMode="PAY_PER_REQUEST"); poll describe_table until TableStatus==ACTIVE.
2. Create two Lambda functions: a backend that PutItems {pk:"invoked", path:<event path>} into that table and returns {statusCode:200}, and a TOKEN authorizer that returns Effect Allow when authorizationToken=="let-me-in" and Deny otherwise, with Resource=event["methodArn"].
3. apigateway.create_rest_api; create_resource pathPart="guarded"; create_authorizer(type="TOKEN", identitySource="method.request.header.Authorization", authorizerUri=<authorizer lambda invocations arn>, authorizerResultTtlInSeconds=0); put_method(httpMethod="GET", authorizationType="CUSTOM", authorizerId=<id>); put_integration(type="AWS_PROXY", integrationHttpMethod="POST", uri=<backend lambda invocations arn>); create_deployment(stageName="test"); lambda.add_permission for apigateway.amazonaws.com on both functions.
4. GET the stage URL /test/guarded with Authorization: let-me-in.
   Observed: the request never completes; the check is cut off at the 60 s harness cap.
   Expected: HTTP 200, then the marker item becomes readable.
5. Delete the marker item, then GET the same URL with Authorization: keep-me-out.
   Expected: HTTP 403 and the marker item never reappears.
