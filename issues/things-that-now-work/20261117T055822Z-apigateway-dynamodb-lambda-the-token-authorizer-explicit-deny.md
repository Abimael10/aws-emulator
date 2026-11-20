Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__dynamodb__lambda.apigw-authorizer-deny-integration-not-invoked
Issue-Key: apigateway__dynamodb__lambda.apigw-authorizer-deny-integration-not-invoked|_collect_marker#28|Timeout|return None
Issue-Commit: 1f5fd9d954c15ebb463483e9fc6bcde2d93c0e61

# apigateway__dynamodb__lambda: the TOKEN-authorizer explicit-Deny scenario never reaches a verdict — the deployed REST stage hangs to the 60 s harness cap

> _Note: this issue was identified against the code base at commit `1f5fd9d954c15ebb463483e9fc6bcde2d93c0e61`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke (execute-api, deployed REST stage with a TOKEN Lambda authorizer)

## Description
Cause 1 of 1: a deployed REST API method guarded by a TOKEN Lambda authorizer never produces a terminal HTTP outcome, so neither the explicit-Deny refusal nor the allowed-token success can be observed; the scenario runs out the harness's 60 s cap with no response.

AWS: a REST API method configured with a TOKEN-type Lambda authorizer invokes the authorizer with `authorizationToken` and `methodArn`, then evaluates the returned IAM policy. When the authorizer returns a policy whose statement for `execute-api:Invoke` on the request's methodArn has `"Effect": "Deny"`, API Gateway refuses the request at the authorization stage and NEVER invokes the method's integration: HTTP 403 with header `x-amzn-ErrorType: AccessDeniedException` and body `{"Message":"User is not authorized to access this resource with an explicit deny"}` (that text is fixed for an explicit deny; the implicit-deny/no-matching-statement form is `{"Message":"User is not authorized to access this resource"}`). Because the integration never runs, the backend's side effect does not happen — the DynamoDB item the backend would PutItem is absent. With the allowed token the same method returns HTTP 200 with the integration's body and the item is present. Recorded behavior only for the per-call timing; the 403/AccessDeniedException/deny-message classification and the "integration not invoked" invariant are the requirement.

Emulator: no response is produced at all. The check is killed by the harness: `Check timed out after 60.0s`. Neither branch of the scenario (deny -> 403 + no item, allow -> 200 + item) is reachable, so the authorization boundary is unverifiable and the overly-permissive risk it guards is unobservable: a client cannot distinguish "deny enforced" from "request hung".

Why: a client that relies on a Lambda authorizer as its authorization boundary gets no answer — not a 403, not a 200, not an error — so every request through an authorizer-guarded stage stalls until the caller's own timeout. Against AWS the same call answers within normal request latency with a definite 403 and leaves no backend side effect; against the emulator the request never terminates, which both breaks callers and hides whether the Deny is honored (the integration may in fact be running).

Fix: the API Gateway request-authorization path for a deployed REST stage — authorizer invocation and policy evaluation in the stage's request pipeline, and the pipeline's terminal response emission (it must emit the 403 AccessDeniedException refusal before any integration dispatch, and must always emit some terminal response). Related open issues describe other apigateway REST-stage scenarios that hang (20261027T101902Z-apigateway-dynamodb-lambda-the-request-validator-negative-check-fa24e3.md, 20261029T235159Z-apigateway-dynamodb-lambda-2-causes-the-malformed-proxy.md, 20261023T210711Z-apigateway-dynamodb-lambda-logs-five-checks-hang-to.md) and 20261105T111951Z-apigateway-dynamodb-lambda-every-scenario-s-bootstrap-lambda.md covers a bootstrap-Lambda init fault; none of them carries this check's replay key, so this TOKEN-authorizer Deny path is not covered by any of them.

Traces: apigateway__dynamodb__lambda.apigw-authorizer-deny-integration-not-invoked.

## Evidence
Check: apigateway__dynamodb__lambda.apigw-authorizer-deny-integration-not-invoked [mismatch]
Error: Check timed out after 60.0s

Expected (AWS):
  - GET <stage invoke URL>/<resource> with Authorization: <bad token>
      -> 403, x-amzn-ErrorType: AccessDeniedException,
         body {"Message":"User is not authorized to access this resource with an explicit deny"}
      -> the backend integration is not invoked, so the DynamoDB item with pk="invoked" is absent.
  - Same request with Authorization: let-me-in
      -> 200 with body {"ok": true} and the DynamoDB item with pk="invoked" present.

Actual (emulator): no terminal HTTP response for either branch; the scenario produces no verdict and is terminated by the 60 s harness cap. The authorizer returns Effect=Deny for any token other than `let-me-in` and Allow for `let-me-in`, with Resource = event.methodArn, so the scenario is well-formed on the request side.

Fixed text vs. values in the expected message: "User is not authorized to access this resource with an explicit deny" is entirely fixed text for the explicit-deny class (no request- or state-derived substitution). The table name, function names and stage/API ids in the scenario are per-run values and are not part of the requirement.

## Steps to Reproduce
1. Create a DynamoDB table (PAY_PER_REQUEST, HASH key `pk`) and wait for ACTIVE.
2. Create a backend Lambda that PutItems {pk: "invoked", path: <event path>} into that table, granted dynamodb:PutItem on it.
3. Create an authorizer Lambda that returns principalId `probe-user` and a policy allowing `execute-api:Invoke` on event.methodArn when authorizationToken == "let-me-in", and denying it otherwise.
4. Create a REST API with a method wired to the backend via AWS_PROXY, attach a TOKEN authorizer (identity source method.request.header.Authorization) pointing at the authorizer Lambda, and deploy a stage.
5. GET the stage invoke URL with Authorization: nope -> expect 403 AccessDeniedException with the explicit-deny message, and verify via GetItem that pk="invoked" was never written.
6. GET the same URL with Authorization: let-me-in -> expect 200 and the item present.
7. Against the emulator neither request produces a response; the scenario hangs. Equivalent replay: run behavioral check apigateway__dynamodb__lambda.apigw-authorizer-deny-integration-not-invoked.
